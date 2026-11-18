//! Freedesktop `.desktop` application entry parser, scanner, and process launcher.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
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
    pub working_dir: Option<PathBuf>,
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

/// Ranked launcher search hit produced by [`search_desktop_entries`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopSearchMatch<'a> {
    pub entry: &'a DesktopEntry,
    pub score: u32,
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

    /// Returns `true` when `TryExec=` is absent or resolves to an existing executable file.
    #[must_use]
    pub fn try_exec_available(&self) -> bool {
        let Some(ref try_exec) = self.try_exec else {
            return true;
        };
        let path_dirs = split_env_path();
        find_executable_in_paths(try_exec, &path_dirs)
    }

    /// Returns `true` when this entry should be displayed in standard app launchers on the
    /// current desktop session (`NoDisplay=false`, `Hidden=false`, `TryExec` satisfied, and
    /// `OnlyShowIn`/`NotShowIn` matched against `XDG_CURRENT_DESKTOP`).
    #[must_use]
    pub fn should_show_in_launcher(&self) -> bool {
        LauncherFilterContext::from_env().is_visible(self)
    }

    /// Resolves the command argv to execute, wrapping `exec_argv` inside `terminal` (or the
    /// auto-detected system terminal when `terminal` is `None`) if `self.terminal` is `true`.
    #[must_use]
    pub fn terminal_argv(&self, terminal: Option<&str>) -> Option<Vec<String>> {
        let path_dirs = split_env_path();
        let env_terminal = env::var("TERMINAL").ok();
        self.resolve_argv_with(terminal, &path_dirs, env_terminal.as_deref())
            .ok()
    }

    fn resolve_argv_with(
        &self,
        terminal: Option<&str>,
        path_dirs: &[PathBuf],
        env_terminal: Option<&str>,
    ) -> Result<Vec<String>> {
        let app_argv = self.exec_argv().ok_or_else(|| {
            WaywiError::InvalidState(format!("desktop entry `{}` has no Exec command", self.id))
        })?;
        if !self.terminal {
            return Ok(app_argv);
        }
        let term_spec = match terminal.map(str::trim).filter(|s| !s.is_empty()) {
            Some(explicit) => explicit.to_owned(),
            None => detect_terminal_in(path_dirs, env_terminal).ok_or_else(|| {
                WaywiError::ProviderUnavailable(format!(
                    "no terminal emulator found to launch `{}`",
                    self.id
                ))
            })?,
        };
        wrap_terminal_argv(&term_spec, app_argv).ok_or_else(|| {
            WaywiError::InvalidState(format!("invalid terminal command `{term_spec}`"))
        })
    }

    fn valid_working_dir(&self) -> Option<&Path> {
        self.working_dir.as_deref().filter(|p| p.is_dir())
    }

    /// Spawns this application with null stdio handles and returns the [`Child`] handle.
    ///
    /// Automatically wraps `Terminal=true` entries using the detected terminal emulator and applies
    /// `Path=` when it points to an existing directory. Callers are responsible for waiting on the
    /// returned [`Child`] to avoid zombie processes; prefer [`Self::launch_detached`] or
    /// [`Self::launch_with_terminal`] when the handle is not needed.
    pub fn launch(&self) -> Result<Child> {
        let path_dirs = split_env_path();
        let env_terminal = env::var("TERMINAL").ok();
        let argv = self.resolve_argv_with(None, &path_dirs, env_terminal.as_deref())?;
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(dir) = self.valid_working_dir() {
            cmd.current_dir(dir);
        }
        cmd.spawn().map_err(WaywiError::Io)
    }

    /// Spawns this application detached in its own session and reaps it on exit.
    ///
    /// Applies `Path=` if it points to an existing directory and wraps `Terminal=true` entries in
    /// the detected system terminal (`xdg-terminal-exec`, `$TERMINAL`, or common Wayland/X11
    /// terminal emulators).
    pub fn launch_detached(&self) -> Result<()> {
        self.launch_with_terminal(None)
    }

    /// Spawns this application detached, using `terminal` when `self.terminal` is `true` (or
    /// auto-detecting the system terminal when `terminal` is `None`).
    pub fn launch_with_terminal(&self, terminal: Option<&str>) -> Result<()> {
        let path_dirs = split_env_path();
        let env_terminal = env::var("TERMINAL").ok();
        let argv = self.resolve_argv_with(terminal, &path_dirs, env_terminal.as_deref())?;
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        super::spawn_detached_in(&argv[0], &args, self.valid_working_dir())
    }
}

