#[path = "support/temp_dir.rs"]
mod temp_dir;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use waywi::capabilities::{Capabilities, Capability};
use waywi::desktop::{
    SpawnOptions, XdgIconLookup, filter_launcher_entries, parse_desktop_entry,
    scan_desktop_entries_in, search_desktop_entries, spawn_with,
};
use waywi::input::KeyboardLayoutInfo;
use waywi::system::cpu::CpuCounters;
use waywi::system::disk::{DiskSampler, parse_proc_diskstats, parse_proc_mounts};
use waywi::system::gpu::amd::parse_pp_dpm_active_mhz;
use waywi::system::gpu::intel::parse_drm_fdinfo_engines;
use waywi::system::gpu::nvidia::parse_nvidia_gpu_info;
use waywi::system::gpu::{GpuVendor, read_gpus_from};
use waywi::system::hardware::{
    VCP_LUMINANCE, decode_ddc_vcp_reply, encode_ddc_get_vcp, encode_ddc_set_vcp,
    read_backlights_from, read_sensors_from,
};
use waywi::system::memory::{ByteSize, parse_meminfo};
use waywi::system::network::{NetInterfaceKind, NetLinkState, NetworkSampler, parse_proc_net_dev};
use waywi::system::os::{parse_loadavg, parse_os_release, parse_uptime};
use waywi::system::privacy::{
    PrivacyAccessKind, PrivacyAccessSource, classify_pipewire_node, scan_proc_fd_from,
};
use waywi::system::{DataRate, SystemProviders};

#[test]
fn cpu_delta_math_handles_zero_and_saturated_cases() {
    let c0 = CpuCounters {
        user: 100,
        nice: 0,
        system: 50,
        idle: 850,
        iowait: 0,
        irq: 0,
        softirq: 0,
        steal: 0,
    };
    let c1 = CpuCounters {
        user: 200,
        nice: 0,
        system: 150,
        idle: 1650,
        iowait: 0,
        irq: 0,
        softirq: 0,
        steal: 0,
    };
    assert_eq!(c0.utilization_since(c0), 0.0);
    // delta total = 1000, delta idle = 800 -> active = 20%
    let pct = c1.utilization_since(c0);
    assert!((pct - 20.0).abs() < 0.01, "expected ~20%, got {pct}");
}

#[test]
fn meminfo_parser_computes_consistent_used_bytes() {
    let sample = "\
MemTotal:       16384000 kB
MemFree:         2000000 kB
MemAvailable:    8192000 kB
Buffers:          500000 kB
Cached:          4000000 kB
SwapTotal:       4194304 kB
SwapFree:        3145728 kB
";
    let snap = parse_meminfo(sample, None).expect("valid meminfo");
    assert_eq!(snap.total, ByteSize::from_kib(16_384_000));
    assert_eq!(snap.available, ByteSize::from_kib(8_192_000));
    assert_eq!(snap.used, ByteSize::from_kib(8_192_000));
    assert_eq!(snap.swap_total, ByteSize::from_kib(4_194_304));
    assert_eq!(snap.swap_used, ByteSize::from_kib(1_048_576));
}

#[test]
fn os_release_parser_extracts_clean_fields() {
    let txt = r#"
NAME="Arch Linux"
PRETTY_NAME="Arch Linux"
ID=arch
VERSION_ID="2026.11"
"#;
    let info = parse_os_release(txt);
    assert_eq!(info.name.as_deref(), Some("Arch Linux"));
    assert_eq!(info.id.as_deref(), Some("arch"));
    assert_eq!(info.version_id.as_deref(), Some("2026.11"));
}

#[test]
fn system_providers_smoke_contracts() {
    let mut sys = SystemProviders::new();

    // First call sets baseline; second call yields real delta on Linux `/proc/stat`.
    let _first = sys.sample_cpu();
    let second = sys.sample_cpu();
    if second.logical_cores > 0 {
        assert!((0.0..=100.0).contains(&second.total_percent));
    }

    let mem = sys.sample_memory();
    assert!(mem.total.as_bytes() > 0);
    assert!(mem.used <= mem.total);

    let t0 = sys.sample_time();
    let t1 = sys.sample_time();
    assert!(t1.monotonic >= t0.monotonic);
}

