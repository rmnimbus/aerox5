//! Profiles: named sets of settings, stored as TOML in `~/.config/aerox5/profiles/`.
//!
//! The mouse can't report its current settings, so (like rivalcfg) the app keeps the
//! last-applied profile on disk and treats it as the mouse's state.

use crate::protocol::{self, Action, Button, Rgb};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sensitivity {
    /// 1–5 CPI presets, cycled by the CPI button.
    pub presets: Vec<u32>,
    /// Index of the preset active after applying.
    pub active: usize,
    pub polling_rate: u16,
}

impl Default for Sensitivity {
    fn default() -> Self {
        Sensitivity { presets: vec![400, 800, 1200, 2400, 3200], active: 0, polling_rate: 1000 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lighting {
    /// Built-in rainbow cycle instead of static zone colours.
    pub rainbow: bool,
    /// Static colours: top, middle, bottom LED of the strip.
    pub zones: [Rgb; 3],
    /// Flash `reactive_color` on every click.
    pub reactive: bool,
    pub reactive_color: Rgb,
}

impl Default for Lighting {
    fn default() -> Self {
        Lighting {
            rainbow: true,
            zones: [Rgb(255, 0, 0), Rgb(0, 255, 0), Rgb(0, 0, 255)],
            reactive: false,
            reactive_color: Rgb(255, 255, 255),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Power {
    /// Idle minutes before sleep, 0–20 (0 = never).
    pub sleep_minutes: u32,
    /// Idle seconds before the LEDs dim, 0–1200 (0 = never).
    pub dim_seconds: u32,
}

impl Default for Power {
    fn default() -> Self {
        Power { sleep_minutes: 5, dim_seconds: 30 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Only the buttons that differ from the default need to be listed.
    pub buttons: BTreeMap<String, Action>,
    pub sensitivity: Sensitivity,
    pub lighting: Lighting,
    pub power: Power,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            buttons: Button::ALL.iter().map(|b| (b.key().to_string(), b.default_action())).collect(),
            sensitivity: Sensitivity::default(),
            lighting: Lighting::default(),
            power: Power::default(),
        }
    }
}

impl Profile {
    pub fn action(&self, b: Button) -> Action {
        self.buttons.get(b.key()).cloned().unwrap_or_else(|| b.default_action())
    }

    pub fn set_action(&mut self, b: Button, a: Action) {
        self.buttons.insert(b.key().to_string(), a);
    }

    pub fn button_array(&self) -> [Action; 11] {
        Button::ALL.map(|b| self.action(b))
    }

    /// Problems that would stop the profile from being applied (or lock the user out).
    pub fn validate(&self) -> Vec<String> {
        let mut e = Vec::new();
        for k in self.buttons.keys() {
            if Button::from_key(k).is_none() {
                e.push(format!("unknown button '{k}'"));
            }
        }
        for (b, a) in Button::ALL.iter().zip(self.button_array()) {
            if let Err(x) = a.encode() {
                e.push(format!("{}: {x}", b.key()));
            }
        }
        if !self.button_array().contains(&Action::Mouse(1)) {
            e.push("no button is mapped to left click (button1)".into());
        }
        let s = &self.sensitivity;
        if s.presets.is_empty() || s.presets.len() > protocol::MAX_DPI_PRESETS {
            e.push(format!("need 1-{} CPI presets", protocol::MAX_DPI_PRESETS));
        } else if s.active >= s.presets.len() {
            e.push("active CPI preset is out of range".into());
        }
        if !protocol::POLLING_RATES.contains(&s.polling_rate) {
            e.push(format!("polling rate {} Hz unsupported", s.polling_rate));
        }
        if self.power.sleep_minutes > protocol::SLEEP_MAX_MIN {
            e.push(format!("sleep timer max is {} min", protocol::SLEEP_MAX_MIN));
        }
        if self.power.dim_seconds > protocol::DIM_MAX_S {
            e.push(format!("dim timer max is {} s", protocol::DIM_MAX_S));
        }
        e
    }

    pub fn load(path: &Path) -> Result<Profile> {
        let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = format!(
            "# SteelSeries Aerox 5 Wireless profile (aerox5)\n\
             # button actions: button1-5, disabled, dpi, scrollup, scrolldown, key:<Name>, media:<Name>\n\n{}",
            toml::to_string_pretty(self)?
        );
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Import rivalcfg's saved settings (`~/.config/rivalcfg/1038_1854.device.json`).
    pub fn from_rivalcfg(path: &Path) -> Result<Profile> {
        let v: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
        )?;
        let s = v.get("default").unwrap_or(&v);
        let mut p = Profile::default();
        let get_str = |k: &str| s.get(k).map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string()));

        if let Some(sens) = get_str("sensitivity") {
            let presets: Vec<u32> = sens
                .split(',')
                .filter_map(|x| x.trim().parse::<u32>().ok())
                .map(protocol::snap_dpi)
                .collect();
            if !presets.is_empty() {
                p.sensitivity.presets = presets;
            }
        }
        if let Some(r) = s.get("polling_rate").and_then(|x| x.as_u64()) {
            p.sensitivity.polling_rate = r as u16;
        }
        for (i, k) in ["z1_color", "z2_color", "z3_color"].iter().enumerate() {
            if let Some(c) = get_str(k).and_then(|c| Rgb::parse(&c)) {
                p.lighting.zones[i] = c;
            }
        }
        if let Some(r) = get_str("reactive_color") {
            match Rgb::parse(&r) {
                Some(c) => {
                    p.lighting.reactive = true;
                    p.lighting.reactive_color = c;
                }
                None => p.lighting.reactive = false,
            }
        }
        if let Some(d) = get_str("default_lighting") {
            p.lighting.rainbow = d.contains("rainbow");
            p.lighting.reactive = p.lighting.reactive || d.contains("reactive");
        }
        if let Some(t) = s.get("sleep_timer").and_then(|x| x.as_u64()) {
            p.power.sleep_minutes = t as u32;
        }
        if let Some(t) = s.get("dim_timer").and_then(|x| x.as_u64()) {
            p.power.dim_seconds = t as u32;
        }
        if let Some(b) = get_str("buttons_mapping") {
            let inner = b.trim();
            if !inner.eq_ignore_ascii_case("default") {
                let inner = inner
                    .strip_prefix("buttons(")
                    .and_then(|x| x.strip_suffix(')'))
                    .with_context(|| format!("unrecognised buttons_mapping '{b}'"))?;
                for part in inner.split(';').map(str::trim).filter(|x| !x.is_empty()) {
                    let Some((k, val)) = part.split_once('=') else { continue };
                    let (k, val) = (k.trim(), val.trim());
                    if k.eq_ignore_ascii_case("layout") {
                        if !val.eq_ignore_ascii_case("qwerty") {
                            bail!("only the qwerty layout is supported");
                        }
                        continue;
                    }
                    let btn = Button::from_key(k).with_context(|| format!("unknown button '{k}'"))?;
                    if val.eq_ignore_ascii_case("default") {
                        continue;
                    }
                    let a: Action = val.parse().map_err(anyhow::Error::msg)?;
                    p.set_action(btn, a);
                }
            }
        }
        Ok(p)
    }
}

// ---------------------------------------------------------------------------------------
// Profile store
// ---------------------------------------------------------------------------------------

#[derive(Default, Serialize, Deserialize)]
struct State {
    /// Profile most recently applied to the mouse.
    active: Option<String>,
}

pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn open() -> Result<Store> {
        let dir = dirs::config_dir().context("no config directory ($HOME unset?)")?.join("aerox5");
        fs::create_dir_all(dir.join("profiles"))?;
        Ok(Store { dir })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join("profiles").join(format!("{name}.toml"))
    }

    pub fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 64
            && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.'))
            && !name.starts_with('.')
    }

    pub fn list(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(self.dir.join("profiles"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                (p.extension()? == "toml").then(|| p.file_stem()?.to_str().map(String::from))?
            })
            .collect();
        v.sort();
        v
    }

    pub fn load(&self, name: &str) -> Result<Profile> {
        Profile::load(&self.path(name))
    }

    pub fn save(&self, name: &str, p: &Profile) -> Result<()> {
        if !Store::valid_name(name) {
            bail!("invalid profile name '{name}' (letters, digits, space, - _ .)");
        }
        p.save(&self.path(name))
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        fs::remove_file(self.path(name)).with_context(|| format!("deleting profile '{name}'"))
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.toml")
    }

    pub fn active(&self) -> Option<String> {
        let s: State = toml::from_str(&fs::read_to_string(self.state_path()).ok()?).ok()?;
        s.active.filter(|n| self.path(n).exists())
    }

    pub fn set_active(&self, name: &str) -> Result<()> {
        let s = State { active: Some(name.to_string()) };
        fs::write(self.state_path(), toml::to_string(&s)?)?;
        Ok(())
    }

    /// Make sure at least one profile exists. On first run, import rivalcfg's saved
    /// settings if present (so an existing setup carries over), else factory defaults.
    pub fn ensure_default(&self) -> Result<String> {
        if let Some(a) = self.active() {
            return Ok(a);
        }
        if let Some(first) = self.list().into_iter().next() {
            return Ok(first);
        }
        let mut p = Profile::default();
        if let Some(cfg) = dirs::config_dir() {
            for pid in ["1038_1854", "1038_1852"] {
                let f = cfg.join("rivalcfg").join(format!("{pid}.device.json"));
                if let Ok(imported) = Profile::from_rivalcfg(&f) {
                    p = imported;
                    break;
                }
            }
        }
        self.save("default", &p)?;
        self.set_active("default")?;
        Ok("default".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rivalcfg_import() {
        let dir = std::env::temp_dir().join(format!("aerox5-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("r.json");
        fs::write(
            &f,
            r#"{"default":{"sensitivity":"400, 800, 1200, 2400, 3200","polling_rate":1000,
            "z1_color":"red","z2_color":"lime","z3_color":"blue","reactive_color":"off",
            "sleep_timer":5,"dim_timer":30,
            "buttons_mapping":"buttons(button9=LeftSuper; layout=qwerty)",
            "default_lighting":"rainbow"}}"#,
        )
        .unwrap();
        let p = Profile::from_rivalcfg(&f).unwrap();
        assert_eq!(p.action(Button::Button9), Action::Key("LeftSuper".into()));
        assert_eq!(p.action(Button::Button6), Action::DpiCycle);
        assert!(p.lighting.rainbow && !p.lighting.reactive);
        assert!(p.validate().is_empty());
        // round trip
        let t = dir.join("p.toml");
        p.save(&t).unwrap();
        assert_eq!(Profile::load(&t).unwrap(), p);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn lockout_is_rejected() {
        let mut p = Profile::default();
        p.set_action(Button::Button1, Action::Disabled);
        assert!(!p.validate().is_empty());
    }
}
