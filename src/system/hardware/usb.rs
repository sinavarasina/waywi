//! Linux `/sys/bus/usb/devices` universal USB device inventory reader.

use std::path::Path;

use crate::error::Result;
use crate::system::sysfs::{read_sysfs_string, read_sysfs_u32};

/// Connected USB device descriptor read from `/sys/bus/usb/devices/<node>`.
#[derive(Debug, Clone, PartialEq)]
pub struct UsbDevice {
    /// Kernel sysfs topology identifier (e.g. `"1-9"`, `"2-1.4"`, `"usb1"`).
    pub sysfs_name: String,
    /// USB bus number (`busnum`).
    pub bus_num: u16,
    /// USB device address on the bus (`devnum`).
    pub dev_num: u16,
    /// 16-bit USB-IF Vendor ID (`idVendor`, e.g. `0x8087`).
    pub vendor_id: u16,
    /// 16-bit USB-IF Product ID (`idProduct`, e.g. `0x0032`).
    pub product_id: u16,
    /// Base USB device class code (`bDeviceClass`, e.g. `0x09` for Hub, `0x08` for Mass Storage).
    pub device_class: u8,
    /// Optional manufacturer descriptor string (`manufacturer`).
    pub manufacturer: Option<String>,
    /// Optional product descriptor string (`product`).
    pub product: Option<String>,
    /// Optional device serial string (`serial`).
    pub serial: Option<String>,
    /// Negotiated link speed in Mbps (`speed`, e.g. `1.5`, `12.0`, `480.0`, `5000.0`, `10000.0`).
    pub speed_mbps: Option<f32>,
    /// Maximum bus power draw in milliamperes (`bMaxPower`, e.g. `"500mA"` -> `500`).
    pub max_power_ma: Option<u32>,
}

impl UsbDevice {
    /// Returns the canonical `"vvvv:pppp"` lowercase hex identifier (e.g. `"8087:0032"`).
    #[must_use]
    pub fn usb_id(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor_id, self.product_id)
    }

    /// Returns `true` if this entry represents a USB controller root hub (`usb1`, `usb2`, ...).
    #[must_use]
    pub fn is_root_hub(&self) -> bool {
        self.sysfs_name.starts_with("usb")
    }

    /// Returns `true` if the base device class is a USB hub (`bDeviceClass == 0x09`).
    #[must_use]
    pub fn is_hub(&self) -> bool {
        self.device_class == 0x09
    }
}

/// Reads all connected USB devices from `/sys/bus/usb/devices`.
pub fn read_usb_devices() -> Result<Vec<UsbDevice>> {
    read_usb_devices_from(Path::new("/sys/bus/usb/devices"))
}

/// Reads USB devices from a custom sysfs directory (testable without live `/sys/bus/usb/devices`).
pub fn read_usb_devices_from(base: &Path) -> Result<Vec<UsbDevice>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        // Interface sub-nodes look like `1-9:1.0` and do not carry device-level `idVendor`.
        if name_str.contains(':') {
            continue;
        }
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(vendor_id) = read_hex_u16(&dir.join("idVendor")) else {
            continue;
        };
        let Some(product_id) = read_hex_u16(&dir.join("idProduct")) else {
            continue;
        };
        let bus_num = read_sysfs_u32(&dir.join("busnum"))
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(0);
        let dev_num = read_sysfs_u32(&dir.join("devnum"))
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(0);
        let device_class = read_hex_u8(&dir.join("bDeviceClass")).unwrap_or(0);
        let manufacturer = read_sysfs_string(&dir.join("manufacturer")).filter(|s| !s.is_empty());
        let product = read_sysfs_string(&dir.join("product")).filter(|s| !s.is_empty());
        let serial = read_sysfs_string(&dir.join("serial")).filter(|s| !s.is_empty());
        let speed_mbps = read_sysfs_string(&dir.join("speed"))
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0);
        let max_power_ma = read_sysfs_string(&dir.join("bMaxPower")).and_then(|s| {
            let digits = s.trim().trim_end_matches("mA").trim();
            digits.parse::<u32>().ok()
        });

        out.push(UsbDevice {
            sysfs_name: name_str.to_owned(),
            bus_num,
            dev_num,
            vendor_id,
            product_id,
            device_class,
            manufacturer,
            product,
            serial,
            speed_mbps,
            max_power_ma,
        });
    }
    out.sort_by(|a, b| {
        a.bus_num
            .cmp(&b.bus_num)
            .then(a.dev_num.cmp(&b.dev_num))
            .then(a.sysfs_name.cmp(&b.sysfs_name))
    });
    Ok(out)
}