#[test]
fn capabilities_report_matches_compile_time_features() {
    let caps = Capabilities::from_compile_features();
    assert_eq!(
        caps.has(Capability::SoftwareRenderer),
        cfg!(feature = "renderer-software")
    );
    assert_eq!(
        caps.has(Capability::GpuRenderer),
        cfg!(feature = "renderer-gpu")
    );
    assert_eq!(caps.has(Capability::Text), cfg!(feature = "text"));
    assert_eq!(caps.has(Capability::ImagePng), cfg!(feature = "image-png"));
    assert_eq!(caps.has(Capability::ImageSvg), cfg!(feature = "image-svg"));
    assert_eq!(caps.has(Capability::ImageGif), cfg!(feature = "image-gif"));
    assert_eq!(
        caps.has(Capability::ImageWebp),
        cfg!(feature = "image-webp")
    );
    assert_eq!(caps.has(Capability::Animation), cfg!(feature = "animation"));
    assert_eq!(caps.has(Capability::Dbus), cfg!(feature = "dbus"));
    assert_eq!(caps.has(Capability::PipeWire), cfg!(feature = "pipewire"));
    assert_eq!(
        caps.has(Capability::PulseAudio),
        cfg!(feature = "pulseaudio")
    );
    assert_eq!(caps.has(Capability::Alsa), cfg!(feature = "alsa"));
    assert_eq!(
        caps.has(Capability::AudioPlayback),
        cfg!(any(
            feature = "audio-playback",
            feature = "pipewire",
            feature = "pulseaudio",
            feature = "alsa"
        ))
    );
    assert_eq!(
        caps.has(Capability::Accessibility),
        cfg!(feature = "accessibility")
    );
    assert_eq!(
        caps.has(Capability::ControlIpc),
        cfg!(feature = "control-ipc")
    );
    // Compositor IPC, portal global shortcuts & Wayland capabilities are session-probed at connection startup:
    assert!(!caps.has(Capability::Wayland));
    assert!(!caps.has(Capability::HyprlandIpc));
    assert!(!caps.has(Capability::SwayIpc));
    assert!(!caps.has(Capability::GlobalShortcuts));
    assert!(!caps.has(Capability::Clipboard));
    assert!(!caps.has(Capability::IdleNotify));
    assert!(!caps.has(Capability::KeyboardLayout));
}

#[cfg(feature = "dbus")]
#[test]
fn dbus_value_conversions_are_safe() {
    use waywi::dbus::{DbusValue, MprisPlaybackStatus, MprisSnapshot};
    assert_eq!(
        DbusValue::String("Playing".into()).as_str(),
        Some("Playing")
    );
    assert_eq!(
        DbusValue::ObjectPath("/org/example/Obj".into()).as_object_path(),
        Some("/org/example/Obj")
    );
    assert_eq!(
        DbusValue::ObjectPath("/org/example/Obj".into()).as_str(),
        Some("/org/example/Obj")
    );
    assert_eq!(DbusValue::Bool(true).as_bool(), Some(true));
    assert_eq!(DbusValue::I32(-7).as_i32(), Some(-7));
    assert_eq!(DbusValue::I32(-7).as_i64(), Some(-7));
    assert_eq!(DbusValue::I32(-7).as_u32(), None);
    assert_eq!(DbusValue::U32(42).as_u32(), Some(42));
    assert_eq!(DbusValue::U32(42).as_u64(), Some(42));
    assert_eq!(DbusValue::U64(42).as_i64(), Some(42));
    assert_eq!(DbusValue::I64(-1).as_u64(), None);
    let prop = DbusValue::Struct(vec![
        DbusValue::String("CPUWeight".into()),
        DbusValue::variant(DbusValue::U64(200)),
    ]);
    let fields = prop.as_struct().expect("struct fields");
    assert_eq!(fields[0].as_str(), Some("CPUWeight"));
    assert_eq!(fields[1].as_u64(), Some(200));
    assert_eq!(fields[1].as_variant(), Some(&DbusValue::U64(200)));

    let mpris = MprisSnapshot {
        service_name: "org.mpris.MediaPlayer2.playerctld".into(),
        status: MprisPlaybackStatus::Playing,
        title: Some("Track".into()),
        artist: Some("Artist".into()),
        album: None,
        ..Default::default()
    };
    assert_eq!(mpris.status, MprisPlaybackStatus::Playing);
}

