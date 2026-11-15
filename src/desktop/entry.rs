//! Freedesktop `.desktop` application entry parser, scanner, and process launcher.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::error::{Result, WaywiError};

/// Parsed `[Desktop Entry]` application metadata from an XDG `.desktop` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub exec: Option<String>,
    pub try_exec: Option<String>,
    pub icon: Option<String>,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    pub terminal: bool,
    pub no_display: bool,
    pub hidden: bool,
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
    pub startup_wm_class: Option<String>,
}

impl DesktopEntry {
    /// Parses `Exec=` into an argument vector compatible with `Command::new`, stripping
    /// freedesktop field codes (`%f`, `%F`, `%u`, `%U`, `%d`, `%D`, `%n`, `%N`, `%v`, `%m`)
    /// and expanding `%c` / `%k` / `%i` where present.
    #[must_use]
    pub fn exec_argv(&self) -> Option<Vec<String>> {
        let raw = self.exec.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        let tokens = split_exec_tokens(raw);
        let mut argv = Vec::with_capacity(tokens.len());
        for tok in tokens {
            match tok.as_str() {
                "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
                "%i" => {
                    if let Some(ref icon) = self.icon
                        && !icon.is_empty()
                    {
                        argv.push("--icon".to_owned());
                        argv.push(icon.clone());
                    }
                }
                "%c" => argv.push(self.name.clone()),
                "%k" => argv.push(self.path.to_string_lossy().into_owned()),
                _ => argv.push(tok.replace("%%", "%")),
            }
        }
        if argv.is_empty() { None } else { Some(argv) }
    }

    /// Returns `true` when this entry should be displayed in standard app launchers on the
    /// current desktop session (`NoDisplay=false`, `Hidden=false`, and desktop filters satisfied).
    #[must_use]
    pub fn should_show_in_launcher(&self) -> bool {
        if self.no_display || self.hidden {
            return false;
        }
        let current_desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let active: Vec<&str> = current_desktop
            .split(':')
            .filter(|s| !s.is_empty())
            .collect();
        if !self.only_show_in.is_empty()
            && !self
                .only_show_in
                .iter()
                .any(|d| active.iter().any(|cur| cur.eq_ignore_ascii_case(d)))
        {
            return false;
        }
        if self
            .not_show_in
            .iter()
            .any(|d| active.iter().any(|cur| cur.eq_ignore_ascii_case(d)))
        {
            return false;
        }
        true
    }

    /// Spawns this application with null stdio handles and returns the [`Child`] handle.
    ///
    /// The caller is responsible for waiting on the returned [`Child`] to avoid zombie processes;
    /// use [`Self::launch_detached`] when you do not need the handle.
    pub fn launch(&self) -> Result<Child> {
        let argv = self.exec_argv().ok_or_else(|| {
            WaywiError::InvalidState(format!("desktop entry `{}` has no Exec command", self.id))
        })?;
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.spawn().map_err(WaywiError::Io)
    }

    /// Spawns this application detached in its own session and reaps it on exit.
    pub fn launch_detached(&self) -> Result<()> {
        let argv = self.exec_argv().ok_or_else(|| {
            WaywiError::InvalidState(format!("desktop entry `{}` has no Exec command", self.id))
        })?;
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        super::spawn_detached(&argv[0], &args)
    }
}

/// Returns standard XDG application directories ordered from highest to lowest priority.
#[must_use]
pub fn default_application_dirs() -> Vec<PathBuf> {
    super::xdg::xdg_data_dirs_for("applications")
}

/// Scans standard XDG application directories and returns deduplicated `Type=Application` entries.
#[must_use]
pub fn scan_desktop_entries() -> Vec<DesktopEntry> {
    scan_desktop_entries_in(&default_application_dirs())
}

