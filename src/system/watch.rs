//! Zero-thread Linux `inotify` filesystem watcher integrated directly into `calloop`.
//!
//! Watches files (via their parent directory to survive atomic rename-replace saves by editors
//! such as Vim/Neovim/Helix/VS Code) and directories without spawning any background threads or
//! polling timers.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};
use crate::wayland::WaylandState;

/// Kind of filesystem change reported by [`FileWatch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileWatchKind {
    /// File contents were modified or closed after writing.
    Modified,
    /// A file or directory was created.
    Created,
    /// A file or directory was deleted or unlinked.
    Deleted,
    /// A file or directory was moved or atomically renamed into/out of place.
    Renamed,
    /// File metadata (permissions, timestamps, attributes) changed.
    MetadataChanged,
    /// The kernel `inotify` event queue overflowed; consumers should re-read state.
    Overflow,
}

/// Filesystem change notification emitted by [`FileWatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWatchEvent {
    /// Path affected by the event.
    pub path: PathBuf,
    /// Classification of the filesystem event.
    pub kind: FileWatchKind,
}

/// Zero-thread Linux `inotify` filesystem watcher subscription builders.
pub struct FileWatch;

impl FileWatch {
    /// Watches a single file or directory path for changes (including atomic-rename saves).
    pub fn path<M: 'static>(
        key: impl IntoSubscriptionKey,
        path: impl Into<PathBuf>,
        map: impl Fn(FileWatchEvent) -> M + 'static,
    ) -> Subscription<M> {
        Self::paths(key, [path.into()], map)
    }

    /// Watches multiple file or directory paths under a single subscription key.
    pub fn paths<M: 'static, I, P>(
        key: impl IntoSubscriptionKey,
        paths: I,
        map: impl Fn(FileWatchEvent) -> M + 'static,
    ) -> Subscription<M>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let mut normalized: Vec<PathBuf> = Vec::new();
        for p in paths {
            let pb = normalize_watch_path(p.into());
            if !normalized.contains(&pb) {
                normalized.push(pb);
            }
        }
        Subscription {
            spec: SubscriptionSpec::FileWatch {
                key: key.into_subscription_key(),
                paths: normalized,
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Normalizes relative paths against the current directory when possible so event paths match
/// subscriber paths consistently.
pub(crate) fn normalize_watch_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path
    }
}

/// Returns `true` if `event` should be delivered to a subscriber watching `watched_paths`.
pub(crate) fn event_matches_watched_paths(
    event: &FileWatchEvent,
    watched_paths: &[PathBuf],
) -> bool {
    if event.kind == FileWatchKind::Overflow {
        return true;
    }
    watched_paths
        .iter()
        .any(|target| event.path == *target || (event.path.starts_with(target) && target.is_dir()))
}

const WATCH_MASK: WatchFlags = WatchFlags::from_bits_truncate(
    WatchFlags::MODIFY.bits()
        | WatchFlags::CLOSE_WRITE.bits()
        | WatchFlags::CREATE.bits()
        | WatchFlags::DELETE.bits()
        | WatchFlags::DELETE_SELF.bits()
        | WatchFlags::MOVED_FROM.bits()
        | WatchFlags::MOVED_TO.bits()
        | WatchFlags::MOVE_SELF.bits()
        | WatchFlags::ATTRIB.bits(),
);

#[derive(Debug, Default)]
struct DirWatchEntry {
    dir: PathBuf,
    watch_all: bool,
    file_names: HashSet<OsString>,
}

/// Pure `inotify` watch table + non-blocking event drainer (zero threads).
pub(crate) struct InotifyWatcher {
    fd: Arc<OwnedFd>,
    dir_to_wd: HashMap<PathBuf, i32>,
    wd_to_entry: HashMap<i32, DirWatchEntry>,
}

