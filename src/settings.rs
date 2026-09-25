//! Keys and settings that come from the operator or the dashboard. Each is looked up as `NAME`,
//! then the file named by `NAME_FILE` (how systemd and compose credentials arrive), then
//! `<state dir>/settings/NAME`, which the dashboard writes (mode 0600). Blank counts as unset.
//! Values are never logged and never returned by the API; only whether one is set.

use std::io::Write;
use std::path::{Path, PathBuf};

/// A setting the dashboard can change. `secret` ones are write-only over the API.
pub struct Editable {
    pub name: &'static str,
    pub secret: bool,
}

pub const EDITABLE: &[Editable] = &[
    Editable { name: "KIS_APP_KEY", secret: true },
    Editable { name: "KIS_APP_SECRET", secret: true },
    Editable { name: "KIS_ENV", secret: false },
    Editable { name: "DART_API_KEY", secret: true },
    Editable { name: "EDGAR_USER_AGENT", secret: false },
];

/// Where a value came from. The dashboard cannot override `Env` or `File`: the operator set those.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Env,
    File,
    Dashboard,
}

/// `$ATRADER_STATE_DIR`, else `$XDG_STATE_HOME/atrader`, else `~/.local/state/atrader`.
pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ATRADER_STATE_DIR") {
        return d.into();
    }
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        return Path::new(&d).join("atrader");
    }
    Path::new(&std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".local/state/atrader")
}

/// The value of `name` from any source, looking in the default state directory.
pub fn secret_env(name: &str) -> Option<String> {
    lookup(&state_dir(), name).map(|(v, _)| v)
}

pub fn source_in(dir: &Path, name: &str) -> Option<Source> {
    lookup(dir, name).map(|(_, s)| s)
}

pub fn get_in(dir: &Path, name: &str) -> Option<String> {
    lookup(dir, name).map(|(v, _)| v)
}

fn lookup(dir: &Path, name: &str) -> Option<(String, Source)> {
    let clean = |v: String| Some(v.trim().to_string()).filter(|v| !v.is_empty());
    if let Some(v) = std::env::var(name).ok().and_then(clean) {
        return Some((v, Source::Env));
    }
    if let Ok(path) = std::env::var(format!("{name}_FILE")) {
        match std::fs::read_to_string(&path) {
            Ok(v) => {
                if let Some(v) = clean(v) {
                    return Some((v, Source::File));
                }
            }
            Err(e) => tracing::error!(error = %e, path, "cannot read {name}_FILE"),
        }
    }
    let v = std::fs::read_to_string(settings_path(dir, name)).ok().and_then(clean)?;
    Some((v, Source::Dashboard))
}

fn settings_path(dir: &Path, name: &str) -> PathBuf {
    dir.join("settings").join(name)
}

/// Store `value` for `name` in `dir` (mode 0600, replaced atomically); blank removes it.
pub fn save_in(dir: &Path, name: &str, value: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let path = settings_path(dir, name);
    let parent = path.parent().expect("settings path has a parent");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    if value.trim().is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(value.trim().as_bytes())?;
    f.sync_all()?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_come_from_env_then_file_then_the_dashboard() {
        let dir = std::env::temp_dir().join(format!("atrader-settings-{}", std::process::id()));
        let file = dir.join("from-file");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "from-file\n").unwrap();

        save_in(&dir, "ATRADER_TEST_KEY", " saved \n").unwrap();
        assert_eq!(get_in(&dir, "ATRADER_TEST_KEY").as_deref(), Some("saved"));
        assert_eq!(source_in(&dir, "ATRADER_TEST_KEY"), Some(Source::Dashboard));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("settings/ATRADER_TEST_KEY")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        // SAFETY: this test is the only user of these variable names.
        unsafe { std::env::set_var("ATRADER_TEST_KEY_FILE", &file) };
        assert_eq!((get_in(&dir, "ATRADER_TEST_KEY").as_deref(), source_in(&dir, "ATRADER_TEST_KEY")), (Some("from-file"), Some(Source::File)));
        unsafe { std::env::set_var("ATRADER_TEST_KEY", "direct") };
        assert_eq!(source_in(&dir, "ATRADER_TEST_KEY"), Some(Source::Env));
        unsafe {
            std::env::remove_var("ATRADER_TEST_KEY");
            std::env::remove_var("ATRADER_TEST_KEY_FILE");
        }

        save_in(&dir, "ATRADER_TEST_KEY", "").unwrap();
        assert_eq!(get_in(&dir, "ATRADER_TEST_KEY"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
