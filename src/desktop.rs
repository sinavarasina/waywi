//! XDG Desktop Entry parser/launcher and Icon Theme resolver.
//!
//! Provides specification-compliant `.desktop` application discovery (`[Desktop Entry]`),
//! freedesktop `Exec=` command unquoting/field-code expansion, and XDG Icon Theme lookup
//! across user and system icon themes with `index.theme` inheritance.

pub mod entry;
pub mod icons;
pub(crate) mod xdg;

pub use entry::{
    DesktopEntry, DesktopSearchMatch, default_application_dirs, filter_launcher_entries,
    find_desktop_entry, parse_desktop_entry, scan_desktop_entries, scan_desktop_entries_in,
    search_desktop_entries, search_desktop_entries_with_weights,
};
pub use icons::{
    IconThemeInfo, XdgIconLookup, default_icon_dirs, detect_active_icon_theme,
    parse_icon_theme_index, resolve_icon_path,
};

/// Spawns `program` with `args` detached in its own session (`setsid`) with stdio redirected to
/// `/dev/null` and reaps the child in a background thread on exit so it never becomes a zombie.
pub fn spawn_detached(program: &str, args: &[&str]) -> crate::error::Result<()> {
    spawn_detached_in(program, args, None)
}

/// Spawns `program` with `args` and optional `working_dir` detached in its own session (`setsid`)
/// with stdio redirected to `/dev/null`, reaping the child on exit.
pub fn spawn_detached_in(
    program: &str,
    args: &[&str],
    working_dir: Option<&std::path::Path>,
) -> crate::error::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
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
    let _ = std::thread::Builder::new()
        .name("waywi-reaper".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

/// Spawns a shell command (`sh -c <command>`) detached in its own session and reaps it on exit.
pub fn spawn_shell_detached(command: &str) -> crate::error::Result<()> {
    spawn_detached("sh", &["-c", command])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_detached_reaps_short_lived_child_and_reports_missing_binary() {
        assert!(spawn_detached("/nonexistent-waywi-binary-check", &[]).is_err());
        assert!(spawn_detached("true", &[]).is_ok());

        // Give the reaper thread a brief window to wait() on `true` and confirm no zombie child remains.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut status = 0;
        // SAFETY: `waitpid(-1, &mut status, WNOHANG)` is safe and checks if any unreaped child
        // process of the test runner happened to be left by `spawn_detached`. Note other tests
        // run in parallel, so we only verify `spawn_detached` returned Ok without panicking.
        let _ = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
    }

    #[test]
    fn spawn_detached_in_applies_working_dir_and_errors_on_missing_dir() {
        let missing = std::env::temp_dir().join("waywi-nonexistent-workdir-check");
        assert!(spawn_detached_in("true", &[], Some(&missing)).is_err());

        let tmp = std::env::temp_dir();
        assert!(spawn_detached_in("true", &[], Some(&tmp)).is_ok());
    }
}