fn read_hex_u16(path: &Path) -> Option<u16> {
    let raw = read_sysfs_string(path)?;
    let trimmed = raw.trim().trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(trimmed, 16).ok()
}

fn read_hex_u8(path: &Path) -> Option<u8> {
    let raw = read_sysfs_string(path)?;
    let trimmed = raw.trim().trim_start_matches("0x").trim_start_matches("0X");
    u8::from_str_radix(trimmed, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_usb_devices_from_sysfs_and_skips_interface_nodes() {
        let root = std::env::temp_dir().join(format!("waywi-usb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // Root hub usb1
        let hub = root.join("usb1");
        std::fs::create_dir_all(&hub).unwrap();
        std::fs::write(hub.join("busnum"), "1\n").unwrap();
        std::fs::write(hub.join("devnum"), "1\n").unwrap();
        std::fs::write(hub.join("idVendor"), "1d6b\n").unwrap();
        std::fs::write(hub.join("idProduct"), "0002\n").unwrap();
        std::fs::write(hub.join("bDeviceClass"), "09\n").unwrap();
        std::fs::write(hub.join("product"), "xHCI Host Controller\n").unwrap();
        std::fs::write(hub.join("speed"), "480\n").unwrap();
        std::fs::write(hub.join("bMaxPower"), "0mA\n").unwrap();

        // Attached USB 3.2 flash drive 2-1
        let drive = root.join("2-1");
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::write(drive.join("busnum"), "2\n").unwrap();
        std::fs::write(drive.join("devnum"), "4\n").unwrap();
        std::fs::write(drive.join("idVendor"), "0781\n").unwrap();
        std::fs::write(drive.join("idProduct"), "5583\n").unwrap();
        std::fs::write(drive.join("bDeviceClass"), "00\n").unwrap();
        std::fs::write(drive.join("manufacturer"), "ExampleCorp\n").unwrap();
        std::fs::write(drive.join("product"), "Ultra Fit\n").unwrap();
        std::fs::write(drive.join("serial"), "ABC12345\n").unwrap();
        std::fs::write(drive.join("speed"), "5000\n").unwrap();
        std::fs::write(drive.join("bMaxPower"), "896mA\n").unwrap();

        // Interface sub-node (must be ignored)
        let iface = root.join("2-1:1.0");
        std::fs::create_dir_all(&iface).unwrap();
        std::fs::write(iface.join("idVendor"), "0781\n").unwrap();
        std::fs::write(iface.join("idProduct"), "5583\n").unwrap();

        let devices = read_usb_devices_from(&root).unwrap();
        assert_eq!(devices.len(), 2);

        assert_eq!(devices[0].sysfs_name, "usb1");
        assert!(devices[0].is_root_hub());
        assert!(devices[0].is_hub());
        assert_eq!(devices[0].usb_id(), "1d6b:0002");

        assert_eq!(devices[1].sysfs_name, "2-1");
        assert!(!devices[1].is_root_hub());
        assert!(!devices[1].is_hub());
        assert_eq!(devices[1].usb_id(), "0781:5583");
        assert_eq!(devices[1].manufacturer.as_deref(), Some("ExampleCorp"));
        assert_eq!(devices[1].product.as_deref(), Some("Ultra Fit"));
        assert_eq!(devices[1].serial.as_deref(), Some("ABC12345"));
        assert_eq!(devices[1].speed_mbps, Some(5000.0));
        assert_eq!(devices[1].max_power_ma, Some(896));

        let _ = std::fs::remove_dir_all(&root);
    }
}
