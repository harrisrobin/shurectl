//! Original MV7 text commands, carried in unnumbered 64-byte HID reports.

use super::{AutoTone, CompressorPreset, DeviceState, InputMode, MicPosition};
use anyhow::{Context, Result, bail, ensure};

// MOTIV's 2% capture is one count below the rounded -54 dB word.
const MIX_MIN: u32 = 0x20C4;
const MIX_PC_MAX: u32 = 0x2026F3;
const MIX_MIC_MAX: u32 = 0x4026E7;
pub const GAIN_STEP_TENTHS: u16 = 15;
pub const FAILED_REPLY: &str = "[Failed]";
pub const GAIN_SET_PREFIX: &str = "inputGain ";

// MOTIV measurements from biozz's PR #89, on firmware 0.0.52.0.
// https://github.com/Humblemonk/shurectl/pull/89
const MIX_CURVE: [(u8, f64, f64); 11] = [
    (0, -54.0, 0.0),
    (2, -54.0, 0.0),
    (10, -39.0, 0.0),
    (26, -19.5, 0.0),
    (40, -10.0, 0.0),
    (50, -6.0, 0.0),
    (60, -6.0, -6.0),
    (74, -6.0, -16.5),
    (90, -6.0, -39.0),
    (98, -6.0, -51.0),
    (100, -6.0, -54.0),
];

pub fn report(command: &str) -> Result<[u8; 65]> {
    ensure!(
        !command.is_empty()
            && command.len() < 64
            && command.bytes().all(|byte| (b' '..=b'~').contains(&byte)),
        "MV7 commands must be printable ASCII and fit in one HID report"
    );
    let mut packet = [0; 65];
    packet[1..=command.len()].copy_from_slice(command.as_bytes());
    Ok(packet)
}

pub fn line(report: &[u8]) -> Result<&str> {
    let payload = report.strip_prefix(&[0]).unwrap_or(report);
    let end = payload
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(payload.len());
    ensure!(payload[..end].is_ascii(), "Non-ASCII MV7 response");
    Ok(std::str::from_utf8(&payload[..end])?.trim())
}

pub fn apply_response(line: &str, state: &mut DeviceState) -> Result<bool> {
    let line = line.trim();
    ensure!(
        line != "Locked"
            && !line.starts_with("Err")
            && !line.contains("Not valid")
            && !line.contains("[Failed]")
            && !line.starts_with("Invalid CMD")
            && !line.starts_with("No such command")
            && !line.starts_with("Data Full"),
        "MV7 rejected the command: {line}"
    );
    if let Some((key, value)) = line.split_once('=') {
        match key {
            "dspMode" => {
                let mode: u32 = value.parse().context("Invalid MV7 DSP mode")?;
                ensure!((1..=7).contains(&mode), "Invalid MV7 DSP mode: {mode}");
                state.mode = if mode == 1 {
                    InputMode::Manual
                } else {
                    InputMode::Auto
                };
                if mode > 1 {
                    apply_auto(mode - 1, state)?;
                }
            }
            "inputGain" => state.gain_tenths = (decibels(value, 0.0, 36.0)? * 10.0).round() as u16,
            "micMute" => state.muted = boolean(value)?,
            "lock" => state.locked = boolean(value)?,
            "audioMute" => state.mv7.monitor_muted = boolean(value)?,
            "volume" => state.mv7.monitor_volume_db = decibels(value, -24.0, 0.0)?.round() as i8,
            "meterMode" => state.mv7.live_meters = boolean(value)?,
            "dimMode" => state.mv7.dim_leds = boolean(value)?,
            "fwVersion" => {
                ensure!(!value.is_empty(), "Empty MV7 firmware version");
                state.firmware_version = value.to_owned();
            }
            "serialNum" => {
                ensure!(!value.is_empty(), "Empty MV7 serial number");
                state.serial_number = value.to_owned();
                state.factory_serial = value.to_owned();
            }
            _ => return Ok(false),
        }
        return Ok(true);
    }
    let Some(block) = line.strip_prefix("block ") else {
        return Ok(false);
    };
    let (id, payload) = block
        .split_once(' ')
        .context("Incomplete MV7 block response")?;
    let payload = payload.trim();
    if id.eq_ignore_ascii_case("22") {
        ensure!(
            payload.len() == 16 && payload.is_ascii(),
            "Invalid MV7 monitor mix block"
        );
        let pc = word(&payload[..8])?;
        let mic = word(&payload[8..])?;
        state.monitor_mix = mix_percent(pc, mic)?;
        return Ok(true);
    }
    if !["19", "1F", "31", "34"]
        .iter()
        .any(|key| key.eq_ignore_ascii_case(id))
    {
        return Ok(false);
    }
    let value = word(payload)?;
    match id.to_ascii_uppercase().as_str() {
        "19" => {
            state.compressor = match value {
                0 => CompressorPreset::Off,
                1 => CompressorPreset::Light,
                2 => CompressorPreset::Medium,
                3 => CompressorPreset::Heavy,
                _ => {
                    bail!("Invalid MV7 compressor: {value}")
                }
            }
        }
        "1F" => {
            ensure!(value <= 1, "Invalid MV7 limiter: {value}");
            state.limiter_enabled = value == 1;
        }
        "31" => {
            ensure!(value <= 3, "Invalid MV7 EQ: {value}");
            state.mv7.high_pass = value & 1 != 0;
            state.mv7.presence = value & 2 != 0;
        }
        "34" => apply_auto(value, state)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn boolean(value: &str) -> Result<bool> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => bail!("Invalid MV7 on/off value: {value}"),
    }
}

