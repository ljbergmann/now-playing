//! The settings file the setup wizard writes: one `NAME=value` per line,
//! readable only by the owner (it can hold an API key).
//!
//! Where it lives:
//!   `NP_SETTINGS` if set, else
//!   macOS: ~/Library/Application Support/Now Playing/settings
//!   Windows: %APPDATA%\Now Playing\settings
//!   Linux: $STATE_DIRECTORY/settings (systemd's StateDirectory=, i.e.
//!          /var/lib/now-playing on the Pi), else ~/.config/now-playing/settings
//!
//! Values from the real environment win over the file, so a one-off
//! `THEME=poster now-playing` still works (the setup page then can't change them).

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NP_SETTINGS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Now Playing").join("settings"));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/Now Playing/settings"));
    }
    if let Some(dir) = std::env::var_os("STATE_DIRECTORY").filter(|d| !d.is_empty()) {
        // systemd may pass several, colon-separated; the first is ours.
        let first = dir.to_string_lossy().split(':').next().unwrap_or_default().to_string();
        return Some(PathBuf::from(first).join("settings"));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".config")));
    config.map(|c| c.join("now-playing/settings"))
}

fn parse_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (k, v) = line.split_once('=')?;
    let k = k.trim();
    (!k.is_empty()).then(|| (k, v.trim()))
}

/// The file's values, read once and kept in step by `save`.
static FILE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn file_values() -> HashMap<String, String> {
    let text = path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    text.lines().filter_map(parse_line).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Current value: the environment first, then the file.
pub fn get(key: &str) -> Option<String> {
    if let Some(v) = std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        return Some(v);
    }
    let mut file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    file.get_or_insert_with(file_values).get(key).filter(|v| !v.is_empty()).cloned()
}

/// Sets (`Some`) or removes (`None`) values, keeping every other line and
/// comment as it was. Written to a temporary file first, then moved into
/// place, so a power cut can't leave half a file.
pub fn save(changes: &[(&str, Option<&str>)]) -> Result<(), String> {
    let path = path().ok_or("no place for the settings file (HOME isn't set)")?;
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let text = apply(&old, changes)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        // Owner only. (On Windows the per-user AppData folder already is.)
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts
            .open(&tmp)
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        f.write_all(text.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    let values = file.get_or_insert_with(HashMap::new);
    for (k, v) in changes {
        match v {
            Some(v) => values.insert(k.to_string(), v.to_string()),
            None => values.remove(*k),
        };
    }
    Ok(())
}

fn apply(old: &str, changes: &[(&str, Option<&str>)]) -> Result<String, String> {
    for (k, v) in changes {
        let bad = |s: &str| s.contains(['\n', '\r', '\0']);
        if k.is_empty() || k.contains('=') || bad(k) || v.is_some_and(bad) {
            return Err(format!("invalid setting {k}"));
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut done: Vec<&str> = Vec::new();
    for line in old.lines() {
        match parse_line(line) {
            Some((k, _)) => match changes.iter().find(|(c, _)| *c == k) {
                Some((c, v)) => {
                    if !done.contains(c) {
                        done.push(c);
                        if let Some(v) = v {
                            out.push(format!("{c}={v}"));
                        }
                    }
                }
                None => out.push(line.to_string()),
            },
            None => out.push(line.to_string()),
        }
    }
    if old.is_empty() {
        out.push("# Now Playing settings, written by the setup page. One NAME=value per line.".into());
    }
    for (k, v) in changes {
        if let (false, Some(v)) = (done.contains(k), v) {
            out.push(format!("{k}={v}"));
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::apply;

    #[test]
    fn keeps_comments_replaces_and_removes() {
        let old = "# hello\nLASTFM_USER=a\nTHEME=vinyl\n\nVINYLVAULT_SHELF=x\n";
        let new = apply(old, &[("LASTFM_USER", Some("b")), ("VINYLVAULT_SHELF", None), ("CLOCK", Some("12h"))]).unwrap();
        assert_eq!(new, "# hello\nLASTFM_USER=b\nTHEME=vinyl\n\nCLOCK=12h\n");
    }

    #[test]
    fn rejects_line_breaks() {
        assert!(apply("", &[("THEME", Some("vinyl\nLASTFM_USER=evil"))]).is_err());
        assert!(apply("", &[("A=B", Some("x"))]).is_err());
    }

    #[test]
    fn new_file_gets_a_header() {
        let new = apply("", &[("THEME", Some("poster"))]).unwrap();
        assert!(new.starts_with("# Now Playing settings"));
        assert!(new.ends_with("THEME=poster\n"));
    }
}
