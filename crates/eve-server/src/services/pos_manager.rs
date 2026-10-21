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
    ctx: &ServerContext,
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

    match (service, method) {
        ("posMgr", "GetControlTowers") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("posMgr", "GetMoonForTower") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let res = with_tower_state(tower_id, |st| {
                st.moon_id.map(PyRep::Integer).unwrap_or(PyRep::None)
            });
            Some(Ok(res))
        }

        ("posMgr", "GetMoonProcessInfoForTower") => {
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

        ("posMgr", "GetConsumptionFactor") => {
            let _tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Real(1.0)))
        }

        ("posMgr", "GetSiloCapacityByItemID") => {
            let _item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Real(20_000.0)))
        }

        ("posMgr", "GetStarbasePermissions") => {
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

        ("posMgr", "SetStarbasePermissions") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
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

        ("posMgr", "GetTowerNotificationSettings") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let res = with_tower_state(tower_id, |st| {
                PyRep::keyval([
                    ("sendFuelNotifications", PyRep::Bool(*st.notifications.get("sendFuelNotifications").unwrap_or(&true))),
                ])
            });
            Some(Ok(res))
        }

        ("posMgr", "SetTowerNotifications") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
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

        ("posMgr", "GetTowerSentrySettings") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let res = with_tower_state(tower_id, |st| {
                PyRep::keyval([
                    ("attackIfLowStanding", PyRep::Bool(*st.sentry_settings.get("attackIfLowStanding").unwrap_or(&true))),
                    ("attackIfWarTarget", PyRep::Bool(*st.sentry_settings.get("attackIfWarTarget").unwrap_or(&true))),
                ])
            });
            Some(Ok(res))
        }

        ("posMgr", "SetTowerSentrySettings") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
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

        ("posMgr", "SetTowerPassword") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let pwd = args.get(1).and_then(|a| a.as_str()).unwrap_or("password");
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            with_tower_state(tower_id, |st| {
                st.password = pwd.to_string();
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("posMgr", "LinkResourceForTower") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let resource_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            with_tower_state(tower_id, |st| {
                st.linked_resources.push(resource_id);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("posMgr", "RunMoonProcessCycleforTower") => {
            let _tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(true)))
        }

        ("posMgr", "ChangeStructureProvisionType") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let prov_type = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            with_tower_state(tower_id, |st| {
                st.provision_type = prov_type;
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("posMgr", "AnchorStructure") => {
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Integer(item_id)))
        }

        ("posMgr", "AnchorOrbital") => {
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_002);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let is_gantry = ctx
                .db
                .with_snapshot(|s| s.items.get(&item_id).map(|i| i.type_id) == Some(3962));
            if is_gantry {
                Some(Ok(PyRep::Dict(vec![
                    (PyRep::String("orbitalID".into()), PyRep::Integer(item_id)),
                    (PyRep::String("typeID".into()), PyRep::Integer(3962)),
                ])))
            } else {
                Some(Ok(PyRep::Integer(item_id)))
            }
        }

        ("posMgr", "UnanchorOrbital") => {
            let _item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_002);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::Bool(true)))
        }

        ("posMgr", "OnlineOrbital") => {
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_002);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            let upgraded = ctx.db.with_mut_snapshot(|s| {
                if let Some(it) = s.items.get_mut(&item_id) {
                    if it.type_id == 3962 {
                        it.type_id = 2233;
                        return true;
                    }
                }
                false
            });
            if upgraded {
                Some(Ok(PyRep::Dict(vec![
                    (PyRep::String("orbitalID".into()), PyRep::Integer(item_id)),
                    (PyRep::String("typeID".into()), PyRep::Integer(2233)),
                ])))
            } else {
                Some(Ok(PyRep::Bool(true)))
            }
        }

        ("posMgr", "AssumeStructureControl") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            with_tower_state(tower_id, |st| {
                st.controlled_by = Some(char_id);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("posMgr", "RelinquishStructureControl") => {
            let tower_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_001);
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            with_tower_state(tower_id, |st| {
                st.controlled_by = None;
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("posMgr", "SetSessionCheck") => {
            let _ = ctx.db.get_character(session.character_id.unwrap_or(0));
            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}