/// Filters `entries` to applications visible in a standard launcher on the current desktop,
/// caching `XDG_CURRENT_DESKTOP` and `PATH` `TryExec` checks across all entries.
#[must_use]
pub fn filter_launcher_entries(entries: &[DesktopEntry]) -> Vec<&DesktopEntry> {
    let mut ctx = LauncherFilterContext::from_env();
    entries
        .iter()
        .filter(|entry| ctx.is_visible(entry))
        .collect()
}

/// Searches `entries` for `query` across visible launcher applications and returns matches ordered
/// from highest relevance score to lowest.
///
/// An empty or whitespace-only `query` returns all visible launcher applications in alphabetical
/// order. Whitespace-separated terms act as an AND filter across entry metadata fields.
#[must_use]
pub fn search_desktop_entries<'a>(
    entries: &'a [DesktopEntry],
    query: &str,
) -> Vec<DesktopSearchMatch<'a>> {
    search_desktop_entries_with_weights(entries, query, None)
}

/// Searches `entries` for `query` with optional per-`DesktopEntry::id` launch frequency weights
/// (`launch_counts`) to boost frequently used applications.
#[must_use]
pub fn search_desktop_entries_with_weights<'a>(
    entries: &'a [DesktopEntry],
    query: &str,
    launch_counts: Option<&HashMap<String, u32>>,
) -> Vec<DesktopSearchMatch<'a>> {
    let mut ctx = LauncherFilterContext::from_env();
    search_entries_with_ctx(entries, query, launch_counts, &mut ctx)
}

fn search_entries_with_ctx<'a>(
    entries: &'a [DesktopEntry],
    query: &str,
    launch_counts: Option<&HashMap<String, u32>>,
    ctx: &mut LauncherFilterContext,
) -> Vec<DesktopSearchMatch<'a>> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|t| !t.is_empty())
        .collect();

    let mut matches: Vec<(DesktopSearchMatch<'a>, String)> = Vec::new();
    for entry in entries {
        if !ctx.is_visible(entry) {
            continue;
        }
        let freq_bonus = launch_counts
            .and_then(|map| map.get(&entry.id))
            .copied()
            .unwrap_or(0)
            .min(100_000)
            .saturating_mul(20);
        let name_lower = entry.name.to_lowercase();

        if terms.is_empty() {
            matches.push((
                DesktopSearchMatch {
                    entry,
                    score: freq_bonus,
                },
                name_lower,
            ));
            continue;
        }

        if let Some(match_score) = score_entry_terms(entry, &name_lower, &terms) {
            matches.push((
                DesktopSearchMatch {
                    entry,
                    score: match_score.saturating_add(freq_bonus),
                },
                name_lower,
            ));
        }
    }

    matches.sort_by(|(a, a_name), (b, b_name)| {
        b.score
            .cmp(&a.score)
            .then_with(|| a_name.cmp(b_name))
            .then_with(|| a.entry.id.cmp(&b.entry.id))
    });
    matches.into_iter().map(|(m, _)| m).collect()
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
        working_dir: fields
            .get("Path")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from),
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

struct LauncherFilterContext {
    active_desktops: Vec<String>,
    path_dirs: Vec<PathBuf>,
    exec_cache: HashMap<String, bool>,
}