/// Scans explicit application directories (highest priority first) and returns sorted entries.
#[must_use]
pub fn scan_desktop_entries_in(dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut seen_ids = HashSet::new();
    let mut entries = Vec::new();
    for base in dirs {
        if !base.is_dir() {
            continue;
        }
        collect_dir_entries(base, base, &mut seen_ids, &mut entries);
    }
    let mut keyed: Vec<(String, DesktopEntry)> = entries
        .into_iter()
        .map(|entry| (entry.name.to_lowercase(), entry))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, entry)| entry).collect()
}

/// Finds a `.desktop` entry matching `id_or_wm_class` by desktop ID, basename, or `StartupWMClass`.
#[must_use]
pub fn find_desktop_entry(id_or_wm_class: &str) -> Option<DesktopEntry> {
    let needle = id_or_wm_class.trim();
    if needle.is_empty() {
        return None;
    }
    let needle_with_ext = if needle.ends_with(".desktop") {
        needle.to_owned()
    } else {
        format!("{needle}.desktop")
    };
    let dirs = default_application_dirs();
    // Fast path for direct `<id>.desktop` lookup before walking subdirectories.
    for base in &dirs {
        let direct = base.join(&needle_with_ext);
        if let Ok(text) = fs::read_to_string(&direct)
            && let Some(entry) = parse_desktop_entry(&text, &needle_with_ext, &direct)
        {
            return Some(entry);
        }
    }
    let mut seen_ids = HashSet::new();
    let mut wm_fallback: Option<DesktopEntry> = None;
    for base in &dirs {
        if !base.is_dir() {
            continue;
        }
        if let Some(exact) = find_in_dir(
            base,
            base,
            needle,
            &needle_with_ext,
            &mut seen_ids,
            &mut wm_fallback,
        ) {
            return Some(exact);
        }
    }
    wm_fallback
}

