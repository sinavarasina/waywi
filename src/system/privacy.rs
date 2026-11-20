//! Hardware access / privacy indicator aggregator (camera, microphone, audio capture,
//! screen sharing, and location) combining PipeWire graph monitoring, `/proc/<pid>/fd` scanning,
//! and D-Bus GeoClue2 state.

use std::collections::BTreeSet;
#[cfg(feature = "pipewire")]
use std::collections::HashSet;
use std::fs;
use std::path::Path;
#[cfg(any(feature = "pipewire", feature = "dbus"))]
use std::sync::Arc;
#[cfg(feature = "pipewire")]
use std::sync::Mutex;
#[cfg(any(feature = "pipewire", feature = "dbus"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(feature = "pipewire", feature = "dbus"))]
use std::thread::JoinHandle;

/// Category of sensitive desktop hardware or session capability currently accessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrivacyAccessKind {
    Camera,
    Microphone,
    AudioCapture,
    ScreenShare,
    Location,
}

/// Telemetry source that observed the hardware access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrivacyAccessSource {
    PipeWire,
    ProcFd,
    GeoClue2,
}

/// Single observed active hardware consumer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrivacyAccessEntry {
    pub kind: PrivacyAccessKind,
    pub source: PrivacyAccessSource,
    pub app_name: String,
    pub node_name: Option<String>,
    pub pid: Option<u32>,
    pub device_path: Option<String>,
}

/// Normalized snapshot of active camera, microphone, screen-share, audio-capture, and location consumers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrivacySnapshot {
    pub active: Vec<PrivacyAccessEntry>,
    pub camera_in_use: bool,
    pub microphone_in_use: bool,
    pub audio_capture_in_use: bool,
    pub screen_share_in_use: bool,
    pub location_in_use: bool,
}

impl PrivacySnapshot {
    #[must_use]
    pub fn any_active(&self) -> bool {
        !self.active.is_empty()
    }
}

/// Classifies a live PipeWire node record into an active [`PrivacyAccessEntry`].
#[must_use]
pub fn classify_pipewire_node(
    media_class: &str,
    media_role: Option<&str>,
    node_name: Option<&str>,
    media_name: Option<&str>,
    app_name: Option<&str>,
    pid: Option<u32>,
    is_running: bool,
) -> Option<PrivacyAccessEntry> {
    if !is_running {
        return None;
    }
    let resolved_app = app_name
        .filter(|s| !s.trim().is_empty())
        .or(node_name.filter(|s| !s.trim().is_empty()))
        .unwrap_or("unknown")
        .to_owned();

    let kind = match media_class {
        "Stream/Input/Audio" => {
            let is_screen_or_loopback = node_name.is_some_and(|n| {
                let low = n.to_ascii_lowercase();
                low.contains("loopback") || low.contains("monitor")
            });
            if is_screen_or_loopback {
                PrivacyAccessKind::AudioCapture
            } else {
                PrivacyAccessKind::Microphone
            }
        }
        "Stream/Input/Video" => {
            let is_screencast = media_role.is_some_and(|r| r.eq_ignore_ascii_case("Screen"))
                || media_name.is_some_and(|m| m.to_ascii_lowercase().contains("screencast"))
                || node_name.is_some_and(|n| {
                    let low = n.to_ascii_lowercase();
                    low.contains("screencast")
                        || low.contains("portal")
                        || low.contains("desktop-portal")
                        || low.contains("xdpw")
                })
                || resolved_app.to_ascii_lowercase().contains("screencast");
            if is_screencast {
                PrivacyAccessKind::ScreenShare
            } else {
                PrivacyAccessKind::Camera
            }
        }
        "Video/Source" => {
            let is_screencast = media_role.is_some_and(|r| r.eq_ignore_ascii_case("Screen"))
                || node_name.is_some_and(|n| {
                    let low = n.to_ascii_lowercase();
                    low.contains("screencast") || low.contains("xdpw") || low.contains("screen")
                });
            if is_screencast {
                PrivacyAccessKind::ScreenShare
            } else {
                PrivacyAccessKind::Camera
            }
        }
        _ => return None,
    };

    Some(PrivacyAccessEntry {
        kind,
        source: PrivacyAccessSource::PipeWire,
        app_name: resolved_app,
        node_name: node_name.map(str::to_owned),
        pid,
        device_path: None,
    })
}

