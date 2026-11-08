//! Table change tracking and FNV-1a row fingerprinting.

use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

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
    pub(crate) fn write_u8(&mut self, v: u8) {
        self.write_bytes(&[v]);
    }

    #[inline]
    pub(crate) fn write_bool(&mut self, v: bool) {
        self.write_u8(u8::from(v));
    }

    #[cfg(test)]
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

#[derive(Debug)]
pub(crate) struct FnvSerError;

impl std::fmt::Display for FnvSerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fnv serialization error")
    }
}

impl std::error::Error for FnvSerError {}

impl serde::ser::Error for FnvSerError {
    fn custom<T: std::fmt::Display>(_msg: T) -> Self {
        Self
    }
}

/// Zero-allocation deterministic FNV-1a serde serializer for persisted state structs.
/// Maps combine order-independently via `(len, xor, sum)`; fields are visited via `#[derive(Serialize)]`.
pub(crate) struct FnvSerializer<'a>(pub(crate) &'a mut FnvHasher);

pub(crate) struct FnvSeqSer<'a>(&'a mut FnvHasher);

impl serde::ser::SerializeSeq for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTuple for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleStruct for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleVariant for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

pub(crate) struct FnvMapSer<'a> {
    parent: &'a mut FnvHasher,
    len: u64,
    xor: u64,
    sum: u64,
    pending_key_hash: u64,
}

impl serde::ser::SerializeMap for FnvMapSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), FnvSerError> {
        self.pending_key_hash = canonical_fp(key);
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        let mut eh = FnvHasher::new();
        eh.write_u64(self.pending_key_hash);
        value.serialize(FnvSerializer(&mut eh))?;
        let entry_fp = eh.finish();
        self.xor ^= entry_fp;
        self.sum = self.sum.wrapping_add(entry_fp.rotate_left(13));
        self.len = self.len.wrapping_add(1);
        Ok(())
    }
    fn end(self) -> Result<(), FnvSerError> {
        self.parent.write_u64(self.len);
        self.parent.write_u64(self.xor);
        self.parent.write_u64(self.sum);
        Ok(())
    }
}