fn find_in_dir(
    root: &Path,
    dir: &Path,
    needle: &str,
    needle_with_ext: &str,
    seen_ids: &mut HashSet<String>,
    wm_fallback: &mut Option<DesktopEntry>,
) -> Option<DesktopEntry> {
    let Ok(rd) = fs::read_dir(dir) else {
        return None;
    };
    for item in rd.flatten() {
        let path = item.path();
        if path.is_dir() {
            if let Some(found) =
                find_in_dir(root, &path, needle, needle_with_ext, seen_ids, wm_fallback)
            {
                return Some(found);
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let id = rel.to_string_lossy().replace('/', "-");
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(entry) = parse_desktop_entry(&text, &id, &path) else {
            continue;
        };
        if entry.id.eq_ignore_ascii_case(needle) || entry.id.eq_ignore_ascii_case(needle_with_ext) {
            return Some(entry);
        }
        if wm_fallback.is_none()
            && entry
                .startup_wm_class
                .as_deref()
                .is_some_and(|wm| wm.eq_ignore_ascii_case(needle))
        {
            *wm_fallback = Some(entry);
        }
    }
    None
}

fn collect_dir_entries(
    root: &Path,
    dir: &Path,
    seen_ids: &mut HashSet<String>,
    out: &mut Vec<DesktopEntry>,
) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for item in rd.flatten() {
        let path = item.path();
        if path.is_dir() {
            collect_dir_entries(root, &path, seen_ids, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let id = rel.to_string_lossy().replace('/', "-");
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(entry) = parse_desktop_entry(&text, &id, &path) {
            out.push(entry);
        }
    }
}

/// Parses `[Desktop Entry]` text into a [`DesktopEntry`], returning `None` unless `Type=Application`
/// and a non-empty `Name` are present.
#[must_use]
pub fn parse_desktop_entry(contents: &str, id: &str, path: &Path) -> Option<DesktopEntry> {
    let mut in_main_section = false;
    let mut fields: HashMap<String, String> = HashMap::new();
    let locale = env::var("LC_MESSAGES")
        .or_else(|_| env::var("LANG"))
        .unwrap_or_default();
    let short_locale = locale
        .split('.')
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("");
    let lang_only = short_locale.split('_').next().unwrap_or("");

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_main_section = line == "[Desktop Entry]";
            continue;
        }
        if !in_main_section {
            continue;
        }
        let Some((raw_key, raw_val)) = line.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        let val = unescape_desktop_value(raw_val.trim_start());
        if let Some((base_key, loc)) = key.split_once('[')
            && let Some(loc) = loc.strip_suffix(']')
        {
            let loc_clean = loc.split('@').next().unwrap_or(loc);
            if (!short_locale.is_empty() && loc_clean == short_locale)
                || (!lang_only.is_empty()
                    && loc_clean == lang_only
                    && !fields.contains_key(base_key))
            {
                fields.insert(format!("{base_key}#loc"), val);
            }
            continue;
        }
        fields.insert(key.to_owned(), val);
    }

    if fields.get("Type").map(String::as_str) != Some("Application") {
        return None;
    }

    let get_loc = |k: &str| -> Option<String> {
        fields
            .get(&format!("{k}#loc"))
            .or_else(|| fields.get(k))
            .filter(|v| !v.is_empty())
            .cloned()
    };

    let name = get_loc("Name")?;
    let parse_bool = |k: &str| {
        fields
            .get(k)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    };
    let parse_list = |k: &str| -> Vec<String> {
        fields
            .get(k)
            .map(|v| {
                v.split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };

    Some(DesktopEntry {
        id: id.to_owned(),
        path: path.to_path_buf(),
        name,
        generic_name: get_loc("GenericName"),
        comment: get_loc("Comment"),
        exec: fields.get("Exec").filter(|v| !v.is_empty()).cloned(),
        try_exec: fields.get("TryExec").filter(|v| !v.is_empty()).cloned(),
        icon: fields.get("Icon").filter(|v| !v.is_empty()).cloned(),
        categories: parse_list("Categories"),
        keywords: parse_list("Keywords"),
        terminal: parse_bool("Terminal"),
        no_display: parse_bool("NoDisplay"),
        hidden: parse_bool("Hidden"),
        only_show_in: parse_list("OnlyShowIn"),
        not_show_in: parse_list("NotShowIn"),
        startup_wm_class: fields
            .get("StartupWMClass")
            .filter(|v| !v.is_empty())
            .cloned(),
    })
}

fn unescape_desktop_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('s') => out.push(' '),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(';') => out.push(';'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn split_exec_tokens(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut in_token = false;
    let mut chars = raw.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '\\' {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            } else if c == '"' {
                in_quotes = false;
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '"' => {
                in_quotes = true;
                in_token = true;
            }
            '\\' => {
                in_token = true;
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            ch if ch.is_whitespace() => {
                if in_token {
                    out.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            ch => {
                in_token = true;
                cur.push(ch);
            }
        }
    }
    if in_token {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_desktop_entry_and_strips_exec_field_codes() {
        let sample = "\
[Desktop Entry]
Type=Application
Name=WayWi Terminal
GenericName=Terminal Emulator
Exec=\"/usr/bin/my term\" --title %c %U
Icon=utilities-terminal
Categories=System;TerminalEmulator;
StartupWMClass=myterm
Terminal=false
";
        let entry = parse_desktop_entry(
            sample,
            "waywi-term.desktop",
            Path::new("/usr/share/applications/waywi-term.desktop"),
        )
        .expect("valid desktop entry");
        assert_eq!(entry.name, "WayWi Terminal");
        assert_eq!(entry.icon.as_deref(), Some("utilities-terminal"));
        assert_eq!(entry.categories, vec!["System", "TerminalEmulator"]);
        assert_eq!(entry.startup_wm_class.as_deref(), Some("myterm"));
        assert_eq!(
            entry.exec_argv().unwrap(),
            vec!["/usr/bin/my term", "--title", "WayWi Terminal"]
        );
    }
}