#[test]
fn gpu_vendor_sysfs_and_parsers_work_deterministically() {
    let dir = temp_dir::TestTempDir::new("gpu");
    let drm = dir.path().join("drm");
    let nv_proc = dir.path().join("nvidia_gpus");

    // Connector directory (must be ignored by DRM card discovery)
    std::fs::create_dir_all(drm.join("card0-DP-1")).unwrap();

    // AMD card0 + hwmon
    let amd_dev = drm.join("card0/device");
    let amd_hwmon = amd_dev.join("hwmon/hwmon0");
    std::fs::create_dir_all(&amd_hwmon).unwrap();
    std::fs::write(amd_dev.join("vendor"), "0x1002\n").unwrap();
    std::fs::write(amd_dev.join("device"), "0x73df\n").unwrap();
    std::fs::write(amd_dev.join("gpu_busy_percent"), "42\n").unwrap();
    std::fs::write(amd_dev.join("mem_info_vram_used"), "2147483648\n").unwrap();
    std::fs::write(amd_dev.join("mem_info_vram_total"), "17179869184\n").unwrap();
    std::fs::write(amd_dev.join("pp_dpm_sclk"), "0: 500Mhz \n1: 2150Mhz *\n").unwrap();
    std::fs::write(amd_hwmon.join("name"), "amdgpu\n").unwrap();
    std::fs::write(amd_hwmon.join("temp1_label"), "edge\n").unwrap();
    std::fs::write(amd_hwmon.join("temp1_input"), "51000\n").unwrap();
    std::fs::write(amd_hwmon.join("temp2_label"), "junction\n").unwrap();
    std::fs::write(amd_hwmon.join("temp2_input"), "63000\n").unwrap();
    std::fs::write(amd_hwmon.join("power1_average"), "45500000\n").unwrap();
    std::fs::write(amd_hwmon.join("fan1_input"), "1450\n").unwrap();
    std::fs::write(amd_hwmon.join("freq1_label"), "sclk\n").unwrap();
    std::fs::write(amd_hwmon.join("freq1_input"), "2150000000\n").unwrap();

    // Intel card1 (i915)
    let intel_card = drm.join("card1");
    std::fs::create_dir_all(intel_card.join("device")).unwrap();
    std::fs::write(intel_card.join("device/vendor"), "0x8086\n").unwrap();
    std::fs::write(intel_card.join("gt_cur_freq_mhz"), "1150\n").unwrap();

    // NVIDIA card2 + /proc/driver/nvidia model metadata
    let nv_dev = drm.join("card2/device");
    std::fs::create_dir_all(&nv_dev).unwrap();
    std::fs::write(nv_dev.join("vendor"), "0x10de\n").unwrap();
    std::fs::create_dir_all(nv_proc.join("0000:01:00.0")).unwrap();
    std::fs::write(
        nv_proc.join("0000:01:00.0/information"),
        "Model:\t\t NVIDIA GeForce RTX 4070\nIRQ:\t\t 16\n",
    )
    .unwrap();

    let snap = read_gpus_from(&drm, &nv_proc, Instant::now());
    assert_eq!(snap.gpus.len(), 3);

    let amd = &snap.gpus[0];
    assert_eq!(amd.card_name, "card0");
    assert_eq!(amd.vendor, GpuVendor::Amd);
    assert_eq!(amd.pci_vendor_id, Some(0x1002));
    assert_eq!(amd.utilization_percent, Some(42.0));
    assert_eq!(amd.vram_used, Some(ByteSize::from_bytes(2_147_483_648)));
    assert_eq!(amd.vram_total, Some(ByteSize::from_bytes(17_179_869_184)));
    assert_eq!(amd.core_clock_mhz, Some(2150));
    assert_eq!(amd.temperature_celsius, Some(51.0));
    assert_eq!(amd.hotspot_celsius, Some(63.0));
    assert_eq!(amd.power_watts, Some(45.5));
    assert_eq!(amd.fan_rpm, Some(1450));

    let intel = &snap.gpus[1];
    assert_eq!(intel.vendor, GpuVendor::Intel);
    assert_eq!(intel.core_clock_mhz, Some(1150));
    assert_eq!(intel.utilization_percent, None);

    let nv = &snap.gpus[2];
    assert_eq!(nv.vendor, GpuVendor::Nvidia);
    assert_eq!(nv.model_name.as_deref(), Some("NVIDIA GeForce RTX 4070"));
    assert_eq!(nv.utilization_percent, None);

    assert_eq!(
        parse_pp_dpm_active_mhz("0: 500Mhz *\n1: 2725Mhz \n"),
        Some(500)
    );
    let engines = parse_drm_fdinfo_engines(
        "drm-driver:\ti915\ndrm-engine-render:\t1234567 ns\ndrm-engine-copy:\t890 ns\n",
    );
    assert_eq!(engines.get("render"), Some(&1_234_567));
    assert_eq!(engines.get("copy"), Some(&890));
    assert_eq!(
        parse_nvidia_gpu_info("Model: \t NVIDIA RTX A2000\n"),
        Some("NVIDIA RTX A2000".to_owned())
    );
}

