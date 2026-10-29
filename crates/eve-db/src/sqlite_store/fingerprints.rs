//! Table change tracking and FNV-1a row fingerprinting.

use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// Fast zero-allocation FNV-1a 64-bit hasher for row-level dirty detection.
pub(crate) struct FnvHasher(pub(crate) u64);

impl FnvHasher {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;

    #[inline]
    pub(crate) fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    #[inline]
    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    #[inline]
    pub(crate) fn write_u64(&mut self, v: u64) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_i64(&mut self, v: i64) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_u32(&mut self, v: u32) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_i32(&mut self, v: i32) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_u8(&mut self, v: u8) {
        self.write_bytes(&[v]);
    }

    #[inline]
    pub(crate) fn write_bool(&mut self, v: bool) {
        self.write_u8(u8::from(v));
    }

    #[inline]
    pub(crate) fn write_f64(&mut self, v: f64) {
        self.write_u64(v.to_bits());
    }

    #[inline]
    pub(crate) fn write_str(&mut self, s: &str) {
        self.write_u64(s.len() as u64);
        self.write_bytes(s.as_bytes());
    }

    #[inline]
    pub(crate) fn finish(self) -> u64 {
        self.0
    }
}

impl std::hash::Hasher for FnvHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.write_bytes(bytes);
    }
}

#[inline]
pub(crate) fn fnv1a_hasher() -> FnvHasher {
    FnvHasher::new()
}

pub(crate) fn fp_account(a: &AccountRecord) -> u64 {
    let mut h = FnvHasher::new();
    h.write_u64(a.user_id);
    h.write_str(&a.username);
    h.write_str(&a.password_token);
    h.write_u64(a.role);
    h.write_u64(a.banned_until_unix.unwrap_or(0));
    if let Some(ref r) = a.ban_reason {
        h.write_str(r);
    }
    h.finish()
}

pub(crate) fn fp_character(c: &CharacterRecord) -> u64 {
    let mut h = FnvHasher::new();
    h.write_i64(c.character_id);
    h.write_u64(c.user_id);
    h.write_str(&c.name);
    h.write_u32(c.race_id);
    h.write_u32(c.bloodline_id);
    h.write_u32(c.ancestry_id);
    h.write_u32(c.school_id);
    h.write_i64(c.corporation_id);
    h.write_i64(c.alliance_id.unwrap_or(-1));
    h.write_i64(c.war_faction_id.unwrap_or(-1));
    h.write_i64(c.station_id.unwrap_or(-1));
    h.write_i64(c.structure_id.unwrap_or(-1));
    h.write_u32(c.solar_system_id);
    h.write_u32(c.constellation_id);
    h.write_u32(c.region_id);
    h.write_i64(c.active_ship_id);
    h.write_i64(c.balance.0);
    h.write_i64(c.plex_balance);
    h.write_f64(c.security_status);
    h.write_u32(c.clone_grade);
    h.write_i64(c.home_station_id);
    h.write_u8(c.neural.charisma);
    h.write_u8(c.neural.intelligence);
    h.write_u8(c.neural.memory);
    h.write_u8(c.neural.perception);
    h.write_u8(c.neural.willpower);
    h.write_u8(c.neural.bonus_remaps);
    h.write_u64(c.neural.last_remap_unix.unwrap_or(0));
    h.write_u64(c.neural.remap_cooldown_until_unix.unwrap_or(0));
    h.write_f64(c.skill_points);
    h.write_f64(c.free_skill_points);
    h.write_u64(c.last_clone_jump_unix);
    h.write_f64(c.jump_fatigue_minutes);
    h.write_u64(c.jump_cooldown_until_unix);
    h.write_u64(c.jump_fatigue_until_unix);
    h.write_i64(c.fleet_id.unwrap_or(-1));
    h.write_u64(c.version);
    h.write_u64(c.system_epoch);
    c.activity_status.hash(&mut h);
    let mut lp_xor = 0u64;
    for (&k, &v) in &c.loyalty_points {
        let mut eh = FnvHasher::new();
        eh.write_i64(k);
        eh.write_i64(v);
        lp_xor ^= eh.finish();
    }
    h.write_u64(lp_xor);
    let mut st_xor = 0u64;
    for (&k, &v) in &c.standings {
        let mut eh = FnvHasher::new();
        eh.write_i64(k);
        eh.write_f64(v);
        st_xor ^= eh.finish();
    }
    h.write_u64(st_xor);
    h.finish()
}

