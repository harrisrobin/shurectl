//! Device I/O: wraps hidapi for Shure USB microphones.
//!
//! Supports five devices:
//!   - Shure MVX2U       (VID 0x14ED, PID 0x1013) — XLR-to-USB interface (Gen 1)
//!   - Shure MVX2U Gen 2 (VID 0x14ED, PID 0x1033) — XLR-to-USB interface (Gen 2)
//!   - Shure MV6         (VID 0x14ED, PID 0x1026) — USB gaming microphone
//!   - Shure MV7         (VID 0x14ED, PID 0x1012) — original ASCII protocol
//!   - Shure MV7+        (VID 0x14ED, PID 0x1019) — USB/XLR dynamic microphone (protocol unverified)
//!
//! All five devices expose a USB HID configuration interface alongside their
//! audio interface. hidapi opens it via /dev/hidrawN on Linux, IOKit on macOS,
//! and \\.\HID#VID_... paths on Windows, bypassing the audio driver entirely.
//!
//! # Transport
//!
//! Configuration uses plain HID Output/Input reports:
//!   - `hid_write()` sends a command to the device (Output report).
//!   - `hid_read()` receives a response (Input report from the Interrupt IN endpoint).
//!
//! The original MV7 uses matched ASCII replies and no CONFIRM packet.
//! For the binary protocol models, every SET command must be followed immediately by a CONFIRM packet; the device
//! will not apply the change otherwise. GET commands receive one response packet
//! on the next read.
//!
//! # Sequence numbers
//!
//! Each packet carries an incrementing sequence number (0–255, wrapping). The
//! device echoes this number in its response. We track it in `ShureDevice` and
//! increment after every `write()`.
//!
//! # Multi-device
//!
//! If only one Shure device is plugged in, `open()` opens it automatically.
//! If multiple Shure devices are detected, `open()` returns an error directing
//! the user to specify one with `--device`. Use `list_devices()` to enumerate.

use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::{Context, Result, anyhow};
use hidapi::{HidApi, HidDevice};

use crate::protocol::mv7;

use crate::protocol::{
    self, DeviceModel, DeviceState, MV6_PID, MV7_PID, MV7_PLUS_PID, MVX2U_GEN2_PID, PACKET_SIZE,
    PID, VID, apply_response, cmd_confirm, cmd_factory_reset, cmd_get_auto_gain,
    cmd_get_auto_position, cmd_get_auto_tone, cmd_get_compressor, cmd_get_device_name,
    cmd_get_eq_band_enable, cmd_get_eq_band_gain, cmd_get_eq_enable, cmd_get_firmware_version,
    cmd_get_gain, cmd_get_hpf, cmd_get_limiter, cmd_get_lock, cmd_get_mix, cmd_get_mode,
    cmd_get_mute, cmd_get_mv6_denoiser, cmd_get_mv6_gain_lock, cmd_get_mv6_mix,
    cmd_get_mv6_mute_btn_disable, cmd_get_mv6_popper_stopper, cmd_get_mv6_tone,
    cmd_get_mv7_led_behavior, cmd_get_mv7_led_brightness, cmd_get_mv7_led_live_edge,
    cmd_get_mv7_led_live_interior, cmd_get_mv7_led_live_middle, cmd_get_mv7_led_live_theme,
    cmd_get_mv7_led_pulsing_color, cmd_get_mv7_led_solid_color, cmd_get_mv7_led_solid_theme,
    cmd_get_mv7_playback_mix, cmd_get_mv7_reverb_intensity, cmd_get_mv7_reverb_monitor,
    cmd_get_mv7_reverb_output, cmd_get_mv7_reverb_type, cmd_get_phantom, cmd_get_serial,
    cmd_set_lock, cmd_set_mv7_gain, cmd_set_mv7_led_behavior, cmd_set_mv7_led_brightness,
    cmd_set_mv7_led_live_edge, cmd_set_mv7_led_live_interior, cmd_set_mv7_led_live_middle,
    cmd_set_mv7_led_live_theme, cmd_set_mv7_led_pulsing_color, cmd_set_mv7_led_pulsing_theme,
    cmd_set_mv7_led_solid_color, cmd_set_mv7_led_solid_theme, parse_response,
    parse_response_with_prefix,
};

