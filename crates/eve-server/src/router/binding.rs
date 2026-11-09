//! Stateful object binding and moniker handles (`MachoBindObject`).

use crate::router::context::{BoundObjectEntry, ServerContext, NEXT_BOUND_ID};
use crate::router::{NotificationTarget, PendingMachoNotification};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::sync::atomic::Ordering;
use std::sync::Arc;

const ERR_BIND_AUTH_DENIED: &str = "BindAuthorizationDenied";
const ERR_BOUND_OWNERSHIP: &str = "BoundObjectOwnershipViolation";

/// RAII guard for TCP client connection tasks (`machoNet` and Python Client Bridge) ensuring
/// synchronous session state (`session_closers`, `bound_objects`, and `PublicGatewayRouter`
/// notice subscribers/receivers) and any spawned frame-reader task are always cleaned up,
/// even if the connection future returns early or is aborted/dropped mid-await.
pub struct SessionCleanupGuard {
    ctx: Arc<ServerContext>,
    session_id: u64,
    reader_abort: Option<tokio::task::AbortHandle>,
}

impl SessionCleanupGuard {
    pub fn new(ctx: Arc<ServerContext>, session_id: u64) -> Self {
        Self {
            ctx,
            session_id,
            reader_abort: None,
        }
    }

    pub fn set_reader_abort(&mut self, abort_handle: tokio::task::AbortHandle) {
        self.reader_abort = Some(abort_handle);
    }
}