pub(crate) fn fp_item(i: &InventoryItemRecord) -> u64 {
    let mut h = FnvHasher::new();
    h.write_i64(i.item_id);
    h.write_u32(i.type_id);
    h.write_i64(i.owner_id);
    h.write_i64(i.location_id);
    h.write_u32(i.flag as u32);
    h.write_i32(i.quantity);
    h.write_bool(i.singleton);
    h.write_u64(i.version);
    h.write_u32(i.source_mutaplasmid_id.unwrap_or(0));
    h.write_f64(i.insured_payout_isk);
    h.write_bool(i.is_blueprint_copy);
    h.write_i32(i.blueprint_runs_remaining.unwrap_or(-1));
    let mut dyn_xor = 0u64;
    for (&k, &v) in &i.dynamic_attributes {
        let mut eh = FnvHasher::new();
        eh.write_u32(k as u32);
        eh.write_f64(v);
        dyn_xor ^= eh.finish();
    }
    h.write_u64(dyn_xor);
    h.finish()
}

#[inline]
pub(crate) fn fp_market_order(o: &MarketOrderRecord) -> u64 {
    const P: u64 = 0x517c_c1b7_2722_0a95;
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    h = (h ^ (o.order_id as u64)).wrapping_mul(P);
    h = (h ^ (o.owner_id as u64)).wrapping_mul(P);
    h = (h ^ (o.station_id as u64)).wrapping_mul(P);
    h = (h ^ ((o.region_id as u64) | ((o.type_id as u64) << 32))).wrapping_mul(P);
    h = (h ^ (o.price.0 as u64)).wrapping_mul(P);
    h = (h ^ ((o.vol_entered as u32 as u64) | ((o.vol_remaining as u32 as u64) << 32)))
        .wrapping_mul(P);
    h = (h
        ^ ((o.min_volume as u32 as u64)
            | (u64::from(o.is_bid) << 32)
            | (u64::from(o.is_npc) << 33)))
        .wrapping_mul(P);
    (h ^ (o.escrow.0 as u64)).wrapping_mul(P)
}

