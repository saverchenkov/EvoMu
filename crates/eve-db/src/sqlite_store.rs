//! `sqlite_store` — Durable SQLite schema migrations, row-level change tracking, and transactional persistence (`D-3`).
//!
//! Persists all `WorldStateSnapshot` domains across 34 relational SQLite tables without
//! cloning the entire world state on every request.

use crate::{
    AccountRecord, ActiveMissionRecord, AllianceRecord, BookmarkRecord, CharacterRecord,
    ContractRecord, CorpProjectRecord, CorporationRecord, FleetRecord, IndustryJobRecord,
    InventoryItemRecord, IskCents, JumpCloneRecord, KillRightRecord, KillmailRecord, MailRecord,
    MarketOrderRecord, NpeProgressRecord, PlanetaryColonyRecord, SavedFittingRecord,
    SkillQueueEntry, StandingTransactionRecord, SystemDynamicState, TrainedSkillRecord,
    UpwellStructureRecord, WalletJournalEntry, WarRecord, WorldStateSnapshot, WorldStateWalDelta,
};
use eve_sde::{SolarSystemId, TypeId};
use rusqlite::{params, Connection, Transaction};
use std::collections::{HashMap, HashSet};

pub const CURRENT_SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_unix INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS world_meta (
    meta_key TEXT PRIMARY KEY,
    meta_value INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS auth_secrets (
    secret_key TEXT PRIMARY KEY,
    secret_hex TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS accounts (
    username TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL,
    password_token TEXT NOT NULL,
    role INTEGER NOT NULL,
    banned_until_unix INTEGER,
    ban_reason TEXT
);

CREATE TABLE IF NOT EXISTS characters (
    character_id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    corporation_id INTEGER NOT NULL,
    alliance_id INTEGER,
    station_id INTEGER,
    structure_id INTEGER,
    solar_system_id INTEGER NOT NULL,
    active_ship_id INTEGER NOT NULL,
    balance_cents INTEGER NOT NULL,
    version INTEGER NOT NULL,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS skills (
    character_id INTEGER NOT NULL,
    type_id INTEGER NOT NULL,
    level INTEGER NOT NULL,
    skill_points REAL NOT NULL,
    PRIMARY KEY (character_id, type_id),
    FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS skill_queues (
    character_id INTEGER NOT NULL,
    queue_index INTEGER NOT NULL,
    type_id INTEGER NOT NULL,
    target_level INTEGER NOT NULL,
    start_sp REAL NOT NULL,
    end_sp REAL NOT NULL,
    duration_seconds REAL NOT NULL,
    PRIMARY KEY (character_id, queue_index),
    FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS jump_clones (
    character_id INTEGER PRIMARY KEY,
    clones_json TEXT NOT NULL,
    FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS active_implants (
    character_id INTEGER PRIMARY KEY,
    implants_json TEXT NOT NULL,
    FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS items (
    item_id INTEGER PRIMARY KEY,
    type_id INTEGER NOT NULL,
    owner_id INTEGER NOT NULL,
    location_id INTEGER NOT NULL,
    flag INTEGER NOT NULL,
    quantity INTEGER NOT NULL,
    singleton INTEGER NOT NULL,
    version INTEGER NOT NULL,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS saved_fittings (
    fitting_id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    ship_type_id INTEGER NOT NULL,
    description TEXT NOT NULL,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS market_orders (
    order_id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    station_id INTEGER NOT NULL,
    region_id INTEGER NOT NULL,
    type_id INTEGER NOT NULL,
    is_bid INTEGER NOT NULL,
    price_cents INTEGER NOT NULL,
    vol_entered INTEGER NOT NULL,
    vol_remaining INTEGER NOT NULL,
    min_volume INTEGER NOT NULL,
    is_npc INTEGER NOT NULL,
    escrow_cents INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS wallet_journal (
    seq_idx INTEGER PRIMARY KEY,
    entry_id INTEGER NOT NULL,
    character_id INTEGER NOT NULL,
    entry_type TEXT NOT NULL,
    amount_cents INTEGER NOT NULL,
    balance_after_cents INTEGER NOT NULL,
    reason TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS contracts (
    contract_id INTEGER PRIMARY KEY,
    issuer_id INTEGER NOT NULL,
    acceptor_id INTEGER,
    contract_type TEXT NOT NULL,
    status TEXT NOT NULL,
    start_station_id INTEGER NOT NULL,
    end_station_id INTEGER NOT NULL,
    price_cents INTEGER NOT NULL,
    reward_cents INTEGER NOT NULL,
    collateral_cents INTEGER NOT NULL,
    expiration_unix INTEGER NOT NULL,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS industry_jobs (
    job_id INTEGER PRIMARY KEY,
    installer_id INTEGER NOT NULL,
    facility_id INTEGER NOT NULL,
    blueprint_item_id INTEGER NOT NULL,
    blueprint_type_id INTEGER NOT NULL,
    activity_id INTEGER NOT NULL,
    runs INTEGER NOT NULL,
    product_type_id INTEGER NOT NULL,
    status TEXT NOT NULL,
    duration_seconds INTEGER NOT NULL,
    start_unix INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS planetary_colonies (
    owner_id INTEGER NOT NULL,
    planet_id INTEGER NOT NULL,
    solar_system_id INTEGER NOT NULL,
    command_center_level INTEGER NOT NULL,
    pins_count INTEGER NOT NULL,
    last_extracted_unix INTEGER NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (owner_id, planet_id),
    FOREIGN KEY (owner_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS missions (
    character_id INTEGER NOT NULL,
    mission_id INTEGER NOT NULL,
    agent_id INTEGER NOT NULL,
    solar_system_id INTEGER NOT NULL,
    state TEXT NOT NULL,
    current_room_id INTEGER NOT NULL,
    npcs_remaining INTEGER NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (character_id, mission_id),
    FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS mission_tally (
    character_id INTEGER PRIMARY KEY,
    tally INTEGER NOT NULL,
    FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS standings (
    character_id INTEGER PRIMARY KEY,
    tx_json TEXT NOT NULL,
    FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS npe_progress (
    character_id INTEGER PRIMARY KEY,
    node_graph_id INTEGER NOT NULL,
    current_node_id INTEGER NOT NULL,
    completed INTEGER NOT NULL,
    record_json TEXT NOT NULL,
    FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS bookmarks (
    bookmark_id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    label TEXT NOT NULL,
    solar_system_id INTEGER NOT NULL,
    x REAL NOT NULL,
    y REAL NOT NULL,
    z REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS mail (
    seq_idx INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL,
    sender_id INTEGER NOT NULL,
    recipient_id INTEGER NOT NULL,
    subject TEXT NOT NULL,
    body TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS fleets (
    fleet_id INTEGER PRIMARY KEY,
    boss_id INTEGER NOT NULL,
    free_move INTEGER NOT NULL,
    is_registered INTEGER NOT NULL,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS corps (
    corporation_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    ticker TEXT NOT NULL,
    ceo_id INTEGER NOT NULL,
    alliance_id INTEGER,
    tax_rate REAL NOT NULL,
    master_wallet_cents INTEGER NOT NULL,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS alliances (
    alliance_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    short_name TEXT NOT NULL,
    executor_corp_id INTEGER NOT NULL,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wars (
    war_id INTEGER PRIMARY KEY,
    aggressor_corp_id INTEGER NOT NULL,
    defender_corp_id INTEGER NOT NULL,
    mutual INTEGER NOT NULL,
    started_unix INTEGER NOT NULL,
    active INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS corp_projects (
    project_id INTEGER PRIMARY KEY,
    corporation_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    target_quantity INTEGER NOT NULL,
    current_quantity INTEGER NOT NULL,
    isk_per_unit_cents INTEGER NOT NULL,
    escrow_remaining_cents INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS killmails (
    kill_id INTEGER PRIMARY KEY,
    victim_character_id INTEGER NOT NULL,
    attacker_character_id INTEGER NOT NULL,
    solar_system_id INTEGER NOT NULL,
    ship_type_id INTEGER NOT NULL,
    total_value_isk REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS kill_rights (
    kill_right_id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL,
    target_id INTEGER NOT NULL,
    activation_fee_cents INTEGER NOT NULL,
    is_active_suspect INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS structures (
    structure_id INTEGER PRIMARY KEY,
    type_id INTEGER NOT NULL,
    owner_corp_id INTEGER NOT NULL,
    solar_system_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    state TEXT NOT NULL,
    quantum_core_installed INTEGER NOT NULL,
    fuel_blocks INTEGER NOT NULL,
    shield_hp REAL NOT NULL,
    armor_hp REAL NOT NULL,
    hull_hp REAL NOT NULL,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS system_states (
    solar_system_id INTEGER PRIMARY KEY,
    dbs_multiplier REAL NOT NULL,
    ess_main_bank_cents INTEGER NOT NULL,
    ess_reserve_bank_cents INTEGER NOT NULL,
    sov_alliance_id INTEGER,
    record_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS idempotency_keys (
    character_id INTEGER NOT NULL,
    idem_key TEXT NOT NULL,
    PRIMARY KEY (character_id, idem_key)
);

CREATE TABLE IF NOT EXISTS licensed_skins (
    character_id INTEGER PRIMARY KEY,
    skins_json TEXT NOT NULL,
    FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS applied_ship_skins (
    ship_id INTEGER PRIMARY KEY,
    skin_id INTEGER NOT NULL
);
"#;

pub fn apply_migrations(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(|e| e.to_string())?;
    conn.execute_batch(SCHEMA_V1_SQL)
        .map_err(|e| e.to_string())?;
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_unix) VALUES (?1, ?2, ?3)",
        params![CURRENT_SCHEMA_VERSION, "0001_initial_world_schema", now_unix],
    )
    .map_err(|e| e.to_string())?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_id', 200000000), ('wal_seq', 0), ('markets_seeded', 0);",
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_or_init_jwt_secret(
    conn: &Connection,
    fallback_gen: impl FnOnce() -> [u8; 32],
) -> Result<[u8; 32], String> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT secret_hex FROM auth_secrets WHERE secret_key = 'jwt_signing_secret'",
            [],
            |r| r.get(0),
        )
        .ok();
    if let Some(hex) = existing {
        if hex.len() == 64 {
            let mut out = [0u8; 32];
            let mut valid = true;
            for (i, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
                let s = std::str::from_utf8(chunk).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(s, 16) {
                    out[i] = b;
                } else {
                    valid = false;
                    break;
                }
            }
            if valid {
                return Ok(out);
            }
        }
    }
    let generated = fallback_gen();
    let mut hex = String::with_capacity(64);
    for b in generated {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', ?1)",
        params![hex],
    )
    .map_err(|e| e.to_string())?;
    Ok(generated)
}

pub fn save_jwt_secret(conn: &Connection, secret: &[u8; 32]) -> Result<(), String> {
    let mut hex = String::with_capacity(64);
    for b in secret {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', ?1)",
        params![hex],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Fast zero-allocation FNV-1a 64-bit hasher for row-level dirty detection.
struct FnvHasher(u64);

impl FnvHasher {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;

    #[inline]
    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    #[inline]
    fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    #[inline]
    fn write_u64(&mut self, v: u64) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    fn write_i64(&mut self, v: i64) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    fn write_u32(&mut self, v: u32) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    fn write_i32(&mut self, v: i32) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    fn write_u8(&mut self, v: u8) {
        self.write_bytes(&[v]);
    }

    #[inline]
    fn write_bool(&mut self, v: bool) {
        self.write_u8(u8::from(v));
    }

    #[inline]
    fn write_f64(&mut self, v: f64) {
        self.write_u64(v.to_bits());
    }

    #[inline]
    fn write_str(&mut self, s: &str) {
        self.write_u64(s.len() as u64);
        self.write_bytes(s.as_bytes());
    }

    #[inline]
    fn finish(self) -> u64 {
        self.0
    }
}

fn fp_account(a: &AccountRecord) -> u64 {
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

fn fp_character(c: &CharacterRecord) -> u64 {
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
    h.write_f64(c.skill_points);
    h.write_f64(c.free_skill_points);
    h.write_u64(c.last_clone_jump_unix);
    h.write_f64(c.jump_fatigue_minutes);
    h.write_u64(c.jump_cooldown_until_unix);
    h.write_u64(c.version);
    h.write_u64(c.system_epoch);
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

fn fp_item(i: &InventoryItemRecord) -> u64 {
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

fn fp_market_order(o: &MarketOrderRecord) -> u64 {
    let mut h = FnvHasher::new();
    h.write_i64(o.order_id);
    h.write_i64(o.owner_id);
    h.write_i64(o.station_id);
    h.write_u32(o.region_id);
    h.write_u32(o.type_id);
    h.write_bool(o.is_bid);
    h.write_i64(o.price.0);
    h.write_i32(o.vol_entered);
    h.write_i32(o.vol_remaining);
    h.write_i32(o.min_volume);
    h.write_bool(o.is_npc);
    h.write_i64(o.escrow.0);
    h.finish()
}

fn fp_contract(c: &ContractRecord) -> u64 {
    let mut h = FnvHasher::new();
    h.write_i64(c.contract_id);
    h.write_i64(c.issuer_id);
    h.write_i64(c.acceptor_id.unwrap_or(-1));
    h.write_str(&c.contract_type);
    h.write_str(&c.status);
    h.write_i64(c.start_station_id);
    h.write_i64(c.end_station_id);
    h.write_i64(c.price.0);
    h.write_i64(c.reward.0);
    h.write_i64(c.collateral.0);
    h.write_u64(c.expiration_unix);
    for &id in &c.item_ids {
        h.write_i64(id);
    }
    h.finish()
}

fn fp_system_state(s: &SystemDynamicState) -> u64 {
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

fn fp_structure(s: &UpwellStructureRecord) -> u64 {
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
    h.finish()
}

fn fp_wallet_journal_slice(entries: &[WalletJournalEntry]) -> u64 {
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

fn fp_mails_slice(entries: &[MailRecord]) -> u64 {
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

fn fp_json<T: serde::Serialize>(val: &T) -> u64 {
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
    pub market_orders: HashMap<i64, u64>,
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
}

/// Synchronizes only modified/inserted/deleted rows from `snap` into `tx` and updates `fp`,
/// returning a `WorldStateWalDelta` for optional legacy `.json.wal` mirroring.
pub(crate) fn sync_dirty_to_sqlite(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
    need_wal_delta: bool,
) -> Result<WorldStateWalDelta, String> {
    let mut delta = WorldStateWalDelta {
        seq: snap.wal_seq,
        next_entity_id: snap.next_entity_id,
        ..Default::default()
    };
    let mut cold_changed = false;

    if fp.next_entity_id != snap.next_entity_id
        || fp.wal_seq != snap.wal_seq
        || fp.markets_seeded != snap.markets_seeded
    {
        if fp.markets_seeded != snap.markets_seeded {
            cold_changed = true;
        }
        tx.execute(
            "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_id', ?1), ('wal_seq', ?2), ('markets_seeded', ?3)",
            params![
                snap.next_entity_id,
                snap.wal_seq as i64,
                i64::from(snap.markets_seeded)
            ],
        )
        .map_err(|e| e.to_string())?;
        fp.next_entity_id = snap.next_entity_id;
        fp.wal_seq = snap.wal_seq;
        fp.markets_seeded = snap.markets_seeded;
    }

    // 1. accounts
    let removed_accounts: Vec<String> = fp
        .accounts
        .keys()
        .filter(|k| !snap.accounts.contains_key(*k))
        .cloned()
        .collect();
    for k in removed_accounts {
        cold_changed = true;
        tx.execute("DELETE FROM accounts WHERE username = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.accounts.remove(&k);
    }
    for (k, v) in &snap.accounts {
        let h = fp_account(v);
        if fp.accounts.get(k) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO accounts (username, user_id, password_token, role, banned_until_unix, ban_reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    v.username,
                    v.user_id as i64,
                    v.password_token,
                    v.role as i64,
                    v.banned_until_unix.map(|u| u as i64),
                    v.ban_reason,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.accounts.insert(k.clone(), h);
        }
    }

    // 2. characters
    let removed_chars: Vec<i64> = fp
        .characters
        .keys()
        .filter(|k| !snap.characters.contains_key(*k))
        .copied()
        .collect();
    for k in removed_chars {
        cold_changed = true;
        tx.execute("DELETE FROM characters WHERE character_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.characters.remove(&k);
    }
    for (&k, v) in &snap.characters {
        let h = fp_character(v);
        if fp.characters.get(&k) != Some(&h) {
            let rec_json = serde_json::to_string(v).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO characters (character_id, user_id, name, corporation_id, alliance_id, station_id, structure_id, solar_system_id, active_ship_id, balance_cents, version, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(character_id) DO UPDATE SET
                     user_id = excluded.user_id,
                     name = excluded.name,
                     corporation_id = excluded.corporation_id,
                     alliance_id = excluded.alliance_id,
                     station_id = excluded.station_id,
                     structure_id = excluded.structure_id,
                     solar_system_id = excluded.solar_system_id,
                     active_ship_id = excluded.active_ship_id,
                     balance_cents = excluded.balance_cents,
                     version = excluded.version,
                     record_json = excluded.record_json",
                params![
                    v.character_id,
                    v.user_id as i64,
                    v.name,
                    v.corporation_id,
                    v.alliance_id,
                    v.station_id,
                    v.structure_id,
                    v.solar_system_id as i64,
                    v.active_ship_id,
                    v.balance.0,
                    v.version as i64,
                    rec_json,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.characters.insert(k, h);
            delta.upserted_characters.push(v.clone());
        }
    }

    // 3. skills
    let removed_skill_chars: Vec<i64> = fp
        .skills
        .keys()
        .filter(|k| !snap.skills.contains_key(*k))
        .copied()
        .collect();
    for cid in removed_skill_chars {
        cold_changed = true;
        tx.execute("DELETE FROM skills WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.skills.remove(&cid);
    }
    for (&cid, sk_map) in &snap.skills {
        let fp_map = fp.skills.entry(cid).or_default();
        let rem_tids: Vec<TypeId> = fp_map
            .keys()
            .filter(|tid| !sk_map.contains_key(*tid))
            .copied()
            .collect();
        for tid in rem_tids {
            cold_changed = true;
            tx.execute(
                "DELETE FROM skills WHERE character_id = ?1 AND type_id = ?2",
                params![cid, tid as i64],
            )
            .map_err(|e| e.to_string())?;
            fp_map.remove(&tid);
        }
        for (&tid, sk) in sk_map {
            let mut h = FnvHasher::new();
            h.write_u32(sk.type_id);
            h.write_u8(sk.level);
            h.write_f64(sk.skill_points);
            let digest = h.finish();
            if fp_map.get(&tid) != Some(&digest) {
                cold_changed = true;
                tx.execute(
                    "INSERT OR REPLACE INTO skills (character_id, type_id, level, skill_points) VALUES (?1, ?2, ?3, ?4)",
                    params![cid, tid as i64, sk.level as i64, sk.skill_points],
                )
                .map_err(|e| e.to_string())?;
                fp_map.insert(tid, digest);
            }
        }
    }

    // 4. skill_queues
    let rem_q_chars: Vec<i64> = fp
        .skill_queues
        .keys()
        .filter(|k| !snap.skill_queues.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_q_chars {
        cold_changed = true;
        tx.execute("DELETE FROM skill_queues WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.skill_queues.remove(&cid);
    }
    for (&cid, q) in &snap.skill_queues {
        let h = fp_json(q);
        if fp.skill_queues.get(&cid) != Some(&h) {
            cold_changed = true;
            tx.execute("DELETE FROM skill_queues WHERE character_id = ?1", params![cid])
                .map_err(|e| e.to_string())?;
            for (idx, entry) in q.iter().enumerate() {
                tx.execute(
                    "INSERT INTO skill_queues (character_id, queue_index, type_id, target_level, start_sp, end_sp, duration_seconds)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        cid,
                        idx as i64,
                        entry.type_id as i64,
                        entry.target_level as i64,
                        entry.start_sp,
                        entry.end_sp,
                        entry.duration_seconds,
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
            fp.skill_queues.insert(cid, h);
        }
    }

    // 5. jump_clones
    let rem_jc: Vec<i64> = fp
        .jump_clones
        .keys()
        .filter(|k| !snap.jump_clones.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_jc {
        cold_changed = true;
        tx.execute("DELETE FROM jump_clones WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.jump_clones.remove(&cid);
    }
    for (&cid, clones) in &snap.jump_clones {
        let h = fp_json(clones);
        if fp.jump_clones.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(clones).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO jump_clones (character_id, clones_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map_err(|e| e.to_string())?;
            fp.jump_clones.insert(cid, h);
        }
    }

    // 6. active_implants
    let rem_imp: Vec<i64> = fp
        .active_implants
        .keys()
        .filter(|k| !snap.active_implants.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_imp {
        cold_changed = true;
        tx.execute("DELETE FROM active_implants WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.active_implants.remove(&cid);
    }
    for (&cid, imps) in &snap.active_implants {
        let h = fp_json(imps);
        if fp.active_implants.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(imps).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO active_implants (character_id, implants_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map_err(|e| e.to_string())?;
            fp.active_implants.insert(cid, h);
        }
    }

    // 7. items
    let removed_items: Vec<i64> = fp
        .items
        .keys()
        .filter(|k| !snap.items.contains_key(*k))
        .copied()
        .collect();
    for id in removed_items {
        tx.execute("DELETE FROM items WHERE item_id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        fp.items.remove(&id);
        delta.removed_items.push(id);
    }
    for (&id, item) in &snap.items {
        let h = fp_item(item);
        if fp.items.get(&id) != Some(&h) {
            let rec_json = serde_json::to_string(item).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO items (item_id, type_id, owner_id, location_id, flag, quantity, singleton, version, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    item.item_id,
                    item.type_id as i64,
                    item.owner_id,
                    item.location_id,
                    item.flag as i64,
                    item.quantity as i64,
                    i64::from(item.singleton),
                    item.version as i64,
                    rec_json,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.items.insert(id, h);
            delta.upserted_items.push(item.clone());
        }
    }

    // 8. saved_fittings
    let rem_fit: Vec<i64> = fp
        .saved_fittings
        .keys()
        .filter(|k| !snap.saved_fittings.contains_key(*k))
        .copied()
        .collect();
    for fid in rem_fit {
        cold_changed = true;
        tx.execute("DELETE FROM saved_fittings WHERE fitting_id = ?1", params![fid])
            .map_err(|e| e.to_string())?;
        fp.saved_fittings.remove(&fid);
    }
    for (&fid, fit) in &snap.saved_fittings {
        let h = fp_json(fit);
        if fp.saved_fittings.get(&fid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(fit).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO saved_fittings (fitting_id, owner_id, name, ship_type_id, description, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![fit.fitting_id, fit.owner_id, fit.name, fit.ship_type_id as i64, fit.description, js],
            )
            .map_err(|e| e.to_string())?;
            fp.saved_fittings.insert(fid, h);
        }
    }

    // 9. market_orders
    let removed_orders: Vec<i64> = fp
        .market_orders
        .keys()
        .filter(|k| !snap.market_orders.contains_key(*k))
        .copied()
        .collect();
    for oid in removed_orders {
        tx.execute("DELETE FROM market_orders WHERE order_id = ?1", params![oid])
            .map_err(|e| e.to_string())?;
        fp.market_orders.remove(&oid);
        delta.removed_market_orders.push(oid);
    }
    for (&oid, ord) in &snap.market_orders {
        let h = fp_market_order(ord);
        if fp.market_orders.get(&oid) != Some(&h) {
            tx.execute(
                "INSERT OR REPLACE INTO market_orders (order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    ord.order_id,
                    ord.owner_id,
                    ord.station_id,
                    ord.region_id as i64,
                    ord.type_id as i64,
                    i64::from(ord.is_bid),
                    ord.price.0,
                    ord.vol_entered as i64,
                    ord.vol_remaining as i64,
                    ord.min_volume as i64,
                    i64::from(ord.is_npc),
                    ord.escrow.0,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.market_orders.insert(oid, h);
            delta.upserted_market_orders.push(ord.clone());
        }
    }

    // 10. wallet_journal
    if snap.wallet_journal.len() < fp.wallet_journal_len
        || (fp.wallet_journal_len > 0
            && fp_wallet_journal_slice(&snap.wallet_journal[..fp.wallet_journal_len])
                != fp.wallet_journal_hash)
    {
        cold_changed = true;
        tx.execute("DELETE FROM wallet_journal", [])
            .map_err(|e| e.to_string())?;
        fp.wallet_journal_len = 0;
        fp.wallet_journal_hash = fp_wallet_journal_slice(&[]);
    }
    if snap.wallet_journal.len() > fp.wallet_journal_len {
        for (idx, entry) in snap.wallet_journal[fp.wallet_journal_len..].iter().enumerate() {
            let seq_idx = (fp.wallet_journal_len + idx) as i64;
            tx.execute(
                "INSERT OR REPLACE INTO wallet_journal (seq_idx, entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    seq_idx,
                    entry.entry_id,
                    entry.character_id,
                    entry.entry_type,
                    entry.amount.0,
                    entry.balance_after.0,
                    entry.reason,
                ],
            )
            .map_err(|e| e.to_string())?;
            delta.appended_wallet_journal.push(entry.clone());
        }
        fp.wallet_journal_len = snap.wallet_journal.len();
        fp.wallet_journal_hash = fp_wallet_journal_slice(&snap.wallet_journal);
    }

    // 11. contracts
    let rem_contracts: Vec<i64> = fp
        .contracts
        .keys()
        .filter(|k| !snap.contracts.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_contracts {
        cold_changed = true;
        tx.execute("DELETE FROM contracts WHERE contract_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.contracts.remove(&cid);
    }
    for (&cid, c) in &snap.contracts {
        let h = fp_contract(c);
        if fp.contracts.get(&cid) != Some(&h) {
            let js = serde_json::to_string(c).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO contracts (contract_id, issuer_id, acceptor_id, contract_type, status, start_station_id, end_station_id, price_cents, reward_cents, collateral_cents, expiration_unix, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    c.contract_id,
                    c.issuer_id,
                    c.acceptor_id,
                    c.contract_type,
                    c.status,
                    c.start_station_id,
                    c.end_station_id,
                    c.price.0,
                    c.reward.0,
                    c.collateral.0,
                    c.expiration_unix as i64,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.contracts.insert(cid, h);
            delta.upserted_contracts.push(c.clone());
        }
    }

    // 12. industry_jobs
    let rem_jobs: Vec<i64> = fp
        .industry_jobs
        .keys()
        .filter(|k| !snap.industry_jobs.contains_key(*k))
        .copied()
        .collect();
    for jid in rem_jobs {
        cold_changed = true;
        tx.execute("DELETE FROM industry_jobs WHERE job_id = ?1", params![jid])
            .map_err(|e| e.to_string())?;
        fp.industry_jobs.remove(&jid);
    }
    for (&jid, j) in &snap.industry_jobs {
        let h = fp_json(j);
        if fp.industry_jobs.get(&jid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO industry_jobs (job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    j.job_id,
                    j.installer_id,
                    j.facility_id,
                    j.blueprint_item_id,
                    j.blueprint_type_id as i64,
                    j.activity_id as i64,
                    j.runs as i64,
                    j.product_type_id as i64,
                    j.status,
                    j.duration_seconds as i64,
                    j.start_unix as i64,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.industry_jobs.insert(jid, h);
        }
    }

    // 13. planetary_colonies
    let rem_col: Vec<(i64, i64)> = fp
        .planetary_colonies
        .keys()
        .filter(|k| !snap.planetary_colonies.contains_key(*k))
        .copied()
        .collect();
    for (oid, pid) in rem_col {
        cold_changed = true;
        tx.execute(
            "DELETE FROM planetary_colonies WHERE owner_id = ?1 AND planet_id = ?2",
            params![oid, pid],
        )
        .map_err(|e| e.to_string())?;
        fp.planetary_colonies.remove(&(oid, pid));
    }
    for (&(oid, pid), col) in &snap.planetary_colonies {
        let h = fp_json(col);
        if fp.planetary_colonies.get(&(oid, pid)) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(col).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO planetary_colonies (owner_id, planet_id, solar_system_id, command_center_level, pins_count, last_extracted_unix, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    oid,
                    pid,
                    col.solar_system_id as i64,
                    col.command_center_level as i64,
                    col.pins_count as i64,
                    col.last_extracted_unix as i64,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.planetary_colonies.insert((oid, pid), h);
        }
    }

    // 14. active_missions
    let rem_mis: Vec<(i64, u32)> = fp
        .active_missions
        .keys()
        .filter(|k| !snap.active_missions.contains_key(*k))
        .copied()
        .collect();
    for (cid, mid) in rem_mis {
        cold_changed = true;
        tx.execute(
            "DELETE FROM missions WHERE character_id = ?1 AND mission_id = ?2",
            params![cid, mid as i64],
        )
        .map_err(|e| e.to_string())?;
        fp.active_missions.remove(&(cid, mid));
    }
    for (&(cid, mid), m) in &snap.active_missions {
        let h = fp_json(m);
        if fp.active_missions.get(&(cid, mid)) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(m).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    cid,
                    mid as i64,
                    m.agent_id,
                    m.solar_system_id as i64,
                    m.state,
                    m.current_room_id as i64,
                    m.npcs_remaining as i64,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.active_missions.insert((cid, mid), h);
        }
    }

    // 15. mission_tally
    let rem_tally: Vec<i64> = fp
        .mission_tally
        .keys()
        .filter(|k| !snap.mission_tally.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_tally {
        cold_changed = true;
        tx.execute("DELETE FROM mission_tally WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.mission_tally.remove(&cid);
    }
    for (&cid, &t) in &snap.mission_tally {
        if fp.mission_tally.get(&cid) != Some(&t) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO mission_tally (character_id, tally) VALUES (?1, ?2)",
                params![cid, t as i64],
            )
            .map_err(|e| e.to_string())?;
            fp.mission_tally.insert(cid, t);
        }
    }

    // 16. standing_transactions
    let rem_st_tx: Vec<i64> = fp
        .standing_transactions
        .keys()
        .filter(|k| !snap.standing_transactions.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_st_tx {
        cold_changed = true;
        tx.execute("DELETE FROM standings WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.standing_transactions.remove(&cid);
    }
    for (&cid, list) in &snap.standing_transactions {
        let h = fp_json(list);
        if fp.standing_transactions.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(list).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO standings (character_id, tx_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map_err(|e| e.to_string())?;
            fp.standing_transactions.insert(cid, h);
        }
    }

    // 17. npe_progress
    let rem_npe: Vec<i64> = fp
        .npe_progress
        .keys()
        .filter(|k| !snap.npe_progress.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_npe {
        cold_changed = true;
        tx.execute("DELETE FROM npe_progress WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.npe_progress.remove(&cid);
    }
    for (&cid, npe) in &snap.npe_progress {
        let h = fp_json(npe);
        if fp.npe_progress.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(npe).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO npe_progress (character_id, node_graph_id, current_node_id, completed, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    cid,
                    npe.node_graph_id as i64,
                    npe.current_node_id as i64,
                    i64::from(npe.completed),
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.npe_progress.insert(cid, h);
        }
    }

    // 18. bookmarks
    let rem_bm: Vec<i64> = fp
        .bookmarks
        .keys()
        .filter(|k| !snap.bookmarks.contains_key(*k))
        .copied()
        .collect();
    for bid in rem_bm {
        cold_changed = true;
        tx.execute("DELETE FROM bookmarks WHERE bookmark_id = ?1", params![bid])
            .map_err(|e| e.to_string())?;
        fp.bookmarks.remove(&bid);
    }
    for (&bid, bm) in &snap.bookmarks {
        let h = fp_json(bm);
        if fp.bookmarks.get(&bid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO bookmarks (bookmark_id, owner_id, label, solar_system_id, x, y, z)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![bm.bookmark_id, bm.owner_id, bm.label, bm.solar_system_id as i64, bm.x, bm.y, bm.z],
            )
            .map_err(|e| e.to_string())?;
            fp.bookmarks.insert(bid, h);
        }
    }

    // 19. mails
    if snap.mails.len() < fp.mails_len
        || (fp.mails_len > 0
            && fp_mails_slice(&snap.mails[..fp.mails_len]) != fp.mails_hash)
    {
        cold_changed = true;
        tx.execute("DELETE FROM mail", []).map_err(|e| e.to_string())?;
        fp.mails_len = 0;
        fp.mails_hash = fp_mails_slice(&[]);
    }
    if snap.mails.len() > fp.mails_len {
        cold_changed = true;
        for (idx, m) in snap.mails[fp.mails_len..].iter().enumerate() {
            let seq_idx = (fp.mails_len + idx) as i64;
            tx.execute(
                "INSERT OR REPLACE INTO mail (seq_idx, message_id, sender_id, recipient_id, subject, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![seq_idx, m.message_id, m.sender_id, m.recipient_id, m.subject, m.body],
            )
            .map_err(|e| e.to_string())?;
        }
        fp.mails_len = snap.mails.len();
        fp.mails_hash = fp_mails_slice(&snap.mails);
    }

    // 20. fleets
    let rem_fl: Vec<i64> = fp
        .fleets
        .keys()
        .filter(|k| !snap.fleets.contains_key(*k))
        .copied()
        .collect();
    for fid in rem_fl {
        cold_changed = true;
        tx.execute("DELETE FROM fleets WHERE fleet_id = ?1", params![fid])
            .map_err(|e| e.to_string())?;
        fp.fleets.remove(&fid);
    }
    for (&fid, fl) in &snap.fleets {
        let h = fp_json(fl);
        if fp.fleets.get(&fid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(fl).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO fleets (fleet_id, boss_id, free_move, is_registered, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![fl.fleet_id, fl.boss_id, i64::from(fl.free_move), i64::from(fl.is_registered), js],
            )
            .map_err(|e| e.to_string())?;
            fp.fleets.insert(fid, h);
        }
    }

    // 21. corporations
    let rem_corps: Vec<i64> = fp
        .corporations
        .keys()
        .filter(|k| !snap.corporations.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_corps {
        cold_changed = true;
        tx.execute("DELETE FROM corps WHERE corporation_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.corporations.remove(&cid);
    }
    for (&cid, corp) in &snap.corporations {
        let h = fp_json(corp);
        if fp.corporations.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(corp).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO corps (corporation_id, name, ticker, ceo_id, alliance_id, tax_rate, master_wallet_cents, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    corp.corporation_id,
                    corp.name,
                    corp.ticker,
                    corp.ceo_id,
                    corp.alliance_id,
                    corp.tax_rate,
                    corp.master_wallet.0,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.corporations.insert(cid, h);
        }
    }

    // 22. alliances
    let rem_all: Vec<i64> = fp
        .alliances
        .keys()
        .filter(|k| !snap.alliances.contains_key(*k))
        .copied()
        .collect();
    for aid in rem_all {
        cold_changed = true;
        tx.execute("DELETE FROM alliances WHERE alliance_id = ?1", params![aid])
            .map_err(|e| e.to_string())?;
        fp.alliances.remove(&aid);
    }
    for (&aid, al) in &snap.alliances {
        let h = fp_json(al);
        if fp.alliances.get(&aid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(al).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO alliances (alliance_id, name, short_name, executor_corp_id, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![al.alliance_id, al.name, al.short_name, al.executor_corp_id, js],
            )
            .map_err(|e| e.to_string())?;
            fp.alliances.insert(aid, h);
        }
    }

    // 23. wars
    let rem_wars: Vec<i64> = fp
        .wars
        .keys()
        .filter(|k| !snap.wars.contains_key(*k))
        .copied()
        .collect();
    for wid in rem_wars {
        cold_changed = true;
        tx.execute("DELETE FROM wars WHERE war_id = ?1", params![wid])
            .map_err(|e| e.to_string())?;
        fp.wars.remove(&wid);
    }
    for (&wid, w) in &snap.wars {
        let h = fp_json(w);
        if fp.wars.get(&wid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO wars (war_id, aggressor_corp_id, defender_corp_id, mutual, started_unix, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    w.war_id,
                    w.aggressor_corp_id,
                    w.defender_corp_id,
                    i64::from(w.mutual),
                    w.started_unix as i64,
                    i64::from(w.active),
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.wars.insert(wid, h);
        }
    }

    // 24. corp_projects
    let rem_proj: Vec<i64> = fp
        .corp_projects
        .keys()
        .filter(|k| !snap.corp_projects.contains_key(*k))
        .copied()
        .collect();
    for pid in rem_proj {
        cold_changed = true;
        tx.execute("DELETE FROM corp_projects WHERE project_id = ?1", params![pid])
            .map_err(|e| e.to_string())?;
        fp.corp_projects.remove(&pid);
    }
    for (&pid, p) in &snap.corp_projects {
        let h = fp_json(p);
        if fp.corp_projects.get(&pid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO corp_projects (project_id, corporation_id, name, target_quantity, current_quantity, isk_per_unit_cents, escrow_remaining_cents)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    p.project_id,
                    p.corporation_id,
                    p.name,
                    p.target_quantity as i64,
                    p.current_quantity as i64,
                    p.isk_per_unit.0,
                    p.escrow_remaining.0,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.corp_projects.insert(pid, h);
        }
    }

    // 25. killmails
    let rem_km: Vec<i64> = fp
        .killmails
        .keys()
        .filter(|k| !snap.killmails.contains_key(*k))
        .copied()
        .collect();
    for kid in rem_km {
        cold_changed = true;
        tx.execute("DELETE FROM killmails WHERE kill_id = ?1", params![kid])
            .map_err(|e| e.to_string())?;
        fp.killmails.remove(&kid);
    }
    for (&kid, km) in &snap.killmails {
        let h = fp_json(km);
        if fp.killmails.get(&kid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO killmails (kill_id, victim_character_id, attacker_character_id, solar_system_id, ship_type_id, total_value_isk)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    km.kill_id,
                    km.victim_character_id,
                    km.attacker_character_id,
                    km.solar_system_id as i64,
                    km.ship_type_id as i64,
                    km.total_value_isk,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.killmails.insert(kid, h);
        }
    }

    // 26. kill_rights
    let rem_kr: Vec<i64> = fp
        .kill_rights
        .keys()
        .filter(|k| !snap.kill_rights.contains_key(*k))
        .copied()
        .collect();
    for krid in rem_kr {
        cold_changed = true;
        tx.execute("DELETE FROM kill_rights WHERE kill_right_id = ?1", params![krid])
            .map_err(|e| e.to_string())?;
        fp.kill_rights.remove(&krid);
    }
    for (&krid, kr) in &snap.kill_rights {
        let h = fp_json(kr);
        if fp.kill_rights.get(&krid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO kill_rights (kill_right_id, owner_id, target_id, activation_fee_cents, is_active_suspect)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    kr.kill_right_id,
                    kr.owner_id,
                    kr.target_id,
                    kr.activation_fee.0,
                    i64::from(kr.is_active_suspect),
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.kill_rights.insert(krid, h);
        }
    }

    // 27. structures
    let rem_st: Vec<i64> = fp
        .structures
        .keys()
        .filter(|k| !snap.structures.contains_key(*k))
        .copied()
        .collect();
    for stid in rem_st {
        cold_changed = true;
        tx.execute("DELETE FROM structures WHERE structure_id = ?1", params![stid])
            .map_err(|e| e.to_string())?;
        fp.structures.remove(&stid);
    }
    for (&stid, st) in &snap.structures {
        let h = fp_structure(st);
        if fp.structures.get(&stid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(st).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO structures (structure_id, type_id, owner_corp_id, solar_system_id, name, state, quantum_core_installed, fuel_blocks, shield_hp, armor_hp, hull_hp, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    st.structure_id,
                    st.type_id as i64,
                    st.owner_corp_id,
                    st.solar_system_id as i64,
                    st.name,
                    st.state,
                    i64::from(st.quantum_core_installed),
                    st.fuel_blocks as i64,
                    st.shield_hp,
                    st.armor_hp,
                    st.hull_hp,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.structures.insert(stid, h);
        }
    }

    // 28. system_states
    let rem_sys: Vec<SolarSystemId> = fp
        .system_states
        .keys()
        .filter(|k| !snap.system_states.contains_key(*k))
        .copied()
        .collect();
    for sys_id in rem_sys {
        cold_changed = true;
        tx.execute(
            "DELETE FROM system_states WHERE solar_system_id = ?1",
            params![sys_id as i64],
        )
        .map_err(|e| e.to_string())?;
        fp.system_states.remove(&sys_id);
    }
    for (&sys_id, st) in &snap.system_states {
        let h = fp_system_state(st);
        if fp.system_states.get(&sys_id) != Some(&h) {
            let js = serde_json::to_string(st).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO system_states (solar_system_id, dbs_multiplier, ess_main_bank_cents, ess_reserve_bank_cents, sov_alliance_id, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    sys_id as i64,
                    st.dbs_multiplier,
                    st.ess_main_bank.0,
                    st.ess_reserve_bank.0,
                    st.sov_alliance_id,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.system_states.insert(sys_id, h);
            delta.upserted_system_states.push(st.clone());
        }
    }

    // 29. idempotency_keys
    let rem_idem: Vec<(i64, String)> = fp
        .committed_idempotency_keys
        .iter()
        .filter(|k| !snap.committed_idempotency_keys.contains(*k))
        .cloned()
        .collect();
    for (cid, key) in rem_idem {
        cold_changed = true;
        tx.execute(
            "DELETE FROM idempotency_keys WHERE character_id = ?1 AND idem_key = ?2",
            params![cid, key],
        )
        .map_err(|e| e.to_string())?;
        fp.committed_idempotency_keys.remove(&(cid, key));
    }
    for k in &snap.committed_idempotency_keys {
        if !fp.committed_idempotency_keys.contains(k) {
            tx.execute(
                "INSERT OR REPLACE INTO idempotency_keys (character_id, idem_key) VALUES (?1, ?2)",
                params![k.0, k.1],
            )
            .map_err(|e| e.to_string())?;
            fp.committed_idempotency_keys.insert(k.clone());
            delta.added_idempotency_keys.push(k.clone());
        }
    }

    // 30. licensed_skins
    let rem_lskins: Vec<i64> = fp
        .licensed_skins
        .keys()
        .filter(|k| !snap.licensed_skins.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_lskins {
        cold_changed = true;
        tx.execute("DELETE FROM licensed_skins WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.licensed_skins.remove(&cid);
    }
    for (&cid, skins) in &snap.licensed_skins {
        let h = fp_json(skins);
        if fp.licensed_skins.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(skins).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO licensed_skins (character_id, skins_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map_err(|e| e.to_string())?;
            fp.licensed_skins.insert(cid, h);
        }
    }

    // 31. applied_ship_skins
    let rem_askins: Vec<i64> = fp
        .applied_ship_skins
        .keys()
        .filter(|k| !snap.applied_ship_skins.contains_key(*k))
        .copied()
        .collect();
    for sid in rem_askins {
        cold_changed = true;
        tx.execute("DELETE FROM applied_ship_skins WHERE ship_id = ?1", params![sid])
            .map_err(|e| e.to_string())?;
        fp.applied_ship_skins.remove(&sid);
    }
    for (&sid, &skin_id) in &snap.applied_ship_skins {
        if fp.applied_ship_skins.get(&sid) != Some(&skin_id) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO applied_ship_skins (ship_id, skin_id) VALUES (?1, ?2)",
                params![sid, skin_id as i64],
            )
            .map_err(|e| e.to_string())?;
            fp.applied_ship_skins.insert(sid, skin_id);
        }
    }

    if need_wal_delta && cold_changed {
        delta.full_snapshot = Some(snap.clone());
    }
    Ok(delta)
}

/// Loads the complete `WorldStateSnapshot` and its `TableFingerprints` from the SQLite database.
pub fn load_snapshot_from_sqlite(
    conn: &Connection,
) -> Result<(WorldStateSnapshot, TableFingerprints), String> {
    let mut snap = WorldStateSnapshot {
        next_entity_id: 200_000_000,
        ..Default::default()
    };

    {
        let mut stmt = conn
            .prepare("SELECT meta_key, meta_value FROM world_meta")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (k, v) = r.map_err(|e| e.to_string())?;
            match k.as_str() {
                "next_entity_id" => snap.next_entity_id = v,
                "wal_seq" => snap.wal_seq = v as u64,
                "markets_seeded" => snap.markets_seeded = v != 0,
                _ => {}
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT username, user_id, password_token, role, banned_until_unix, ban_reason FROM accounts")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(AccountRecord {
                    username: r.get(0)?,
                    user_id: r.get::<_, i64>(1)? as u64,
                    password_token: r.get(2)?,
                    role: r.get::<_, i64>(3)? as u64,
                    banned_until_unix: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    ban_reason: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let a = r.map_err(|e| e.to_string())?;
            snap.accounts.insert(a.username.clone(), a);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM characters")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let ch: CharacterRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.characters.insert(ch.character_id, ch);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, type_id, level, skill_points FROM skills")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    TrainedSkillRecord {
                        type_id: r.get::<_, i64>(1)? as TypeId,
                        level: r.get::<_, i64>(2)? as u8,
                        skill_points: r.get(3)?,
                    },
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, sk) = r.map_err(|e| e.to_string())?;
            snap.skills.entry(cid).or_default().insert(sk.type_id, sk);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, type_id, target_level, start_sp, end_sp, duration_seconds FROM skill_queues ORDER BY character_id ASC, queue_index ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    SkillQueueEntry {
                        type_id: r.get::<_, i64>(1)? as TypeId,
                        target_level: r.get::<_, i64>(2)? as u8,
                        start_sp: r.get(3)?,
                        end_sp: r.get(4)?,
                        duration_seconds: r.get(5)?,
                    },
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, q) = r.map_err(|e| e.to_string())?;
            snap.skill_queues.entry(cid).or_default().push(q);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, clones_json FROM jump_clones")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, js) = r.map_err(|e| e.to_string())?;
            let clones: Vec<JumpCloneRecord> =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.jump_clones.insert(cid, clones);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, implants_json FROM active_implants")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, js) = r.map_err(|e| e.to_string())?;
            let imps: Vec<TypeId> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.active_implants.insert(cid, imps);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM items")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let item: InventoryItemRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.items.insert(item.item_id, item);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM saved_fittings")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let fit: SavedFittingRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.saved_fittings.insert(fit.fitting_id, fit);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents FROM market_orders")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(MarketOrderRecord {
                    order_id: r.get(0)?,
                    owner_id: r.get(1)?,
                    station_id: r.get(2)?,
                    region_id: r.get::<_, i64>(3)? as u32,
                    type_id: r.get::<_, i64>(4)? as TypeId,
                    is_bid: r.get::<_, i64>(5)? != 0,
                    price: IskCents(r.get(6)?),
                    vol_entered: r.get::<_, i64>(7)? as i32,
                    vol_remaining: r.get::<_, i64>(8)? as i32,
                    min_volume: r.get::<_, i64>(9)? as i32,
                    is_npc: r.get::<_, i64>(10)? != 0,
                    escrow: IskCents(r.get(11)?),
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let ord = r.map_err(|e| e.to_string())?;
            snap.market_orders.insert(ord.order_id, ord);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason FROM wallet_journal ORDER BY seq_idx ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WalletJournalEntry {
                    entry_id: r.get(0)?,
                    character_id: r.get(1)?,
                    entry_type: r.get(2)?,
                    amount: IskCents(r.get(3)?),
                    balance_after: IskCents(r.get(4)?),
                    reason: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            snap.wallet_journal.push(r.map_err(|e| e.to_string())?);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM contracts")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let c: ContractRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.contracts.insert(c.contract_id, c);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix FROM industry_jobs")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(IndustryJobRecord {
                    job_id: r.get(0)?,
                    installer_id: r.get(1)?,
                    facility_id: r.get(2)?,
                    blueprint_item_id: r.get(3)?,
                    blueprint_type_id: r.get::<_, i64>(4)? as TypeId,
                    activity_id: r.get::<_, i64>(5)? as u8,
                    runs: r.get::<_, i64>(6)? as u32,
                    product_type_id: r.get::<_, i64>(7)? as TypeId,
                    status: r.get(8)?,
                    duration_seconds: r.get::<_, i64>(9)? as u64,
                    start_unix: r.get::<_, i64>(10)? as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let j = r.map_err(|e| e.to_string())?;
            snap.industry_jobs.insert(j.job_id, j);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM planetary_colonies")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let col: PlanetaryColonyRecord =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.planetary_colonies
                .insert((col.owner_id, col.planet_id), col);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM missions")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let m: ActiveMissionRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.active_missions
                .insert((m.character_id, m.mission_id), m);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, tally FROM mission_tally")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u32)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, t) = r.map_err(|e| e.to_string())?;
            snap.mission_tally.insert(cid, t);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, tx_json FROM standings")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, js) = r.map_err(|e| e.to_string())?;
            let list: Vec<StandingTransactionRecord> =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.standing_transactions.insert(cid, list);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM npe_progress")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let npe: NpeProgressRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.npe_progress.insert(npe.character_id, npe);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT bookmark_id, owner_id, label, solar_system_id, x, y, z FROM bookmarks")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(BookmarkRecord {
                    bookmark_id: r.get(0)?,
                    owner_id: r.get(1)?,
                    label: r.get(2)?,
                    solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                    x: r.get(4)?,
                    y: r.get(5)?,
                    z: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let bm = r.map_err(|e| e.to_string())?;
            snap.bookmarks.insert(bm.bookmark_id, bm);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT message_id, sender_id, recipient_id, subject, body FROM mail ORDER BY seq_idx ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(MailRecord {
                    message_id: r.get(0)?,
                    sender_id: r.get(1)?,
                    recipient_id: r.get(2)?,
                    subject: r.get(3)?,
                    body: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            snap.mails.push(r.map_err(|e| e.to_string())?);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM fleets")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let fl: FleetRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.fleets.insert(fl.fleet_id, fl);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM corps")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let corp: CorporationRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.corporations.insert(corp.corporation_id, corp);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM alliances")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let al: AllianceRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.alliances.insert(al.alliance_id, al);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT war_id, aggressor_corp_id, defender_corp_id, mutual, started_unix, active FROM wars")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WarRecord {
                    war_id: r.get(0)?,
                    aggressor_corp_id: r.get(1)?,
                    defender_corp_id: r.get(2)?,
                    mutual: r.get::<_, i64>(3)? != 0,
                    started_unix: r.get::<_, i64>(4)? as u64,
                    active: r.get::<_, i64>(5)? != 0,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let w = r.map_err(|e| e.to_string())?;
            snap.wars.insert(w.war_id, w);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT project_id, corporation_id, name, target_quantity, current_quantity, isk_per_unit_cents, escrow_remaining_cents FROM corp_projects")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CorpProjectRecord {
                    project_id: r.get(0)?,
                    corporation_id: r.get(1)?,
                    name: r.get(2)?,
                    target_quantity: r.get::<_, i64>(3)? as u64,
                    current_quantity: r.get::<_, i64>(4)? as u64,
                    isk_per_unit: IskCents(r.get(5)?),
                    escrow_remaining: IskCents(r.get(6)?),
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let p = r.map_err(|e| e.to_string())?;
            snap.corp_projects.insert(p.project_id, p);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT kill_id, victim_character_id, attacker_character_id, solar_system_id, ship_type_id, total_value_isk FROM killmails")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KillmailRecord {
                    kill_id: r.get(0)?,
                    victim_character_id: r.get(1)?,
                    attacker_character_id: r.get(2)?,
                    solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                    ship_type_id: r.get::<_, i64>(4)? as TypeId,
                    total_value_isk: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let km = r.map_err(|e| e.to_string())?;
            snap.killmails.insert(km.kill_id, km);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT kill_right_id, owner_id, target_id, activation_fee_cents, is_active_suspect FROM kill_rights")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KillRightRecord {
                    kill_right_id: r.get(0)?,
                    owner_id: r.get(1)?,
                    target_id: r.get(2)?,
                    activation_fee: IskCents(r.get(3)?),
                    is_active_suspect: r.get::<_, i64>(4)? != 0,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let kr = r.map_err(|e| e.to_string())?;
            snap.kill_rights.insert(kr.kill_right_id, kr);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM structures")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let st: UpwellStructureRecord =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.structures.insert(st.structure_id, st);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM system_states")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let st: SystemDynamicState = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.system_states.insert(st.solar_system_id, st);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, idem_key FROM idempotency_keys")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            snap.committed_idempotency_keys
                .insert(r.map_err(|e| e.to_string())?);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, skins_json FROM licensed_skins")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, js) = r.map_err(|e| e.to_string())?;
            let skins: Vec<u32> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.licensed_skins.insert(cid, skins);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT ship_id, skin_id FROM applied_ship_skins")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u32)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (sid, skin_id) = r.map_err(|e| e.to_string())?;
            snap.applied_ship_skins.insert(sid, skin_id);
        }
    }

    let fp = compute_fingerprints(&snap);
    Ok((snap, fp))
}

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
        fp.skill_queues.insert(cid, fp_json(q));
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
    for (&id, ord) in &snap.market_orders {
        fp.market_orders.insert(id, fp_market_order(ord));
    }
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
    fp
}

pub fn replace_sqlite_from_snapshot(
    conn: &mut Connection,
    snap: &WorldStateSnapshot,
) -> Result<TableFingerprints, String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute_batch(
        "DELETE FROM world_meta;
         DELETE FROM accounts;
         DELETE FROM characters;
         DELETE FROM skills;
         DELETE FROM skill_queues;
         DELETE FROM jump_clones;
         DELETE FROM active_implants;
         DELETE FROM items;
         DELETE FROM saved_fittings;
         DELETE FROM market_orders;
         DELETE FROM wallet_journal;
         DELETE FROM contracts;
         DELETE FROM industry_jobs;
         DELETE FROM planetary_colonies;
         DELETE FROM missions;
         DELETE FROM mission_tally;
         DELETE FROM standings;
         DELETE FROM npe_progress;
         DELETE FROM bookmarks;
         DELETE FROM mail;
         DELETE FROM fleets;
         DELETE FROM corps;
         DELETE FROM alliances;
         DELETE FROM wars;
         DELETE FROM corp_projects;
         DELETE FROM killmails;
         DELETE FROM kill_rights;
         DELETE FROM structures;
         DELETE FROM system_states;
         DELETE FROM idempotency_keys;
         DELETE FROM licensed_skins;
         DELETE FROM applied_ship_skins;",
    )
    .map_err(|e| e.to_string())?;
    let mut fp = TableFingerprints::default();
    let _ = sync_dirty_to_sqlite(&tx, snap, &mut fp, false)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(fp)
}

/// Selectively rolls back only rows in `snap` whose fingerprint differs from the last committed
/// `fp: &TableFingerprints`, avoiding any SQLite query or full-world deserialization when a
/// closure returns `Err` before mutating state or when only a few rows were touched (`R34-P1-8`).
pub fn rollback_dirty_from_sqlite(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    fp: &TableFingerprints,
) -> Result<(), String> {
    snap.next_entity_id = fp.next_entity_id;
    snap.wal_seq = fp.wal_seq;
    snap.markets_seeded = fp.markets_seeded;

    // 1. accounts
    snap.accounts.retain(|k, _| fp.accounts.contains_key(k));
    for (k, &expected_h) in &fp.accounts {
        if snap.accounts.get(k).map(fp_account) != Some(expected_h) {
            let row = conn
                .query_row(
                    "SELECT username, user_id, password_token, role, banned_until_unix, ban_reason FROM accounts WHERE username = ?1",
                    params![k],
                    |r| {
                        Ok(AccountRecord {
                            username: r.get(0)?,
                            user_id: r.get::<_, i64>(1)? as u64,
                            password_token: r.get(2)?,
                            role: r.get::<_, i64>(3)? as u64,
                            banned_until_unix: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                            ban_reason: r.get(5)?,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.accounts.insert(k.clone(), row);
        }
    }

    // 2. characters
    snap.characters.retain(|k, _| fp.characters.contains_key(k));
    for (&k, &expected_h) in &fp.characters {
        if snap.characters.get(&k).map(fp_character) != Some(expected_h) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM characters WHERE character_id = ?1",
                    params![k],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let ch: CharacterRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.characters.insert(k, ch);
        }
    }

    // 3. items
    snap.items.retain(|k, _| fp.items.contains_key(k));
    for (&k, &expected_h) in &fp.items {
        if snap.items.get(&k).map(fp_item) != Some(expected_h) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM items WHERE item_id = ?1",
                    params![k],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let item: InventoryItemRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.items.insert(k, item);
        }
    }

    // 4. market_orders
    snap.market_orders
        .retain(|k, _| fp.market_orders.contains_key(k));
    for (&k, &expected_h) in &fp.market_orders {
        if snap.market_orders.get(&k).map(fp_market_order) != Some(expected_h) {
            let ord = conn
                .query_row(
                    "SELECT order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents FROM market_orders WHERE order_id = ?1",
                    params![k],
                    |r| {
                        Ok(MarketOrderRecord {
                            order_id: r.get(0)?,
                            owner_id: r.get(1)?,
                            station_id: r.get(2)?,
                            region_id: r.get::<_, i64>(3)? as u32,
                            type_id: r.get::<_, i64>(4)? as TypeId,
                            is_bid: r.get::<_, i64>(5)? != 0,
                            price: IskCents(r.get(6)?),
                            vol_entered: r.get::<_, i64>(7)? as i32,
                            vol_remaining: r.get::<_, i64>(8)? as i32,
                            min_volume: r.get::<_, i64>(9)? as i32,
                            is_npc: r.get::<_, i64>(10)? != 0,
                            escrow: IskCents(r.get(11)?),
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.market_orders.insert(k, ord);
        }
    }

    // 5. In-memory direct restores for exact-value tracked sets/maps and append-only logs
    if snap.wallet_journal.len() > fp.wallet_journal_len {
        snap.wallet_journal.truncate(fp.wallet_journal_len);
    } else if snap.wallet_journal.len() < fp.wallet_journal_len {
        snap.wallet_journal.clear();
        let mut stmt = conn
            .prepare("SELECT entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason FROM wallet_journal ORDER BY seq_idx ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WalletJournalEntry {
                    entry_id: r.get(0)?,
                    character_id: r.get(1)?,
                    entry_type: r.get(2)?,
                    amount: IskCents(r.get(3)?),
                    balance_after: IskCents(r.get(4)?),
                    reason: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            snap.wallet_journal.push(r.map_err(|e| e.to_string())?);
        }
    }

    if snap.mails.len() > fp.mails_len {
        snap.mails.truncate(fp.mails_len);
    } else if snap.mails.len() < fp.mails_len {
        snap.mails.clear();
        let mut stmt = conn
            .prepare("SELECT message_id, sender_id, recipient_id, subject, body FROM mail ORDER BY seq_idx ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(MailRecord {
                    message_id: r.get(0)?,
                    sender_id: r.get(1)?,
                    recipient_id: r.get(2)?,
                    subject: r.get(3)?,
                    body: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            snap.mails.push(r.map_err(|e| e.to_string())?);
        }
    }

    if snap.committed_idempotency_keys != fp.committed_idempotency_keys {
        snap.committed_idempotency_keys = fp.committed_idempotency_keys.clone();
    }
    if snap.mission_tally != fp.mission_tally {
        snap.mission_tally = fp.mission_tally.clone();
    }
    if snap.applied_ship_skins != fp.applied_ship_skins {
        snap.applied_ship_skins = fp.applied_ship_skins.clone();
    }

    // 6. skills
    snap.skills.retain(|cid, _| fp.skills.contains_key(cid));
    for (&cid, expected_map) in &fp.skills {
        let sk_dirty = match snap.skills.get(&cid) {
            None => true,
            Some(cur_map) => {
                cur_map.len() != expected_map.len()
                    || expected_map.iter().any(|(&tid, &eh)| {
                        cur_map.get(&tid).map(|sk| {
                            let mut h = FnvHasher::new();
                            h.write_u32(sk.type_id);
                            h.write_u8(sk.level);
                            h.write_f64(sk.skill_points);
                            h.finish()
                        }) != Some(eh)
                    })
            }
        };
        if sk_dirty {
            let mut restored = HashMap::new();
            let mut stmt = conn
                .prepare("SELECT type_id, level, skill_points FROM skills WHERE character_id = ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![cid], |r| {
                    Ok(TrainedSkillRecord {
                        type_id: r.get::<_, i64>(0)? as TypeId,
                        level: r.get::<_, i64>(1)? as u8,
                        skill_points: r.get(2)?,
                    })
                })
                .map_err(|e| e.to_string())?;
            for r in rows {
                let sk = r.map_err(|e| e.to_string())?;
                restored.insert(sk.type_id, sk);
            }
            snap.skills.insert(cid, restored);
        }
    }

    // 7. skill_queues
    snap.skill_queues
        .retain(|cid, _| fp.skill_queues.contains_key(cid));
    for (&cid, &eh) in &fp.skill_queues {
        if snap.skill_queues.get(&cid).map(fp_json) != Some(eh) {
            let mut q = Vec::new();
            let mut stmt = conn
                .prepare("SELECT type_id, target_level, start_sp, end_sp, duration_seconds FROM skill_queues WHERE character_id = ?1 ORDER BY queue_index ASC")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![cid], |r| {
                    Ok(SkillQueueEntry {
                        type_id: r.get::<_, i64>(0)? as TypeId,
                        target_level: r.get::<_, i64>(1)? as u8,
                        start_sp: r.get(2)?,
                        end_sp: r.get(3)?,
                        duration_seconds: r.get(4)?,
                    })
                })
                .map_err(|e| e.to_string())?;
            for r in rows {
                q.push(r.map_err(|e| e.to_string())?);
            }
            snap.skill_queues.insert(cid, q);
        }
    }

    // 8. jump_clones
    snap.jump_clones
        .retain(|cid, _| fp.jump_clones.contains_key(cid));
    for (&cid, &eh) in &fp.jump_clones {
        if snap.jump_clones.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT clones_json FROM jump_clones WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.jump_clones
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 9. active_implants
    snap.active_implants
        .retain(|cid, _| fp.active_implants.contains_key(cid));
    for (&cid, &eh) in &fp.active_implants {
        if snap.active_implants.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT implants_json FROM active_implants WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.active_implants
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 10. saved_fittings
    snap.saved_fittings
        .retain(|id, _| fp.saved_fittings.contains_key(id));
    for (&id, &eh) in &fp.saved_fittings {
        if snap.saved_fittings.get(&id).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM saved_fittings WHERE fitting_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.saved_fittings
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 11. contracts
    snap.contracts.retain(|id, _| fp.contracts.contains_key(id));
    for (&id, &eh) in &fp.contracts {
        if snap.contracts.get(&id).map(fp_contract) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM contracts WHERE contract_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.contracts
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 12. industry_jobs
    snap.industry_jobs
        .retain(|id, _| fp.industry_jobs.contains_key(id));
    for (&id, &eh) in &fp.industry_jobs {
        if snap.industry_jobs.get(&id).map(fp_json) != Some(eh) {
            let j = conn
                .query_row(
                    "SELECT job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix FROM industry_jobs WHERE job_id = ?1",
                    params![id],
                    |r| {
                        Ok(IndustryJobRecord {
                            job_id: r.get(0)?,
                            installer_id: r.get(1)?,
                            facility_id: r.get(2)?,
                            blueprint_item_id: r.get(3)?,
                            blueprint_type_id: r.get::<_, i64>(4)? as TypeId,
                            activity_id: r.get::<_, i64>(5)? as u8,
                            runs: r.get::<_, i64>(6)? as u32,
                            product_type_id: r.get::<_, i64>(7)? as TypeId,
                            status: r.get(8)?,
                            duration_seconds: r.get::<_, i64>(9)? as u64,
                            start_unix: r.get::<_, i64>(10)? as u64,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.industry_jobs.insert(id, j);
        }
    }

    // 13. planetary_colonies
    snap.planetary_colonies
        .retain(|k, _| fp.planetary_colonies.contains_key(k));
    for (&k, &eh) in &fp.planetary_colonies {
        if snap.planetary_colonies.get(&k).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM planetary_colonies WHERE owner_id = ?1 AND planet_id = ?2",
                    params![k.0, k.1],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.planetary_colonies
                .insert(k, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 14. active_missions
    snap.active_missions
        .retain(|k, _| fp.active_missions.contains_key(k));
    for (&k, &eh) in &fp.active_missions {
        if snap.active_missions.get(&k).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM missions WHERE character_id = ?1 AND mission_id = ?2",
                    params![k.0, k.1 as i64],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.active_missions
                .insert(k, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 15. standing_transactions
    snap.standing_transactions
        .retain(|cid, _| fp.standing_transactions.contains_key(cid));
    for (&cid, &eh) in &fp.standing_transactions {
        if snap.standing_transactions.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT tx_json FROM standings WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.standing_transactions
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 16. npe_progress
    snap.npe_progress
        .retain(|cid, _| fp.npe_progress.contains_key(cid));
    for (&cid, &eh) in &fp.npe_progress {
        if snap.npe_progress.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM npe_progress WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.npe_progress
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 17. bookmarks
    snap.bookmarks.retain(|id, _| fp.bookmarks.contains_key(id));
    for (&id, &eh) in &fp.bookmarks {
        if snap.bookmarks.get(&id).map(fp_json) != Some(eh) {
            let bm = conn
                .query_row(
                    "SELECT bookmark_id, owner_id, label, solar_system_id, x, y, z FROM bookmarks WHERE bookmark_id = ?1",
                    params![id],
                    |r| {
                        Ok(BookmarkRecord {
                            bookmark_id: r.get(0)?,
                            owner_id: r.get(1)?,
                            label: r.get(2)?,
                            solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                            x: r.get(4)?,
                            y: r.get(5)?,
                            z: r.get(6)?,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.bookmarks.insert(id, bm);
        }
    }

    // 18. fleets
    snap.fleets.retain(|id, _| fp.fleets.contains_key(id));
    for (&id, &eh) in &fp.fleets {
        if snap.fleets.get(&id).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM fleets WHERE fleet_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.fleets
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 19. corporations
    snap.corporations
        .retain(|id, _| fp.corporations.contains_key(id));
    for (&id, &eh) in &fp.corporations {
        if snap.corporations.get(&id).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM corps WHERE corporation_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.corporations
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 20. alliances
    snap.alliances.retain(|id, _| fp.alliances.contains_key(id));
    for (&id, &eh) in &fp.alliances {
        if snap.alliances.get(&id).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM alliances WHERE alliance_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.alliances
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 21. wars
    snap.wars.retain(|id, _| fp.wars.contains_key(id));
    for (&id, &eh) in &fp.wars {
        if snap.wars.get(&id).map(fp_json) != Some(eh) {
            let w = conn
                .query_row(
                    "SELECT war_id, aggressor_corp_id, defender_corp_id, mutual, started_unix, active FROM wars WHERE war_id = ?1",
                    params![id],
                    |r| {
                        Ok(WarRecord {
                            war_id: r.get(0)?,
                            aggressor_corp_id: r.get(1)?,
                            defender_corp_id: r.get(2)?,
                            mutual: r.get::<_, i64>(3)? != 0,
                            started_unix: r.get::<_, i64>(4)? as u64,
                            active: r.get::<_, i64>(5)? != 0,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.wars.insert(id, w);
        }
    }

    // 22. corp_projects
    snap.corp_projects
        .retain(|id, _| fp.corp_projects.contains_key(id));
    for (&id, &eh) in &fp.corp_projects {
        if snap.corp_projects.get(&id).map(fp_json) != Some(eh) {
            let p = conn
                .query_row(
                    "SELECT project_id, corporation_id, name, target_quantity, current_quantity, isk_per_unit_cents, escrow_remaining_cents FROM corp_projects WHERE project_id = ?1",
                    params![id],
                    |r| {
                        Ok(CorpProjectRecord {
                            project_id: r.get(0)?,
                            corporation_id: r.get(1)?,
                            name: r.get(2)?,
                            target_quantity: r.get::<_, i64>(3)? as u64,
                            current_quantity: r.get::<_, i64>(4)? as u64,
                            isk_per_unit: IskCents(r.get(5)?),
                            escrow_remaining: IskCents(r.get(6)?),
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.corp_projects.insert(id, p);
        }
    }

    // 23. killmails
    snap.killmails.retain(|id, _| fp.killmails.contains_key(id));
    for (&id, &eh) in &fp.killmails {
        if snap.killmails.get(&id).map(fp_json) != Some(eh) {
            let km = conn
                .query_row(
                    "SELECT kill_id, victim_character_id, attacker_character_id, solar_system_id, ship_type_id, total_value_isk FROM killmails WHERE kill_id = ?1",
                    params![id],
                    |r| {
                        Ok(KillmailRecord {
                            kill_id: r.get(0)?,
                            victim_character_id: r.get(1)?,
                            attacker_character_id: r.get(2)?,
                            solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                            ship_type_id: r.get::<_, i64>(4)? as TypeId,
                            total_value_isk: r.get(5)?,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.killmails.insert(id, km);
        }
    }

    // 24. kill_rights
    snap.kill_rights
        .retain(|id, _| fp.kill_rights.contains_key(id));
    for (&id, &eh) in &fp.kill_rights {
        if snap.kill_rights.get(&id).map(fp_json) != Some(eh) {
            let kr = conn
                .query_row(
                    "SELECT kill_right_id, owner_id, target_id, activation_fee_cents, is_active_suspect FROM kill_rights WHERE kill_right_id = ?1",
                    params![id],
                    |r| {
                        Ok(KillRightRecord {
                            kill_right_id: r.get(0)?,
                            owner_id: r.get(1)?,
                            target_id: r.get(2)?,
                            activation_fee: IskCents(r.get(3)?),
                            is_active_suspect: r.get::<_, i64>(4)? != 0,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.kill_rights.insert(id, kr);
        }
    }

    // 25. structures
    snap.structures
        .retain(|id, _| fp.structures.contains_key(id));
    for (&id, &eh) in &fp.structures {
        if snap.structures.get(&id).map(fp_structure) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM structures WHERE structure_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.structures
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 26. system_states
    snap.system_states
        .retain(|id, _| fp.system_states.contains_key(id));
    for (&id, &eh) in &fp.system_states {
        if snap.system_states.get(&id).map(fp_system_state) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM system_states WHERE solar_system_id = ?1",
                    params![id as i64],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.system_states
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 27. licensed_skins
    snap.licensed_skins
        .retain(|cid, _| fp.licensed_skins.contains_key(cid));
    for (&cid, &eh) in &fp.licensed_skins {
        if snap.licensed_skins.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT skins_json FROM licensed_skins WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.licensed_skins
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    Ok(())
}

