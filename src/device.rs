//! Finding the mouse and talking to it through Linux hidraw (no hidapi/libusb needed).

use crate::profile::Profile;
use crate::protocol::{self, *};
use anyhow::{anyhow, bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// rivalcfg waits this long after each command; the mouse drops commands sent faster.
const COMMAND_DELAY: Duration = Duration::from_millis(50);
const READ_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Wired,
    Wireless,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Wired => "USB cable",
            Mode::Wireless => "2.4 GHz dongle",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub node: PathBuf,
    pub product_id: u16,
    pub mode: Mode,
    pub edition: &'static str,
}

impl DeviceInfo {
    pub fn name(&self) -> String {
        let ed = if self.edition.is_empty() { String::new() } else { format!(" {} Edition", self.edition) };
        format!("SteelSeries Aerox 5 Wireless{ed}")
    }
}

/// Every Aerox 5 Wireless configuration interface present, wired ones first.
pub fn find_devices() -> Vec<DeviceInfo> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/hidraw") else { return found };
    for e in entries.flatten() {
        let Ok(dev) = fs::canonicalize(e.path().join("device")) else { continue };
        // .../3-2.2:1.3/0003:1038:1854.000A
        let Some(hid) = dev.file_name().and_then(|n| n.to_str()) else { continue };
        let parts: Vec<&str> = hid.split([':', '.']).collect();
        if parts.len() < 3 {
            continue;
        }
        let (Ok(vid), Ok(pid)) = (u16::from_str_radix(parts[1], 16), u16::from_str_radix(parts[2], 16))
        else {
            continue;
        };
        if vid != VENDOR_ID {
            continue;
        }
        let iface = dev
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .and_then(|n| n.rsplit('.').next())
            .and_then(|n| n.parse::<u8>().ok());
        if iface != Some(CONFIG_INTERFACE) {
            continue;
        }
        let (mode, edition) = if let Some((_, ed)) = WIRED_PIDS.iter().find(|(p, _)| *p == pid) {
            (Mode::Wired, *ed)
        } else if let Some((_, ed)) = WIRELESS_PIDS.iter().find(|(p, _)| *p == pid) {
            (Mode::Wireless, *ed)
        } else {
            continue;
        };
        found.push(DeviceInfo { node: PathBuf::from("/dev").join(e.file_name()), product_id: pid, mode, edition });
    }
    found.sort_by_key(|d| (d.mode == Mode::Wireless, d.node.clone()));
    found
}

pub struct Mouse {
    pub info: DeviceInfo,
    file: File,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub battery: Option<(u8, bool)>,
    pub firmware: Option<String>,
}