impl serde::ser::SerializeStruct for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        self.0.write_str(key);
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeStructVariant for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        self.0.write_str(key);
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl<'a> serde::Serializer for FnvSerializer<'a> {
    type Ok = ();
    type Error = FnvSerError;
    type SerializeSeq = FnvSeqSer<'a>;
    type SerializeTuple = FnvSeqSer<'a>;
    type SerializeTupleStruct = FnvSeqSer<'a>;
    type SerializeTupleVariant = FnvSeqSer<'a>;
    type SerializeMap = FnvMapSer<'a>;
    type SerializeStruct = FnvSeqSer<'a>;
    type SerializeStructVariant = FnvSeqSer<'a>;

    fn serialize_bool(self, v: bool) -> Result<(), FnvSerError> {
        self.0.write_u8(1);
        self.0.write_bool(v);
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), FnvSerError> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i16(self, v: i16) -> Result<(), FnvSerError> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i32(self, v: i32) -> Result<(), FnvSerError> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i64(self, v: i64) -> Result<(), FnvSerError> {
        self.0.write_u8(2);
        self.0.write_i64(v);
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), FnvSerError> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u16(self, v: u16) -> Result<(), FnvSerError> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u32(self, v: u32) -> Result<(), FnvSerError> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u64(self, v: u64) -> Result<(), FnvSerError> {
        self.0.write_u8(3);
        self.0.write_u64(v);
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), FnvSerError> {
        self.serialize_f64(f64::from(v))
    }
    fn serialize_f64(self, v: f64) -> Result<(), FnvSerError> {
        self.0.write_u8(4);
        let bits = if v == 0.0 { 0u64 } else { v.to_bits() };
        self.0.write_u64(bits);
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), FnvSerError> {
        self.serialize_u32(v as u32)
    }
    fn serialize_str(self, v: &str) -> Result<(), FnvSerError> {
        self.0.write_u8(5);
        self.0.write_str(v);
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), FnvSerError> {
        self.0.write_u8(6);
        self.0.write_u64(v.len() as u64);
        self.0.write_bytes(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<(), FnvSerError> {
        self.0.write_u8(7);
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), FnvSerError> {
        self.0.write_u8(8);
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), FnvSerError> {
        self.0.write_u8(9);
        Ok(())
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<(), FnvSerError> {
        self.0.write_u8(10);
        self.0.write_str(name);
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
    ) -> Result<(), FnvSerError> {
        self.0.write_u8(11);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        self.0.write_u8(12);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        value.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(13);
        if let Some(n) = len {
            self.0.write_u64(n as u64);
        }
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_tuple(self, len: usize) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(14);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(15);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(16);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<FnvMapSer<'a>, FnvSerError> {
        self.0.write_u8(17);
        Ok(FnvMapSer {
            parent: self.0,
            len: 0,
            xor: 0,
            sum: 0,
            pending_key_hash: 0,
        })
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(18);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(19);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
}

#[inline]
pub(crate) fn canonical_fp<T: Serialize + ?Sized>(val: &T) -> u64 {
    let mut h = FnvHasher::new();
    let _ = val.serialize(FnvSerializer(&mut h));
    h.finish()
}

#[inline]
pub(crate) fn fp_account(a: &AccountRecord) -> u64 {
    canonical_fp(a)
}

#[inline]
pub(crate) fn fp_character(c: &CharacterRecord) -> u64 {
    canonical_fp(c)
}

#[inline]
pub(crate) fn fp_item(i: &InventoryItemRecord) -> u64 {
    canonical_fp(i)
}

#[inline]
pub(crate) fn fp_market_order(o: &MarketOrderRecord) -> u64 {
    canonical_fp(o)
}

#[inline]
pub(crate) fn fp_market_orders_digest(orders: &HashMap<i64, MarketOrderRecord>) -> u64 {
    canonical_fp(orders)
}

#[inline]
pub(crate) fn fp_contract(c: &ContractRecord) -> u64 {
    canonical_fp(c)
}

#[inline]
pub(crate) fn fp_system_state(s: &SystemDynamicState) -> u64 {
    canonical_fp(s)
}

#[inline]
pub(crate) fn fp_structure(s: &UpwellStructureRecord) -> u64 {
    canonical_fp(s)
}

#[inline]
pub(crate) fn fp_wallet_journal_slice(entries: &[WalletJournalEntry]) -> u64 {
    canonical_fp(entries)
}

#[inline]
pub(crate) fn fp_mails_slice(entries: &[MailRecord]) -> u64 {
    canonical_fp(entries)
}

#[inline]
pub(crate) fn fp_skill_queue(q: &[SkillQueueEntry]) -> u64 {
    canonical_fp(q)
}

#[inline]
pub(crate) fn fp_sp6_lifecycle(st: &crate::Sp6PersistedState) -> u64 {
    canonical_fp(st)
}

#[inline]
pub(crate) fn fp_json<T: Serialize + ?Sized>(val: &T) -> u64 {
    canonical_fp(val)
}

#[inline]
pub fn fp_asteroid(a: &AsteroidRecord) -> u64 {
    canonical_fp(a)
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
    pub d6: super::notes_lp_sequence_store::D6Fingerprints,
    pub d6_social: super::social_store::D6SocialFingerprints,
    pub d6_corp_crimewatch: super::corp_crimewatch_store::D6CorpCrimewatchFingerprints,
    pub d6_batch4: super::industry_market_pve_store::D6Batch4Fingerprints,
    pub service_domains: super::service_domains_store::ServiceDomainsFingerprints,
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
            m.insert(tid, canonical_fp(sk));
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
        fp.asteroids.insert(id, fp_asteroid(ast));
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
    fp.d6 = super::notes_lp_sequence_store::compute_d6_fingerprints(snap);
    fp.d6_social = super::social_store::compute_d6_social_fp(snap);
    fp.d6_corp_crimewatch =
        super::corp_crimewatch_store::compute_d6_corp_crimewatch_fingerprints(snap);
    fp.d6_batch4 = super::industry_market_pve_store::compute_d6_batch4_fingerprints(snap);
    fp.service_domains =
        super::service_domains_store::compute_service_domains_fingerprints(snap);
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
            mission_initial_npc_counts: HashMap::with_capacity(cap),
            mission_initial_npc_types: HashMap::with_capacity(cap),
            active_scanner_probes: HashMap::with_capacity(cap),
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