fn decibels(value: &str, min: f32, max: f32) -> Result<f32> {
    let db: f32 = value.strip_suffix("dB").unwrap_or(value).trim().parse()?;
    ensure!(
        db.is_finite() && (min..=max).contains(&db),
        "Invalid MV7 level: {value}"
    );
    Ok(db)
}

fn word(payload: &str) -> Result<u32> {
    ensure!(
        payload.len() == 8 && payload.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid MV7 block: {payload}"
    );
    Ok(u32::from_str_radix(payload, 16)?)
}

fn apply_auto(value: u32, state: &mut DeviceState) -> Result<()> {
    ensure!(value <= 6, "Invalid MV7 Auto Level preset: {value}");
    if value == 0 {
        return Ok(());
    }
    state.auto_position = if value >= 4 {
        MicPosition::Far
    } else {
        MicPosition::Near
    };
    state.auto_tone = match (value - 1) % 3 {
        0 => AutoTone::Natural,
        1 => AutoTone::Dark,
        _ => AutoTone::Bright,
    };
    Ok(())
}

pub fn auto_value(position: MicPosition, tone: AutoTone) -> u32 {
    let distance = match position {
        MicPosition::Near => 1,
        MicPosition::Far => 4,
    };
    distance
        + match tone {
            AutoTone::Natural => 0,
            AutoTone::Dark => 1,
            AutoTone::Bright => 2,
        }
}

fn mix_levels(percent: u8) -> (f64, f64) {
    let percent = percent.min(100);
    for pair in MIX_CURVE.windows(2) {
        let (start, pc_start, mic_start) = pair[0];
        let (end, pc_end, mic_end) = pair[1];
        if percent <= end {
            let fraction = f64::from(percent - start) / f64::from(end - start);
            return (
                pc_start + fraction * (pc_end - pc_start),
                mic_start + fraction * (mic_end - mic_start),
            );
        }
    }
    (-6.0, -54.0)
}

pub fn mix_words(percent: u8) -> (u32, u32) {
    let (pc, mic) = mix_levels(percent);
    let word = |db| (f64::from(MIX_MIC_MAX) * 10f64.powf(db / 20.0)).round() as u32;
    (word(pc), word(mic))
}

