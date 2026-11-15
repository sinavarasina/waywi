//! Shared XDG Base Directory, GTK settings, and `index.theme` parsing helpers.

use std::env;
use std::fs;
use std::path::PathBuf;

/// Returns `$XDG_DATA_HOME/<subdir>` (falling back to `$HOME/.local/share/<subdir>`) followed by
/// each `$XDG_DATA_DIRS/<subdir>` entry ordered from highest to lowest priority.
pub(crate) fn xdg_data_dirs_for(subdir: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(xdg_home) = env::var("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(xdg_home).join(subdir));
    } else if let Ok(home) = env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share").join(subdir));
    }
    let data_dirs =
        env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    for dir in data_dirs.split(':').filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(dir).join(subdir));
    }
    dirs
}

/// Returns `$XDG_CONFIG_HOME` or `$HOME/.config` if configured.
pub(crate) fn xdg_config_home() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

/// Extracts the first matching `key = value` entry from INI-formatted text.
pub(crate) fn ini_key(text: &str, key_name: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some((k, v)) = trimmed.split_once('=')
            && k.trim() == key_name
        {
            let clean = v.trim().trim_matches('"');
            if !clean.is_empty() {
                return Some(clean.to_owned());
            }
        }
    }
    None
}

/// Looks up a configuration key across GTK 4 and GTK 3 `settings.ini` files.
pub(crate) fn read_gtk_setting(key_name: &str) -> Option<String> {
    let cfg = xdg_config_home()?;
    for file in ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"] {
        if let Ok(text) = fs::read_to_string(cfg.join(file))
            && let Some(val) = ini_key(&text, key_name)
        {
            return Some(val);
        }
    }
    None
}

/// Parses a comma-separated INI list value into trimmed, non-empty strings.
pub(crate) fn parse_csv_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Produces dashed fallback names ordered from most specific (`a-b-c`) to base (`a`).
pub(crate) fn dash_fallback_names(name: &str) -> Vec<String> {
    let mut out = vec![name.to_owned()];
    let mut cur = name;
    while let Some((prefix, _)) = cur.rsplit_once('-') {
        if prefix.is_empty() {
            break;
        }
        out.push(prefix.to_owned());
        cur = prefix;
    }
    out
}

/// Traverses XDG theme `Inherits=` chains breadth-first from `seeds` without cycles.
pub(crate) fn walk_theme_chain<'a>(
    seeds: impl IntoIterator<Item = &'a str>,
    mut load_inherits: impl FnMut(&str) -> Vec<String>,
) -> Vec<String> {
    let mut queue: std::collections::VecDeque<String> =
        seeds.into_iter().map(str::to_owned).collect();
    let mut visited = std::collections::HashSet::new();
    let mut chain = Vec::new();
    while let Some(name) = queue.pop_front() {
        if name.is_empty() || !visited.insert(name.clone()) {
            continue;
        }
        for parent in load_inherits(&name) {
            if !visited.contains(&parent) {
                queue.push_back(parent);
            }
        }
        chain.push(name);
    }
    chain
}
