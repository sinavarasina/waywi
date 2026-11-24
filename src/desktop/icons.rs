//! Specification-aware XDG Icon Theme resolver (`index.theme` inheritance + bounded caching).

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_RESULTS: usize = 2048;

/// Parsed icon subdirectory descriptor from a theme's `index.theme`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconDirSpec {
    pub subdir: String,
    pub size: u16,
    pub min_size: u16,
    pub max_size: u16,
    pub scalable: bool,
}

/// Parsed theme metadata from `<theme>/index.theme`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconThemeInfo {
    pub name: String,
    pub inherits: Vec<String>,
    pub directories: Vec<IconDirSpec>,
}

/// Stateful caching resolver for XDG icon theme lookups.
#[derive(Debug)]
pub struct XdgIconLookup {
    pinned_theme: Option<String>,
    themes_order: Option<Vec<String>>,
    dirs: Vec<PathBuf>,
    theme_cache: HashMap<String, Option<IconThemeInfo>>,
    result_cache: HashMap<(String, u16), Option<PathBuf>>,
    max_results: usize,
}

impl Default for XdgIconLookup {
    fn default() -> Self {
        Self::new()
    }
}

impl XdgIconLookup {
    /// Creates an XDG icon lookup using standard environment icon search paths and a default
    /// result cache capacity of `2048` entries.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_entries(DEFAULT_MAX_RESULTS)
    }

    /// Creates an XDG icon lookup using standard environment icon search paths with an explicit
    /// maximum number of cached resolution results (`0` disables result caching).
    #[must_use]
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            pinned_theme: None,
            themes_order: None,
            dirs: default_icon_dirs(),
            theme_cache: HashMap::new(),
            result_cache: HashMap::new(),
            max_results: max_entries,
        }
    }

    /// Creates an XDG icon lookup pinned to explicit base directories (for deterministic tests).
    #[must_use]
    pub fn with_dirs(dirs: Vec<PathBuf>, primary_theme: impl Into<String>) -> Self {
        let primary = primary_theme.into();
        let mut s = Self {
            pinned_theme: Some(primary.clone()),
            themes_order: None,
            dirs,
            theme_cache: HashMap::new(),
            result_cache: HashMap::new(),
            max_results: DEFAULT_MAX_RESULTS,
        };
        let order = s.build_theme_chain(&primary);
        s.themes_order = Some(order);
        s
    }

    /// Updates the maximum number of cached resolution results (`0` disables result caching).
    pub fn set_max_entries(&mut self, max_entries: usize) {
        self.max_results = max_entries;
        if self.max_results == 0 || self.result_cache.len() > self.max_results {
            self.result_cache.clear();
        }
    }

    /// Clears all cached icon resolution results and parsed theme metadata.
    pub fn clear(&mut self) {
        self.themes_order = None;
        self.theme_cache.clear();
        self.theme_cache.shrink_to_fit();
        self.result_cache.clear();
        self.result_cache.shrink_to_fit();
    }

    /// Clears only cached `(icon_name, size)` resolution results while retaining parsed
    /// `index.theme` metadata.
    pub fn clear_results(&mut self) {
        self.result_cache.clear();
        self.result_cache.shrink_to_fit();
    }

    /// Resolves an icon name, `file://` URI, or absolute file path for `preferred_size` px.
    pub fn resolve(&mut self, icon_name_or_path: &str, preferred_size: u16) -> Option<PathBuf> {
        let trimmed = icon_name_or_path.trim();
        if trimmed.is_empty() {
            return None;
        }
        let direct = trimmed.strip_prefix("file://").unwrap_or(trimmed);
        let direct_path = Path::new(direct);
        if direct_path.is_absolute() {
            return direct_path.is_file().then(|| direct_path.to_path_buf());
        }

        let cache_key = (trimmed.to_owned(), preferred_size);
        if self.max_results > 0
            && let Some(hit) = self.result_cache.get(&cache_key)
        {
            return hit.clone();
        }

        if self.themes_order.is_none() {
            let active = self
                .pinned_theme
                .clone()
                .unwrap_or_else(detect_active_icon_theme);
            self.themes_order = Some(self.build_theme_chain(&active));
        }

        let candidates = super::xdg::dash_fallback_names(trimmed);
        let exts = ["svg", "png", "webp"];
        let order = self.themes_order.take().unwrap_or_default();

        let mut found = None;
        'outer: for name in &candidates {
            for theme in &order {
                let info = self.load_theme_info(theme).cloned();
                if let Some(ref info) = info {
                    let mut ranked: Vec<&IconDirSpec> = info.directories.iter().collect();
                    ranked.sort_by_key(|d| dir_distance(d, preferred_size));
                    for base in &self.dirs {
                        let theme_root = base.join(theme);
                        if !theme_root.is_dir() {
                            continue;
                        }
                        for spec in &ranked {
                            let dir = theme_root.join(&spec.subdir);
                            if !dir.is_dir() {
                                continue;
                            }
                            for ext in &exts {
                                let p = dir.join(format!("{name}.{ext}"));
                                if p.is_file() {
                                    found = Some(p);
                                    break 'outer;
                                }
                            }
                        }
                    }
                } else if let Some(p) = probe_conventional_layout(&self.dirs, theme, name, &exts) {
                    found = Some(p);
                    break 'outer;
                }
            }
            for pixmap_base in ["/usr/share/pixmaps", "/usr/local/share/pixmaps"] {
                for ext in &exts {
                    let p = PathBuf::from(pixmap_base).join(format!("{name}.{ext}"));
                    if p.is_file() {
                        found = Some(p);
                        break 'outer;
                    }
                }
            }
        }
        self.themes_order = Some(order);

        if self.max_results > 0 {
            if self.result_cache.len() >= self.max_results {
                self.result_cache.clear();
            }
            self.result_cache.insert(cache_key, found.clone());
        }
        found
    }

    fn build_theme_chain(&mut self, primary: &str) -> Vec<String> {
        super::xdg::walk_theme_chain(
            [primary, "Adwaita", "breeze", "Papirus", "hicolor"],
            |name| {
                self.load_theme_info(name)
                    .map(|info| info.inherits.clone())
                    .unwrap_or_default()
            },
        )
    }

    fn load_theme_info(&mut self, theme: &str) -> Option<&IconThemeInfo> {
        if !self.theme_cache.contains_key(theme) {
            let parsed = self.dirs.iter().find_map(|base| {
                let idx = base.join(theme).join("index.theme");
                let text = fs::read_to_string(idx).ok()?;
                parse_icon_theme_index(&text)
            });
            self.theme_cache.insert(theme.to_owned(), parsed);
        }
        self.theme_cache.get(theme).and_then(|opt| opt.as_ref())
    }
}