#[cfg(target_os = "linux")]
const ACCESS_HINT: &str = "ensure the udev rule is installed, or run with sudo";
#[cfg(target_os = "windows")]
const ACCESS_HINT: &str =
    "ensure no other software (e.g. ShurePlus MOTIV) has exclusive access to the device";
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
const ACCESS_HINT: &str = "ensure the device is plugged in and accessible";

/// How long to wait for a read response, in milliseconds.
const READ_TIMEOUT_MS: i32 = 200;

/// A connected Shure USB microphone (MVX2U or MV6).
pub struct ShureDevice {
    device: HidDevice,
    /// Which device model this is — drives protocol and UI decisions.
    pub model: DeviceModel,
    /// Packet sequence number. Increments after every write; wraps at 256.
    seq: AtomicU8,
    /// Serial number string read from the USB device descriptor at open time.
    pub serial_number: String,
}

impl ShureDevice {
    fn from_hid_device(device: HidDevice, pid: u16) -> Result<Self> {
        device.set_blocking_mode(false).ok();
        let serial_number = device
            .get_serial_number_string()
            .ok()
            .flatten()
            .unwrap_or_else(|| "(unknown)".to_string());
        let model = match pid {
            MV6_PID => DeviceModel::Mv6,
            MVX2U_GEN2_PID => DeviceModel::Mvx2uGen2,
            MV7_PLUS_PID => DeviceModel::Mv7Plus,
            MV7_PID => DeviceModel::Mv7,
            _ => DeviceModel::Mvx2u,
        };
        let connected = Self {
            device,
            model,
            seq: AtomicU8::new(0),
            serial_number,
        };
        if connected.model == DeviceModel::Mv7 {
            let response = connected.mv7_request(mv7::ADMIN)?;
            mv7::verify_set(mv7::ADMIN, &response)?;
        }
        Ok(connected)
    }

    /// Open the first (and only) Shure device found on the system.
    ///
    /// Returns an error if zero or more than one Shure device is detected.
    /// Use `--device` to select a specific device when multiple are present.
    pub fn open() -> Result<Self> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        let found = shure_devices(&api);

