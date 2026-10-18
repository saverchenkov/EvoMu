//! `router` — Authenticated Asynchronous `machoNet` & Client Bridge RPC Service Router (`M1`–`M8`).
//!
//! Routes cataloged `machoNet` RPC services and all 135 Quasar Protobuf domains across the
//! 12 modular domain handlers in `crate::services::*`.

#![forbid(unsafe_code)]

use crate::living_world::LivingWorldDirector;
use crate::services;
use crate::system_actor::SystemRegistry;
use crate::world_seeder;
use eve_db::WorldRepository;
use eve_dogma::ShipFittingEngine;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tracing::{info, warn};

static NEXT_BOUND_ID: AtomicU64 = AtomicU64::new(10_000);

const EMBEDDED_RPC_CATALOG_JSON: &str =
    include_str!("../../../docs/index/rpc_service_catalog.json");
const EMBEDDED_QUASAR_CATALOG_JSON: &str =
    include_str!("../../../docs/index/quasar_protobuf_catalog.json");

#[derive(Debug, Clone)]
pub struct BoundObjectEntry {
    pub bound_id: u64,
    pub session_id: u64,
    pub character_id: i64,
    pub service: String,
    pub object_id: i64,
}

pub struct ServerContext {
    pub sde: Arc<SdeRegistry>,
    pub db: Arc<WorldRepository>,
    pub systems: Arc<SystemRegistry>,
    pub living_world: Arc<LivingWorldDirector>,
    pub cataloged_services: HashSet<String>,
    pub quasar_domains: HashSet<String>,
    pub bound_objects: RwLock<HashMap<u64, BoundObjectEntry>>,
    pub ship_fittings: RwLock<HashMap<i64, ShipFittingEngine>>,
    pub weapon_cycle_state: RwLock<HashMap<i64, (Option<u64>, u32)>>,
    pub fighter_cooldown_state: RwLock<HashMap<(i64, u32), u64>>,
    pub scanned_signatures: RwLock<HashSet<(i64, u32, i64)>>,
    pub virtual_clock_offset_sec: AtomicU64,
    pub invention_roll_override: RwLock<Option<f64>>,
}

impl Default for ServerContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerContext {
    /// Initializes a `ServerContext` with the provided `WorldRepository` and seeds the NPC regional markets.
    pub fn with_repository(db: Arc<WorldRepository>) -> Self {
        let sde = Arc::new(SdeRegistry::with_canonical_seed());
        let _ = world_seeder::seed_dual_anchor_markets(&db, &sde);

        let catalog_map: HashMap<String, serde_json::Value> =
            serde_json::from_str(EMBEDDED_RPC_CATALOG_JSON).unwrap_or_default();
        let cataloged_services: HashSet<String> = catalog_map.into_keys().collect();

        let quasar_map: HashMap<String, serde_json::Value> =
            serde_json::from_str(EMBEDDED_QUASAR_CATALOG_JSON).unwrap_or_default();
        let quasar_domains: HashSet<String> = quasar_map.into_keys().collect();

        Self {
            sde,
            db,
            systems: Arc::new(SystemRegistry::new()),
            living_world: Arc::new(LivingWorldDirector::new_seeded()),
            cataloged_services,
            quasar_domains,
            bound_objects: RwLock::new(HashMap::new()),
            ship_fittings: RwLock::new(HashMap::new()),
            weapon_cycle_state: RwLock::new(HashMap::new()),
            fighter_cooldown_state: RwLock::new(HashMap::new()),
            scanned_signatures: RwLock::new(HashSet::new()),
            virtual_clock_offset_sec: AtomicU64::new(0),
            invention_roll_override: RwLock::new(None),
        }
    }

    /// Initializes the production server context (`R22-P1-5`): starts with an empty account/character table
    /// (`WorldRepository::new_empty()`) unless `EVOMU_ENABLE_DEMO_SEED=1` or `--demo-seed` is explicitly set,
    /// and provisions operator-supplied player accounts from `EVOMU_ACCOUNTS` (`user:secret,...`).
    pub fn new_production() -> Self {
        let enable_demo = std::env::var("EVOMU_ENABLE_DEMO_SEED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
            || std::env::args().any(|a| a == "--demo-seed");
        let db = if enable_demo {
            Arc::new(WorldRepository::with_demo_seed())
        } else {
            Arc::new(WorldRepository::new_empty())
        };
        if let Ok(accounts_spec) = std::env::var("EVOMU_ACCOUNTS") {
            for entry in accounts_spec.split(',') {
                if let Some((user, pass)) = entry.trim().split_once(':') {
                    let u = user.trim();
                    let p = pass.trim();
                    if !u.is_empty() && !p.is_empty() {
                        db.provision_player_account(u, p);
                    }
                }
            }
        }
        Self::with_repository(db)
    }

    pub fn new() -> Self {
        let disable_demo = std::env::var("EVOMU_DISABLE_DEMO_SEED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let db = if disable_demo {
            Arc::new(WorldRepository::new_empty())
        } else {
            Arc::new(WorldRepository::with_demo_seed())
        };
        Self::with_repository(db)
    }

    pub fn mark_signature_scanned(&self, char_id: i64, solar_system_id: u32, sig_ball_id: i64) {
        self.scanned_signatures
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((char_id, solar_system_id, sig_ball_id));
    }

    pub fn has_scanned_signature(
        &self,
        char_id: i64,
        solar_system_id: u32,
        sig_ball_id: i64,
    ) -> bool {
        self.scanned_signatures
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(char_id, solar_system_id, sig_ball_id))
    }

    pub fn character_skill_levels(&self, char_id: i64) -> HashMap<u32, u8> {
        self.db.with_snapshot(|snap| {
            snap.skills
                .get(&char_id)
                .map(|m| m.iter().map(|(&tid, rec)| (tid, rec.level)).collect())
                .unwrap_or_default()
        })
    }

    pub fn set_invention_roll_override(&self, roll: Option<f64>) {
        *self
            .invention_roll_override
            .write()
            .unwrap_or_else(|e| e.into_inner()) = roll.map(|r| r.clamp(0.0, 0.999_999));
    }