#[test]
fn disk_mounts_diskstats_and_sampler_compute_clean_deltas() {
    let mounts_txt = "\
/dev/nvme0n1p4 / btrfs rw,noatime 0 0
/dev/nvme0n1p4 /\\040My\\040Drive ext4 rw 0 0
proc /proc proc rw 0 0
tmpfs /run tmpfs rw 0 0
192.168.1.5:/share /mnt/nas nfs4 rw 0 0
";
    let entries = parse_proc_mounts(mounts_txt);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].mount_point, PathBuf::from("/"));
    assert_eq!(entries[1].mount_point, PathBuf::from("/ My Drive"));

    let s0 = "\
   7       0 loop0 0 0 0 0 0 0 0 0 0 0 0
 259       0 nvme0n1 100 0 2000 10 50 0 1000 5 0 0 0
 259       1 nvme0n1p1 100 0 2000 10 50 0 1000 5 0 0 0
";
    let s1 = "\
   7       0 loop0 0 0 0 0 0 0 0 0 0 0 0
 259       0 nvme0n1 300 0 6000 20 150 0 5000 15 0 0 0
 259       1 nvme0n1p1 300 0 6000 20 150 0 5000 15 0 0 0
";
    let parsed = parse_proc_diskstats(s0);
    assert_eq!(parsed.len(), 2);
    assert!(parsed[0].is_whole_disk);
    assert!(!parsed[1].is_whole_disk);

    let mut sampler = DiskSampler::new();
    let t0 = Instant::now();
    let snap0 = sampler.feed_snapshot(Vec::new(), s0, t0);
    assert_eq!(snap0.total_read_rate, DataRate::ZERO);

    // After 2s: +4000 read sectors (2,048,000 B -> 1,024,000 B/s), +4000 write sectors,
    // without double-counting partition `nvme0n1p1`.
    let snap1 = sampler.feed_snapshot(Vec::new(), s1, t0 + Duration::from_secs(2));
    assert!((snap1.total_read_rate.as_bytes_per_sec() - 1_024_000.0).abs() < 1.0);
    assert!((snap1.total_write_rate.as_bytes_per_sec() - 1_024_000.0).abs() < 1.0);
    assert!((snap1.io[0].read_iops - 100.0).abs() < 0.01);
    assert!((snap1.io[0].write_iops - 50.0).abs() < 0.01);
}

