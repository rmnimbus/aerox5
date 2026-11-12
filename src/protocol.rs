//! Aerox 5 Wireless HID protocol: turns settings into the raw command bytes.
//!
//! Byte layouts come from rivalcfg's `aerox5_wireless_wired.py` profile (the reference
//! implementation that is known to work on this mouse) and OpenRGB's
//! `SteelSeriesAeroxWirelessController` (64-byte packets, firmware query).
//!
//! Every command here is the *wired* form. In 2.4 GHz mode the first byte gets
//! [`WIRELESS_FLAG`] OR-ed in (done by [`crate::device`]), and the dongle answers each
//! command with a 64-byte readback.

use crate::tables::{DPI_CODES, KEYS, KEY_ALIASES, MEDIA_ALIASES, MEDIA_KEYS};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub const VENDOR_ID: u16 = 0x1038;
/// (product id, edition) for the USB-cable connection.
pub const WIRED_PIDS: &[(u16, &str)] = &[(0x1854, ""), (0x185E, "Destiny 2"), (0x1862, "Diablo IV")];
/// (product id, edition) for the 2.4 GHz dongle.
pub const WIRELESS_PIDS: &[(u16, &str)] =
    &[(0x1852, ""), (0x185C, "Destiny 2"), (0x1860, "Diablo IV")];
/// The vendor-specific HID interface that takes configuration commands.
pub const CONFIG_INTERFACE: u8 = 3;
/// Output/input report size (from the interface's report descriptor).
pub const PACKET_SIZE: usize = 64;
pub const WIRELESS_FLAG: u8 = 0b0100_0000;

pub const CMD_SAVE: u8 = 0x11;
pub const CMD_COLOR: u8 = 0x21;
pub const CMD_RAINBOW: u8 = 0x22;
pub const CMD_DIM: u8 = 0x23;
pub const CMD_REACTIVE: u8 = 0x26;
pub const CMD_DEFAULT_LIGHTING: u8 = 0x27;
pub const CMD_SLEEP: u8 = 0x29;
pub const CMD_BUTTONS: u8 = 0x2A;
pub const CMD_POLLING: u8 = 0x2B;
pub const CMD_DPI: u8 = 0x2D;
pub const CMD_FIRMWARE: u8 = 0x90;
pub const CMD_BATTERY: u8 = 0x92;

pub const DPI_MIN: u32 = 100;
pub const DPI_MAX: u32 = 18000;
pub const DPI_STEP: u32 = 100;
pub const MAX_DPI_PRESETS: usize = 5;
pub const SLEEP_MAX_MIN: u32 = 20;
pub const DIM_MAX_S: u32 = 1200;
pub const POLLING_RATES: &[u16] = &[125, 250, 500, 1000];

// ---------------------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------------------

/// A physical input on the mouse that can be remapped (order = slot order in the packet).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Button {
    Button1,
    Button2,
    Button3,
    Button4,
    Button5,
    Button6,
    Button7,
    Button8,
    Button9,
    ScrollUp,
    ScrollDown,
}

impl Button {
    pub const ALL: [Button; 11] = [
        Button::Button1,
        Button::Button2,
        Button::Button3,
        Button::Button4,
        Button::Button5,
        Button::Button6,
        Button::Button7,
        Button::Button8,
        Button::Button9,
        Button::ScrollUp,
        Button::ScrollDown,
    ];