pub fn mix_percent(pc: u32, mic: u32) -> Result<u8> {
    ensure!(
        (MIX_MIN..=MIX_PC_MAX).contains(&pc) && (MIX_MIN..=MIX_MIC_MAX).contains(&mic),
        "Invalid MV7 monitor mix levels"
    );
    let db = |word| 20.0 * (f64::from(word) / f64::from(MIX_MIC_MAX)).log10();
    let (pc_db, mic_db) = (db(pc), db(mic));
    let mut nearest = 0;
    let mut best_distance = f64::INFINITY;
    for percent in 0..=100 {
        let (expected_pc, expected_mic) = mix_levels(percent);
        let distance = (pc_db - expected_pc).powi(2) + (mic_db - expected_mic).powi(2);
        if distance < best_distance {
            best_distance = distance;
            nearest = percent;
        }
    }
    Ok(nearest)
}

/// Only the documented, single-report settings queries are used here.
pub const GET_LOCK: &str = "lock";
pub const GET_GAIN: &str = "inputGain";
pub const GET_SERIAL: &str = "serialNum";
pub const GET_AUTO: &str = "getBlock 34";
pub const LOCK_SET_PREFIX: &str = "lock ";
pub const GETTERS: &[&str] = &[
    "fwVersion",
    GET_SERIAL,
    "dspMode",
    GET_GAIN,
    "micMute",
    GET_LOCK,
    "audioMute",
    "volume",
    "meterMode",
    "dimMode",
    "getBlock 22",
    "getBlock 19",
    "getBlock 1F",
    "getBlock 31",
    GET_AUTO,
];
pub const ADMIN: &str = "su adm";

pub fn response_prefix(command: &str) -> String {
    let mut words = command.split_whitespace();
    match words.next() {
        Some("getBlock" | "setBlock") => {
            format!("block {} ", words.next().unwrap_or_default())
        }
        Some(key) => format!("{key}="),
        None => String::new(),
    }
}

pub fn verify_set(command: &str, response: &str) -> Result<()> {
    let (key, value) = command.split_once(' ').context("MV7 setter has no value")?;
    let expected = if key == "setBlock" {
        format!("block {value}")
    } else if matches!(key, "inputGain" | "volume") {
        format!("{key}={value}dB")
    } else {
        format!("{key}={value}")
    };
    ensure!(
        response.eq_ignore_ascii_case(&expected),
        "MV7 did not apply {command}: {response}"
    );
    Ok(())
}

pub fn verify_gain(tenths: u16, response: &str) -> Result<()> {
    ensure!(
        response.starts_with(&response_prefix(GET_GAIN)),
        "Missing MV7 gain reply: {response}"
    );
    let mut state = DeviceState::default();
    apply_response(response, &mut state)?;
    ensure!(
        state.gain_tenths == tenths,
        "MV7 gain readback differs from {} dB: {response}",
        super::format_gain(tenths)
    );
    Ok(())
}