/// Scans `/proc/<pid>/fd` symlinks owned by the current user for open `/dev/video*` or ALSA
/// capture (`/dev/snd/pcm*c*`) device descriptors.
#[must_use]
pub fn scan_proc_fd() -> Vec<PrivacyAccessEntry> {
    scan_proc_fd_from(Path::new("/proc"), std::process::id())
}

/// Scans an explicit procfs root cleanly (used by unit tests with synthetic fixtures).
#[must_use]
pub fn scan_proc_fd_from(proc_root: &Path, self_pid: u32) -> Vec<PrivacyAccessEntry> {
    let Ok(rd) = fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut found: BTreeSet<PrivacyAccessEntry> = BTreeSet::new();

    for entry in rd.flatten() {
        let fname = entry.file_name();
        let Some(pid_str) = fname.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        let proc_dir = entry.path();
        let fd_dir = proc_dir.join("fd");
        let Ok(fd_entries) = fs::read_dir(&fd_dir) else {
            continue;
        };
        let mut matched: Vec<(PrivacyAccessKind, String)> = Vec::new();
        for fd_item in fd_entries.flatten() {
            let Ok(target) = fs::read_link(fd_item.path()) else {
                continue;
            };
            let target_str = target.to_string_lossy();
            if target_str.starts_with("/dev/video") {
                matched.push((PrivacyAccessKind::Camera, target_str.into_owned()));
            } else if is_alsa_capture_dev(&target_str) {
                matched.push((PrivacyAccessKind::Microphone, target_str.into_owned()));
            }
        }
        if matched.is_empty() {
            continue;
        }
        let app_name = read_proc_name(&proc_dir).unwrap_or_else(|| format!("pid:{pid}"));
        for (kind, dev_path) in matched {
            found.insert(PrivacyAccessEntry {
                kind,
                source: PrivacyAccessSource::ProcFd,
                app_name: app_name.clone(),
                node_name: None,
                pid: Some(pid),
                device_path: Some(dev_path),
            });
        }
    }

    found.into_iter().collect()
}

fn is_alsa_capture_dev(path: &str) -> bool {
    let Some(base) = path.strip_prefix("/dev/snd/pcm") else {
        return false;
    };
    base.contains('c')
}