#[inline]
pub(crate) fn fp_market_orders_digest(orders: &HashMap<i64, MarketOrderRecord>) -> u64 {
    let mut xor = (orders.len() as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    for ord in orders.values() {
        xor ^= fp_market_order(ord);
    }
    xor
}

pub(crate) fn fp_contract(c: &ContractRecord) -> u64 {
    let mut h = FnvHasher::new();
    h.write_i64(c.contract_id);
    h.write_i64(c.issuer_id);
    h.write_i64(c.issuer_corp_id);
    h.write_i64(c.assignee_id);
    h.write_i64(c.acceptor_id.unwrap_or(-1));
    h.write_bool(c.for_corp);
    h.write_str(&c.availability);
    h.write_str(&c.contract_type);
    h.write_str(&c.status);
    h.write_str(&c.title);
    h.write_i64(c.start_station_id);
    h.write_i64(c.end_station_id);
    h.write_i64(c.price.0);
    h.write_i64(c.reward.0);
    h.write_i64(c.collateral.0);
    h.write_i64(c.buyout.0);
    h.write_i64(c.current_bid.0);
    h.write_i64(c.current_bidder_id.unwrap_or(-1));
    h.write_u64(c.date_issued_unix);
    h.write_u64(c.expiration_unix);
    h.write_u32(c.num_days);
    h.write_u64(c.item_ids.len() as u64);
    for &id in &c.item_ids {
        h.write_i64(id);
    }
    h.write_u64(c.requested_items.len() as u64);
    for &(tid, qty) in &c.requested_items {
        h.write_u32(tid);
        h.write_i32(qty);
    }
    h.write_u64(c.bid_history.len() as u64);
    for &(bidder_id, amount, ts) in &c.bid_history {
        h.write_i64(bidder_id);
        h.write_i64(amount.0);
        h.write_u64(ts);
    }
    h.finish()
}

pub(crate) fn fp_system_state(s: &SystemDynamicState) -> u64 {
    let mut h = FnvHasher::new();
    h.write_u32(s.solar_system_id);
    h.write_f64(s.dbs_multiplier);
    h.write_i64(s.ess_main_bank.0);
    h.write_i64(s.ess_reserve_bank.0);
    h.write_i64(s.ess_linked_character_id.unwrap_or(-1));
    h.write_u64(s.ess_link_started_tick.unwrap_or(0));
    h.write_i64(s.sov_alliance_id.unwrap_or(-1));
    h.write_u8(s.military_index);
    h.write_u8(s.industrial_index);
    h.write_u8(s.strategic_index);
    h.write_f64(s.fw_contested_pct);
    h.write_u8(s.insurgency_corruption_stage);
    h.write_u8(s.insurgency_suppression_stage);
    h.write_f64(s.wormhole_remaining_mass_kg);
    h.write_u32(s.wormhole_destination_system_id.unwrap_or(0));
    if let Some(p) = s.signature_position {
        h.write_f64(p[0]);
        h.write_f64(p[1]);
        h.write_f64(p[2]);
    }
    if let Some(p) = s.fw_complex_position {
        h.write_f64(p[0]);
        h.write_f64(p[1]);
        h.write_f64(p[2]);
    }
    h.write_u64(s.fw_capture_seconds_elapsed);
    h.write_u64(s.fw_capture_duration_seconds);
    h.write_u64(s.last_fw_capture_tick);
    h.write_u64(s.last_sov_claim_tick);
    h.finish()
}

pub(crate) fn fp_structure(s: &UpwellStructureRecord) -> u64 {
    let mut h = FnvHasher::new();
    h.write_i64(s.structure_id);
    h.write_u32(s.type_id);
    h.write_i64(s.owner_corp_id);
    h.write_u32(s.solar_system_id);
    h.write_str(&s.name);
    h.write_f64(s.position[0]);
    h.write_f64(s.position[1]);
    h.write_f64(s.position[2]);
    h.write_str(&s.state);
    h.write_bool(s.quantum_core_installed);
    h.write_u32(s.fuel_blocks);
    h.write_f64(s.shield_hp);
    h.write_f64(s.armor_hp);
    h.write_f64(s.hull_hp);
    h.write_u64(s.reinforced_until_unix);
    h.write_u64(s.last_damage_tick);
    h.write_u64(s.last_damage_unix);
    h.write_u64(s.self_destruct_at_unix.unwrap_or(0));
    h.finish()
}

pub(crate) fn fp_wallet_journal_slice(entries: &[WalletJournalEntry]) -> u64 {
    let mut h = FnvHasher::new();
    h.write_u64(entries.len() as u64);
    for e in entries {
        h.write_i64(e.entry_id);
        h.write_i64(e.character_id);
        h.write_str(&e.entry_type);
        h.write_i64(e.amount.0);
        h.write_i64(e.balance_after.0);
        h.write_str(&e.reason);
    }
    h.finish()
}

pub(crate) fn fp_mails_slice(entries: &[MailRecord]) -> u64 {
    let mut h = FnvHasher::new();
    h.write_u64(entries.len() as u64);
    for m in entries {
        h.write_i64(m.message_id);
        h.write_i64(m.sender_id);
        h.write_i64(m.recipient_id);
        h.write_str(&m.subject);
        h.write_str(&m.body);
    }
    h.finish()
}

pub(crate) fn fp_skill_queue(q: &[SkillQueueEntry]) -> u64 {
    let mut h = FnvHasher::new();
    h.write_u64(q.len() as u64);
    for entry in q {
        h.write_u32(entry.type_id);
        h.write_u8(entry.target_level);
        h.write_f64(entry.start_sp);
        h.write_f64(entry.end_sp);
        h.write_f64(entry.duration_seconds);
        h.write_u64(entry.started_at_unix.unwrap_or(0));
    }
    h.finish()
}

#[inline]
fn hash_map_xor<K, V, F>(h: &mut FnvHasher, map: &HashMap<K, V>, mut hash_entry: F)
where
    F: FnMut(&mut FnvHasher, &K, &V),
{
    h.write_u64(map.len() as u64);
    let mut xor = 0u64;
    let mut sum = 0u64;
    for (k, v) in map {
        let mut eh = FnvHasher::new();
        hash_entry(&mut eh, k, v);
        let entry_fp = eh.finish();
        xor ^= entry_fp;
        sum = sum.wrapping_add(entry_fp.rotate_left(13));
    }
    h.write_u64(xor);
    h.write_u64(sum);
}

#[inline]
fn write_opt_u64(h: &mut FnvHasher, v: Option<u64>) {
    match v {
        Some(x) => {
            h.write_u8(1);
            h.write_u64(x);
        }
        None => h.write_u8(0),
    }
}

#[inline]
fn write_opt_i64(h: &mut FnvHasher, v: Option<i64>) {
    match v {
        Some(x) => {
            h.write_u8(1);
            h.write_i64(x);
        }
        None => h.write_u8(0),
    }
}

#[inline]
fn write_opt_f64(h: &mut FnvHasher, v: Option<f64>) {
    match v {
        Some(x) => {
            h.write_u8(1);
            h.write_f64(x);
        }
        None => h.write_u8(0),
    }
}

#[inline]
fn write_vec3(h: &mut FnvHasher, v: &[f64; 3]) {
    h.write_f64(v[0]);
    h.write_f64(v[1]);
    h.write_f64(v[2]);
}

#[inline]
fn write_opt_vec3(h: &mut FnvHasher, v: &Option<[f64; 3]>) {
    match v {
        Some(arr) => {
            h.write_u8(1);
            write_vec3(h, arr);
        }
        None => h.write_u8(0),
    }
}

pub(crate) fn fp_sp6_lifecycle(st: &crate::Sp6PersistedState) -> u64 {
    let crate::Sp6PersistedState {
        safe_logoffs,
        disconnected_ships,
        self_destructs,
        abyssal_sessions,
        pvp_timers_until,
        npc_timers_until,
        weapon_timers_until,
        criminal_timers_until,
        last_downtime_day_idx,
        ship_combat_hp,
        space_object_meta,
        missions_awaiting_downtime_respawn,
    } = st;
    let mut h = FnvHasher::new();
    hash_map_xor(&mut h, safe_logoffs, |eh, &k, v| {
        let crate::SafeLogoffEntry {
            char_id,
            ship_id,
            solar_system_id,
            started_unix,
            complete_at_unix,
            completed,
        } = *v;
        eh.write_i64(k);
        eh.write_i64(char_id);
        eh.write_i64(ship_id);
        eh.write_u32(solar_system_id);
        eh.write_u64(started_unix);
        eh.write_u64(complete_at_unix);
        eh.write_bool(completed);
    });
    hash_map_xor(&mut h, disconnected_ships, |eh, &k, v| {
        let crate::DisconnectedShipEntry {
            char_id,
            ship_id,
            ship_type_id,
            solar_system_id,
            disconnected_at_unix,
            remove_at_unix,
            ref emergency_warp_dest,
            ref last_position,
            removed_from_space,
            ref pre_warp_position,
        } = *v;
        eh.write_i64(k);
        eh.write_i64(char_id);
        eh.write_i64(ship_id);
        eh.write_u32(ship_type_id);
        eh.write_u32(solar_system_id);
        eh.write_u64(disconnected_at_unix);
        eh.write_u64(remove_at_unix);
        write_opt_vec3(eh, emergency_warp_dest);
        write_vec3(eh, last_position);
        eh.write_bool(removed_from_space);
        write_opt_vec3(eh, pre_warp_position);
    });
    hash_map_xor(&mut h, self_destructs, |eh, &k, v| {
        let crate::SelfDestructPersistedEntry {
            ship_id,
            char_id,
            solar_system_id,
            started_unix,
            detonate_at_unix,
        } = *v;
        eh.write_i64(k);
        eh.write_i64(ship_id);
        eh.write_i64(char_id);
        eh.write_u32(solar_system_id);
        eh.write_u64(started_unix);
        eh.write_u64(detonate_at_unix);
    });
    hash_map_xor(&mut h, abyssal_sessions, |eh, &k, v| {
        let crate::AbyssalPersistedSession {
            char_id,
            ship_id,
            pocket_system_id,
            origin_system_id,
            entered_unix,
            collapse_at_unix,
        } = *v;
        eh.write_i64(k);
        eh.write_i64(char_id);
        eh.write_i64(ship_id);
        eh.write_u32(pocket_system_id);
        eh.write_u32(origin_system_id);
        eh.write_u64(entered_unix);
        eh.write_u64(collapse_at_unix);
    });
    hash_map_xor(&mut h, pvp_timers_until, |eh, &k, &v| {
        eh.write_i64(k);
        eh.write_u64(v);
    });
    hash_map_xor(&mut h, npc_timers_until, |eh, &k, &v| {
        eh.write_i64(k);
        eh.write_u64(v);
    });
    hash_map_xor(&mut h, weapon_timers_until, |eh, &k, &v| {
        eh.write_i64(k);
        eh.write_u64(v);
    });
    hash_map_xor(&mut h, criminal_timers_until, |eh, &k, &v| {
        eh.write_i64(k);
        eh.write_u64(v);
    });
    write_opt_u64(&mut h, *last_downtime_day_idx);
    hash_map_xor(&mut h, ship_combat_hp, |eh, &k, v| {
        let crate::PersistedCombatHp {
            shield_hp,
            armor_hp,
            hull_hp,
        } = *v;
        eh.write_i64(k);
        eh.write_f64(shield_hp);
        eh.write_f64(armor_hp);
        eh.write_f64(hull_hp);
    });
    hash_map_xor(&mut h, space_object_meta, |eh, &k, v| {
        let crate::PersistedSpaceObjectMeta {
            created_unix,
            is_npc_wreck,
            anchor_start_unix,
            anchoring_delay_secs,
            anchor_completed,
            bubble_radius_m,
            drone_ball_id,
            planet_id,
        } = *v;
        eh.write_i64(k);
        write_opt_u64(eh, created_unix);
        eh.write_bool(is_npc_wreck);
        write_opt_u64(eh, anchor_start_unix);
        eh.write_f64(anchoring_delay_secs);
        eh.write_bool(anchor_completed);
        write_opt_f64(eh, bubble_radius_m);
        write_opt_i64(eh, drone_ball_id);
        write_opt_i64(eh, planet_id);
    });
    h.write_u64(missions_awaiting_downtime_respawn.len() as u64);
    for &(c, m) in missions_awaiting_downtime_respawn {
        h.write_i64(c);
        h.write_u32(m);
    }
    h.finish()
}

pub(crate) fn fp_json<T: serde::Serialize>(val: &T) -> u64 {
    let mut h = FnvHasher::new();
    if let Ok(s) = serde_json::to_string(val) {
        h.write_str(&s);
    }
    h.finish()
}

#[derive(Debug, Clone, Default)]
pub struct TableFingerprints {
    pub next_entity_id: i64,
    pub wal_seq: u64,
    pub markets_seeded: bool,
    pub accounts: HashMap<String, u64>,
    pub characters: HashMap<i64, u64>,
    pub skills: HashMap<i64, HashMap<TypeId, u64>>,
    pub skill_queues: HashMap<i64, u64>,
    pub jump_clones: HashMap<i64, u64>,
    pub active_implants: HashMap<i64, u64>,
    pub items: HashMap<i64, u64>,
    pub saved_fittings: HashMap<i64, u64>,
    pub market_orders: std::sync::Arc<HashMap<i64, u64>>,
    pub market_orders_digest: u64,
    pub wallet_journal_len: usize,
    pub wallet_journal_hash: u64,
    pub contracts: HashMap<i64, u64>,
    pub industry_jobs: HashMap<i64, u64>,
    pub planetary_colonies: HashMap<(i64, i64), u64>,
    pub active_missions: HashMap<(i64, u32), u64>,
    pub mission_tally: HashMap<i64, u32>,
    pub standing_transactions: HashMap<i64, u64>,
    pub npe_progress: HashMap<i64, u64>,
    pub bookmarks: HashMap<i64, u64>,
    pub mails_len: usize,
    pub mails_hash: u64,
    pub fleets: HashMap<i64, u64>,
    pub corporations: HashMap<i64, u64>,
    pub alliances: HashMap<i64, u64>,
    pub wars: HashMap<i64, u64>,
    pub corp_projects: HashMap<i64, u64>,
    pub killmails: HashMap<i64, u64>,
    pub kill_rights: HashMap<i64, u64>,
    pub structures: HashMap<i64, u64>,
    pub system_states: HashMap<SolarSystemId, u64>,
    pub committed_idempotency_keys: HashSet<(i64, String)>,
    pub licensed_skins: HashMap<i64, u64>,
    pub applied_ship_skins: HashMap<i64, u32>,
    pub asteroids: HashMap<i64, u64>,
    pub character_last_jump_unix: HashMap<i64, u64>,
    pub character_cspa_charges: HashMap<i64, i64>,
    pub mail_timestamps: HashMap<i64, u64>,
    pub character_login_stats: HashMap<i64, u64>,
    pub item_custom_names: HashMap<i64, u64>,
    pub item_positions: HashMap<i64, u64>,
    pub structure_vulnerability_windows: HashMap<i64, (u64, u64)>,
    pub skill_history: HashMap<i64, u64>,
    pub character_doll_info: HashMap<i64, u64>,
    pub ledger: super::ledger_store::LedgerFingerprints,
    pub d6: super::d6_store::D6Fingerprints,
    pub d6_social: super::d6_social_store::D6SocialFingerprints,
    pub d6_corp_crimewatch: super::d6_corp_crimewatch_store::D6CorpCrimewatchFingerprints,
    pub d6_batch4: super::d6_batch4_store::D6Batch4Fingerprints,
}

/// Synchronizes only modified/inserted/deleted rows from `snap` into `tx` and updates `fp`,
/// returning a `WorldStateWalDelta` for optional legacy `.json.wal` mirroring.

pub fn compute_fingerprints(snap: &WorldStateSnapshot) -> TableFingerprints {
    let mut fp = TableFingerprints {
        next_entity_id: snap.next_entity_id,
        wal_seq: snap.wal_seq,
        markets_seeded: snap.markets_seeded,
        wallet_journal_len: snap.wallet_journal.len(),
        wallet_journal_hash: fp_wallet_journal_slice(&snap.wallet_journal),
        mails_len: snap.mails.len(),
        mails_hash: fp_mails_slice(&snap.mails),
        committed_idempotency_keys: snap.committed_idempotency_keys.clone(),
        mission_tally: snap.mission_tally.clone(),
        applied_ship_skins: snap.applied_ship_skins.clone(),
        ..Default::default()
    };
    for (k, v) in &snap.accounts {
        fp.accounts.insert(k.clone(), fp_account(v));
    }
    for (&k, v) in &snap.characters {
        fp.characters.insert(k, fp_character(v));
    }
    for (&cid, sk_map) in &snap.skills {
        let m = fp.skills.entry(cid).or_default();
        for (&tid, sk) in sk_map {
            let mut h = FnvHasher::new();
            h.write_u32(sk.type_id);
            h.write_u8(sk.level);
            h.write_f64(sk.skill_points);
            m.insert(tid, h.finish());
        }
    }
    for (&cid, q) in &snap.skill_queues {
        fp.skill_queues.insert(cid, fp_skill_queue(q));
    }
    for (&cid, jc) in &snap.jump_clones {
        fp.jump_clones.insert(cid, fp_json(jc));
    }
    for (&cid, imp) in &snap.active_implants {
        fp.active_implants.insert(cid, fp_json(imp));
    }
    for (&id, item) in &snap.items {
        fp.items.insert(id, fp_item(item));
    }
    for (&id, fit) in &snap.saved_fittings {
        fp.saved_fittings.insert(id, fp_json(fit));
    }
    let mut mkt_map = HashMap::with_capacity(snap.market_orders.len());
    for (&id, ord) in &snap.market_orders {
        mkt_map.insert(id, fp_market_order(ord));
    }
    fp.market_orders = std::sync::Arc::new(mkt_map);
    fp.market_orders_digest = fp_market_orders_digest(&snap.market_orders);
    for (&id, c) in &snap.contracts {
        fp.contracts.insert(id, fp_contract(c));
    }
    for (&id, j) in &snap.industry_jobs {
        fp.industry_jobs.insert(id, fp_json(j));
    }
    for (&k, col) in &snap.planetary_colonies {
        fp.planetary_colonies.insert(k, fp_json(col));
    }
    for (&k, m) in &snap.active_missions {
        fp.active_missions.insert(k, fp_json(m));
    }
    for (&cid, list) in &snap.standing_transactions {
        fp.standing_transactions.insert(cid, fp_json(list));
    }
    for (&cid, npe) in &snap.npe_progress {
        fp.npe_progress.insert(cid, fp_json(npe));
    }
    for (&bid, bm) in &snap.bookmarks {
        fp.bookmarks.insert(bid, fp_json(bm));
    }
    for (&fid, fl) in &snap.fleets {
        fp.fleets.insert(fid, fp_json(fl));
    }
    for (&cid, corp) in &snap.corporations {
        fp.corporations.insert(cid, fp_json(corp));
    }
    for (&aid, al) in &snap.alliances {
        fp.alliances.insert(aid, fp_json(al));
    }
    for (&wid, w) in &snap.wars {
        fp.wars.insert(wid, fp_json(w));
    }
    for (&pid, p) in &snap.corp_projects {
        fp.corp_projects.insert(pid, fp_json(p));
    }
    for (&kid, km) in &snap.killmails {
        fp.killmails.insert(kid, fp_json(km));
    }
    for (&krid, kr) in &snap.kill_rights {
        fp.kill_rights.insert(krid, fp_json(kr));
    }
    for (&stid, st) in &snap.structures {
        fp.structures.insert(stid, fp_structure(st));
    }
    for (&sys_id, st) in &snap.system_states {
        fp.system_states.insert(sys_id, fp_system_state(st));
    }
    for (&cid, skins) in &snap.licensed_skins {
        fp.licensed_skins.insert(cid, fp_json(skins));
    }
    for (&id, ast) in &snap.asteroids {
        fp.asteroids.insert(id, fp_json(ast));
    }
    for (&cid, &v) in &snap.character_last_jump_unix {
        fp.character_last_jump_unix.insert(cid, v);
    }
    for (&cid, &cents) in &snap.character_cspa_charges {
        fp.character_cspa_charges.insert(cid, cents.0);
    }
    for (&mid, &v) in &snap.mail_timestamps {
        fp.mail_timestamps.insert(mid, v);
    }
    for (&cid, v) in &snap.character_login_stats {
        fp.character_login_stats.insert(cid, fp_json(v));
    }
    for (&iid, v) in &snap.item_custom_names {
        fp.item_custom_names.insert(iid, fp_json(v));
    }
    for (&iid, v) in &snap.item_positions {
        fp.item_positions.insert(iid, fp_json(v));
    }
    for (&sid, &v) in &snap.structure_vulnerability_windows {
        fp.structure_vulnerability_windows.insert(sid, v);
    }
    for (&cid, v) in &snap.skill_history {
        fp.skill_history.insert(cid, fp_json(v));
    }
    for (&cid, v) in &snap.character_doll_info {
        fp.character_doll_info.insert(cid, fp_json(v));
    }
    fp.ledger = super::ledger_store::compute_ledger_fingerprints(snap);
    fp.d6 = super::d6_store::compute_d6_fingerprints(snap);
    fp.d6_social = super::d6_social_store::compute_d6_social_fp(snap);
    fp.d6_corp_crimewatch =
        super::d6_corp_crimewatch_store::compute_d6_corp_crimewatch_fingerprints(snap);
    fp.d6_batch4 = super::d6_batch4_store::compute_d6_batch4_fingerprints(snap);
    fp
}

#[cfg(test)]
mod tests {
    use super::fp_sp6_lifecycle;
    use crate::{
        AbyssalPersistedSession, DisconnectedShipEntry, PersistedCombatHp,
        PersistedSpaceObjectMeta, SafeLogoffEntry, SelfDestructPersistedEntry, Sp6PersistedState,
    };
    use std::collections::HashMap;

    fn sample_sp6(keys: &[i64], cap: usize, scramble: bool) -> Sp6PersistedState {
        let mut st = Sp6PersistedState {
            safe_logoffs: HashMap::with_capacity(cap),
            disconnected_ships: HashMap::with_capacity(cap),
            self_destructs: HashMap::with_capacity(cap),
            abyssal_sessions: HashMap::with_capacity(cap),
            pvp_timers_until: HashMap::with_capacity(cap),
            npc_timers_until: HashMap::with_capacity(cap),
            weapon_timers_until: HashMap::with_capacity(cap),
            criminal_timers_until: HashMap::with_capacity(cap),
            last_downtime_day_idx: Some(19_850),
            ship_combat_hp: HashMap::with_capacity(cap),
            space_object_meta: HashMap::with_capacity(cap),
            missions_awaiting_downtime_respawn: vec![(90_000_001, 42), (90_000_002, 77)],
        };
        if scramble {
            for dummy in 9_000..9_032 {
                st.pvp_timers_until.insert(dummy, 1);
                st.safe_logoffs.insert(dummy, SafeLogoffEntry::default());
            }
            for dummy in 9_000..9_032 {
                st.pvp_timers_until.remove(&dummy);
                st.safe_logoffs.remove(&dummy);
            }
        }
        for &k in keys {
            let u = k as u64;
            st.safe_logoffs.insert(
                k,
                SafeLogoffEntry {
                    char_id: k,
                    ship_id: k + 10_000,
                    solar_system_id: 30_000_142 + (k as u32),
                    started_unix: 1_700_000_000 + u,
                    complete_at_unix: 1_700_000_030 + u,
                    completed: false,
                },
            );
            st.disconnected_ships.insert(
                k,
                DisconnectedShipEntry {
                    char_id: k,
                    ship_id: k + 20_000,
                    ship_type_id: 587 + (k as u32),
                    solar_system_id: 30_002_187,
                    disconnected_at_unix: 1_700_001_000 + u,
                    remove_at_unix: 1_700_001_060 + u,
                    emergency_warp_dest: Some([10.0 + k as f64, 20.0, 30.0]),
                    last_position: [1.0, 2.0, 3.0 + k as f64],
                    removed_from_space: false,
                    pre_warp_position: Some([-5.0, 0.0, 5.0]),
                },
            );
            st.self_destructs.insert(
                k + 30_000,
                SelfDestructPersistedEntry {
                    ship_id: k + 30_000,
                    char_id: k,
                    solar_system_id: 30_002_537,
                    started_unix: 1_700_002_000 + u,
                    detonate_at_unix: 1_700_002_120 + u,
                },
            );
            st.abyssal_sessions.insert(
                k,
                AbyssalPersistedSession {
                    char_id: k,
                    ship_id: k + 40_000,
                    pocket_system_id: 32_000_001,
                    origin_system_id: 30_000_142,
                    entered_unix: 1_700_003_000 + u,
                    collapse_at_unix: 1_700_004_200 + u,
                },
            );
            st.pvp_timers_until.insert(k, 1_700_010_000 + u);
            st.npc_timers_until.insert(k, 1_700_020_000 + u);
            st.weapon_timers_until.insert(k, 1_700_030_000 + u);
            st.criminal_timers_until.insert(k, 1_700_040_000 + u);
            st.ship_combat_hp.insert(
                k + 50_000,
                PersistedCombatHp {
                    shield_hp: 250.0 + k as f64,
                    armor_hp: 500.0,
                    hull_hp: 650.0,
                },
            );
            st.space_object_meta.insert(
                k + 60_000,
                PersistedSpaceObjectMeta {
                    created_unix: Some(1_700_050_000 + u),
                    is_npc_wreck: false,
                    anchor_start_unix: Some(1_700_050_010 + u),
                    anchoring_delay_secs: 60.0,
                    anchor_completed: false,
                    bubble_radius_m: Some(20_000.0),
                    drone_ball_id: Some(k + 70_000),
                    planet_id: Some(40_000_001),
                },
            );
        }
        st
    }

    #[test]
    fn fp_sp6_lifecycle_order_and_capacity_independent() {
        let forward = sample_sp6(&[101, 202, 303, 404], 0, false);
        let reverse = sample_sp6(&[404, 303, 202, 101], 128, true);
        let perm = sample_sp6(&[303, 101, 404, 202], 16, false);
        assert_eq!(fp_sp6_lifecycle(&forward), fp_sp6_lifecycle(&reverse));
        assert_eq!(fp_sp6_lifecycle(&forward), fp_sp6_lifecycle(&perm));
    }

    #[test]
    fn fp_sp6_lifecycle_sensitive_to_every_persisted_field() {
        let base = sample_sp6(&[101, 202], 4, false);
        let base_fp = fp_sp6_lifecycle(&base);
        let check = |mutate: &dyn Fn(&mut Sp6PersistedState), label: &str| {
            let mut m = base.clone();
            mutate(&mut m);
            assert_ne!(fp_sp6_lifecycle(&m), base_fp, "fingerprint missed {label}");
        };

        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().char_id += 1, "safe_logoffs.char_id");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().ship_id += 1, "safe_logoffs.ship_id");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().solar_system_id += 1, "safe_logoffs.solar_system_id");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().started_unix += 1, "safe_logoffs.started_unix");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().complete_at_unix += 1, "safe_logoffs.complete_at_unix");
        check(&|s| s.safe_logoffs.get_mut(&101).unwrap().completed = true, "safe_logoffs.completed");

        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().char_id += 1, "disconnected.char_id");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().ship_id += 1, "disconnected.ship_id");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().ship_type_id += 1, "disconnected.ship_type_id");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().solar_system_id += 1, "disconnected.solar_system_id");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().disconnected_at_unix += 1, "disconnected.disconnected_at_unix");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().remove_at_unix += 1, "disconnected.remove_at_unix");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().emergency_warp_dest = None, "disconnected.emergency_warp_dest=None");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().emergency_warp_dest = Some([0.0; 3]), "disconnected.emergency_warp_dest=zero");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().last_position[1] += 1.0, "disconnected.last_position");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().removed_from_space = true, "disconnected.removed_from_space");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().pre_warp_position = None, "disconnected.pre_warp_position=None");
        check(&|s| s.disconnected_ships.get_mut(&101).unwrap().pre_warp_position = Some([9.0, 8.0, 7.0]), "disconnected.pre_warp_position=val");

        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().ship_id += 1, "self_destructs.ship_id");
        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().char_id += 1, "self_destructs.char_id");
        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().solar_system_id += 1, "self_destructs.solar_system_id");
        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().started_unix += 1, "self_destructs.started_unix");
        check(&|s| s.self_destructs.get_mut(&30_101).unwrap().detonate_at_unix += 1, "self_destructs.detonate_at_unix");

        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().char_id += 1, "abyssal.char_id");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().ship_id += 1, "abyssal.ship_id");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().pocket_system_id += 1, "abyssal.pocket_system_id");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().origin_system_id += 1, "abyssal.origin_system_id");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().entered_unix += 1, "abyssal.entered_unix");
        check(&|s| s.abyssal_sessions.get_mut(&101).unwrap().collapse_at_unix += 1, "abyssal.collapse_at_unix");

        check(&|s| *s.pvp_timers_until.get_mut(&101).unwrap() += 1, "pvp_timers_until");
        check(&|s| *s.npc_timers_until.get_mut(&101).unwrap() += 1, "npc_timers_until");
        check(&|s| *s.weapon_timers_until.get_mut(&101).unwrap() += 1, "weapon_timers_until");
        check(&|s| *s.criminal_timers_until.get_mut(&101).unwrap() += 1, "criminal_timers_until");
        check(&|s| s.last_downtime_day_idx = None, "last_downtime_day_idx=None");
        check(&|s| s.last_downtime_day_idx = Some(0), "last_downtime_day_idx=Some(0)");
        {
            let mut m_none = base.clone();
            m_none.last_downtime_day_idx = None;
            let mut m_zero = base.clone();
            m_zero.last_downtime_day_idx = Some(0);
            assert_ne!(fp_sp6_lifecycle(&m_none), fp_sp6_lifecycle(&m_zero));
        }

        check(&|s| s.ship_combat_hp.get_mut(&50_101).unwrap().shield_hp += 0.5, "ship_combat_hp.shield_hp");
        check(&|s| s.ship_combat_hp.get_mut(&50_101).unwrap().armor_hp += 0.5, "ship_combat_hp.armor_hp");
        check(&|s| s.ship_combat_hp.get_mut(&50_101).unwrap().hull_hp += 0.5, "ship_combat_hp.hull_hp");

        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().created_unix = None, "meta.created_unix");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().is_npc_wreck = true, "meta.is_npc_wreck");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().anchor_start_unix = None, "meta.anchor_start_unix");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().anchoring_delay_secs += 1.0, "meta.anchoring_delay_secs");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().anchor_completed = true, "meta.anchor_completed");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().bubble_radius_m = None, "meta.bubble_radius_m");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().drone_ball_id = None, "meta.drone_ball_id");
        check(&|s| s.space_object_meta.get_mut(&60_101).unwrap().planet_id = None, "meta.planet_id");

        check(&|s| s.missions_awaiting_downtime_respawn.reverse(), "missions_awaiting_downtime_respawn.order");
        check(&|s| s.missions_awaiting_downtime_respawn[0].1 += 1, "missions_awaiting_downtime_respawn.val");
    }
}