impl LauncherFilterContext {
    fn from_env() -> Self {
        let current_desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let active_desktops = current_desktop
            .split(':')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        Self {
            active_desktops,
            path_dirs: split_env_path(),
            exec_cache: HashMap::new(),
        }
    }

    fn is_visible(&mut self, entry: &DesktopEntry) -> bool {
        if entry.no_display || entry.hidden {
            return false;
        }
        if !entry.only_show_in.is_empty()
            && !entry.only_show_in.iter().any(|d| {
                self.active_desktops
                    .iter()
                    .any(|cur| cur.eq_ignore_ascii_case(d))
            })
        {
            return false;
        }
        if entry.not_show_in.iter().any(|d| {
            self.active_desktops
                .iter()
                .any(|cur| cur.eq_ignore_ascii_case(d))
        }) {
            return false;
        }
        if let Some(ref try_exec) = entry.try_exec {
            let ok = if let Some(&cached) = self.exec_cache.get(try_exec) {
                cached
            } else {
                let found = find_executable_in_paths(try_exec, &self.path_dirs);
                self.exec_cache.insert(try_exec.clone(), found);
                found
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

fn split_env_path() -> Vec<PathBuf> {
    env::var_os("PATH")
        .map(|v| env::split_paths(&v).collect())
        .unwrap_or_default()
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111) != 0)
}

fn find_executable_in_paths(program: &str, search_dirs: &[PathBuf]) -> bool {
    let trimmed = program.trim();
    if trimmed.is_empty() {
        return false;
    }
    let tokens = split_exec_tokens(trimmed);
    let Some(bin) = tokens.first() else {
        return false;
    };
    let bin_path = Path::new(bin);
    if bin.contains('/') {
        return is_executable_file(bin_path);
    }
    search_dirs
        .iter()
        .any(|dir| is_executable_file(&dir.join(bin_path)))
}

fn detect_terminal_in(search_dirs: &[PathBuf], env_terminal: Option<&str>) -> Option<String> {
    const FALLBACK_TERMINALS: &[&str] = &[
        "footclient",
        "foot",
        "alacritty",
        "kitty",
        "wezterm",
        "ghostty",
        "gnome-terminal",
        "konsole",
        "xterm",
    ];

    if find_executable_in_paths("xdg-terminal-exec", search_dirs) {
        return Some("xdg-terminal-exec".to_owned());
    }
    if let Some(term) = env_terminal.map(str::trim).filter(|s| !s.is_empty())
        && find_executable_in_paths(term, search_dirs)
    {
        return Some(term.to_owned());
    }
    FALLBACK_TERMINALS
        .iter()
        .find(|&&term| find_executable_in_paths(term, search_dirs))
        .map(|&term| term.to_owned())
}

fn wrap_terminal_argv(term_spec: &str, app_argv: Vec<String>) -> Option<Vec<String>> {
    let mut term_tokens = split_exec_tokens(term_spec);
    let bin = term_tokens.first()?.clone();
    let basename = Path::new(&bin)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(bin.as_str());

    match basename {
        "xdg-terminal-exec" => {}
        "wezterm" => {
            term_tokens.push("start".to_owned());
            term_tokens.push("--".to_owned());
        }
        "gnome-terminal" | "kgx" => {
            term_tokens.push("--".to_owned());
        }
        _ => {
            term_tokens.push("-e".to_owned());
        }
    }
    term_tokens.extend(app_argv);
    Some(term_tokens)
}

fn score_entry_terms(entry: &DesktopEntry, name_lower: &str, terms: &[String]) -> Option<u32> {
    let exec_bin = entry
        .exec
        .as_deref()
        .and_then(|raw| split_exec_tokens(raw).into_iter().next())
        .and_then(|bin| {
            Path::new(&bin)
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_lowercase)
        });
    let id_lower = entry.id.to_lowercase();
    let generic_lower = entry.generic_name.as_deref().map(str::to_lowercase);
    let comment_lower = entry.comment.as_deref().map(str::to_lowercase);

    let mut total = 0u32;
    for term in terms {
        let mut best = score_name(name_lower, term).unwrap_or(0);

        if best < 260
            && let Some(ref g) = generic_lower
            && let Some(idx) = g.find(term.as_str())
        {
            best = best.max(if idx == 0 { 280 } else { 250 });
        }
        if best < 240 {
            for kw in &entry.keywords {
                let kw_l = kw.to_lowercase();
                if kw_l.starts_with(term.as_str()) {
                    best = best.max(240);
                    break;
                } else if kw_l.contains(term.as_str()) {
                    best = best.max(210);
                }
            }
        }
        if best < 200
            && (exec_bin
                .as_deref()
                .is_some_and(|e| e.contains(term.as_str()))
                || id_lower.contains(term.as_str()))
        {
            best = best.max(
                if exec_bin
                    .as_deref()
                    .is_some_and(|e| e.starts_with(term.as_str()))
                {
                    200
                } else {
                    175
                },
            );
        }
        if best < 140
            && entry
                .categories
                .iter()
                .any(|c| c.to_lowercase().contains(term.as_str()))
        {
            best = best.max(140);
        }
        if best < 110
            && comment_lower
                .as_deref()
                .is_some_and(|c| c.contains(term.as_str()))
        {
            best = best.max(110);
        }

        if best == 0 {
            return None;
        }
        total = total.saturating_add(best);
    }
    Some(total)
}

fn score_name(name_lower: &str, term: &str) -> Option<u32> {
    if name_lower == term {
        return Some(1000);
    }
    if name_lower.starts_with(term) {
        let penalty = (name_lower.len().saturating_sub(term.len())).min(150) as u32;
        return Some(850 - penalty);
    }
    if let Some(pos) = name_lower.find(term) {
        let at_word_start = name_lower[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| matches!(c, ' ' | '-' | '_' | '/' | '.'));
        return Some(if at_word_start { 700 } else { 550 });
    }
    fuzzy_subsequence_score(name_lower, term)
}

fn fuzzy_subsequence_score(haystack: &str, needle: &str) -> Option<u32> {
    let mut n_chars = needle.chars();
    let mut target = n_chars.next()?;
    let mut prev_match_char_idx: Option<usize> = None;
    let mut first_match_char_idx = None;
    let mut consecutive = 0u32;
    let mut word_starts = 0u32;
    let mut prev_ch = ' ';

    for (char_idx, ch) in haystack.chars().enumerate() {
        if ch == target {
            if first_match_char_idx.is_none() {
                first_match_char_idx = Some(char_idx);
            }
            if prev_match_char_idx == Some(char_idx.wrapping_sub(1)) && char_idx > 0 {
                consecutive = consecutive.saturating_add(1);
            }
            if char_idx == 0 || matches!(prev_ch, ' ' | '-' | '_' | '/' | '.') {
                word_starts = word_starts.saturating_add(1);
            }
            prev_match_char_idx = Some(char_idx);
            if let Some(next) = n_chars.next() {
                target = next;
            } else {
                let span = char_idx.saturating_sub(first_match_char_idx.unwrap_or(0)) as u32;
                let score = 340u32
                    .saturating_add(consecutive.saturating_mul(20))
                    .saturating_add(word_starts.saturating_mul(15))
                    .saturating_sub(span.saturating_mul(4));
                return Some(score.clamp(280, 480));
            }
        }
        prev_ch = ch;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_exec_script(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&p, perms).unwrap();
        p
    }

    #[test]
    fn parses_desktop_entry_path_and_strips_exec_field_codes() {
        let sample = "\
[Desktop Entry]
Type=Application
Name=WayWi Terminal
GenericName=Terminal Emulator
Exec=\"/usr/bin/my term\" --title %c %U
Path=/tmp/waywi-work
Icon=utilities-terminal
Categories=System;TerminalEmulator;
Keywords=shell;prompt;
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
        assert_eq!(
            entry.working_dir.as_deref(),
            Some(Path::new("/tmp/waywi-work"))
        );
        assert_eq!(entry.icon.as_deref(), Some("utilities-terminal"));
        assert_eq!(entry.categories, vec!["System", "TerminalEmulator"]);
        assert_eq!(entry.keywords, vec!["shell", "prompt"]);
        assert_eq!(entry.startup_wm_class.as_deref(), Some("myterm"));
        assert_eq!(
            entry.exec_argv().unwrap(),
            vec!["/usr/bin/my term", "--title", "WayWi Terminal"]
        );
    }

    #[test]
    fn launcher_filter_checks_try_exec_permissions_and_desktop_rules() {
        let root = std::env::temp_dir().join(format!("waywi-tryexec-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let _exec_ok = make_exec_script(&root, "good-tool");
        let non_exec = root.join("not-executable");
        fs::write(&non_exec, "plain").unwrap();
        let mut perms = fs::metadata(&non_exec).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&non_exec, perms).unwrap();

        let visible = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=Good\nExec=good-tool\nTryExec=good-tool\nOnlyShowIn=Hyprland;sway;\n",
            "good.desktop",
            Path::new("/usr/share/applications/good.desktop"),
        )
        .unwrap();
        let missing_bin = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=Missing\nExec=nope\nTryExec=definitely-missing-bin\n",
            "missing.desktop",
            Path::new("/usr/share/applications/missing.desktop"),
        )
        .unwrap();
        let not_exec = parse_desktop_entry(
            &format!(
                "[Desktop Entry]\nType=Application\nName=NonExec\nExec=nope\nTryExec={}\n",
                non_exec.display()
            ),
            "nonexec.desktop",
            Path::new("/usr/share/applications/nonexec.desktop"),
        )
        .unwrap();
        let hidden = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=Hidden\nExec=good-tool\nNoDisplay=true\n",
            "hidden.desktop",
            Path::new("/usr/share/applications/hidden.desktop"),
        )
        .unwrap();
        let wrong_de = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=KdeOnly\nExec=good-tool\nNotShowIn=Hyprland;\n",
            "kde.desktop",
            Path::new("/usr/share/applications/kde.desktop"),
        )
        .unwrap();

        let mut ctx = LauncherFilterContext {
            active_desktops: vec!["Hyprland".to_owned()],
            path_dirs: vec![root.clone()],
            exec_cache: HashMap::new(),
        };
        assert!(ctx.is_visible(&visible));
        assert!(!ctx.is_visible(&missing_bin));
        assert!(!ctx.is_visible(&not_exec));
        assert!(!ctx.is_visible(&hidden));
        assert!(!ctx.is_visible(&wrong_de));
        assert_eq!(ctx.exec_cache.get("good-tool"), Some(&true));
        assert_eq!(ctx.exec_cache.get("definitely-missing-bin"), Some(&false));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn terminal_wrapping_detects_terminals_and_uses_correct_flags() {
        let root = std::env::temp_dir().join(format!("waywi-term-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let entry = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=Monitor\nExec=btop --utf-force\nTerminal=true\n",
            "btop.desktop",
            Path::new("/usr/share/applications/btop.desktop"),
        )
        .unwrap();

        // Explicit terminal overrides follow each emulator's flag convention.
        assert_eq!(
            entry.resolve_argv_with(Some("foot"), &[], None).unwrap(),
            vec!["foot", "-e", "btop", "--utf-force"]
        );
        assert_eq!(
            entry.resolve_argv_with(Some("wezterm"), &[], None).unwrap(),
            vec!["wezterm", "start", "--", "btop", "--utf-force"]
        );
        assert_eq!(
            entry
                .resolve_argv_with(Some("gnome-terminal"), &[], None)
                .unwrap(),
            vec!["gnome-terminal", "--", "btop", "--utf-force"]
        );
        assert_eq!(
            entry
                .resolve_argv_with(Some("xdg-terminal-exec"), &[], None)
                .unwrap(),
            vec!["xdg-terminal-exec", "btop", "--utf-force"]
        );

        // Auto-detection error when no terminal is available.
        let err = entry.resolve_argv_with(None, std::slice::from_ref(&root), None);
        assert!(err.is_err());

        // Fallback picks `$TERMINAL` if present in search dirs, and `xdg-terminal-exec` wins priority.
        make_exec_script(&root, "my-term");
        assert_eq!(
            entry
                .resolve_argv_with(None, std::slice::from_ref(&root), Some("my-term"))
                .unwrap(),
            vec!["my-term", "-e", "btop", "--utf-force"]
        );
        make_exec_script(&root, "xdg-terminal-exec");
        assert_eq!(
            entry
                .resolve_argv_with(None, std::slice::from_ref(&root), Some("my-term"))
                .unwrap(),
            vec!["xdg-terminal-exec", "btop", "--utf-force"]
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_ranks_prefix_substring_fuzzy_keywords_and_weights() {
        let firefox = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=Firefox\nGenericName=Web Browser\nExec=firefox %u\nKeywords=internet;www;\n",
            "firefox.desktop",
            Path::new("/usr/share/applications/firefox.desktop"),
        )
        .unwrap();
        let firejail = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=Firejail Tools\nExec=firetools\n",
            "firetools.desktop",
            Path::new("/usr/share/applications/firetools.desktop"),
        )
        .unwrap();
        let zen = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=Zen Browser\nGenericName=Web Browser\nExec=zen-browser\nKeywords=firefox-alt;privacy;\n",
            "zen.desktop",
            Path::new("/usr/share/applications/zen.desktop"),
        )
        .unwrap();
        let hidden = parse_desktop_entry(
            "[Desktop Entry]\nType=Application\nName=Firefox Helper\nExec=helper\nNoDisplay=true\n",
            "helper.desktop",
            Path::new("/usr/share/applications/helper.desktop"),
        )
        .unwrap();

        let all = vec![zen.clone(), firejail.clone(), firefox.clone(), hidden];
        let mut ctx = LauncherFilterContext {
            active_desktops: vec![],
            path_dirs: vec![],
            exec_cache: HashMap::new(),
        };

        // Empty query returns visible apps sorted alphabetically.
        let empty = search_entries_with_ctx(&all, "   ", None, &mut ctx);
        let empty_ids: Vec<&str> = empty.iter().map(|m| m.entry.id.as_str()).collect();
        assert_eq!(
            empty_ids,
            vec!["firefox.desktop", "firetools.desktop", "zen.desktop"]
        );

        // Exact/prefix name matches rank above secondary keyword matches, and hidden is excluded.
        let hits = search_entries_with_ctx(&all, "firefox", None, &mut ctx);
        let hit_ids: Vec<&str> = hits.iter().map(|m| m.entry.id.as_str()).collect();
        assert_eq!(hit_ids, vec!["firefox.desktop", "zen.desktop"]);
        assert!(hits[0].score > hits[1].score);

        // Multi-term AND query matches across fields ("web priv" -> Zen Browser).
        let multi = search_entries_with_ctx(&all, "web priv", None, &mut ctx);
        assert_eq!(multi.len(), 1);
        assert_eq!(multi[0].entry.id, "zen.desktop");

        // Subsequence fuzzy match ("fz" or "frf" -> Firefox).
        let fuzzy = search_entries_with_ctx(&all, "frf", None, &mut ctx);
        assert_eq!(
            fuzzy.first().map(|m| m.entry.id.as_str()),
            Some("firefox.desktop")
        );

        // Launch frequency weights lift frequently launched apps on equal/empty queries.
        let mut counts = HashMap::new();
        counts.insert("zen.desktop".to_owned(), 5);
        let weighted = search_entries_with_ctx(&all, "", Some(&counts), &mut ctx);
        assert_eq!(weighted[0].entry.id, "zen.desktop");
    }
}