#[test]
fn network_sampler_excludes_loopback_and_reads_sysfs_links() {
    let dir = temp_dir::TestTempDir::new("net");
    let base = dir.path();
    std::fs::create_dir_all(base.join("wlan0/wireless")).unwrap();
    std::fs::write(base.join("wlan0/operstate"), "up\n").unwrap();
    std::fs::write(base.join("wlan0/carrier"), "1\n").unwrap();
    std::fs::create_dir_all(base.join("enp9s0")).unwrap();
    std::fs::write(base.join("enp9s0/operstate"), "down\n").unwrap();
    std::fs::write(base.join("enp9s0/speed"), "-1\n").unwrap();

    let n0 = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1000 10 0 0 0 0 0 0 1000 10 0 0 0 0 0 0
enp9s0: 2000 20 0 0 0 0 0 0 500 5 0 0 0 0 0 0
 wlan0: 10000 100 0 0 0 0 0 0 4000 40 0 0 0 0 0 0
";
    let n1 = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 99999 999 0 0 0 0 0 0 99999 999 0 0 0 0 0 0
enp9s0: 2000 20 0 0 0 0 0 0 500 5 0 0 0 0 0 0
 wlan0: 14000 120 0 0 0 0 0 0 6000 50 0 0 0 0 0 0
";
    assert_eq!(parse_proc_net_dev(n0).len(), 3);

    let mut sampler = NetworkSampler::new();
    let t0 = Instant::now();
    let _ = sampler.feed_proc_net_dev(n0, base, t0);
    let snap = sampler.feed_proc_net_dev(n1, base, t0 + Duration::from_secs(2));

    // Loopback excluded from total rates -> total reflects wlan0 (+4000 rx / 2s = 2000, +2000 tx / 2s = 1000)
    assert!((snap.total_rx_bytes_per_sec - 2000.0).abs() < 0.01);
    assert!((snap.total_tx_bytes_per_sec - 1000.0).abs() < 0.01);

    let wlan = snap.interfaces.iter().find(|i| i.name == "wlan0").unwrap();
    assert_eq!(wlan.kind, NetInterfaceKind::Wifi);
    assert_eq!(wlan.link_state, NetLinkState::Up);
    assert_eq!(wlan.carrier, Some(true));

    let eth = snap.interfaces.iter().find(|i| i.name == "enp9s0").unwrap();
    assert_eq!(eth.kind, NetInterfaceKind::Ethernet);
    assert_eq!(eth.link_state, NetLinkState::Down);
    assert_eq!(eth.link_speed_mbps, None);
}

#[test]
fn hwmon_sensors_uptime_and_live_system_providers_contracts() {
    let dir = temp_dir::TestTempDir::new("hwmon");
    let base = dir.path();
    let hw0 = base.join("hwmon0");
    std::fs::create_dir_all(&hw0).unwrap();
    std::fs::write(hw0.join("name"), "k10temp\n").unwrap();
    std::fs::write(hw0.join("temp1_label"), "Tctl\n").unwrap();
    std::fs::write(hw0.join("temp1_input"), "54250\n").unwrap();
    std::fs::write(hw0.join("temp1_crit"), "95000\n").unwrap();

    let sensors = read_sensors_from(base).unwrap();
    assert_eq!(sensors.len(), 1);
    assert_eq!(sensors[0].hwmon_name, "k10temp");
    assert_eq!(sensors[0].label, "Tctl");
    assert!((sensors[0].celsius - 54.25).abs() < 0.01);
    assert_eq!(sensors[0].critical_celsius, Some(95.0));

    let (up, idle) = parse_uptime("86828.50 991136.00\n").unwrap();
    assert!((up.as_secs_f64() - 86828.50).abs() < 0.01);
    assert!((idle.as_secs_f64() - 991136.00).abs() < 0.01);

    let load = parse_loadavg("1.11 0.83 1.09 5/1210 2582113\n").unwrap();
    assert!((load.one - 1.11).abs() < 0.001);
    assert_eq!(load.running_tasks, 5);
    assert_eq!(load.total_tasks, 1210);

    let mut sys = SystemProviders::new();
    let up_live = sys.uptime().expect("/proc/uptime readable");
    assert!(up_live.uptime > Duration::ZERO);
    let _gpus_live = sys.sample_gpu();
    let _disk_live = sys.sample_disk();
    let net_live = sys.sample_network();
    assert!(!net_live.interfaces.is_empty());
}

