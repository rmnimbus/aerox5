//! aerox5: configure the SteelSeries Aerox 5 Wireless (buttons, lighting, CPI,
//! polling rate, power) from a GUI or the command line.

mod device;
mod gui;
mod profile;
mod protocol;
mod tables;
mod update;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use device::{find_devices, Mouse};
use profile::{Profile, Store};
use std::path::PathBuf;

pub const UDEV_RULE: &str = include_str!("../packaging/70-aerox5.rules");
pub const UDEV_RULE_PATH: &str = "/etc/udev/rules.d/70-aerox5.rules";
const DESKTOP_ENTRY: &str = include_str!("../packaging/aerox5.desktop");
const ICON_SVG: &str = include_str!("../packaging/aerox5.svg");

/// How the user should invoke this program: the AppImage path when run from one.
pub fn self_command() -> String {
    std::env::var("APPIMAGE").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| "aerox5".into())
}

#[derive(Parser)]
#[command(version, about = "SteelSeries Aerox 5 Wireless configuration (run without arguments for the GUI)")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the graphical editor (the default).
    Gui,
    /// List detected Aerox 5 interfaces.
    List,
    /// Show connection, battery and firmware.
    Info,
    /// Apply a profile to the mouse (default: the last applied one) and save it in the mouse.
    Apply {
        /// Profile name (see `aerox5 profiles`) or a path to a .toml file.
        profile: Option<String>,
        /// Apply for this session only; don't write the mouse's flash.
        #[arg(long)]
        no_save: bool,
    },
    /// List saved profiles (* = last applied).
    Profiles,
    /// Print a profile as TOML.
    Show { profile: Option<String> },
    /// Import rivalcfg's settings file (e.g. ~/.config/rivalcfg/1038_1854.device.json) as a profile.
    ImportRivalcfg {
        file: PathBuf,
        #[arg(long, default_value = "rivalcfg")]
        name: String,
    },
    /// Restore factory settings on the mouse (buttons, lighting, CPI, timers) and save.
    Reset,
    /// Print every value a button can be mapped to.
    Actions,
    /// Check GitHub for a newer release and, when running as an AppImage, install it.
    Update {
        /// Only report whether an update is available.
        #[arg(long)]
        check: bool,
    },
    /// Print the udev rule that lets your user access the mouse without root.
    /// Install it with: aerox5 udev-rule | sudo tee /etc/udev/rules.d/70-aerox5.rules
    UdevRule,
    /// Add (or with --remove, delete) a launcher entry + icon in ~/.local/share for this binary/AppImage.
    DesktopEntry {
        #[arg(long)]
        remove: bool,
    },
}

