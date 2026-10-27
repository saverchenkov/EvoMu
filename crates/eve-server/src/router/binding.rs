//! Stateful object binding and moniker handles (`MachoBindObject`).

use crate::router::context::{BoundObjectEntry, ServerContext, NEXT_BOUND_ID};
use crate::router::{NotificationTarget, PendingMachoNotification};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::sync::atomic::Ordering;
use std::sync::Arc;

const ERR_BIND_AUTH_DENIED: &str = "BindAuthorizationDenied";
const ERR_BOUND_OWNERSHIP: &str = "BoundObjectOwnershipViolation";

impl ServerContext {
    /// Registers a disconnect notification channel for an active TCP client session (`session_id`).
    pub fn register_session_closer(
        &self,
        session_id: u64,
        tx: tokio::sync::mpsc::UnboundedSender<(String, String)>,
    ) {
        self.session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id, tx);
    }

    /// Removes the disconnect notification channel for `session_id`.
    pub fn unregister_session_closer(&self, session_id: u64) {
        self.session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
    }

    /// Actively signals an open TCP session (`session_id`) to send `GPSTransportClosed` and close its socket.
    pub fn disconnect_session(&self, session_id: u64, reason_code: &str, reason: &str) -> bool {
        self.release_session_bindings(session_id);
        let tx = self
            .session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
        if let Some(sender) = tx {
            sender
                .send((reason_code.to_string(), reason.to_string()))
                .is_ok()
        } else {
            false
        }
    }

    /// Releases all bound objects associated with `session_id` (called on disconnect or re-login).
    pub fn release_session_bindings(&self, session_id: u64) {
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, v| v.session_id != session_id);
    }

    /// Releases all bound objects associated with `user_id` (called on ban, kick, or session displacement).
    pub fn release_user_bindings(&self, user_id: u64) {
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, v| v.user_id != user_id);
    }

    /// Binds a stateful remote object scoped to `(session_id, user_id, character_id)`.
    pub fn bind_object(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
    ) -> Result<u64, BridgeRpcError> {
        let user_id = Self::require_auth(session)?;
        let char_id = match session.character_id {
            Some(cid) => cid,
            None if service == "populationCap" => {
                // `populationCap` is bound on the character-selection screen (`TryEnterGame`)
                // before `SelectCharacterID` sets `session.character_id`; the bound character ID
                // must belong to the authenticated account.
                let owned = self
                    .db
                    .get_characters_for_user(user_id)
                    .iter()
                    .any(|c| c.character_id == object_id);
                if !owned {
                    return Err(BridgeRpcError {
                        code: ERR_BIND_AUTH_DENIED.into(),
                        message: format!(
                            "Service '{service}' cannot be bound to character {object_id} not owned by user {user_id}"
                        ),
                    });
                }
                object_id
            }
            None => {
                return Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "Cannot bind object before selecting a character".into(),
                });
            }
        };

        let authorized = match service {
            "populationCap" => object_id == char_id,
            "beyonce" | "scanMgr" => session.solar_system_id.map(|s| s as i64) == Some(object_id),
            "dogmaIM" | "ship" => {
                session.ship_id == Some(object_id)
                    || session.station_id == Some(object_id)
                    || session.structure_id == Some(object_id)
                    || session.solar_system_id.map(|s| s as i64) == Some(object_id)
            }
            "invbroker" => {
                session.station_id == Some(object_id)
                    || session.structure_id == Some(object_id)
                    || session.ship_id == Some(object_id)
                    || (session.station_id.is_none()
                        && session.structure_id.is_none()
                        && session.solar_system_id.map(|s| s as i64) == Some(object_id))
            }
            "station" | "corpStationMgr" | "repairSvc" | "reprocessingSvc" | "insuranceSvc" => {
                session.station_id == Some(object_id) || session.structure_id == Some(object_id)
            }
            "skillMgr2" | "charMgr" | "jumpCloneSvc" => {
                Some(object_id) == session.character_id
                    || session.station_id == Some(object_id)
                    || session.structure_id == Some(object_id)
                    || object_id == 0
            }
            "crimewatch" => {
                Some(object_id) == session.character_id
                    || session.solar_system_id.map(|s| s as i64) == Some(object_id)
                    || session.station_id == Some(object_id)
                    || object_id == 0
            }
            "i2" => session.has_gm_role() && object_id > 0,
            "agentMgr" | "keeper" | "planetMgr" | "posMgr" | "fleetObjectHandler"
            | "corpRegistry" | "corpmgr" | "allianceRegistry" | "warRegistry" | "warStatisticMgr"
            | "trademgr" | "voucher" | "structure" | "devIndexManager" | "contractProxy" => {
                object_id > 0
            }
            _ => false,
        };

        if !authorized {
            return Err(BridgeRpcError {
                code: ERR_BIND_AUTH_DENIED.into(),
                message: format!(
                    "Service '{service}' cannot be bound to object_id {object_id} for current session state"
                ),
            });
        }

        let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
        self.insert_bound_entry(session, bound_id, user_id, char_id, service, object_id);
        Ok(bound_id)
    }

    /// Binds a sub-object handed out by an already-bound service method (e.g. the inventory
    /// returned by `invbroker.GetInventoryFromId`). The caller must have verified that the
    /// session may access `object_id`; only character selection is checked here.
    pub fn bind_verified_sub_object(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
    ) -> Result<u64, BridgeRpcError> {
        let (user_id, char_id) = Self::require_character(session)?;
        let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
        self.insert_bound_entry(session, bound_id, user_id, char_id, service, object_id);
        Ok(bound_id)
    }

    fn insert_bound_entry(
        &self,
        session: &SessionState,
        bound_id: u64,
        user_id: u64,
        char_id: i64,
        service: &str,
        object_id: i64,
    ) {
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                bound_id,
                BoundObjectEntry {
                    bound_id,
                    session_id: session.session_id,
                    user_id,
                    character_id: char_id,
                    service: service.to_string(),
                    object_id,
                },
            );
    }

    /// Wire form of a bound-object reference. The native client (`caller_trace == "machoNet:26000"`)
    /// gets `SubStruct(SubStream((b"N={nodeID}:{boundID}", refID)))`, which `ObjectCall.GetObjectID`
    /// (`carbon/common/script/net/ObjectCallGPCS.pyj`) / `ParseObjectID` turn into a remote object;
    /// the JSON bridge gets the plain bound ID.
    pub fn bound_object_rep(&self, bound_id: u64, caller_trace: Option<&str>) -> PyRep {
        if caller_trace == Some("machoNet:26000") {
            let oid_str = format!("N={}:{bound_id}", eve_machonet::MACHO_PROXY_NODE_ID);
            let ref_id = (self.now_unix() as i64 + 11_644_473_600).saturating_mul(10_000_000);
            PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::ascii_bytes(oid_str),
                PyRep::Integer(ref_id),
            ])))))
        } else {
            PyRep::Integer(bound_id as i64)
        }
    }

    /// Verifies that `bound_id` exists and belongs to the caller's `(session_id, user_id, character_id)`.
    pub fn verify_bound_object(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Result<BoundObjectEntry, BridgeRpcError> {
        let entry = self
            .bound_objects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&bound_id)
            .cloned()
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidBoundObject".into(),
                message: format!("Bound object {bound_id} does not exist"),
            })?;

        let char_matches = Some(entry.character_id) == session.character_id
            || (entry.service == "populationCap" && session.character_id.is_none());
        if entry.session_id != session.session_id
            || Some(entry.user_id) != session.user_id
            || !char_matches
        {
            return Err(BridgeRpcError {
                code: ERR_BOUND_OWNERSHIP.into(),
                message: format!("Bound object {bound_id} is not owned by this session"),
            });
        }

        let context_valid = match entry.service.as_str() {
            "i2" => session.has_gm_role() && entry.object_id > 0,
            "beyonce" | "scanMgr" => {
                session.solar_system_id.map(i64::from) == Some(entry.object_id)
            }
            "station" | "stationSvc" | "corpStationMgr" | "repairSvc" | "reprocessingSvc"
            | "insuranceSvc" => {
                session.station_id == Some(entry.object_id)
                    || session.structure_id == Some(entry.object_id)
            }
            "invbroker" => {
                Some(entry.object_id) == session.character_id
                    || Some(entry.object_id) == session.ship_id
                    || Some(entry.object_id) == session.station_id
                    || Some(entry.object_id) == session.structure_id
                    || (session.station_id.is_none()
                        && session.structure_id.is_none()
                        && session.solar_system_id.map(i64::from) == Some(entry.object_id))
                    || self
                        .sde
                        .stations
                        .get(&entry.object_id)
                        .is_some_and(|st| Some(st.solar_system_id) == session.solar_system_id)
                    || self.db.with_snapshot(|snap| {
                        snap.structures
                            .get(&entry.object_id)
                            .is_some_and(|st| Some(st.solar_system_id) == session.solar_system_id)
                            || snap.items.get(&entry.object_id).is_some_and(|it| {
                                Some(it.location_id) == session.station_id
                                    || Some(it.location_id) == session.structure_id
                                    || Some(it.location_id) == session.ship_id
                                    || (session.station_id.is_none()
                                        && session.structure_id.is_none()
                                        && Some(it.location_id)
                                            == session.solar_system_id.map(i64::from))
                            })
                    })
            }
            _ => true,
        };
        if !context_valid {
            return Err(BridgeRpcError {
                code: ERR_BOUND_OWNERSHIP.into(),
                message: format!(
                    "Bound object {bound_id} ({}) for object_id {} is no longer valid in current session context",
                    entry.service, entry.object_id
                ),
            });
        }
        Ok(entry)
    }

    /// Releases a single bound object `bound_id` after verifying session ownership.
    pub fn release_bound_object(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Result<(), BridgeRpcError> {
        let _ = self.verify_bound_object(session, bound_id)?;
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&bound_id);
        Ok(())
    }

    /// Validates container ownership, co-location, and 2,500m in-space loot range for `invbroker.GetContainerContents`.
    pub async fn get_container_contents(
        &self,
        session: &SessionState,
        bound_entry: Option<&BoundObjectEntry>,
        char_id: i64,
        container_id: i64,
    ) -> Result<eve_proto::PyRep, BridgeRpcError> {
        use eve_proto::{DBRowDescriptor, PyRep};

        let (owner_id, loc_id, group_id, category_id, rows) = self.db.with_snapshot(|snap| {
            let Some(container) = snap.items.get(&container_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Container item {container_id} not found"),
                });
            };
            let (grp, cat) = self
                .sde
                .types
                .get(&container.type_id)
                .map(|t| (t.group_id, t.category_id))
                .unwrap_or((0, 0));
            let rows: Vec<Vec<PyRep>> = snap
                .items
                .values()
                .filter(|i| i.location_id == container_id)
                .map(|i| {
                    let (g, c) = self
                        .sde
                        .types
                        .get(&i.type_id)
                        .map(|t| (t.group_id as i64, t.category_id as i64))
                        .unwrap_or((0, 0));
                    let qty = if i.is_blueprint_copy {
                        -2
                    } else if i.singleton {
                        -1
                    } else {
                        i.quantity as i64
                    };
                    vec![
                        PyRep::Integer(i.item_id),
                        PyRep::Integer(i.type_id as i64),
                        PyRep::Integer(i.owner_id),
                        PyRep::Integer(i.location_id),
                        PyRep::Integer(i.flag as i64),
                        PyRep::Integer(qty),
                        PyRep::Integer(g),
                        PyRep::Integer(c),
                        PyRep::String(String::new()),
                    ]
                })
                .collect();
            Ok((container.owner_id, container.location_id, grp, cat, rows))
        })?;

        let deny = || BridgeRpcError {
            code: "CantDoThatWithSomeoneElsesStuff".into(),
            message: format!("Container {container_id} is not accessible by character {char_id}"),
        };

        if let Some(b) = bound_entry {
            if b.object_id != container_id && b.object_id != loc_id {
                return Err(deny());
            }
        }

        if owner_id == char_id {
            let co_located_docked_or_ship = session.ship_id == Some(container_id)
                || session.station_id == Some(loc_id)
                || session.structure_id == Some(loc_id)
                || session.ship_id == Some(loc_id);
            if !co_located_docked_or_ship {
                let Some(sys_id) = u32::try_from(loc_id).ok().filter(|s| {
                    session.station_id.is_none()
                        && session.structure_id.is_none()
                        && session.solar_system_id == Some(*s)
                        && self.sde.solar_systems.contains_key(s)
                }) else {
                    return Err(deny());
                };
                let Some(ship_id) = session.ship_id else {
                    return Err(deny());
                };
                let sys_handle = self.systems.get_or_spawn(sys_id);
                let ship_ball = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten();
                let cont_ball = sys_handle
                    .get_ball_snapshot(container_id)
                    .await
                    .ok()
                    .flatten();
                let (Some((_, sb)), Some((_, cb))) = (ship_ball, cont_ball) else {
                    return Err(deny());
                };
                if sb.position.distance(cb.position) > 2_500.0 {
                    return Err(deny());
                }
            }
        } else {
            let is_openable_container =
                category_id != 6 && matches!(group_id, 12 | 186 | 340 | 448 | 649);
            if !is_openable_container {
                return Err(deny());
            }
            let Some(sys_id) = u32::try_from(loc_id).ok().filter(|s| {
                session.station_id.is_none()
                    && session.structure_id.is_none()
                    && session.solar_system_id == Some(*s)
                    && self.sde.solar_systems.contains_key(s)
            }) else {
                return Err(deny());
            };
            let Some(ship_id) = session.ship_id else {
                return Err(deny());
            };
            let sys_handle = self.systems.get_or_spawn(sys_id);
            let ship_ball = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten();
            let cont_ball = sys_handle
                .get_ball_snapshot(container_id)
                .await
                .ok()
                .flatten();
            let (Some((_, sb)), Some((_, cb))) = (ship_ball, cont_ball) else {
                return Err(deny());
            };
            if sb.position.distance(cb.position) > 2_500.0 {
                return Err(deny());
            }
        }

        Ok(DBRowDescriptor::inv_item_header().build_crowset(rows))
    }

    /// Builds a `CombatState` for a player's ship populated from its Dogma `ShipFittingEngine`
    /// attributes (or SDE `default_attributes` fallback), including all 12 shield/armor/hull resonances.
    pub fn build_ship_combat_state(
        &self,
        char_id: i64,
        ship_id: i64,
        type_id: u32,
    ) -> eve_destiny::CombatState {
        if let Ok(fit) = self.get_or_rebuild_ship_fitting(char_id, ship_id) {
            let get_attr = |attr_id: u16, fallback: f64| -> f64 {
                if fit.ship.base_attributes.contains_key(&attr_id)
                    || self.sde.attributes.contains_key(&attr_id)
                    || fit
                        .ship
                        .modifiers
                        .iter()
                        .any(|m| m.target_attribute_id == attr_id)
                {
                    fit.ship.get_attribute(attr_id, &self.sde)
                } else {
                    fallback
                }
            };
            let shield = get_attr(eve_sde::attr_ids::SHIELD_CAPACITY, 450.0);
            let armor = get_attr(eve_sde::attr_ids::ARMOR_HP, 350.0);
            let hull = get_attr(eve_sde::attr_ids::HP_STRUCT, 350.0);
            let cap = get_attr(eve_sde::attr_ids::CAPACITOR_CAPACITY, 250.0);
            let sig = get_attr(eve_sde::attr_ids::SIGNATURE_RADIUS, 35.0);
            let shield_rech = get_attr(eve_sde::attr_ids::SHIELD_RECHARGE_RATE, 120_000.0);
            let cap_rech = get_attr(eve_sde::attr_ids::RECHARGE_RATE, 120_000.0);
            let mut cs = eve_destiny::CombatState {
                shield_hp: shield,
                max_shield_hp: shield,
                armor_hp: armor,
                max_armor_hp: armor,
                structure_hp: hull,
                max_structure_hp: hull,
                capacitor: cap,
                max_capacitor: cap,
                signature_radius: sig,
                shield_recharge_time_ms: shield_rech,
                capacitor_recharge_time_ms: cap_rech,
                shield_resonance: [
                    get_attr(eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE, 1.0),
                    get_attr(eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE, 0.8),
                    get_attr(eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE, 0.6),
                    get_attr(eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, 0.5),
                ],
                armor_resonance: [
                    get_attr(eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE, 0.5),
                    get_attr(eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE, 0.65),
                    get_attr(eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE, 0.75),
                    get_attr(eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, 0.9),
                ],
                structure_resonance: [
                    get_attr(eve_sde::attr_ids::EM_DAMAGE_RESONANCE, 0.67),
                    get_attr(eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE, 0.67),
                    get_attr(eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE, 0.67),
                    get_attr(eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE, 0.67),
                ],
                rng_state: eve_destiny::CombatState::seed_rng_from_u64(
                    (ship_id as u64)
                        ^ (char_id as u64).rotate_left(17)
                        ^ (u64::from(type_id) << 32),
                ),
                ..eve_destiny::CombatState::default()
            };
            crate::sp6_lifecycle::apply_persisted_hp_to_combat_state(self, ship_id, &mut cs);
            return cs;
        }
        let mut cs = self.build_type_combat_state(type_id);
        cs.rng_state = eve_destiny::CombatState::seed_rng_from_u64(
            (ship_id as u64) ^ (char_id as u64).rotate_left(17) ^ (u64::from(type_id) << 32),
        );
        crate::sp6_lifecycle::apply_persisted_hp_to_combat_state(self, ship_id, &mut cs);
        cs
    }

    /// Builds a `CombatState` directly from a type's SDE `default_attributes` (for NPCs, wrecks, or fallback hulls).
    pub fn build_type_combat_state(&self, type_id: u32) -> eve_destiny::CombatState {
        let seq = self
            .combat_rng_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let rng_state =
            eve_destiny::CombatState::seed_rng_from_u64(seq ^ (u64::from(type_id) << 32));
        if let Some(t) = self.sde.types.get(&type_id) {
            let a = &t.default_attributes;
            let shield = a
                .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                .copied()
                .unwrap_or(50.0);
            let armor = a.get(&eve_sde::attr_ids::ARMOR_HP).copied().unwrap_or(50.0);
            let hull = a
                .get(&eve_sde::attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(50.0);
            let cap = a
                .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
                .copied()
                .unwrap_or(100.0);
            let sig = a
                .get(&eve_sde::attr_ids::SIGNATURE_RADIUS)
                .copied()
                .unwrap_or(t.radius.max(25.0));
            let shield_rech = a
                .get(&eve_sde::attr_ids::SHIELD_RECHARGE_RATE)
                .copied()
                .unwrap_or(120_000.0);
            let cap_rech = a
                .get(&eve_sde::attr_ids::RECHARGE_RATE)
                .copied()
                .unwrap_or(120_000.0);
            return eve_destiny::CombatState {
                shield_hp: shield,
                max_shield_hp: shield,
                armor_hp: armor,
                max_armor_hp: armor,
                structure_hp: hull,
                max_structure_hp: hull,
                capacitor: cap,
                max_capacitor: cap,
                signature_radius: sig,
                shield_recharge_time_ms: shield_rech,
                capacitor_recharge_time_ms: cap_rech,
                shield_resonance: [
                    a.get(&eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                ],
                armor_resonance: [
                    a.get(&eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                ],
                structure_resonance: [
                    a.get(&eve_sde::attr_ids::EM_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                    a.get(&eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE)
                        .copied()
                        .unwrap_or(1.0),
                ],
                rng_state,
                ..eve_destiny::CombatState::default()
            };
        }
        eve_destiny::CombatState {
            rng_state,
            ..eve_destiny::CombatState::default()
        }
    }

    /// Builds a [`eve_destiny::SlimItemMeta`] for a player ship from SDE type metadata and the character's DB record (`P-7`).
    pub fn build_ship_slim_meta(&self, char_id: i64, type_id: u32) -> eve_destiny::SlimItemMeta {
        let (group_id, category_id) = self
            .sde
            .types
            .get(&type_id)
            .map(|t| (t.group_id, t.category_id))
            .unwrap_or((25, 6));
        let (corp_id, alliance_id, security_status) = self
            .db
            .get_character(char_id)
            .map(|ch| {
                (
                    ch.corporation_id,
                    ch.alliance_id.unwrap_or(0),
                    ch.security_status,
                )
            })
            .unwrap_or((1_000_009, 0, 0.0));
        eve_destiny::SlimItemMeta {
            group_id,
            category_id,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id,
            alliance_id,
            security_status,
            is_cloaked: false,
        }
    }

    /// Emits an `OnSkillQueuePausedServer` notification to character (`feat:skills.queue-pause`).
    pub fn notify_skill_queue_paused_server(&self, char_id: i64) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnSkillQueuePausedServer",
            "charid",
            vec![],
        )
    }

    /// `OnNewSkillQueueSaved(newQueue)`; the client stores `newQueue` as its skill queue and
    /// scatters `OnSkillQueueChanged`.
    pub fn notify_new_skill_queue_saved(
        &self,
        char_id: i64,
        new_queue: PyRep,
    ) -> Vec<PendingMachoNotification> {
        self.notify(
            NotificationTarget::Character(char_id),
            "OnNewSkillQueueSaved",
            "charid",
            vec![new_queue],
        )
    }

    /// Wires the module deactivation hook on the `SystemRegistry` to handle automatic cycle deactivations
    /// (e.g. entering warp, target out of range, or target leaving the ballpark).
    /// Emits `OnGodmaShipEffect` (with `isActive = 0`) to the pilot and invalidates the cached ship fitting.
    pub fn wire_module_deactivation_hook(&self) {
        let sde = Arc::clone(&self.sde);
        let db = Arc::clone(&self.db);
        let pending = Arc::clone(&self.pending_notifications);
        let notify_tx = self.notify_tx.clone();
        let ship_fittings = Arc::clone(&self.ship_fittings);

        self.systems.set_module_deactivation_hook(Arc::new(move |char_id, cycle| {
            ship_fittings
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&cycle.ship_id);

            let effect_id = effect_id_for_module_sde(&sde, cycle.module_type_id, Some(&cycle.effect_name)).unwrap_or(0);
            let now = db.clock().now_blueos_filetime_ticks();
            let environment = PyRep::Tuple(vec![
                PyRep::Integer(cycle.module_id),
                PyRep::Integer(char_id),
                PyRep::Integer(cycle.ship_id),
                cycle.target_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                PyRep::None,
                PyRep::List(vec![]),
                PyRep::Integer(effect_id),
            ]);
            let args = vec![
                PyRep::Integer(cycle.module_id),
                PyRep::Integer(effect_id),
                PyRep::Integer(now),
                PyRep::Integer(0),
                PyRep::Integer(0),
                environment,
                PyRep::Integer(now),
                PyRep::Real(0.0),
                PyRep::Integer(0),
                PyRep::None,
            ];
            let user_id = db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id));
            let (packet, marshaled_packet) =
                ServerContext::build_macho_notification_packet(char_id, user_id, "OnGodmaShipEffect", "charid", args.clone());
            let notif = PendingMachoNotification {
                recipient_char_id: char_id,
                event: "OnGodmaShipEffect".to_string(),
                id_type: "charid".to_string(),
                args,
                packet,
                marshaled_packet,
                created_filetime: now,
            };
            {
                let mut q_guard = pending.write().unwrap_or_else(|e| e.into_inner());
                let q = q_guard.entry(char_id).or_default();
                if q.len() >= 512 {
                    q.remove(0);
                }
                q.push(notif.clone());
            }
            let _ = notify_tx.send((char_id, notif));

            let auto_reload_ev: Option<(Vec<i64>, u32, f64)> = db.with_mut_snapshot(|snap| {
                snap.service_state_blobs
                    .remove(&format!("auto_reloaded_event:{}", cycle.module_id))
                    .and_then(|s| serde_json::from_str(&s).ok())
            });
            if let Some((mod_ids, charge_tid, reload_ms)) = auto_reload_ev {
                let reload_args = vec![
                    PyRep::List(mod_ids.into_iter().map(PyRep::Integer).collect()),
                    PyRep::Integer(charge_tid as i64),
                    PyRep::Real(reload_ms),
                ];
                let (r_pkt, r_marshaled) = ServerContext::build_macho_notification_packet(
                    char_id,
                    user_id,
                    "OnChargeBeingLoadedToModule",
                    "charid",
                    reload_args.clone(),
                );
                let r_notif = PendingMachoNotification {
                    recipient_char_id: char_id,
                    event: "OnChargeBeingLoadedToModule".to_string(),
                    id_type: "charid".to_string(),
                    args: reload_args,
                    packet: r_pkt,
                    marshaled_packet: r_marshaled,
                    created_filetime: now,
                };
                {
                    let mut q_guard = pending.write().unwrap_or_else(|e| e.into_inner());
                    let q = q_guard.entry(char_id).or_default();
                    if q.len() >= 512 {
                        q.remove(0);
                    }
                    q.push(r_notif.clone());
                }
                let _ = notify_tx.send((char_id, r_notif));
            }
        }));
    }

    /// Constructs a lightweight `ServerContext` handle referencing shared core components for system step execution.
    #[allow(clippy::too_many_arguments)]
    pub fn from_step_components(
        systems: std::sync::Arc<crate::system_actor::SystemRegistry>,
        db: std::sync::Arc<eve_db::WorldRepository>,
        sde: std::sync::Arc<eve_sde::SdeRegistry>,
        mechanic_rng: std::sync::Arc<crate::rng::MechanicRng>,
        ship_fittings: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<i64, eve_dogma::ShipFittingEngine>>>,
        pending_notifications: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<i64, Vec<super::cache_notifications::PendingMachoNotification>>>>,
        notify_tx: tokio::sync::broadcast::Sender<(i64, super::cache_notifications::PendingMachoNotification)>,
        combat_ephemeral: std::sync::Arc<std::sync::RwLock<crate::services::combat_crimewatch::CombatRuntimeState>>,
        quasar_gateway: std::sync::Arc<crate::quasar_gateway::PublicGatewayRouter>,
        invention_roll_override: std::sync::Arc<std::sync::RwLock<Option<f64>>>,
        module_reactivation_cooldowns: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<(i64, i64, u32), super::context::ModuleCooldownEntry>>>,
    ) -> Self {
        let crimewatch = crate::combat::crimewatch::CrimeWatchLock::new(std::sync::Arc::clone(&db));
        let clock = std::sync::RwLock::new(db.clock());
        let living_world = std::sync::Arc::new(crate::living_world::LivingWorldDirector::new());
        let esi_state = std::sync::Arc::new(crate::esi::runtime_state::EsiRuntimeState::new());
        let cluster_state = std::sync::Arc::new(std::sync::RwLock::new(crate::services::macho_cluster_mgmt::MachoClusterState::default()));
        let tactical_state = std::sync::Arc::new(std::sync::RwLock::new(crate::services::tactical_modules::TacticalModulesState::default()));
        Self {
            instance_id: super::context::next_ctx_instance_id(),
            sde,
            db,
            clock,
            systems,
            living_world,
            quasar_gateway,
            cataloged_services: std::collections::HashSet::new(),
            quasar_domains: std::collections::HashSet::new(),
            bound_objects: std::sync::RwLock::new(std::collections::HashMap::new()),
            object_cache: std::sync::RwLock::new(std::collections::HashMap::new()),
            object_cache_timestamps: std::sync::RwLock::new(std::collections::HashMap::new()),
            notification_records: std::sync::RwLock::new(Vec::new()),
            pending_notifications,
            notify_tx,
            map_landmarks: std::sync::RwLock::new(Vec::new()),
            session_closers: std::sync::RwLock::new(std::collections::HashMap::new()),
            ship_fittings,
            projected_ewar: std::sync::RwLock::new(std::collections::HashMap::new()),
            weapon_cycle_state: std::sync::RwLock::new(std::collections::HashMap::new()),
            fighter_cooldown_state: std::sync::RwLock::new(std::collections::HashMap::new()),
            fighter_charge_state: std::sync::RwLock::new(std::collections::HashMap::new()),
            active_fighters: std::sync::RwLock::new(std::collections::HashMap::new()),
            fighter_tube_loads: std::sync::RwLock::new(std::collections::HashMap::new()),
            active_scanner_probes: std::sync::RwLock::new(std::collections::HashMap::new()),
            active_moon_probes: std::sync::RwLock::new(Vec::new()),
            scanned_signatures: std::sync::RwLock::new(std::collections::HashSet::new()),
            character_scan_progress: std::sync::RwLock::new(std::collections::HashMap::new()),
            invention_roll_override,
            mechanic_rng,
            module_reactivation_cooldowns,
            mining_cycles: std::sync::RwLock::new(std::collections::HashMap::new()),
            crimewatch,
            combat_ephemeral,
            tactical_state,
            session_change_timers: std::sync::Mutex::new(std::collections::HashMap::new()),
            active_warps: std::sync::Mutex::new(std::collections::HashMap::new()),
            location_change_invuln: std::sync::Mutex::new(std::collections::HashMap::new()),
            feature_flags: crate::config::FeatureFlagsConfig::default(),
            esi_port: 0,
            esi_state,
            fleet_extras: std::sync::Mutex::new(Default::default()),
            social_extras: std::sync::Mutex::new(Default::default()),
            cluster_misc_state: std::sync::Mutex::new(Default::default()),
            cluster_state,
            global_config_values: std::sync::RwLock::new(std::collections::HashMap::new()),
            alert_telemetry: std::sync::Mutex::new(Default::default()),
            npc_spawns: std::sync::RwLock::new(std::collections::HashMap::new()),
            combat_rng_seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Computes a target entity's effective signature radius including active Target Painters
    /// (`group 379`, stacking-penalized) and ship fitting modifiers (`feat:combat.target-painter-bloom`).
    pub fn effective_target_signature_radius(&self, target_ball_id: i64, fallback_base_sig: f64) -> f64 {
        let (owner_char_opt, item_base_sig_opt, tp_resist_opt) = self.db.with_snapshot(|snap| {
            let char_id = snap.characters.values().find(|c| c.active_ship_id == target_ball_id).map(|c| c.character_id)
                .or_else(|| snap.items.get(&target_ball_id).map(|i| i.owner_id).filter(|oid| snap.characters.contains_key(oid)));
            let item_t = snap.items.get(&target_ball_id).and_then(|i| self.sde.types.get(&i.type_id));
            let base_sig = item_t
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SIGNATURE_RADIUS).copied()).filter(|&v| v > 0.0);
            let tp_resist = item_t
                .and_then(|t| t.default_attributes.get(&2114).copied());
            (char_id, base_sig, tp_resist)
        });
        if let Some(cid) = owner_char_opt {
            if let Ok(fit) = self.get_or_rebuild_ship_fitting(cid, target_ball_id) {
                let sig = fit.ship.get_attribute(eve_sde::attr_ids::SIGNATURE_RADIUS, &self.sde);
                if sig > 0.0 {
                    return sig;
                }
            }
        }
        let base_sig = item_base_sig_opt.unwrap_or_else(|| fallback_base_sig.max(1.0));
        let tp_resist = tp_resist_opt.unwrap_or(1.0).clamp(0.0, 1.0);
        let bonuses: Vec<f64> = self.projected_ewar.read().unwrap_or_else(|e| e.into_inner())
            .get(&target_ball_id)
            .map(|list| {
                list.iter()
                    .filter(|proj| self.sde.types.get(&proj.source_module_type_id).is_some_and(|t| t.group_id == 379) || proj.effective_module.base_attributes.contains_key(&554))
                    .map(|proj| {
                        let opt = proj.effective_module.get_attribute(eve_sde::attr_ids::MAX_RANGE, &self.sde);
                        let fall = eve_dogma::get_module_falloff(&proj.effective_module, &self.sde);
                        let att = eve_dogma::compute_falloff_attenuation(proj.distance, opt, fall);
                        proj.effective_module.get_attribute(554, &self.sde) * att * tp_resist
                    })
                    .collect()
            })
            .unwrap_or_default();
        eve_dogma::eval_target_painters_sig_radius(base_sig, &bonuses)
    }

    // Covers: ship boarding skill and clone checks (`feat:ships.board-ship-skill-and-clone-check`)
    pub fn check_type_skill_prerequisites_in_snapshot(
        &self,
        snap: &eve_db::WorldStateSnapshot,
        char_id: i64,
        type_id: u32,
    ) -> Result<(), BridgeRpcError> {
        let Some(t) = self.sde.types.get(&type_id) else {
            return Ok(());
        };
        let char_skills = snap.skills.get(&char_id);
        let char_rec = snap.characters.get(&char_id);
        let is_omega = char_rec.is_some_and(|c| {
            let exp_opt = crate::services::account_services::subscription_expiry_for_user(snap, c.user_id);
            c.clone_grade >= 2 && exp_opt.map(|u| u > self.now_unix()).unwrap_or(true)
        });
        let is_alpha_clone = !is_omega;
        let race_id = char_rec.map(|c| c.race_id).unwrap_or(1);
        let req_pairs = [
            (182_u16, 277_u16),
            (183, 278),
            (184, 279),
            (1285, 1286),
            (1289, 1287),
            (1290, 1288),
        ];
        for (req_skill_attr, req_lvl_attr) in req_pairs {
            if let Some(&skill_f) = t.default_attributes.get(&req_skill_attr) {
                let skill_id = skill_f.round() as u32;
                if skill_id > 0 {
                    let req_lvl = t
                        .default_attributes
                        .get(&req_lvl_attr)
                        .copied()
                        .unwrap_or(1.0)
                        .round()
                        .clamp(1.0, 5.0) as u8;
                    let mut raw_trained = char_skills
                        .and_then(|m| m.get(&skill_id))
                        .map(|r| r.level)
                        .unwrap_or(0);
                    if raw_trained < req_lvl && (12180..=12199).contains(&skill_id) && skill_id != 12189 {
                        for alt_skill in [60377_u32, 60378, 60379, 60380, 60381] {
                            let alt_lvl = char_skills
                                .and_then(|m| m.get(&alt_skill))
                                .map(|r| r.level)
                                .unwrap_or(0);
                            raw_trained = raw_trained.max(alt_lvl);
                        }
                    }
                    if raw_trained < req_lvl {
                        let is_ship = t.category_id == 6;
                        let code = if is_ship { "ShipHasSkillPrerequisites" } else { "SkillPrerequisitesNotMet" };
                        return Err(BridgeRpcError::user_error(code));
                    }
                    if is_alpha_clone {
                        let alpha_cap =
                            crate::services::character_skills::alpha_skill_cap_for_character(
                                &self.sde, race_id, skill_id,
                            );
                        let effective_trained = raw_trained.min(alpha_cap);
                        if req_lvl > alpha_cap || effective_trained < req_lvl {
                            let is_ship = t.category_id == 6;
                            let code = if is_ship { "TrialAccountRestriction" } else { "SkillPrerequisitesNotMet" };
                            return Err(BridgeRpcError::user_error(code));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Helper to determine the effect ID for a module given its type ID and optional effect name.
pub fn effect_id_for_module_sde(
    sde: &eve_sde::SdeRegistry,
    type_id: u32,
    effect_name: Option<&str>,
) -> Option<i64> {
    let ty = sde.types.get(&type_id);
    if let Some(name) = effect_name {
        let in_type = ty.and_then(|t| {
            t.effect_ids
                .iter()
                .find(|eid| sde.effects.get(eid).is_some_and(|e| e.name == name))
                .copied()
        });
        if let Some(eid) = in_type {
            return Some(eid as i64);
        }
        if let Some(e) = sde.effects.values().find(|e| e.name == name) {
            return Some(e.effect_id as i64);
        }
    }
    ty.and_then(|t| t.default_effect_id).map(|e| e as i64)
}
