//! SQLite relational schema migrations and JWT secret storage.

use rusqlite::{params, Connection};

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
    started_at_unix INTEGER,
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

CREATE TABLE IF NOT EXISTS char_mining_ledger (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    character_id INTEGER NOT NULL,
    solar_system_id INTEGER NOT NULL,
    type_id INTEGER NOT NULL,
    quantity INTEGER NOT NULL,
    date TEXT NOT NULL,
    timestamp_unix INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS asteroids (
    asteroid_id INTEGER PRIMARY KEY,
    solar_system_id INTEGER NOT NULL,
    belt_id INTEGER NOT NULL,
    type_id INTEGER NOT NULL,
    quantity INTEGER NOT NULL,
    initial_quantity INTEGER NOT NULL,
    x REAL NOT NULL,
    y REAL NOT NULL,
    z REAL NOT NULL,
    radius REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS character_last_jump_unix (
    character_id INTEGER PRIMARY KEY,
    last_jump_unix INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS character_cspa_charges (
    character_id INTEGER PRIMARY KEY,
    cspa_charge_cents INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS mail_timestamps (
    mail_id INTEGER PRIMARY KEY,
    timestamp_unix INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS character_login_stats (
    character_id INTEGER PRIMARY KEY,
    last_login_unix INTEGER NOT NULL,
    last_logout_unix INTEGER NOT NULL,
    logins_count INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS item_custom_names (
    item_id INTEGER PRIMARY KEY,
    custom_name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS item_positions (
    item_id INTEGER PRIMARY KEY,
    x REAL NOT NULL,
    y REAL NOT NULL,
    z REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS structure_vulnerability_windows (
    structure_id INTEGER PRIMARY KEY,
    window_start_unix INTEGER NOT NULL,
    window_end_unix INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS skill_history (
    character_id INTEGER PRIMARY KEY,
    history_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS character_doll_info (
    character_id INTEGER PRIMARY KEY,
    doll_dna TEXT NOT NULL,
    portrait_meta TEXT NOT NULL
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
    let now_unix = crate::clock::host_wall_unix_secs_and_nanos().0 as i64;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_unix) VALUES (?1, ?2, ?3)",
        params![
            CURRENT_SCHEMA_VERSION,
            "0001_initial_world_schema",
            now_unix
        ],
    )
    .map_err(|e| e.to_string())?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_id', 200000000), ('wal_seq', 0), ('markets_seeded', 0);",
    )
    .map_err(|e| e.to_string())?;
    let _ = conn.execute("ALTER TABLE skill_queues ADD COLUMN started_at_unix INTEGER", []);
    // Kill time and kill-time affiliations (war killmail attribution); legacy rows default to 0/NULL.
    for col in [
        "killed_unix INTEGER NOT NULL DEFAULT 0",
        "victim_corp_id INTEGER NOT NULL DEFAULT 0",
        "victim_alliance_id INTEGER",
        "attacker_corp_id INTEGER NOT NULL DEFAULT 0",
        "attacker_alliance_id INTEGER",
    ] {
        let _ = conn.execute(&format!("ALTER TABLE killmails ADD COLUMN {col}"), []);
    }
    super::ledger_store::apply_ledger_migration(conn)?;
    super::d6_store::apply_d6_migration(conn)?;
    super::d6_social_store::apply_d6_social_migration(conn)?;
    super::d6_corp_crimewatch_store::apply_d6_corp_crimewatch_migration(conn)?;
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