fn read_proc_name(proc_dir: &Path) -> Option<String> {
    if let Ok(comm) = fs::read_to_string(proc_dir.join("comm")) {
        let trimmed = comm.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    if let Ok(bytes) = fs::read(proc_dir.join("cmdline")) {
        let first = bytes.split(|&b| b == 0).next().unwrap_or(&[]);
        if let Ok(s) = std::str::from_utf8(first)
            && !s.is_empty()
        {
            let name = Path::new(s)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(s);
            return Some(name.to_owned());
        }
    }
    None
}

#[cfg(feature = "pipewire")]
mod pw_monitor {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use pipewire as pw;
    use pw::node::{Node, NodeListener, NodeState};
    use pw::types::ObjectType;

    use super::*;

    struct TrackedNode {
        media_class: String,
        media_role: Option<String>,
        node_name: Option<String>,
        media_name: Option<String>,
        app_name: Option<String>,
        pid: Option<u32>,
        running: bool,
        _node: Node,
        _listener: NodeListener,
    }

    #[derive(Default)]
    struct PwPrivacyState {
        nodes: HashMap<u32, TrackedNode>,
    }

    impl PwPrivacyState {
        fn publish(&self, shared: &Arc<Mutex<Vec<PrivacyAccessEntry>>>) {
            let entries: Vec<PrivacyAccessEntry> = self
                .nodes
                .values()
                .filter_map(|n| {
                    classify_pipewire_node(
                        &n.media_class,
                        n.media_role.as_deref(),
                        n.node_name.as_deref(),
                        n.media_name.as_deref(),
                        n.app_name.as_deref(),
                        n.pid,
                        n.running,
                    )
                })
                .collect();
            if let Ok(mut g) = shared.lock()
                && *g != entries
            {
                *g = entries;
            }
        }
    }

    pub(super) fn run_pipewire_privacy_thread(
        shared: Arc<Mutex<Vec<PrivacyAccessEntry>>>,
        stop_rx: pw::channel::Receiver<()>,
    ) {
        pw::init();
        let Ok(mainloop) = pw::main_loop::MainLoopRc::new(None) else {
            return;
        };
        let ml_stop = mainloop.downgrade();
        let _stop_listener = stop_rx.attach(mainloop.loop_(), move |()| {
            if let Some(ml) = ml_stop.upgrade() {
                ml.quit();
            }
        });
        let Ok(ctx) = pw::context::ContextRc::new(&mainloop, None) else {
            return;
        };
        let Ok(core) = ctx.connect_rc(None) else {
            return;
        };
        let ml_err = mainloop.downgrade();
        let _core_listener = core
            .add_listener_local()
            .error(move |id, _seq, _res, _msg| {
                if id == 0
                    && let Some(ml) = ml_err.upgrade()
                {
                    ml.quit();
                }
            })
            .register();
        let Ok(registry) = core.get_registry_rc() else {
            return;
        };
        let reg_weak = registry.downgrade();
        let state = Rc::new(RefCell::new(PwPrivacyState::default()));

        let state_add = Rc::clone(&state);
        let shared_add = Arc::clone(&shared);
        let state_rem = Rc::clone(&state);
        let shared_rem = Arc::clone(&shared);

        let _reg_listener = registry
            .add_listener_local()
            .global(move |obj| {
                if obj.type_ != ObjectType::Node {
                    return;
                }
                let Some(props) = obj.props else {
                    return;
                };
                let Some(media_class) = props.get("media.class") else {
                    return;
                };
                if !matches!(
                    media_class,
                    "Stream/Input/Audio" | "Stream/Input/Video" | "Video/Source"
                ) {
                    return;
                }
                let Some(registry) = reg_weak.upgrade() else {
                    return;
                };
                let Ok(node): std::result::Result<Node, _> = registry.bind(obj) else {
                    return;
                };
                let id = obj.id;
                let media_class = media_class.to_owned();
                let media_role = props.get("media.role").map(str::to_owned);
                let node_name = props.get("node.name").map(str::to_owned);
                let media_name = props.get("media.name").map(str::to_owned);
                let app_name = props.get("application.name").map(str::to_owned);
                let pid = props
                    .get("application.process.id")
                    .and_then(|s| s.parse::<u32>().ok());

                let state_node = Rc::clone(&state_add);
                let shared_node = Arc::clone(&shared_add);
                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        let running = matches!(info.state(), NodeState::Running);
                        let mut st = state_node.borrow_mut();
                        let mut changed = false;
                        if let Some(rec) = st.nodes.get_mut(&id) {
                            if rec.running != running {
                                rec.running = running;
                                changed = true;
                            }
                            if let Some(p) = info.props() {
                                if let Some(a) = p.get("application.name")
                                    && rec.app_name.as_deref() != Some(a)
                                {
                                    rec.app_name = Some(a.to_owned());
                                    changed = true;
                                }
                                if let Some(m) = p.get("media.name")
                                    && rec.media_name.as_deref() != Some(m)
                                {
                                    rec.media_name = Some(m.to_owned());
                                    changed = true;
                                }
                            }
                        }
                        if changed {
                            st.publish(&shared_node);
                        }
                    })
                    .register();

                let mut st = state_add.borrow_mut();
                st.nodes.insert(
                    id,
                    TrackedNode {
                        media_class,
                        media_role,
                        node_name,
                        media_name,
                        app_name,
                        pid,
                        running: false,
                        _node: node,
                        _listener: listener,
                    },
                );
            })
            .global_remove(move |id| {
                let mut st = state_rem.borrow_mut();
                if st.nodes.remove(&id).is_some() {
                    st.publish(&shared_rem);
                }
            })
            .register();

        mainloop.run();
    }
}

/// Stateful privacy monitor aggregating PipeWire active nodes, `/proc/<pid>/fd`, and GeoClue2.
pub struct PrivacyMonitor {
    #[cfg(feature = "pipewire")]
    pw_entries: Arc<Mutex<Vec<PrivacyAccessEntry>>>,
    #[cfg(feature = "pipewire")]
    pw_stop: Option<pipewire::channel::Sender<()>>,
    #[cfg(feature = "pipewire")]
    pw_join: Option<JoinHandle<()>>,
    #[cfg(feature = "dbus")]
    geoclue_in_use: Arc<AtomicBool>,
    #[cfg(feature = "dbus")]
    geoclue_started: bool,
    #[cfg(feature = "dbus")]
    geoclue_stop: Option<crate::dbus::worker::AsyncStopSignal>,
    #[cfg(feature = "dbus")]
    geoclue_join: Option<JoinHandle<()>>,
}