    /// Short name as rivalcfg spells it (`button1` … `scrolldown`).
    pub fn key(self) -> &'static str {
        match self {
            Button::Button1 => "button1",
            Button::Button2 => "button2",
            Button::Button3 => "button3",
            Button::Button4 => "button4",
            Button::Button5 => "button5",
            Button::Button6 => "button6",
            Button::Button7 => "button7",
            Button::Button8 => "button8",
            Button::Button9 => "button9",
            Button::ScrollUp => "scrollup",
            Button::ScrollDown => "scrolldown",
        }
    }

    pub fn from_key(s: &str) -> Option<Button> {
        Button::ALL.into_iter().find(|b| b.key().eq_ignore_ascii_case(s))
    }

    /// Human label. Buttons 7–9 are the extra ones the Aerox 5 adds over the Aerox 3;
    /// SteelSeries GG labels them as the side "flick" switch and the top button.
    pub fn label(self) -> &'static str {
        match self {
            Button::Button1 => "Button 1 · left click",
            Button::Button2 => "Button 2 · right click",
            Button::Button3 => "Button 3 · wheel click",
            Button::Button4 => "Button 4 · side back",
            Button::Button5 => "Button 5 · side forward",
            Button::Button6 => "Button 6 · CPI (behind wheel)",
            Button::Button7 => "Button 7 · side flick (one way)",
            Button::Button8 => "Button 8 · side flick (other way)",
            Button::Button9 => "Button 9 · extra button",
            Button::ScrollUp => "Wheel up",
            Button::ScrollDown => "Wheel down",
        }
    }

    pub fn default_action(self) -> Action {
        match self {
            Button::Button1 => Action::Mouse(1),
            Button::Button2 => Action::Mouse(2),
            Button::Button3 => Action::Mouse(3),
            Button::Button4 => Action::Mouse(4),
            Button::Button5 => Action::Mouse(5),
            Button::Button6 => Action::DpiCycle,
            Button::Button7 | Button::Button8 | Button::Button9 => Action::Disabled,
            Button::ScrollUp => Action::ScrollUp,
            Button::ScrollDown => Action::ScrollDown,
        }
    }
}

/// What a button does. Serialized as a short string: `button1`…`button5`, `disabled`,
/// `dpi`, `scrollup`, `scrolldown`, `key:<Name>`, `media:<Name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Disabled,
    /// Acts as mouse button 1–5.
    Mouse(u8),
    /// Cycles through the CPI presets.
    DpiCycle,
    ScrollUp,
    ScrollDown,
    /// A keyboard key, by canonical name from [`KEYS`].
    Key(String),
    /// A multimedia key, by canonical name from [`MEDIA_KEYS`].
    Media(String),
}

const FIELD_DISABLED: u8 = 0x00;
const FIELD_DPI: u8 = 0x30;
const FIELD_SCROLL_UP: u8 = 0x31;
const FIELD_SCROLL_DOWN: u8 = 0x32;
const FIELD_KEYBOARD: u8 = 0x51;
const FIELD_MEDIA: u8 = 0x61;
const BUTTON_FIELD_LEN: usize = 5;

pub fn key_code(name: &str) -> Option<u8> {
    let canonical = KEY_ALIASES
        .iter()
        .find(|(a, _)| a.eq_ignore_ascii_case(name))
        .map(|(_, c)| *c)
        .unwrap_or(name);
    KEYS.iter().find(|(k, _)| k.eq_ignore_ascii_case(canonical)).map(|(_, c)| *c)
}

pub fn canonical_key(name: &str) -> Option<&'static str> {
    let code = key_code(name)?;
    KEYS.iter().find(|(_, c)| *c == code).map(|(k, _)| *k)
}

pub fn media_code(name: &str) -> Option<u8> {
    let canonical = MEDIA_ALIASES
        .iter()
        .find(|(a, _)| a.eq_ignore_ascii_case(name))
        .map(|(_, c)| *c)
        .unwrap_or(name);
    MEDIA_KEYS.iter().find(|(k, _)| k.eq_ignore_ascii_case(canonical)).map(|(_, c)| *c)
}

pub fn canonical_media(name: &str) -> Option<&'static str> {
    let code = media_code(name)?;
    MEDIA_KEYS.iter().find(|(_, c)| *c == code).map(|(k, _)| *k)
}

impl Action {
    /// The 5-byte slot for this action.
    pub fn encode(&self) -> Result<[u8; BUTTON_FIELD_LEN], String> {
        let mut f = [0u8; BUTTON_FIELD_LEN];
        match self {
            Action::Disabled => f[0] = FIELD_DISABLED,
            Action::Mouse(n @ 1..=5) => f[0] = *n,
            Action::Mouse(n) => return Err(format!("mouse button {n} does not exist (1-5)")),
            Action::DpiCycle => f[0] = FIELD_DPI,
            Action::ScrollUp => f[0] = FIELD_SCROLL_UP,
            Action::ScrollDown => f[0] = FIELD_SCROLL_DOWN,
            Action::Key(k) => {
                f[0] = FIELD_KEYBOARD;
                f[1] = key_code(k).ok_or_else(|| format!("unknown key '{k}'"))?;
            }
            Action::Media(k) => {
                f[0] = FIELD_MEDIA;
                f[1] = media_code(k).ok_or_else(|| format!("unknown media key '{k}'"))?;
            }
        }
        Ok(f)
    }

