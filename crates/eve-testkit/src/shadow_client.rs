//! Shadow client replica convergence check (`#930`, RFC-3 `§3` & `§5 G4`).
//!
//! Maintains an in-memory replica of what the V24.01 EVE client holds across:
//! - [`ShadowInventory`]: `invCache` / `godma.stateManager.invItems` (`item_id -> ShadowItem`) and
//!   `sublocationsByLocationFlag` / `instanceFlagQuantityCache` (`(ship_id, flag_id) -> ShadowChargeSublocation`).
//! - [`ShadowWallet`]: personal (`"cash"`) and corporation division (`"cash"`..`"cash7"` / `1000..=1006`) balances.
//! - [`ShadowModuleStates`]: active module effect cycles and online states (`OnGodmaShipEffect`, `GetAllInfo`).
//! - [`ShadowBallpark`]: space ballpark state (`ego`, `stamp`, `balls`, `slim_items`, `damage_states`, `special_fx_log`).
//! - [`ShadowSession`]: client session attributes (`OnSessionChanged` / `SessionChangeNotification`).

#![forbid(unsafe_code)]

use eve_db::{InventoryItemRecord, WorldRepository, WorldStateSnapshot};
use eve_proto::PyRep;
use eve_sde::{attr_ids, SdeRegistry};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const DST_BALL_GOTO: u8 = 0;
const DST_BALL_FOLLOW: u8 = 1;
const DST_BALL_STOP: u8 = 2;
const DST_BALL_WARP: u8 = 3;
const DST_BALL_ORBIT: u8 = 4;
const DST_BALL_MISSILE: u8 = 5;
const DST_BALL_MUSHROOM: u8 = 6;
const DST_BALL_BOID: u8 = 7;
const DST_BALL_TROLL: u8 = 8;
const DST_BALL_MINIBALL: u8 = 9;
const DST_BALL_FIELD: u8 = 10;
const DST_BALL_RIGID: u8 = 11;
const DST_BALL_FORMATION: u8 = 12;
const BALL_FLAG_IS_FREE: u8 = 0x01;

/// Returns `true` when `flag_id` is a ship fitting slot (`11..=34 | 92..=94 | 125..=132 | 164..=171`).
pub fn is_fitting_slot_flag(flag_id: u16) -> bool {
    matches!(flag_id, 11..=34 | 92..=94 | 125..=132 | 164..=171)
}

/// Returns `true` when `flag_id` is a charge-capable module slot (`11..=34 | 125..=132`).
pub fn is_charge_slot_flag(flag_id: u16) -> bool {
    matches!(flag_id, 11..=34 | 125..=132)
}

/// Returns `true` when `item` is a Charge (`category_id == 8`).
pub fn is_charge_record(item: &InventoryItemRecord, sde: Option<&SdeRegistry>) -> bool {
    if let Some(t) = sde.and_then(|s| s.types.get(&item.type_id)) {
        return t.category_id == 8;
    }
    !item.singleton && is_charge_slot_flag(item.flag as u16)
}

/// Structured divergence between the [`ShadowClient`] replica and authoritative server state.
#[derive(Debug, Clone, PartialEq)]
pub enum ShadowDivergence {
    MissingShadowItem { item_id: i64, type_id: u32, location_id: i64, flag_id: u16, quantity: i32 },
    StaleShadowItem { item_id: i64, type_id: u32, location_id: i64, flag_id: u16, quantity: i32 },
    ItemFieldMismatch { item_id: i64, field: &'static str, shadow_value: String, world_value: String },
    MissingLoadedChargeSublocation { ship_id: i64, flag_id: u16, expected_type_id: u32, expected_quantity: i32 },
    StaleLoadedChargeSublocation { ship_id: i64, flag_id: u16, shadow_type_id: u32, shadow_quantity: i32 },
    LoadedChargeSublocationMismatch {
        ship_id: i64,
        flag_id: u16,
        shadow_type_id: u32,
        shadow_quantity: i32,
        world_type_id: u32,
        world_quantity: i32,
    },
    UnprimedFittedItemChange { ship_id: i64, item_id: i64, old_flag_id: u16, detail: String },
    PersonalWalletMismatch { char_id: i64, shadow_isk: f64, world_isk: f64 },
    CorpWalletMismatch { corp_id: i64, division: u16, shadow_isk: f64, world_isk: f64 },
    ActiveModuleSetMismatch { shadow_active: Vec<i64>, world_active: Vec<i64> },
    ModuleOnlineStateMismatch { module_id: i64, shadow_online: bool, world_online: bool },
    BallparkMissingBall { ball_id: i64 },
    BallparkStaleBall { ball_id: i64 },
    BallparkFieldMismatch { ball_id: i64, field: &'static str, shadow_value: String, world_value: String },
    SessionFieldMismatch { field: &'static str, shadow_value: String, world_value: String },
}

impl fmt::Display for ShadowDivergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ShadowDivergence {}

/// Replicated inventory item row in [`ShadowInventory`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowItem {
    pub item_id: i64,
    pub type_id: u32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: u16,
    pub quantity: i32,
    pub singleton: bool,
    pub is_blueprint_copy: bool,
    pub group_id: Option<u32>,
    pub category_id: Option<u32>,
    pub custom_info: String,
}

impl ShadowItem {
    pub fn from_record(rec: &InventoryItemRecord, sde: Option<&SdeRegistry>) -> Self {
        let (group_id, category_id) = sde
            .and_then(|s| s.types.get(&rec.type_id))
            .map(|t| (Some(t.group_id), Some(t.category_id)))
            .unwrap_or((None, None));
        let is_charge = category_id == Some(8);
        let singleton = if is_fitting_slot_flag(rec.flag as u16) && !is_charge {
            true
        } else {
            rec.singleton
        };
        let quantity = if singleton { 1 } else { rec.quantity };
        Self {
            item_id: rec.item_id,
            type_id: rec.type_id,
            owner_id: rec.owner_id,
            location_id: rec.location_id,
            flag_id: rec.flag as u16,
            quantity,
            singleton,
            is_blueprint_copy: rec.is_blueprint_copy,
            group_id,
            category_id,
            custom_info: String::new(),
        }
    }

    pub fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let get_i64 = |name: &str| rep.dbrow_get(name).or_else(|| rep.dict_get(name)).and_then(|v| v.as_i64());
        let item_id = get_i64("itemID")?;
        let type_id = u32::try_from(get_i64("typeID")?).ok()?;
        let owner_id = get_i64("ownerID").unwrap_or(0);
        let location_id = get_i64("locationID")?;
        let flag_id = u16::try_from(get_i64("flagID").unwrap_or(0).max(0)).unwrap_or(0);
        let raw_qty = get_i64("quantity").unwrap_or(1) as i32;
        let explicit_stack = get_i64("stacksize").map(|v| v as i32);
        let explicit_sin = rep
            .dbrow_get("singleton")
            .or_else(|| rep.dict_get("singleton"))
            .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|i| i != 0)));
        let (quantity, singleton, is_blueprint_copy) = if raw_qty == -2 {
            (explicit_stack.unwrap_or(1), true, true)
        } else if raw_qty < 0 {
            (explicit_stack.unwrap_or(1), true, false)
        } else {
            let sin = explicit_sin.unwrap_or(false);
            let qty = explicit_stack.unwrap_or(raw_qty);
            (if sin { 1 } else { qty }, sin, false)
        };
        Some(Self {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag_id,
            quantity,
            singleton,
            is_blueprint_copy,
            group_id: get_i64("groupID").and_then(|v| u32::try_from(v).ok()),
            category_id: get_i64("categoryID").and_then(|v| u32::try_from(v).ok()),
            custom_info: rep
                .dbrow_get("customInfo")
                .or_else(|| rep.dict_get("customInfo"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }
}

/// Replicated charge sublocation in [`ShadowInventory`] (`(ship_id, flag_id, type_id)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowChargeSublocation {
    pub location_id: i64,
    pub flag_id: u16,
    pub type_id: u32,
    pub quantity: i32,
}

/// Client inventory & sublocation replica (`invCache` + `godma.stateManager`).
#[derive(Debug, Clone, Default)]
pub struct ShadowInventory {
    pub items: BTreeMap<i64, ShadowItem>,
    pub sublocations_by_ship_flag: BTreeMap<(i64, u16), ShadowChargeSublocation>,
    pub primed_ships: BTreeSet<i64>,
    pub listed_containers: BTreeSet<i64>,
    pub full_world_synced: bool,
}

/// Client wallet replica (`wallet.py`).
#[derive(Debug, Clone, Default)]
pub struct ShadowWallet {
    pub personal_balance_isk: Option<f64>,
    pub corp_division_balances_isk: BTreeMap<(i64, u16), f64>,
}

/// Client module effect & online state replica (`godma` / `shipui`).
#[derive(Debug, Clone, Default)]
pub struct ShadowModuleStates {
    pub active_modules: BTreeSet<i64>,
    pub active_effects_by_module: BTreeMap<i64, BTreeSet<i64>>,
    pub online_states: BTreeMap<i64, bool>,
}

/// Replicated movement mode in [`ShadowBallpark`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowBallMode {
    Goto,
    Follow,
    Stop,
    Warp,
    Orbit,
    Missile,
    Mushroom,
    Boid,
    Troll,
    Miniball,
    Field,
    Rigid,
    Formation,
    Unknown(u8),
}

impl ShadowBallMode {
    pub fn from_mode_byte(mode: u8) -> Self {
        match mode {
            DST_BALL_GOTO => Self::Goto,
            DST_BALL_FOLLOW => Self::Follow,
            DST_BALL_STOP => Self::Stop,
            DST_BALL_WARP => Self::Warp,
            DST_BALL_ORBIT => Self::Orbit,
            DST_BALL_MISSILE => Self::Missile,
            DST_BALL_MUSHROOM => Self::Mushroom,
            DST_BALL_BOID => Self::Boid,
            DST_BALL_TROLL => Self::Troll,
            DST_BALL_MINIBALL => Self::Miniball,
            DST_BALL_FIELD => Self::Field,
            DST_BALL_RIGID => Self::Rigid,
            DST_BALL_FORMATION => Self::Formation,
            other => Self::Unknown(other),
        }
    }
}

/// Replicated ball in [`ShadowBallpark`].
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowBall {
    pub ball_id: i64,
    pub mode: ShadowBallMode,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub radius: f64,
    pub max_velocity: Option<f64>,
    pub speed_fraction: f64,
    pub is_free: bool,
    pub is_cloaked: bool,
    pub target_id: Option<i64>,
    pub follow_range: Option<f64>,
    pub warp_destination: Option<[f64; 3]>,
    pub warp_speed_milli_au_s: Option<u32>,
}

/// Replicated shield/armor/hull damage state in [`ShadowBallpark`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowDamageState {
    pub shield: f64,
    pub armor: f64,
    pub hull: f64,
}