/// Resolves an XDG icon name or file path using standard system icon theme directories.
#[must_use]
pub fn resolve_icon_path(icon_name_or_path: &str, preferred_size: u16) -> Option<PathBuf> {
    XdgIconLookup::new().resolve(icon_name_or_path, preferred_size)
}

/// Returns standard XDG icon root directories ordered from highest to lowest priority.
#[must_use]
pub fn default_icon_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = env::var("HOME")
        && Path::new(&home).is_absolute()
    {
        dirs.push(PathBuf::from(home).join(".icons"));
    }
    dirs.extend(super::xdg::XdgBaseDirs::from_env().icon_dirs());
    dirs
}

/// Detects the user's configured icon theme name from environment or GTK/KDE settings, falling
/// back to `"hicolor"`.
#[must_use]
pub fn detect_active_icon_theme() -> String {
    if let Ok(theme) = env::var("XDG_ICON_THEME")
        && !theme.trim().is_empty()
    {
        return theme.trim().to_owned();
    }
    if let Some(val) = super::xdg::read_gtk_setting("gtk-icon-theme-name") {
        return val;
    }
    if let Some(config_home) = super::xdg::XdgBaseDirs::from_env().config_home
        && let Ok(text) = fs::read_to_string(config_home.join("kdeglobals"))
        && let Some(val) = parse_kdeglobals_icon_theme(&text)
    {
        return val;
    }
    "hicolor".to_owned()
}

fn parse_kdeglobals_icon_theme(text: &str) -> Option<String> {
    let mut in_icons = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_icons = line[1..line.len() - 1].trim() == "Icons";
            continue;
        }
        if !in_icons {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && k.trim() == "Theme"
        {
            let clean = v.trim().trim_matches('"');
            if !clean.is_empty() {
                return Some(clean.to_owned());
            }
        }
    }
    None
}

/// Parses a freedesktop `index.theme` file into structured directory metadata.
#[must_use]
pub fn parse_icon_theme_index(contents: &str) -> Option<IconThemeInfo> {
    let mut section = String::new();
    let mut theme_name = String::new();
    let mut inherits = Vec::new();
    let mut dir_order = Vec::new();
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_owned();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        if section == "Icon Theme" {
            match key {
                "Name" if theme_name.is_empty() => theme_name = val.to_owned(),
                "Inherits" => inherits = super::xdg::parse_csv_list(val),
                "Directories" => dir_order = super::xdg::parse_csv_list(val),
                _ => {}
            }
        } else if !section.is_empty() {
            sections
                .entry(section.clone())
                .or_default()
                .insert(key.to_owned(), val.to_owned());
        }
    }

    if dir_order.is_empty() && theme_name.is_empty() {
        return None;
    }

    let mut directories = Vec::with_capacity(dir_order.len());
    for subdir in dir_order {
        let Some(props) = sections.get(&subdir) else {
            continue;
        };
        let size = props
            .get("Size")
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(48);
        let dir_type = props.get("Type").map(String::as_str).unwrap_or("Threshold");
        let scalable = dir_type.eq_ignore_ascii_case("Scalable");
        let min_size = props
            .get("MinSize")
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(size);
        let max_size = props
            .get("MaxSize")
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(size);
        directories.push(IconDirSpec {
            subdir,
            size,
            min_size,
            max_size,
            scalable,
        });
    }

    Some(IconThemeInfo {
        name: theme_name,
        inherits,
        directories,
    })
}

