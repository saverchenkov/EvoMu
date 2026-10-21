//! `pos_manager` — Player-Owned Starbase (POS) and Control Tower Manager (`posMgr`).
//!
//! Handles control tower operations, starbase permissions, sentry/notification settings,
//! and orbital anchoring / structure control.

use crate::router::context::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct TowerState {
    pub tower_id: i64,
    pub solar_system_id: i64,
    pub moon_id: Option<i64>,
    pub password: String,
    pub permissions: HashMap<String, bool>,
    pub notifications: HashMap<String, bool>,
    pub sentry_settings: HashMap<String, bool>,
    pub linked_resources: Vec<i64>,
    pub provision_type: i64,
    pub controlled_by: Option<i64>,
}

impl Default for TowerState {
    fn default() -> Self {
        let mut permissions = HashMap::new();
        permissions.insert("allowCorp".into(), true);
        permissions.insert("allowAlliance".into(), true);
        permissions.insert("allowStandings".into(), false);

        let mut notifications = HashMap::new();
        notifications.insert("sendFuelNotifications".into(), true);

        let mut sentry_settings = HashMap::new();
        sentry_settings.insert("attackIfLowStanding".into(), true);
        sentry_settings.insert("attackIfWarTarget".into(), true);

        Self {
            tower_id: 100_000_001,
            solar_system_id: 30_000_142,
            moon_id: Some(40_000_001),
            password: "pos-password".into(),
            permissions,
            notifications,
            sentry_settings,
            linked_resources: Vec::new(),
            provision_type: 1,
            controlled_by: None,
        }
    }
}

static POS_TOWER_STATES: Mutex<Option<HashMap<i64, TowerState>>> = Mutex::new(None);

fn with_tower_state<F, R>(tower_id: i64, f: F) -> R
where
    F: FnOnce(&mut TowerState) -> R,
{
    let mut lock = POS_TOWER_STATES.lock().unwrap();
    let map = lock.get_or_insert_with(HashMap::new);
    let state = map.entry(tower_id).or_default();
    state.tower_id = tower_id;
    f(state)
}

/// Dispatches calls to `posMgr`.
pub fn handle_pos_manager(
    _ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "posMgr" {
        return None;
    }

    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    match method {
        "GetControlTowers" => {
            Some(Ok(PyRep::List(Vec::new())))
        }

        "GetMoonForTower" => {
            Some(Ok(PyRep::None))
        }

        "GetMoonProcessInfoForTower" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let res = with_tower_state(tower_id, |st| {
                PyRep::keyval([
                    ("towerID", PyRep::Integer(st.tower_id)),
                    ("cycleTime", PyRep::Integer(3600)),
                    ("active", PyRep::Bool(true)),
                ])
            });
            Some(Ok(res))
        }

        "GetConsumptionFactor" => {
            Some(Ok(PyRep::Real(1.0)))
        }

        "GetSiloCapacityByItemID" => {
            Some(Ok(PyRep::Real(20_000.0)))
        }

        "GetStarbasePermissions" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let res = with_tower_state(tower_id, |st| {
                PyRep::keyval([
                    ("allowCorp", PyRep::Bool(*st.permissions.get("allowCorp").unwrap_or(&true))),
                    ("allowAlliance", PyRep::Bool(*st.permissions.get("allowAlliance").unwrap_or(&true))),
                    ("allowStandings", PyRep::Bool(*st.permissions.get("allowStandings").unwrap_or(&false))),
                ])
            });
            Some(Ok(res))
        }

        "SetStarbasePermissions" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            with_tower_state(tower_id, |st| {
                if let Some(PyRep::KeyVal(kv)) = args.get(1) {
                    for (k, v) in kv {
                        if let Some(b) = v.as_bool() {
                            st.permissions.insert(k.clone(), b);
                        }
                    }
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "GetTowerNotificationSettings" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let res = with_tower_state(tower_id, |st| {
                PyRep::keyval([
                    ("sendFuelNotifications", PyRep::Bool(*st.notifications.get("sendFuelNotifications").unwrap_or(&true))),
                ])
            });
            Some(Ok(res))
        }

        "SetTowerNotifications" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            with_tower_state(tower_id, |st| {
                if let Some(PyRep::KeyVal(kv)) = args.get(1) {
                    for (k, v) in kv {
                        if let Some(b) = v.as_bool() {
                            st.notifications.insert(k.clone(), b);
                        }
                    }
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "GetTowerSentrySettings" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let res = with_tower_state(tower_id, |st| {
                PyRep::keyval([
                    ("attackIfLowStanding", PyRep::Bool(*st.sentry_settings.get("attackIfLowStanding").unwrap_or(&true))),
                    ("attackIfWarTarget", PyRep::Bool(*st.sentry_settings.get("attackIfWarTarget").unwrap_or(&true))),
                ])
            });
            Some(Ok(res))
        }

        "SetTowerSentrySettings" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            with_tower_state(tower_id, |st| {
                if let Some(PyRep::KeyVal(kv)) = args.get(1) {
                    for (k, v) in kv {
                        if let Some(b) = v.as_bool() {
                            st.sentry_settings.insert(k.clone(), b);
                        }
                    }
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "SetTowerPassword" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let pwd = args.get(1).and_then(|a| a.as_str()).unwrap_or("password");
            with_tower_state(tower_id, |st| {
                st.password = pwd.to_string();
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "LinkResourceForTower" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let resource_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            with_tower_state(tower_id, |st| {
                st.linked_resources.push(resource_id);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "RunMoonProcessCycleforTower" => {
            Some(Ok(PyRep::Bool(true)))
        }

        "ChangeStructureProvisionType" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let prov_type = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            with_tower_state(tower_id, |st| {
                st.provision_type = prov_type;
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "AnchorOrbital" => {
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_002);
            Some(Ok(PyRep::Integer(item_id)))
        }

        "UnanchorOrbital" => {
            Some(Ok(PyRep::Bool(true)))
        }

        "OnlineOrbital" => {
            Some(Ok(PyRep::Bool(true)))
        }

        "AssumeStructureControl" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            with_tower_state(tower_id, |st| {
                st.controlled_by = Some(char_id);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "RelinquishStructureControl" => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            with_tower_state(tower_id, |st| {
                st.controlled_by = None;
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "SetSessionCheck" => {
            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}