impl Mouse {
    pub fn open(info: &DeviceInfo) -> Result<Mouse> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&info.node)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    anyhow!(
                        "no permission to open {} — install the udev rule \
                         (packaging/70-aerox5.rules) and replug the mouse/dongle",
                        info.node.display()
                    )
                } else {
                    anyhow!("opening {}: {e}", info.node.display())
                }
            })?;
        Ok(Mouse { info: info.clone(), file })
    }

    /// Open the best connection: if the cable is in, talk over the cable (the dongle
    /// stays enumerated but the mouse ignores it). Otherwise the dongle.
    pub fn open_first() -> Result<Mouse> {
        let devs = find_devices();
        if devs.is_empty() {
            bail!("no SteelSeries Aerox 5 Wireless found (cable or dongle)");
        }
        let mut last_err = None;
        for d in &devs {
            match Mouse::open(d) {
                Ok(m) => return Ok(m),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap())
    }

    fn drain(&mut self) {
        let mut buf = [0u8; PACKET_SIZE];
        while matches!(self.file.read(&mut buf), Ok(n) if n > 0) {}
    }

    fn read_timeout(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd for the duration of the call.
            let r = unsafe { libc::poll(&mut pfd, 1, left.as_millis() as i32) };
            if r <= 0 {
                return None;
            }
            let mut buf = [0u8; PACKET_SIZE];
            match self.file.read(&mut buf) {
                Ok(n) if n > 0 => return Some(buf[..n].to_vec()),
                _ => continue,
            }
        }
    }

    /// Send one command (wired form; the wireless flag is added here). Returns the
    /// readback when one arrives (always expected in wireless mode).
    pub fn command(&mut self, cmd: &[u8]) -> Result<Option<Vec<u8>>> {
        if cmd.is_empty() || cmd.len() > PACKET_SIZE {
            bail!("bad command length {}", cmd.len());
        }
        let wireless = self.info.mode == Mode::Wireless;
        let mut pkt = [0u8; PACKET_SIZE + 1]; // byte 0 = report id 0 (none)
        pkt[1..=cmd.len()].copy_from_slice(cmd);
        if wireless {
            pkt[1] |= WIRELESS_FLAG;
        }
        self.drain();
        self.file
            .write_all(&pkt)
            .with_context(|| format!("writing to {}", self.info.node.display()))?;
        let reply = if wireless { self.read_timeout(READ_TIMEOUT) } else { None };
        sleep(COMMAND_DELAY);
        Ok(reply)
    }

    /// Send a query and wait for the reply. With `echo`, only a reply whose first byte
    /// echoes the command counts (battery); the firmware reply has no echo.
    fn query(&mut self, cmd: u8, echo: bool) -> Result<Option<Vec<u8>>> {
        let wireless = self.info.mode == Mode::Wireless;
        let expect = if wireless { cmd | WIRELESS_FLAG } else { cmd };
        let mut pkt = [0u8; PACKET_SIZE + 1];
        pkt[1] = expect;
        self.drain();
        self.file.write_all(&pkt).with_context(|| format!("writing to {}", self.info.node.display()))?;
        let deadline = Instant::now() + READ_TIMEOUT;
        let mut result = None;
        while let Some(r) = self.read_timeout(deadline.saturating_duration_since(Instant::now())) {
            if !echo || r.first() == Some(&expect) {
                result = Some(r);
                break;
            }
        }
        sleep(COMMAND_DELAY);
        Ok(result)
    }

    pub fn battery(&mut self) -> Result<Option<(u8, bool)>> {
        Ok(self.query(CMD_BATTERY, true)?.and_then(|r| decode_battery(&r)))
    }

    pub fn firmware(&mut self) -> Result<Option<String>> {
        let flagged = CMD_FIRMWARE | WIRELESS_FLAG;
        Ok(self.query(CMD_FIRMWARE, false)?.and_then(|r| {
            // Wired: "1.11.4\0…". Skip a leading command echo should the dongle add one.
            let body = if matches!(r.first(), Some(&b) if b == CMD_FIRMWARE || b == flagged) { &r[1..] } else { &r[..] };
            let s: String = body
                .iter()
                .take_while(|b| **b != 0)
                .filter(|b| b.is_ascii_graphic() || **b == b' ' )
                .map(|b| *b as char)
                .collect();
            (!s.trim().is_empty()).then(|| s.trim().to_string())
        }))
    }

    pub fn status(&mut self) -> Status {
        Status { battery: self.battery().ok().flatten(), firmware: self.firmware().ok().flatten() }
    }

    /// Only the lighting commands (for live preview; not saved to flash).
    pub fn apply_lighting(&mut self, p: &Profile) -> Result<()> {
        let l = &p.lighting;
        if l.rainbow {
            self.command(&protocol::encode_rainbow())?;
        } else {
            for (i, c) in l.zones.iter().enumerate() {
                self.command(&protocol::encode_zone_color(i as u8, *c))?;
            }
        }
        self.command(&protocol::encode_reactive(l.reactive.then_some(l.reactive_color)))?;
        Ok(())
    }

    /// Write every setting in the profile; `save` also stores them in the mouse's flash
    /// so they survive power-off and work on any computer.
    pub fn apply(&mut self, p: &Profile, save: bool) -> Result<()> {
        let errors = p.validate();
        if !errors.is_empty() {
            bail!("profile is invalid: {}", errors.join("; "));
        }
        let buttons = p.button_array();
        self.command(&protocol::encode_buttons(&buttons).map_err(|e| anyhow!(e))?)?;
        self.command(&protocol::encode_dpi(&p.sensitivity.presets, p.sensitivity.active).map_err(|e| anyhow!(e))?)?;
        self.command(&protocol::encode_polling(p.sensitivity.polling_rate).map_err(|e| anyhow!(e))?)?;
        self.command(&protocol::encode_sleep(p.power.sleep_minutes))?;
        self.command(&protocol::encode_dim(p.power.dim_seconds))?;
        // Start-up lighting first, so the live colour commands below win either way.
        self.command(&protocol::encode_default_lighting(p.lighting.rainbow, p.lighting.reactive))?;
        self.apply_lighting(p)?;
        if save {
            self.command(&protocol::encode_save())?;
        }
        Ok(())
    }
}