/// Replicated `OnSpecialFX` event recorded by [`ShadowBallpark`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowSpecialFx {
    pub ship_id: i64,
    pub module_id: Option<i64>,
    pub module_type_id: Option<i64>,
    pub target_id: Option<i64>,
    pub guid: String,
    pub start: bool,
    pub active: bool,
}

/// Client space ballpark replica (`michelle.Ballpark`).
#[derive(Debug, Clone, Default)]
pub struct ShadowBallpark {
    pub ego_ball_id: Option<i64>,
    pub current_stamp: Option<u32>,
    pub balls: BTreeMap<i64, ShadowBall>,
    pub slim_items: BTreeMap<i64, BTreeMap<String, PyRep>>,
    pub damage_states: BTreeMap<i64, ShadowDamageState>,
    pub special_fx_log: Vec<ShadowSpecialFx>,
}

/// Client session replica (`carbon.common.script.sys.sessions`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShadowSession {
    pub user_id: Option<u64>,
    pub char_id: Option<i64>,
    pub corp_id: Option<i64>,
    pub alliance_id: Option<i64>,
    pub war_faction_id: Option<i64>,
    pub station_id: Option<i64>,
    pub structure_id: Option<i64>,
    pub solar_system_id: Option<u32>,
    pub solar_system_id2: Option<u32>,
    pub constellation_id: Option<u32>,
    pub region_id: Option<u32>,
    pub ship_id: Option<i64>,
    pub fleet_id: Option<i64>,
    pub wing_id: Option<i64>,
    pub squad_id: Option<i64>,
    pub role: Option<u64>,
    pub synced: bool,
}

/// Shadow client state machine maintaining a V24.01 client replica from server notifications
/// and comparing it against authoritative server state (`#930`, RFC-3 `§3`).
#[derive(Debug, Clone)]
pub struct ShadowClient {
    pub char_id: i64,
    pub corp_id: Option<i64>,
    pub inventory: ShadowInventory,
    pub wallet: ShadowWallet,
    pub module_states: ShadowModuleStates,
    pub ballpark: ShadowBallpark,
    pub session: ShadowSession,
    pub recorded_divergences: Vec<ShadowDivergence>,
}

impl ShadowClient {
    pub fn new(char_id: i64) -> Self {
        Self {
            char_id,
            corp_id: None,
            inventory: ShadowInventory::default(),
            wallet: ShadowWallet::default(),
            module_states: ShadowModuleStates::default(),
            ballpark: ShadowBallpark::default(),
            session: ShadowSession { char_id: Some(char_id), ..ShadowSession::default() },
            recorded_divergences: Vec::new(),
        }
    }

    pub fn from_world(char_id: i64, db: &WorldRepository, sde: Option<&SdeRegistry>) -> Self {
        let mut client = Self::new(char_id);
        client.sync_from_world(db, sde);
        client
    }

    pub fn sync_from_world(&mut self, db: &WorldRepository, sde: Option<&SdeRegistry>) {
        db.with_snapshot(|snap| self.sync_from_snapshot(snap, sde));
    }

    pub fn sync_from_snapshot(&mut self, snap: &WorldStateSnapshot, sde: Option<&SdeRegistry>) {
        self.recorded_divergences.clear();
        self.inventory.items.clear();
        self.inventory.sublocations_by_ship_flag.clear();
        self.inventory.primed_ships.clear();
        self.inventory.full_world_synced = true;

        let active_ship_id = snap.characters.get(&self.char_id).map(|c| c.active_ship_id);
        if let Some(ship_id) = active_ship_id.filter(|&id| id > 0) {
            self.inventory.primed_ships.insert(ship_id);
        }

        for item in snap.items.values() {
            if item.owner_id != self.char_id
                || is_tombstone_location_or_flag(item.location_id, item.flag as u16, item.quantity)
            {
                continue;
            }
            if active_ship_id == Some(item.location_id)
                && is_charge_slot_flag(item.flag as u16)
                && is_charge_record(item, sde)
                && item.quantity > 0
            {
                self.inventory.sublocations_by_ship_flag.insert(
                    (item.location_id, item.flag as u16),
                    ShadowChargeSublocation {
                        location_id: item.location_id,
                        flag_id: item.flag as u16,
                        type_id: item.type_id,
                        quantity: item.quantity,
                    },
                );
            }
            self.inventory.items.insert(item.item_id, ShadowItem::from_record(item, sde));
        }

        if let Some(ch) = snap.characters.get(&self.char_id) {
            self.corp_id = Some(ch.corporation_id);
            self.wallet.personal_balance_isk = Some(ch.balance.to_wire_f64());
            if let Some(corp) = snap.corporations.get(&ch.corporation_id) {
                for div in 1000..=1006u16 {
                    let div_id = ((div - 1000) + 1) as u8;
                    self.wallet
                        .corp_division_balances_isk
                        .insert((ch.corporation_id, div), corp.division_balance(div_id).to_wire_f64());
                }
            }
            let (wing_id, squad_id) = ch
                .fleet_id
                .and_then(|fid| snap.fleets.get(&fid))
                .and_then(|f| f.member_details.get(&ch.character_id))
                .map(|m| (m.wing_id, m.squad_id))
                .unwrap_or((None, None));
            self.session = ShadowSession {
                user_id: Some(ch.user_id),
                char_id: Some(ch.character_id),
                corp_id: Some(ch.corporation_id),
                alliance_id: ch.alliance_id,
                war_faction_id: ch.war_faction_id,
                station_id: ch.station_id,
                structure_id: ch.structure_id,
                solar_system_id: Some(ch.solar_system_id),
                solar_system_id2: Some(ch.solar_system_id),
                constellation_id: None,
                region_id: None,
                ship_id: (ch.active_ship_id > 0).then_some(ch.active_ship_id),
                fleet_id: ch.fleet_id,
                wing_id,
                squad_id,
                role: None,
                synced: true,
            };
        }

        self.module_states.active_modules.clear();
        self.module_states.active_effects_by_module.clear();
        self.module_states.online_states.clear();
        for item in snap.items.values() {
            if Some(item.location_id) == active_ship_id
                && is_fitting_slot_flag(item.flag as u16)
                && !is_charge_record(item, sde)
            {
                let is_online = snap.module_online_states.get(&item.item_id).copied().unwrap_or(true);
                self.module_states.online_states.insert(item.item_id, is_online);
                if snap.module_active_states.contains(&item.item_id) {
                    self.module_states.active_modules.insert(item.item_id);
                    self.module_states.active_effects_by_module.entry(item.item_id).or_default().insert(-1);
                }
            }
        }
    }

