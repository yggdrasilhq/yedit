//! yedit's LAUNCHER MANIFEST — how the yggterm menus learn yedit exists.
//!
//! Written to `~/.yggterm/apps/yedit.json` on the app's OWN host on every run
//! (repairs the binary path after an upgrade). The host's yggterm daemon scans
//! the directory and deletes manifests whose binary is gone — that is the whole
//! uninstall story. Hand-rolled JSON: an app declares itself with a FILE, not
//! by linking the platform.

use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn manifest_value(binary: &Path) -> Value {
    json!({
        "name": "yedit",
        "label": "Yedit",
        "icon": "",
        "binary": binary.to_string_lossy(),
        "verbs": [
            // No file ⇒ yedit opens on its empty state (open from the pane).
            { "id": "new", "label": "New Yedit", "args": [] },
        ],
    })
}

fn write_to(apps_dir: &Path, binary: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(apps_dir)?;
    let path = apps_dir.join("yedit.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&manifest_value(binary))?,
    )?;
    Ok(path)
}

/// Best-effort on every run; a failure must never stop the editor.
pub fn write_best_effort() {
    let Some(home) = dirs::home_dir() else { return };
    let Ok(binary) = std::env::current_exe() else {
        return;
    };
    let _ = write_to(&home.join(".yggterm").join("apps"), &binary);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_names_match_the_file_stem_and_binary_is_absolute() {
        let value = manifest_value(Path::new("/usr/local/bin/yedit"));
        assert_eq!(value["name"], "yedit");
        assert!(value["binary"].as_str().unwrap().starts_with('/'));
        assert!(value["verbs"].as_array().unwrap().len() == 1);
    }
}