    pub fn next_invention_roll(&self, job_id: u64, installer_id: u64, now_unix: u64) -> f64 {
        if let Some(r) = *self
            .invention_roll_override
            .read()
            .unwrap_or_else(|e| e.into_inner())
        {
            return r;
        }
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 8];
            if std::io::Read::read_exact(&mut f, &mut buf).is_ok() {
                let raw = u64::from_le_bytes(buf) >> 11;
                return (raw as f64) / ((1u64 << 53) as f64);
            }
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(now_unix);
        let mut z = job_id
            .wrapping_add(installer_id.wrapping_mul(0x517c_c1b7_2722_0a95))
            .wrapping_add(nanos)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        ((z >> 11) as f64) / ((1u64 << 53) as f64)
    }

    pub fn now_unix(&self) -> u64 {
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(1_760_000_000);
        base.saturating_add(self.virtual_clock_offset_sec.load(Ordering::Relaxed))
    }

    pub fn advance_virtual_clock(&self, secs: u64) {
        self.virtual_clock_offset_sec
            .fetch_add(secs, Ordering::Relaxed);
    }

    pub fn now_blueos_filetime_ticks(&self) -> i64 {
        ((self.now_unix() as i64) + 11_644_473_600) * 10_000_000
    }

    pub fn invalidate_ship_fitting(&self, ship_id: i64) {
        self.ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
    }

    pub fn store_ship_fitting(&self, ship_id: i64, fit: ShipFittingEngine) {
        self.ship_fittings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ship_id, fit);
    }

    pub fn get_or_rebuild_ship_fitting(
        &self,
        char_id: i64,
        ship_id: i64,
    ) -> Result<ShipFittingEngine, BridgeRpcError> {
        let skill_levels = self.character_skill_levels(char_id);
        if let Some(mut existing) = self
            .ship_fittings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ship_id)
            .cloned()
        {
            existing.set_character_skills(skill_levels);
            return Ok(existing);
        }

        let items = self.db.get_items_for_character(char_id);
        let Some(ship_item) = items.iter().find(|i| i.item_id == ship_id) else {
            return Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: format!("Ship item {ship_id} not found for character {char_id}"),
            });
        };

        let mut fit = ShipFittingEngine::from_sde(ship_id, ship_item.type_id, &self.sde)
            .with_character_skills(skill_levels);
        for item in &items {
            let is_module_or_subsystem = self
                .sde
                .types
                .get(&item.type_id)
                .map(|t| t.category_id == 7 || t.category_id == 32)
                .unwrap_or(false);
            if item.location_id == ship_id && (item.flag as u16) >= 11 && is_module_or_subsystem {
                let _ = fit.fit_and_online_module(
                    item.item_id,
                    item.type_id,
                    item.flag as u16,
                    &self.sde,
                );
            }
        }
        self.store_ship_fitting(ship_id, fit.clone());
        Ok(fit)
    }

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
        for (req_skill_attr, req_lvl_attr) in [(182_u16, 277_u16), (183, 278), (184, 279)] {
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
                    let trained = char_skills
                        .and_then(|m| m.get(&skill_id))
                        .map(|r| r.level)
                        .unwrap_or(0);
                    if trained < req_lvl {
                        return Err(BridgeRpcError {
                            code: "SkillPrerequisitesNotMet".into(),
                            message: format!(
                                "{} (type {type_id}) requires skill {skill_id} at level {req_lvl} (character has level {trained})",
                                t.name
                            ),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    pub fn is_charge_compatible_with_weapon(
        weapon_type: &eve_sde::InvType,
        charge_type: &eve_sde::InvType,
    ) -> bool {
        if charge_type.category_id != 8 {
            return false;
        }
        let req_charge_group = weapon_type
            .default_attributes
            .get(&604)
            .map(|g| *g as u32)
            .unwrap_or(0);
        let group_ok = (req_charge_group > 0 && charge_type.group_id == req_charge_group)
            || charge_type
                .default_attributes
                .get(&137)
                .map(|lg| *lg as u32 == weapon_type.group_id)
                .unwrap_or(false);
        let size_ok = match (
            weapon_type.default_attributes.get(&128).copied(),
            charge_type.default_attributes.get(&128).copied(),
        ) {
            (Some(rs), Some(cs)) => (rs - cs).abs() < 0.5,
            _ => true,
        };
        group_ok && size_ok
    }

    pub fn weapon_slot_max_charge_capacity(
        weapon_type: &eve_sde::InvType,
        charge_type: &eve_sde::InvType,
    ) -> i32 {
        let cap_m3 = if weapon_type.capacity > 0.0 {
            weapon_type.capacity
        } else {
            5.0
        };
        let vol_m3 = charge_type.volume.max(1e-6);
        ((cap_m3 / vol_m3).floor() as i32).max(1)
    }

    /// Loads or swaps an ammunition/missile charge stack into a fitted high-slot weapon (`flag 27..=34`),
    /// enforcing weapon/charge group & size compatibility, character skill prerequisites (`182..=184`/`277..=279`),
    /// and `floor(weapon.capacity / charge.volume)` clip bounds (splitting oversized stacks and unloading
    /// any previously loaded charge of a different type back into `ship_id` `Cargo`).
    pub fn load_charge_into_weapon_slot(
        &self,
        char_id: i64,
        ship_id: i64,
        flag: eve_db::InventoryFlag,
        charge_item_id: i64,
        current_dock: Option<i64>,
    ) -> Result<(i64, u32, i32, u64), BridgeRpcError> {
        self.db.try_with_mut_snapshot(|snap| {
            self.load_charge_into_weapon_slot_in_snapshot(
                snap,
                char_id,
                ship_id,
                flag,
                charge_item_id,
                current_dock,
            )
        })
    }

    pub fn load_charge_into_weapon_slot_in_snapshot(
        &self,
        snap: &mut eve_db::WorldStateSnapshot,
        char_id: i64,
        ship_id: i64,
        flag: eve_db::InventoryFlag,
        charge_item_id: i64,
        current_dock: Option<i64>,
    ) -> Result<(i64, u32, i32, u64), BridgeRpcError> {
        let flag_u16 = flag as u16;
        if !(27..=34).contains(&flag_u16) {
            return Err(BridgeRpcError {
                code: "InvalidWeaponSlot".into(),
                message: format!("Charges can only be loaded into high weapon slots 27..=34 (got {flag_u16})"),
            });
        }
        {
            let Some(ship_item) = snap.items.get(&ship_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Target ship {ship_id} not found"),
                });
            };
            let is_ship = self
                .sde
                .types
                .get(&ship_item.type_id)
                .map(|t| t.category_id == 6)
                .unwrap_or(false);
            if ship_item.owner_id != char_id
                || !is_ship
                || !snap.is_ship_colocated_with_character(char_id, ship_id, current_dock)
            {
                return Err(BridgeRpcError {
                    code: "InvalidFittingTarget".into(),
                    message: format!("Target {ship_id} is not an owned co-located ship"),
                });
            }

            let weapon_type_id = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && i.flag == flag
                        && self
                            .sde
                            .types
                            .get(&i.type_id)
                            .map(|t| t.category_id == 7)
                            .unwrap_or(false)
                })
                .map(|i| i.type_id);
            let Some(wtid) = weapon_type_id else {
                return Err(BridgeRpcError {
                    code: "NoWeaponFitted".into(),
                    message: format!("High slot {flag_u16} on ship {ship_id} has no fitted weapon module"),
                });
            };
            let Some(weapon_type) = self.sde.types.get(&wtid) else {
                return Err(BridgeRpcError {
                    code: "UnknownWeaponType".into(),
                    message: format!("Unknown weapon type {wtid}"),
                });
            };

            let Some(src_charge) = snap.items.get(&charge_item_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Charge item {charge_item_id} not found"),
                });
            };
            if src_charge.location_id == ship_id && src_charge.flag == flag {
                return Ok((
                    src_charge.item_id,
                    src_charge.type_id,
                    src_charge.quantity,
                    src_charge.version,
                ));
            }
            if !snap.is_item_consumable_by(&src_charge, char_id, current_dock, Some(ship_id)) {
                return Err(BridgeRpcError {
                    code: "AmmoNotReachable".into(),
                    message: format!(
                        "Charge item {charge_item_id} must be owned, unlocked, and in ship {ship_id} Cargo or current station Hangar"
                    ),
                });
            }
            let Some(charge_type) = self.sde.types.get(&src_charge.type_id) else {
                return Err(BridgeRpcError {
                    code: "UnknownChargeType".into(),
                    message: format!("Unknown charge type {}", src_charge.type_id),
                });
            };
            if !Self::is_charge_compatible_with_weapon(weapon_type, charge_type) {
                return Err(BridgeRpcError {
                    code: "IncompatibleAmmo".into(),
                    message: format!(
                        "Charge {} (type {}) is not compatible with weapon {} (type {wtid})",
                        charge_type.name, src_charge.type_id, weapon_type.name
                    ),
                });
            }
            self.check_type_skill_prerequisites_in_snapshot(snap, char_id, src_charge.type_id)?;

            let max_cap = Self::weapon_slot_max_charge_capacity(weapon_type, charge_type);

            // Check if a charge is already in `(ship_id, flag)`
            let existing_loaded: Option<(i64, u32, i32)> = snap
                .items
                .values()
                .find(|i| {
                    i.item_id != charge_item_id
                        && i.location_id == ship_id
                        && i.flag == flag
                        && self
                            .sde
                            .types
                            .get(&i.type_id)
                            .map(|t| t.category_id == 8)
                            .unwrap_or(false)
                })
                .map(|i| (i.item_id, i.type_id, i.quantity));

            let mut current_same_id_qty: Option<(i64, i32)> = None;
            if let Some((ex_id, ex_type_id, ex_qty)) = existing_loaded {
                if ex_type_id == src_charge.type_id {
                    current_same_id_qty = Some((ex_id, ex_qty));
                } else {
                    // ChangeAmmo: unload previous charge stack back to `(ship_id, Cargo)`
                    let merge_cargo_id = snap
                        .items
                        .values()
                        .find(|i| {
                            i.item_id != ex_id
                                && i.owner_id == char_id
                                && i.location_id == ship_id
                                && i.flag == eve_db::InventoryFlag::Cargo
                                && i.type_id == ex_type_id
                                && !i.singleton
                        })
                        .map(|i| i.item_id);
                    if let Some(mc_id) = merge_cargo_id {
                        snap.items.remove(&ex_id);
                        if let Some(mc) = snap.items.get_mut(&mc_id) {
                            mc.quantity = mc.quantity.saturating_add(ex_qty);
                            mc.version += 1;
                        }
                    } else if let Some(ex_item) = snap.items.get_mut(&ex_id) {
                        ex_item.location_id = ship_id;
                        ex_item.flag = eve_db::InventoryFlag::Cargo;
                        ex_item.singleton = false;
                        ex_item.version += 1;
                    }
                }
            }

            let cur_qty = current_same_id_qty.map(|(_, q)| q).unwrap_or(0);
            let room = (max_cap - cur_qty).max(0);
            if room == 0 {
                let (ex_id, ex_q) = current_same_id_qty.unwrap();
                let ver = snap.items.get(&ex_id).map(|i| i.version).unwrap_or(1);
                return Ok((ex_id, src_charge.type_id, ex_q, ver));
            }
            let to_load = src_charge.quantity.min(room);

            if let Some((ex_id, _)) = current_same_id_qty {
                if src_charge.quantity > to_load {
                    if let Some(src_mut) = snap.items.get_mut(&charge_item_id) {
                        src_mut.quantity -= to_load;
                        src_mut.version += 1;
                    }
                } else {
                    snap.items.remove(&charge_item_id);
                }
                let ex_mut = snap.items.get_mut(&ex_id).unwrap();
                ex_mut.quantity += to_load;
                ex_mut.version += 1;
                Ok((ex_id, ex_mut.type_id, ex_mut.quantity, ex_mut.version))
            } else if src_charge.quantity > to_load {
                if let Some(src_mut) = snap.items.get_mut(&charge_item_id) {
                    src_mut.quantity -= to_load;
                    src_mut.version += 1;
                }
                snap.next_entity_id += 1;
                let loaded_id = snap.next_entity_id;
                snap.items.insert(
                    loaded_id,
                    eve_db::InventoryItemRecord {
                        item_id: loaded_id,
                        type_id: src_charge.type_id,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: to_load,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                Ok((loaded_id, src_charge.type_id, to_load, 1))
            } else {
                let src_mut = snap.items.get_mut(&charge_item_id).unwrap();
                src_mut.location_id = ship_id;
                src_mut.flag = flag;
                src_mut.singleton = false;
                src_mut.version += 1;
                Ok((
                    src_mut.item_id,
                    src_mut.type_id,
                    src_mut.quantity,
                    src_mut.version,
                ))
            }
        }
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
                ..eve_destiny::CombatState::default()
            };
        }
        self.build_type_combat_state(type_id)
    }

    /// Builds a `CombatState` directly from a type's SDE `default_attributes` (for NPCs, wrecks, or fallback hulls).
    pub fn build_type_combat_state(&self, type_id: u32) -> eve_destiny::CombatState {
        if let Some(t) = self.sde.types.get(&type_id) {
            let a = &t.default_attributes;
            let shield = a
                .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                .copied()
                .unwrap_or(50.0);
            let armor = a
                .get(&eve_sde::attr_ids::ARMOR_HP)
                .copied()
                .unwrap_or(50.0);
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
                ..eve_destiny::CombatState::default()
            };
        }
        eve_destiny::CombatState::default()
    }

    /// Releases all bound objects associated with `session_id` (called on disconnect or re-login).
    pub fn release_session_bindings(&self, session_id: u64) {
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, v| v.session_id != session_id);
    }

    /// Binds a stateful remote object scoped to `(session_id, character_id)`.
    pub fn bind_object(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
    ) -> Result<u64, BridgeRpcError> {
        let char_id = session.character_id.ok_or_else(|| BridgeRpcError {
            code: "CharacterNotSelected".into(),
            message: "Cannot bind object before selecting a character".into(),
        })?;

        let authorized = match service {
            "beyonce" => session.solar_system_id.map(|s| s as i64) == Some(object_id),
            "dogmaIM" | "ship" => {
                session.ship_id == Some(object_id)
                    || session.station_id == Some(object_id)
                    || session.solar_system_id.map(|s| s as i64) == Some(object_id)
            }
            "invbroker" => {
                session.station_id == Some(object_id)
                    || session.structure_id == Some(object_id)
                    || session.ship_id == Some(object_id)
            }
            "station" | "corpStationMgr" | "repairSvc" | "reprocessingSvc" | "insuranceSvc" => {
                session.station_id == Some(object_id) || session.structure_id == Some(object_id)
            }
            "skillMgr2" | "charMgr" | "jumpCloneSvc" => {
                Some(object_id) == session.character_id || object_id == 0
            }
            "agentMgr" | "keeper" | "planetMgr" | "posMgr" | "fleetObjectHandler"
            | "corpRegistry" | "allianceRegistry" | "warRegistry" | "warStatisticMgr"
            | "trademgr" | "voucher" => object_id > 0,
            _ => false,
        };

        if !authorized {
            return Err(BridgeRpcError {
                code: "BindAuthorizationDenied".into(),
                message: format!(
                    "Service '{service}' cannot be bound to object_id {object_id} for current session state"
                ),
            });
        }

        let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
        self.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                bound_id,
                BoundObjectEntry {
                    bound_id,
                    session_id: session.session_id,
                    character_id: char_id,
                    service: service.to_string(),
                    object_id,
                },
            );
        Ok(bound_id)
    }

    /// Verifies that `bound_id` exists and belongs to the caller's `(session_id, character_id)`.
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

        if entry.session_id != session.session_id
            || Some(entry.character_id) != session.character_id
        {
            return Err(BridgeRpcError {
                code: "BoundObjectOwnershipViolation".into(),
                message: format!("Bound object {bound_id} is not owned by this session"),
            });
        }
        Ok(entry)
    }

    pub fn require_auth(session: &SessionState) -> Result<u64, BridgeRpcError> {
        session.user_id.ok_or_else(|| BridgeRpcError {
            code: "Unauthorized".into(),
            message: "Authentication required before calling this service".into(),
        })
    }

    pub fn require_character(session: &SessionState) -> Result<(u64, i64), BridgeRpcError> {
        let uid = Self::require_auth(session)?;
        let cid = session.character_id.ok_or_else(|| BridgeRpcError {
            code: "CharacterNotSelected".into(),
            message: "Character selection required before calling this service".into(),
        })?;
        Ok((uid, cid))
    }

    pub fn require_bound_system(
        bound_entry: Option<&BoundObjectEntry>,
        expected_service: &str,
    ) -> Result<SolarSystemId, BridgeRpcError> {
        let entry = bound_entry.ok_or_else(|| BridgeRpcError {
            code: "BoundObjectRequired".into(),
            message: format!("Calls to {expected_service} require a bound object handle"),
        })?;
        if entry.service != expected_service {
            return Err(BridgeRpcError {
                code: "InvalidBoundService".into(),
                message: format!(
                    "Expected bound service {expected_service}, got {}",
                    entry.service
                ),
            });
        }
        Ok(entry.object_id as SolarSystemId)
    }

    /// Dispatches an incoming RPC call `(service, bound_id, method, args, kwargs)` across the 12
    /// modular domain handlers, enforcing authentication and ownership checks.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch_rpc(
        &self,
        session: &mut SessionState,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
        caller_trace: Option<&str>,
    ) -> Result<PyRep, BridgeRpcError> {
        info!(
            session_id = session.session_id,
            user_id = ?session.user_id,
            char_id = ?session.character_id,
            service,
            ?bound_id,
            method,
            "Dispatching RPC call"
        );

        // Synchronize active character's ship/station/structure/solar-system state from DB
        if let Some(cid) = session.character_id {
            if let Ok(ch) = self.db.get_character(cid) {
                session.ship_id = Some(ch.active_ship_id);
                session.station_id = ch.station_id;
                session.structure_id = ch.structure_id;
                session.solar_system_id = Some(ch.solar_system_id);
                session.constellation_id = Some(ch.constellation_id);
                session.region_id = Some(ch.region_id);
            }
        }

        let bound_entry = if let Some(bid) = bound_id {
            Some(self.verify_bound_object(session, bid)?)
        } else {
            None
        };

        let effective_service = bound_entry
            .as_ref()
            .map(|b| b.service.as_str())
            .unwrap_or(service);

        // Validate Quasar Protobuf Public Gateway (`publicGatewaySvc`) domains and return explicit UnimplementedRPC
        if effective_service == "publicGatewaySvc" {
            let _ = Self::require_character(session)?;
            if matches!(method, "SendRequest" | "PublishEvent" | "CallQuasar") {
                let domain = args
                    .first()
                    .and_then(|a| a.as_str())
                    .unwrap_or("eve_public.character");
                if !self.quasar_domains.contains(domain) {
                    return Err(BridgeRpcError {
                        code: "UnknownQuasarDomain".into(),
                        message: format!(
                            "Quasar domain '{domain}' is not in quasar_protobuf_catalog.json"
                        ),
                    });
                }
                let msg_type = args
                    .get(1)
                    .and_then(|a| a.as_str())
                    .unwrap_or("GetRequest");
                append_unimplemented_rpc_log(
                    session.session_id,
                    effective_service,
                    method,
                    args.len(),
                    kwargs.keys().map(|s| s.as_str()).collect(),
                    caller_trace,
                );
                return Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: format!(
                        "publicGatewaySvc.{method}({domain} / {msg_type}) is not yet implemented"
                    ),
                });
            }
        }

        // Handle universal `MachoBindObject` + optional piggybacked RPC call
        if method == "MachoBindObject" {
            let _ = Self::require_character(session)?;
            let obj_id = match args.first() {
                Some(PyRep::Integer(id)) => *id,
                Some(PyRep::String(s)) | Some(PyRep::Token(s)) if !s.is_empty() => {
                    s.bytes().fold(1_000i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64)).abs().max(1)
                }
                Some(PyRep::Tuple(items)) => {
                    if let Some(id) = items.first().and_then(|v| v.as_i64()) {
                        id
                    } else if let Some(s) = items.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                        s.bytes().fold(1_000i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64)).abs().max(1)
                    } else {
                        return Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "MachoBindObject bind tuple missing objectID".into(),
                        });
                    }
                }
                _ => {
                    return Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "MachoBindObject requires an explicit integer or string objectID".into(),
                    })
                }
            };
            let bid = self.bind_object(session, effective_service, obj_id)?;

            let call_result = match args.get(1) {
                Some(PyRep::Tuple(call_tuple)) if !call_tuple.is_empty() => {
                    let sub_method = call_tuple
                        .first()
                        .and_then(|m| m.as_str())
                        .ok_or_else(|| BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "Piggybacked call in MachoBindObject requires string method name".into(),
                        })?;
                    let sub_args: Vec<PyRep> = match call_tuple.get(1) {
                        Some(PyRep::Tuple(items)) | Some(PyRep::List(items)) => items.clone(),
                        _ => Vec::new(),
                    };
                    let mut sub_kwargs = HashMap::new();
                    if let Some(PyRep::Dict(entries)) = call_tuple.get(2) {
                        for (k, v) in entries {
                            if let Some(ks) = k.as_str() {
                                sub_kwargs.insert(ks.to_owned(), v.clone());
                            }
                        }
                    }
                    Box::pin(self.dispatch_rpc(
                        session,
                        effective_service,
                        Some(bid),
                        sub_method,
                        &sub_args,
                        &sub_kwargs,
                        caller_trace,
                    ))
                    .await?
                }
                _ => PyRep::None,
            };

            return Ok(PyRep::Tuple(vec![
                PyRep::Integer(bid as i64),
                call_result,
            ]));
        }

        // Dispatch across the 12 domain modules in order
        if let Some(res) =
            services::auth_cache::handle(self, session, effective_service, method, args, kwargs)
        {
            return res;
        }
        if let Some(res) = services::character_skills::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::inventory_fitting::handle(
            self,
            session,
            bound_entry.as_ref(),
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::station_structure::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::space_destiny::handle(
            self,
            session,
            bound_entry.as_ref(),
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::combat_crimewatch::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::market_contracts::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) =
            services::industry_pi::handle(self, session, effective_service, method, args, kwargs)
        {
            return res;
        }
        if let Some(res) = services::agents_missions::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::social_corp_fleet::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::sov_fw_insurgency::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::cosmetics_events::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }

        warn!(
            service = effective_service,
            method,
            arg_count = args.len(),
            caller = caller_trace.unwrap_or("unknown"),
            "Unimplemented RPC intercepted — returning explicit UnimplementedRPC error"
        );
        if session.is_authenticated() {
            append_unimplemented_rpc_log(
                session.session_id,
                effective_service,
                method,
                args.len(),
                kwargs.keys().map(|s| s.as_str()).collect(),
                caller_trace,
            );
        }
        Err(BridgeRpcError {
            code: "UnimplementedRPC".into(),
            message: format!("{effective_service}.{method} is not yet implemented"),
        })
    }
}