fn dir_distance(spec: &IconDirSpec, preferred: u16) -> u16 {
    if spec.scalable {
        return 0;
    }
    if preferred >= spec.min_size && preferred <= spec.max_size {
        return 1;
    }
    spec.size.abs_diff(preferred).saturating_add(2)
}

fn probe_conventional_layout(
    dirs: &[PathBuf],
    theme: &str,
    name: &str,
    exts: &[&str],
) -> Option<PathBuf> {
    const SIZES: &[&str] = &["scalable", "48x48", "64x64", "128x128", "32x32", "24x24"];
    const CATS: &[&str] = &[
        "apps",
        "status",
        "devices",
        "actions",
        "categories",
        "mimetypes",
    ];
    for base in dirs {
        let root = base.join(theme);
        if !root.is_dir() {
            continue;
        }
        for sz in SIZES {
            for cat in CATS {
                let d = root.join(sz).join(cat);
                if !d.is_dir() {
                    continue;
                }
                for ext in exts {
                    let p = d.join(format!("{name}.{ext}"));
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_inherited_icon_theme_and_dashed_fallback() {
        let root = std::env::temp_dir().join(format!("waywi-icons-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let child = root.join("CustomTheme");
        let parent = root.join("BaseTheme/scalable/apps");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&parent).unwrap();
        fs::write(
            child.join("index.theme"),
            "[Icon Theme]\nName=Custom\nInherits=BaseTheme\nDirectories=48x48/apps\n\n[48x48/apps]\nSize=48\nType=Fixed\n",
        )
        .unwrap();
        fs::write(
            root.join("BaseTheme/index.theme"),
            "[Icon Theme]\nName=Base\nDirectories=scalable/apps\n\n[scalable/apps]\nSize=48\nType=Scalable\n",
        )
        .unwrap();
        let target = parent.join("audio-volume.svg");
        fs::write(&target, "<svg/>").unwrap();

        let mut lookup = XdgIconLookup::with_dirs(vec![root.clone()], "CustomTheme");
        assert_eq!(
            lookup.resolve("audio-volume-high", 48),
            Some(target.clone())
        );

        // Bounded cache eviction and explicit clear / clear_results.
        lookup.set_max_entries(2);
        assert_eq!(
            lookup.resolve("audio-volume-high", 48),
            Some(target.clone())
        );
        assert_eq!(lookup.resolve("audio-volume-low", 48), Some(target.clone()));
        assert_eq!(lookup.result_cache.len(), 2);
        assert_eq!(lookup.resolve("missing-icon-check", 48), None);
        assert_eq!(lookup.result_cache.len(), 1);

        lookup.clear_results();
        assert!(lookup.result_cache.is_empty());
        assert!(!lookup.theme_cache.is_empty());

        lookup.clear();
        assert!(lookup.result_cache.is_empty());
        assert!(lookup.theme_cache.is_empty());
        assert_eq!(lookup.resolve("audio-volume-high", 48), Some(target));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn kdeglobals_reads_theme_only_inside_icons_section() {
        let sample = "\
[General]
Theme=WrongGlobalTheme
ColorScheme=BreezeDark

[Icons]
# Theme=CommentedOut
Theme=Papirus-Dark

[KDE]
Theme=WrongKdeTheme
";
        assert_eq!(
            parse_kdeglobals_icon_theme(sample).as_deref(),
            Some("Papirus-Dark")
        );

        let after_section = "\
[Icons]
ForceFontDPI=96

[ColorScheme]
Theme=NotAnIconTheme
";
        assert_eq!(parse_kdeglobals_icon_theme(after_section), None);

        let before_section = "\
Theme=TopLevelIgnored
[General]
Theme=GeneralIgnored
";
        assert_eq!(parse_kdeglobals_icon_theme(before_section), None);
    }

    #[test]
    fn with_max_entries_zero_disables_result_caching() {
        let mut lookup = XdgIconLookup::with_max_entries(0);
        assert_eq!(lookup.max_results, 0);
        let _ = lookup.resolve("definitely-nonexistent-waywi-icon-xyz", 24);
        assert!(lookup.result_cache.is_empty());
    }
}
