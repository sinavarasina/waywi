//! ModemManager cellular modem, SIM, signal quality, roaming, and 4G/5G technology inspection
//! (`org.freedesktop.ModemManager1`).

use zbus::zvariant::Value;

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{
    DBUS_SLOW_TIMEOUT, DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec,
    validate_object_path,
};
use super::types::{Dbus, DbusBus};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{
    ManagedObjects, dict_i32, dict_str, dict_u32, get_all_async, get_all_blocking,
    get_managed_objects_async, get_managed_objects_blocking,
};

const MM_DEST: &str = "org.freedesktop.ModemManager1";
const MM_PATH: &str = "/org/freedesktop/ModemManager1";
const MM_MODEM_IFACE: &str = "org.freedesktop.ModemManager1.Modem";
const MM_3GPP_IFACE: &str = "org.freedesktop.ModemManager1.Modem.Modem3gpp";
const MM_SIM_IFACE: &str = "org.freedesktop.ModemManager1.Sim";

/// Modem lifecycle state (`MMModemState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ModemState {
    Failed,
    #[default]
    Unknown,
    Initializing,
    Locked,
    Disabled,
    Disabling,
    Enabling,
    Enabled,
    Searching,
    Registered,
    Disconnecting,
    Connecting,
    Connected,
}

impl ModemState {
    /// Decodes `org.freedesktop.ModemManager1.Modem.State` signed integers.
    #[must_use]
    pub fn from_i32(val: i32) -> Self {
        match val {
            -1 => Self::Failed,
            1 => Self::Initializing,
            2 => Self::Locked,
            3 => Self::Disabled,
            4 => Self::Disabling,
            5 => Self::Enabling,
            6 => Self::Enabled,
            7 => Self::Searching,
            8 => Self::Registered,
            9 => Self::Disconnecting,
            10 => Self::Connecting,
            11 => Self::Connected,
            _ => Self::Unknown,
        }
    }
}

/// Cellular radio access technology (`MMModemAccessTechnology`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModemAccessTech {
    Pots,
    Gsm,
    GsmCompact,
    Gprs,
    Edge,
    Umts,
    Hsdpa,
    Hsupa,
    Hspa,
    HspaPlus,
    OneXrtt,
    Evdo0,
    EvdoA,
    EvdoB,
    Lte,
    FiveGNr,
}

/// 3GPP network registration state (`MMModem3gppRegistrationState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ModemRegistrationState {
    Idle,
    Home,
    Searching,
    Denied,
    #[default]
    Unknown,
    Roaming,
    HomeSmsOnly,
    RoamingSmsOnly,
    EmergencyOnly,
}

impl ModemRegistrationState {
    /// Decodes `Modem3gpp.RegistrationState` numeric enum.
    #[must_use]
    pub fn from_u32(val: u32) -> Self {
        match val {
            0 => Self::Idle,
            1 => Self::Home,
            2 => Self::Searching,
            3 => Self::Denied,
            5 => Self::Roaming,
            6 => Self::HomeSmsOnly,
            7 => Self::RoamingSmsOnly,
            8 => Self::EmergencyOnly,
            _ => Self::Unknown,
        }
    }
}

/// Cellular modem telemetry reported by `ModemManager1`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModemInfo {
    pub object_path: String,
    pub manufacturer: String,
    pub model: String,
    pub device: String,
    pub primary_port: String,
    pub state: ModemState,
    pub signal_quality_percent: Option<u32>,
    pub signal_recent: bool,
    pub access_technologies: Vec<ModemAccessTech>,
    pub operator_name: Option<String>,
    pub operator_code: Option<String>,
    pub registration_state: ModemRegistrationState,
    pub roaming: bool,
    pub sim_path: Option<String>,
    pub sim_operator_name: Option<String>,
}

/// Snapshot of detected cellular modems and SIM states.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModemSnapshot {
    pub modems: Vec<ModemInfo>,
}

