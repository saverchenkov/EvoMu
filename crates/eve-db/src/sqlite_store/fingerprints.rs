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
