# shurectl

An open-source terminal UI configurator for Shure USB audio interfaces and microphones on Linux, macOS, and Windows. Replaces the Windows/Mac-only ShurePlus MOTIV Desktop app.

![Project Example Screenshot](images/shurectl.png)

---

## Supported Devices
- MVX2U Gen 1 — Digital Audio Interface
- MVX2U Gen 2 — Digital Audio Interface
- MV6 — USB Gaming Microphone
- MV7 — Original USB/XLR Dynamic Microphone (`14ed:1012`)
- MV7+ — USB/XLR Dynamic Microphone

---

## Features

### All Devices
- **Gain Control** — Auto Level / Manual toggle
- **Mic Mute** — toggle mute
- **Monitor Mix** — mic vs. playback blend slider
- **Compressor** — Off / Light / Medium / Heavy
- **High-Pass Filter** — Off / 75 Hz / 150 Hz; original MV7 uses an on/off filter
- **Real-time Level Meter** — dBFS input meter with peak-hold display
- **4 Preset Slots** — save and load named presets stored as TOML in `~/.config/shurectl/presets/`
- **Device Info** — factory serial number, device name, and firmware version
- **Demo mode** — run without a device plugged in (`--demo`)

### MVX2U Gen 1
- **Gain range** — 0–60 dB
- **Phantom Power** — 48V on/off; warns if enabled when muting ribbon mics
- **5-band Parametric EQ** — per-band enable, gain (−8 to +6 dB in 2 dB steps)
- **Limiter** — enable/disable
- **Panel Lock** — lock the physical panel controls on the device
- **Auto Level controls** — mic position (Near/Far), tone (Dark/Natural/Bright), gain environment (Quiet/Normal/Loud)

### MVX2U Gen 2 -  Builds on Gen 1 features with the following
- **5-band Parametric EQ** — gain (−8 to +6 dB in 0.5 dB steps)
- **Tone** — Dark / Natural / Bright
- **Real-time Denoiser** — enable/disable
- **Popper Stopper** — enable/disable
- **Gain Lock** — hardware freeze of the gain control (Manual mode only)

### MV6
- **Gain range** — 0–36 dB
- **Tone** — Dark / Natural / Bright
- **Real-time Denoiser** — enable/disable
- **Popper Stopper** — enable/disable
- **Mute Button Disable** — prevent accidental mutes
- **Gain Lock** — hardware freeze of the gain control (Manual mode only)

### Original MV7
- **Manual gain** — 0–36 dB in 1.5 dB steps
- **Auto Level** — Near/Far distance and Natural/Dark/Bright tone
- **EQ** — independent High-pass Filter and Presence Boost switches
- **Dynamics** — limiter and Off/Light/Medium/Heavy compressor in Manual mode
- **Headphones** — monitor mix, volume (−24 to 0 dB), and separate headphone mute
- **Config Lock** — unlock before changing settings or loading presets
- **LED Panel** — Live Meters and Dim LEDs

Run `shurectl --demo mv7` to explore these controls without hardware. EQ and Dynamics
are unavailable in Auto Level mode. There is no phantom power, five-band EQ,
gain-environment selector, denoiser, reverb, or RGB lighting on this model.

Original MV7 settings are read back after changes. A failed read disables further
changes until `r` successfully refreshes the device. Loading a preset applies the
active mode's controls; explicit gain writes are sent only in Manual mode.
Older presets without an MV7 section preserve headphone and LED settings.
Fractional gain values survive preset save/load; older integer values still load.
Returning from Manual to Auto restores the distance and tone selected during the
current session. Switching modes reloads DSP settings; save a preset before changing
modes if you want to recover a particular manual setup.

Monitor mix follows MOTIV's measured attenuation curve. Positions 0–2% share the same
hardware floor and read back as 0%. Intermediate positions use interpolation.
The limiter register has been checked by write/readback; its effect on recorded audio
has not yet been verified.