#[test]
fn backlight_sysfs_and_ddc_packet_codec_contracts() {
    let dir = temp_dir::TestTempDir::new("backlight");
    let dev = dir.path().join("amdgpu_bl1");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(dev.join("max_brightness"), "255\n").unwrap();
    std::fs::write(dev.join("actual_brightness"), "128\n").unwrap();
    std::fs::write(dev.join("brightness"), "128\n").unwrap();
    std::fs::write(dev.join("type"), "raw\n").unwrap();

    let backlights = read_backlights_from(dir.path()).unwrap();
    assert_eq!(backlights.len(), 1);
    assert_eq!(backlights[0].name, "amdgpu_bl1");
    assert_eq!(backlights[0].max_brightness, 255);
    assert!((backlights[0].percent() - (128.0 / 255.0) * 100.0).abs() < 0.1);
    assert_eq!(backlights[0].device_type.as_deref(), Some("raw"));

    let get_pkt = encode_ddc_get_vcp(VCP_LUMINANCE);
    assert_eq!(get_pkt[0], 0x51);
    assert_eq!(get_pkt[3], VCP_LUMINANCE);
    let set_pkt = encode_ddc_set_vcp(VCP_LUMINANCE, 75);
    assert_eq!(set_pkt[0], 0x51);
    assert_eq!(set_pkt[5], 75);

    let mut reply = [
        0x6E,
        0x88,
        0x02,
        0x00,
        VCP_LUMINANCE,
        0x00,
        0x00,
        100,
        0x00,
        65,
        0x00,
    ];
    let mut chk = 0x50u8;
    for &b in &reply[..10] {
        chk ^= b;
    }
    reply[10] = chk;
    assert_eq!(decode_ddc_vcp_reply(&reply, VCP_LUMINANCE), Some((65, 100)));
    reply[10] ^= 0x01;
    assert_eq!(decode_ddc_vcp_reply(&reply, VCP_LUMINANCE), None);
}

#[test]
fn privacy_classifiers_and_proc_fd_scanner_contracts() {
    let cam = classify_pipewire_node(
        "Video/Source",
        Some("Camera"),
        Some("v4l2_input.pci-0000"),
        Some("Integrated Camera"),
        Some("obs"),
        Some(4242),
        true,
    )
    .expect("running video source classified");
    assert_eq!(cam.kind, PrivacyAccessKind::Camera);
    assert_eq!(cam.source, PrivacyAccessSource::PipeWire);
    assert_eq!(cam.app_name, "obs");

    let share = classify_pipewire_node(
        "Stream/Input/Video",
        Some("Screen"),
        Some("xdg-desktop-portal-wlr"),
        Some("Screen Cast"),
        Some("chromium"),
        Some(1337),
        true,
    )
    .expect("screencast stream classified");
    assert_eq!(share.kind, PrivacyAccessKind::ScreenShare);

    assert!(
        classify_pipewire_node(
            "Video/Source",
            None,
            Some("cam"),
            None,
            Some("app"),
            None,
            false
        )
        .is_none()
    );

    let proc_dir = temp_dir::TestTempDir::new("proc");
    let pid_dir = proc_dir.path().join("99901");
    let fd_dir = pid_dir.join("fd");
    std::fs::create_dir_all(&fd_dir).unwrap();
    std::fs::write(pid_dir.join("comm"), "firefox\n").unwrap();
    std::os::unix::fs::symlink("/dev/video0", fd_dir.join("11")).unwrap();
    std::os::unix::fs::symlink("/dev/snd/pcmC0D0c", fd_dir.join("12")).unwrap();

    let entries = scan_proc_fd_from(proc_dir.path(), 1);
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .any(|e| e.kind == PrivacyAccessKind::Camera && e.app_name == "firefox")
    );
    assert!(
        entries
            .iter()
            .any(|e| e.kind == PrivacyAccessKind::Microphone
                && e.device_path.as_deref() == Some("/dev/snd/pcmC0D0c"))
    );
}