impl Dbus {
    /// Subscribes to `ModemManager1` cellular modem state updates.
    pub fn modem<M: 'static>(map: impl Fn(ModemSnapshot) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusModem {
                key: "dbus:modem".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Reads the current [`ModemSnapshot`] from the subscription cache (0 ms) or
    /// `org.freedesktop.ModemManager1`.
    pub fn modem_snapshot(&self) -> Result<ModemSnapshot> {
        if let Ok(guard) = self.cache.modem.read()
            && let Some(ref snap) = *guard
        {
            return Ok(snap.clone());
        }
        let conn = self.connection(DbusBus::System)?;
        let objs = get_managed_objects_blocking(&conn, MM_DEST, MM_PATH)?;
        let mut snap = parse_modem_objects(&objs);
        for modem in &mut snap.modems {
            if let Some(ref sim_path) = modem.sim_path
                && let Ok(sim_props) = get_all_blocking(&conn, MM_DEST, sim_path, MM_SIM_IFACE)
            {
                modem.sim_operator_name =
                    dict_str(&sim_props, "OperatorName").filter(|s| !s.is_empty());
            }
        }
        Ok(snap)
    }

    /// Enables or disables a cellular modem via `org.freedesktop.ModemManager1.Modem.Enable`
    /// without blocking the UI thread.
    pub fn enable_modem(&self, modem_path: &str, enabled: bool) -> Result<()> {
        let path = validate_object_path(modem_path, "modem object path")?;
        let target = path.clone();
        self.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::NetworkManager,
                DbusBus::System,
                "Modem.Enable",
                target,
            )
            .with_dedup(format!("mm:enable:{path}"))
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                conn.call_method(
                    Some(MM_DEST),
                    path.as_str(),
                    Some(MM_MODEM_IFACE),
                    "Enable",
                    &(enabled,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("Modem.Enable failed: {e}")))?;
                Ok(())
            },
        )
    }
}

pub(crate) fn spawn_modem_worker(
    sender: calloop::channel::Sender<ModemSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::System,
        "waywi-dbus-modem",
        Some(MM_DEST),
        None,
        sender,
        shared,
        |conn| async move {
            let objs = get_managed_objects_async(&conn, MM_DEST, MM_PATH).await?;
            let mut snap = parse_modem_objects(&objs);
            for modem in &mut snap.modems {
                if let Some(ref sim_path) = modem.sim_path
                    && let Some(sim_props) =
                        get_all_async(&conn, MM_DEST, sim_path, MM_SIM_IFACE).await
                {
                    modem.sim_operator_name =
                        dict_str(&sim_props, "OperatorName").filter(|s| !s.is_empty());
                }
            }
            Some(snap)
        },
    )
}

/// Decodes ModemManager's `AccessTechnologies` bitmask cleanly via table lookup.
#[must_use]
pub fn decode_access_tech_bitmask(mask: u32) -> Vec<ModemAccessTech> {
    const TABLE: &[(u32, ModemAccessTech)] = &[
        (1 << 0, ModemAccessTech::Pots),
        (1 << 1, ModemAccessTech::Gsm),
        (1 << 2, ModemAccessTech::GsmCompact),
        (1 << 3, ModemAccessTech::Gprs),
        (1 << 4, ModemAccessTech::Edge),
        (1 << 5, ModemAccessTech::Umts),
        (1 << 6, ModemAccessTech::Hsdpa),
        (1 << 7, ModemAccessTech::Hsupa),
        (1 << 8, ModemAccessTech::Hspa),
        (1 << 9, ModemAccessTech::HspaPlus),
        (1 << 10, ModemAccessTech::OneXrtt),
        (1 << 11, ModemAccessTech::Evdo0),
        (1 << 12, ModemAccessTech::EvdoA),
        (1 << 13, ModemAccessTech::EvdoB),
        (1 << 14, ModemAccessTech::Lte),
        (1 << 15, ModemAccessTech::FiveGNr),
    ];
    TABLE
        .iter()
        .filter_map(|&(bit, tech)| (mask & bit != 0).then_some(tech))
        .collect()
}