    pub fn describe(&self) -> String {
        match self {
            Action::Disabled => "Disabled".into(),
            Action::Mouse(1) => "Left click".into(),
            Action::Mouse(2) => "Right click".into(),
            Action::Mouse(3) => "Middle click".into(),
            Action::Mouse(4) => "Back (button 4)".into(),
            Action::Mouse(5) => "Forward (button 5)".into(),
            Action::Mouse(n) => format!("Mouse button {n}"),
            Action::DpiCycle => "Cycle CPI presets".into(),
            Action::ScrollUp => "Scroll up".into(),
            Action::ScrollDown => "Scroll down".into(),
            Action::Key(k) => format!("Key: {k}"),
            Action::Media(k) => format!("Media: {k}"),
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Disabled => write!(f, "disabled"),
            Action::Mouse(n) => write!(f, "button{n}"),
            Action::DpiCycle => write!(f, "dpi"),
            Action::ScrollUp => write!(f, "scrollup"),
            Action::ScrollDown => write!(f, "scrolldown"),
            Action::Key(k) => write!(f, "key:{k}"),
            Action::Media(k) => write!(f, "media:{k}"),
        }
    }
}

impl FromStr for Action {
    type Err = String;
    /// Accepts this crate's own syntax and rivalcfg's bare names (`LeftSuper`, `PlayPause`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        if let Some(k) = s.strip_prefix("key:").or_else(|| s.strip_prefix("Key:")) {
            return canonical_key(k)
                .map(|c| Action::Key(c.to_string()))
                .ok_or_else(|| format!("unknown key '{k}'"));
        }
        if let Some(k) = s.strip_prefix("media:").or_else(|| s.strip_prefix("Media:")) {
            return canonical_media(k)
                .map(|c| Action::Media(c.to_string()))
                .ok_or_else(|| format!("unknown media key '{k}'"));
        }
        match lower.as_str() {
            "disabled" | "disable" | "none" => return Ok(Action::Disabled),
            "dpi" | "cpi" => return Ok(Action::DpiCycle),
            "scrollup" => return Ok(Action::ScrollUp),
            "scrolldown" | "scrolldwn" | "scrolldn" => return Ok(Action::ScrollDown),
            _ => {}
        }
        if let Some(n) = lower.strip_prefix("button").and_then(|n| n.parse::<u8>().ok()) {
            return match n {
                1..=5 => Ok(Action::Mouse(n)),
                // rivalcfg maps button6..9 to their own (unusable) ids; treat as disabled
                // only for 7-9, which have id 0 in the profile.
                7..=9 => Ok(Action::Disabled),
                _ => Err(format!("'{s}' is not a usable mouse button")),
            };
        }
        // rivalcfg style: media keys win over keyboard keys (same precedence as rivalcfg).
        if let Some(c) = canonical_media(s) {
            return Ok(Action::Media(c.to_string()));
        }
        if let Some(c) = canonical_key(s) {
            return Ok(Action::Key(c.to_string()));
        }
        Err(format!("unknown button action '{s}'"))
    }
}