#[test]
fn desktop_entry_icon_lookup_keyboard_layout_and_sfx_preset_contracts() {
    let dir = temp_dir::TestTempDir::new("desktop");
    let apps_dir = dir.path().join("applications");
    let icons_dir = dir.path().join("icons");
    let svg_dir = icons_dir.join("CustomTheme/scalable/apps");
    std::fs::create_dir_all(&apps_dir).unwrap();
    std::fs::create_dir_all(&svg_dir).unwrap();
    std::fs::write(
        icons_dir.join("CustomTheme/index.theme"),
        "[Icon Theme]\nName=CustomTheme\nDirectories=scalable/apps\n\n[scalable/apps]\nSize=48\nType=Scalable\nMinSize=16\nMaxSize=512\n",
    )
    .unwrap();
    let expected_icon = svg_dir.join("waywi-demo.webp");
    std::fs::write(&expected_icon, b"RIFF....WEBP").unwrap();

    let desktop_file = apps_dir.join("org.waywi.Demo.desktop");
    let work_dir = dir.path().join("work");
    std::fs::create_dir_all(&work_dir).unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let try_bin = bin_dir.join("waywi-demo-check");
    std::fs::write(&try_bin, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&try_bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&try_bin, perms).unwrap();
    }

    std::fs::write(
        &desktop_file,
        format!(
            "[Desktop Entry]\nType=Application\nName=WayWi Demo\nExec=/usr/bin/waywi-demo --flag %u %i\nTryExec={}\nPath={}\nIcon=waywi-demo\nKeywords=demo;launcher;\nStartupWMClass=WayWiDemo\n",
            try_bin.display(),
            work_dir.display()
        ),
    )
    .unwrap();
    let term_file = apps_dir.join("org.waywi.TermTool.desktop");
    std::fs::write(
        &term_file,
        "[Desktop Entry]\nType=Application\nName=Term Tool\nExec=htop -C\nTerminal=true\nTryExec=/definitely/missing/waywi-binary\n",
    )
    .unwrap();

    let scanned = scan_desktop_entries_in(std::slice::from_ref(&apps_dir));
    assert_eq!(scanned.len(), 2);
    assert_eq!(filter_launcher_entries(&scanned).len(), 1);
    let hits = search_desktop_entries(&scanned, "launcher");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry.id, "org.waywi.Demo.desktop");

    let parsed = parse_desktop_entry(
        &std::fs::read_to_string(&desktop_file).unwrap(),
        "org.waywi.Demo.desktop",
        &desktop_file,
    )
    .expect("valid desktop entry");
    assert!(parsed.try_exec_available());
    assert_eq!(parsed.working_dir.as_deref(), Some(work_dir.as_path()));
    assert_eq!(
        parsed.exec_argv().unwrap(),
        vec!["/usr/bin/waywi-demo", "--flag", "--icon", "waywi-demo"]
    );

    let term_parsed = parse_desktop_entry(
        &std::fs::read_to_string(&term_file).unwrap(),
        "org.waywi.TermTool.desktop",
        &term_file,
    )
    .expect("valid terminal desktop entry");
    assert!(!term_parsed.try_exec_available());
    assert_eq!(
        term_parsed.terminal_argv(Some("alacritty")).unwrap(),
        vec!["alacritty", "-e", "htop", "-C"]
    );
    assert!(
        spawn_with(
            "true",
            std::iter::empty::<&str>(),
            &SpawnOptions::new().working_dir(&work_dir),
        )
        .is_ok()
    );

    let mut lookup = XdgIconLookup::with_dirs(vec![icons_dir], "CustomTheme");
    assert_eq!(lookup.resolve("waywi-demo", 48), Some(expected_icon));

    let kbd =
        KeyboardLayoutInfo::from_layouts(vec!["English (US)".to_owned(), "Japanese".to_owned()], 0);
    assert_eq!(kbd.short_label(), "US");
}