impl InotifyWatcher {
    pub(crate) fn new() -> Result<Self> {
        let fd = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)
            .map_err(|e| WaywiError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())))?;
        Ok(Self {
            fd: Arc::new(fd),
            dir_to_wd: HashMap::new(),
            wd_to_entry: HashMap::new(),
        })
    }

    pub(crate) fn fd_handle(&self) -> Arc<OwnedFd> {
        Arc::clone(&self.fd)
    }

    /// Synchronizes active `inotify` watches against the full set of desired paths.
    pub(crate) fn sync_paths(&mut self, desired_paths: &[PathBuf]) {
        // Group desired paths by actual directory inode to watch.
        // Directories are watched directly (watch_all = true); files are watched via their parent
        // directory with a filename filter so atomic rename-over-file saves keep working.
        let mut desired_dirs: HashMap<PathBuf, (bool, HashSet<OsString>)> = HashMap::new();
        for path in desired_paths {
            let is_dir = std::fs::metadata(path).is_ok_and(|m| m.is_dir());
            if is_dir {
                let entry = desired_dirs
                    .entry(path.clone())
                    .or_insert_with(|| (false, HashSet::new()));
                entry.0 = true;
            } else if let Some(name) = path.file_name() {
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."));
                let entry = desired_dirs
                    .entry(parent)
                    .or_insert_with(|| (false, HashSet::new()));
                entry.1.insert(name.to_os_string());
            }
        }

        // Remove watches for directories no longer needed.
        let stale_dirs: Vec<PathBuf> = self
            .dir_to_wd
            .keys()
            .filter(|dir| !desired_dirs.contains_key(*dir))
            .cloned()
            .collect();
        for dir in stale_dirs {
            if let Some(wd) = self.dir_to_wd.remove(&dir) {
                let _ = inotify::remove_watch(&*self.fd, wd);
                self.wd_to_entry.remove(&wd);
            }
        }

        // Add or update watches for desired directories.
        for (dir, (watch_all, file_names)) in desired_dirs {
            if let Some(&wd) = self.dir_to_wd.get(&dir) {
                if let Some(entry) = self.wd_to_entry.get_mut(&wd) {
                    entry.watch_all = watch_all;
                    entry.file_names = file_names;
                }
                continue;
            }
            if let Ok(wd) = inotify::add_watch(&*self.fd, &dir, WATCH_MASK) {
                self.dir_to_wd.insert(dir.clone(), wd);
                self.wd_to_entry.insert(
                    wd,
                    DirWatchEntry {
                        dir,
                        watch_all,
                        file_names,
                    },
                );
            }
        }
    }

    /// Drains all currently queued kernel `inotify` events without blocking.
    pub(crate) fn drain_events(&self) -> Vec<FileWatchEvent> {
        let mut out = Vec::new();
        let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
        let mut reader = inotify::Reader::new(&*self.fd, &mut buf);

        loop {
            match reader.next() {
                Ok(ev) => {
                    let flags = ev.events();
                    if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                        push_dedup(
                            &mut out,
                            FileWatchEvent {
                                path: PathBuf::new(),
                                kind: FileWatchKind::Overflow,
                            },
                        );
                        continue;
                    }
                    if flags.contains(ReadFlags::IGNORED) {
                        continue;
                    }
                    let Some(kind) = classify_flags(flags) else {
                        continue;
                    };
                    let Some(entry) = self.wd_to_entry.get(&ev.wd()) else {
                        continue;
                    };
                    let file_name = ev
                        .file_name()
                        .map(|c| std::ffi::OsStr::from_bytes(c.to_bytes()));

                    let matched_path = match file_name {
                        Some(name) => {
                            if entry.watch_all || entry.file_names.contains(name) {
                                Some(entry.dir.join(name))
                            } else {
                                None
                            }
                        }
                        None => {
                            if entry.watch_all {
                                Some(entry.dir.clone())
                            } else {
                                None
                            }
                        }
                    };

                    if let Some(path) = matched_path {
                        push_dedup(&mut out, FileWatchEvent { path, kind });
                    }
                }
                Err(Errno::AGAIN | Errno::INTR) => break,
                Err(_) => break,
            }
        }

        out
    }
}

fn classify_flags(flags: ReadFlags) -> Option<FileWatchKind> {
    if flags.contains(ReadFlags::MOVED_TO)
        || flags.contains(ReadFlags::MOVED_FROM)
        || flags.contains(ReadFlags::MOVE_SELF)
    {
        Some(FileWatchKind::Renamed)
    } else if flags.contains(ReadFlags::CREATE) {
        Some(FileWatchKind::Created)
    } else if flags.contains(ReadFlags::DELETE) || flags.contains(ReadFlags::DELETE_SELF) {
        Some(FileWatchKind::Deleted)
    } else if flags.contains(ReadFlags::CLOSE_WRITE) || flags.contains(ReadFlags::MODIFY) {
        Some(FileWatchKind::Modified)
    } else if flags.contains(ReadFlags::ATTRIB) {
        Some(FileWatchKind::MetadataChanged)
    } else {
        None
    }
}