        match found.len() {
            0 => Err(anyhow!(
                "No Shure MVX2U, MVX2U Gen 2, MV6, MV7, or MV7+ device found.\nHint: {ACCESS_HINT}."
            )),
            1 => {
                let info = found[0];
                let pid = info.product_id();
                let c_path = std::ffi::CString::new(info.path().to_string_lossy().as_ref())
                    .map_err(|_| anyhow!("Device path contains a null byte"))?;
                let device = api
                    .open_path(c_path.as_c_str())
                    .map_err(|e| anyhow!("Cannot open device: {e}\nHint: {ACCESS_HINT}."))?;
                Self::from_hid_device(device, pid)
            }
            n => Err(anyhow!(
                "{n} Shure devices found. Use --device to specify one.\n\
                Run --list to see available devices and their paths."
            )),
        }
    }

    /// Open a Shure device at a specific HID device path.
    pub fn open_path(path: &str) -> Result<Self> {
        let api = HidApi::new().context("Failed to initialise hidapi")?;
        let info = api
            .device_list()
            .find(|d| d.path().to_string_lossy() == path)
            .ok_or_else(|| {
                anyhow!("No device found at {path}. Use --list to see available devices.")
            })?;

        let pid = info.product_id();
        if info.vendor_id() != VID || !is_supported_pid(pid) {
            return Err(anyhow!(
                "{path} is not a supported Shure device \
                (VID={:#06x} PID={:#06x}); expected VID={:#06x} with PID={:#06x}, {:#06x}, {:#06x}, {:#06x}, or {:#06x}.",
                info.vendor_id(),
                pid,
                VID,
                PID,
                MVX2U_GEN2_PID,
                MV6_PID,
                MV7_PLUS_PID,
                MV7_PID,
            ));
        }

        let c_path = std::ffi::CString::new(path)
            .map_err(|_| anyhow!("Device path contains a null byte: {path}"))?;
        let device = api
            .open_path(c_path.as_c_str())
            .map_err(|e| anyhow!("Cannot open {path}: {e}\nHint: {ACCESS_HINT}."))?;
        Self::from_hid_device(device, pid)
    }

    fn mv7_request(&self, command: &str) -> Result<String> {
        anyhow::ensure!(
            self.model == DeviceModel::Mv7,
            "Command requires an original MV7"
        );
        // Clear unsolicited touch-panel notifications before correlating this request.
        let mut buffer = [0; 65];
        for _ in 0..64 {
            if self.device.read_timeout(&mut buffer, 0)? == 0 {
                break;
            }
        }
        let packet = mv7::report(command)?;
        let written = self.device.write(&packet).context("MV7 HID write failed")?;
        anyhow::ensure!(
            written == packet.len(),
            "Incomplete MV7 HID write ({written} bytes)"
        );
        let prefix = mv7::response_prefix(command);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            let count = self.device.read_timeout(&mut buffer, READ_TIMEOUT_MS)?;
            if count == 0 {
                continue;
            }
            let response = mv7::line(&buffer[..count])?;
            // An unchanged gain can return [Failed]; only its setter may recover
            // by proving the requested value through a separate GET.
            if command.starts_with(mv7::GAIN_SET_PREFIX) && response == mv7::FAILED_REPLY {
                return Ok(response.to_owned());
            }
            // Decode even an unrelated response to surface explicit device errors.
            mv7::apply_response(response, &mut DeviceState::default())?;
            if response.starts_with(&prefix) {
                return Ok(response.to_owned());
            }
        }
        Err(anyhow!("MV7 timed out waiting for {prefix}"))
    }

    fn mv7_set(&self, command: String) -> Result<()> {
        if !command.starts_with(mv7::LOCK_SET_PREFIX) {
            self.mv7_require_unlocked()?;
        }
        let response = self.mv7_request(&command)?;
        mv7::verify_set(&command, &response)
    }

    fn mv7_require_unlocked(&self) -> Result<()> {
        let mut state = DeviceState::default();
        mv7::apply_response(&self.mv7_request(mv7::GET_LOCK)?, &mut state)?;
        anyhow::ensure!(
            !state.locked,
            "Unlock Config Lock before changing MV7 settings"
        );
        Ok(())
    }

    fn mv7_set_gain(&self, tenths: u16) -> Result<()> {
        self.mv7_require_unlocked()?;
        let response = self.mv7_request(&mv7::cmd_gain(tenths))?;
        if response != mv7::FAILED_REPLY {
            mv7::verify_gain(tenths, &response)?;
        }
        mv7::verify_gain(tenths, &self.mv7_request(mv7::GET_GAIN)?)
    }

    fn get_state_mv7(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();
        for query in mv7::GETTERS {
            let response = self
                .mv7_request(query)
                .with_context(|| format!("Reading MV7 {query}"))?;
            anyhow::ensure!(
                mv7::apply_response(&response, &mut state)?,
                "Unknown MV7 response: {response}"
            );
        }
        Ok(state)
    }

    pub fn set_mv7_eq(&self, high_pass: bool, presence: bool) -> Result<()> {
        self.mv7_set(mv7::cmd_eq(high_pass, presence))
    }
    pub fn set_mv7_auto(
        &self,
        position: protocol::MicPosition,
        tone: protocol::AutoTone,
    ) -> Result<()> {
        self.mv7_set(mv7::cmd_auto(position, tone))
    }
    pub fn set_mv7_monitor_volume(&self, db: i8) -> Result<()> {
        self.mv7_set(mv7::cmd_volume(db))
    }
    pub fn set_mv7_monitor_mute(&self, on: bool) -> Result<()> {
        self.mv7_set(mv7::cmd_monitor_mute(on))
    }
    pub fn set_mv7_live_meters(&self, on: bool) -> Result<()> {
        self.mv7_set(mv7::cmd_live_meters(on))
    }
    pub fn set_mv7_dim_leds(&self, on: bool) -> Result<()> {
        self.mv7_set(mv7::cmd_dim_leds(on))
    }

    fn next_seq(&self) -> u8 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn write(&self, packet: &[u8]) -> Result<()> {
        let written = self.device.write(packet).context("HID write failed")?;
        if written == 0 {
            return Err(anyhow!("HID write returned 0 bytes"));
        }
        Ok(())
    }

    fn read(&self) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; PACKET_SIZE];
        match self.device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
            Ok(0) => Err(anyhow!("HID read timed out — no response from device")),
            Ok(n) => Ok(buf[..n].to_vec()),
            Err(e) => Err(anyhow!("HID read failed (device disconnected?): {e}")),
        }
    }

    fn send_set(&self, set_packet: &[u8]) -> Result<()> {
        anyhow::ensure!(
            self.model != DeviceModel::Mv7,
            "Setting is unsupported on the original MV7"
        );
        self.write(set_packet)?;
        let confirm = cmd_confirm(self.next_seq());
        self.write(&confirm)?;
        // The MV7+ sends two unsolicited IN responses after every CONFIRM:
        // 1. a CONFIRM ACK (cmd=[09 00 00])
        // 2. a SET echo (RES_SET_FEAT or RES_SET_LOCK with the applied value)
        // Drain both so they don't offset subsequent GET reads in get_state().
        if self.model == DeviceModel::Mv7Plus {
            let mut buf = vec![0u8; PACKET_SIZE];
            for _ in 0..2 {
                let _ = self.device.read_timeout(&mut buf, 50);
            }
        }
        Ok(())
    }

    fn send_get(&self, get_packet: &[u8]) -> Result<Option<([u8; 2], Vec<u8>)>> {
        self.write(get_packet)?;
        let buf = self.read()?;
        Ok(parse_response(&buf))
    }

    /// Like `send_get` but returns `(prefix, feat_addr, value)`.
    /// Used for MV7+ playback mix which shares a feature address with mic mix.
    #[allow(clippy::type_complexity)]
    fn send_get_with_prefix(&self, get_packet: &[u8]) -> Result<Option<(u8, [u8; 2], Vec<u8>)>> {
        self.write(get_packet)?;
        let buf = self.read()?;
        Ok(parse_response_with_prefix(&buf))
    }

    /// Fetch all 5 EQ band gain values and apply them to `state`.
    /// Used by both Gen 1 and Gen 2 state readback.
    fn fetch_eq_band_gains(&self, state: &mut DeviceState, context: &str) {
        for band in 0..5 {
            let gain_pkt = cmd_get_eq_band_gain(self.next_seq(), band);
            if let Ok(Some((feat, value))) = self.send_get(&gain_pkt)
                && !apply_response(feat, &value, state)
            {
                eprintln!(
                    "{context}: unrecognised feature {feat:#04x?} in EQ band {band} gain response"
                );
            }
        }
    }

    /// Send each getter, apply the response to `state`, and log any unrecognised features.
    fn run_getters(&self, getters: &[fn(u8) -> Vec<u8>], state: &mut DeviceState, context: &str) {
        for getter in getters {
            let pkt = getter(self.next_seq());
            if let Ok(Some((feat, value))) = self.send_get(&pkt)
                && !apply_response(feat, &value, state)
            {
                eprintln!("{context}: unrecognised feature {feat:#04x?} in response");
            }
        }
    }

    /// Fetch the complete device state by querying every feature for this model.
    pub fn get_state(&self) -> Result<DeviceState> {
        match self.model {
            DeviceModel::Mv7 => self.get_state_mv7(),
            DeviceModel::Mvx2u => self.get_state_mvx2u(),
            DeviceModel::Mvx2uGen2 => self.get_state_mvx2u_gen2(),
            DeviceModel::Mv6 => self.get_state_mv6(),
            DeviceModel::Mv7Plus => self.get_state_mv7_plus(),
        }
    }

    fn get_state_mvx2u(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

        let lock_pkt = cmd_get_lock(self.next_seq());
        if let Ok(Some((feat, value))) = self.send_get(&lock_pkt)
            && !apply_response(feat, &value, &mut state)
        {
            eprintln!("get_state: unrecognised feature {feat:#04x?} in lock response");
        }

        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_phantom,
            cmd_get_mode,
            cmd_get_auto_position,
            cmd_get_auto_tone,
            cmd_get_auto_gain,
            cmd_get_mix,
            cmd_get_hpf,
            cmd_get_limiter,
            cmd_get_compressor,
            cmd_get_eq_enable,
            cmd_get_device_name,
            cmd_get_firmware_version,
            cmd_get_serial,
        ];

        self.run_getters(getters, &mut state, "get_state");

        for band in 0..5 {
            let en_pkt = cmd_get_eq_band_enable(self.next_seq(), band);
            if let Ok(Some((feat, value))) = self.send_get(&en_pkt)
                && !apply_response(feat, &value, &mut state)
            {
                eprintln!(
                    "get_state: unrecognised feature {feat:#04x?} in EQ band {band} enable response"
                );
            }
        }
        self.fetch_eq_band_gains(&mut state, "get_state");

        Ok(state)
    }

    fn get_state_mvx2u_gen2(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

        // Gen 2 shares most getters with Gen 1 but has no config lock, no EQ master
        // enable, and no per-band enable. It adds denoiser, popper stopper, gain lock,
        // tone, and uses the MV6-style monitor mix framing.
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_phantom,
            cmd_get_mode,
            cmd_get_mv6_mix, // same address as Gen 1 mix; GET uses standard framing
            cmd_get_hpf,
            cmd_get_limiter,
            cmd_get_compressor,
            cmd_get_mv6_denoiser,
            cmd_get_mv6_popper_stopper,
            cmd_get_mv6_tone,
            cmd_get_mv6_gain_lock,
            cmd_get_device_name,
            cmd_get_firmware_version,
            cmd_get_serial,
        ];

        self.run_getters(getters, &mut state, "get_state(mvx2u_gen2)");

        // Gen 2 has 5-band EQ gain (no master enable, no per-band enable toggle).
        self.fetch_eq_band_gains(&mut state, "get_state(mvx2u_gen2)");

        Ok(state)
    }

    fn get_state_mv6(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_hpf,
            cmd_get_mode,
            cmd_get_mv6_denoiser,
            cmd_get_mv6_popper_stopper,
            cmd_get_mv6_tone,
            cmd_get_mv6_gain_lock,
            cmd_get_mv6_mix,
            cmd_get_mv6_mute_btn_disable,
            cmd_get_device_name,
            cmd_get_firmware_version,
            cmd_get_serial,
        ];

        self.run_getters(getters, &mut state, "get_state(mv6)");

        Ok(state)
    }

    fn get_state_mv7_plus(&self) -> Result<DeviceState> {
        let mut state = DeviceState::default();

        // Shared features: use standard GET framing (HDR_CONSTANT=0x03).
        let getters: &[fn(u8) -> Vec<u8>] = &[
            cmd_get_gain,
            cmd_get_mute,
            cmd_get_hpf,
            cmd_get_mode,
            cmd_get_limiter,
            cmd_get_compressor,
            cmd_get_mv6_denoiser,
            cmd_get_mv6_popper_stopper,
            cmd_get_mv6_tone,
            cmd_get_mv6_mute_btn_disable,
            cmd_get_mv6_mix, // mic monitor mix (prefix=0x00)
            cmd_get_mv7_reverb_output,
            cmd_get_mv7_reverb_type,
            cmd_get_mv7_reverb_intensity,
            cmd_get_mv7_reverb_monitor,
            cmd_get_mv7_led_behavior,
            cmd_get_mv7_led_brightness,
            cmd_get_mv7_led_live_theme,
            cmd_get_mv7_led_live_edge,
            cmd_get_mv7_led_live_middle,
            cmd_get_mv7_led_live_interior,
            cmd_get_mv7_led_solid_color,
            cmd_get_mv7_led_pulsing_color,
            cmd_get_mv7_led_solid_theme,
            // Pulsing theme (A6) aliases FEAT_LOCK — not readable via GET.
            cmd_get_device_name,
            cmd_get_firmware_version,
            cmd_get_serial,
        ];
        self.run_getters(getters, &mut state, "get_state(mv7plus)");

        // Playback mix uses the same FEAT_MIX address as mic mix but with prefix=0x03.
        // We issued the request so we know the response is the playback mix channel.
        let pmix_pkt = cmd_get_mv7_playback_mix(self.next_seq());
        if let Ok(Some((_prefix, _feat, value))) = self.send_get_with_prefix(&pmix_pkt)
            && !value.is_empty()
        {
            state.playback_mix = value[0].min(100);
        }

        Ok(state)
    }

    /// Read just the factory serial number (the serial printed on the device,
    /// shown in the MOTIV app) from an opened device. Returns `None` if the device
    /// can't be queried or doesn't report one. Used by `--list` to upgrade the
    /// displayed serial from the USB descriptor value to the printed serial.
    pub fn read_factory_serial(&self) -> Option<String> {
        if self.model == DeviceModel::Mv7 {
            let response = self.mv7_request(mv7::GET_SERIAL).ok()?;
            let mut state = DeviceState::default();
            mv7::apply_response(&response, &mut state).ok()?;
            return Some(state.factory_serial);
        }
        let pkt = cmd_get_serial(self.next_seq());
        let (feat, value) = self.send_get(&pkt).ok()??;
        let mut state = DeviceState::default();
        apply_response(feat, &value, &mut state).then_some(state.factory_serial)
    }

    // ── Shared SET commands ───────────────────────────────────────────────────

    /// Set manual gain. Clamped to the model's maximum.
    pub fn set_gain(&self, gain_tenths: u16) -> Result<()> {
        if self.model == DeviceModel::Mv7 {
            return self.mv7_set_gain(self.model.clamp_gain(gain_tenths));
        }
        let clamped = (self.model.clamp_gain(gain_tenths) / 10) as u8;
        let pkt = match self.model {
            DeviceModel::Mv7Plus => cmd_set_mv7_gain(self.next_seq(), clamped),
            _ => protocol::cmd_set_gain(self.next_seq(), clamped),
        };
        self.send_set(&pkt)
    }

    pub fn set_mode(
        &self,
        auto: bool,
        position: protocol::MicPosition,
        tone: protocol::AutoTone,
    ) -> Result<()> {
        if self.model == DeviceModel::Mv7 {
            return self.mv7_set(mv7::cmd_mode(auto, position, tone));
        }
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_mode(self.next_seq(), auto),
            _ => protocol::cmd_set_mode(self.next_seq(), auto),
        };
        self.send_set(&pkt)
    }

    pub fn set_mute(&self, muted: bool) -> Result<()> {
        if self.model == DeviceModel::Mv7 {
            return self.mv7_set(mv7::cmd_mute(muted));
        }
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_mute(self.next_seq(), muted),
            _ => protocol::cmd_set_mute(self.next_seq(), muted),
        };
        self.send_set(&pkt)
    }

    pub fn set_hpf(&self, freq: &protocol::HpfFrequency) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_hpf(self.next_seq(), freq),
            _ => protocol::cmd_set_hpf(self.next_seq(), freq),
        };
        self.send_set(&pkt)
    }

    // ── MVX2U Gen 1 and Gen 2 SET commands ───────────────────────────────────

    pub fn set_auto_position(&self, position: &protocol::MicPosition) -> Result<()> {
        if self.model == DeviceModel::Mv7 {
            let mut state = DeviceState::default();
            mv7::apply_response(&self.mv7_request(mv7::GET_AUTO)?, &mut state)?;
            return self.set_mv7_auto(*position, state.auto_tone);
        }
        self.send_set(&protocol::cmd_set_auto_position(self.next_seq(), position))
    }

    pub fn set_auto_tone(&self, tone: &protocol::AutoTone) -> Result<()> {
        if self.model == DeviceModel::Mv7 {
            let mut state = DeviceState::default();
            mv7::apply_response(&self.mv7_request(mv7::GET_AUTO)?, &mut state)?;
            return self.set_mv7_auto(state.auto_position, *tone);
        }
        self.send_set(&protocol::cmd_set_auto_tone(self.next_seq(), tone))
    }

    pub fn set_auto_gain(&self, gain: &protocol::AutoGain) -> Result<()> {
        self.send_set(&protocol::cmd_set_auto_gain(self.next_seq(), gain))
    }

    pub fn set_phantom(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_phantom(self.next_seq(), enabled))
    }

    pub fn set_monitor_mix(&self, mix: u8) -> Result<()> {
        if self.model == DeviceModel::Mv7 {
            return self.mv7_set(mv7::cmd_mix(mix));
        }
        self.send_set(&protocol::cmd_set_mix(self.next_seq(), mix))
    }

    pub fn set_limiter(&self, enabled: bool) -> Result<()> {
        if self.model == DeviceModel::Mv7 {
            return self.mv7_set(mv7::cmd_limiter(enabled));
        }
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_limiter(self.next_seq(), enabled),
            _ => protocol::cmd_set_limiter(self.next_seq(), enabled),
        };
        self.send_set(&pkt)
    }

    pub fn set_compressor(&self, preset: &protocol::CompressorPreset) -> Result<()> {
        if self.model == DeviceModel::Mv7 {
            return self.mv7_set(mv7::cmd_compressor(*preset));
        }
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_compressor(self.next_seq(), preset),
            _ => protocol::cmd_set_compressor(self.next_seq(), preset),
        };
        self.send_set(&pkt)
    }

    pub fn set_eq_enable(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_enable(self.next_seq(), enabled))
    }

    pub fn set_eq_band_enable(&self, band: usize, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_band_enable(
            self.next_seq(),
            band,
            enabled,
        ))
    }

    pub fn set_eq_band_gain(&self, band: usize, gain_tenths: i16) -> Result<()> {
        self.send_set(&protocol::cmd_set_eq_band_gain(
            self.next_seq(),
            band,
            gain_tenths,
            self.model,
        ))
    }

    pub fn set_lock(&self, locked: bool) -> Result<()> {
        if self.model == DeviceModel::Mv7 {
            return self.mv7_set(mv7::cmd_lock(locked));
        }
        self.send_set(&cmd_set_lock(self.next_seq(), locked))
    }

    // ── MV6 and MV7+ shared SET commands ─────────────────────────────────────

    pub fn set_mv6_denoiser(&self, enabled: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_denoiser(self.next_seq(), enabled),
            _ => protocol::cmd_set_mv6_denoiser(self.next_seq(), enabled),
        };
        self.send_set(&pkt)
    }

    pub fn set_mv6_popper_stopper(&self, enabled: bool) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_popper_stopper(self.next_seq(), enabled),
            _ => protocol::cmd_set_mv6_popper_stopper(self.next_seq(), enabled),
        };
        self.send_set(&pkt)
    }

    /// Disable or enable the physical mute button. MV6 and MV7+ share identical framing here.
    pub fn set_mv6_mute_btn_disable(&self, disabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_mute_btn_disable(
            self.next_seq(),
            disabled,
        ))
    }

    pub fn set_mv6_tone(&self, tone: i8) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_tone(self.next_seq(), tone),
            _ => protocol::cmd_set_mv6_tone(self.next_seq(), tone),
        };
        self.send_set(&pkt)
    }

    pub fn set_mv6_gain_lock(&self, locked: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv6_gain_lock(self.next_seq(), locked))
    }

    /// Set monitor mic mix. MV7+ uses cmd_set_mv7_mic_mix (HDR_CONST=0x00 with prefix=0x00).
    pub fn set_mv6_monitor_mix(&self, mix: u8) -> Result<()> {
        let pkt = match self.model {
            DeviceModel::Mv7Plus => protocol::cmd_set_mv7_mic_mix(self.next_seq(), mix),
            _ => protocol::cmd_set_mv6_mix(self.next_seq(), mix),
        };
        self.send_set(&pkt)
    }

    // ── MV7+ exclusive SET commands ───────────────────────────────────────────

    pub fn set_mv7_playback_mix(&self, mix: u8) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_playback_mix(self.next_seq(), mix))
    }

    pub fn set_mv7_reverb_output(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_reverb_output(
            self.next_seq(),
            enabled,
        ))
    }

    pub fn set_mv7_reverb_monitor(&self, enabled: bool) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_reverb_monitor(
            self.next_seq(),
            enabled,
        ))
    }

    pub fn set_mv7_reverb_type(&self, rtype: &protocol::ReverbType) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_reverb_type(self.next_seq(), rtype))
    }

    pub fn set_mv7_reverb_intensity(&self, intensity: u8) -> Result<()> {
        self.send_set(&protocol::cmd_set_mv7_reverb_intensity(
            self.next_seq(),
            intensity,
        ))
    }

    pub fn set_mv7_led_behavior(&self, behavior: protocol::LedBehavior) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_behavior(self.next_seq(), behavior))
    }

    pub fn set_mv7_led_brightness(&self, brightness: protocol::LedBrightness) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_brightness(self.next_seq(), brightness))
    }

    pub fn set_mv7_led_live_theme(&self, theme: protocol::LedLiveTheme) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_live_theme(self.next_seq(), theme))
    }

    pub fn set_mv7_led_solid_theme(&self, theme: protocol::LedSolidTheme) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_solid_theme(self.next_seq(), theme))
    }

    pub fn set_mv7_led_pulsing_theme(&self, theme: protocol::LedPulsingTheme) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_pulsing_theme(self.next_seq(), theme))
    }

    pub fn set_mv7_led_solid_color(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_solid_color(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    pub fn set_mv7_led_pulsing_color(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_pulsing_color(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    pub fn set_mv7_led_live_edge(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_live_edge(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    pub fn set_mv7_led_live_middle(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_live_middle(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    pub fn set_mv7_led_live_interior(&self, rgb: [u8; 3]) -> Result<()> {
        self.send_set(&cmd_set_mv7_led_live_interior(
            self.next_seq(),
            rgb[0],
            rgb[1],
            rgb[2],
        ))
    }

    /// Send a factory reset to the MV7+.
    ///
    /// The device disconnects and re-enumerates immediately; no CONFIRM is sent.
    /// After this call succeeds the device handle is stale — do not use it again.
    pub fn factory_reset(&self) -> Result<()> {
        anyhow::ensure!(
            self.model == DeviceModel::Mv7Plus,
            "Factory reset requires an MV7+"
        );
        self.write(&cmd_factory_reset(self.next_seq()))
    }
}

/// Information about a detected Shure device, returned by [`list_devices`].
pub struct DeviceInfo {
    pub path: String,
    pub serial: String,
    pub model: DeviceModel,
}

/// Returns `true` if `pid` is one of the five supported Shure device PIDs.
fn is_supported_pid(pid: u16) -> bool {
    pid == MV7_PID || pid == PID || pid == MVX2U_GEN2_PID || pid == MV6_PID || pid == MV7_PLUS_PID
}

/// Enumerate supported Shure devices, one entry per physical device.
///
/// Two-pass approach:
/// 1. Filter to VID/PID candidates.
/// 2. If any candidate advertises a vendor-defined HID usage page (0xFF00–0xFFFF),
///    restrict to those — this drops the Windows telephony/mute-button collection
///    (usage page 0x000B) that appears as a phantom second entry on Windows.
///    Linux backends sometimes report usage_page=0 (descriptor not parsed); in
///    that case no candidate looks vendor-defined, so all are kept and step 3
///    handles deduplication.
/// 3. Deduplicate by path — on Linux all collections share one /dev/hidrawN path.
const VENDOR_USAGE_PAGE_MIN: u16 = 0xFF00;

fn shure_devices(api: &HidApi) -> Vec<&hidapi::DeviceInfo> {
    let candidates: Vec<&hidapi::DeviceInfo> = api
        .device_list()
        .filter(|d| d.vendor_id() == VID && is_supported_pid(d.product_id()))
        .collect();

    let has_vendor = candidates
        .iter()
        .any(|d| d.usage_page() >= VENDOR_USAGE_PAGE_MIN);

    let mut seen_paths = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|d| !has_vendor || d.usage_page() >= VENDOR_USAGE_PAGE_MIN)
        .filter(|d| seen_paths.insert(d.path().to_string_lossy().into_owned()))
        .collect()
}

/// Probe the system for supported Shure devices without opening them.
pub fn list_devices() -> Vec<DeviceInfo> {
    let Ok(api) = HidApi::new() else {
        return vec![];
    };
    shure_devices(&api)
        .into_iter()
        .map(|d| DeviceInfo {
            path: d.path().to_string_lossy().into_owned(),
            serial: d.serial_number().unwrap_or("(unknown)").to_owned(),
            model: match d.product_id() {
                MV6_PID => DeviceModel::Mv6,
                MVX2U_GEN2_PID => DeviceModel::Mvx2uGen2,
                MV7_PLUS_PID => DeviceModel::Mv7Plus,
                MV7_PID => DeviceModel::Mv7,
                _ => DeviceModel::Mvx2u,
            },
        })
        .collect()
}
