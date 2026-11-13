//! Self-update from GitHub releases (same design as Mithlond's updater).
//!
//! Only the AppImage updates itself: the new AppImage is downloaded next to the running one,
//! checked (size, GitHub's SHA-256 digest, AppImage header), made executable and renamed over
//! the old file, so launchers pointing at that path keep working. Linux keeps the running
//! (mounted) copy alive until the app exits; "Restart" starts the new one.

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const REPO: &str = "rmnimbus/aerox5";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const API: &str = "https://api.github.com/repos/rmnimbus/aerox5/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/rmnimbus/aerox5/releases/latest";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub notes: String,
    pub page: String,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub asset_size: u64,
    pub sha256: Option<String>,
}

fn vkey(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    vkey(candidate) > vkey(current)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .user_agent(&format!("aerox5/{VERSION}"))
        .build()
}

pub fn parse_release(v: &serde_json::Value) -> Release {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let tag = s("tag_name");
    let asset = v
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|x| x["name"].as_str().is_some_and(|n| n.ends_with("-x86_64.AppImage"))));
    let digest = asset.and_then(|a| a["digest"].as_str()).unwrap_or_default();
    Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        notes: s("body").trim().to_string(),
        page: Some(s("html_url")).filter(|p| !p.is_empty()).unwrap_or_else(|| RELEASES_PAGE.into()),
        asset_name: asset.and_then(|a| a["name"].as_str()).map(String::from),
        asset_url: asset.and_then(|a| a["browser_download_url"].as_str()).map(String::from),
        asset_size: asset.and_then(|a| a["size"].as_u64()).unwrap_or(0),
        sha256: digest.strip_prefix("sha256:").map(String::from),
        tag,
    }
}

pub fn latest_release() -> Result<Release> {
    match agent().get(API).set("Accept", "application/vnd.github+json").call() {
        Ok(r) => Ok(parse_release(&r.into_json::<serde_json::Value>().context("GitHub sent an unreadable reply")?)),
        Err(ureq::Error::Status(403 | 429, _)) => {
            bail!("GitHub's hourly limit for update checks was reached; try again later.")
        }
        Err(ureq::Error::Status(404, _)) => bail!("No release has been published on GitHub yet."),
        Err(e) => Err(anyhow!("Couldn't check GitHub for updates: {e}")),
    }
}

/// The .AppImage file we're running from (the AppImage runtime sets $APPIMAGE).
pub fn running_appimage() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file())
}

/// Path that would be replaced, or why updating in place isn't possible.
pub fn can_self_update() -> std::result::Result<PathBuf, String> {
    let Some(cur) = running_appimage() else {
        return Err("aerox5 isn't running from an AppImage, so it can't replace itself.".into());
    };
    let dir = cur.parent().unwrap_or(Path::new("/"));
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: valid NUL-terminated path.
    if unsafe { libc::access(c.as_ptr(), libc::W_OK) } != 0 {
        return Err(format!("{} isn't writable, so the AppImage there can't be replaced.", dir.display()));
    }
    Ok(cur)
}

fn check_appimage(path: &Path) -> Result<()> {
    let mut head = [0u8; 11];
    fs::File::open(path)?.read_exact(&mut head)?;
    if &head[..4] != b"\x7fELF" || &head[8..11] != b"AI\x02" {
        bail!("The download isn't an AppImage; the update was cancelled.");
    }
    Ok(())
}

/// Download `rel` and replace the running AppImage with it. `progress(done, total)`.
pub fn update(rel: &Release, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    let target = can_self_update().map_err(anyhow::Error::msg)?;
    let url = rel.asset_url.as_deref().ok_or_else(|| anyhow!("Release {} has no x86_64 AppImage.", rel.tag))?;
    let dir = target.parent().unwrap_or(Path::new("."));
    let part = dir.join(format!(".{}.part", rel.asset_name.as_deref().unwrap_or("aerox5.AppImage")));
    let result = (|| -> Result<()> {
        let resp = agent().get(url).call().map_err(|e| anyhow!("Download failed: {e}"))?;
        let total = resp.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(rel.asset_size);
        let mut reader = resp.into_reader();
        let mut out = fs::File::create(&part).with_context(|| format!("writing {}", part.display()))?;
        let mut sha = Sha256::new();
        let (mut done, mut buf) = (0u64, vec![0u8; 256 * 1024]);
        loop {
            let n = reader.read(&mut buf).context("Download failed")?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            sha.update(&buf[..n]);
            done += n as u64;
            progress(done, total);
        }
        out.sync_all()?;
        drop(out);
        if rel.asset_size != 0 && done != rel.asset_size {
            bail!("The download is incomplete; the update was cancelled.");
        }
        let hex: String = sha.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if rel.sha256.as_ref().is_some_and(|want| *want != hex) {
            bail!("The download doesn't match GitHub's checksum; the update was cancelled.");
        }
        check_appimage(&part)?;
        fs::set_permissions(&part, fs::Permissions::from_mode(0o755))?;
        fs::rename(&part, &target)?;
        Ok(())
    })();
    let _ = fs::remove_file(&part);
    result.map(|()| target)
}

/// A command that runs with the host's environment, not the AppImage's (our bundled libraries
/// must not leak into a browser or into the next aerox5).
pub fn host_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    if let Some(appdir) = std::env::var_os("APPDIR") {
        let appdir = appdir.to_string_lossy().into_owned();
        let kept: Vec<String> = std::env::var("LD_LIBRARY_PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|p| !p.is_empty() && !p.starts_with(&appdir))
            .map(String::from)
            .collect();
        if kept.is_empty() {
            c.env_remove("LD_LIBRARY_PATH");
        } else {
            c.env("LD_LIBRARY_PATH", kept.join(":"));
        }
        for v in ["APPIMAGE", "APPDIR", "ARGV0", "OWD"] {
            c.env_remove(v);
        }
    }
    c
}

pub fn open_url(url: &str) {
    let _ = host_command("xdg-open").arg(url).spawn();
}

/// Start the (new) AppImage at `path` detached from us.
pub fn restart(path: &Path) -> Result<()> {
    host_command(path).spawn().with_context(|| format!("starting {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.1", "0.1.1"));
    }

    #[test]
    fn parse() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"tag_name":"v0.2.0","body":"notes","html_url":"https://x/r",
            "assets":[{"name":"aerox5-0.2.0-x86_64.AppImage.sha256","size":1},
                      {"name":"aerox5-0.2.0-x86_64.AppImage","size":42,
                       "browser_download_url":"https://x/a","digest":"sha256:abc"}]}"#,
        )
        .unwrap();
        let r = parse_release(&v);
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.asset_size, 42);
        assert_eq!(r.sha256.as_deref(), Some("abc"));
        assert_eq!(r.asset_url.as_deref(), Some("https://x/a"));
    }
}
