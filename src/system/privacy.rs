//! Hardware access / privacy indicator aggregator (camera, microphone, audio capture,
//! screen sharing, and location) combining stateless `/proc/<pid>/fd` scanning with opt-in
//! background PipeWire graph monitoring and D-Bus GeoClue2 state.

use std::collections::BTreeSet;
#[cfg(feature = "pipewire")]
use std::collections::HashSet;
use std::fs;
use std::path::Path;
#[cfg(any(feature = "pipewire", feature = "dbus"))]
use std::sync::Arc;
#[cfg(feature = "pipewire")]
use std::sync::Mutex;
#[cfg(feature = "dbus")]
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

fn build_snapshot(entries: BTreeSet<PrivacyAccessEntry>) -> PrivacySnapshot {
    let active: Vec<PrivacyAccessEntry> = entries.into_iter().collect();
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

/// Performs a stateless one-shot privacy scan of `/proc/<pid>/fd` for open `/dev/video*`
/// cameras and ALSA capture (`/dev/snd/pcm*c*`) devices without spawning or joining any
/// PipeWire or D-Bus background threads.
#[must_use]
pub fn sample_privacy_oneshot() -> PrivacySnapshot {
    sample_privacy_oneshot_from(Path::new("/proc"), std::process::id())
}

/// Performs a stateless one-shot privacy scan against an explicit procfs root (for unit tests).
#[must_use]
pub fn sample_privacy_oneshot_from(proc_root: &Path, self_pid: u32) -> PrivacySnapshot {
    let entries: BTreeSet<PrivacyAccessEntry> =
        scan_proc_fd_from(proc_root, self_pid).into_iter().collect();
    build_snapshot(entries)
}

/// PipeWire node properties and optional link-graph context passed to [`classify_pipewire_node`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipeWireNodeProps<'a> {
    pub media_class: &'a str,
    pub media_role: Option<&'a str>,
    pub node_name: Option<&'a str>,
    pub media_name: Option<&'a str>,
    pub app_name: Option<&'a str>,
    pub device_api: Option<&'a str>,
    pub target_object: Option<&'a str>,
    pub pid: Option<u32>,
    pub is_running: bool,
    /// `true` when PipeWire reports `stream.capture.sink = "true" | "1"`.
    pub stream_capture_sink: bool,
    /// `true` when PipeWire reports `stream.monitor = "true" | "1"`.
    pub stream_monitor: bool,
    /// Authoritative PipeWire link-graph classification for `Stream/Input/Audio`:
    /// - `Some(true)`: linked from an `Audio/Sink` or `Stream/Output/Audio` (output monitor/loopback)
    /// - `Some(false)`: linked from an `Audio/Source` (microphone/input)
    /// - `None`: no active graph link observed; fall back to node properties
    pub linked_from_output_monitor: Option<bool>,
    /// Authoritative PipeWire link-graph classification for `Stream/Input/Video`:
    /// - `Some(true)`: linked from a screen-capture `Video/Source`
    /// - `Some(false)`: linked from a physical camera `Video/Source` (`v4l2`/`libcamera`)
    /// - `None`: no active graph link observed; fall back to node properties
    pub linked_from_screencast: Option<bool>,
}

#[cfg(feature = "pipewire")]
fn parse_pw_bool(val: Option<&str>) -> bool {
    matches!(
        val.map(str::trim),
        Some("true" | "TRUE" | "True" | "1" | "yes" | "YES")
    )
}

fn is_physical_camera_api(device_api: Option<&str>, node_name: Option<&str>) -> bool {
    device_api.is_some_and(|api| {
        api.eq_ignore_ascii_case("v4l2") || api.eq_ignore_ascii_case("libcamera")
    }) || node_name
        .is_some_and(|n| n.starts_with("v4l2_input.") || n.starts_with("libcamera_input."))
}

fn is_screencast_role(role: Option<&str>) -> bool {
    role.is_some_and(|r| r.eq_ignore_ascii_case("Screen") || r.eq_ignore_ascii_case("ScreenCast"))
}