Tested on an original MV7 running firmware `0.0.52.0` on macOS with a USB connection.
Linux and Windows hardware behavior has not been verified for this model.

### MV7+ - Builds on MV6 features with the following
- **Reverb** — output and monitor enable/disable; Type: Plate / Hall / Studio; Intensity: 0–100%
- **LED Panel** — Behavior (Live / Pulsing / Solid), Brightness (Low / Med / High / Max), theme and custom RGB color per mode

---

## Installing

### Via the AUR (Arch Linux)

```bash
paru -S shurectl        # or: yay -S shurectl
```

The package installs the udev rules to `/usr/lib/udev/rules.d/62-shure.rules` for you, so
the [Linux setup step](#linux--udev-rules-required-for-non-root-access) below can be
skipped — **replug the device after installing** so udev applies the new rules to it.

### Via Homebrew (macOS)

```bash
brew install humblemonk/shurectl/shurectl
```

Updates arrive through `brew upgrade` like any other formula. The formula builds from
source, so the first install pulls in a Rust toolchain and takes a minute or two.

### Via cargo install

```bash
cargo install --git https://github.com/Humblemonk/shurectl.git
```

### From source

```bash
git clone https://github.com/Humblemonk/shurectl.git
cd shurectl
cargo build --release
```

The binary will be at `target/release/shurectl`.

To install system-wide:

```bash
sudo install -m 755 target/release/shurectl /usr/local/bin/
```

Or for your user only:

```bash
install -m 755 target/release/shurectl ~/.local/bin/
```

On Windows, building from source requires the MSVC toolchain. Install
[Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
with the "Desktop development with C++" workload, then ensure MSVC is your active rustup
toolchain:

```
rustup default stable-x86_64-pc-windows-msvc
```

---

## Platform Setup

### Linux — udev Rules (Required for Non-Root Access)

Skip this section if you installed via the AUR package — it ships these rules already.

Without a udev rule, `/dev/hidrawN` for the device is only accessible by root.

Create `/etc/udev/rules.d/62-shure.rules`:

```
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1013", TAG+="uaccess"
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1033", TAG+="uaccess"
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1026", TAG+="uaccess"
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1012", TAG+="uaccess"
ACTION!="remove", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="14ed", ATTRS{idProduct}=="1019", TAG+="uaccess"
```

Then reload udev and replug your device:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Verify the device appears:

```bash
shurectl --list
# Found 1 Shure device(s):
#   /dev/hidraw2 | Shure MVX2U Gen 2 | S/N: MVX2U GEN 2#2-a646351d...
```

### macOS

On macOS, IOKit grants user-space access to HID devices without extra configuration.
Plug in your device and run `shurectl --list` to confirm detection.

### Windows

Windows grants user-space HID access out of the box via the `setupapi` backend — no driver installation or equivalent of a udev rule is needed. Plug in your device and run `shurectl --list` to confirm detection. Device paths look like `\\?\HID#VID_14ED&PID_1026&...` rather than `/dev/hidrawN`.

---

## Usage

```bash
shurectl                         # Connect to first detected device and launch TUI
shurectl --device <path>         # Connect to a specific device (use --list to find paths)
shurectl --demo                  # Run without a device (explore the UI)
shurectl --list                  # List detected Shure devices and exit
shurectl --mute                  # Toggle mute without launching the TUI
shurectl --mute on               # Mute
shurectl --mute off              # Unmute
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Switch section |
| `↑` / `k` | Focus previous control |
| `↓` / `j` | Focus next control |
| `←` / `h` | Decrease value |
| `→` / `l` | Increase value |
| `Enter` / `Space` | Toggle boolean / cycle option |
| `f` | Flatten EQ — zero bands on Gen 1/Gen 2; turn both filters off on original MV7 (Manual mode) |
| `r` | Refresh state from device |
| `s` | Save preset (on Presets tab, focused slot) |
| `d` | Delete preset (on Presets tab, focused slot) |
| `?` | Toggle help overlay |
| `q` / `Ctrl+C` | Quit |

---

## Presets

Presets are stored as human-readable TOML files in `~/.config/shurectl/presets/`:

```
~/.config/shurectl/presets/
├── preset_1.toml
├── preset_2.toml
├── preset_3.toml
└── preset_4.toml
```

Each file captures all configurable DSP settings (gain, mode, EQ, dynamics, monitor mix, etc.)
but not hardware-identity fields like serial number or firmware version. Files are hand-editable.

On the **Presets tab**:
- Navigate to a slot with `↑`/`↓`
- Press `Enter` on the name field to rename it (type, then `Enter` to confirm or `Esc` to cancel)
- Press `Enter` on the actions row to load a filled preset — all settings are applied to the device immediately
- Press `s` to save the current device state into the focused slot
- Press `d` to delete the focused slot

---

## Troubleshooting

**"Cannot open device"** — device not found or a permissions issue.
Run `shurectl --list` to check detection. On Linux, try `sudo shurectl` to confirm it's a udev permissions issue. On macOS and Windows, ensure no other software (e.g. ShurePlus MOTIV) has exclusive access to the device.

**Gain slider is greyed out in Auto Level mode** — This is correct hardware behaviour;
the device ignores gain commands in Auto Level mode. Switch to Manual mode first.

**PipeWire/PulseAudio volume vs. device gain** — This tool controls the **hardware DSP gain**
on the device itself, not the OS capture volume level. Both can be set independently.

---

## Original MV7 protocol

The original MV7 uses an ASCII command console on the vendor HID interface
(usage page `0xFF00`). Each command is NUL-terminated in a 64-byte payload, preceded
by report ID zero for hidapi writes. It does **not** use the other models' binary
framing, CRC, or SET/CONFIRM packets. Connecting selects `su adm` without rebooting
its DSP. Firmware-update and raw memory commands are not implemented.

| Setting | Commands / blocks |
| --- | --- |
| Mode, gain, microphone mute, configuration lock | `dspMode`, `inputGain`, `micMute`, `lock` |
| Headphone mute and volume, panel LEDs | `audioMute`, `volume`, `meterMode`, `dimMode` |
| Monitor mix, compressor, limiter | Blocks `22`, `19`, `1F` |
| High-pass/presence switches | Block `31` |
| Auto Level profile | `dspMode` (`2`–`7`); block `34` readback |

Replies are matched to the requested property, validated, and bounded by a three-second
timeout. All six Auto Level profiles (`dspMode=2` through `7`) are accepted. Gain is
readable in Auto mode but can only be changed in Manual mode. The reported MV7 serial
comes from `serialNum`, the device's silicon identifier.

## Acknowledgements

Initial protocol reverse-engineering credit goes to **PennRobotics** and the
[shux project](https://gitlab.com/PennRobotics/shux) (Apache 2.0), without which
this tool would not exist. If you find shurectl useful, consider starring their
repository.

Original MV7 protocol references: [mv7config](https://github.com/matteodelabre/mv7config),
[OpenMV7](https://github.com/vladikk/openmv7), and the
[MV7 vendor HID protocol documentation](https://github.com/schlarpc/shure-mv7-firmware-re/blob/main/hid-protocol.md).
The original MV7 command behavior was also verified directly on connected hardware.
Gain-step observations and measured MOTIV monitor-mix points come from
[biozz's original MV7 PR #89](https://github.com/Humblemonk/shurectl/pull/89).

---

## Legal

Protocol implementation is based on publicly documented USB HID packet captures
by PennRobotics (shux project, Apache 2.0) as well as author's own usbmon captures. No Shure software was used, decompiled,
or examined in the creation of this tool.

shurectl is not affiliated with or endorsed by Shure Incorporated.