impl Default for PrivacyMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PrivacyMonitor {
    fn drop(&mut self) {
        self.stop_background();
    }
}

impl PrivacyMonitor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "pipewire")]
            pw_entries: Arc::new(Mutex::new(Vec::new())),
            #[cfg(feature = "pipewire")]
            pw_stop: None,
            #[cfg(feature = "pipewire")]
            pw_join: None,
            #[cfg(feature = "dbus")]
            geoclue_in_use: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "dbus")]
            geoclue_started: false,
            #[cfg(feature = "dbus")]
            geoclue_stop: None,
            #[cfg(feature = "dbus")]
            geoclue_join: None,
        }
    }

    pub fn ensure_started(&mut self) {
        #[cfg(feature = "pipewire")]
        if self.pw_join.is_none() {
            let shared = Arc::clone(&self.pw_entries);
            let (stop_tx, stop_rx) = pipewire::channel::channel::<()>();
            self.pw_stop = Some(stop_tx);
            self.pw_join = std::thread::Builder::new()
                .name("waywi-privacy-pw".to_owned())
                .spawn(move || pw_monitor::run_pipewire_privacy_thread(shared, stop_rx))
                .ok();
        }
        #[cfg(feature = "dbus")]
        if !self.geoclue_started {
            self.geoclue_started = true;
            let active_flag = Arc::clone(&self.geoclue_in_use);
            let stop_flag = crate::dbus::worker::AsyncStopSignal::new();
            let stop_cl = stop_flag.clone();
            self.geoclue_stop = Some(stop_flag);
            self.geoclue_join = std::thread::Builder::new()
                .name("waywi-privacy-geo".to_owned())
                .spawn(move || run_geoclue_privacy_thread(active_flag, stop_cl))
                .ok();
        }
    }

    pub fn stop_background(&mut self) {
        #[cfg(feature = "pipewire")]
        {
            if let Some(tx) = self.pw_stop.take() {
                let _ = tx.send(());
            }
            if let Some(j) = self.pw_join.take() {
                let _ = j.join();
            }
            if let Ok(mut g) = self.pw_entries.lock() {
                g.clear();
            }
        }
        #[cfg(feature = "dbus")]
        {
            if let Some(ref s) = self.geoclue_stop {
                s.trigger();
            }
            if let Some(j) = self.geoclue_join.take() {
                let _ = j.join();
            }
            self.geoclue_stop = None;
            self.geoclue_in_use.store(false, Ordering::Release);
            self.geoclue_started = false;
        }
    }

    /// Collects the latest aggregated [`PrivacySnapshot`].
    pub fn sample(&mut self) -> PrivacySnapshot {
        self.ensure_started();
        let mut combined: BTreeSet<PrivacyAccessEntry> = BTreeSet::new();
        #[cfg(feature = "pipewire")]
        let mut seen_pw: HashSet<(PrivacyAccessKind, Option<u32>)> = HashSet::new();

        #[cfg(feature = "pipewire")]
        if let Ok(g) = self.pw_entries.lock() {
            for entry in g.iter() {
                seen_pw.insert((entry.kind, entry.pid));
                combined.insert(entry.clone());
            }
        }
        for entry in scan_proc_fd() {
            // Avoid duplicating the exact same PID+kind already reported by PipeWire.
            #[cfg(feature = "pipewire")]
            if entry.pid.is_some() && seen_pw.contains(&(entry.kind, entry.pid)) {
                continue;
            }
            combined.insert(entry);
        }

        #[cfg(feature = "dbus")]
        if self.geoclue_in_use.load(Ordering::Acquire) {
            combined.insert(PrivacyAccessEntry {
                kind: PrivacyAccessKind::Location,
                source: PrivacyAccessSource::GeoClue2,
                app_name: "GeoClue2".to_owned(),
                node_name: None,
                pid: None,
                device_path: None,
            });
        }

        let active: Vec<PrivacyAccessEntry> = combined.into_iter().collect();
        let has = |k: PrivacyAccessKind| active.iter().any(|e| e.kind == k);
        PrivacySnapshot {
            camera_in_use: has(PrivacyAccessKind::Camera),
            microphone_in_use: has(PrivacyAccessKind::Microphone),
            audio_capture_in_use: has(PrivacyAccessKind::AudioCapture),
            screen_share_in_use: has(PrivacyAccessKind::ScreenShare),
            location_in_use: has(PrivacyAccessKind::Location),
            active,
        }
    }
}