fn is_audio_monitor_role(role: Option<&str>) -> bool {
    role.is_some_and(|r| {
        r.eq_ignore_ascii_case("Monitor")
            || r.eq_ignore_ascii_case("Screen")
            || r.eq_ignore_ascii_case("DSP")
    })
}

/// Classifies a live PipeWire node record into an active [`PrivacyAccessEntry`].
///
/// Prefers authoritative PipeWire link-graph topology and explicit protocol properties
/// (`stream.capture.sink`, `stream.monitor`, `media.role`, `device.api`, `target.object`) before
/// falling back to conservative naming conventions.
#[must_use]
pub fn classify_pipewire_node(props: PipeWireNodeProps<'_>) -> Option<PrivacyAccessEntry> {
    if !props.is_running {
        return None;
    }
    let resolved_app = props
        .app_name
        .filter(|s| !s.trim().is_empty())
        .or(props.node_name.filter(|s| !s.trim().is_empty()))
        .unwrap_or("unknown")
        .to_owned();

    let kind = match props.media_class {
        "Stream/Input/Audio" => {
            let is_output_monitor = match props.linked_from_output_monitor {
                Some(from_monitor) => from_monitor,
                None => {
                    props.stream_capture_sink
                        || props.stream_monitor
                        || is_audio_monitor_role(props.media_role)
                        || props.target_object.is_some_and(|t| t.ends_with(".monitor"))
                        || props.node_name.is_some_and(|n| {
                            let low = n.to_ascii_lowercase();
                            low.contains("loopback")
                                || low.ends_with(".monitor")
                                || low.ends_with("-monitor")
                                || low.ends_with("_monitor")
                        })
                }
            };
            if is_output_monitor {
                PrivacyAccessKind::AudioCapture
            } else {
                PrivacyAccessKind::Microphone
            }
        }
        "Stream/Input/Video" => {
            let is_screencast = match props.linked_from_screencast {
                Some(from_screen) => from_screen,
                None => {
                    if is_physical_camera_api(props.device_api, props.node_name)
                        || props
                            .media_role
                            .is_some_and(|r| r.eq_ignore_ascii_case("Camera"))
                    {
                        false
                    } else {
                        is_screencast_role(props.media_role)
                            || props
                                .media_name
                                .is_some_and(|m| m.to_ascii_lowercase().contains("screencast"))
                            || props.node_name.is_some_and(|n| {
                                let low = n.to_ascii_lowercase();
                                low.contains("screencast")
                                    || low.contains("xdg-desktop-portal")
                                    || low.starts_with("xdpw")
                            })
                            || resolved_app.to_ascii_lowercase().contains("screencast")
                    }
                }
            };
            if is_screencast {
                PrivacyAccessKind::ScreenShare
            } else {
                PrivacyAccessKind::Camera
            }
        }
        "Video/Source" => {
            let is_screencast = if is_physical_camera_api(props.device_api, props.node_name) {
                false
            } else {
                is_screencast_role(props.media_role)
                    || props.node_name.is_some_and(|n| {
                        let low = n.to_ascii_lowercase();
                        low.contains("screencast")
                            || low.contains("xdg-desktop-portal")
                            || low.starts_with("xdpw")
                    })
            };
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
        node_name: props.node_name.map(str::to_owned),
        pid: props.pid,
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
            if is_v4l2_video_dev(&target_str) {
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

fn strip_deleted_suffix(path: &str) -> &str {
    path.strip_suffix(" (deleted)").unwrap_or(path)
}

/// Matches Linux V4L2 device nodes strictly (`/dev/video<digits>`).
fn is_v4l2_video_dev(path: &str) -> bool {
    let clean = strip_deleted_suffix(path);
    clean
        .strip_prefix("/dev/video")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Matches Linux ALSA PCM capture device nodes strictly (`/dev/snd/pcmC<card>D<dev>c`).
fn is_alsa_capture_dev(path: &str) -> bool {
    let clean = strip_deleted_suffix(path);
    let Some(rest) = clean.strip_prefix("/dev/snd/pcmC") else {
        return false;
    };
    let Some(after_c) = rest.strip_suffix('c') else {
        return false;
    };
    let Some((card, dev)) = after_c.split_once('D') else {
        return false;
    };
    !card.is_empty()
        && !dev.is_empty()
        && card.bytes().all(|b| b.is_ascii_digit())
        && dev.bytes().all(|b| b.is_ascii_digit())
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum GraphSourceKind {
        AudioSinkOrOutput,
        AudioSource,
        VideoCamera,
        VideoScreenCast,
    }

    struct TrackedNode {
        media_class: String,
        media_role: Option<String>,
        node_name: Option<String>,
        media_name: Option<String>,
        app_name: Option<String>,
        device_api: Option<String>,
        target_object: Option<String>,
        pid: Option<u32>,
        stream_capture_sink: bool,
        stream_monitor: bool,
        running: bool,
        _node: Node,
        _listener: NodeListener,
    }

    #[derive(Default)]
    struct PwPrivacyState {
        nodes: HashMap<u32, TrackedNode>,
        /// Lightweight graph index of upstream producer nodes (no `registry.bind()` needed).
        source_kinds: HashMap<u32, GraphSourceKind>,
        /// Active PipeWire links (`link_id -> (output_node_id, input_node_id)`).
        links: HashMap<u32, (u32, u32)>,
    }

    impl PwPrivacyState {
        fn resolve_audio_link_monitor(&self, input_node_id: u32) -> Option<bool> {
            let mut saw_monitor = false;
            let mut saw_mic = false;
            for &(out_id, in_id) in self.links.values() {
                if in_id != input_node_id {
                    continue;
                }
                match self.source_kinds.get(&out_id) {
                    Some(GraphSourceKind::AudioSinkOrOutput) => saw_monitor = true,
                    Some(GraphSourceKind::AudioSource) => saw_mic = true,
                    _ => {}
                }
            }
            if saw_mic {
                // If linked to an actual microphone source, report Microphone honestly.
                Some(false)
            } else if saw_monitor {
                Some(true)
            } else {
                None
            }
        }

        fn resolve_video_link_screencast(&self, input_node_id: u32) -> Option<bool> {
            let mut saw_screen = false;
            let mut saw_camera = false;
            for &(out_id, in_id) in self.links.values() {
                if in_id != input_node_id {
                    continue;
                }
                match self.source_kinds.get(&out_id) {
                    Some(GraphSourceKind::VideoScreenCast) => saw_screen = true,
                    Some(GraphSourceKind::VideoCamera) => saw_camera = true,
                    _ => {}
                }
            }
            if saw_camera {
                Some(false)
            } else if saw_screen {
                Some(true)
            } else {
                None
            }
        }

        fn publish(&self, shared: &Arc<Mutex<Vec<PrivacyAccessEntry>>>) {
            let entries: Vec<PrivacyAccessEntry> = self
                .nodes
                .iter()
                .filter_map(|(&id, n)| {
                    let linked_from_output_monitor = if n.media_class == "Stream/Input/Audio" {
                        self.resolve_audio_link_monitor(id)
                    } else {
                        None
                    };
                    let linked_from_screencast = if n.media_class == "Stream/Input/Video" {
                        self.resolve_video_link_screencast(id)
                    } else {
                        None
                    };
                    classify_pipewire_node(PipeWireNodeProps {
                        media_class: &n.media_class,
                        media_role: n.media_role.as_deref(),
                        node_name: n.node_name.as_deref(),
                        media_name: n.media_name.as_deref(),
                        app_name: n.app_name.as_deref(),
                        device_api: n.device_api.as_deref(),
                        target_object: n.target_object.as_deref(),
                        pid: n.pid,
                        is_running: n.running,
                        stream_capture_sink: n.stream_capture_sink,
                        stream_monitor: n.stream_monitor,
                        linked_from_output_monitor,
                        linked_from_screencast,
                    })
                })
                .collect();
            if let Ok(mut g) = shared.lock()
                && *g != entries
            {
                *g = entries;
            }
        }
    }

    fn classify_graph_source(props: &pw::spa::utils::dict::DictRef) -> Option<GraphSourceKind> {
        let media_class = props.get("media.class")?;
        if media_class == "Audio/Sink"
            || media_class == "Audio/Duplex"
            || media_class.starts_with("Stream/Output/Audio")
        {
            Some(GraphSourceKind::AudioSinkOrOutput)
        } else if media_class == "Audio/Source" || media_class == "Audio/Source/Virtual" {
            Some(GraphSourceKind::AudioSource)
        } else if media_class == "Video/Source" {
            if is_physical_camera_api(props.get("device.api"), props.get("node.name")) {
                Some(GraphSourceKind::VideoCamera)
            } else if is_screencast_role(props.get("media.role"))
                || props.get("node.name").is_some_and(|n| {
                    let low = n.to_ascii_lowercase();
                    low.contains("screencast")
                        || low.contains("xdg-desktop-portal")
                        || low.starts_with("xdpw")
                })
            {
                Some(GraphSourceKind::VideoScreenCast)
            } else {
                Some(GraphSourceKind::VideoCamera)
            }
        } else {
            None
        }
    }

    pub(super) fn run_pipewire_privacy_thread(
        shared: Arc<Mutex<Vec<PrivacyAccessEntry>>>,
        stop_rx: pw::channel::Receiver<()>,
    ) {
        crate::audio::pipewire::ensure_pw_init();
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
                let Some(props) = obj.props else {
                    return;
                };
                if obj.type_ == ObjectType::Link {
                    let Some(out_id) = props
                        .get("link.output.node")
                        .and_then(|s| s.parse::<u32>().ok())
                    else {
                        return;
                    };
                    let Some(in_id) = props
                        .get("link.input.node")
                        .and_then(|s| s.parse::<u32>().ok())
                    else {
                        return;
                    };
                    let mut st = state_add.borrow_mut();
                    st.links.insert(obj.id, (out_id, in_id));
                    if st.nodes.contains_key(&in_id) {
                        st.publish(&shared_add);
                    }
                    return;
                }
                if obj.type_ != ObjectType::Node {
                    return;
                }
                if let Some(src_kind) = classify_graph_source(props) {
                    let mut st = state_add.borrow_mut();
                    st.source_kinds.insert(obj.id, src_kind);
                }
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
                let device_api = props.get("device.api").map(str::to_owned);
                let target_object = props
                    .get("target.object")
                    .or_else(|| props.get("node.target"))
                    .map(str::to_owned);
                let pid = props
                    .get("application.process.id")
                    .and_then(|s| s.parse::<u32>().ok());
                let stream_capture_sink = parse_pw_bool(props.get("stream.capture.sink"));
                let stream_monitor = parse_pw_bool(props.get("stream.monitor"));

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
                                if let Some(r) = p.get("media.role")
                                    && rec.media_role.as_deref() != Some(r)
                                {
                                    rec.media_role = Some(r.to_owned());
                                    changed = true;
                                }
                                if let Some(n) = p.get("node.name")
                                    && rec.node_name.as_deref() != Some(n)
                                {
                                    rec.node_name = Some(n.to_owned());
                                    changed = true;
                                }
                                if let Some(api) = p.get("device.api")
                                    && rec.device_api.as_deref() != Some(api)
                                {
                                    rec.device_api = Some(api.to_owned());
                                    changed = true;
                                }
                                if let Some(t) =
                                    p.get("target.object").or_else(|| p.get("node.target"))
                                    && rec.target_object.as_deref() != Some(t)
                                {
                                    rec.target_object = Some(t.to_owned());
                                    changed = true;
                                }
                                let cap_sink = parse_pw_bool(p.get("stream.capture.sink"));
                                if cap_sink && !rec.stream_capture_sink {
                                    rec.stream_capture_sink = true;
                                    changed = true;
                                }
                                let mon = parse_pw_bool(p.get("stream.monitor"));
                                if mon && !rec.stream_monitor {
                                    rec.stream_monitor = true;
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
                        device_api,
                        target_object,
                        pid,
                        stream_capture_sink,
                        stream_monitor,
                        running: false,
                        _node: node,
                        _listener: listener,
                    },
                );
            })
            .global_remove(move |id| {
                let mut st = state_rem.borrow_mut();
                let removed_node = st.nodes.remove(&id).is_some();
                let removed_src = st.source_kinds.remove(&id).is_some();
                let removed_link = st.links.remove(&id);
                if removed_node
                    || removed_src
                    || removed_link.is_some_and(|(_, in_id)| st.nodes.contains_key(&in_id))
                {
                    st.publish(&shared_rem);
                }
            })
            .register();

        mainloop.run();
    }
}

#[cfg(feature = "dbus")]
mod geoclue_monitor {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    use futures_lite::StreamExt;

    #[derive(Clone, Default)]
    pub(super) struct GeoClueStopSignal {
        stopped: Arc<AtomicBool>,
        waker: Arc<Mutex<Option<Waker>>>,
    }

    impl GeoClueStopSignal {
        pub(super) fn new() -> Self {
            Self::default()
        }

        pub(super) fn is_stopped(&self) -> bool {
            self.stopped.load(Ordering::Acquire)
        }

        pub(super) fn trigger(&self) {
            self.stopped.store(true, Ordering::Release);
            if let Ok(mut guard) = self.waker.lock()
                && let Some(w) = guard.take()
            {
                w.wake();
            }
        }

        fn wait_stopped(&self) -> StopFuture {
            StopFuture {
                stopped: Arc::clone(&self.stopped),
                waker: Arc::clone(&self.waker),
            }
        }
    }

    struct StopFuture {
        stopped: Arc<AtomicBool>,
        waker: Arc<Mutex<Option<Waker>>>,
    }

    impl Future for StopFuture {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.stopped.load(Ordering::Acquire) {
                return Poll::Ready(());
            }
            if let Ok(mut guard) = self.waker.lock() {
                *guard = Some(cx.waker().clone());
            }
            if self.stopped.load(Ordering::Acquire) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }

    async fn cancellable<T>(stop: &GeoClueStopSignal, fut: impl Future<Output = T>) -> Option<T> {
        futures_lite::future::or(async { Some(fut.await) }, async {
            stop.wait_stopped().await;
            None
        })
        .await
    }

    async fn query_in_use(conn: &zbus::Connection) -> Option<bool> {
        let reply = conn
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

    pub(super) fn run_geoclue_privacy_thread(in_use: Arc<AtomicBool>, stop: GeoClueStopSignal) {
        futures_lite::future::block_on(async move {
            let Some(Ok(conn)) = cancellable(&stop, zbus::Connection::system()).await else {
                return;
            };
            if let Some(Some(v)) = cancellable(&stop, query_in_use(&conn)).await {
                in_use.store(v, Ordering::Release);
            }
            if stop.is_stopped() {
                return;
            }
            let Ok(rule) = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.freedesktop.GeoClue2")
            else {
                return;
            };
            let Some(Ok(mut stream)) = cancellable(
                &stop,
                zbus::MessageStream::for_match_rule(rule.build(), &conn, Some(32)),
            )
            .await
            else {
                return;
            };
            while !stop.is_stopped() {
                let Some(Some(Ok(_))) = cancellable(&stop, stream.next()).await else {
                    break;
                };
                if let Some(Some(v)) = cancellable(&stop, query_in_use(&conn)).await {
                    in_use.store(v, Ordering::Release);
                }
            }
        });
    }
}

/// Stateful privacy monitor aggregating PipeWire active nodes, `/proc/<pid>/fd`, and GeoClue2.
///
/// Background PipeWire and GeoClue2 threads are only started while subscribed via
/// [`Self::set_subscribed`]. For stateless one-shot scanning without threads, call
/// [`sample_privacy_oneshot`].
pub struct PrivacyMonitor {
    subscribed: bool,
    #[cfg(feature = "pipewire")]
    pw_entries: Arc<Mutex<Vec<PrivacyAccessEntry>>>,
    #[cfg(feature = "pipewire")]
    pw_stop: Option<pipewire::channel::Sender<()>>,
    #[cfg(feature = "pipewire")]
    pw_join: Option<JoinHandle<()>>,
    #[cfg(feature = "dbus")]
    geoclue_in_use: Arc<AtomicBool>,
    #[cfg(feature = "dbus")]
    geoclue_stop: Option<geoclue_monitor::GeoClueStopSignal>,
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
            subscribed: false,
            #[cfg(feature = "pipewire")]
            pw_entries: Arc::new(Mutex::new(Vec::new())),
            #[cfg(feature = "pipewire")]
            pw_stop: None,
            #[cfg(feature = "pipewire")]
            pw_join: None,
            #[cfg(feature = "dbus")]
            geoclue_in_use: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "dbus")]
            geoclue_stop: None,
            #[cfg(feature = "dbus")]
            geoclue_join: None,
        }
    }

    /// Returns `true` while background PipeWire/GeoClue2 subscription monitoring is active.
    #[must_use]
    pub const fn is_subscribed(&self) -> bool {
        self.subscribed
    }

    /// Enables or disables background PipeWire and GeoClue2 monitoring.
    ///
    /// Background threads are spawned only when `active` transitions to `true` and stopped
    /// cleanly when `active` transitions to `false`.
    pub fn set_subscribed(&mut self, active: bool) {
        if self.subscribed == active {
            return;
        }
        if active {
            self.ensure_started();
        } else {
            self.stop_background();
        }
    }

    pub fn ensure_started(&mut self) {
        self.subscribed = true;
        #[cfg(feature = "pipewire")]
        if self.pw_join.is_none() {
            let shared = Arc::clone(&self.pw_entries);
            let (stop_tx, stop_rx) = pipewire::channel::channel::<()>();
            if let Ok(join) = std::thread::Builder::new()
                .name("waywi-privacy-pw".to_owned())
                .spawn(move || pw_monitor::run_pipewire_privacy_thread(shared, stop_rx))
            {
                self.pw_stop = Some(stop_tx);
                self.pw_join = Some(join);
            }
        }
        #[cfg(feature = "dbus")]
        if self.geoclue_join.is_none() {
            let active_flag = Arc::clone(&self.geoclue_in_use);
            let stop_flag = geoclue_monitor::GeoClueStopSignal::new();
            let stop_cl = stop_flag.clone();
            if let Ok(join) = std::thread::Builder::new()
                .name("waywi-privacy-geo".to_owned())
                .spawn(move || geoclue_monitor::run_geoclue_privacy_thread(active_flag, stop_cl))
            {
                self.geoclue_stop = Some(stop_flag);
                self.geoclue_join = Some(join);
            }
        }
    }

    pub fn stop_background(&mut self) {
        self.subscribed = false;
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
            if let Some(s) = self.geoclue_stop.take() {
                s.trigger();
            }
            if let Some(j) = self.geoclue_join.take() {
                let _ = j.join();
            }
            self.geoclue_in_use.store(false, Ordering::Release);
        }
    }

    /// Collects the latest aggregated [`PrivacySnapshot`].
    ///
    /// Includes PipeWire and GeoClue2 state when subscribed via [`Self::set_subscribed`], or
    /// performs a stateless `/proc/<pid>/fd` scan without spawning threads when unsubscribed.
    #[must_use]
    pub fn sample(&self) -> PrivacySnapshot {
        if !self.subscribed {
            return sample_privacy_oneshot();
        }
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

        build_snapshot(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_pipewire_mic_monitor_camera_and_screencast_accurately() {
        // 1. Real microphone capture.
        let mic = classify_pipewire_node(PipeWireNodeProps {
            media_class: "Stream/Input/Audio",
            media_role: Some("Communication"),
            node_name: Some("obs-mic"),
            app_name: Some("OBS Studio"),
            pid: Some(1234),
            is_running: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(mic.kind, PrivacyAccessKind::Microphone);
        assert_eq!(mic.app_name, "OBS Studio");

        // 2. Output monitor capture (like waywi-capture) with stream.capture.sink / media.role=Monitor
        //    must classify as AudioCapture even when node_name lacks "monitor"/"loopback".
        let waywi_mon = classify_pipewire_node(PipeWireNodeProps {
            media_class: "Stream/Input/Audio",
            media_role: Some("Monitor"),
            node_name: Some("waywi-capture"),
            app_name: Some("waywi"),
            pid: Some(2000),
            is_running: true,
            stream_capture_sink: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(waywi_mon.kind, PrivacyAccessKind::AudioCapture);

        // 3. Authoritative PipeWire link graph overrides misleading node names in both directions.
        let linked_monitor = classify_pipewire_node(PipeWireNodeProps {
            media_class: "Stream/Input/Audio",
            node_name: Some("obs-audio"),
            app_name: Some("OBS"),
            is_running: true,
            linked_from_output_monitor: Some(true),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(linked_monitor.kind, PrivacyAccessKind::AudioCapture);

        let linked_mic = classify_pipewire_node(PipeWireNodeProps {
            media_class: "Stream/Input/Audio",
            node_name: Some("studio-monitor-mic"),
            app_name: Some("DAW"),
            is_running: true,
            linked_from_output_monitor: Some(false),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(linked_mic.kind, PrivacyAccessKind::Microphone);

        // 4. Screencast stream vs physical camera whose name happens to contain "screen".
        let screen = classify_pipewire_node(PipeWireNodeProps {
            media_class: "Stream/Input/Video",
            media_role: Some("Screen"),
            node_name: Some("xdg-desktop-portal-wlr"),
            media_name: Some("screencast"),
            app_name: Some("Chromium"),
            pid: Some(4567),
            is_running: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(screen.kind, PrivacyAccessKind::ScreenShare);

        let cam = classify_pipewire_node(PipeWireNodeProps {
            media_class: "Video/Source",
            media_role: Some("Camera"),
            node_name: Some("v4l2_input.pci-screencam"),
            device_api: Some("v4l2"),
            app_name: Some("Firefox"),
            pid: Some(999),
            is_running: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cam.kind, PrivacyAccessKind::Camera);

        assert!(
            classify_pipewire_node(PipeWireNodeProps {
                media_class: "Stream/Input/Audio",
                app_name: Some("IdleApp"),
                pid: Some(1),
                is_running: false,
                ..Default::default()
            })
            .is_none()
        );
    }

    #[test]
    fn validates_alsa_and_v4l2_device_paths_strictly() {
        assert!(is_alsa_capture_dev("/dev/snd/pcmC0D0c"));
        assert!(is_alsa_capture_dev("/dev/snd/pcmC12D3c (deleted)"));
        assert!(!is_alsa_capture_dev("/dev/snd/pcmC0D0p"));
        assert!(!is_alsa_capture_dev("/dev/snd/pcmC0D0p (deleted)"));
        assert!(!is_alsa_capture_dev("/dev/snd/pcm_custom"));
        assert!(is_v4l2_video_dev("/dev/video0"));
        assert!(is_v4l2_video_dev("/dev/video12 (deleted)"));
        assert!(!is_v4l2_video_dev("/dev/video_loopback"));
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

        let snap = sample_privacy_oneshot_from(&root, 1);
        assert!(snap.any_active());
        assert!(snap.camera_in_use);
        assert!(snap.microphone_in_use);
        assert!(!snap.screen_share_in_use);
        assert!(!snap.location_in_use);
        assert_eq!(snap.active.len(), 2);
        assert!(
            snap.active
                .iter()
                .any(|e| e.kind == PrivacyAccessKind::Camera && e.app_name == "cheese")
        );
        assert!(
            snap.active
                .iter()
                .any(|e| e.kind == PrivacyAccessKind::Microphone && e.app_name == "cheese")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unsubscribed_privacy_monitor_stays_stateless() {
        let monitor = PrivacyMonitor::new();
        assert!(!monitor.is_subscribed());
        let _snap = monitor.sample();
        assert!(!monitor.is_subscribed());
    }
}