fn resolve(store: &Store, name: Option<&str>) -> Result<(String, Profile)> {
    let name = match name {
        Some(n) => n.to_string(),
        None => store.ensure_default()?,
    };
    let as_path = PathBuf::from(&name);
    if name.ends_with(".toml") && as_path.exists() {
        return Ok((name.clone(), Profile::load(&as_path)?));
    }
    let p = store.load(&name).with_context(|| format!("profile '{name}' not found"))?;
    Ok((name, p))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Gui) {
        Cmd::Gui => gui::run(),
        Cmd::List => {
            let devs = find_devices();
            if devs.is_empty() {
                println!("no Aerox 5 Wireless found");
            }
            for d in devs {
                println!("{}  {:04x}:{:04x}  {}  {}", d.node.display(), protocol::VENDOR_ID, d.product_id, d.mode.label(), d.name());
            }
            Ok(())
        }
        Cmd::Info => {
            let mut m = Mouse::open_first()?;
            println!("{} via {} ({})", m.info.name(), m.info.mode.label(), m.info.node.display());
            match m.battery()? {
                Some((lvl, chg)) => println!("battery: {lvl}%{}", if chg { " (charging)" } else { "" }),
                None => println!("battery: unknown (mouse off or asleep?)"),
            }
            println!("firmware: {}", m.firmware()?.unwrap_or_else(|| "unknown".into()));
            Ok(())
        }
        Cmd::Apply { profile, no_save } => {
            let store = Store::open()?;
            let (name, p) = resolve(&store, profile.as_deref())?;
            let mut m = Mouse::open_first()?;
            m.apply(&p, !no_save)?;
            if store.path(&name).exists() {
                store.set_active(&name)?;
            }
            println!("applied '{name}' via {}{}", m.info.mode.label(), if no_save { "" } else { " and saved to the mouse" });
            Ok(())
        }
        Cmd::Profiles => {
            let store = Store::open()?;
            store.ensure_default()?;
            let active = store.active();
            for n in store.list() {
                println!("{} {n}", if Some(&n) == active.as_ref() { "*" } else { " " });
            }
            println!("\n(stored in {})", store.dir.join("profiles").display());
            Ok(())
        }
        Cmd::Show { profile } => {
            let store = Store::open()?;
            let (_, p) = resolve(&store, profile.as_deref())?;
            print!("{}", toml::to_string_pretty(&p)?);
            Ok(())
        }
        Cmd::ImportRivalcfg { file, name } => {
            let store = Store::open()?;
            if store.path(&name).exists() {
                bail!("profile '{name}' already exists; pick another --name");
            }
            let p = Profile::from_rivalcfg(&file)?;
            store.save(&name, &p)?;
            println!("imported as profile '{name}' ({})", store.path(&name).display());
            Ok(())
        }
        Cmd::Reset => {
            let mut m = Mouse::open_first()?;
            m.apply(&Profile::default(), true)?;
            println!("factory settings applied and saved (your profiles are unchanged)");
            Ok(())
        }
        Cmd::Update { check } => {
            let rel = update::latest_release()?;
            if !update::is_newer(&rel.version, update::VERSION) {
                println!("aerox5 {} is the latest version", update::VERSION);
                return Ok(());
            }
            println!("aerox5 {} is available (you have {})\n{}", rel.version, update::VERSION, rel.page);
            if check {
                return Ok(());
            }
            match update::can_self_update() {
                Err(why) => bail!("{why} Download it from {}", rel.page),
                Ok(_) => {
                    let path = update::update(&rel, |_, _| {})?;
                    println!("updated {} to {}", path.display(), rel.version);
                }
            }
            Ok(())
        }
        Cmd::UdevRule => {
            print!("{UDEV_RULE}");
            Ok(())
        }
        Cmd::DesktopEntry { remove } => desktop_entry(remove),
        Cmd::Actions => {
            println!("special: disabled, dpi, scrollup, scrolldown");
            println!("mouse:   button1 (left), button2 (right), button3 (middle), button4 (back), button5 (forward)");
            println!("media:   {}", tables::MEDIA_KEYS.iter().map(|(k, _)| format!("media:{k}")).collect::<Vec<_>>().join(", "));
            println!("keys:    {}", tables::KEYS.iter().map(|(k, _)| format!("key:{k}")).collect::<Vec<_>>().join(", "));
            Ok(())
        }
    }
}

fn desktop_entry(remove: bool) -> Result<()> {
    let data = dirs::data_dir().context("no data directory")?;
    let entry = data.join("applications/aerox5.desktop");
    let icon = data.join("icons/hicolor/scalable/apps/aerox5.svg");
    if remove {
        for f in [&entry, &icon] {
            if f.exists() {
                std::fs::remove_file(f)?;
                println!("removed {}", f.display());
            }
        }
        return Ok(());
    }
    let exe = match std::env::var("APPIMAGE").ok().filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe()?,
    };
    let exe = exe.canonicalize().unwrap_or(exe);
    let exec = exe.display().to_string();
    if exec.contains(char::is_whitespace) || exec.contains('"') {
        bail!("the path {exec} contains spaces or quotes; move the file to a simpler path first");
    }
    let text: String = DESKTOP_ENTRY
        .lines()
        .map(|l| if l.starts_with("Exec=") { format!("Exec={exec}") } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    for f in [&entry, &icon] {
        std::fs::create_dir_all(f.parent().unwrap())?;
    }
    std::fs::write(&icon, ICON_SVG)?;
    std::fs::write(&entry, text)?;
    println!("wrote {} (Exec={exec})", entry.display());
    println!("wrote {}", icon.display());
    Ok(())
}