pub fn cmd_gain(tenths: u16) -> String {
    format!(
        "{GAIN_SET_PREFIX}{}",
        super::format_gain(super::DeviceModel::Mv7.clamp_gain(tenths))
    )
}
pub fn cmd_mode(auto: bool, position: MicPosition, tone: AutoTone) -> String {
    format!(
        "dspMode {}",
        if auto {
            auto_value(position, tone) + 1
        } else {
            1
        }
    )
}
pub fn cmd_mute(on: bool) -> String {
    switch("micMute", on)
}
pub fn cmd_lock(on: bool) -> String {
    switch("lock", on)
}
pub fn cmd_monitor_mute(on: bool) -> String {
    switch("audioMute", on)
}
pub fn cmd_live_meters(on: bool) -> String {
    switch("meterMode", on)
}
pub fn cmd_dim_leds(on: bool) -> String {
    switch("dimMode", on)
}
pub fn cmd_volume(db: i8) -> String {
    format!("volume {}", db.clamp(-24, 0))
}
pub fn cmd_limiter(on: bool) -> String {
    block("1F", u32::from(on))
}
pub fn cmd_compressor(preset: CompressorPreset) -> String {
    block(
        "19",
        match preset {
            CompressorPreset::Off => 0,
            CompressorPreset::Light => 1,
            CompressorPreset::Medium => 2,
            CompressorPreset::Heavy => 3,
        },
    )
}
pub fn cmd_eq(high_pass: bool, presence: bool) -> String {
    block("31", u32::from(high_pass) | (u32::from(presence) << 1))
}
pub fn cmd_auto(position: MicPosition, tone: AutoTone) -> String {
    cmd_mode(true, position, tone)
}
pub fn cmd_mix(percent: u8) -> String {
    let (pc, mic) = mix_words(percent);
    format!("setBlock 22 {pc:08X}{mic:08X}")
}
fn switch(key: &str, on: bool) -> String {
    format!("{key} {}", if on { "on" } else { "off" })
}
fn block(id: &str, value: u32) -> String {
    format!("setBlock {id} {value:08X}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CompressorPreset;

    #[test]
    fn fractional_gain_survives_readback() {
        let mut state = DeviceState::default();
        apply_response("inputGain=31.5dB", &mut state).unwrap();
        assert_eq!(state.gain_tenths, 315);
    }

    #[test]
    fn motiv_two_percent_capture_decodes_as_floor() {
        let mut state = DeviceState::default();
        apply_response("block 22 000020C4004026E7", &mut state).unwrap();
        assert_eq!(state.monitor_mix, 0);
        assert!(mix_percent(0x20C3, 0x4026E7).is_err());
    }

    #[test]
    fn gain_commands_floor_to_grid_and_require_matching_readback() {
        for (tenths, command) in [
            (0, "inputGain 0"),
            (315, "inputGain 31.5"),
            (330, "inputGain 33"),
            (340, "inputGain 33"),
            (345, "inputGain 34.5"),
            (u16::MAX, "inputGain 36"),
        ] {
            assert_eq!(cmd_gain(tenths), command);
        }
        verify_gain(315, "inputGain=31.5dB").unwrap();
        verify_gain(330, "inputGain=33.0dB").unwrap();
        for reply in [
            "inputGain=30dB",
            "inputGain=NaN",
            "[Failed]",
            "Locked",
            "micMute=off",
        ] {
            assert!(verify_gain(315, reply).is_err(), "{reply}");
        }
    }

    #[test]
    fn mix_intermediates_roundtrip_with_the_motiv_floor() {
        for percent in 0..=100 {
            let (pc, mic) = mix_words(percent);
            assert_eq!(
                mix_percent(pc, mic).unwrap(),
                if percent <= 2 { 0 } else { percent }
            );
        }
    }

    #[test]
    fn monitor_mix_matches_independent_motiv_captures() {
        for (percent, pc, mic) in [
            (0, 0x000020C5, 0x004026E7),
            (10, 0x0000B845, 0x004026E7),
            (26, 0x0006CB9A, 0x004026E7),
            (40, 0x00144961, 0x004026E7),
            (50, 0x002026F3, 0x004026E7),
            (60, 0x002026F3, 0x002026F3),
            (74, 0x002026F3, 0x00099941),
            (90, 0x002026F3, 0x0000B845),
            (100, 0x002026F3, 0x000020C5),
        ] {
            assert_eq!(mix_words(percent), (pc, mic), "encode {percent}%");
            assert_eq!(mix_percent(pc, mic).unwrap(), percent, "decode {percent}%");
        }
    }

    #[test]
    fn far_profile_uses_the_complete_dsp_preset() {
        assert_eq!(cmd_auto(MicPosition::Far, AutoTone::Natural), "dspMode 5");
    }

    #[test]
    fn setting_commands_match_documented_wire_values_and_clamp_limits() {
        for (command, expected, response) in [
            (cmd_gain(u16::MAX), "inputGain 36", "inputGain=36dB"),
            (
                cmd_mode(false, MicPosition::Far, AutoTone::Bright),
                "dspMode 1",
                "dspMode=1",
            ),
            (
                cmd_mode(true, MicPosition::Near, AutoTone::Natural),
                "dspMode 2",
                "dspMode=2",
            ),
            (cmd_mute(true), "micMute on", "micMute=on"),
            (cmd_lock(false), "lock off", "lock=off"),
            (cmd_monitor_mute(false), "audioMute off", "audioMute=off"),
            (cmd_live_meters(true), "meterMode on", "meterMode=on"),
            (cmd_dim_leds(true), "dimMode on", "dimMode=on"),
            (cmd_volume(-100), "volume -24", "volume=-24dB"),
            (cmd_volume(100), "volume 0", "volume=0dB"),
            (
                cmd_limiter(true),
                "setBlock 1F 00000001",
                "block 1F 00000001",
            ),
            (
                cmd_compressor(CompressorPreset::Heavy),
                "setBlock 19 00000003",
                "block 19 00000003",
            ),
            (
                cmd_eq(true, true),
                "setBlock 31 00000003",
                "block 31 00000003",
            ),
            (
                cmd_auto(MicPosition::Far, AutoTone::Bright),
                "dspMode 7",
                "dspMode=7",
            ),
            (
                cmd_mix(50),
                "setBlock 22 002026F3004026E7",
                "block 22 002026F3004026E7",
            ),
        ] {
            assert_eq!(command, expected);
            let packet = report(&command).unwrap();
            assert_eq!(line(&packet).unwrap(), expected);
            assert!(response.starts_with(&response_prefix(&command)));
            verify_set(&command, response).unwrap();
        }
    }

    #[test]
    fn unchanged_or_unrelated_acknowledgements_do_not_report_success() {
        assert!(verify_set("micMute on", "micMute=off").is_err());
        assert!(verify_set("setBlock 31 00000003", "block 31 00000000").is_err());
        assert!(verify_set("su adm", "su=nor").is_err());
        assert!(!"block 31 00000000".starts_with(&response_prefix("getBlock 34")));
        assert!(!"micMute=on".starts_with(&response_prefix("inputGain 33")));
    }

    #[test]
    fn commands_have_report_id_zero_and_a_padded_64_byte_payload() {
        let packet = report("getBlock 34").unwrap();
        assert_eq!(packet.len(), 65);
        assert_eq!(&packet[..13], b"\0getBlock 34\0");
        assert!(packet[13..].iter().all(|b| *b == 0));
    }

    #[test]
    fn rejects_truncated_or_injected_commands() {
        for command in [
            "",
            "micMute on\nlock off",
            "micMute\0off",
            "café",
            &"x".repeat(64),
        ] {
            assert!(report(command).is_err(), "{command:?}");
        }
    }

    #[test]
    fn reads_payloads_with_or_without_an_unnumbered_report_id() {
        assert_eq!(line(b"dspMode=5\n\0\0").unwrap(), "dspMode=5");
        assert_eq!(line(b"\0dspMode=5\n\0").unwrap(), "dspMode=5");
        assert!(line(&[255]).is_err());
    }

    #[test]
    fn all_six_auto_profiles_decode_to_their_actual_distance_and_tone() {
        for (mode, position, tone) in [
            (2, MicPosition::Near, AutoTone::Natural),
            (3, MicPosition::Near, AutoTone::Dark),
            (4, MicPosition::Near, AutoTone::Bright),
            (5, MicPosition::Far, AutoTone::Natural),
            (6, MicPosition::Far, AutoTone::Dark),
            (7, MicPosition::Far, AutoTone::Bright),
        ] {
            let mut state = DeviceState::default();
            assert!(apply_response(&format!("dspMode={mode}"), &mut state).unwrap());
            assert_eq!(state.mode, InputMode::Auto);
            assert_eq!(state.auto_position, position);
            assert_eq!(state.auto_tone, tone);
        }
    }

    #[test]
    fn decodes_a_captured_mv7_settings_snapshot() {
        let mut state = DeviceState::default();
        for response in [
            "dspMode=5",
            "inputGain=33dB",
            "micMute=off",
            "lock=on",
            "audioMute=off",
            "volume=-5dB",
            "block 22 002026F3004026E7",
            "block 19 00000000",
            "block 1F 00000001",
            "block 31 00000000",
            "block 34 00000004",
            "meterMode=off",
            "dimMode=off",
            "fwVersion=0.0.52.0",
            "serialNum=TEST-SERIAL",
        ] {
            assert!(apply_response(response, &mut state).unwrap(), "{response}");
        }
        assert_eq!(state.gain_tenths, 330);
        assert!(state.locked);
        assert!(!state.muted);
        assert_eq!(state.monitor_mix, 50);
        assert_eq!(state.compressor, CompressorPreset::Off);
        assert!(state.limiter_enabled);
        assert_eq!(state.mv7.monitor_volume_db, -5);
        assert!(!state.mv7.monitor_muted);
        assert!(!state.mv7.live_meters);
        assert!(!state.mv7.dim_leds);
        assert_eq!(state.firmware_version, "0.0.52.0");
        assert_eq!(state.display_serial(), "TEST-SERIAL");
    }

    #[test]
    fn manual_mode_and_eq_preserve_independent_filter_switches() {
        let mut state = DeviceState::default();
        assert!(apply_response("dspMode=1", &mut state).unwrap());
        assert_eq!(state.mode, InputMode::Manual);
        for (value, high_pass, presence) in [
            (0, false, false),
            (1, true, false),
            (2, false, true),
            (3, true, true),
        ] {
            assert!(apply_response(&format!("block 31 {value:08X}"), &mut state).unwrap());
            assert_eq!(state.mv7.high_pass, high_pass);
            assert_eq!(state.mv7.presence, presence);
        }
    }

    #[test]
    fn malformed_recognized_values_fail_instead_of_becoming_defaults() {
        for response in [
            "dspMode=0",
            "dspMode=8",
            "inputGain=NaN",
            "inputGain=37dB",
            "volume=-25dB",
            "micMute=maybe",
            "block 19 00000004",
            "block 1F 00000002",
            "block 31 00000004",
            "block 34 00000007",
            "block 22 00",
            "block 31",
            "block 31 Not valid",
            "Locked",
            "ErrA",
        ] {
            assert!(
                apply_response(response, &mut DeviceState::default()).is_err(),
                "{response}"
            );
        }
        assert!(!apply_response("USB IN Token Missing", &mut DeviceState::default()).unwrap());
    }

    #[test]
    fn auto_commands_use_near_far_and_natural_dark_bright_encoding() {
        assert_eq!(auto_value(MicPosition::Near, AutoTone::Natural), 1);
        assert_eq!(auto_value(MicPosition::Near, AutoTone::Dark), 2);
        assert_eq!(auto_value(MicPosition::Near, AutoTone::Bright), 3);
        assert_eq!(auto_value(MicPosition::Far, AutoTone::Natural), 4);
        assert_eq!(auto_value(MicPosition::Far, AutoTone::Dark), 5);
        assert_eq!(auto_value(MicPosition::Far, AutoTone::Bright), 6);
    }

    #[test]
    fn monitor_mix_keeps_one_channel_at_reference_level() {
        assert_eq!(mix_words(0), (0x20C5, 0x4026E7));
        assert_eq!(mix_words(50), (0x2026F3, 0x4026E7));
        assert_eq!(mix_words(100), (0x2026F3, 0x20C5));
        assert_eq!(mix_words(255), mix_words(100));
        assert_eq!(mix_percent(0x20C5, 0x4026E7).unwrap(), 0);
        assert_eq!(mix_percent(0x2026F3, 0x4026E7).unwrap(), 50);
        assert_eq!(mix_percent(0x2026F3, 0x20C5).unwrap(), 100);
        assert!(mix_percent(u32::MAX, 0).is_err());
    }
}
