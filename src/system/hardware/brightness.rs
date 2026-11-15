//! Laptop backlight sysfs control + non-root `logind` brightness updates + background
//! DDC/CI external monitor luminance worker (`/dev/i2c-*`, VCP `0x10`).

use std::ffi::c_void;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::error::{Result, WaywiError};
use crate::system::sysfs::read_sysfs_u64;

/// VESA MCCS VCP code for display Luminance / Brightness.
pub const VCP_LUMINANCE: u8 = 0x10;
const DDC_I2C_ADDR: usize = 0x37;
const I2C_SLAVE_IOCTL: rustix::ioctl::Opcode = 0x0703;
const DDC_SETTLE_WAIT: Duration = Duration::from_millis(45);

/// Internal panel backlight exposed via `/sys/class/backlight/*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklightDevice {
    pub name: String,
    pub path: PathBuf,
    pub brightness: u64,
    pub actual_brightness: u64,
    pub max_brightness: u64,
    pub device_type: Option<String>,
}

impl BacklightDevice {
    /// Current brightness as a normalized percentage (`0.0..=100.0`).
    #[must_use]
    pub fn percent(&self) -> f32 {
        if self.max_brightness == 0 {
            return 0.0;
        }
        ((self.actual_brightness as f64 / self.max_brightness as f64) * 100.0).clamp(0.0, 100.0)
            as f32
    }
}

/// External monitor DDC/CI luminance snapshot discovered over `/dev/i2c-*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdcMonitorInfo {
    pub i2c_bus: u32,
    pub device_path: PathBuf,
    pub current_luminance: u16,
    pub max_luminance: u16,
}

impl DdcMonitorInfo {
    /// Current monitor luminance as a normalized percentage (`0.0..=100.0`).
    #[must_use]
    pub fn percent(&self) -> f32 {
        if self.max_luminance == 0 {
            return 0.0;
        }
        ((f32::from(self.current_luminance) / f32::from(self.max_luminance)) * 100.0)
            .clamp(0.0, 100.0)
    }
}

/// Combined brightness telemetry across internal backlights and external DDC/CI monitors.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrightnessSnapshot {
    pub backlights: Vec<BacklightDevice>,
    pub external_monitors: Vec<DdcMonitorInfo>,
}

/// Enumerates internal panel backlights under `/sys/class/backlight`.
pub fn read_backlights() -> Result<Vec<BacklightDevice>> {
    read_backlights_from(Path::new("/sys/class/backlight"))
}

