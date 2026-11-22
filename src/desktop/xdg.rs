//! XDG Base Directory Specification v0.8, `user-dirs.dirs`, GTK settings, and `index.theme`
//! helpers.

use std::collections::{HashSet, VecDeque};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Resolved XDG Base Directories according to the freedesktop XDG Base Directory Specification
/// v0.8.
///
/// Relative paths in `XDG_*` environment variables are rejected per specification and fall back
/// to their `$HOME`-relative or system defaults.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XdgBaseDirs {
    /// `$XDG_CONFIG_HOME` or `$HOME/.config`.
    pub config_home: Option<PathBuf>,
    /// `$XDG_CONFIG_DIRS` (colon-separated absolute paths) or `["/etc/xdg"]`.
    pub config_dirs: Vec<PathBuf>,
    /// `$XDG_DATA_HOME` or `$HOME/.local/share`.
    pub data_home: Option<PathBuf>,
    /// `$XDG_DATA_DIRS` (colon-separated absolute paths) or `["/usr/local/share", "/usr/share"]`.
    pub data_dirs: Vec<PathBuf>,
    /// `$XDG_CACHE_HOME` or `$HOME/.cache`.
    pub cache_home: Option<PathBuf>,
    /// `$XDG_STATE_HOME` or `$HOME/.local/state`.
    pub state_home: Option<PathBuf>,
    /// `$XDG_RUNTIME_DIR` if set to an absolute path (`None` if unset or relative).
    pub runtime_dir: Option<PathBuf>,
}

impl XdgBaseDirs {
    /// Resolves all XDG Base Directories from the current process environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|k| env::var(k).ok())
    }

    /// Resolves XDG Base Directories using a custom environment lookup closure (useful for
    /// deterministic unit testing without mutating global process environment variables).
    #[must_use]
    pub fn from_lookup<F>(mut get_env: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        let home = get_env("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        let abs_or_home = |raw: Option<String>, rel: &str| -> Option<PathBuf> {
            raw.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| home.as_ref().map(|h| h.join(rel)))
        };
        let split_abs_dirs = |raw: Option<String>, defaults: &[&str]| -> Vec<PathBuf> {
            let parsed: Vec<PathBuf> = raw
                .as_deref()
                .map(|s| {
                    s.split(':')
                        .filter(|part| !part.is_empty())
                        .map(PathBuf::from)
                        .filter(|p| p.is_absolute())
                        .collect()
                })
                .unwrap_or_default();
            if parsed.is_empty() {
                defaults.iter().map(PathBuf::from).collect()
            } else {
                parsed
            }
        };

        Self {
            config_home: abs_or_home(get_env("XDG_CONFIG_HOME"), ".config"),
            config_dirs: split_abs_dirs(get_env("XDG_CONFIG_DIRS"), &["/etc/xdg"]),
            data_home: abs_or_home(get_env("XDG_DATA_HOME"), ".local/share"),
            data_dirs: split_abs_dirs(
                get_env("XDG_DATA_DIRS"),
                &["/usr/local/share", "/usr/share"],
            ),
            cache_home: abs_or_home(get_env("XDG_CACHE_HOME"), ".cache"),
            state_home: abs_or_home(get_env("XDG_STATE_HOME"), ".local/state"),
            runtime_dir: get_env("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
        }
    }

    /// Returns config directories ordered from highest priority (`config_home`) to lowest
    /// (`config_dirs`).
    #[must_use]
    pub fn all_config_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(self.config_dirs.len() + 1);
        if let Some(ref home) = self.config_home {
            out.push(home.clone());
        }
        out.extend(self.config_dirs.iter().cloned());
        out
    }

    /// Returns data directories ordered from highest priority (`data_home`) to lowest
    /// (`data_dirs`).
    #[must_use]
    pub fn all_data_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(self.data_dirs.len() + 1);
        if let Some(ref home) = self.data_home {
            out.push(home.clone());
        }
        out.extend(self.data_dirs.iter().cloned());
        out
    }

    /// Searches config directories in priority order for an existing relative path `rel`.
    #[must_use]
    pub fn find_config_file(&self, rel: impl AsRef<Path>) -> Option<PathBuf> {
        let rel = rel.as_ref();
        self.all_config_dirs()
            .into_iter()
            .map(|dir| dir.join(rel))
            .find(|candidate| candidate.exists())
    }

    /// Searches data directories in priority order for an existing relative path `rel`.
    #[must_use]
    pub fn find_data_file(&self, rel: impl AsRef<Path>) -> Option<PathBuf> {
        let rel = rel.as_ref();
        self.all_data_dirs()
            .into_iter()
            .map(|dir| dir.join(rel))
            .find(|candidate| candidate.exists())
    }
}

