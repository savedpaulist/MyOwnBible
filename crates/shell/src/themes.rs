//! Filesystem adapter for user-installed themes.
//!
//! GPUI-specific parsing and registration stay in `bible_ui`; directory
//! discovery and file I/O stay out of the presentation layer.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct ThemeSource {
    pub path: PathBuf,
    pub contents: String,
}

pub fn storage_dir() -> PathBuf {
    app_state::paths::data_dir().join("themes")
}

pub fn external_sources() -> Vec<ThemeSource> {
    let local = storage_dir();
    if let Err(error) = std::fs::create_dir_all(&local) {
        log::warn!("cannot create theme folder {}: {error}", local.display());
    }

    let mut directories = vec![local];
    directories.extend(zed_theme_dirs());

    let mut files = BTreeSet::new();
    for directory in directories {
        collect_theme_files(&directory, 0, &mut files);
    }

    files
        .into_iter()
        .filter_map(|path| match std::fs::read_to_string(&path) {
            Ok(contents) => Some(ThemeSource { path, contents }),
            Err(error) => {
                log::warn!("cannot read Zed theme {}: {error}", path.display());
                None
            }
        })
        .collect()
}

fn zed_theme_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(target_os = "windows") {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            dirs.push(appdata.join("Zed").join("themes"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            dirs.push(local.join("Zed").join("extensions").join("installed"));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let zed = home.join("Library").join("Application Support").join("Zed");
            dirs.push(zed.join("themes"));
            dirs.push(zed.join("extensions").join("installed"));
        }
    } else {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        if let Some(config) = config {
            dirs.push(config.join("zed").join("themes"));
        }
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            });
        if let Some(data) = data {
            dirs.push(data.join("zed").join("extensions").join("installed"));
        }
    }
    dirs
}

fn collect_theme_files(dir: &Path, depth: usize, output: &mut BTreeSet<PathBuf>) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_theme_files(&path, depth + 1, output);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            output.insert(path);
        }
    }
}