    /// Synchronizes ship fitting, module states, and loaded charge sublocations from `dogmaIM.GetAllInfo`.
    pub fn sync_from_get_all_info(&mut self, all_info: &PyRep) {
        let Some(ship_id) = all_info.dict_get("shipID").and_then(|v| v.as_i64()) else { return };
        if let Some(char_id) = all_info.dict_get("charID").and_then(|v| v.as_i64()) {
            self.char_id = char_id;
            self.session.char_id = Some(char_id);
        }
        self.session.ship_id = Some(ship_id);
        if let Some(ship_info) = all_info.dict_get("shipInfo").filter(|v| !matches!(v, PyRep::None)) {
            self.prime_ship_from_ship_info_dict(ship_id, ship_info);
        }
        if let Some(PyRep::Tuple(ship_state)) = all_info.dict_get("shipState") {
            if let Some(PyRep::Dict(ship_flag_cache)) = ship_state.get(1) {
                for (k, v) in ship_flag_cache {
                    if k.as_i64() == Some(ship_id) {
                        if let PyRep::Dict(flags_dict) = v {
                            for (flag_rep, row_rep) in flags_dict {
                                let flag_id = flag_rep
                                    .as_i64()
                                    .or_else(|| row_rep.dbrow_get("flagID").and_then(|x| x.as_i64()))
                                    .unwrap_or(0) as u16;
                                let type_id = row_rep
                                    .dbrow_get("typeID")
                                    .or_else(|| row_rep.dict_get("typeID"))
                                    .and_then(|x| x.as_i64())
                                    .unwrap_or(0) as u32;
                                let quantity = row_rep
                                    .dbrow_get("quantity")
                                    .or_else(|| row_rep.dict_get("quantity"))
                                    .and_then(|x| x.as_i64())
                                    .unwrap_or(0) as i32;
                                if flag_id > 0 && type_id > 0 && quantity > 0 {
                                    self.inventory
                                        .sublocations_by_ship_flag
                                        .entry((ship_id, flag_id))
                                        .or_insert(ShadowChargeSublocation {
                                            location_id: ship_id,
                                            flag_id,
                                            type_id,
                                            quantity,
                                        });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn sync_from_inventory_list(&mut self, container_location_id: i64, list_rsp: &PyRep) {
        self.inventory.listed_containers.insert(container_location_id);
        self.inventory.items.retain(|_, item| item.location_id != container_location_id);
        let rows: Option<&[PyRep]> = match list_rsp {
            PyRep::ObjectEx { list, .. } | PyRep::List(list) => Some(list.as_slice()),
            _ => None,
        };
        for row in rows.unwrap_or_default() {
            if let Some(item) = ShadowItem::from_pyrep(row) {
                if !is_tombstone_location_or_flag(item.location_id, item.flag_id, item.quantity) {
                    self.inventory.items.insert(item.item_id, item);
                }
            }
        }
    }

    pub fn sync_from_wallet_balance(&mut self, balance_rsp: &PyRep) {
        if let Some(bal) = balance_rsp.as_f64() {
            self.wallet.personal_balance_isk = Some(bal);
        }
    }

    pub fn sync_corp_wallet_balance(&mut self, corp_id: i64, division: u16, balance_rsp: &PyRep) {
        if let Some(bal) = balance_rsp.as_f64() {
            self.corp_id = Some(corp_id);
            self.wallet.corp_division_balances_isk.insert((corp_id, normalize_corp_division(division)), bal);
        }
    }

    pub fn apply_notifications<'a, I>(&mut self, notifications: I)
    where
        I: IntoIterator<Item = (&'a str, &'a [PyRep])>,
    {
        for (event, args) in notifications {
            self.apply_notification(event, args);
        }
    }

    pub fn apply_notification(&mut self, event: &str, args: &[PyRep]) {
        match event {
            "OnItemChange" => self.apply_on_item_change(args),
            "OnMultipleItemChange" => self.apply_on_multiple_item_change(args),
            "OnAccountChange" => self.apply_on_account_change(args),
            "OnModuleAttributeChanges" => self.apply_on_module_attribute_changes(args),
            "OnModuleAttributeChange" => self.apply_single_module_attribute_change(args),
            "OnGodmaShipEffect" => self.apply_on_godma_ship_effect(args),
            "OnGodmaPrimeItem" => self.apply_on_godma_prime_item(args),
            "ProcessGodmaPrimeLocation" => self.apply_process_godma_prime_location(args),
            "DoDestinyUpdate" => self.apply_do_destiny_update(args),
            "DoDestinyUpdates" => {
                if let Some(PyRep::List(batches)) = args.first() {
                    for batch in batches {
                        self.apply_do_destiny_update(std::slice::from_ref(batch));
                    }
                }
            }
            "OnDamageStateChange" => self.apply_on_damage_state_change(args),
            "OnSlimItemChange" => self.apply_on_slim_item_change(args),
            "OnSpecialFX" => self.apply_on_special_fx(args),
            "OnSessionChanged" | "SessionChangeNotification" => self.apply_session_change(args),
            "OnMultiEvent" => {
                if let Some(PyRep::List(events)) = args.first() {
                    for entry in events {
                        if let PyRep::Tuple(outer) = entry {
                            if let Some(PyRep::Tuple(inner)) = outer.first() {
                                if let Some(ev_name) = inner.first().and_then(|v| v.as_str()) {
                                    self.apply_notification(ev_name, &inner[1..]);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    pub fn apply_on_item_change(&mut self, args: &[PyRep]) {
        let Some(new_item) = args.first().and_then(ShadowItem::from_pyrep) else { return };
        let change_dict = args.get(1);
        let old_location_id = Some(
            extract_change_dict_i64(change_dict, 3)
                .or_else(|| self.inventory.items.get(&new_item.item_id).map(|i| i.location_id))
                .unwrap_or(new_item.location_id),
        );
        let old_flag_id = Some(
            extract_change_dict_i64(change_dict, 4)
                .and_then(|v| u16::try_from(v.max(0)).ok())
                .or_else(|| self.inventory.items.get(&new_item.item_id).map(|i| i.flag_id))
                .unwrap_or(new_item.flag_id),
        );

        if let (Some(old_loc), Some(old_flag)) = (old_location_id, old_flag_id) {
            if self.inventory.primed_ships.contains(&old_loc)
                && is_fitting_slot_flag(old_flag)
                && (old_loc != new_item.location_id || old_flag != new_item.flag_id)
            {
                let was_primed_item = self.inventory.items.contains_key(&new_item.item_id);
                let was_primed_subloc = self
                    .inventory
                    .sublocations_by_ship_flag
                    .get(&(old_loc, old_flag))
                    .is_some_and(|s| s.type_id == new_item.type_id);
                if !was_primed_item && !was_primed_subloc {
                    self.recorded_divergences.push(ShadowDivergence::UnprimedFittedItemChange {
                        ship_id: old_loc,
                        item_id: new_item.item_id,
                        old_flag_id: old_flag,
                        detail: format!(
                            "OnItemChange moved unprimed item {} (type {}) from ship {} flag {} -> location {} flag {} (godma.py:1512 KeyError)",
                            new_item.item_id, new_item.type_id, old_loc, old_flag, new_item.location_id, new_item.flag_id
                        ),
                    });
                }
            }
        }

        if is_tombstone_location_or_flag(new_item.location_id, new_item.flag_id, new_item.quantity) {
            self.inventory.items.remove(&new_item.item_id);
            self.module_states.active_modules.remove(&new_item.item_id);
            self.module_states.active_effects_by_module.remove(&new_item.item_id);
            self.module_states.online_states.remove(&new_item.item_id);
            if let (Some(old_loc), Some(old_flag)) = (old_location_id, old_flag_id) {
                if self.inventory.sublocations_by_ship_flag.get(&(old_loc, old_flag)).is_some_and(|s| s.type_id == new_item.type_id) {
                    self.inventory.sublocations_by_ship_flag.remove(&(old_loc, old_flag));
                }
            }
            return;
        }

        if let (Some(old_loc), Some(old_flag)) = (old_location_id, old_flag_id) {
            if (old_loc != new_item.location_id || old_flag != new_item.flag_id)
                && self.inventory.sublocations_by_ship_flag.get(&(old_loc, old_flag)).is_some_and(|s| s.type_id == new_item.type_id)
            {
                self.inventory.sublocations_by_ship_flag.remove(&(old_loc, old_flag));
            }
        }
        self.inventory.items.insert(new_item.item_id, new_item);
    }

    pub fn apply_on_multiple_item_change(&mut self, args: &[PyRep]) {
        if let Some(PyRep::List(items)) = args.first() {
            for entry in items {
                if let PyRep::Tuple(pair) | PyRep::List(pair) = entry {
                    self.apply_on_item_change(pair);
                }
            }
        }
    }

    pub fn apply_on_account_change(&mut self, args: &[PyRep]) {
        if args.len() < 3 {
            return;
        }
        let account_key = args[0].as_str().unwrap_or("cash");
        let (Some(owner_id), Some(new_balance)) = (args[1].as_i64(), args[2].as_f64()) else { return };
        if owner_id == self.char_id && (account_key == "cash" || account_key == "1000") {
            self.wallet.personal_balance_isk = Some(new_balance);
        } else if let Some(div_key) = parse_corp_account_key(account_key) {
            self.corp_id.get_or_insert(owner_id);
            self.wallet.corp_division_balances_isk.insert((owner_id, div_key), new_balance);
        }
    }

    pub fn apply_on_module_attribute_changes(&mut self, args: &[PyRep]) {
        if let Some(PyRep::List(entries)) = args.first() {
            for entry in entries {
                if let PyRep::Tuple(tuple) | PyRep::List(tuple) = entry {
                    let slice = if tuple.first().and_then(|v| v.as_str()) == Some("OnModuleAttributeChange") {
                        &tuple[1..]
                    } else {
                        tuple.as_slice()
                    };
                    self.apply_single_module_attribute_change(slice);
                }
            }
        }
    }

    pub fn apply_single_module_attribute_change(&mut self, args: &[PyRep]) {
        if args.len() < 5 {
            return;
        }
        let item_key = &args[1];
        let (Some(attr_id), Some(new_val)) = (args[2].as_i64(), args[4].as_f64()) else { return };
        if let PyRep::Tuple(subloc) = item_key {
            if subloc.len() == 3 {
                if let (Some(ship_id), Some(flag_i64), Some(type_i64)) =
                    (subloc[0].as_i64(), subloc[1].as_i64(), subloc[2].as_i64())
                {
                    if attr_id == i64::from(attr_ids::QUANTITY) || attr_id == 80 {
                        let flag_id = flag_i64.max(0) as u16;
                        let type_id = type_i64.max(0) as u32;
                        let qty = new_val.round() as i32;
                        if qty <= 0 {
                            self.inventory.sublocations_by_ship_flag.remove(&(ship_id, flag_id));
                        } else {
                            self.inventory.sublocations_by_ship_flag.insert(
                                (ship_id, flag_id),
                                ShadowChargeSublocation { location_id: ship_id, flag_id, type_id, quantity: qty },
                            );
                        }
                    }
                }
            }
        } else if let Some(module_id) = item_key.as_i64() {
            if attr_id == 2 {
                let is_online = new_val > 0.5;
                self.module_states.online_states.insert(module_id, is_online);
                if !is_online {
                    self.module_states.active_modules.remove(&module_id);
                    self.module_states.active_effects_by_module.remove(&module_id);
                }
            }
        }
    }

    pub fn apply_on_godma_ship_effect(&mut self, args: &[PyRep]) {
        if args.len() >= 5 {
            let Some(item_id) = args[0].as_i64() else { return };
            let effect_id = args[1].as_i64().unwrap_or(0);
            let active = pyrep_as_truthy(&args[3]) && pyrep_as_truthy(&args[4]);
            self.set_module_effect_state(item_id, effect_id, active);
        } else if args.len() == 3 {
            let Some(item_id) = args[0].as_i64() else { return };
            let effect_id = args[1].as_i64().unwrap_or(-1);
            let active = pyrep_as_truthy(&args[2]);
            self.set_module_effect_state(item_id, effect_id, active);
        }
    }

    fn set_module_effect_state(&mut self, item_id: i64, effect_id: i64, active: bool) {
        if effect_id == 16 {
            self.module_states.online_states.insert(item_id, active);
            if !active {
                self.module_states.active_modules.remove(&item_id);
                self.module_states.active_effects_by_module.remove(&item_id);
            }
        } else if active {
            self.module_states.active_effects_by_module.entry(item_id).or_default().insert(effect_id);
            self.module_states.active_modules.insert(item_id);
        } else if effect_id < 0 {
            self.module_states.active_effects_by_module.remove(&item_id);
            self.module_states.active_modules.remove(&item_id);
        } else if let Some(effs) = self.module_states.active_effects_by_module.get_mut(&item_id) {
            effs.remove(&effect_id);
            if effs.is_empty() {
                self.module_states.active_effects_by_module.remove(&item_id);
                self.module_states.active_modules.remove(&item_id);
            }
        } else {
            self.module_states.active_modules.remove(&item_id);
        }
    }

    pub fn apply_on_godma_prime_item(&mut self, args: &[PyRep]) {
        if args.len() >= 2 {
            if let Some(ship_id) = args[0].as_i64() {
                self.apply_primed_ship_entry(ship_id, args[1].dict_get("itemID"), &args[1]);
            }
        }
    }

    pub fn apply_process_godma_prime_location(&mut self, args: &[PyRep]) {
        if args.len() >= 2 {
            if let Some(ship_id) = args[0].as_i64() {
                self.prime_ship_from_ship_info_dict(ship_id, &args[1]);
            }
        }
    }

    fn prime_ship_from_ship_info_dict(&mut self, ship_id: i64, ship_info: &PyRep) {
        let PyRep::Dict(entries) = ship_info else { return };
        self.inventory.primed_ships.insert(ship_id);
        self.inventory.sublocations_by_ship_flag.retain(|&(loc_id, _), _| loc_id != ship_id);
        let ship_mod_ids: Vec<i64> = self
            .inventory
            .items
            .values()
            .filter(|i| i.location_id == ship_id && is_fitting_slot_flag(i.flag_id))
            .map(|i| i.item_id)
            .collect();
        for mid in ship_mod_ids {
            self.module_states.active_modules.remove(&mid);
            self.module_states.active_effects_by_module.remove(&mid);
            self.module_states.online_states.remove(&mid);
        }
        for (key, kv) in entries {
            self.apply_primed_ship_entry(ship_id, Some(key), kv);
        }
    }

    fn apply_primed_ship_entry(&mut self, ship_id: i64, key_opt: Option<&PyRep>, kv: &PyRep) {
        let item_key = key_opt.or_else(|| kv.dict_get("itemID"));
        if let Some(PyRep::Tuple(parts)) = item_key {
            if parts.len() == 3 {
                if let (Some(loc_id), Some(flag_i64), Some(type_i64)) =
                    (parts[0].as_i64(), parts[1].as_i64(), parts[2].as_i64())
                {
                    let (flag_id, type_id) = (flag_i64.max(0) as u16, type_i64.max(0) as u32);
                    let qty = extract_attr_f64(kv.dict_get("attributes"), attr_ids::QUANTITY)
                        .map(|v| v.round() as i32)
                        .unwrap_or(1);
                    if flag_id > 0 && type_id > 0 && qty > 0 {
                        self.inventory.sublocations_by_ship_flag.insert(
                            (loc_id, flag_id),
                            ShadowChargeSublocation { location_id: loc_id, flag_id, type_id, quantity: qty },
                        );
                    }
                }
            }
            return;
        }
        let Some(shadow_item) = kv.dict_get("invItem").and_then(ShadowItem::from_pyrep) else { return };
        let item_id = shadow_item.item_id;
        let is_fitted_mod = shadow_item.location_id == ship_id
            && is_fitting_slot_flag(shadow_item.flag_id)
            && shadow_item.category_id != Some(8);
        self.inventory.items.insert(item_id, shadow_item);
        if is_fitted_mod {
            let mut is_online = extract_attr_f64(kv.dict_get("attributes"), 2).is_some_and(|v| v > 0.5);
            let mut active_effs = BTreeSet::new();
            if let Some(PyRep::Dict(eff_entries)) = kv.dict_get("activeEffects") {
                for (eff_k, _) in eff_entries {
                    if let Some(eff_id) = eff_k.as_i64() {
                        if eff_id == 16 {
                            is_online = true;
                        } else {
                            active_effs.insert(eff_id);
                        }
                    }
                }
            }
            self.module_states.online_states.insert(item_id, is_online);
            if !active_effs.is_empty() {
                self.module_states.active_modules.insert(item_id);
                self.module_states.active_effects_by_module.insert(item_id, active_effs);
            } else {
                self.module_states.active_modules.remove(&item_id);
                self.module_states.active_effects_by_module.remove(&item_id);
            }
        }
    }

    pub fn apply_do_destiny_update(&mut self, args: &[PyRep]) {
        let Some(first) = args.first() else { return };
        let entries: &[PyRep] = match first {
            PyRep::List(list) => list.as_slice(),
            PyRep::Tuple(outer) => match outer.first() {
                Some(PyRep::List(list)) => list.as_slice(),
                _ => outer.as_slice(),
            },
            _ => return,
        };
        for entry in entries {
            let PyRep::Tuple(stamp_and_action) = entry else { continue };
            if stamp_and_action.len() < 2 {
                continue;
            }
            if let Some(stamp) = stamp_and_action[0].as_i64().filter(|&s| s >= 0) {
                self.ballpark.current_stamp = Some(stamp as u32);
            }
            let PyRep::Tuple(action_pair) = &stamp_and_action[1] else { continue };
            if action_pair.len() < 2 {
                continue;
            }
            let Some(action_name) = action_pair[0].as_str() else { continue };
            let action_args: &[PyRep] = match &action_pair[1] {
                PyRep::Tuple(t) | PyRep::List(t) => t.as_slice(),
                other => std::slice::from_ref(other),
            };
            self.apply_destiny_action(action_name, action_args);
        }
    }

    fn apply_destiny_action(&mut self, action_name: &str, action_args: &[PyRep]) {
        match action_name {
            "SetState" => {
                let Some(state_kv) = action_args.first() else { return };
                self.ballpark.balls.clear();
                self.ballpark.slim_items.clear();
                self.ballpark.damage_states.clear();
                if let Some(ego) = state_kv.dict_get("ego").and_then(|v| v.as_i64()) {
                    self.ballpark.ego_ball_id = Some(ego);
                }
                if let Some(stamp) = state_kv.dict_get("stamp").and_then(|v| v.as_i64()).filter(|&s| s >= 0) {
                    self.ballpark.current_stamp = Some(stamp as u32);
                }
                if let Some(PyRep::Buffer(bytes) | PyRep::Bytes(bytes)) = state_kv.dict_get("state") {
                    if let Ok((_, stamp, decoded)) = decode_destiny_ball_buffer(bytes) {
                        self.ballpark.current_stamp.get_or_insert(stamp);
                        for b in decoded {
                            self.ballpark.balls.insert(b.ball_id, b);
                        }
                    }
                }
                if let Some(PyRep::List(slims)) = state_kv.dict_get("slims") {
                    for slim in slims {
                        self.insert_slim_item(slim);
                    }
                }
                if let Some(PyRep::Dict(dmg_entries)) = state_kv.dict_get("damageState") {
                    for (k, v) in dmg_entries {
                        if let (Some(ball_id), Some(dmg)) = (k.as_i64(), parse_damage_state_tuple(v)) {
                            self.ballpark.damage_states.insert(ball_id, dmg);
                        }
                    }
                }
            }
            "AddBalls2" | "AddBalls" => {
                let inner: &[PyRep] = match action_args.first() {
                    Some(PyRep::Tuple(t) | PyRep::List(t)) => t.as_slice(),
                    _ => action_args,
                };
                if let Some(PyRep::Buffer(bytes) | PyRep::Bytes(bytes)) = inner.first() {
                    if let Ok((_, _, decoded)) = decode_destiny_ball_buffer(bytes) {
                        for b in decoded {
                            self.ballpark.balls.insert(b.ball_id, b);
                        }
                    }
                }
                if let Some(PyRep::List(slims)) = inner.get(1) {
                    for slim in slims {
                        self.insert_slim_item(slim);
                    }
                }
                if let Some(PyRep::Dict(dmg_entries)) = inner.get(2) {
                    for (k, v) in dmg_entries {
                        if let (Some(ball_id), Some(dmg)) = (k.as_i64(), parse_damage_state_tuple(v)) {
                            self.ballpark.damage_states.insert(ball_id, dmg);
                        }
                    }
                }
            }
            "AddBall" if action_args.len() >= 4 => {
                if let (Some(ball_id), Some(x), Some(y), Some(z)) = (
                    action_args[0].as_i64(),
                    action_args[1].as_f64(),
                    action_args[2].as_f64(),
                    action_args[3].as_f64(),
                ) {
                    let radius = action_args.get(4).and_then(|v| v.as_f64()).unwrap_or(50.0);
                    self.ballpark.balls.insert(
                        ball_id,
                        ShadowBall {
                            ball_id,
                            mode: ShadowBallMode::Stop,
                            position: [x, y, z],
                            velocity: [0.0, 0.0, 0.0],
                            radius,
                            max_velocity: None,
                            speed_fraction: 0.0,
                            is_free: true,
                            is_cloaked: false,
                            target_id: None,
                            follow_range: None,
                            warp_destination: None,
                            warp_speed_milli_au_s: None,
                        },
                    );
                }
            }
            "RemoveBalls" => {
                if let Some(PyRep::List(ids) | PyRep::Tuple(ids)) = action_args.first() {
                    for ball_id in ids.iter().filter_map(PyRep::as_i64) {
                        self.remove_ball(ball_id);
                    }
                }
            }
            "RemoveBall" => {
                if let Some(ball_id) = action_args.first().and_then(PyRep::as_i64) {
                    self.remove_ball(ball_id);
                }
            }
            "GotoPoint" | "GotoDirection" => {
                if let Some(ball) = action_args.first().and_then(PyRep::as_i64).and_then(|id| self.ballpark.balls.get_mut(&id)) {
                    ball.mode = ShadowBallMode::Goto;
                    ball.target_id = None;
                    ball.follow_range = None;
                    ball.warp_destination = None;
                }
            }
            "Orbit" | "Follow" | "FollowBall" if action_args.len() >= 3 => {
                if let (Some(ball_id), Some(target_id), Some(range)) =
                    (action_args[0].as_i64(), action_args[1].as_i64(), action_args[2].as_f64())
                {
                    if let Some(ball) = self.ballpark.balls.get_mut(&ball_id) {
                        ball.mode = if action_name == "Orbit" { ShadowBallMode::Orbit } else { ShadowBallMode::Follow };
                        ball.target_id = Some(target_id);
                        ball.follow_range = Some(range);
                        ball.warp_destination = None;
                    }
                }
            }
            "Stop" => {
                if let Some(ball) = action_args.first().and_then(PyRep::as_i64).and_then(|id| self.ballpark.balls.get_mut(&id)) {
                    ball.mode = ShadowBallMode::Stop;
                    ball.speed_fraction = 0.0;
                    ball.velocity = [0.0, 0.0, 0.0];
                    ball.target_id = None;
                    ball.follow_range = None;
                    ball.warp_destination = None;
                }
            }
            "WarpTo" if action_args.len() >= 4 => {
                if let (Some(ball_id), Some(x), Some(y), Some(z)) = (
                    action_args[0].as_i64(),
                    action_args[1].as_f64(),
                    action_args[2].as_f64(),
                    action_args[3].as_f64(),
                ) {
                    let ws = action_args
                        .get(5)
                        .or_else(|| action_args.get(4))
                        .and_then(PyRep::as_i64)
                        .and_then(|v| u32::try_from(v.max(0)).ok());
                    if let Some(ball) = self.ballpark.balls.get_mut(&ball_id) {
                        ball.mode = ShadowBallMode::Warp;
                        ball.warp_destination = Some([x, y, z]);
                        ball.warp_speed_milli_au_s = ws;
                        ball.target_id = None;
                        ball.follow_range = None;
                    }
                }
            }
            "SetSpeedFraction" if action_args.len() >= 2 => {
                if let (Some(ball_id), Some(frac)) = (action_args[0].as_i64(), action_args[1].as_f64()) {
                    if let Some(ball) = self.ballpark.balls.get_mut(&ball_id) {
                        ball.speed_fraction = frac;
                    }
                }
            }
            "SetBallPosition" | "SetBallVelocity" if action_args.len() >= 4 => {
                if let (Some(ball_id), Some(x), Some(y), Some(z)) = (
                    action_args[0].as_i64(),
                    action_args[1].as_f64(),
                    action_args[2].as_f64(),
                    action_args[3].as_f64(),
                ) {
                    if let Some(ball) = self.ballpark.balls.get_mut(&ball_id) {
                        if action_name == "SetBallPosition" {
                            ball.position = [x, y, z];
                        } else {
                            ball.velocity = [x, y, z];
                        }
                    }
                }
            }
            "CloakBall" | "UncloakBall" => {
                if let Some(ball) = action_args.first().and_then(PyRep::as_i64).and_then(|id| self.ballpark.balls.get_mut(&id)) {
                    ball.is_cloaked = action_name == "CloakBall" && action_args.get(1).map_or(true, pyrep_as_truthy);
                }
            }
            "OnDamageStateChange" => self.apply_on_damage_state_change(action_args),
            "OnSlimItemChange" => self.apply_on_slim_item_change(action_args),
            "OnSpecialFX" => self.apply_on_special_fx(action_args),
            _ => {}
        }
    }

    fn remove_ball(&mut self, ball_id: i64) {
        self.ballpark.balls.remove(&ball_id);
        self.ballpark.slim_items.remove(&ball_id);
        self.ballpark.damage_states.remove(&ball_id);
    }

    fn insert_slim_item(&mut self, slim: &PyRep) {
        let unwrapped = match slim {
            PyRep::Object { args, .. } => args.as_ref(),
            other => other,
        };
        let Some(item_id) = unwrapped.dict_get("itemID").or_else(|| unwrapped.dbrow_get("itemID")).and_then(PyRep::as_i64) else {
            return;
        };
        let mut fields = BTreeMap::new();
        match unwrapped {
            PyRep::KeyVal(map) => {
                for (k, v) in map {
                    fields.insert(k.clone(), v.clone());
                }
            }
            PyRep::Dict(entries) => {
                for (k, v) in entries {
                    if let Some(ks) = k.as_str() {
                        fields.insert(ks.to_string(), v.clone());
                    }
                }
            }
            _ => {}
        }
        self.ballpark.slim_items.insert(item_id, fields);
    }

    fn apply_on_damage_state_change(&mut self, args: &[PyRep]) {
        if args.len() >= 2 {
            if let (Some(ball_id), Some(dmg)) = (args[0].as_i64(), parse_damage_state_tuple(&args[1])) {
                self.ballpark.damage_states.insert(ball_id, dmg);
            }
        }
    }

    fn apply_on_slim_item_change(&mut self, args: &[PyRep]) {
        if args.len() >= 2 {
            if let Some(ball_id) = args[0].as_i64() {
                self.insert_slim_item(&args[1]);
                self.ballpark.slim_items.entry(ball_id).or_default();
            }
        }
    }

    fn apply_on_special_fx(&mut self, args: &[PyRep]) {
        if args.len() < 9 {
            return;
        }
        let Some(ship_id) = args[0].as_i64() else { return };
        let guid = args[5].as_str().unwrap_or("").to_string();
        let start = pyrep_as_truthy(&args[7]);
        let active = pyrep_as_truthy(&args[8]);
        if matches!(guid.as_str(), "effects.Cloak" | "effects.CloakNoAmim" | "effects.Cloaking" | "effects.GateCloak") {
            if let Some(ball) = self.ballpark.balls.get_mut(&ship_id) {
                ball.is_cloaked = start && active;
            }
        }
        self.ballpark.special_fx_log.push(ShadowSpecialFx {
            ship_id,
            module_id: args[1].as_i64().filter(|&id| id > 0),
            module_type_id: args[2].as_i64().filter(|&id| id > 0),
            target_id: args[3].as_i64().filter(|&id| id > 0),
            guid,
            start,
            active,
        });
    }

    pub fn apply_session_change(&mut self, args: &[PyRep]) {
        let entries_opt = args.iter().rev().find_map(|arg| match arg {
            PyRep::Dict(entries) => Some(entries.as_slice()),
            PyRep::Tuple(inner) => inner.iter().rev().find_map(|sub| match sub {
                PyRep::Dict(entries) => Some(entries.as_slice()),
                _ => None,
            }),
            _ => None,
        });
        let Some(entries) = entries_opt else { return };
        self.session.synced = true;
        for (k, v) in entries {
            let Some(attr) = k.as_str() else { continue };
            let new_val = match v {
                PyRep::Tuple(pair) | PyRep::List(pair) if pair.len() >= 2 => &pair[1],
                other => other,
            };
            let opt_i64 = new_val.as_i64();
            match attr {
                "userid" => self.session.user_id = opt_i64.and_then(|x| u64::try_from(x).ok()),
                "charid" => {
                    self.session.char_id = opt_i64;
                    if let Some(cid) = opt_i64 {
                        self.char_id = cid;
                    }
                }
                "corpid" => {
                    self.session.corp_id = opt_i64;
                    self.corp_id = opt_i64;
                }
                "allianceid" => self.session.alliance_id = opt_i64.filter(|&x| x > 0),
                "warfactionid" | "factionid" => self.session.war_faction_id = opt_i64.filter(|&x| x > 0),
                "stationid" | "stationid2" => {
                    self.session.station_id = opt_i64.filter(|&x| x > 0);
                    if self.session.station_id.is_some() {
                        self.ballpark.balls.clear();
                        self.ballpark.slim_items.clear();
                        self.ballpark.damage_states.clear();
                    }
                }
                "structureid" => {
                    self.session.structure_id = opt_i64.filter(|&x| x > 0);
                    if self.session.structure_id.is_some() {
                        self.ballpark.balls.clear();
                        self.ballpark.slim_items.clear();
                        self.ballpark.damage_states.clear();
                    }
                }
                "solarsystemid" => self.session.solar_system_id = opt_i64.and_then(|x| u32::try_from(x).ok()),
                "solarsystemid2" => {
                    let sys = opt_i64.and_then(|x| u32::try_from(x).ok());
                    self.session.solar_system_id2 = sys;
                    if self.session.solar_system_id.is_none() {
                        self.session.solar_system_id = sys;
                    }
                }
                "constellationid" => self.session.constellation_id = opt_i64.and_then(|x| u32::try_from(x).ok()),
                "regionid" => self.session.region_id = opt_i64.and_then(|x| u32::try_from(x).ok()),
                "shipid" => self.session.ship_id = opt_i64.filter(|&x| x > 0),
                "fleetid" => self.session.fleet_id = opt_i64.filter(|&x| x > 0),
                "wingid" => self.session.wing_id = opt_i64.filter(|&x| x > 0),
                "squadid" => self.session.squad_id = opt_i64.filter(|&x| x > 0),
                "role" => self.session.role = opt_i64.and_then(|x| u64::try_from(x).ok()),
                _ => {}
            }
        }
    }

    pub fn compare_inventory_against_world(
        &self,
        db: &WorldRepository,
        sde: Option<&SdeRegistry>,
    ) -> Vec<ShadowDivergence> {
        db.with_snapshot(|snap| self.compare_inventory_against_snapshot(snap, sde))
    }

    pub fn compare_inventory_against_snapshot(
        &self,
        snap: &WorldStateSnapshot,
        sde: Option<&SdeRegistry>,
    ) -> Vec<ShadowDivergence> {
        let mut out = self.recorded_divergences.clone();

        for &ship_id in &self.inventory.primed_ships {
            let mut expected_sublocs: BTreeMap<u16, (u32, i32)> = BTreeMap::new();
            for item in snap.items.values() {
                if is_tombstone_location_or_flag(item.location_id, item.flag as u16, item.quantity) {
                    continue;
                }
                if item.item_id == ship_id {
                    if !self.inventory.items.contains_key(&ship_id) {
                        out.push(ShadowDivergence::MissingShadowItem {
                            item_id: ship_id,
                            type_id: item.type_id,
                            location_id: item.location_id,
                            flag_id: item.flag as u16,
                            quantity: 1,
                        });
                    }
                    continue;
                }
                if item.location_id != ship_id || !is_fitting_slot_flag(item.flag as u16) {
                    continue;
                }
                let flag_u16 = item.flag as u16;
                if is_charge_record(item, sde) && is_charge_slot_flag(flag_u16) {
                    if item.quantity > 0 {
                        expected_sublocs.insert(flag_u16, (item.type_id, item.quantity));
                    }
                } else {
                    match self.inventory.items.get(&item.item_id) {
                        None => out.push(ShadowDivergence::MissingShadowItem {
                            item_id: item.item_id,
                            type_id: item.type_id,
                            location_id: item.location_id,
                            flag_id: flag_u16,
                            quantity: 1,
                        }),
                        Some(sh) => compare_shadow_item_fields(sh, &ShadowItem::from_record(item, sde), &mut out),
                    }
                }
            }

            for (&flag_id, &(exp_type_id, exp_qty)) in &expected_sublocs {
                match self.inventory.sublocations_by_ship_flag.get(&(ship_id, flag_id)) {
                    None => out.push(ShadowDivergence::MissingLoadedChargeSublocation {
                        ship_id,
                        flag_id,
                        expected_type_id: exp_type_id,
                        expected_quantity: exp_qty,
                    }),
                    Some(subloc) if subloc.type_id != exp_type_id || subloc.quantity != exp_qty => {
                        out.push(ShadowDivergence::LoadedChargeSublocationMismatch {
                            ship_id,
                            flag_id,
                            shadow_type_id: subloc.type_id,
                            shadow_quantity: subloc.quantity,
                            world_type_id: exp_type_id,
                            world_quantity: exp_qty,
                        });
                    }
                    Some(_) => {}
                }
            }
            for (&(loc_id, flag_id), subloc) in &self.inventory.sublocations_by_ship_flag {
                if loc_id == ship_id && !expected_sublocs.contains_key(&flag_id) {
                    out.push(ShadowDivergence::StaleLoadedChargeSublocation {
                        ship_id,
                        flag_id,
                        shadow_type_id: subloc.type_id,
                        shadow_quantity: subloc.quantity,
                    });
                }
            }
        }

        if self.inventory.full_world_synced || !self.inventory.listed_containers.is_empty() {
            let in_scope = |loc_id: i64, owner_id: i64| {
                (self.inventory.full_world_synced && owner_id == self.char_id)
                    || self.inventory.listed_containers.contains(&loc_id)
            };
            let mut expected_items: BTreeMap<i64, ShadowItem> = BTreeMap::new();
            for item in snap.items.values() {
                if !is_tombstone_location_or_flag(item.location_id, item.flag as u16, item.quantity)
                    && in_scope(item.location_id, item.owner_id)
                {
                    expected_items.insert(item.item_id, ShadowItem::from_record(item, sde));
                }
            }
            for (item_id, exp) in &expected_items {
                match self.inventory.items.get(item_id) {
                    None => {
                        if !out.iter().any(|d| matches!(d, ShadowDivergence::MissingShadowItem { item_id: id, .. } if id == item_id)) {
                            out.push(ShadowDivergence::MissingShadowItem {
                                item_id: *item_id,
                                type_id: exp.type_id,
                                location_id: exp.location_id,
                                flag_id: exp.flag_id,
                                quantity: exp.quantity,
                            });
                        }
                    }
                    Some(sh) => compare_shadow_item_fields(sh, exp, &mut out),
                }
            }
            for (item_id, sh) in &self.inventory.items {
                if in_scope(sh.location_id, sh.owner_id) && !expected_items.contains_key(item_id) {
                    if let Some(world_rec) = snap.items.get(item_id).filter(|r| {
                        !is_tombstone_location_or_flag(r.location_id, r.flag as u16, r.quantity)
                    }) {
                        compare_shadow_item_fields(sh, &ShadowItem::from_record(world_rec, sde), &mut out);
                    } else {
                        out.push(ShadowDivergence::StaleShadowItem {
                            item_id: *item_id,
                            type_id: sh.type_id,
                            location_id: sh.location_id,
                            flag_id: sh.flag_id,
                            quantity: sh.quantity,
                        });
                    }
                }
            }
        }
        out
    }

    pub fn compare_wallet_against_world(&self, db: &WorldRepository) -> Vec<ShadowDivergence> {
        db.with_snapshot(|snap| self.compare_wallet_against_snapshot(snap))
    }

    pub fn compare_wallet_against_snapshot(&self, snap: &WorldStateSnapshot) -> Vec<ShadowDivergence> {
        let mut out = Vec::new();
        if let (Some(shadow_isk), Some(ch)) = (self.wallet.personal_balance_isk, snap.characters.get(&self.char_id)) {
            let world_isk = ch.balance.to_wire_f64();
            if (shadow_isk - world_isk).abs() > 0.005 {
                out.push(ShadowDivergence::PersonalWalletMismatch { char_id: self.char_id, shadow_isk, world_isk });
            }
        }
        for (&(corp_id, div_key), &shadow_isk) in &self.wallet.corp_division_balances_isk {
            if let Some(corp) = snap.corporations.get(&corp_id) {
                let div_id = if div_key >= 1000 {
                    (div_key - 999) as u8
                } else {
                    div_key as u8
                };
                let world_isk = corp.division_balance(div_id).to_wire_f64();
                if (shadow_isk - world_isk).abs() > 0.005 {
                    out.push(ShadowDivergence::CorpWalletMismatch { corp_id, division: div_key, shadow_isk, world_isk });
                }
            }
        }
        out
    }

    pub fn compare_active_modules_against_world(
        &self,
        db: &WorldRepository,
        sde: Option<&SdeRegistry>,
    ) -> Vec<ShadowDivergence> {
        db.with_snapshot(|snap| self.compare_active_modules_against_snapshot(snap, sde))
    }

    pub fn compare_active_modules_against_snapshot(
        &self,
        snap: &WorldStateSnapshot,
        sde: Option<&SdeRegistry>,
    ) -> Vec<ShadowDivergence> {
        let mut out = Vec::new();
        let active_ship_id = self
            .session
            .ship_id
            .or_else(|| snap.characters.get(&self.char_id).map(|c| c.active_ship_id));
        let Some(ship_id) = active_ship_id.filter(|&id| id > 0) else { return out };

        let ship_module_ids: BTreeSet<i64> = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && is_fitting_slot_flag(i.flag as u16) && !is_charge_record(i, sde))
            .map(|i| i.item_id)
            .collect();

        let shadow_active: Vec<i64> = self
            .module_states
            .active_modules
            .iter()
            .copied()
            .filter(|id| ship_module_ids.contains(id) || self.inventory.items.contains_key(id))
            .collect();
        let mut world_active: Vec<i64> = snap
            .module_active_states
            .iter()
            .copied()
            .filter(|id| ship_module_ids.contains(id))
            .collect();
        world_active.sort_unstable();

        if shadow_active != world_active {
            out.push(ShadowDivergence::ActiveModuleSetMismatch { shadow_active, world_active });
        }
        for (&mod_id, &shadow_online) in &self.module_states.online_states {
            if ship_module_ids.contains(&mod_id) {
                let world_online = snap.module_online_states.get(&mod_id).copied().unwrap_or(true);
                if shadow_online != world_online {
                    out.push(ShadowDivergence::ModuleOnlineStateMismatch {
                        module_id: mod_id,
                        shadow_online,
                        world_online,
                    });
                }
            }
        }
        out
    }

    pub fn compare_session_against_world(&self, db: &WorldRepository) -> Vec<ShadowDivergence> {
        db.with_snapshot(|snap| self.compare_session_against_snapshot(snap))
    }

    pub fn compare_session_against_snapshot(&self, snap: &WorldStateSnapshot) -> Vec<ShadowDivergence> {
        let mut out = Vec::new();
        if !self.session.synced {
            return out;
        }
        let Some(ch) = snap.characters.get(&self.char_id) else { return out };
        if let Some(sh_sys) = self.session.solar_system_id {
            if sh_sys != ch.solar_system_id {
                out.push(ShadowDivergence::SessionFieldMismatch {
                    field: "solarsystemid2",
                    shadow_value: sh_sys.to_string(),
                    world_value: ch.solar_system_id.to_string(),
                });
            }
        }
        if self.session.station_id != ch.station_id {
            out.push(ShadowDivergence::SessionFieldMismatch {
                field: "stationid",
                shadow_value: format!("{:?}", self.session.station_id),
                world_value: format!("{:?}", ch.station_id),
            });
        }
        if self.session.structure_id != ch.structure_id {
            out.push(ShadowDivergence::SessionFieldMismatch {
                field: "structureid",
                shadow_value: format!("{:?}", self.session.structure_id),
                world_value: format!("{:?}", ch.structure_id),
            });
        }
        let world_ship_id = (ch.active_ship_id > 0).then_some(ch.active_ship_id);
        if self.session.ship_id.is_some() && self.session.ship_id != world_ship_id {
            out.push(ShadowDivergence::SessionFieldMismatch {
                field: "shipid",
                shadow_value: format!("{:?}", self.session.ship_id),
                world_value: format!("{:?}", world_ship_id),
            });
        }
        if let Some(sh_corp) = self.session.corp_id {
            if sh_corp != ch.corporation_id {
                out.push(ShadowDivergence::SessionFieldMismatch {
                    field: "corpid",
                    shadow_value: sh_corp.to_string(),
                    world_value: ch.corporation_id.to_string(),
                });
            }
        }
        out
    }

    pub fn compare_ballpark_against_world_balls(
        &self,
        expected_balls: &[(i64, ShadowBallMode, Option<i64>, f64, bool)],
    ) -> Vec<ShadowDivergence> {
        let mut out = Vec::new();
        let expected_ids: BTreeSet<i64> = expected_balls.iter().map(|b| b.0).collect();
        for &(ball_id, exp_mode, exp_target, exp_speed_frac, exp_cloaked) in expected_balls {
            match self.ballpark.balls.get(&ball_id) {
                None => out.push(ShadowDivergence::BallparkMissingBall { ball_id }),
                Some(sh) => {
                    if sh.mode != exp_mode {
                        out.push(ShadowDivergence::BallparkFieldMismatch {
                            ball_id,
                            field: "mode",
                            shadow_value: format!("{:?}", sh.mode),
                            world_value: format!("{:?}", exp_mode),
                        });
                    }
                    if sh.target_id != exp_target {
                        out.push(ShadowDivergence::BallparkFieldMismatch {
                            ball_id,
                            field: "target_id",
                            shadow_value: format!("{:?}", sh.target_id),
                            world_value: format!("{:?}", exp_target),
                        });
                    }
                    if (sh.speed_fraction - exp_speed_frac).abs() > 1e-3 {
                        out.push(ShadowDivergence::BallparkFieldMismatch {
                            ball_id,
                            field: "speed_fraction",
                            shadow_value: format!("{:.4}", sh.speed_fraction),
                            world_value: format!("{:.4}", exp_speed_frac),
                        });
                    }
                    if sh.is_cloaked != exp_cloaked {
                        out.push(ShadowDivergence::BallparkFieldMismatch {
                            ball_id,
                            field: "is_cloaked",
                            shadow_value: sh.is_cloaked.to_string(),
                            world_value: exp_cloaked.to_string(),
                        });
                    }
                }
            }
        }
        for &ball_id in self.ballpark.balls.keys() {
            if !expected_ids.contains(&ball_id) {
                out.push(ShadowDivergence::BallparkStaleBall { ball_id });
            }
        }
        out
    }

    pub fn compare_all_against_world(
        &self,
        db: &WorldRepository,
        sde: Option<&SdeRegistry>,
    ) -> Vec<ShadowDivergence> {
        db.with_snapshot(|snap| {
            let mut out = self.compare_inventory_against_snapshot(snap, sde);
            out.extend(self.compare_wallet_against_snapshot(snap));
            out.extend(self.compare_active_modules_against_snapshot(snap, sde));
            out.extend(self.compare_session_against_snapshot(snap));
            out
        })
    }

    pub fn assert_converged_with_world(&self, db: &WorldRepository, sde: Option<&SdeRegistry>) {
        let divergences = self.compare_all_against_world(db, sde);
        assert!(
            divergences.is_empty(),
            "ShadowClient (char_id={}) diverged from authoritative world state ({} divergence(s)):\n  - {}",
            self.char_id,
            divergences.len(),
            divergences.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n  - ")
        );
    }
}

fn is_tombstone_location_or_flag(location_id: i64, flag_id: u16, quantity: i32) -> bool {
    location_id == 6 || location_id == 10 || flag_id == 147 || quantity <= 0
}

fn extract_change_dict_i64(change_dict: Option<&PyRep>, col_ix: i64) -> Option<i64> {
    let PyRep::Dict(entries) = change_dict? else { return None };
    entries.iter().find(|(k, _)| k.as_i64() == Some(col_ix)).and_then(|(_, v)| v.as_i64())
}

fn extract_attr_f64(attrs_rep: Option<&PyRep>, attr_id: u16) -> Option<f64> {
    let PyRep::Dict(entries) = attrs_rep? else { return None };
    entries.iter().find(|(k, _)| k.as_i64() == Some(i64::from(attr_id))).and_then(|(_, v)| v.as_f64())
}

fn pyrep_as_truthy(rep: &PyRep) -> bool {
    match rep {
        PyRep::Bool(b) => *b,
        PyRep::Integer(i) => *i != 0,
        PyRep::Real(r) => *r != 0.0,
        _ => false,
    }
}

fn normalize_corp_division(div: u16) -> u16 {
    if (1..=7).contains(&div) {
        999 + div
    } else if (1000..=1006).contains(&div) {
        div
    } else {
        1000
    }
}

fn parse_corp_account_key(key: &str) -> Option<u16> {
    match key {
        "cash" | "1000" => Some(1000),
        "cash2" | "1001" => Some(1001),
        "cash3" | "1002" => Some(1002),
        "cash4" | "1003" => Some(1003),
        "cash5" | "1004" => Some(1004),
        "cash6" | "1005" => Some(1005),
        "cash7" | "1006" => Some(1006),
        _ => None,
    }
}

fn compare_shadow_item_fields(sh: &ShadowItem, exp: &ShadowItem, out: &mut Vec<ShadowDivergence>) {
    for (mismatch, field, sv, wv) in [
        (sh.type_id != exp.type_id, "typeID", sh.type_id.to_string(), exp.type_id.to_string()),
        (sh.owner_id != exp.owner_id, "ownerID", sh.owner_id.to_string(), exp.owner_id.to_string()),
        (sh.location_id != exp.location_id, "locationID", sh.location_id.to_string(), exp.location_id.to_string()),
        (sh.flag_id != exp.flag_id, "flagID", sh.flag_id.to_string(), exp.flag_id.to_string()),
        (sh.quantity != exp.quantity, "quantity", sh.quantity.to_string(), exp.quantity.to_string()),
        (sh.singleton != exp.singleton, "singleton", sh.singleton.to_string(), exp.singleton.to_string()),
    ] {
        if mismatch {
            out.push(ShadowDivergence::ItemFieldMismatch {
                item_id: sh.item_id,
                field,
                shadow_value: sv,
                world_value: wv,
            });
        }
    }
}

fn parse_damage_state_tuple(rep: &PyRep) -> Option<ShadowDamageState> {
    let (PyRep::Tuple(parts) | PyRep::List(parts)) = rep else { return None };
    if parts.len() < 3 {
        return None;
    }
    let shield = match &parts[0] {
        PyRep::Tuple(inner) | PyRep::List(inner) => inner.first().and_then(PyRep::as_f64)?,
        other => other.as_f64()?,
    };
    Some(ShadowDamageState { shield, armor: parts[1].as_f64()?, hull: parts[2].as_f64()? })
}

/// Decodes a binary `AddBalls` / `SetState` buffer (`packet_type: u8`, `stamp: u32`, ball sectors).
pub fn decode_destiny_ball_buffer(bytes: &[u8]) -> Result<(u8, u32, Vec<ShadowBall>), String> {
    if bytes.len() < 5 {
        return Err(format!("Destiny buffer too short: {} bytes", bytes.len()));
    }
    let packet_type = bytes[0];
    let stamp = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    let mut off = 5usize;
    let mut balls = Vec::new();

    let ru8 = |b: &[u8], o: &mut usize| -> Result<u8, String> {
        let v = *b.get(*o).ok_or_else(|| format!("EOF u8 at {}", *o))?;
        *o += 1;
        Ok(v)
    };
    let ri32 = |b: &[u8], o: &mut usize| -> Result<i32, String> {
        let s = b.get(*o..*o + 4).ok_or_else(|| format!("EOF i32 at {}", *o))?;
        *o += 4;
        Ok(i32::from_le_bytes(s.try_into().unwrap()))
    };
    let ri64 = |b: &[u8], o: &mut usize| -> Result<i64, String> {
        let s = b.get(*o..*o + 8).ok_or_else(|| format!("EOF i64 at {}", *o))?;
        *o += 8;
        Ok(i64::from_le_bytes(s.try_into().unwrap()))
    };
    let rf32 = |b: &[u8], o: &mut usize| -> Result<f32, String> {
        let s = b.get(*o..*o + 4).ok_or_else(|| format!("EOF f32 at {}", *o))?;
        *o += 4;
        Ok(f32::from_le_bytes(s.try_into().unwrap()))
    };
    let rf64 = |b: &[u8], o: &mut usize| -> Result<f64, String> {
        let s = b.get(*o..*o + 8).ok_or_else(|| format!("EOF f64 at {}", *o))?;
        *o += 8;
        Ok(f64::from_le_bytes(s.try_into().unwrap()))
    };

    while off < bytes.len() {
        let ball_id = ri64(bytes, &mut off)?;
        let mode_byte = ru8(bytes, &mut off)?;
        let radius = f64::from(rf32(bytes, &mut off)?);
        let (px, py, pz) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
        let flags = ru8(bytes, &mut off)?;
        let is_rigid = mode_byte == DST_BALL_RIGID;
        let is_cloaked = if !is_rigid {
            let _mass = rf64(bytes, &mut off)?;
            let cloak = ru8(bytes, &mut off)?;
            let _alliance_id = ri64(bytes, &mut off)?;
            let _corp_id = ri32(bytes, &mut off)?;
            let _harmonic = ri32(bytes, &mut off)?;
            cloak != 0
        } else {
            false
        };
        let is_free = !is_rigid && (flags & BALL_FLAG_IS_FREE) != 0;
        let (max_velocity, velocity, speed_fraction) = if is_free {
            let max_v = f64::from(rf32(bytes, &mut off)?);
            let (vx, vy, vz) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
            let _inertia = rf32(bytes, &mut off)?;
            let sf = f64::from(rf32(bytes, &mut off)?);
            (Some(max_v), [vx, vy, vz], sf)
        } else {
            (None, [0.0, 0.0, 0.0], 0.0)
        };
        let _formation_id = ru8(bytes, &mut off)?;
        let mut target_id = None;
        let mut follow_range = None;
        let mut warp_destination = None;
        let mut warp_speed_milli_au_s = None;

        match mode_byte {
            DST_BALL_GOTO => {
                let (_, _, _) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
            }
            DST_BALL_FOLLOW | DST_BALL_ORBIT => {
                target_id = Some(ri64(bytes, &mut off)?);
                follow_range = Some(f64::from(rf32(bytes, &mut off)?));
            }
            DST_BALL_WARP => {
                let (wx, wy, wz) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
                let (_, _, _) = (ri32(bytes, &mut off)?, rf64(bytes, &mut off)?, ri64(bytes, &mut off)?);
                warp_destination = Some([wx, wy, wz]);
                warp_speed_milli_au_s = Some(ri64(bytes, &mut off)? as u32);
            }
            DST_BALL_MISSILE => {
                target_id = Some(ri64(bytes, &mut off)?);
                follow_range = Some(f64::from(rf32(bytes, &mut off)?));
                let (_, _) = (ri64(bytes, &mut off)?, ri32(bytes, &mut off)?);
                let (_, _, _) = (rf64(bytes, &mut off)?, rf64(bytes, &mut off)?, rf64(bytes, &mut off)?);
            }
            DST_BALL_MUSHROOM => {
                follow_range = Some(f64::from(rf32(bytes, &mut off)?));
                let (_, _, _) = (rf64(bytes, &mut off)?, ri32(bytes, &mut off)?, ri64(bytes, &mut off)?);
            }
            DST_BALL_TROLL => {
                let _ = ri32(bytes, &mut off)?;
            }
            DST_BALL_FORMATION => {
                target_id = Some(ri64(bytes, &mut off)?);
                follow_range = Some(f64::from(rf32(bytes, &mut off)?));
                let _ = ri32(bytes, &mut off)?;
            }
            DST_BALL_STOP | DST_BALL_BOID | DST_BALL_MINIBALL | DST_BALL_FIELD | DST_BALL_RIGID => {}
            other => return Err(format!("Unknown Destiny ball mode byte {other}")),
        }

        balls.push(ShadowBall {
            ball_id,
            mode: ShadowBallMode::from_mode_byte(mode_byte),
            position: [px, py, pz],
            velocity,
            radius,
            max_velocity,
            speed_fraction,
            is_free,
            is_cloaked,
            target_id,
            follow_range,
            warp_destination,
            warp_speed_milli_au_s,
        });
    }
    Ok((packet_type, stamp, balls))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_db::{InventoryFlag, IskCents};
    use eve_destiny::wire::encode_add_balls;
    use eve_destiny::{Ball, MovementMode};
    use eve_proto::{DBRowDescriptor, DbColumnType};
    use glam::DVec3;

    const PILOT_ID: i64 = 90_000_001;
    const CORP_ID: i64 = 98_000_001;
    const STATION_ID: i64 = 60_003_760;
    const SHIP_ID: i64 = 100_000_001;

    fn build_inv_row(
        item_id: i64,
        type_id: u32,
        owner_id: i64,
        location_id: i64,
        flag_id: u16,
        packed_qty: i32,
        group_id: u32,
        category_id: u32,
    ) -> PyRep {
        DBRowDescriptor::inv_item_header().build_dbrow(vec![
            PyRep::Integer(item_id),
            PyRep::Integer(i64::from(type_id)),
            PyRep::Integer(owner_id),
            PyRep::Integer(location_id),
            PyRep::Integer(i64::from(flag_id)),
            PyRep::Integer(i64::from(packed_qty)),
            PyRep::Integer(i64::from(group_id)),
            PyRep::Integer(i64::from(category_id)),
            PyRep::String(String::new()),
        ])
    }

    #[test]
    fn missing_on_item_change_and_on_account_change_deltas_are_caught() {
        let db = WorldRepository::with_demo_seed();
        db.with_mut_snapshot(|snap| {
            snap.corporations.entry(CORP_ID).or_insert_with(|| {
                eve_db::CorporationRecord::new(CORP_ID, "Shadow Corp", "SHDW", PILOT_ID)
            });
            snap.characters.get_mut(&PILOT_ID).unwrap().corporation_id = CORP_ID;
        });
        let mut shadow = ShadowClient::from_world(PILOT_ID, &db, None);
        shadow.assert_converged_with_world(&db, None);

        let new_item_id = 999_000_111;
        db.with_mut_snapshot(|snap| {
            snap.items.insert(
                new_item_id,
                InventoryItemRecord {
                    item_id: new_item_id,
                    type_id: 34,
                    owner_id: PILOT_ID,
                    location_id: STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 500,
                    singleton: false,
                    ..Default::default()
                },
            );
        });
        assert!(shadow.compare_inventory_against_world(&db, None).iter().any(|d| matches!(
            d,
            ShadowDivergence::MissingShadowItem { item_id, type_id: 34, quantity: 500, .. } if *item_id == new_item_id
        )));

        let row = build_inv_row(new_item_id, 34, PILOT_ID, STATION_ID, 4, 500, 18, 4);
        shadow.apply_notification("OnItemChange", &[row, PyRep::Dict(Vec::new())]);
        shadow.assert_converged_with_world(&db, None);

        db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&new_item_id).unwrap().quantity = 350;
        });
        assert!(shadow.compare_inventory_against_world(&db, None).iter().any(|d| matches!(
            d,
            ShadowDivergence::ItemFieldMismatch { item_id, field: "quantity", .. } if *item_id == new_item_id
        )));

        let row_350 = build_inv_row(new_item_id, 34, PILOT_ID, STATION_ID, 4, 350, 18, 4);
        shadow.apply_notification("OnItemChange", &[row_350, PyRep::Dict(vec![(PyRep::Integer(9), PyRep::Integer(500))])]);
        shadow.assert_converged_with_world(&db, None);

        db.with_mut_snapshot(|snap| {
            snap.items.remove(&new_item_id);
        });
        assert!(shadow.compare_inventory_against_world(&db, None).iter().any(|d| matches!(
            d,
            ShadowDivergence::StaleShadowItem { item_id, .. } if *item_id == new_item_id
        )));

        let tomb_row = build_inv_row(new_item_id, 34, PILOT_ID, 6, 0, 0, 18, 4);
        let tomb_changes = PyRep::Dict(vec![
            (PyRep::Integer(3), PyRep::Integer(STATION_ID)),
            (PyRep::Integer(4), PyRep::Integer(4)),
            (PyRep::Integer(9), PyRep::Integer(350)),
        ]);
        shadow.apply_notification("OnItemChange", &[tomb_row, tomb_changes]);
        shadow.assert_converged_with_world(&db, None);

        db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&PILOT_ID).unwrap().balance = IskCents::from_isk(2_500_000);
            if let Some(corp) = snap.corporations.get_mut(&CORP_ID) {
                corp.set_division_balance(1, IskCents::from_isk(9_000_000));
            }
        });
        let wallet_divs = shadow.compare_wallet_against_world(&db);
        assert!(wallet_divs.iter().any(|d| matches!(d, ShadowDivergence::PersonalWalletMismatch { char_id: PILOT_ID, .. })));
        assert!(wallet_divs.iter().any(|d| matches!(d, ShadowDivergence::CorpWalletMismatch { corp_id: CORP_ID, division: 1000, .. })));

        shadow.apply_notification("OnAccountChange", &[PyRep::String("cash".into()), PyRep::Integer(PILOT_ID), PyRep::Real(2_500_000.0)]);
        shadow.apply_notification("OnAccountChange", &[PyRep::String("cash".into()), PyRep::Integer(CORP_ID), PyRep::Real(9_000_000.0)]);
        shadow.assert_converged_with_world(&db, None);
    }

    #[test]
    fn scenario_941_948_get_all_info_loaded_charges_across_login_undock_and_jump() {
        let db = WorldRepository::with_demo_seed();
        let (launcher_id, charge_id, flag_hi0) = (999_000_201, 999_000_202, InventoryFlag::HiSlot0 as u16);

        db.with_mut_snapshot(|snap| {
            snap.items
                .retain(|&id, it| id == SHIP_ID || it.location_id != SHIP_ID);
            snap.items.insert(
                launcher_id,
                InventoryItemRecord {
                    item_id: launcher_id,
                    type_id: 484,
                    owner_id: PILOT_ID,
                    location_id: SHIP_ID,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                charge_id,
                InventoryItemRecord {
                    item_id: charge_id,
                    type_id: 179,
                    owner_id: PILOT_ID,
                    location_id: SHIP_ID,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 40,
                    singleton: false,
                    ..Default::default()
                },
            );
        });

        let ship_row = build_inv_row(SHIP_ID, 606, PILOT_ID, STATION_ID, 4, -1, 25, 6);
        let launcher_row = build_inv_row(launcher_id, 484, PILOT_ID, SHIP_ID, flag_hi0, -1, 55, 7);
        let subloc_key = PyRep::Tuple(vec![PyRep::Integer(SHIP_ID), PyRep::Integer(i64::from(flag_hi0)), PyRep::Integer(179)]);

        let build_get_all_info_payload = |include_loaded_charge: bool| -> PyRep {
            let mut ship_info_entries = vec![
                (
                    PyRep::Integer(SHIP_ID),
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(SHIP_ID)),
                        ("invItem", ship_row.clone()),
                        ("attributes", PyRep::Dict(Vec::new())),
                        ("activeEffects", PyRep::Dict(Vec::new())),
                    ]),
                ),
                (
                    PyRep::Integer(launcher_id),
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(launcher_id)),
                        ("invItem", launcher_row.clone()),
                        ("attributes", PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Real(1.0))])),
                        ("activeEffects", PyRep::Dict(Vec::new())),
                    ]),
                ),
            ];
            let mut subloc_cache_entries = Vec::new();
            if include_loaded_charge {
                ship_info_entries.push((
                    subloc_key.clone(),
                    PyRep::keyval([
                        ("itemID", subloc_key.clone()),
                        ("invItem", PyRep::None),
                        ("attributes", PyRep::Dict(vec![(PyRep::Integer(i64::from(attr_ids::QUANTITY)), PyRep::Real(40.0))])),
                        ("activeEffects", PyRep::Dict(Vec::new())),
                    ]),
                ));
                let subloc_row = DBRowDescriptor::new([
                    ("locationID", DbColumnType::I8),
                    ("flagID", DbColumnType::I4),
                    ("typeID", DbColumnType::I4),
                    ("quantity", DbColumnType::I4),
                ])
                .build_dbrow(vec![
                    PyRep::Integer(SHIP_ID),
                    PyRep::Integer(i64::from(flag_hi0)),
                    PyRep::Integer(179),
                    PyRep::Integer(40),
                ]);
                subloc_cache_entries.push((
                    PyRep::Integer(SHIP_ID),
                    PyRep::Dict(vec![(PyRep::Integer(i64::from(flag_hi0)), subloc_row)]),
                ));
            }
            PyRep::keyval([
                ("shipID", PyRep::Integer(SHIP_ID)),
                ("charID", PyRep::Integer(PILOT_ID)),
                ("shipInfo", PyRep::Dict(ship_info_entries)),
                (
                    "shipState",
                    PyRep::Tuple(vec![
                        PyRep::Dict(Vec::new()),
                        PyRep::Dict(subloc_cache_entries),
                        PyRep::Dict(Vec::new()),
                        PyRep::Dict(Vec::new()),
                    ]),
                ),
            ])
        };

        let mut buggy_shadow = ShadowClient::new(PILOT_ID);
        buggy_shadow.sync_from_get_all_info(&build_get_all_info_payload(false));
        assert!(buggy_shadow.compare_inventory_against_world(&db, None).iter().any(|d| matches!(
            d,
            ShadowDivergence::MissingLoadedChargeSublocation {
                ship_id: SHIP_ID,
                flag_id: 27,
                expected_type_id: 179,
                expected_quantity: 40,
            }
        )));
        let unloaded_row = build_inv_row(charge_id, 179, PILOT_ID, SHIP_ID, 5, 40, 83, 8);
        buggy_shadow.apply_notification("OnItemChange", &[unloaded_row, PyRep::Dict(vec![(PyRep::Integer(4), PyRep::Integer(i64::from(flag_hi0)))])]);
        assert!(buggy_shadow.recorded_divergences.iter().any(|d| matches!(
            d,
            ShadowDivergence::UnprimedFittedItemChange { ship_id: SHIP_ID, item_id, old_flag_id: 27, .. } if *item_id == charge_id
        )));

        let mut shadow = ShadowClient::new(PILOT_ID);
        for _step in ["login", "undock", "stargate_jump"] {
            shadow.sync_from_get_all_info(&build_get_all_info_payload(true));
            assert!(shadow.compare_inventory_against_world(&db, None).is_empty());
        }
    }

    #[test]
    fn scenario_937_one_shot_module_activation_start_and_stop_convergence() {
        let db = WorldRepository::with_demo_seed();
        let launcher_id = 999_000_301;
        db.with_mut_snapshot(|snap| {
            snap.items
                .retain(|&id, it| id == SHIP_ID || it.location_id != SHIP_ID);
            snap.items.insert(
                launcher_id,
                InventoryItemRecord {
                    item_id: launcher_id,
                    type_id: 17938,
                    owner_id: PILOT_ID,
                    location_id: SHIP_ID,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });

        let mut shadow = ShadowClient::from_world(PILOT_ID, &db, None);
        shadow.assert_converged_with_world(&db, None);

        let effect_args = |flag: i64| vec![
            PyRep::Integer(launcher_id),
            PyRep::Integer(101),
            PyRep::Integer(133_000_000_000_000_000),
            PyRep::Integer(flag),
            PyRep::Integer(flag),
            PyRep::Tuple(Vec::new()),
            PyRep::Integer(133_000_000_000_000_000),
            PyRep::Integer(1000),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::None,
        ];
        shadow.apply_notification("OnGodmaShipEffect", &effect_args(1));
        assert_eq!(
            shadow.compare_active_modules_against_world(&db, None),
            vec![ShadowDivergence::ActiveModuleSetMismatch {
                shadow_active: vec![launcher_id],
                world_active: vec![],
            }]
        );

        shadow.apply_notification("OnGodmaShipEffect", &effect_args(0));
        shadow.assert_converged_with_world(&db, None);
    }

    #[test]
    fn destiny_ballpark_and_session_change_convergence() {
        let db = WorldRepository::with_demo_seed();
        let mut shadow = ShadowClient::from_world(PILOT_ID, &db, None);

        let ship_ball = Ball {
            id: SHIP_ID,
            type_id: 606,
            position: DVec3::new(1_000.0, 2_000.0, 3_000.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let target_ball_id = 999_000_401;
        let target_ball = Ball {
            id: target_ball_id,
            type_id: 587,
            position: DVec3::new(5_000.0, 2_000.0, 3_000.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 3.0,
            mass: 1_100_000.0,
            radius: 38.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let set_state_kv = PyRep::keyval([
            ("stamp", PyRep::Integer(42)),
            ("ego", PyRep::Integer(SHIP_ID)),
            ("state", PyRep::Buffer(encode_add_balls(42, &[&ship_ball]))),
            ("slims", PyRep::List(vec![PyRep::keyval([("itemID", PyRep::Integer(SHIP_ID)), ("typeID", PyRep::Integer(606))])])),
            ("damageState", PyRep::Dict(Vec::new())),
        ]);
        let updates = PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::Integer(42),
                PyRep::Tuple(vec![PyRep::String("SetState".into()), PyRep::Tuple(vec![set_state_kv])]),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(43),
                PyRep::Tuple(vec![
                    PyRep::String("AddBalls2".into()),
                    PyRep::Tuple(vec![PyRep::Tuple(vec![
                        PyRep::Buffer(encode_add_balls(43, &[&target_ball])),
                        PyRep::List(vec![PyRep::keyval([("itemID", PyRep::Integer(target_ball_id)), ("typeID", PyRep::Integer(587))])]),
                        PyRep::Dict(Vec::new()),
                    ])]),
                ]),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(44),
                PyRep::Tuple(vec![
                    PyRep::String("Orbit".into()),
                    PyRep::Tuple(vec![PyRep::Integer(SHIP_ID), PyRep::Integer(target_ball_id), PyRep::Real(2_500.0)]),
                ]),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(44),
                PyRep::Tuple(vec![
                    PyRep::String("SetSpeedFraction".into()),
                    PyRep::Tuple(vec![PyRep::Integer(SHIP_ID), PyRep::Real(1.0)]),
                ]),
            ]),
        ]);
        shadow.apply_notification("DoDestinyUpdate", &[updates]);
        assert!(shadow
            .compare_ballpark_against_world_balls(&[
                (SHIP_ID, ShadowBallMode::Orbit, Some(target_ball_id), 1.0, false),
                (target_ball_id, ShadowBallMode::Stop, None, 0.0, false),
            ])
            .is_empty());

        let remove_and_stop = PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::Integer(45),
                PyRep::Tuple(vec![
                    PyRep::String("RemoveBalls".into()),
                    PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(target_ball_id)])]),
                ]),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(45),
                PyRep::Tuple(vec![PyRep::String("Stop".into()), PyRep::Tuple(vec![PyRep::Integer(SHIP_ID)])]),
            ]),
        ]);
        shadow.apply_notification("DoDestinyUpdate", &[remove_and_stop]);
        assert!(shadow.compare_ballpark_against_world_balls(&[(SHIP_ID, ShadowBallMode::Stop, None, 0.0, false)]).is_empty());

        db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&PILOT_ID).unwrap().station_id = None;
        });
        assert!(shadow.compare_session_against_world(&db).iter().any(|d| matches!(d, ShadowDivergence::SessionFieldMismatch { field: "stationid", .. })));

        shadow.apply_notification(
            "OnSessionChanged",
            &[
                PyRep::Bool(false),
                PyRep::Dict(vec![(
                    PyRep::String("stationid".into()),
                    PyRep::Tuple(vec![PyRep::Integer(STATION_ID), PyRep::None]),
                )]),
            ],
        );
        shadow.assert_converged_with_world(&db, None);
    }
}