/// Parsed XDG user directories from `$XDG_CONFIG_HOME/user-dirs.dirs`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XdgUserDirs {
    pub desktop: Option<PathBuf>,
    pub download: Option<PathBuf>,
    pub templates: Option<PathBuf>,
    pub publicshare: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub music: Option<PathBuf>,
    pub pictures: Option<PathBuf>,
    pub videos: Option<PathBuf>,
}

impl XdgUserDirs {
    /// Loads `$XDG_CONFIG_HOME/user-dirs.dirs` from disk, returning empty (`None`) fields when
    /// the file does not exist or cannot be read.
    #[must_use]
    pub fn load() -> Self {
        let Some(cfg) = xdg_config_home() else {
            return Self::default();
        };
        let Ok(contents) = fs::read_to_string(cfg.join("user-dirs.dirs")) else {
            return Self::default();
        };
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        Self::parse(&contents, home.as_deref())
    }

    /// Parses freedesktop `user-dirs.dirs` shell-assignment syntax (`XDG_*_DIR="$HOME/..."`),
    /// expanding leading `$HOME` against `home` and ignoring comments or non-absolute paths.
    #[must_use]
    pub fn parse(contents: &str, home: Option<&Path>) -> Self {
        let mut out = Self::default();
        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((raw_key, raw_val)) = line.split_once('=') else {
                continue;
            };
            let key = raw_key.trim();
            let Some(resolved) = parse_user_dir_value(raw_val.trim(), home) else {
                continue;
            };
            match key {
                "XDG_DESKTOP_DIR" => out.desktop = Some(resolved),
                "XDG_DOWNLOAD_DIR" => out.download = Some(resolved),
                "XDG_TEMPLATES_DIR" => out.templates = Some(resolved),
                "XDG_PUBLICSHARE_DIR" => out.publicshare = Some(resolved),
                "XDG_DOCUMENTS_DIR" => out.documents = Some(resolved),
                "XDG_MUSIC_DIR" => out.music = Some(resolved),
                "XDG_PICTURES_DIR" => out.pictures = Some(resolved),
                "XDG_VIDEOS_DIR" => out.videos = Some(resolved),
                _ => {}
            }
        }
        out
    }
}

fn parse_user_dir_value(raw: &str, home: Option<&Path>) -> Option<PathBuf> {
    let inner = if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        unescape_double_quoted(&raw[1..raw.len() - 1])
    } else if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        raw[1..raw.len() - 1].to_owned()
    } else {
        raw.to_owned()
    };
    if inner.is_empty() {
        return None;
    }
    let path = if inner == "$HOME" || inner == "${HOME}" {
        home?.to_path_buf()
    } else if let Some(rest) = inner
        .strip_prefix("$HOME/")
        .or_else(|| inner.strip_prefix("${HOME}/"))
    {
        home?.join(rest)
    } else {
        PathBuf::from(inner)
    };
    path.is_absolute().then_some(path)
}