#[cfg(feature = "dbus")]
fn run_geoclue_privacy_thread(in_use: Arc<AtomicBool>, stop: crate::dbus::worker::AsyncStopSignal) {
    use futures_lite::StreamExt;
    futures_lite::future::block_on(async move {
        let Ok(conn) = zbus::Connection::system().await else {
            return;
        };
        let query = |c: &zbus::Connection| {
            let c = c.clone();
            async move {
                let reply = c
                    .call_method(
                        Some("org.freedesktop.GeoClue2"),
                        "/org/freedesktop/GeoClue2/Manager",
                        Some("org.freedesktop.DBus.Properties"),
                        "Get",
                        &("org.freedesktop.GeoClue2.Manager", "InUse"),
                    )
                    .await
                    .ok()?;
                let val: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
                Some(matches!(&*val, zbus::zvariant::Value::Bool(true)))
            }
        };
        if let Some(v) = query(&conn).await {
            in_use.store(v, Ordering::Release);
        }
        let Ok(rule) = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.GeoClue2")
        else {
            return;
        };
        let Ok(mut stream) =
            zbus::MessageStream::for_match_rule(rule.build(), &conn, Some(32)).await
        else {
            return;
        };
        while !stop.is_stopped() {
            let next_msg = futures_lite::future::or(async { Some(stream.next().await) }, async {
                stop.wait_stopped().await;
                None
            })
            .await;
            let Some(Some(Ok(_))) = next_msg else {
                break;
            };
            if let Some(v) = query(&conn).await {
                in_use.store(v, Ordering::Release);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_pipewire_mic_camera_and_screencast_accurately() {
        let mic = classify_pipewire_node(
            "Stream/Input/Audio",
            Some("Communication"),
            Some("obs-mic"),
            None,
            Some("OBS Studio"),
            Some(1234),
            true,
        )
        .unwrap();
        assert_eq!(mic.kind, PrivacyAccessKind::Microphone);
        assert_eq!(mic.app_name, "OBS Studio");

        let screen = classify_pipewire_node(
            "Stream/Input/Video",
            Some("Screen"),
            Some("xdg-desktop-portal-wlr"),
            Some("screencast"),
            Some("Chromium"),
            Some(4567),
            true,
        )
        .unwrap();
        assert_eq!(screen.kind, PrivacyAccessKind::ScreenShare);

        let cam = classify_pipewire_node(
            "Video/Source",
            Some("Camera"),
            Some("v4l2_input.pci"),
            None,
            Some("Firefox"),
            Some(999),
            true,
        )
        .unwrap();
        assert_eq!(cam.kind, PrivacyAccessKind::Camera);

        assert!(
            classify_pipewire_node(
                "Stream/Input/Audio",
                None,
                None,
                None,
                Some("IdleApp"),
                Some(1),
                false
            )
            .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn scans_proc_fd_symlinks_for_video_and_alsa_capture() {
        let root = std::env::temp_dir().join(format!("waywi-privacy-proc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let fd_dir = root.join("55555/fd");
        fs::create_dir_all(&fd_dir).unwrap();
        fs::write(root.join("55555/comm"), "cheese\n").unwrap();
        std::os::unix::fs::symlink("/dev/video0", fd_dir.join("3")).unwrap();
        std::os::unix::fs::symlink("/dev/snd/pcmC0D0c", fd_dir.join("4")).unwrap();
        std::os::unix::fs::symlink("/dev/snd/pcmC0D0p", fd_dir.join("5")).unwrap(); // playback only, ignored

        let entries = scan_proc_fd_from(&root, 1);
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|e| e.kind == PrivacyAccessKind::Camera && e.app_name == "cheese")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.kind == PrivacyAccessKind::Microphone && e.app_name == "cheese")
        );

        let _ = fs::remove_dir_all(&root);
    }
}