const MAX_UNIMPLEMENTED_LOG_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB cap

fn append_unimplemented_rpc_log(
    session_id: u64,
    service: &str,
    method: &str,
    arg_count: usize,
    kwarg_keys: Vec<&str>,
    caller_trace: Option<&str>,
) {
    use std::io::Write;
    let _ = std::fs::create_dir_all("logs");
    let path = "logs/unimplemented_rpcs.jsonl";
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() >= MAX_UNIMPLEMENTED_LOG_BYTES {
            return;
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let sanitized_trace: String = caller_trace
            .unwrap_or("unknown")
            .chars()
            .filter(|c| !c.is_control())
            .take(256)
            .collect();
        let record = serde_json::json!({
            "session_id": session_id,
            "service": service,
            "method": method,
            "arg_count": arg_count,
            "kwarg_keys": kwarg_keys,
            "caller_trace": sanitized_trace,
        });
        let _ = writeln!(file, "{record}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_test_expansion_assets(ctx: &ServerContext) {
        use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
        ctx.db.with_mut_snapshot(|snap| {
            let extra_items = [
                (
                    100_000_013,
                    47702,
                    60_003_760,
                    InventoryFlag::Hangar,
                    10,
                    false,
                ),
                (
                    100_000_014,
                    40520,
                    60_003_760,
                    InventoryFlag::Hangar,
                    1,
                    false,
                ),
                (
                    100_000_015,
                    35832,
                    100_000_001,
                    InventoryFlag::Cargo,
                    5,
                    false,
                ),
                (
                    100_000_016,
                    56201,
                    100_000_001,
                    InventoryFlag::Cargo,
                    5,
                    false,
                ),
                (
                    100_000_017,
                    35814,
                    60_003_760,
                    InventoryFlag::Hangar,
                    10,
                    false,
                ),
                (
                    100_000_019,
                    47763,
                    100_000_001,
                    InventoryFlag::Cargo,
                    5,
                    false,
                ),
                (
                    100_000_020,
                    20424,
                    60_003_760,
                    InventoryFlag::Hangar,
                    10,
                    false,
                ),
                (
                    100_000_028,
                    20172,
                    60_003_760,
                    InventoryFlag::Hangar,
                    10,
                    false,
                ),
                (
                    100_000_023,
                    23055,
                    100_000_024,
                    InventoryFlag::FighterBay,
                    9,
                    false,
                ),
                (
                    100_000_024,
                    24483,
                    60_003_760,
                    InventoryFlag::Hangar,
                    1,
                    true,
                ),
                (
                    100_000_025,
                    4247,
                    100_000_024,
                    InventoryFlag::Cargo,
                    500,
                    false,
                ),
                (
                    100_000_026,
                    4247,
                    100_000_001,
                    InventoryFlag::Cargo,
                    500,
                    false,
                ),
                (
                    100_000_018,
                    1230,
                    60_003_760,
                    InventoryFlag::Hangar,
                    5_000,
                    false,
                ),
                (
                    100_000_027,
                    55001,
                    60_003_760,
                    InventoryFlag::Hangar,
                    1,
                    false,
                ),
                // Spare Rifter (100_000_029) in Jita 4-4 hangar with fitted autocannon for post-CONCORD kill-right engagement
                (
                    100_000_029,
                    587,
                    60_003_760,
                    InventoryFlag::Hangar,
                    1,
                    true,
                ),
                (
                    100_000_030,
                    2889,
                    100_000_029,
                    InventoryFlag::HiSlot0,
                    1,
                    true,
                ),
                // Third Rifter (100_000_035) in Jita 4-4 hangar carrying structure/filament supplies for post-PvP ship loss
                (
                    100_000_035,
                    587,
                    60_003_760,
                    InventoryFlag::Hangar,
                    1,
                    true,
                ),
                (
                    100_000_031,
                    35832,
                    100_000_035,
                    InventoryFlag::Cargo,
                    5,
                    false,
                ),
                (
                    100_000_032,
                    56201,
                    100_000_035,
                    InventoryFlag::Cargo,
                    5,
                    false,
                ),
                (
                    100_000_033,
                    4247,
                    100_000_035,
                    InventoryFlag::Cargo,
                    500,
                    false,
                ),
                (
                    100_000_034,
                    47763,
                    100_000_035,
                    InventoryFlag::Cargo,
                    5,
                    false,
                ),
                (
                    100_000_040,
                    22989,
                    100_000_001,
                    InventoryFlag::HiSlot0,
                    2_000,
                    false,
                ),
                (
                    100_000_041,
                    22989,
                    100_000_029,
                    InventoryFlag::HiSlot0,
                    2_000,
                    false,
                ),
            ];
            for (item_id, type_id, location_id, flag, quantity, singleton) in extra_items {
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id,
                        owner_id: 90_000_001,
                        location_id,
                        flag,
                        quantity,
                        singleton,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
            }
            snap.items.insert(
                100_000_042,
                InventoryItemRecord {
                    item_id: 100_000_042,
                    type_id: 22989,
                    owner_id: 90_000_002,
                    location_id: 100_000_002,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 2_000,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            let char_skills = snap.skills.entry(90_000_001).or_default();
            for (skill_id, level, sp) in [
                (3388, 4, 45_255.0),
                (11448, 4, 226_275.0),
                (11452, 4, 226_275.0),
                (21791, 4, 226_275.0),
                (24242, 4, 45_255.0),
                (33407, 4, 90_510.0),
                (21611, 2, 12_699.0),
            ] {
                char_skills.insert(
                    skill_id,
                    TrainedSkillRecord {
                        type_id: skill_id,
                        level,
                        skill_points: sp,
                    },
                );
            }
        });
    }

    async fn login_and_select_aura(ctx: &ServerContext, session: &mut SessionState) {
        seed_test_expansion_assets(ctx);
        ctx.dispatch_rpc(
            session,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        ctx.dispatch_rpc(
            session,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_authentication_and_ownership_enforcement() {
        // R22-P1-5: Production ServerContext::new_production() starts with an empty account table and rejects demo credentials
        let prod_ctx = ServerContext::new_production();
        assert!(prod_ctx.db.with_snapshot(|s| s.accounts.is_empty()));
        assert!(prod_ctx.db.authenticate("demo", "demo-secret").is_err());
        let op_uid = prod_ctx
            .db
            .provision_player_account("capsuleer_op", "op-secret-999");
        assert_eq!(op_uid, 1);
        assert!(prod_ctx
            .db
            .authenticate("capsuleer_op", "op-secret-999")
            .is_ok());
        // Verify seed_dual_anchor_markets is idempotent across restarts and does not recreate consumed orders
        assert!(prod_ctx.db.with_snapshot(|s| s.markets_seeded));
        prod_ctx.db.with_mut_snapshot(|s| {
            s.market_orders.remove(&700_000_001);
        });
        let orders_after_consume = prod_ctx.db.with_snapshot(|s| s.market_orders.len());
        assert_eq!(
            crate::world_seeder::seed_dual_anchor_markets(&prod_ctx.db, &prod_ctx.sde),
            0
        );
        assert_eq!(
            prod_ctx.db.with_snapshot(|s| s.market_orders.len()),
            orders_after_consume
        );

        let ctx = ServerContext::new();
        let mut session = SessionState::new(1);

        let unauth_res = ctx
            .dispatch_rpc(
                &mut session,
                "charUnboundMgr",
                None,
                "GetCharactersToSelect",
                &[],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(unauth_res.unwrap_err().code, "Unauthorized");

        login_and_select_aura(&ctx, &mut session).await;
        assert_eq!(session.character_id, Some(90_000_001));
        assert_eq!(session.race_id, Some(2));

        let bad_bind = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(30_000_999)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(bad_bind.unwrap_err().code, "BindAuthorizationDenied");

        // Non-admin account ("demo", role 0x01) must be rejected on GM /slash commands
        let slash_denied = ctx
            .dispatch_rpc(
                &mut session,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/giveisk 1000".into())],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(slash_denied.unwrap_err().code, "InsufficientRole");

        // By default, "admin" is NOT seeded in WorldRepository::with_demo_seed()
        let mut admin_sess = SessionState::new(2);
        let unseeded_admin = ctx
            .dispatch_rpc(
                &mut admin_sess,
                "authentication",
                None,
                "Login",
                &[
                    PyRep::String("admin".into()),
                    PyRep::String("admin-secret".into()),
                ],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(unseeded_admin.unwrap_err().code, "AuthenticationFailed");

        // Provision GM account explicitly via provision_gm_account (role 0x03) and verify /giveisk
        ctx.db
            .provision_gm_account("admin", "admin-secret", 90_000_003, "ISD Polaris");
        ctx.dispatch_rpc(
            &mut admin_sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("admin".into()),
                PyRep::String("admin-secret".into()),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut admin_sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_003)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut admin_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 1000".into())],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        // Reprocessing non-ore (Tritanium 100_000_011) must be rejected with ItemNotReprocessable
        let bad_reproc = ctx
            .dispatch_rpc(
                &mut session,
                "reprocessingSvc",
                None,
                "Reprocess",
                &[PyRep::Integer(100_000_011)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(bad_reproc.unwrap_err().code, "ItemNotReprocessable");

        // Jump clone install succeeds when docked, duplicate at same station is rejected,
        // same-station CloneJump places pilot in a Capsule (670) with 0s cooldown, after which Pilot re-boards Rifter 100_000_001
        let clone_res = ctx
            .dispatch_rpc(
                &mut session,
                "jumpCloneSvc",
                None,
                "InstallCloneInStation",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(ckv) = clone_res else {
            panic!("Expected clone KeyVal");
        };
        let jc_id = ckv.get("jumpCloneID").unwrap().as_i64().unwrap();
        let dup_clone = ctx
            .dispatch_rpc(
                &mut session,
                "jumpCloneSvc",
                None,
                "InstallCloneInStation",
                &[],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(
            dup_clone.unwrap_err().code,
            "JumpCloneAlreadyAtStation"
        );
        let cj_res = ctx
            .dispatch_rpc(
                &mut session,
                "jumpCloneSvc",
                None,
                "CloneJump",
                &[PyRep::Integer(jc_id)],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(cj_kv) = cj_res else {
            panic!("Expected CloneJump KeyVal");
        };
        // Same-station clone swap applies 0s cooldown and places character in a Capsule (type_id 670)
        assert_eq!(cj_kv.get("cooldownSeconds"), Some(&PyRep::Integer(0)));
        assert_ne!(session.ship_id, Some(100_000_001));
        // Re-board Rifter 100_000_001 in station hangar via ship.Board
        ctx.dispatch_rpc(
            &mut session,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(100_000_001)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(session.ship_id, Some(100_000_001));

        // Research ME (activity 4, 10 runs) and TE (activity 3, 10 runs) on Rifter BPO (100_000_007),
        // copy 2 BPCs (activity 5), and test both failed and successful T2 Invention (activity 8) rolls
        for (act, runs) in [(4_i64, 10_i64), (3_i64, 10_i64), (5_i64, 1_i64), (5_i64, 1_i64)] {
            let jr = ctx
                .dispatch_rpc(
                    &mut session,
                    "industryManager",
                    None,
                    "InstallJob",
                    &[
                        PyRep::Integer(687),
                        PyRep::Integer(act),
                        PyRep::Integer(runs),
                    ],
                    &HashMap::new(),
                    None,
                )
                .await
                .unwrap();
            let PyRep::KeyVal(jkv) = jr else {
                panic!("Expected job KeyVal");
            };
            let jid = jkv.get("jobID").unwrap().as_i64().unwrap();
            ctx.advance_virtual_clock(30_000);
            ctx.dispatch_rpc(
                &mut session,
                "industryManager",
                None,
                "CompleteJob",
                &[PyRep::Integer(jid)],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        }

        // First T2 Invention job: test premature completion rejection, then deterministic failed roll (override 0.95 >= inv_prob)
        let inv_job_1 = ctx
            .dispatch_rpc(
                &mut session,
                "industryManager",
                None,
                "InstallJob",
                &[
                    PyRep::Integer(687),
                    PyRep::Integer(8),
                    PyRep::Integer(1),
                ],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(ijkv1) = inv_job_1 else {
            panic!("Expected invention job KeyVal");
        };
        let inv_job_id_1 = ijkv1.get("jobID").unwrap().as_i64().unwrap();
        ctx.advance_virtual_clock(7_200);
        let too_early = ctx
            .dispatch_rpc(
                &mut session,
                "industryManager",
                None,
                "CompleteJob",
                &[PyRep::Integer(inv_job_id_1)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(too_early.unwrap_err().code, "JobNotYetComplete");
        ctx.advance_virtual_clock(40_000);
        ctx.set_invention_roll_override(Some(0.95));
        let inv_failed = ctx
            .dispatch_rpc(
                &mut session,
                "industryManager",
                None,
                "CompleteJob",
                &[PyRep::Integer(inv_job_id_1)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(inv_failed.unwrap_err().code, "InventionAttemptFailed");

        // Second T2 Invention job: deterministic successful roll (override 0.10 < inv_prob) yields Wolf Blueprint (11372)
        let inv_job_2 = ctx
            .dispatch_rpc(
                &mut session,
                "industryManager",
                None,
                "InstallJob",
                &[
                    PyRep::Integer(687),
                    PyRep::Integer(8),
                    PyRep::Integer(1),
                ],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(ijkv2) = inv_job_2 else {
            panic!("Expected invention job 2 KeyVal");
        };
        let inv_job_id_2 = ijkv2.get("jobID").unwrap().as_i64().unwrap();
        ctx.advance_virtual_clock(40_000);
        ctx.set_invention_roll_override(Some(0.10));
        let inv_done = ctx
            .dispatch_rpc(
                &mut session,
                "industryManager",
                None,
                "CompleteJob",
                &[PyRep::Integer(inv_job_id_2)],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        ctx.set_invention_roll_override(None);
        let PyRep::KeyVal(idkv) = inv_done else {
            panic!("Expected invention completion KeyVal");
        };
        assert_eq!(
            idkv.get("productTypeID"),
            Some(&PyRep::Integer(11372)),
            "Invention of Rifter Blueprint (687) must yield Wolf Blueprint (11372)"
        );
    }

    #[tokio::test]
    async fn test_vertical_slice_undock_double_undock_prevention_and_movement() {
        let ctx = ServerContext::new();
        let mut session = SessionState::new(10);
        login_and_select_aura(&ctx, &mut session).await;

        let undock_res = ctx
            .dispatch_rpc(
                &mut session,
                "stationSvc",
                None,
                "Undock",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();

        let double_undock = ctx
            .dispatch_rpc(
                &mut session,
                "stationSvc",
                None,
                "Undock",
                &[],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(double_undock.unwrap_err().code, "UndockFailed");

        let PyRep::KeyVal(kv) = undock_res else {
            panic!("Expected KeyVal from Undock");
        };
        let Some(PyRep::Integer(bound_id)) = kv.get("beyonceBoundID") else {
            panic!("Expected beyonceBoundID");
        };

        let unbound_call = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                None,
                "CmdGotoDirection",
                &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(unbound_call.unwrap_err().code, "BoundObjectRequired");

        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(*bound_id as u64),
            "CmdGotoDirection",
            &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        let sys_handle = ctx.systems.get_or_spawn(30_000_142);
        let tick_broadcast = sys_handle.step_manual(1.0).await.unwrap();
        assert!(tick_broadcast.tick >= 2);
    }

    #[tokio::test]
    async fn test_all_18_e2e_journeys_and_catalog_coverage() {
        let ctx = ServerContext::new();
        let mut session = SessionState::new(100);
        let empty = HashMap::new();

        // E2E-01: Auth, Character Creation (Minmatar race=2, Brutor bloodline=3, ancestry=13, school=20), Selection, & Station Hangar Sync
        ctx.dispatch_rpc(
            &mut session,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        let new_char = ctx
            .dispatch_rpc(
                &mut session,
                "charUnboundMgr",
                None,
                "CreateCharacterWithDoll",
                &[
                    PyRep::String("Valkyrie Pilot".into()),
                    PyRep::Integer(2),
                    PyRep::Integer(3),
                    PyRep::Integer(19),
                    PyRep::Integer(14),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        assert!(new_char.as_i64().unwrap() > 200_000_000);
        login_and_select_aura(&ctx, &mut session).await;

        // E2E-02: Skill Queue, Skill Injector Consumption, Free SP Application, & Jump Clone Installation
        let injected_sp = ctx
            .dispatch_rpc(
                &mut session,
                "skillMgr2",
                None,
                "InjectSkillPoints",
                &[],
                &empty,
                None,
            )
            .await
            .unwrap();
        assert_eq!(injected_sp.as_f64().unwrap(), 400_000.0);
        ctx.dispatch_rpc(
            &mut session,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::Integer(3327), PyRep::Integer(5)],
            &empty,
            None,
        )
        .await
        .unwrap();
        let sp_after = ctx
            .dispatch_rpc(
                &mut session,
                "skillMgr2",
                None,
                "ApplyFreeSkillPoints",
                &[],
                &empty,
                None,
            )
            .await
            .unwrap();
        assert!(sp_after.as_f64().unwrap() > 15_000_000.0);
        ctx.dispatch_rpc(
            &mut session,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-03: Ship Fitting, Module Overload, Insurance, & Abyssal Mutaplasmid Rolling
        ctx.dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(100_000_003),
                PyRep::Integer(100_000_001),
                PyRep::Integer(19),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Overload",
            &[PyRep::Integer(100_000_003)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47702), PyRep::Integer(100_000_003)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(100_000_001), PyRep::Real(1.0)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-04 & E2E-05: Undock, Sub-Warp Flight, Intra-System Warp to Stargate 50001248, & Jump to Perimeter
        let undock = ctx
            .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &empty, None)
            .await
            .unwrap();
        let PyRep::KeyVal(ukv) = undock else {
            panic!("Expected KeyVal");
        };
        let b_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(b_id),
            "CmdWarpToPoint",
            &[
                PyRep::Real(-161_369_743_360.0),
                PyRep::Real(-30_690_549_760.0),
                PyRep::Real(-1_107_962_183_680.0),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        let jita_handle = ctx.systems.get_or_spawn(30_000_142);
        for _ in 0..30 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        let jump_res = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                Some(b_id),
                "CmdStargateJump",
                &[PyRep::Integer(50001248)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(jkv) = jump_res else {
            panic!("Expected jump KeyVal");
        };
        assert_eq!(jkv.get("solarSystemID"), Some(&PyRep::Integer(30_000_144)));
        let perim_b_id = jkv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
        let perim_handle = ctx.systems.get_or_spawn(30_000_144);

        // Warp to Perimeter -> Tama stargate 50001250 and jump to Low-Sec Tama (30002813)
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(perim_b_id),
            "CmdWarpToPoint",
            &[
                PyRep::Real(86_000_000_000.0),
                PyRep::Real(12_000_000_000.0),
                PyRep::Real(-141_000_000_000.0),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        for _ in 0..30 {
            let _ = perim_handle.step_manual(1.0).await.unwrap();
        }
        let jump_tama = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                Some(perim_b_id),
                "CmdStargateJump",
                &[PyRep::Integer(50001250)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(tama_kv) = jump_tama else {
            panic!("Expected Tama jump KeyVal");
        };
        let tama_b_id = tama_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
        let tama_handle = ctx.systems.get_or_spawn(30_002_813);

        // E2E-11: Probe Scanning in Low-Sec Tama (30002813, spawns K162 ball 750_000_001), Warp to Wormhole, & Wormhole Jump into J105934 (31000005)
        let scan_res = ctx
            .dispatch_rpc(
                &mut session,
                "scanMgr",
                None,
                "RequestScans",
                &[PyRep::Real(0.25)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(skv) = scan_res else {
            panic!("Expected scan KeyVal");
        };
        assert_eq!(skv.get("warpable"), Some(&PyRep::Bool(true)));
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(tama_b_id),
            "CmdWarpToPoint",
            &[
                PyRep::Real(15_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        for _ in 0..30 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        ctx.dispatch_rpc(
            &mut session,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();
        assert_eq!(session.solar_system_id, Some(31_000_005));

        // Jump wormhole static exit from J105559 (31_000_005) into K-space Null-Sec VFK-IV (30_002_904)
        let jump_vfk = ctx
            .dispatch_rpc(
                &mut session,
                "wormholeMgr",
                None,
                "WormholeJump",
                &[],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(vfk_kv) = jump_vfk else {
            panic!("Expected VFK-IV wormhole jump KeyVal");
        };
        assert_eq!(vfk_kv.get("solarSystemID"), Some(&PyRep::Integer(30_002_904)));

        // E2E-06: Turret Combat against test-spawned NPC Ball 700_000_001, DBS/ESS Bounty Split, & ESS Bank Hack (in K-space Null-Sec VFK-IV 30002904)
        let hed_handle = ctx.systems.get_or_spawn(30_002_904);
        let (_, ship_in_hed) = hed_handle
            .get_ball_snapshot(100_000_001)
            .await
            .unwrap()
            .unwrap();
        let _ = hed_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: 700_000_001,
                    type_id: 11001,
                    position: ship_in_hed.position + glam::DVec3::new(900.0, 0.0, 0.0),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 310.0,
                    inertia: 2.5,
                    mass: 1_000_000.0,
                    radius: 35.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState {
                    shield_hp: 50.0,
                    max_shield_hp: 50.0,
                    armor_hp: 50.0,
                    max_armor_hp: 50.0,
                    structure_hp: 50.0,
                    max_structure_hp: 50.0,
                    ..eve_destiny::CombatState::default()
                },
            )
            .await;
        let _ = hed_handle
            .spawn_ball(eve_destiny::Ball {
                id: 750_000_002,
                type_id: 32882,
                position: ship_in_hed.position + glam::DVec3::new(500.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Rigid,
            })
            .await;
        let _ = hed_handle.step_manual(1.0).await.unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "FireWeapon",
            &[PyRep::Integer(700_000_001)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();
        for _ in 0..5 {
            let _ = hed_handle.step_manual(1.0).await.unwrap();
        }
        ctx.dispatch_rpc(
            &mut session,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();

        // Jump wormhole from VFK-IV (30002904) -> Tama (30002813) -> stargate 50001251 -> Perimeter (50001873) -> Jita (30000142)
        let jb_tama = ctx
            .dispatch_rpc(
                &mut session,
                "wormholeMgr",
                None,
                "WormholeJump",
                &[],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(jb_tama_kv) = jb_tama else {
            panic!("Expected jb_tama KeyVal");
        };
        let tama_b2 = jb_tama_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(tama_b2),
            "CmdWarpToPoint",
            &[
                PyRep::Real(10_000_000_000.0),
                PyRep::Real(0.0),
                PyRep::Real(10_000_000_000.0),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        for _ in 0..30 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        let jb_perim = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                Some(tama_b2),
                "CmdStargateJump",
                &[PyRep::Integer(50001251)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(jb_perim_kv) = jb_perim else {
            panic!("Expected jb_perim KeyVal");
        };
        let perim_b2 = jb_perim_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(perim_b2),
            "CmdWarpToPoint",
            &[
                PyRep::Real(84_000_000_000.0),
                PyRep::Real(12_000_000_000.0),
                PyRep::Real(-139_000_000_000.0),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        for _ in 0..30 {
            let _ = perim_handle.step_manual(1.0).await.unwrap();
        }
        let jump_back = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                Some(perim_b2),
                "CmdStargateJump",
                &[PyRep::Integer(50001873)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(jb_kv) = jump_back else {
            panic!("Expected jump_back KeyVal");
        };
        let jita_b_id = jb_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

        // Warp to Jita 4-4 station (60_003_760) at [-107303362560.0, -18744975360.0, 436489052160.0] and dock
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(jita_b_id),
            "CmdWarpToPoint",
            &[
                PyRep::Real(-107_303_362_560.0),
                PyRep::Real(-18_744_975_360.0),
                PyRep::Real(436_489_052_160.0),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        for _ in 0..30 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        ctx.dispatch_rpc(
            &mut session,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(60_003_760)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-07: Regional Market Order Book, Immediate Match, Broker Fee, & Sales Tax
        let order_res = ctx
            .dispatch_rpc(
                &mut session,
                "marketProxy",
                None,
                "PlaceCharOrder",
                &[
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(34),
                    PyRep::Real(10.0),
                    PyRep::Integer(100),
                    PyRep::Bool(true),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(okv) = order_res else {
            panic!("Expected market order KeyVal");
        };
        assert_eq!(okv.get("filledImmediately"), Some(&PyRep::Bool(true)));

        // E2E-08: Ore Compression, Reprocessing, & Blueprint Manufacturing
        ctx.dispatch_rpc(
            &mut session,
            "structureCompressionMgr",
            None,
            "CompressItemInStructure",
            &[PyRep::Integer(100_000_012)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_012)],
            &empty,
            None,
        )
        .await
        .unwrap();
        let job_res = ctx
            .dispatch_rpc(
                &mut session,
                "industryManager",
                None,
                "InstallJob",
                &[
                    PyRep::Integer(687),
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(job_kv) = job_res else {
            panic!("Expected job KeyVal");
        };
        let job_id = job_kv.get("jobID").unwrap().as_i64().unwrap();
        ctx.advance_virtual_clock(6_000);
        ctx.dispatch_rpc(
            &mut session,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(job_id)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-09: Agent Mission Request, Combat NPC Room Clear, Acceleration Gate (`keeper`), & 16-Mission Storyline Trigger
        let undock_m = ctx
            .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &empty, None)
            .await
            .unwrap();
        let PyRep::KeyVal(um_kv) = undock_m else {
            panic!("Expected Undock KeyVal");
        };
        let m_b_id = um_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(m_b_id),
            "CmdStop",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();
        let mut storyline_triggered = false;
        for i in 1..=16i64 {
            let mid = 1000 + i;
            ctx.dispatch_rpc(
                &mut session,
                "agentMgr",
                None,
                "RequestMission",
                &[PyRep::Integer(mid)],
                &empty,
                None,
            )
            .await
            .unwrap();
            let acc_m = ctx
                .dispatch_rpc(
                    &mut session,
                    "agentMgr",
                    None,
                    "AcceptMission",
                    &[PyRep::Integer(mid)],
                    &empty,
                    None,
                )
                .await
                .unwrap();
            let PyRep::KeyVal(acc_mkv) = acc_m else {
                panic!("Expected AcceptMission KeyVal");
            };
            let npc_ball_id = acc_mkv.get("spawnedNpcBallID").unwrap().as_i64().unwrap();
            for _ in 0..10 {
                for _ in 0..4 {
                    let _ = jita_handle.step_manual(1.0).await.unwrap();
                }
                let shot = ctx
                    .dispatch_rpc(
                        &mut session,
                        "dogmaIM",
                        None,
                        "FireWeapon",
                        &[PyRep::Integer(npc_ball_id)],
                        &empty,
                        None,
                    )
                    .await
                    .unwrap();
                if let PyRep::KeyVal(skv) = shot {
                    if skv.get("destroyed") == Some(&PyRep::Bool(true)) {
                        break;
                    }
                }
            }
            ctx.dispatch_rpc(
                &mut session,
                "keeper",
                None,
                "ActivateAccelerationGate",
                &[PyRep::Integer(mid)],
                &empty,
                None,
            )
            .await
            .unwrap();
            let mres = ctx
                .dispatch_rpc(
                    &mut session,
                    "agentMgr",
                    None,
                    "CompleteMission",
                    &[PyRep::Integer(mid)],
                    &empty,
                    None,
                )
                .await
                .unwrap();
            if let PyRep::KeyVal(mkv) = mres {
                if mkv.get("storylineTriggered") == Some(&PyRep::Bool(true)) {
                    storyline_triggered = true;
                }
            }
        }
        assert!(storyline_triggered, "16th mission must trigger storyline offer");

        // E2E-17 (while in space in Jita): Living World 3-Tier LOD Materialization (auto-triggered on Undock), Convoy Combat Destruction via FireWeapon, & +15% NPC Price Spike
        let non_gm_mat = ctx
            .dispatch_rpc(
                &mut session,
                "taleMgr",
                None,
                "MaterializeSystemLOD2",
                &[PyRep::Integer(30_000_142)],
                &empty,
                None,
            )
            .await;
        assert_eq!(non_gm_mat.unwrap_err().code, "InsufficientRole");
        let non_gm_int = ctx
            .dispatch_rpc(
                &mut session,
                "taleMgr",
                None,
                "InterdictConvoy",
                &[PyRep::Integer(800_000_001)],
                &empty,
                None,
            )
            .await;
        assert_eq!(non_gm_int.unwrap_err().code, "InsufficientRole");
        let mut lw_destroyed = false;
        let mut lw_new_ask = 0.0;
        for _ in 0..10 {
            for _ in 0..4 {
                let _ = jita_handle.step_manual(1.0).await.unwrap();
            }
            let lw_res = ctx
                .dispatch_rpc(
                    &mut session,
                    "dogmaIM",
                    None,
                    "FireWeapon",
                    &[PyRep::Integer(800_000_001)],
                    &empty,
                    None,
                )
                .await
                .unwrap();
            let PyRep::KeyVal(lw_kv) = lw_res else {
                panic!("Expected Living World combat destruction KeyVal");
            };
            if lw_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
                lw_destroyed = true;
                lw_new_ask = lw_kv.get("newDestinationAskISK").unwrap().as_f64().unwrap();
                break;
            }
        }
        assert!(lw_destroyed);
        assert!(lw_new_ask > 5.0);

        // E2E-10: Fleet Creation, EveMail, & Bookmark Creation
        ctx.dispatch_rpc(
            &mut session,
            "fleetObjectHandler",
            None,
            "CreateFleet",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "bookmark",
            None,
            "BookmarkLocation",
            &[PyRep::String("Wormhole Entry".into())],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(&mut session, "mailMgr", None, "SendMail", &[], &empty, None)
            .await
            .unwrap();

        // E2E-12: CrimeWatch Illegal High-Sec Attack (with rival capsuleer 90_000_002 undocked on grid in Jita 1.0), CONCORD Response, & Kill Right Activation
        let mut sess2 = SessionState::new(200);
        ctx.dispatch_rpc(
            &mut sess2,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("other".into()),
                PyRep::String("other-secret".into()),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut sess2,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_002)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(&mut sess2, "stationSvc", None, "Undock", &[], &empty, None)
            .await
            .unwrap();

        for _ in 0..4 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        let cw_res = ctx
            .dispatch_rpc(
                &mut session,
                "dogmaIM",
                None,
                "FireWeapon",
                &[PyRep::Integer(90_000_002)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(cw_kv) = cw_res else {
            panic!("Expected CrimeWatch KeyVal");
        };
        assert_ne!(session.ship_id, Some(100_000_001));
        assert!(ctx.db.get_item(100_000_001).is_none());
        let kr_id = cw_kv.get("killRightID").unwrap().as_i64().unwrap();
        ctx.dispatch_rpc(
            &mut sess2,
            "killRightMgr",
            None,
            "ActivateKillRight",
            &[PyRep::Integer(kr_id)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // CONCORD destroyed 100_000_001 and respawned 90_000_001 in a Capsule at 60_003_760;
        // board spare Rifter 100_000_029 in hangar, undock via RPC, and verify legal kill right engagement by sess2
        ctx.dispatch_rpc(
            &mut session,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(100_000_029)],
            &empty,
            None,
        )
        .await
        .unwrap();
        let undock_29 = ctx
            .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &empty, None)
            .await
            .unwrap();
        let PyRep::KeyVal(u29_kv) = undock_29 else {
            panic!("Expected Undock KeyVal");
        };
        let b29 = u29_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
        ctx.dispatch_rpc(&mut session, "beyonce", Some(b29), "CmdStop", &[], &empty, None)
            .await
            .unwrap();
        let b_sess2 = ctx.bind_object(&mut sess2, "beyonce", 30_000_142).unwrap();
        ctx.dispatch_rpc(&mut sess2, "beyonce", Some(b_sess2), "CmdStop", &[], &empty, None)
            .await
            .unwrap();
        let mut legal_destroyed = false;
        for _ in 0..45 {
            for _ in 0..4 {
                let _ = jita_handle.step_manual(1.0).await.unwrap();
            }
            let legal_res = ctx
                .dispatch_rpc(
                    &mut sess2,
                    "dogmaIM",
                    None,
                    "FireWeapon",
                    &[PyRep::Integer(90_000_001)],
                    &empty,
                    None,
                )
                .await
                .unwrap();
            let PyRep::KeyVal(lkv) = legal_res else {
                panic!("Expected legal kill KeyVal");
            };
            assert_eq!(lkv.get("legalEngagement"), Some(&PyRep::Bool(true)));
            if lkv.get("destroyed") == Some(&PyRep::Bool(true)) {
                legal_destroyed = true;
                break;
            }
        }
        assert!(legal_destroyed, "Target ship should be destroyed across volley");
        assert!(
            ctx.db.get_item(100_000_029).is_none() && ctx.db.get_item(100_000_030).is_none(),
            "PvP ship destruction must remove destroyed ship and fitted modules from DB"
        );
        // After 100_000_029 was destroyed in PvP, session is in an ejected Capsule in space; dock at 60_003_760, board 100_000_035, and undock
        ctx.dispatch_rpc(
            &mut session,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(60_003_760)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(100_000_035)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &empty, None)
            .await
            .unwrap();

        // E2E-13: Planetary Interaction Colony Setup, Routing, & POCO Export Tax
        ctx.dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "CreateColony",
            &[PyRep::Integer(40_009_082)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(40_009_082)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // Create player corporation "Apex Syndicate" before anchoring structure so high-sec war declaration can target it
        let corp1_res = ctx
            .dispatch_rpc(
                &mut session,
                "corpmgr",
                None,
                "CreateCorporation",
                &[
                    PyRep::String("Apex Syndicate".into()),
                    PyRep::String("APEX".into()),
                    PyRep::Real(0.05),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(corp1_kv) = corp1_res else {
            panic!("Expected corp1 KeyVal");
        };
        let apex_corp_id = corp1_kv.get("corporationID").unwrap().as_i64().unwrap();

        // E2E-14: Upwell Structure Anchoring (consuming Astrahus 35832 + Quantum Core 56201), Fueling, & Damage-Capped Reinforcement
        let st_res = ctx
            .dispatch_rpc(
                &mut session,
                "structureDeployment",
                None,
                "Anchor",
                &[PyRep::Integer(35832), PyRep::String("EvoMu Keepstar".into())],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(st_kv) = st_res else {
            panic!("Expected structure KeyVal");
        };
        let st_id = st_kv.get("structureID").unwrap().as_i64().unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "structureControl",
            None,
            "FuelStructure",
            &[PyRep::Integer(st_id), PyRep::Integer(50)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut sess2,
            "corpmgr",
            None,
            "CreateCorporation",
            &[
                PyRep::String("Rival Raiders".into()),
                PyRep::String("RVL".into()),
                PyRep::Real(0.05),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        // Verify attacking a high-sec structure without an active war fails even if attacker owns a personal kill right
        let no_war_atk = ctx
            .dispatch_rpc(
                &mut sess2,
                "structureControl",
                None,
                "AttackStructure",
                &[PyRep::Integer(st_id)],
                &empty,
                None,
            )
            .await;
        assert_eq!(no_war_atk.unwrap_err().code, "IllegalHighSecStructureAttack");

        // Declare corporate war against Apex Syndicate, advance virtual clock past 24h warmup, and attack structure legally
        ctx.dispatch_rpc(
            &mut sess2,
            "warRegistry",
            None,
            "DeclareWarAgainst",
            &[PyRep::Integer(apex_corp_id)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.advance_virtual_clock(86_401);
        let _ = jita_handle.step_manual(1.0).await.unwrap();
        ctx.dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(st_id)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // Dock both sess2 (contract issuer) and session (courier) at anchored structure `st_id`
        ctx.dispatch_rpc(
            &mut sess2,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(st_id)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "structureDocking",
            None,
            "Dock",
            &[PyRep::Integer(st_id)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-15: Courier Contract Creation (st_id -> Jita 4-4 60_003_760 by sess2), Self-Accept Rejection, Acceptance by session, Undock/Dock Delivery, & Destination-Verified Completion
        let c_res = ctx
            .dispatch_rpc(
                &mut sess2,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::String("Courier".into()),
                    PyRep::Integer(st_id),
                    PyRep::Integer(60_003_760),
                    PyRep::Real(500_000.0),
                    PyRep::Real(2_000_000.0),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(c_kv) = c_res else {
            panic!("Expected contract KeyVal");
        };
        let cid = c_kv.get("contractID").unwrap().as_i64().unwrap();
        let self_acc = ctx
            .dispatch_rpc(
                &mut sess2,
                "contractProxy",
                None,
                "AcceptContract",
                &[PyRep::Integer(cid)],
                &empty,
                None,
            )
            .await;
        assert_eq!(self_acc.unwrap_err().code, "CannotAcceptOwnContract");
        ctx.dispatch_rpc(
            &mut session,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid)],
            &empty,
            None,
        )
        .await
        .unwrap();
        // Attempting to complete at `st_id` before delivering to `60_003_760` must fail
        let early_complete = ctx
            .dispatch_rpc(
                &mut session,
                "contractProxy",
                None,
                "CompleteContract",
                &[PyRep::Integer(cid)],
                &empty,
                None,
            )
            .await;
        assert_eq!(early_complete.unwrap_err().code, "CourierNotAtDestinationStation");

        // Undock from structure `st_id` via RPC and dock at destination station `60_003_760` via RPC
        let st_undock = ctx
            .dispatch_rpc(
                &mut session,
                "structureDocking",
                None,
                "Undock",
                &[],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(stu_kv) = st_undock else {
            panic!("Expected structure undock KeyVal");
        };
        let stu_b_id = stu_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
        let short_warp = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                Some(stu_b_id),
                "CmdWarpToPoint",
                &[
                    PyRep::Real(-107_303_362_560.0),
                    PyRep::Real(-18_744_975_360.0),
                    PyRep::Real(436_489_052_160.0),
                ],
                &empty,
                None,
            )
            .await;
        assert_eq!(short_warp.unwrap_err().code, "WarpDistanceTooShort");
        let (_, sb) = jita_handle
            .get_ball_snapshot(session.ship_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(stu_b_id),
            "CmdGotoDirection",
            &[
                PyRep::Real(-107_303_362_560.0 - sb.position.x),
                PyRep::Real(-18_744_975_360.0 - sb.position.y),
                PyRep::Real(436_489_052_160.0 - sb.position.z),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        for _ in 0..8 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(stu_b_id),
            "CmdStop",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(60_003_760)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(cid)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-18: Corporation Project Escrow & Member Contribution Payout (while docked at 60_003_760 where ore/mineral is in Hangar)
        let proj_res = ctx
            .dispatch_rpc(
                &mut session,
                "corporationSvc",
                None,
                "CreateProject",
                &[
                    PyRep::String("Mine Veldspar".into()),
                    PyRep::Integer(500),
                    PyRep::Real(100.0),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(pkv) = proj_res else {
            panic!("Expected project KeyVal");
        };
        let pid = pkv.get("projectID").unwrap().as_i64().unwrap();
        let contrib = ctx
            .dispatch_rpc(
                &mut session,
                "corporationSvc",
                None,
                "ContributeToProject",
                &[PyRep::Integer(pid), PyRep::Integer(500)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(ckv) = contrib else {
            panic!("Expected contribution KeyVal");
        };
        assert_eq!(ckv.get("completed"), Some(&PyRep::Bool(true)));

        // Undock via RPC and test Abyssal Filament consumption (`abyssalMgr.ActivateFilament`)
        ctx.dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &empty, None)
            .await
            .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "abyssalMgr",
            None,
            "ActivateFilament",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-16: AIR NPE Node Graph 473 Progression & AIR Career Program SP Claim (after reaching target_progress)
        for _ in 0..3 {
            ctx.dispatch_rpc(
                &mut session,
                "air_npe",
                None,
                "AdvanceNode",
                &[],
                &empty,
                None,
            )
            .await
            .unwrap();
        }
        ctx.dispatch_rpc(
            &mut session,
            "seasonManager",
            None,
            "claim_goal_reward",
            &[PyRep::Integer(1)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // Verify unimplemented methods on cataloged services strictly return UnimplementedRPC (no catch-all stub)
        assert_eq!(ctx.cataloged_services.len(), 216);
        let unimpl_res = ctx
            .dispatch_rpc(
                &mut session,
                "marketProxy",
                None,
                "NonExistentMethod",
                &[],
                &empty,
                None,
            )
            .await;
        assert_eq!(unimpl_res.unwrap_err().code, "UnimplementedRPC");

        assert_eq!(ctx.quasar_domains.len(), 135);
        for ns in &ctx.quasar_domains {
            let q_res = ctx
                .dispatch_rpc(
                    &mut session,
                    "publicGatewaySvc",
                    None,
                    "SendRequest",
                    &[
                        PyRep::String(ns.clone()),
                        PyRep::String("GetStateRequest".into()),
                    ],
                    &empty,
                    None,
                )
                .await;
            assert_eq!(
                q_res.unwrap_err().code,
                "UnimplementedRPC",
                "Cataloged Quasar namespace {ns} must return explicit UnimplementedRPC"
            );
        }
    }
}