fn unescape_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                match next {
                    '"' | '\\' | '$' | '`' => out.push(next),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            } else {
                out.push('\\');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Returns `$XDG_CONFIG_HOME` (if absolute) or `$HOME/.config`.
#[must_use]
pub fn xdg_config_home() -> Option<PathBuf> {
    XdgBaseDirs::from_env().config_home
}

/// Returns `$XDG_CONFIG_DIRS` (absolute entries) or `["/etc/xdg"]`.
#[must_use]
pub fn xdg_config_dirs() -> Vec<PathBuf> {
    XdgBaseDirs::from_env().config_dirs
}

/// Returns `$XDG_DATA_HOME` (if absolute) or `$HOME/.local/share`.
#[must_use]
pub fn xdg_data_home() -> Option<PathBuf> {
    XdgBaseDirs::from_env().data_home
}

/// Returns `$XDG_DATA_DIRS` (absolute entries) or `["/usr/local/share", "/usr/share"]`.
#[must_use]
pub fn xdg_data_dirs() -> Vec<PathBuf> {
    XdgBaseDirs::from_env().data_dirs
}

/// Returns `$XDG_CACHE_HOME` (if absolute) or `$HOME/.cache`.
#[must_use]
pub fn xdg_cache_home() -> Option<PathBuf> {
    XdgBaseDirs::from_env().cache_home
}

/// Returns `$XDG_STATE_HOME` (if absolute) or `$HOME/.local/state`.
#[must_use]
pub fn xdg_state_home() -> Option<PathBuf> {
    XdgBaseDirs::from_env().state_home
}

/// Returns `$XDG_RUNTIME_DIR` if set to an absolute path.
#[must_use]
pub fn xdg_runtime_dir() -> Option<PathBuf> {
    XdgBaseDirs::from_env().runtime_dir
}

/// Returns `$XDG_DATA_HOME/<subdir>` (falling back to `$HOME/.local/share/<subdir>`) followed by
/// each `$XDG_DATA_DIRS/<subdir>` entry ordered from highest to lowest priority.
pub(crate) fn xdg_data_dirs_for(subdir: &str) -> Vec<PathBuf> {
    XdgBaseDirs::from_env()
        .all_data_dirs()
        .into_iter()
        .map(|d| d.join(subdir))
        .collect()
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
    let mut queue: VecDeque<String> = seeds.into_iter().map(str::to_owned).collect();
    let mut visited = HashSet::new();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_base_dirs_ignores_relative_paths_and_applies_spec_defaults() {
        let dirs = XdgBaseDirs::from_lookup(|k| match k {
            "HOME" => Some("/home/alice".to_owned()),
            "XDG_CONFIG_HOME" => Some("relative/config".to_owned()),
            "XDG_DATA_DIRS" => Some("rel:/opt/share::/usr/share".to_owned()),
            "XDG_CACHE_HOME" => Some("/var/tmp/alice-cache".to_owned()),
            "XDG_STATE_HOME" => None,
            "XDG_RUNTIME_DIR" => Some("/run/user/1000".to_owned()),
            _ => None,
        });
        assert_eq!(dirs.config_home, Some(PathBuf::from("/home/alice/.config")));
        assert_eq!(dirs.config_dirs, vec![PathBuf::from("/etc/xdg")]);
        assert_eq!(
            dirs.data_home,
            Some(PathBuf::from("/home/alice/.local/share"))
        );
        assert_eq!(
            dirs.data_dirs,
            vec![PathBuf::from("/opt/share"), PathBuf::from("/usr/share")]
        );
        assert_eq!(dirs.cache_home, Some(PathBuf::from("/var/tmp/alice-cache")));
        assert_eq!(
            dirs.state_home,
            Some(PathBuf::from("/home/alice/.local/state"))
        );
        assert_eq!(dirs.runtime_dir, Some(PathBuf::from("/run/user/1000")));
        assert_eq!(
            dirs.all_config_dirs(),
            vec![
                PathBuf::from("/home/alice/.config"),
                PathBuf::from("/etc/xdg")
            ]
        );
    }

    #[test]
    fn xdg_user_dirs_parses_home_expansion_quotes_and_rejects_relative() {
        let sample = r#"
# Standard xdg-user-dirs file
XDG_DESKTOP_DIR="$HOME/Desktop"
XDG_DOWNLOAD_DIR="$HOME/Downloads"
XDG_PICTURES_DIR="$HOME/My \"Photos\""
XDG_VIDEOS_DIR="/mnt/media/Videos"
XDG_MUSIC_DIR="relative/should/be/ignored"
XDG_DOCUMENTS_DIR="${HOME}/Docs"
"#;
        let parsed = XdgUserDirs::parse(sample, Some(Path::new("/home/alice")));
        assert_eq!(parsed.desktop, Some(PathBuf::from("/home/alice/Desktop")));
        assert_eq!(
            parsed.download,
            Some(PathBuf::from("/home/alice/Downloads"))
        );
        assert_eq!(
            parsed.pictures,
            Some(PathBuf::from("/home/alice/My \"Photos\""))
        );
        assert_eq!(parsed.videos, Some(PathBuf::from("/mnt/media/Videos")));
        assert_eq!(parsed.music, None);
        assert_eq!(parsed.documents, Some(PathBuf::from("/home/alice/Docs")));
        assert_eq!(parsed.templates, None);
    }
}