impl Drop for SessionCleanupGuard {
    fn drop(&mut self) {
        if let Some(ref handle) = self.reader_abort {
            handle.abort();
        }
        self.ctx.unregister_session_closer(self.session_id);
        self.ctx.release_session_bindings(self.session_id);
        self.ctx.quasar_gateway.unsubscribe_session(self.session_id);
    }
}

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

    /// Actively signals an open TCP session (`session_id`) to send `GPSTransportClosed` and close its socket,
    /// releasing bound objects and unsubscribing any Quasar gateway notice stream/receiver.
    pub fn disconnect_session(&self, session_id: u64, reason_code: &str, reason: &str) -> bool {
        self.release_session_bindings(session_id);
        self.quasar_gateway.unsubscribe_session(session_id);
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

    /// Releases all bound objects and Quasar notice subscriptions associated with `user_id`
    /// (called on ban, suspension, kick, or session displacement).
    pub fn release_user_bindings(&self, user_id: u64) {
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, v| v.user_id != user_id);
        self.quasar_gateway.unsubscribe_user(user_id);
    }

    /// Unified disconnect cleanup shared by both native `machoNet` TCP (`handle_macho_client`)
    /// and Python Client Bridge TCP (`handle_bridge_client`) listeners.
    ///
    /// Unregisters the session closer, releases all bound objects, unsubscribes any
    /// `PublicGatewayRouter` notice subscriber and stream receiver for `session.session_id`,
    /// and—if this session still held the account's active online lock—releases structure
    /// control, persists in-space ship ball coordinates, runs SP-6 logoff/disconnect
    /// lifecycle, and notifies watchlist and fleet/social presence listeners.
    pub async fn cleanup_disconnected_client_session(&self, session: &mut SessionState) {
        let session_id = session.session_id;
        self.unregister_session_closer(session_id);
        self.release_session_bindings(session_id);
        self.quasar_gateway.unsubscribe_session(session_id);
        if let Some(uid) = session.user_id {
            // A dropped connection that still held the character online is a log-off for watchers and triggers SP-6 in-space logoff handling.
            if self.db.release_online_lock(uid, session_id) {
                if let Some(cid) = session.character_id {
                    crate::services::structure_control_ops::release_all_structure_control_for_character(
                        self,
                        Some(session),
                        cid,
                    );
                }
                if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                    if session.station_id.is_none() && session.structure_id.is_none() {
                        let handle = self.systems.get_or_spawn(sys_id);
                        if let Ok(Some((_, ball))) = handle.get_ball_snapshot(ship_id).await {
                            self.db.with_mut_snapshot(|snap| {
                                snap.item_positions.insert(
                                    ship_id,
                                    [ball.position.x, ball.position.y, ball.position.z],
                                );
                            });
                        }
                    }
                }
                if let Some(cid) = session.character_id {
                    let _ = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(self, cid)
                        .await;
                    crate::services::character_skills::notify_watchlist_presence(self, cid, false);
                    crate::services::fleet_social::fleet_social_notify::on_character_logout(
                        self, cid,
                    );
                    crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(
                        self, cid,
                    );
                }
            }
        }
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

        let authorized =
            crate::router::binding_auth::authorize_service_binding(self, session, char_id, service, object_id);

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
        let oid_str = format!("N={}:{bound_id}", eve_machonet::MACHO_PROXY_NODE_ID);
        let ref_id = (self.now_unix() as i64 + 11_644_473_600).saturating_mul(10_000_000);
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
                    oid_str,
                    ref_id,
                },
            );
    }

    /// Wire form of a bound-object reference. The native client (`caller_trace == "machoNet:26000"`)
    /// gets `SubStruct(SubStream((b"N={nodeID}:{boundID}", refID)))`, which `ObjectCall.GetObjectID`
    /// (`carbon/common/script/net/ObjectCallGPCS.pyj`) / `ParseObjectID` turn into a remote object;
    /// the JSON bridge gets the plain bound ID.
    pub fn bound_object_rep(&self, bound_id: u64, caller_trace: Option<&str>) -> PyRep {
        if caller_trace == Some("machoNet:26000") {
            let (oid_str, ref_id) = self
                .bound_objects
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&bound_id)
                .map(|b| (b.oid_str.clone(), b.ref_id))
                .unwrap_or_else(|| {
                    (
                        format!("N={}:{bound_id}", eve_machonet::MACHO_PROXY_NODE_ID),
                        (self.now_unix() as i64 + 11_644_473_600).saturating_mul(10_000_000),
                    )
                });
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

        let context_valid =
            crate::router::binding_auth::verify_service_binding_context(self, session, &entry);
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

    fn emit_macho_object_disconnect(&self, entry: &BoundObjectEntry) {
        if entry.character_id <= 0 {
            return;
        }
        let macho_client_id =
            1_000_000_000_000_u64.saturating_add(entry.session_id) as i64;
        self.notify(
            NotificationTarget::Character(entry.character_id),
            "OnMachoObjectDisconnect",
            "clientID",
            vec![
                PyRep::ascii_bytes(entry.oid_str.clone()),
                PyRep::Integer(macho_client_id),
                PyRep::Integer(entry.ref_id),
            ],
        );
    }

    /// Invalidates solar-system-scoped and station/structure-scoped bound objects for `char_id`
    /// across location transitions (`stargate_jump`, `dock`, `undock`, `podding`, `wormhole_jump`,
    /// `jump_drive`, `cyno_jump`, `filamentJump`, `jump_bridge`, `shipcaster`, `clone_jump`, etc.)
    /// and emits `OnMachoObjectDisconnect(oid_str, macho_client_id, ref_id)` for each removed binding.
    pub fn invalidate_bound_objects_for_session_transition(
        &self,
        char_id: i64,
        system_id: u32,
        active_dock_id: Option<i64>,
        reason: &str,
    ) {
        if matches!(reason, "set_account_key" | "login") {
            return;
        }
        let mut removed = Vec::new();
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|&_bid, b| {
                if b.character_id != char_id {
                    return true;
                }
                if crate::router::binding_auth::should_invalidate_bound_object(
                    self,
                    b,
                    system_id,
                    active_dock_id,
                    reason,
                ) {
                    removed.push(b.clone());
                    false
                } else {
                    true
                }
            });
        for entry in &removed {
            self.emit_macho_object_disconnect(entry);
        }
    }

    /// Releases a single bound object `bound_id` after verifying session ownership.
    pub fn release_bound_object(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Result<(), BridgeRpcError> {
        let _ = self.verify_bound_object(session, bound_id)?;
        let removed = self
            .bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&bound_id);
        if let Some(entry) = removed {
            self.emit_macho_object_disconnect(&entry);
        }
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
            use eve_sde::attr_ids as a;
            let mut eff_attrs: std::collections::HashMap<u16, f64> = [
                a::SHIELD_CAPACITY, a::ARMOR_HP, a::HP_STRUCT, a::CAPACITOR_CAPACITY,
                a::SIGNATURE_RADIUS, a::SHIELD_RECHARGE_RATE, a::RECHARGE_RATE,
                a::MAX_VELOCITY, a::MASS, a::AGILITY,
                a::SHIELD_EM_DAMAGE_RESONANCE, a::SHIELD_THERMAL_DAMAGE_RESONANCE,
                a::SHIELD_KINETIC_DAMAGE_RESONANCE, a::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE,
                a::ARMOR_EM_DAMAGE_RESONANCE, a::ARMOR_THERMAL_DAMAGE_RESONANCE,
                a::ARMOR_KINETIC_DAMAGE_RESONANCE, a::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE,
                a::EM_DAMAGE_RESONANCE, a::THERMAL_DAMAGE_RESONANCE,
                a::KINETIC_DAMAGE_RESONANCE, a::EXPLOSIVE_DAMAGE_RESONANCE,
            ]
            .into_iter()
            .filter(|&aid| {
                fit.ship.base_attributes.contains_key(&aid)
                    || self.sde.attributes.contains_key(&aid)
                    || fit.ship.modifiers.iter().any(|m| m.target_attribute_id == aid)
            })
            .map(|aid| (aid, fit.ship.get_attribute(aid, &self.sde)))
            .collect();
            crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(self, ship_id, &mut eff_attrs);
            let get_attr = |attr_id: u16, fallback: f64| eff_attrs.get(&attr_id).copied().unwrap_or(fallback);
            let shield = get_attr(eve_sde::attr_ids::SHIELD_CAPACITY, 450.0);
            let armor = get_attr(eve_sde::attr_ids::ARMOR_HP, 350.0);
            let hull = get_attr(eve_sde::attr_ids::HP_STRUCT, 350.0);
            let raw_cap = get_attr(eve_sde::attr_ids::CAPACITOR_CAPACITY, 250.0);
            let cap = if raw_cap <= 0.0 { if type_id == 670 { 100.0 } else { 250.0 } } else { raw_cap };
            let sig = get_attr(eve_sde::attr_ids::SIGNATURE_RADIUS, 35.0);
            let tp_mult = self.effective_target_painter_multiplier(ship_id);
            let base_sig = if tp_mult > 1.0 + 1e-6 { sig / tp_mult } else { sig };
            let max_vel_opt = eff_attrs.get(&a::MAX_VELOCITY).copied().filter(|&v| v > 0.0);
            let mass_opt = eff_attrs.get(&a::MASS).copied().filter(|&v| v > 0.0);
            let inertia_opt = eff_attrs.get(&a::AGILITY).copied().filter(|&v| v > 0.0);
            let shield_rech = get_attr(eve_sde::attr_ids::SHIELD_RECHARGE_RATE, 120_000.0);
            let cap_rech = get_attr(eve_sde::attr_ids::RECHARGE_RATE, 120_000.0);
            let shield_res = [
                get_attr(eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE, 1.0),
                get_attr(eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE, 0.8),
                get_attr(eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE, 0.6),
                get_attr(eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, 0.5),
            ];
            let armor_res = [
                get_attr(eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE, 0.5),
                get_attr(eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE, 0.65),
                get_attr(eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE, 0.75),
                get_attr(eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, 0.9),
            ];
            let struct_res = [
                get_attr(eve_sde::attr_ids::EM_DAMAGE_RESONANCE, 0.67),
                get_attr(eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE, 0.67),
                get_attr(eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE, 0.67),
                get_attr(eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE, 0.67),
            ];
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
                base_signature_radius: Some(base_sig),
                tp_sig_multiplier: (tp_mult > 1.0 + 1e-6).then_some(tp_mult),
                shield_recharge_time_ms: shield_rech,
                capacitor_recharge_time_ms: cap_rech,
                shield_resonance: shield_res,
                armor_resonance: armor_res,
                structure_resonance: struct_res,
                base_max_velocity: max_vel_opt,
                base_mass: mass_opt,
                base_inertia: inertia_opt,
                base_shield_resonance: Some(shield_res),
                base_armor_resonance: Some(armor_res),
                base_structure_resonance: Some(struct_res),
                base_max_shield_hp: Some(shield),
                base_max_armor_hp: Some(armor),
                base_max_structure_hp: Some(hull),
                base_max_capacitor: Some(cap),
                base_capacitor_recharge_time_ms: Some(cap_rech),
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
        {
            use eve_sde::attr_ids as a;
            let mut eff_attrs: std::collections::HashMap<u16, f64> = [
                (a::SHIELD_CAPACITY, cs.max_shield_hp), (a::ARMOR_HP, cs.max_armor_hp),
                (a::HP_STRUCT, cs.max_structure_hp), (a::CAPACITOR_CAPACITY, cs.max_capacitor),
                (a::SIGNATURE_RADIUS, cs.signature_radius),
                (a::SHIELD_RECHARGE_RATE, cs.shield_recharge_time_ms),
                (a::RECHARGE_RATE, cs.capacitor_recharge_time_ms),
                (a::SHIELD_EM_DAMAGE_RESONANCE, cs.shield_resonance[0]),
                (a::SHIELD_THERMAL_DAMAGE_RESONANCE, cs.shield_resonance[1]),
                (a::SHIELD_KINETIC_DAMAGE_RESONANCE, cs.shield_resonance[2]),
                (a::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, cs.shield_resonance[3]),
                (a::ARMOR_EM_DAMAGE_RESONANCE, cs.armor_resonance[0]),
                (a::ARMOR_THERMAL_DAMAGE_RESONANCE, cs.armor_resonance[1]),
                (a::ARMOR_KINETIC_DAMAGE_RESONANCE, cs.armor_resonance[2]),
                (a::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, cs.armor_resonance[3]),
                (a::EM_DAMAGE_RESONANCE, cs.structure_resonance[0]),
                (a::THERMAL_DAMAGE_RESONANCE, cs.structure_resonance[1]),
                (a::KINETIC_DAMAGE_RESONANCE, cs.structure_resonance[2]),
                (a::EXPLOSIVE_DAMAGE_RESONANCE, cs.structure_resonance[3]),
            ].into_iter().collect();
            crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(self, ship_id, &mut eff_attrs);
            (cs.max_shield_hp, cs.shield_hp) = (eff_attrs[&a::SHIELD_CAPACITY], eff_attrs[&a::SHIELD_CAPACITY]);
            (cs.max_armor_hp, cs.armor_hp) = (eff_attrs[&a::ARMOR_HP], eff_attrs[&a::ARMOR_HP]);
            (cs.max_structure_hp, cs.structure_hp) = (eff_attrs[&a::HP_STRUCT], eff_attrs[&a::HP_STRUCT]);
            (cs.max_capacitor, cs.capacitor) = (eff_attrs[&a::CAPACITOR_CAPACITY], eff_attrs[&a::CAPACITOR_CAPACITY]);
            cs.signature_radius = eff_attrs[&a::SIGNATURE_RADIUS];
            cs.shield_recharge_time_ms = eff_attrs[&a::SHIELD_RECHARGE_RATE];
            cs.capacitor_recharge_time_ms = eff_attrs[&a::RECHARGE_RATE];
            cs.shield_resonance = [eff_attrs[&a::SHIELD_EM_DAMAGE_RESONANCE], eff_attrs[&a::SHIELD_THERMAL_DAMAGE_RESONANCE], eff_attrs[&a::SHIELD_KINETIC_DAMAGE_RESONANCE], eff_attrs[&a::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE]];
            cs.armor_resonance = [eff_attrs[&a::ARMOR_EM_DAMAGE_RESONANCE], eff_attrs[&a::ARMOR_THERMAL_DAMAGE_RESONANCE], eff_attrs[&a::ARMOR_KINETIC_DAMAGE_RESONANCE], eff_attrs[&a::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE]];
            cs.structure_resonance = [eff_attrs[&a::EM_DAMAGE_RESONANCE], eff_attrs[&a::THERMAL_DAMAGE_RESONANCE], eff_attrs[&a::KINETIC_DAMAGE_RESONANCE], eff_attrs[&a::EXPLOSIVE_DAMAGE_RESONANCE]];
            cs.base_max_shield_hp = Some(cs.max_shield_hp);
            cs.base_max_armor_hp = Some(cs.max_armor_hp);
            cs.base_max_structure_hp = Some(cs.max_structure_hp);
            cs.base_max_capacitor = Some(cs.max_capacitor);
            cs.base_capacitor_recharge_time_ms = Some(cs.capacitor_recharge_time_ms);
            cs.base_signature_radius = Some(cs.signature_radius);
            cs.base_shield_resonance = Some(cs.shield_resonance);
            cs.base_armor_resonance = Some(cs.armor_resonance);
            cs.base_structure_resonance = Some(cs.structure_resonance);
        }
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
            let shield_res = [
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
            ];
            let armor_res = [
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
            ];
            let struct_res = [
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
            ];
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
                base_signature_radius: Some(sig),
                shield_recharge_time_ms: shield_rech,
                capacitor_recharge_time_ms: cap_rech,
                shield_resonance: shield_res,
                armor_resonance: armor_res,
                structure_resonance: struct_res,
                base_max_velocity: a.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().filter(|&v| v > 0.0),
                base_mass: a.get(&eve_sde::attr_ids::MASS).copied().filter(|&v| v > 0.0),
                base_inertia: a.get(&eve_sde::attr_ids::AGILITY).copied().filter(|&v| v > 0.0),
                base_shield_resonance: Some(shield_res),
                base_armor_resonance: Some(armor_res),
                base_structure_resonance: Some(struct_res),
                base_max_shield_hp: Some(shield),
                base_max_armor_hp: Some(armor),
                base_max_structure_hp: Some(hull),
                base_max_capacitor: Some(cap),
                base_capacitor_recharge_time_ms: Some(cap_rech),
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
        let (corp_id, alliance_id, war_faction_id, security_status, raw_skin, custom_name) = self.db.with_snapshot(|snap| {
            let Some(ch) = snap.characters.get(&char_id) else {
                return (1_000_009, 0, None, 0.0, None, None);
            };
            let skin = snap.applied_ship_skins.get(&ch.active_ship_id).copied();
            let ship_name = snap
                .item_custom_names
                .get(&ch.active_ship_id)
                .filter(|n| !n.is_empty() && snap.items.get(&ch.active_ship_id).is_some_and(|i| i.type_id == type_id))
                .cloned();
            (
                ch.corporation_id,
                ch.alliance_id.unwrap_or(0),
                ch.war_faction_id,
                ch.security_status,
                skin,
                ship_name,
            )
        });
        let ship_display_name = custom_name
            .or_else(|| self.sde.types.get(&type_id).map(|t| t.name.clone()).filter(|n| !n.is_empty()));
        let skin_id = raw_skin.map(|raw| {
            self.sde
                .skin_licenses
                .get(&raw)
                .map(|l| l.skin_id)
                .unwrap_or(raw)
        });
        let skin_material_set_id = skin_id
            .and_then(|sid| self.sde.skins.get(&sid))
            .and_then(|sk| self.sde.skin_materials.get(&sk.skin_material_id))
            .and_then(|m| m.material_set_id);
        eve_destiny::SlimItemMeta {
            group_id,
            category_id,
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id,
            alliance_id,
            security_status,
            is_cloaked: false,
            skin_id,
            skin_material_set_id,
            jump_target: None,
            war_faction_id,
            name: ship_display_name,
            ..Default::default()
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
        let instance_id = self.instance_id;
        let systems_w = Arc::downgrade(&self.systems);
        let sde = Arc::clone(&self.sde);
        let db = Arc::clone(&self.db);
        let rng = Arc::clone(&self.mechanic_rng);
        let pending = Arc::clone(&self.pending_notifications);
        let notify_tx = self.notify_tx.clone();
        let ship_fittings = Arc::clone(&self.ship_fittings);
        let combat_ephemeral = Arc::clone(&self.combat_ephemeral);
        let quasar_gateway = Arc::clone(&self.quasar_gateway);
        let inv_roll = Arc::clone(&self.invention_roll_override);
        let cd_map = Arc::clone(&self.module_reactivation_cooldowns);
        let active_fighters = Arc::clone(&self.active_fighters);
        let projected_ewar = Arc::clone(&self.projected_ewar);
        let projected_ewar_sources = Arc::clone(&self.projected_ewar_sources);
        let active_ewar_starts = Arc::clone(&self.active_ewar_starts);
        let tactical_state = Arc::clone(&self.tactical_state);

        self.systems.set_module_deactivation_hook(Arc::new(move |char_id, cycle| {
            ship_fittings
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&cycle.ship_id);

            let is_cloak = sde.types.get(&cycle.module_type_id).is_some_and(|t| t.group_id == 330)
                || cycle.effect_name.to_ascii_lowercase().contains("cloak");
            if is_cloak {
                let reactiv_delay_s = if cycle.reactivation_delay_s > 0.0 {
                    cycle.reactivation_delay_s
                } else {
                    sde.types
                        .get(&cycle.module_type_id)
                        .and_then(|t| t.default_attributes.get(&669).copied())
                        .map(|ms| ms / 1000.0)
                        .unwrap_or(0.0)
                };
                if reactiv_delay_s > 0.0 {
                    let now_s = db.clock().now_unix_secs() as f64;
                    cd_map.write().unwrap_or_else(|e| e.into_inner()).insert(
                        (cycle.ship_id, cycle.module_id, cycle.module_type_id),
                        super::context::ModuleCooldownEntry {
                            recorded_unix_f64: now_s,
                            delay_s: reactiv_delay_s,
                            solar_system_id: None,
                            recorded_sys_sim_s: 0.0,
                        },
                    );
                }
            }

            let solar_system_id = db.with_mut_snapshot(|snap| {
                snap.module_active_states.remove(&cycle.module_id);
                snap.module_active_charges.remove(&cycle.module_id);
                snap.characters.get(&char_id).map(|c| c.solar_system_id)
            });

            if let Some(sys_reg) = systems_w.upgrade() {
                let step_ctx = ServerContext::from_step_components(
                    instance_id,
                    sys_reg,
                    Arc::clone(&db),
                    Arc::clone(&sde),
                    Arc::clone(&rng),
                    Arc::clone(&ship_fittings),
                    Arc::clone(&pending),
                    notify_tx.clone(),
                    Arc::clone(&combat_ephemeral),
                    Arc::clone(&quasar_gateway),
                    Arc::clone(&inv_roll),
                    Arc::clone(&cd_map),
                    Arc::clone(&active_fighters),
                    Arc::clone(&projected_ewar),
                    Arc::clone(&projected_ewar_sources),
                    Arc::clone(&active_ewar_starts),
                    Arc::clone(&tactical_state),
                );
                let _ = crate::services::notify_emitters_space_dungeons::flush_module_profile_and_ewar_end(
                    &step_ctx,
                    char_id,
                    solar_system_id,
                    cycle.ship_id,
                    cycle.module_id,
                );
                crate::services::tactical_modules::remove_projected_remote_assistance(
                    &step_ctx,
                    cycle.ship_id,
                    cycle.module_id,
                );
            }

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
                seq_id: crate::router::cache_notifications::next_notification_seq_id(),
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

            if sde.types.get(&cycle.module_type_id).is_some_and(|t| t.group_id == 330)
                || cycle.effect_name.to_ascii_lowercase().contains("cloak")
            {
                let sfx_args = vec![
                    PyRep::Integer(cycle.ship_id),
                    PyRep::Integer(cycle.module_id),
                    PyRep::Integer(cycle.module_type_id as i64),
                    PyRep::None,
                    PyRep::None,
                    PyRep::String("effects.Cloak".to_string()),
                    PyRep::Bool(false),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Real(cycle.duration_s * 1000.0),
                    PyRep::Integer(0),
                    PyRep::Integer(now),
                    PyRep::Real(0.0),
                    PyRep::None,
                ];
                let (sfx_pkt, sfx_bytes) =
                    ServerContext::build_macho_notification_packet(char_id, user_id, "OnSpecialFX", "charid", sfx_args.clone());
                let sfx_notif = PendingMachoNotification {
                    seq_id: crate::router::cache_notifications::next_notification_seq_id(),
                    recipient_char_id: char_id,
                    event: "OnSpecialFX".to_string(),
                    id_type: "charid".to_string(),
                    args: sfx_args,
                    packet: sfx_pkt,
                    marshaled_packet: sfx_bytes,
                    created_filetime: now,
                };
                {
                    let mut q_guard = pending.write().unwrap_or_else(|e| e.into_inner());
                    let q = q_guard.entry(char_id).or_default();
                    if q.len() >= 512 {
                        q.remove(0);
                    }
                    q.push(sfx_notif.clone());
                }
                let _ = notify_tx.send((char_id, sfx_notif));
            }

            let auto_reload_ev: Option<(Vec<i64>, u32, f64)> = db.with_mut_snapshot(|snap| {
                snap.cluster_and_runtime
                    .auto_reloaded_event
                    .remove(&cycle.module_id)
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
                    seq_id: crate::router::cache_notifications::next_notification_seq_id(),
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

        let db_lock = Arc::clone(&self.db);
        let pending_lock = Arc::clone(&self.pending_notifications);
        let notify_tx_lock = self.notify_tx.clone();
        self.systems.set_target_lock_completed_hook(Arc::new(move |char_id, target_id| {
            let now = db_lock.clock().now_blueos_filetime_ticks();
            let enqueue_target_ev = |rcpt_char: i64, rcpt_user: Option<u64>, sub_ev: &str, item_id: i64| {
                let args = vec![PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Tuple(vec![
                        PyRep::String("OnTarget".into()),
                        PyRep::String(sub_ev.into()),
                        PyRep::Integer(item_id),
                        PyRep::String(String::new()),
                    ]),
                    PyRep::Integer(0),
                ])])];
                let (packet, marshaled_packet) =
                    ServerContext::build_macho_notification_packet(rcpt_char, rcpt_user, "OnMultiEvent", "charid", args.clone());
                let notif = PendingMachoNotification {
                    seq_id: crate::router::cache_notifications::next_notification_seq_id(),
                    recipient_char_id: rcpt_char,
                    event: "OnMultiEvent".to_string(),
                    id_type: "charid".to_string(),
                    args,
                    packet,
                    marshaled_packet,
                    created_filetime: now,
                };
                {
                    let mut q_guard = pending_lock.write().unwrap_or_else(|e| e.into_inner());
                    let q = q_guard.entry(rcpt_char).or_default();
                    if q.len() >= 512 {
                        q.remove(0);
                    }
                    q.push(notif.clone());
                }
                let _ = notify_tx_lock.send((rcpt_char, notif));
            };
            let hook_info = db_lock.with_snapshot(|s| {
                let locker = s.characters.get(&char_id)?;
                if locker.station_id.is_some() { return None; }
                let target_info = s.items.get(&target_id).map(|i| i.owner_id)
                    .filter(|&oid| oid != char_id && s.characters.contains_key(&oid))
                    .and_then(|target_char| {
                        let target = s.characters.get(&target_char)?;
                        if target.station_id.is_some() || locker.solar_system_id != target.solar_system_id {
                            return None;
                        }
                        let locker_ship = crate::services::station_structure::structure_upwell_ops::load_ext_state(s)
                            .active_pilot_by_structure
                            .iter()
                            .find_map(|(&sid, &pid)| (pid == char_id).then_some(sid))
                            .or_else(|| (locker.active_ship_id > 0).then_some(locker.active_ship_id))
                            .unwrap_or(char_id);
                        Some((target_char, Some(target.user_id), locker_ship))
                    });
                Some((Some(locker.user_id), target_info))
            });
            if let Some((locker_user, target_owner_info)) = hook_info {
                enqueue_target_ev(char_id, locker_user, "add", target_id);
                if let Some((target_char, target_user, locker_ship)) = target_owner_info {
                    enqueue_target_ev(target_char, target_user, "otheradd", locker_ship);
                }
            }
        }));
    }

    /// Constructs a lightweight `ServerContext` handle referencing shared core components for system step execution.
    #[allow(clippy::too_many_arguments)]
    pub fn from_step_components(
        instance_id: u64,
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
        active_fighters: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<i64, super::context::ActiveFighterSquadron>>>,
        projected_ewar: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<i64, Vec<eve_dogma::ProjectedEwarEffect>>>>,
        projected_ewar_sources: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<i64, i64>>>,
        active_ewar_starts: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<(i64, i64), Vec<(i64, &'static str)>>>>,
        tactical_state: std::sync::Arc<std::sync::RwLock<crate::services::tactical_modules::TacticalModulesState>>,
    ) -> Self {
        let crimewatch = crate::combat::crimewatch::CrimeWatchLock::new(std::sync::Arc::clone(&db));
        let clock = std::sync::RwLock::new(db.clock());
        let living_world = std::sync::Arc::new(crate::living_world::LivingWorldDirector::new());
        let esi_state = std::sync::Arc::new(crate::esi::runtime_state::EsiRuntimeState::new());
        let cluster_state = std::sync::Arc::new(std::sync::RwLock::new(crate::services::macho_cluster_mgmt::MachoClusterState::default()));
        Self {
            instance_id,
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
            projected_ewar,
            projected_ewar_sources,
            active_ewar_starts,
            weapon_cycle_state: std::sync::RwLock::new(std::collections::HashMap::new()),
            fighter_cooldown_state: std::sync::RwLock::new(std::collections::HashMap::new()),
            fighter_charge_state: std::sync::RwLock::new(std::collections::HashMap::new()),
            active_fighters,
            fighter_tube_loads: std::sync::RwLock::new(std::collections::HashMap::new()),
            active_scanner_probes: std::sync::RwLock::new(std::collections::HashMap::new()),
            active_moon_probes: std::sync::RwLock::new(Vec::new()),
            scanned_signatures: std::sync::RwLock::new(std::collections::HashSet::new()),
            character_scan_progress: std::sync::RwLock::new(std::collections::HashMap::new()),
            invention_roll_override,
            mechanic_rng,
            module_reactivation_cooldowns,
            mining_cycles: std::sync::RwLock::new(std::collections::HashMap::new()),
            phased_asteroid_fields: std::sync::RwLock::new(std::collections::HashMap::new()),
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
            cluster_misc_state: std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
            cluster_state,
            global_config_values: std::sync::RwLock::new(std::collections::HashMap::new()),
            alert_telemetry: std::sync::Mutex::new(Default::default()),
            flight_recorder: std::sync::Arc::new(crate::flight_recorder::FlightRecorder::new()),
            bug_reporting: std::sync::Arc::new(crate::bug_reporting::BugReportingRuntime::new()),
            npc_spawns: std::sync::RwLock::new(std::collections::HashMap::new()),
            combat_rng_seq: std::sync::atomic::AtomicU64::new(0),
            cluster_tick_last_unix: std::sync::atomic::AtomicU64::new(0),
            cluster_tick_persisted_unix: std::sync::atomic::AtomicU64::new(0),
            dedicated_physics_ticker_active: std::sync::atomic::AtomicBool::new(false),
            fw_idle_last_step_unix: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub fn upsert_projected_ewar_on_ship(
        &self,
        source_ship_id: i64,
        target_ship_id: i64,
        effect: eve_dogma::ProjectedEwarEffect,
    ) {
        self.projected_ewar_sources
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(effect.source_module_id, source_ship_id);
        {
            let mut guard = self.projected_ewar.write().unwrap_or_else(|e| e.into_inner());
            let list = guard.entry(target_ship_id).or_default();
            if let Some(existing) = list
                .iter_mut()
                .find(|e| e.source_module_id == effect.source_module_id)
            {
                *existing = effect.clone();
            } else {
                list.push(effect.clone());
            }
        }
        if let Some(fit) = self
            .ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&target_ship_id)
        {
            fit.upsert_projected_ewar(effect, &self.sde);
        }
    }

    pub fn remove_projected_ewar_by_module(&self, source_ship_id: i64, source_module_id: i64) -> Vec<i64> {
        {
            let mut owners = self.projected_ewar_sources.write().unwrap_or_else(|e| e.into_inner());
            if let Some(&recorded_ship) = owners.get(&source_module_id) {
                if recorded_ship != source_ship_id {
                    return Vec::new();
                }
                owners.remove(&source_module_id);
            } else if !self.db.with_snapshot(|s| s.items.get(&source_module_id).is_some_and(|i| i.location_id == source_ship_id)) {
                return Vec::new();
            }
        }
        let mut affected_ships = Vec::new();
        {
            let mut guard = self.projected_ewar.write().unwrap_or_else(|e| e.into_inner());
            for (&ship_id, list) in guard.iter_mut() {
                let before = list.len();
                list.retain(|e| e.source_module_id != source_module_id);
                if list.len() < before {
                    affected_ships.push(ship_id);
                }
            }
        }
        if !affected_ships.is_empty() {
            let mut fit_guard = self
                .ship_fittings
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for &ship_id in &affected_ships {
                if let Some(fit) = fit_guard.get_mut(&ship_id) {
                    fit.remove_projected_ewar(source_module_id, &self.sde);
                }
            }
        }
        affected_ships
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

    /// Computes ONLY the stacking-penalized Target Painter signature radius multiplier (`>= 1.0`)
    /// from active projected EWAR on `target_ball_id` (`#584`).
    pub fn effective_target_painter_multiplier(&self, target_ball_id: i64) -> f64 {
        let tp_resist = self.db.with_snapshot(|snap| {
            snap.items
                .get(&target_ball_id)
                .and_then(|i| self.sde.types.get(&i.type_id))
                .and_then(|t| t.default_attributes.get(&2114).copied())
        }).unwrap_or(1.0).clamp(0.0, 1.0);
        let bonuses: Vec<f64> = self
            .projected_ewar
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&target_ball_id)
            .map(|list| {
                list.iter()
                    .filter(|proj| {
                        self.sde
                            .types
                            .get(&proj.source_module_type_id)
                            .is_some_and(|t| t.group_id == 379)
                            || proj.effective_module.base_attributes.contains_key(&554)
                    })
                    .map(|proj| {
                        let opt = proj
                            .effective_module
                            .get_attribute(eve_sde::attr_ids::MAX_RANGE, &self.sde);
                        let fall = eve_dogma::get_module_falloff(&proj.effective_module, &self.sde);
                        let att =
                            eve_dogma::compute_falloff_attenuation(proj.distance, opt, fall);
                        proj.effective_module.get_attribute(554, &self.sde) * att * tp_resist
                    })
                    .collect()
            })
            .unwrap_or_default();
        if bonuses.is_empty() {
            1.0
        } else {
            eve_dogma::eval_target_painters_sig_radius(1.0, &bonuses)
        }
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

pub use crate::router::binding_effects::effect_id_for_module_sde;

#[cfg(test)] #[path = "binding_unit_tests.rs"] mod binding_unit_tests;
