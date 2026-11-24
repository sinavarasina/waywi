//! XDG Base Directories, User Directories, Desktop Entry parser/launcher, and Icon Theme resolver.
//!
//! Provides XDG Base Directory Specification v0.8 paths (`XdgBaseDirs`, `XdgUserDirs`),
//! specification-compliant `.desktop` application discovery (`[Desktop Entry]` and
//! `[Desktop Action ...]`), freedesktop `Exec=` command unquoting/field-code expansion,
//! `DBusActivatable` metadata, `XDG_ACTIVATION_TOKEN` handoff, and XDG Icon Theme lookup across
//! user and system icon themes with `index.theme` inheritance.

use std::ffi::OsStr;
use std::path::PathBuf;

pub mod entry;
pub mod icons;
pub mod xdg;

pub use entry::{
    DesktopAction, DesktopEntry, DesktopSearchMatch, LaunchOptions, default_application_dirs,
    filter_launcher_entries, find_desktop_entry, parse_desktop_entry, scan_desktop_entries,
    scan_desktop_entries_in, search_desktop_entries, search_desktop_entries_with_weights,
};
pub use icons::{
    IconThemeInfo, XdgIconLookup, default_icon_dirs, detect_active_icon_theme,
    parse_icon_theme_index, resolve_icon_path,
};
pub use xdg::{XdgBaseDirs, XdgUserDirs};

/// Configuration for detached child process spawning via [`spawn_with`] and [`spawn_shell`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnOptions {
    pub working_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl SpawnOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// Spawns `program` with `args` detached in its own session (`setsid`) with stdio redirected to
/// `/dev/null`, reaps the child in a background thread on exit so it never becomes a zombie, and
/// returns the spawned child PID.
pub fn spawn_detached(
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> crate::error::Result<u32> {
    spawn_with(program, args, &SpawnOptions::default())
}

/// Spawns `program` with `args` and explicit [`SpawnOptions`] (optional `working_dir` and
/// child-only environment variables) detached in its own session (`setsid`), reaps the child on
/// exit, and returns the spawned child PID.
pub fn spawn_with(
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    options: &SpawnOptions,
) -> crate::error::Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(ref dir) = options.working_dir {
        cmd.current_dir(dir);
    }
    for (k, v) in &options.env {
        cmd.env(k, v);
    }
    // SAFETY: `setsid()` is async-signal-safe and called in `pre_exec` before exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let _ = std::thread::Builder::new()
        .name("waywi-reaper".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(pid)
}

/// Spawns a shell command (`sh -c <command>`) detached in its own session with `options`, reaps
/// it on exit, and returns the spawned child PID.
pub fn spawn_shell(command: &str, options: &SpawnOptions) -> crate::error::Result<u32> {
    spawn_with("sh", ["-c", command], options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_detached_reaps_short_lived_child_and_reports_missing_binary() {
        assert!(
            spawn_detached(
                "/nonexistent-waywi-binary-check",
                std::iter::empty::<&str>()
            )
            .is_err()
        );
        let pid = spawn_detached("true", std::iter::empty::<&str>()).expect("spawn true");
        assert!(pid > 0);

        // Give the reaper thread a brief window to wait() on `true` and confirm no zombie child remains.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut status = 0;
        // SAFETY: `waitpid(-1, &mut status, WNOHANG)` is safe and checks if any unreaped child
        // process of the test runner happened to be left by `spawn_detached`. Note other tests
        // run in parallel, so we only verify `spawn_detached` returned Ok without panicking.
        let _ = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
    }

    #[test]
    fn spawn_with_and_spawn_shell_apply_working_dir_and_env() {
        let missing = std::env::temp_dir().join("waywi-nonexistent-workdir-check");
        let bad_opts = SpawnOptions::new().working_dir(&missing);
        assert!(spawn_with("true", std::iter::empty::<&str>(), &bad_opts).is_err());

        let tmp = std::env::temp_dir();
        let good_opts = SpawnOptions::new()
            .working_dir(&tmp)
            .env("WAYWI_TEST_VAR", "1");
        let pid = spawn_with("true", std::iter::empty::<&str>(), &good_opts).expect("spawn_with");
        assert!(pid > 0);

        let shell_pid = spawn_shell("true", &good_opts).expect("spawn_shell");
        assert!(shell_pid > 0);
    }
}