fn push_dedup(out: &mut Vec<FileWatchEvent>, event: FileWatchEvent) {
    if out.last() != Some(&event) {
        out.push(event);
    }
}

/// Active `calloop` registration for the zero-thread `inotify` watcher.
pub(crate) struct InotifyRegistration {
    watcher: Rc<RefCell<InotifyWatcher>>,
    token: RegistrationToken,
}

impl InotifyRegistration {
    pub(crate) fn register(
        loop_handle: &LoopHandle<'static, WaylandState>,
        on_events: impl Fn(Vec<FileWatchEvent>) + 'static,
    ) -> Result<Self> {
        let watcher = Rc::new(RefCell::new(InotifyWatcher::new()?));
        let fd = watcher.borrow().fd_handle();
        let watcher_cb = Rc::clone(&watcher);
        let source = Generic::new(fd, Interest::READ, Mode::Level);
        let token = loop_handle
            .insert_source(source, move |_, _, _| {
                let events = watcher_cb.borrow().drain_events();
                if !events.is_empty() {
                    on_events(events);
                }
                Ok(PostAction::Continue)
            })
            .map_err(|e| {
                WaywiError::InvalidState(format!("failed to register inotify source: {e}"))
            })?;
        Ok(Self { watcher, token })
    }

    pub(crate) fn sync_paths(&self, desired_paths: &[PathBuf]) {
        self.watcher.borrow_mut().sync_paths(desired_paths);
    }

    pub(crate) fn remove(self, loop_handle: &LoopHandle<'static, WaylandState>) {
        loop_handle.remove(self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inotify_detects_direct_modify_atomic_rename_and_dir_events() {
        let base = std::env::temp_dir().join(format!(
            "waywi-inotify-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let cfg_file = base.join("config.toml");
        let ignored_file = base.join("unrelated.txt");
        let tmp_file = base.join("config.toml.tmp");
        std::fs::write(&cfg_file, "theme = \"dark\"\n").unwrap();

        let mut watcher = InotifyWatcher::new().unwrap();
        watcher.sync_paths(std::slice::from_ref(&cfg_file));

        // 1. Write to unrelated sibling should be filtered out.
        std::fs::write(&ignored_file, "ignore me").unwrap();
        let evs = watcher.drain_events();
        assert!(
            evs.is_empty(),
            "unrelated sibling must not emit events: {evs:?}"
        );

        // 2. Direct modify of watched file.
        std::fs::write(&cfg_file, "theme = \"light\"\n").unwrap();
        let evs = watcher.drain_events();
        assert!(
            evs.iter()
                .any(|e| e.path == cfg_file && e.kind == FileWatchKind::Modified),
            "expected Modified event for {}: {evs:?}",
            cfg_file.display()
        );

        // 3. Atomic save (write tmp + rename over watched file).
        std::fs::write(&tmp_file, "theme = \"nord\"\n").unwrap();
        std::fs::rename(&tmp_file, &cfg_file).unwrap();
        let evs = watcher.drain_events();
        assert!(
            evs.iter()
                .any(|e| e.path == cfg_file && e.kind == FileWatchKind::Renamed),
            "expected Renamed event on atomic save: {evs:?}"
        );

        // 4. Directory watch captures created & deleted children.
        watcher.sync_paths(std::slice::from_ref(&base));
        let added = base.join("theme.css");
        std::fs::write(&added, "body {}").unwrap();
        std::fs::remove_file(&added).unwrap();
        let evs = watcher.drain_events();
        assert!(
            evs.iter()
                .any(|e| e.path == added && e.kind == FileWatchKind::Created),
            "expected Created event in watched dir: {evs:?}"
        );
        assert!(
            evs.iter()
                .any(|e| e.path == added && e.kind == FileWatchKind::Deleted),
            "expected Deleted event in watched dir: {evs:?}"
        );

        // 5. Removing all paths clears watches cleanly.
        watcher.sync_paths(&[]);
        assert!(watcher.dir_to_wd.is_empty());
        assert!(watcher.wd_to_entry.is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }
}
