#[cfg(any(
    feature = "hyprland-ipc",
    feature = "sway-ipc",
    feature = "control-ipc"
))]
#[path = "support/temp_dir.rs"]
mod temp_dir;

#[cfg(feature = "hyprland-ipc")]
#[test]
fn hyprland_ipc_parsing_and_mock_socket_roundtrip() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    use waywi::hyprland::{
        HyprlandClient, HyprlandEvent, HyprlandSockets, WorkspaceId, WorkspaceInfo,
        apply_workspace_event, parse_event_line,
    };

    assert_eq!(
        parse_event_line("workspacev2>>2,term"),
        Some(HyprlandEvent::WorkspaceV2 {
            id: WorkspaceId(2),
            name: "term".into(),
        })
    );
    assert_eq!(
        parse_event_line("activewindow>>Alacritty,vim"),
        Some(HyprlandEvent::ActiveWindow {
            class: "Alacritty".into(),
            title: "vim".into(),
        })
    );
    assert_eq!(parse_event_line("malformed-no-delimiter"), None);

    let mut list = vec![
        WorkspaceInfo {
            id: WorkspaceId(1),
            name: "1".into(),
            monitor: "eDP-1".into(),
            windows: 1,
            focused: true,
            ..WorkspaceInfo::default()
        },
        WorkspaceInfo {
            id: WorkspaceId(2),
            name: "2".into(),
            monitor: "eDP-1".into(),
            windows: 0,
            focused: false,
            ..WorkspaceInfo::default()
        },
    ];
    assert!(apply_workspace_event(
        &mut list,
        &HyprlandEvent::WorkspaceV2 {
            id: WorkspaceId(2),
            name: "2".into(),
        }
    ));
    assert!(!list[0].focused);
    assert!(list[1].focused);

    let dir = temp_dir::TestTempDir::new("hypr-ipc");
    let cmd_sock = dir.path().join(".socket.sock");
    let evt_sock = dir.path().join(".socket2.sock");
    let listener = UnixListener::bind(&cmd_sock).unwrap();

    let srv = std::thread::spawn(move || {
        for _ in 0..2 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = [0u8; 256];
                let n = stream.read(&mut req).unwrap_or(0);
                let req_str = std::str::from_utf8(&req[..n]).unwrap_or("");
                let payload = if req_str.contains("activeworkspace") {
                    r#"{"id":1,"name":"1","monitor":"eDP-1","windows":2}"#
                } else {
                    r#"[{"id":1,"name":"1","monitor":"eDP-1","windows":2}]"#
                };
                let _ = stream.write_all(payload.as_bytes());
            }
        }
    });

    let client = HyprlandClient::with_sockets(HyprlandSockets {
        command_socket: cmd_sock.clone(),
        event_socket: evt_sock,
    });
    let ws = client.workspaces().expect("mock hyprland workspaces");
    assert_eq!(ws.len(), 1);
    assert_eq!(ws[0].id, WorkspaceId(1));
    assert_eq!(ws[0].windows, 2);
    assert!(ws[0].focused);
    let _ = srv.join();
    let _ = std::fs::remove_file(&cmd_sock);

    // Verify HyprLua v0.57 hydration: the one-shot `j/systeminfo` probe reports a non-Lua
    // provider, then `dispatch workspace N` returns a Lua syntax hint and the client falls back.
    let listener2 = UnixListener::bind(&cmd_sock).unwrap();
    let srv2 = std::thread::spawn(move || {
        let legacy_error = "error: [string \"return hl.dispatch(workspace 3)\"]:1: ')' expected near '3'\n\n → Note: dispatch in lua is a shorthand for hl.dispatch(...), your syntax might need to be updated.";
        let mut seen = Vec::new();
        for _ in 0..3 {
            if let Ok((mut stream, _)) = listener2.accept() {
                let mut buf = [0u8; 256];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let reply = if req.starts_with("j/systeminfo") {
                    "System Information:\nconfigProvider: hypr\n"
                } else if req == "dispatch workspace 3" {
                    legacy_error
                } else {
                    "ok"
                };
                seen.push(req);
                let _ = stream.write_all(reply.as_bytes());
            }
        }
        seen
    });
    client
        .switch_workspace(WorkspaceId(3))
        .expect("HyprLua fallback switch_workspace");
    let cmds = srv2.join().unwrap();
    assert_eq!(
        cmds,
        vec![
            "j/systeminfo".to_owned(),
            "dispatch workspace 3".to_owned(),
            "dispatch hl.dsp.focus({ workspace = 3 })".to_owned(),
        ]
    );
}

#[cfg(feature = "sway-ipc")]
#[test]
fn sway_ipc_framing_and_mock_socket_roundtrip() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    use waywi::sway::{SwayClient, SwayEvent, encode_frame, parse_event_frame};

    let frame = encode_frame(1, br#"{"ok":true}"#);
    assert_eq!(&frame[..6], b"i3-ipc");

    let ev = parse_event_frame(
        0x8000_0000,
        br#"{"change":"focus","current":null,"old":null}"#,
    );
    assert!(matches!(ev, SwayEvent::Workspace { ref change, .. } if change == "focus"));

    let dir = temp_dir::TestTempDir::new("sway-ipc");
    let sock = dir.path().join("sway-ipc.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let srv = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut hdr = [0u8; 14];
            let _ = stream.read_exact(&mut hdr);
            let body = br#"[{"id":10,"num":1,"name":"1","visible":true,"focused":true,"urgent":false,"output":"DP-1"}]"#;
            let resp = encode_frame(1, body);
            let _ = stream.write_all(&resp);
        }
    });

    let client = SwayClient::with_socket(&sock);
    let ws = client.workspaces().expect("mock sway workspaces");
    assert_eq!(ws.len(), 1);
    assert!(ws[0].focused);
    assert_eq!(ws[0].output, "DP-1");
    let _ = srv.join();
}

#[cfg(feature = "control-ipc")]
#[test]
fn control_ipc_socket_roundtrip() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    use waywi::control::{resolve_socket_path, send_command};

    let explicit = resolve_socket_path(std::path::Path::new("/tmp/test-waywi-ctrl.sock"));
    assert_eq!(
        explicit,
        std::path::PathBuf::from("/tmp/test-waywi-ctrl.sock")
    );

    let dir = temp_dir::TestTempDir::new("ctrl-ipc");
    let sock = dir.path().join("ctrl.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let srv = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = String::new();
        let _ = s.read_to_string(&mut buf);
        buf
    });

    send_command(&sock, "toggle-osd").expect("send_command");
    let received = srv.join().unwrap();
    assert!(received.contains("toggle-osd"));
}