/// Enumerates backlights from an arbitrary sysfs directory (for deterministic testing).
pub fn read_backlights_from(base: &Path) -> Result<Vec<BacklightDevice>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let max_brightness = read_sysfs_u64(&dir.join("max_brightness")).unwrap_or(0);
        if max_brightness == 0 {
            continue;
        }
        let brightness = read_sysfs_u64(&dir.join("brightness")).unwrap_or(0);
        let actual_brightness =
            read_sysfs_u64(&dir.join("actual_brightness")).unwrap_or(brightness);
        let device_type = fs::read_to_string(dir.join("type"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());

        out.push(BacklightDevice {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: dir,
            brightness,
            actual_brightness,
            max_brightness,
            device_type,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Resolves a percentage (`0.0..=100.0`) against `device_name`'s `max_brightness`.
pub(crate) fn backlight_percent_to_raw(device_name: &str, percent: f32) -> Result<u64> {
    let devs = read_backlights()?;
    let target = devs
        .into_iter()
        .find(|d| d.name == device_name)
        .ok_or_else(|| {
            WaywiError::ProviderUnavailable(format!("backlight device `{device_name}` not found"))
        })?;
    let clamped = percent.clamp(0.0, 100.0);
    let raw = ((f64::from(clamped) / 100.0) * target.max_brightness as f64).round() as u64;
    Ok(raw.min(target.max_brightness))
}

/// Writes a raw backlight value directly to `/sys/class/backlight/<device>/brightness`.
pub(crate) fn write_backlight_sysfs_raw(device_name: &str, value: u64) -> Result<()> {
    let path = Path::new("/sys/class/backlight")
        .join(device_name)
        .join("brightness");
    fs::write(path, format!("{value}\n")).map_err(WaywiError::Io)
}

/// Sets laptop backlight brightness as a percentage (`0.0..=100.0`) via `logind` or sysfs.
pub fn set_backlight_percent(device_name: &str, percent: f32) -> Result<()> {
    let raw = backlight_percent_to_raw(device_name, percent)?;
    set_backlight_raw(device_name, raw)
}

/// Writes an explicit raw hardware value to backlight `device_name` via `logind` or sysfs.
pub fn set_backlight_raw(device_name: &str, value: u64) -> Result<()> {
    #[cfg(feature = "dbus")]
    if let Ok(val32) = u32::try_from(value) {
        let client = crate::dbus::DbusClient::new();
        if client
            .set_backlight_brightness("backlight", device_name, val32)
            .is_ok()
        {
            return Ok(());
        }
    }
    write_backlight_sysfs_raw(device_name, value)
}

/// Encodes a VESA DDC/CI "Get VCP Feature" command packet for 7-bit I2C slave `0x37`.
#[must_use]
pub fn encode_ddc_get_vcp(vcp_code: u8) -> [u8; 5] {
    let chk = 0x6Eu8 ^ 0x51 ^ 0x82 ^ 0x01 ^ vcp_code;
    [0x51, 0x82, 0x01, vcp_code, chk]
}

/// Encodes a VESA DDC/CI "Set VCP Feature" command packet for 7-bit I2C slave `0x37`.
#[must_use]
pub fn encode_ddc_set_vcp(vcp_code: u8, value: u16) -> [u8; 7] {
    let hi = (value >> 8) as u8;
    let lo = (value & 0xFF) as u8;
    let chk = 0x6Eu8 ^ 0x51 ^ 0x84 ^ 0x03 ^ vcp_code ^ hi ^ lo;
    [0x51, 0x84, 0x03, vcp_code, hi, lo, chk]
}

/// Decodes a VESA DDC/CI "VCP Feature Reply" packet, validating length, return code, VCP code,
/// and XOR checksum. Returns `(current, max)` on success.
#[must_use]
pub fn decode_ddc_vcp_reply(buf: &[u8], expected_vcp: u8) -> Option<(u16, u16)> {
    if buf.len() < 11 {
        return None;
    }
    let len = buf[1] & 0x7F;
    if buf[0] != 0x6E || len < 8 || buf[2] != 0x02 || buf[3] != 0x00 || buf[4] != expected_vcp {
        return None;
    }
    let mut chk = 0x50u8;
    for &b in &buf[..10] {
        chk ^= b;
    }
    if chk != buf[10] {
        return None;
    }
    let max_val = u16::from_be_bytes([buf[6], buf[7]]);
    let cur_val = u16::from_be_bytes([buf[8], buf[9]]);
    Some((cur_val, max_val))
}

struct I2cSlaveIoctl(usize);

// SAFETY: `I2C_SLAVE` (`0x0703`) expects the 7-bit I2C target address encoded as a scalar
// `unsigned long` argument (`without_provenance_mut(addr)`) and mutates no userspace memory.
unsafe impl rustix::ioctl::Ioctl for I2cSlaveIoctl {
    type Output = ();
    const IS_MUTATING: bool = false;

    fn opcode(&self) -> rustix::ioctl::Opcode {
        I2C_SLAVE_IOCTL
    }

    fn as_ptr(&mut self) -> *mut c_void {
        std::ptr::without_provenance_mut(self.0)
    }

    unsafe fn output_from_ptr(
        _out: rustix::ioctl::IoctlOutput,
        _extract_output: *mut c_void,
    ) -> rustix::io::Result<Self::Output> {
        Ok(())
    }
}

fn open_ddc_slave(dev_path: &Path) -> Result<std::fs::File> {
    let file = OpenOptions::new().read(true).write(true).open(dev_path)?;
    // SAFETY: `I2C_SLAVE` on an open `/dev/i2c-*` fd takes the 7-bit scalar address `0x37`
    // and reads/writes no caller pointers.
    unsafe { rustix::ioctl::ioctl(&file, I2cSlaveIoctl(DDC_I2C_ADDR)) }
        .map_err(|e| WaywiError::Io(std::io::Error::from_raw_os_error(e.raw_os_error())))?;
    Ok(file)
}

fn ddc_read_vcp(dev_path: &Path, vcp: u8) -> Result<(u16, u16)> {
    let mut file = open_ddc_slave(dev_path)?;
    let req = encode_ddc_get_vcp(vcp);
    file.write_all(&req)?;
    thread::sleep(DDC_SETTLE_WAIT);
    let mut reply = [0u8; 16];
    let n = file.read(&mut reply)?;
    decode_ddc_vcp_reply(&reply[..n], vcp).ok_or_else(|| {
        WaywiError::InvalidState(format!("invalid DDC/CI reply on {}", dev_path.display()))
    })
}

fn ddc_write_vcp(dev_path: &Path, vcp: u8, value: u16) -> Result<()> {
    let mut file = open_ddc_slave(dev_path)?;
    let pkt = encode_ddc_set_vcp(vcp, value);
    file.write_all(&pkt)?;
    thread::sleep(DDC_SETTLE_WAIT);
    Ok(())
}

fn scan_ddc_monitors() -> Vec<DdcMonitorInfo> {
    let Ok(rd) = fs::read_dir("/dev") else {
        return Vec::new();
    };
    let mut candidates: Vec<(u32, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let s = name.to_str()?;
            let bus_num = s.strip_prefix("i2c-")?.parse::<u32>().ok()?;
            Some((bus_num, e.path()))
        })
        .collect();
    candidates.sort_by_key(|(b, _)| *b);

    let mut monitors = Vec::new();
    for (bus, path) in candidates.into_iter().take(16) {
        if let Ok((cur, max)) = ddc_read_vcp(&path, VCP_LUMINANCE)
            && max > 0
        {
            monitors.push(DdcMonitorInfo {
                i2c_bus: bus,
                device_path: path,
                current_luminance: cur,
                max_luminance: max,
            });
        }
    }
    monitors
}

enum DdcCmd {
    Refresh,
    SetLuminance { i2c_bus: u32, percent: f32 },
    Shutdown,
}

/// Lazy background DDC/CI worker preventing slow `/dev/i2c-*` waits from blocking Wayland frames.
pub(crate) struct DdcWorker {
    latest: Arc<Mutex<Vec<DdcMonitorInfo>>>,
    refresh_pending: Arc<AtomicBool>,
    tx: Option<mpsc::Sender<DdcCmd>>,
    join: Option<JoinHandle<()>>,
}

impl Default for DdcWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DdcWorker {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(DdcCmd::Shutdown);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl DdcWorker {
    pub(crate) fn new() -> Self {
        Self {
            latest: Arc::new(Mutex::new(Vec::new())),
            refresh_pending: Arc::new(AtomicBool::new(false)),
            tx: None,
            join: None,
        }
    }

    fn ensure_started(&mut self) {
        if self.tx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel::<DdcCmd>();
        let shared = Arc::clone(&self.latest);
        let refresh_pending = Arc::clone(&self.refresh_pending);
        if let Ok(join) = thread::Builder::new()
            .name("waywi-ddc-ci".to_owned())
            .spawn(move || {
                let initial = scan_ddc_monitors();
                if let Ok(mut g) = shared.lock() {
                    *g = initial;
                }
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        DdcCmd::Refresh => {
                            refresh_pending.store(false, Ordering::Release);
                            let mons = scan_ddc_monitors();
                            if let Ok(mut g) = shared.lock() {
                                *g = mons;
                            }
                        }
                        DdcCmd::SetLuminance { i2c_bus, percent } => {
                            let path = PathBuf::from(format!("/dev/i2c-{i2c_bus}"));
                            let max_lum = {
                                let g = shared.lock().unwrap_or_else(|e| e.into_inner());
                                g.iter()
                                    .find(|m| m.i2c_bus == i2c_bus)
                                    .map_or(100, |m| m.max_luminance)
                            };
                            let target = ((f64::from(percent.clamp(0.0, 100.0)) / 100.0)
                                * f64::from(max_lum))
                            .round() as u16;
                            if ddc_write_vcp(&path, VCP_LUMINANCE, target.min(max_lum)).is_ok()
                                && let Ok(mut g) = shared.lock()
                                && let Some(m) = g.iter_mut().find(|m| m.i2c_bus == i2c_bus)
                            {
                                m.current_luminance = target.min(max_lum);
                            }
                        }
                        DdcCmd::Shutdown => break,
                    }
                }
            })
        {
            self.tx = Some(tx);
            self.join = Some(join);
        }
    }

    pub(crate) fn snapshot_and_request_refresh(&mut self) -> Vec<DdcMonitorInfo> {
        let just_started = self.tx.is_none();
        self.ensure_started();
        if !just_started
            && let Some(ref tx) = self.tx
            && !self.refresh_pending.swap(true, Ordering::AcqRel)
        {
            let _ = tx.send(DdcCmd::Refresh);
        }
        self.latest.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub(crate) fn set_luminance(&mut self, i2c_bus: u32, percent: f32) {
        self.ensure_started();
        if let Some(ref tx) = self.tx {
            let _ = tx.send(DdcCmd::SetLuminance { i2c_bus, percent });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_backlight_sysfs_and_computes_percent() {
        let base = std::env::temp_dir().join(format!("waywi-bl-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let bl = base.join("intel_backlight");
        fs::create_dir_all(&bl).unwrap();
        fs::write(bl.join("max_brightness"), "1000\n").unwrap();
        fs::write(bl.join("brightness"), "350\n").unwrap();
        fs::write(bl.join("actual_brightness"), "350\n").unwrap();
        fs::write(bl.join("type"), "raw\n").unwrap();

        let devs = read_backlights_from(&base).unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].name, "intel_backlight");
        assert_eq!(devs[0].device_type.as_deref(), Some("raw"));
        assert!((devs[0].percent() - 35.0).abs() < 0.01);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn encodes_and_decodes_ddc_vcp_packets_with_valid_checksums() {
        let get = encode_ddc_get_vcp(VCP_LUMINANCE);
        assert_eq!(get, [0x51, 0x82, 0x01, 0x10, 0xAC]);
        let set = encode_ddc_set_vcp(VCP_LUMINANCE, 75);
        assert_eq!(set[3], VCP_LUMINANCE);
        assert_eq!(u16::from_be_bytes([set[4], set[5]]), 75);

        // Construct valid VCP Feature Reply (cur=65, max=100)
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
            0,
        ];
        let mut chk = 0x50u8;
        for &b in &reply[..10] {
            chk ^= b;
        }
        reply[10] = chk;
        assert_eq!(decode_ddc_vcp_reply(&reply, VCP_LUMINANCE), Some((65, 100)));
        reply[10] ^= 1;
        assert_eq!(decode_ddc_vcp_reply(&reply, VCP_LUMINANCE), None);
    }
}