#[cfg(any(
    feature = "pipewire",
    feature = "pulseaudio",
    feature = "alsa",
    feature = "audio-playback"
))]
#[test]
fn audio_player_presets_and_cache_contracts() {
    use waywi::audio::{AudioPlayer, SoundData, SoundPlay, SoundPreset};

    let click = SoundData::from_preset(SoundPreset::Click);
    assert_eq!(click.channels, 2);
    assert!(!click.samples.is_empty());
    let player = AudioPlayer::new();
    let a = player.load_preset(SoundPreset::DragTick);
    let b = player.load_preset(SoundPreset::DragTick);
    assert_eq!(a.id, b.id);
    let _play_asset: SoundPlay = a.into();
    let _play_scaled: SoundPlay = (a, 0.75).into();
    let _play_preset: SoundPlay = SoundPreset::Hover.into();
    assert!(player.unload(a));
    assert!(!player.unload(a));
    player.clear_cache();
}

#[cfg(any(feature = "pipewire", feature = "pulseaudio", feature = "alsa"))]
#[test]
fn audio_volume_and_orthogonal_target_contracts() {
    use waywi::audio::{
        AudioChannelPosition, AudioDirection, AudioPortTarget, AudioTarget, AudioVolume,
    };

    let vol = AudioVolume {
        linear: 0.8,
        channels: vec![0.8, 0.4],
        channel_map: vec![
            AudioChannelPosition::FrontLeft,
            AudioChannelPosition::FrontRight,
        ],
        db: Some(-1.9),
        muted: false,
    };
    assert_eq!(vol.channel_count(), 2);
    assert!((vol.percent() - 80.0).abs() < 0.01);
    assert!(vol.balance().is_some_and(|b| b < 0.0));
    assert_ne!(AudioTarget::DefaultSink, AudioTarget::DefaultSource);
    assert_ne!(AudioPortTarget::Sink(1), AudioPortTarget::Source(1));
    assert_ne!(AudioDirection::Output, AudioDirection::Input);
}

#[cfg(feature = "dbus")]
#[test]
fn dbus_client_subhandles_and_options_contracts() {
    use waywi::dbus::{
        BluetoothAgentReply, DbusActionReply, DbusClient, PowerAction, UDisksMountOptions,
        WifiConnectOptions,
    };

    let client = DbusClient::new();
    assert!(client.mpris().cached_snapshots().is_none());
    assert!(client.nm().cached_snapshot().is_none());
    assert!(client.bluetooth().cached_snapshot().is_none());
    assert!(client.udisks().cached_snapshot().is_none());
    assert!(client.logind().cached_snapshot().is_none());
    assert!(client.tray().cached_items().is_none());
    assert!(client.power_profiles().cached_snapshot().is_none());
    assert!(client.upower().cached_snapshot().is_none());
    assert!(client.modem().cached_snapshot().is_none());
    let _ = client.notifications().capabilities();
    let _ = client.portal();
    let _ = client.screensaver();
    let _ = client.geoclue();
    client.trim_cache();

    let wifi = WifiConnectOptions::new().password("secret").hidden(true);
    assert!(wifi.hidden);
    assert_eq!(wifi.password.as_deref(), Some("secret"));

    let mount = UDisksMountOptions::new().fs_type("ext4").interactive(false);
    assert!(!mount.interactive);
    assert_eq!(mount.fs_type.as_deref(), Some("ext4"));

    assert_eq!(DbusActionReply::default(), DbusActionReply::Unit);
    assert_ne!(PowerAction::Suspend, PowerAction::Reboot);
    let _ = BluetoothAgentReply::Reject;
}