impl Serialize for Action {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Action {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Command 0x2A: all 11 slots, 5 bytes each.
pub fn encode_buttons(map: &[Action; 11]) -> Result<Vec<u8>, String> {
    let mut out = vec![CMD_BUTTONS];
    for (b, a) in Button::ALL.iter().zip(map.iter()) {
        out.extend_from_slice(&a.encode().map_err(|e| format!("{}: {e}", b.key()))?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Sensitivity / polling
// ---------------------------------------------------------------------------------------

pub fn snap_dpi(dpi: u32) -> u32 {
    let d = dpi.clamp(DPI_MIN, DPI_MAX);
    ((d + DPI_STEP / 2) / DPI_STEP) * DPI_STEP
}

pub fn dpi_code(dpi: u32) -> u8 {
    let d = snap_dpi(dpi);
    DPI_CODES.iter().find(|(v, _)| *v == d).map(|(_, c)| *c).expect("table covers range")
}

/// Command 0x2D: `[count, selected (0-based), code…]`.
pub fn encode_dpi(presets: &[u32], selected: usize) -> Result<Vec<u8>, String> {
    if presets.is_empty() || presets.len() > MAX_DPI_PRESETS {
        return Err(format!("need 1-{MAX_DPI_PRESETS} CPI presets, got {}", presets.len()));
    }
    if selected >= presets.len() {
        return Err("selected CPI preset is out of range".into());
    }
    let mut out = vec![CMD_DPI, presets.len() as u8, selected as u8];
    out.extend(presets.iter().map(|d| dpi_code(*d)));
    Ok(out)
}

pub fn encode_polling(hz: u16) -> Result<Vec<u8>, String> {
    let code = match hz {
        1000 => 0x00,
        500 => 0x01,
        250 => 0x02,
        125 => 0x03,
        _ => return Err(format!("unsupported polling rate {hz} Hz (125/250/500/1000)")),
    };
    Ok(vec![CMD_POLLING, code])
}

// ---------------------------------------------------------------------------------------
// Lighting
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
    pub fn parse(s: &str) -> Option<Rgb> {
        let named = [
            ("white", "ffffff"), ("silver", "c0c0c0"), ("gray", "808080"), ("black", "000000"),
            ("red", "ff0000"), ("maroon", "800000"), ("yellow", "ffff00"), ("olive", "808000"),
            ("lime", "00ff00"), ("green", "008000"), ("aqua", "00ffff"), ("teal", "008080"),
            ("blue", "0000ff"), ("navy", "000080"), ("fuchsia", "ff00ff"), ("purple", "800080"),
        ];
        let s = s.trim();
        let h = named
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(s))
            .map(|(_, h)| *h)
            .unwrap_or_else(|| s.trim_start_matches('#'));
        let h: String = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect() } else { h.into() };
        if h.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(&h, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}

impl Serialize for Rgb {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}
impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("bad color '{s}'")))
    }
}

/// The three LEDs of the light strip, top to bottom.
pub const ZONE_NAMES: [&str; 3] = ["Top", "Middle", "Bottom"];

/// Command 0x21: static colour for one zone (0 = top, 1 = middle, 2 = bottom).
pub fn encode_zone_color(zone: u8, c: Rgb) -> Vec<u8> {
    vec![CMD_COLOR, 0x01, zone, c.0, c.1, c.2]
}

/// Command 0x22 0xFF: built-in rainbow cycle (any static colour command ends it).
pub fn encode_rainbow() -> Vec<u8> {
    vec![CMD_RAINBOW, 0xFF]
}

