//! The chartplotters' passwords (each one's WiFi password, which may differ per unit), kept on disk by serial
//! number so a chartplotter needs its password only once.
//!
//! A JSON file, readable by its owner only: `{ "<serial>": { "password": "..." } }`. Written to a temporary
//! file first and renamed, so it is never half written.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default, Deserialize, Serialize)]
struct Entry {
    password: String,
}

#[derive(Clone)]
pub struct Credentials {
    path: PathBuf,
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
}

impl Credentials {
    /// Loads `path`; a missing file is an empty store.
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let entries = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("{}: {e}", path.display()),
                ))
            }
        };
        Ok(Credentials {
            path,
            entries: Arc::new(Mutex::new(entries)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn password(&self, serial: &str) -> Option<String> {
        self.lock().get(serial).map(|e| e.password.clone())
    }

    pub fn set_password(&self, serial: &str, password: &str) -> std::io::Result<()> {
        let mut g = self.lock();
        g.insert(
            serial.to_string(),
            Entry {
                password: password.to_string(),
            },
        );
        save(&self.path, &g)
    }

    pub fn forget(&self, serial: &str) -> std::io::Result<()> {
        let mut g = self.lock();
        if g.remove(serial).is_some() {
            save(&self.path, &g)?;
        }
        Ok(())
    }
}

fn save(path: &Path, entries: &BTreeMap<String, Entry>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let data = serde_json::to_vec_pretty(entries).map_err(std::io::Error::other)?;
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let mut f = o.open(&tmp)?;
        std::io::Write::write_all(&mut f, &data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Where the program keeps its state: `$STATE_DIRECTORY` (systemd's StateDirectory=), else the platform's
/// per-user config directory.
pub fn default_dir(app: &str) -> PathBuf {
    if let Some(d) = std::env::var_os("STATE_DIRECTORY").filter(|d| !d.is_empty()) {
        // systemd may give several, separated by ':'
        let d = d
            .to_string_lossy()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string();
        return PathBuf::from(d);
    }
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home)
            .join(app)
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support").join(app)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home().join(".config"))
            .join(app)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_passwords_across_opens() {
        let dir = std::env::temp_dir().join(format!("mfd-net-test-{}", std::process::id()));
        let path = dir.join("credentials.json");
        let c = Credentials::open(path.clone()).unwrap();
        assert_eq!(c.password("123"), None);
        c.set_password("123", "secret").unwrap();
        c.set_password("456", "other").unwrap();
        c.forget("456").unwrap();
        let c = Credentials::open(path.clone()).unwrap();
        assert_eq!(c.password("123").as_deref(), Some("secret"));
        assert_eq!(c.password("456"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
