//! Physical hardware telemetry: battery/power supplies (`/sys/class/power_supply`),
//! panel backlights + DDC/CI monitor luminance, thermal/hwmon sensors (`/sys/class/hwmon`),
//! and USB bus devices (`/sys/bus/usb/devices`).

pub mod brightness;
pub mod power;
pub mod sensors;
pub mod usb;

pub use brightness::{
    BacklightDevice, BrightnessSnapshot, DdcMonitorInfo, VCP_LUMINANCE, decode_ddc_vcp_reply,
    encode_ddc_get_vcp, encode_ddc_set_vcp, read_backlights, read_backlights_from,
    set_backlight_percent, set_backlight_raw,
};
pub(crate) use brightness::{DdcWorker, backlight_percent_to_raw, write_backlight_sysfs_raw};
pub use power::{BatteryInfo, BatterySnapshot, PowerStatus, read_batteries, read_batteries_from};
pub use sensors::{ThermalSensor, ThermalSnapshot, read_sensors, read_sensors_from};
pub use usb::{UsbDevice, read_usb_devices, read_usb_devices_from};