/// Command 0x26: flash colour on click, or off.
pub fn encode_reactive(c: Option<Rgb>) -> Vec<u8> {
    match c {
        Some(c) => vec![CMD_REACTIVE, 0x01, 0x00, c.0, c.1, c.2],
        None => vec![CMD_REACTIVE, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

/// Command 0x27: lighting the mouse starts up with. `[rainbow, reactive]`.
pub fn encode_default_lighting(rainbow: bool, reactive: bool) -> Vec<u8> {
    vec![CMD_DEFAULT_LIGHTING, rainbow as u8, reactive as u8]
}

// ---------------------------------------------------------------------------------------
// Power
// ---------------------------------------------------------------------------------------

fn le24(v: u32) -> [u8; 3] {
    [v as u8, (v >> 8) as u8, (v >> 16) as u8]
}

/// Command 0x29: idle minutes before the mouse sleeps (0 = never).
pub fn encode_sleep(minutes: u32) -> Vec<u8> {
    let ms = minutes.min(SLEEP_MAX_MIN) * 60_000;
    let mut v = vec![CMD_SLEEP];
    v.extend_from_slice(&le24(ms));
    v
}

/// Command 0x23: idle seconds before the LEDs dim (0 = never).
pub fn encode_dim(seconds: u32) -> Vec<u8> {
    let ms = seconds.min(DIM_MAX_S) * 1000;
    let mut v = vec![CMD_DIM, 0x0F, 0x01, 0x00, 0x00];
    v.extend_from_slice(&le24(ms));
    v
}

pub fn encode_save() -> Vec<u8> {
    vec![CMD_SAVE, 0x00]
}

/// Battery readback: `data[1]` = charging flag (bit 7) + level in 5 % steps, offset 1.
pub fn decode_battery(data: &[u8]) -> Option<(u8, bool)> {
    let b = *data.get(1)?;
    let charging = b & 0x80 != 0;
    let raw = (b & 0x7F) as i32;
    let level = (raw - 1) * 5;
    (0..=100).contains(&level).then_some((level as u8, charging))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> [Action; 11] {
        Button::ALL.map(|b| b.default_action())
    }

    #[test]
    fn default_buttons_match_rivalcfg() {
        let p = encode_buttons(&defaults()).unwrap();
        assert_eq!(p.len(), 1 + 55);
        assert_eq!(p[0], 0x2A);
        let slot = |i: usize| p[1 + i * 5];
        assert_eq!([slot(0), slot(1), slot(2), slot(3), slot(4)], [1, 2, 3, 4, 5]);
        assert_eq!(slot(5), 0x30);
        assert_eq!([slot(6), slot(7), slot(8)], [0, 0, 0]);
        assert_eq!([slot(9), slot(10)], [0x31, 0x32]);
    }

    #[test]
    fn button9_super_like_rivalcfg() {
        let mut m = defaults();
        m[8] = "LeftSuper".parse().unwrap();
        let p = encode_buttons(&m).unwrap();
        assert_eq!(&p[1 + 8 * 5..1 + 9 * 5], &[0x51, 0xE3, 0, 0, 0]);
        assert_eq!(m[8].to_string(), "key:LeftSuper");
    }

    #[test]
    fn media_and_aliases() {
        assert_eq!("vol+".parse::<Action>().unwrap(), Action::Media("VolumeUp".into()));
        assert_eq!("key:win".parse::<Action>().unwrap(), Action::Key("LeftSuper".into()));
        assert_eq!(Action::Media("PlayPause".into()).encode().unwrap()[..2], [0x61, 0xCD]);
    }

    #[test]
    fn dpi_packet() {
        let p = encode_dpi(&[400, 800, 1200, 2400, 3200], 1).unwrap();
        assert_eq!(p, vec![0x2D, 5, 1, 0x04, 0x09, 0x0D, 0x1B, dpi_code(3200)]);
        assert_eq!(dpi_code(18000), 0xD6);
        assert_eq!(snap_dpi(1249), 1200);
        assert!(encode_dpi(&[], 0).is_err());
    }

    #[test]
    fn timers() {
        // 5 minutes = 300000 ms = 0x0493E0
        assert_eq!(encode_sleep(5), vec![0x29, 0xE0, 0x93, 0x04]);
        // 30 s = 30000 ms = 0x007530
        assert_eq!(encode_dim(30), vec![0x23, 0x0F, 0x01, 0x00, 0x00, 0x30, 0x75, 0x00]);
    }

    #[test]
    fn colors() {
        assert_eq!(Rgb::parse("lime"), Some(Rgb(0, 255, 0)));
        assert_eq!(Rgb::parse("#f80"), Some(Rgb(255, 136, 0)));
        assert_eq!(encode_zone_color(2, Rgb(1, 2, 3)), vec![0x21, 1, 2, 1, 2, 3]);
        assert_eq!(encode_default_lighting(true, false), vec![0x27, 1, 0]);
    }

    #[test]
    fn battery() {
        assert_eq!(decode_battery(&[0x92, 0x15]), Some((100, false)));
        assert_eq!(decode_battery(&[0xD2, 0x80 | 0x0B]), Some((50, true)));
        assert_eq!(decode_battery(&[0xD2, 0x00]), None);
    }
}