pub(crate) fn parse_modem_objects(objects: &ManagedObjects) -> ModemSnapshot {
    let mut modems = Vec::new();
    for (path, ifaces) in objects {
        let Some(props) = ifaces.get(MM_MODEM_IFACE) else {
            continue;
        };
        let (signal_quality_percent, signal_recent) = props
            .get("SignalQuality")
            .and_then(|v| match &**v {
                Value::Structure(st) => {
                    let fields = st.fields();
                    let pct = fields.first().and_then(|f| match f {
                        Value::U32(u) => Some(*u),
                        _ => None,
                    });
                    let recent = matches!(fields.get(1), Some(Value::Bool(true)));
                    pct.map(|p| (Some(p.min(100)), recent))
                }
                _ => None,
            })
            .unwrap_or((None, false));

        let gpp = ifaces.get(MM_3GPP_IFACE);
        let registration_state = gpp
            .and_then(|g| dict_u32(g, "RegistrationState"))
            .map(ModemRegistrationState::from_u32)
            .unwrap_or_default();
        let sim_path = dict_str(props, "Sim").filter(|s| s != "/");

        modems.push(ModemInfo {
            object_path: path.to_string(),
            manufacturer: dict_str(props, "Manufacturer").unwrap_or_default(),
            model: dict_str(props, "Model").unwrap_or_default(),
            device: dict_str(props, "Device").unwrap_or_default(),
            primary_port: dict_str(props, "PrimaryPort").unwrap_or_default(),
            state: ModemState::from_i32(dict_i32(props, "State").unwrap_or(0)),
            signal_quality_percent,
            signal_recent,
            access_technologies: decode_access_tech_bitmask(
                dict_u32(props, "AccessTechnologies").unwrap_or(0),
            ),
            operator_name: gpp
                .and_then(|g| dict_str(g, "OperatorName"))
                .filter(|s| !s.is_empty()),
            operator_code: gpp
                .and_then(|g| dict_str(g, "OperatorCode"))
                .filter(|s| !s.is_empty()),
            registration_state,
            roaming: matches!(
                registration_state,
                ModemRegistrationState::Roaming | ModemRegistrationState::RoamingSmsOnly
            ),
            sim_path,
            sim_operator_name: None,
        });
    }
    modems.sort_by(|a, b| a.object_path.cmp(&b.object_path));
    ModemSnapshot { modems }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, StructureBuilder};

    #[test]
    fn decodes_modem_access_tech_and_3gpp_properties() {
        let techs = decode_access_tech_bitmask((1 << 14) | (1 << 15));
        assert_eq!(techs, vec![ModemAccessTech::Lte, ModemAccessTech::FiveGNr]);
        assert_eq!(ModemState::from_i32(11), ModemState::Connected);

        let mut objs: ManagedObjects = HashMap::new();
        let mut ifaces = HashMap::new();
        let mut props = HashMap::new();
        props.insert(
            "Model".into(),
            OwnedValue::try_from(Value::from("RM520N-GL")).unwrap(),
        );
        props.insert(
            "State".into(),
            OwnedValue::try_from(Value::from(11i32)).unwrap(),
        );
        props.insert(
            "AccessTechnologies".into(),
            OwnedValue::try_from(Value::from(1u32 << 15)).unwrap(),
        );
        let sig_val = Value::Structure(
            StructureBuilder::new()
                .add_field(84u32)
                .add_field(true)
                .build()
                .unwrap(),
        );
        props.insert(
            "SignalQuality".into(),
            OwnedValue::try_from(sig_val).unwrap(),
        );
        let mut gpp = HashMap::new();
        gpp.insert(
            "OperatorName".into(),
            OwnedValue::try_from(Value::from("Carrier")).unwrap(),
        );
        gpp.insert(
            "RegistrationState".into(),
            OwnedValue::try_from(Value::from(5u32)).unwrap(),
        );

        ifaces.insert(MM_MODEM_IFACE.to_owned(), props);
        ifaces.insert(MM_3GPP_IFACE.to_owned(), gpp);
        objs.insert(
            OwnedObjectPath::try_from("/org/freedesktop/ModemManager1/Modem/0").unwrap(),
            ifaces,
        );

        let snap = parse_modem_objects(&objs);
        assert_eq!(snap.modems.len(), 1);
        assert_eq!(snap.modems[0].signal_quality_percent, Some(84));
        assert!(snap.modems[0].signal_recent);
        assert!(snap.modems[0].roaming);
        assert_eq!(
            snap.modems[0].access_technologies,
            vec![ModemAccessTech::FiveGNr]
        );
    }
}
