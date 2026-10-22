//! Action Plan Acceptance Test Suite: Persistence, Dogma, Economy & Mining
//!
//! Validates:
//! - D-1 / D-2: SDE runtime loading, celestials, Jita-Perimeter gate pairs, schools, indexed lookups
//! - D-3 / D-4 / D-5: Durable SQLite WorldRepository, Argon2id accounts & online locks, evomu.toml config
//! - DG-1: Dogma modifier engine, modifierInfo functions, stacking penalties, Rifter with overdrives
//! - E-1 / E-2: Market fees (Broker Relations / Accounting), order lifecycle, escrow management
//! - E-3: Contracts (ItemExchange and Courier delivery & collateral)
//! - C-5: Asteroid belts spawning, Miner I laser cycle, depletion
//! - C-6: Reprocessing yield, station tax, material outputs, non-reprocessable rejection



use crate::common;
use eve_db::{
    AsteroidRecord, BookmarkRecord, CorporationRecord, DbError, FleetRecord,
    IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents, MailRecord, MarketOrderRecord,
    SkillQueueEntry, StandingTransactionRecord, TrainedSkillRecord, TypeId, UpwellStructureRecord,
    WalletJournalEntry, WorldRepository,
};
use eve_dogma::{
    stacking_penalty_factor, DogmaEffectContext, DogmaItemState, EffectModifierSpec,
    ShipFittingEngine,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, SdeError, SdeRegistry};
use eve_server::config::ServerConfig;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const PERIMETER_SOLAR_SYSTEM_ID: u32 = 30_000_144;
const TRITANIUM_TYPE_ID: TypeId = 34;
const VELDSPAR_TYPE_ID: TypeId = 1230;
const SCORDITE_TYPE_ID: TypeId = 1228;
const RIFTER_TYPE_ID: TypeId = 587;
const MINER_I_TYPE_ID: TypeId = 483;
const OVERDRIVE_II_TYPE_ID: TypeId = 1236;
const BROKER_RELATIONS_SKILL: TypeId = 3446;
const MINING_SKILL: TypeId = 3386;
const REPROCESSING_SKILL: TypeId = 3385;
const REPROC_EFFICIENCY_SKILL: TypeId = 3389;

async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    sess
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

fn get_dict_val<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| match k { PyRep::String(s) => s == key, _ => false }).map(|(_, v)| v),
        _ => None,
    }
}

// ====================================================================
// Test 1: D-1 & D-2: SDE Loader, Celestials, Jita-Perimeter Gate Pairs
// ====================================================================
#[tokio::test]
async fn test_plan_d1_d2_sde_loader_celestials_and_gates() {
    // 1. SdeRegistry::open fails clearly if database path is missing
    let missing_path = Path::new("/tmp/nonexistent_evomu_sde_db_never_exists.sqlite");
    let open_missing = SdeRegistry::open(missing_path);
    assert!(matches!(open_missing, Err(SdeError::MissingDatabase(_))), "Opening missing SDE database must return SdeError::MissingDatabase");

    // 2. Load runtime SDE from canonical path
    let sde_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&sde_path).expect("Real SDE SQLite database must open");

    // 3. Verify >= 5,000 solar systems in the registry (action_plan.md D-2 criterion)
    assert!(sde.solar_systems.len() >= 5_000, "SDE solar_systems must contain at least 5,000 systems (got {})", sde.solar_systems.len());

    // 4. Verify Jita <-> Perimeter stargate pair with positions and destinations
    let jita_to_perim_gate = sde.jumps.get(&50_001_249).expect("Jita -> Perimeter stargate 50001249 must exist");
    assert_eq!(jita_to_perim_gate.from_solar_system_id, JITA_SOLAR_SYSTEM_ID);
    assert_eq!(jita_to_perim_gate.to_solar_system_id, PERIMETER_SOLAR_SYSTEM_ID);
    assert_eq!(jita_to_perim_gate.destination_stargate_id, 50_002_185, "Destination of 50001249 must be 50002185");
    assert!(jita_to_perim_gate.position.length() > 1.0, "Stargate position must be non-trivial 3D coordinates");

    let perim_to_jita_gate = sde.jumps.get(&50_002_185).expect("Perimeter -> Jita stargate 50002185 must exist");
    assert_eq!(perim_to_jita_gate.from_solar_system_id, PERIMETER_SOLAR_SYSTEM_ID);
    assert_eq!(perim_to_jita_gate.to_solar_system_id, JITA_SOLAR_SYSTEM_ID);
    assert_eq!(perim_to_jita_gate.destination_stargate_id, 50_001_249, "Destination of 50002185 must be 50001249");
    assert!(perim_to_jita_gate.position.length() > 1.0, "Stargate position must be non-trivial 3D coordinates");

    // 5. Verify school -> race/corp mapping and all 12 starter schools (11..=22)
    assert!(sde.schools.len() >= 12, "SDE schools table must have all starter schools");
    assert_eq!(sde.schools[&11].race_id, 4, "School 11 must map to Caldari (4)");
    assert_eq!(sde.schools[&11].corporation_id, 1000166);
    assert_eq!(sde.schools[&14].race_id, 2, "School 14 must map to Gallente (2)");
    assert_eq!(sde.schools[&14].corporation_id, 1000170);
    assert_eq!(sde.schools[&17].race_id, 1, "School 17 must map to Minmatar (1)");
    assert_eq!(sde.schools[&17].corporation_id, 1000167);
    assert_eq!(sde.schools[&20].race_id, 8, "School 20 must map to Amarr (8)");
    assert_eq!(sde.schools[&20].corporation_id, 1000168);

    for school_id in 11..=22 {
        let school = sde.schools.get(&school_id).expect("Starter school 11..=22 must exist");
        assert!((30_000_001..30_005_500).contains(&school.solar_system_id), "School {school_id} must reside in a valid highsec solar system");
    }

    // 6. Verify indexed lookups in SdeRegistry (D-1 real SDE database contents)
    assert!(sde.types.contains_key(&TRITANIUM_TYPE_ID), "Tritanium (34) must exist in SDE types");
    assert!(sde.types.contains_key(&RIFTER_TYPE_ID), "Rifter (587) must exist in SDE types");
    assert!(sde.types.contains_key(&MINER_I_TYPE_ID), "Miner I (483) must exist in SDE types");
    assert!(sde.types.contains_key(&OVERDRIVE_II_TYPE_ID), "Overdrive II (1236) must exist in SDE types");

    assert!(sde.groups.contains_key(&55), "Projectile Weapon group 55 must exist");
    assert!(sde.groups.contains_key(&53), "Propulsion Module group 53 must exist");
    assert!(sde.categories.contains_key(&6), "Ship category 6 must exist");
    assert!(sde.categories.contains_key(&7), "Module category 7 must exist");

    assert!(sde.attributes.contains_key(&attr_ids::MAX_VELOCITY), "maxVelocity attribute 37 must exist");
    assert!(sde.attributes.contains_key(&attr_ids::MASS), "mass attribute 4 must exist");
    assert!(sde.attributes.contains_key(&attr_ids::CAPACITY), "capacity attribute 38 must exist");
    assert!(sde.attributes.contains_key(&161), "volume attribute 161 must exist");

    assert!(sde.effects.contains_key(&5934), "warpScrambleBlockMWD effect 5934 must exist");
    assert!(sde.blueprints.contains_key(&691), "Rifter Blueprint 691 must exist in SDE");
    assert!(sde.stations.contains_key(&JITA_STATION_ID), "Jita 4-4 station 60003760 must exist in SDE");
    assert_eq!(sde.stations[&JITA_STATION_ID].solar_system_id, JITA_SOLAR_SYSTEM_ID);

    assert!(sde.factions.contains_key(&500001), "Caldari State faction 500001 must exist");
    assert!(sde.factions.contains_key(&500002), "Minmatar Republic faction 500002 must exist");
    assert!(sde.npc_corporations.contains_key(&1000003), "Caldari Navy NPC corp 1000003 must exist");

    let veld_mats = sde.type_materials.get(&VELDSPAR_TYPE_ID).expect("Veldspar typeMaterials");
    assert_eq!(veld_mats.len(), 1);
    assert_eq!(veld_mats[0].material_type_id, 34);
    assert_eq!(veld_mats[0].quantity, 400);

    let scor_mats = sde.type_materials.get(&SCORDITE_TYPE_ID).expect("Scordite typeMaterials");
    assert!(scor_mats.iter().any(|m| m.material_type_id == 34));
    assert!(scor_mats.iter().any(|m| m.material_type_id == 35));
}

// ====================================================================
// Test 2: D-3, D-4, D-5: Durable World DB, Argon2 Accounts, evomu.toml
// ====================================================================
#[tokio::test]
async fn test_plan_d3_d4_d5_world_db_accounts_and_config() {
    // Part A (D-5): Config loader and feature flags
    let toml_sample = r#"
bind_address = "127.0.0.1"
macho_port = 26000
bridge_port = 26002
esi_port = 26004
sde_path = "data/evomu_sde.sqlite"
world_db_path = "data/evomu_world.sqlite"
belt_respawn_interval_secs = 7200
downtime_utc_secs_of_day = 39600

[feature_flags]
enable_quasar_gateway = true
enable_esi_http = true
enable_living_world = false
seed_demo = false

[[admin_accounts]]
username = "polaris_root"
password_token = "polaris_secret_token"
character_id = 90000003
character_name = "ISD Polaris"

[[player_accounts]]
username = "new_cadet"
password_token = "cadet_secret_token"
"#;
    let cfg = ServerConfig::from_toml_str(toml_sample).expect("ServerConfig TOML parsing must succeed");
    assert_eq!(cfg.bind_address, "127.0.0.1");
    assert_eq!(cfg.macho_port, 26_000);
    assert_eq!(cfg.bridge_port, 26_002);
    assert_eq!(cfg.esi_port, 26_004);
    assert!(cfg.feature_flags.enable_quasar_gateway);
    assert!(!cfg.feature_flags.enable_living_world);
    assert_eq!(cfg.admin_accounts.len(), 1);
    assert_eq!(cfg.admin_accounts[0].username, "polaris_root");
    assert_eq!(cfg.player_accounts.len(), 1);
    assert_eq!(cfg.player_accounts[0].username, "new_cadet");

    // Part B (D-4): Accounts, Argon2id hashes, bans, online locks
    let sde = SdeRegistry::with_canonical_seed();
    let pid = std::process::id();
    let temp_db = std::env::temp_dir().join(format!("evomu_action_plan_d3_d4_{pid}.sqlite"));
    let _ = std::fs::remove_file(&temp_db);

    let repo = Arc::new(WorldRepository::open_sqlite(&temp_db).expect("open_sqlite must succeed"));
    assert_eq!(repo.sqlite_path(), Some(temp_db.as_path()));

    // Verify WAL journal mode (D-3)
    {
        let conn = rusqlite::Connection::open(&temp_db).unwrap();
        let journal_mode: String = conn.query_row("PRAGMA journal_mode;", [], |r| r.get(0)).unwrap();
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    }

    // Provision player account with salted Argon2id password hash
    let uid = repo.provision_player_account("cadet_alpha", "S0m3Str0ngP@ssphrase!");
    let auth_record = repo.with_snapshot(|snap| snap.accounts.get("cadet_alpha").cloned()).expect("Account must exist");
    assert!(auth_record.password_token.starts_with("$argon2id$"), "Password hash must use Argon2id, got: {}", auth_record.password_token);

    // Verify credentials verification
    assert!(repo.authenticate("cadet_alpha", "S0m3Str0ngP@ssphrase!").is_ok());
    assert!(repo.authenticate("cadet_alpha", "WrongPassphrase!").is_err());

    // Single active session lock (multiLoginBlocker)
    assert!(repo.acquire_online_lock(uid, 501, None, false, 1_700_000_000).unwrap().is_none());
    let conflict = repo.acquire_online_lock(uid, 502, None, false, 1_700_000_005);
    assert!(matches!(conflict, Err(DbError::AlreadyLoggedIn { .. })), "Second session must be rejected with AlreadyLoggedIn lock");
    assert_eq!(repo.check_session_revoked(uid, 501, 1_700_000_006), None);
    let revoked_502 = repo.check_session_revoked(uid, 502, 1_700_000_006);
    assert_eq!(revoked_502.unwrap().0, "MultiLoginBlocked");
    repo.release_online_lock(uid, 501);

    // Account ban enforcement
    let now = repo.now_unix_secs();
    repo.ban_account("cadet_alpha", now + 3600, "Rule violation test").expect("ban_account");
    let banned_login = repo.authenticate("cadet_alpha", "S0m3Str0ngP@ssphrase!");
    assert!(matches!(banned_login, Err(DbError::AccountBanned { .. })), "Banned user must receive AccountBanned error");
    repo.unban_account("cadet_alpha").expect("unban_account");
    assert!(repo.authenticate("cadet_alpha", "S0m3Str0ngP@ssphrase!").is_ok());

    // Part C (D-3): Relational domain tables, concurrent writes & durable restart
    let u2 = repo.provision_player_account("cadet_bravo", "AnotherP@ssphrase!456");
    let c1 = repo.create_character(uid, "Alpha Pilot", 1, 1, 11, 17, &sde).unwrap();
    let c2 = repo.create_character(u2, "Bravo Pilot", 2, 3, 19, 14, &sde).unwrap();
    let c1_id = c1.character_id;
    let c2_id = c2.character_id;

    // Populate relational domain tables
    repo.with_mut_snapshot(|snap| {
        snap.skills.entry(c1_id).or_default().insert(3300, TrainedSkillRecord { type_id: 3300, level: 5, skill_points: 256_000.0 });
        snap.skill_queues.insert(c1_id, vec![SkillQueueEntry { type_id: 3302, target_level: 4, start_sp: 8_000.0, end_sp: 45_255.0, duration_seconds: 1200.0, started_at_unix: None }]);
        snap.market_orders.insert(800_101, MarketOrderRecord { order_id: 800_101, owner_id: c1_id, station_id: JITA_STATION_ID, region_id: 10_000_002, type_id: 34, is_bid: true, price: IskCents::from_isk(5), vol_entered: 5_000, vol_remaining: 5_000, min_volume: 1, is_npc: false, escrow: IskCents::from_isk(25_000) });
        snap.wallet_journal.push(WalletJournalEntry { entry_id: 900_101, character_id: c1_id, entry_type: "market_escrow".to_string(), amount: IskCents::from_isk(-25_000), balance_after: IskCents::from_isk(75_000), reason: "Buy order escrow".to_string() });
        snap.contracts.insert(910_101, eve_db::ContractRecord { contract_id: 910_101, issuer_id: c1_id, acceptor_id: None, contract_type: "ItemExchange".to_string(), status: "Outstanding".to_string(), price: IskCents::from_isk(200_000), reward: IskCents(0), collateral: IskCents(0), start_station_id: JITA_STATION_ID, end_station_id: JITA_STATION_ID, item_ids: vec![], expiration_unix: 2_000_000_000, ..Default::default() });
        snap.industry_jobs.insert(920_101, IndustryJobRecord { job_id: 920_101, installer_id: c1_id, facility_id: JITA_STATION_ID, activity_id: 1, blueprint_item_id: 100_000_007, blueprint_type_id: 691, product_type_id: 587, runs: 1, status: "Active".to_string(), duration_seconds: 3600, start_unix: 1_700_000_000 });
        snap.standing_transactions.insert(c1_id, vec![StandingTransactionRecord { event_id: 1, from_id: 1_000_035, to_id: c1_id, modification: 0.5, event_type_id: 33 }]);
        snap.mails.push(MailRecord { message_id: 930_101, sender_id: c1_id, recipient_id: c2_id, subject: "Action Plan Parity".to_string(), body: "Testing durable world restart".to_string() });
        snap.fleets.insert(940_101, FleetRecord { fleet_id: 940_101, boss_id: c1_id, members: vec![c1_id], wings: vec![], squads: vec![], free_move: true, is_registered: true, ..Default::default() });
        snap.corporations.insert(950_101, CorporationRecord { corporation_id: 950_101, name: "Parity Corps Ltd".to_string(), ticker: "PCL".to_string(), ceo_id: c1_id, alliance_id: None, tax_rate: 0.03, master_wallet: IskCents::from_isk(2_000_000), members: HashSet::from([c1_id]), ..Default::default() });
        snap.bookmarks.insert(960_101, BookmarkRecord { bookmark_id: 960_101, owner_id: c1_id, label: "Safe Spot Perimeter".to_string(), solar_system_id: PERIMETER_SOLAR_SYSTEM_ID, x: 1000.0, y: 2000.0, z: 3000.0, item_id: None, memo: String::new(), folder_id: None, is_shared: false });
        snap.structures.insert(970_101, UpwellStructureRecord { structure_id: 970_101, type_id: 35832, owner_corp_id: 950_101, solar_system_id: PERIMETER_SOLAR_SYSTEM_ID, name: "Perimeter - Astrahus Hub".to_string(), position: [5000.0, 6000.0, 7000.0], state: "ShieldVulnerable".to_string(), quantum_core_installed: true, fuel_blocks: 1000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0 });
    });

    // Concurrent writes from two sessions must not lose updates
    let r1 = Arc::clone(&repo);
    let r2 = Arc::clone(&repo);
    let t1 = std::thread::spawn(move || {
        for _ in 0..25 {
            r1.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&c1_id) {
                    ch.balance = ch.balance.checked_add(IskCents::from_isk(1_000)).unwrap();
                }
            });
        }
    });
    let t2 = std::thread::spawn(move || {
        for _ in 0..25 {
            r2.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&c2_id) {
                    ch.balance = ch.balance.checked_add(IskCents::from_isk(2_000)).unwrap();
                }
            });
        }
    });
    t1.join().unwrap();
    t2.join().unwrap();

    // Drop the active repository to simulate clean server shutdown
    drop(repo);

    // Reopen WorldRepository from the same SQLite DB file and verify persistence
    let restarted = WorldRepository::open_sqlite(&temp_db).expect("Reopen SQLite world DB");
    let c1_post = restarted.get_character(c1_id).unwrap();
    let c2_post = restarted.get_character(c2_id).unwrap();

    assert_eq!(c1_post.balance, IskCents::from_isk(5_000 + 25_000));
    assert_eq!(c2_post.balance, IskCents::from_isk(5_000 + 50_000));

    restarted.with_snapshot(|snap| {
        assert_eq!(snap.skills[&c1_id][&3300].level, 5);
        assert_eq!(snap.skill_queues[&c1_id].len(), 1);
        assert!(snap.market_orders.contains_key(&800_101));
        assert_eq!(snap.wallet_journal.len(), 1);
        assert!(snap.contracts.contains_key(&910_101));
        assert!(snap.industry_jobs.contains_key(&920_101));
        assert_eq!(snap.standing_transactions[&c1_id].len(), 1);
        assert_eq!(snap.mails.len(), 1);
        assert!(snap.fleets.contains_key(&940_101));
        assert!(snap.corporations.contains_key(&950_101));
        assert!(snap.bookmarks.contains_key(&960_101));
        assert!(snap.structures.contains_key(&970_101));
    });

    let _ = std::fs::remove_file(&temp_db);
    let _ = std::fs::remove_file(temp_db.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(temp_db.with_extension("sqlite-shm"));
}

// ====================================================================
// Test 3: DG-1: Dogma Modifier Engine, Stacking Penalties, Rifter + 2x Overdrive
// ====================================================================
// parity: plan:DG-1
#[tokio::test]
async fn test_plan_dg1_dogma_modifier_engine_rifter_overdrives() {
    let sde = SdeRegistry::with_canonical_seed();

    // 1. Validate modifier functions over V24 modifierInfo:
    // ItemModifier: modifies ship maxVelocity directly (+12.5% speedFactor)
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(100, OVERDRIVE_II_TYPE_ID, &sde),
            DogmaItemState::from_sde(1000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(4000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let spec_item = EffectModifierSpec {
            domain: "shipID".into(),
            func: "ItemModifier".into(),
            modified_attribute_id: attr_ids::MAX_VELOCITY,
            modifying_attribute_id: 20, // speedFactor
            operator: 6,                 // PostPercent (+12.5%)
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        };
        ctx.item.base_attributes.insert(20, 12.5);
        assert_eq!(ctx.apply_modifier_spec(100, &spec_item, &sde), 1);
        let vel = ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
        assert!((vel - 365.0 * 1.125).abs() < 1e-5);
    }

    // LocationGroupModifier: modifies items located in ship matching projectile weapon group 55
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(101, 519, &sde), // Gyro II
            DogmaItemState::from_sde(1000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(4000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let ac2 = DogmaItemState::from_sde(201, 2881, &sde); // group 55 (Projectile Weapon)
        let lml2 = DogmaItemState::from_sde(202, 2410, &sde); // group 509 (Missile Launcher)
        let base_ac_dmg = ac2.get_attribute(64, &sde);
        let base_lml_dmg = lml2.get_attribute(64, &sde);

        ctx.ship_located_items.push(ac2);
        ctx.ship_located_items.push(lml2);

        let spec_grp = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationGroupModifier".into(),
            modified_attribute_id: 64, // damageMultiplier
            modifying_attribute_id: 64,
            operator: 4,               // PostMul (*1.10)
            group_id: Some(55),
            skill_type_id: None,
            effect_id: None,
        };
        ctx.item.base_attributes.insert(64, 1.10);
        assert_eq!(ctx.apply_modifier_spec(101, &spec_grp, &sde), 1);

        assert!((ctx.ship_located_items[0].get_attribute(64, &sde) - base_ac_dmg * 1.10).abs() < 1e-5);
        assert_eq!(ctx.ship_located_items[1].get_attribute(64, &sde), base_lml_dmg);
    }

    // LocationRequiredSkillModifier: modifies items located in ship requiring specific skill
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(102, 22291, &sde), // BCS II
            DogmaItemState::from_sde(1000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(4000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let lml2 = DogmaItemState::from_sde(203, 2410, &sde); // requires 3319 Missile Launcher Operation
        let ac2 = DogmaItemState::from_sde(204, 2881, &sde);
        let base_lml_speed = lml2.get_attribute(51, &sde);
        let base_ac_speed = ac2.get_attribute(51, &sde);

        ctx.ship_located_items.push(lml2);
        ctx.ship_located_items.push(ac2);

        let spec_srq = EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationRequiredSkillModifier".into(),
            modified_attribute_id: 51,
            modifying_attribute_id: 51,
            operator: 4,
            group_id: None,
            skill_type_id: Some(3319),
            effect_id: None,
        };
        ctx.item.base_attributes.insert(51, 0.895);
        assert_eq!(ctx.apply_modifier_spec(102, &spec_srq, &sde), 1);
        assert!((ctx.ship_located_items[0].get_attribute(51, &sde) - base_lml_speed * 0.895).abs() < 1e-5);
        assert_eq!(ctx.ship_located_items[1].get_attribute(51, &sde), base_ac_speed);
    }

    // OwnerRequiredSkillModifier: modifies items owned by char requiring skill
    {
        let mut ctx = DogmaEffectContext::new(
            DogmaItemState::from_sde(103, 10037, &sde),
            DogmaItemState::from_sde(1000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(2000, 1373, &sde),
            DogmaItemState::from_sde(3000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(4000, RIFTER_TYPE_ID, &sde),
            DogmaItemState::from_sde(5000, 35832, &sde),
        );
        let mut drone = DogmaItemState::from_sde(501, 2454, &sde);
        drone.base_attributes.insert(attr_ids::REQUIRED_SKILL_1, 3436.0);
        drone.base_attributes.insert(64, 1.0);
        ctx.owned_items.push(drone);

        let spec_own = EffectModifierSpec {
            domain: "charID".into(),
            func: "OwnerRequiredSkillModifier".into(),
            modified_attribute_id: 64,
            modifying_attribute_id: 64,
            operator: 6,
            group_id: None,
            skill_type_id: Some(3436),
            effect_id: None,
        };
        ctx.item.base_attributes.insert(64, 10.0);
        assert_eq!(ctx.apply_modifier_spec(103, &spec_own, &sde), 1);
        assert!((ctx.owned_items[0].get_attribute(64, &sde) - 1.10).abs() < 1e-5);
    }

    // 2. Mathematical verification of stacking penalty curve S(n) = exp(-(n-1)^2 / 7.1289)
    let s0 = stacking_penalty_factor(0);
    let s1 = stacking_penalty_factor(1);
    let s2 = stacking_penalty_factor(2);
    let s3 = stacking_penalty_factor(3);

    assert_eq!(s0, 1.0);
    assert!((s1 - (-1.0_f64 / 7.1289).exp()).abs() < 1e-9);
    assert!((s1 - 0.869118).abs() < 1e-4);
    assert!((s2 - (-4.0_f64 / 7.1289).exp()).abs() < 1e-9);
    assert!((s2 - 0.570603).abs() < 1e-4);
    assert!((s3 - (-9.0_f64 / 7.1289).exp()).abs() < 1e-9);

    // 3. Canonical Rifter (type 587) with skills and 2 Overdrive Injector System II (type 1236)
    // Base velocity: 365 m/s, base cargo: 140 m³
    let mut fit = ShipFittingEngine::from_sde(1000, RIFTER_TYPE_ID, &sde);
    fit.set_character_skills(HashMap::from([(3394, 5)])); // Hull Upgrades V required for Overdrive II
    let v0 = fit.evaluated_context(&sde).ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
    let c0 = fit.evaluated_context(&sde).ship.get_attribute(38, &sde);
    assert_eq!(v0, 365.0);
    assert_eq!(c0, 140.0);

    // Fit 1st Overdrive II (Slot 11): +12.5% maxVelocity, -20% cargoCapacity
    fit.fit_module(2001, OVERDRIVE_II_TYPE_ID, 11, &sde).unwrap();
    let ctx1 = fit.evaluated_context(&sde);
    let v1 = ctx1.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
    let c1 = ctx1.ship.get_attribute(38, &sde);
    assert!((v1 - 365.0 * 1.125).abs() < 1e-4, "1st Overdrive: velocity must be 410.625, got {v1}");
    assert!((c1 - 140.0 * 0.80).abs() < 1e-4, "1st Overdrive: cargo must be 112.0, got {c1}");

    // Fit 2nd Overdrive II (Slot 12): stacking penalized velocity, unpenalized cargo penalty
    fit.fit_module(2002, OVERDRIVE_II_TYPE_ID, 12, &sde).unwrap();
    let ctx2 = fit.evaluated_context(&sde);
    let v2 = ctx2.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
    let c2 = ctx2.ship.get_attribute(38, &sde);

    let expected_v2 = 365.0 * (1.0 + 0.125) * (1.0 + 0.125 * s1);
    let expected_c2 = 140.0 * 0.80 * 0.80;
    assert!((v2 - expected_v2).abs() < 1e-3, "2nd Overdrive: velocity with stacking penalty must match published formula: got {v2}, expected {expected_v2}");
    assert!((c2 - expected_c2).abs() < 1e-4, "2nd Overdrive: cargo penalty (-20%) must be unpenalized: got {c2}, expected {expected_c2}");

    // Add Navigation V skill (+25% unpenalized bonus): skills stack freely without penalty
    let mut skills = HashMap::new();
    skills.insert(3394, 5); // Hull Upgrades V
    skills.insert(3449, 5); // Navigation V
    fit.set_character_skills(skills);
    let ctx3 = fit.evaluated_context(&sde);
    let v3 = ctx3.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
    let expected_v3 = expected_v2 * 1.25;
    assert!((v3 - expected_v3).abs() < 1e-3, "Skills must stack without penalty: got {v3}, expected {expected_v3}");
}

// ====================================================================
// Test 4: E-1 & E-2: Market Fees, Order Lifecycle, Escrow Management
// ====================================================================
// plan: E-1, plan: E-2
#[tokio::test]
async fn test_plan_e1_e2_market_fees_order_lifecycle_escrow() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", 90_000_002).await;

    // Part A (E-1): Broker fee discount with Broker Relations skill
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.standings.clear();
        }
        snap.skills.entry(90_000_001).or_default().insert(
            BROKER_RELATIONS_SKILL,
            TrainedSkillRecord { type_id: BROKER_RELATIONS_SKILL, level: 0, skill_points: 0.0 },
        );
    });

    // Buy order at Broker Relations 0: 3.0% base fee on 10,000 ISK = 300.0 ISK
    let order_l0 = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10_000), PyRep::Integer(90), PyRep::Integer(1)],
    )
    .await
    .expect("PlaceBuyOrder level 0");

    let PyRep::KeyVal(kv_l0) = order_l0 else { panic!("Expected KeyVal") };
    let fee_l0 = kv_l0.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    assert!((fee_l0 - 300.0).abs() < 0.01, "Broker fee at level 0 must be 300.0 ISK, got {fee_l0}");

    let oid_l0 = kv_l0.get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_l0)]).await.expect("CancelCharOrder");

    // Train Broker Relations to level 5: fee reduces to 1.5% (150.0 ISK on 10,000 ISK)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            BROKER_RELATIONS_SKILL,
            TrainedSkillRecord { type_id: BROKER_RELATIONS_SKILL, level: 5, skill_points: 256_000.0 },
        );
    });

    let order_l5 = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(10_000), PyRep::Integer(90), PyRep::Integer(1)],
    )
    .await
    .expect("PlaceBuyOrder level 5");

    let PyRep::KeyVal(kv_l5) = order_l5 else { panic!("Expected KeyVal") };
    let fee_l5 = kv_l5.get("brokerFeeISK").and_then(|v| v.as_f64()).unwrap();
    assert!((fee_l5 - 150.0).abs() < 0.01, "Broker fee at level 5 must be 150.0 ISK, got {fee_l5}");

    let oid_l5 = kv_l5.get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid_l5)]).await.expect("CancelCharOrder");

    // Part B (E-2): Order lifecycle, ModifyCharOrder, CancelCharOrder & Escrow
    let bal_before_buy = ctx.db.get_character(90_000_001).unwrap().balance;

    let buy_res = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(1.0), PyRep::Integer(20_000), PyRep::Integer(90), PyRep::Integer(1)],
    )
    .await
    .expect("PlaceBuyOrder for escrow test");

    let PyRep::KeyVal(buy_kv) = buy_res else { panic!("Expected KeyVal") };
    assert_eq!(buy_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    let buy_order_id = buy_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();
    let bal_after_buy = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(bal_after_buy, bal_before_buy.checked_sub(IskCents::from_isk(20_000 + 300)).unwrap());

    // Advance virtual clock past 300s modification cooldown (D-5 / E-2)
    ctx.advance_virtual_clock(305);

    // ModifyCharOrder: update price to 1.10 ISK
    let mod_res = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "ModifyCharOrder",
        &[PyRep::Integer(buy_order_id), PyRep::Real(1.10), PyRep::Integer(20_000), PyRep::Integer(1)],
    )
    .await;
    let mod_rep = mod_res.expect("ModifyCharOrder must succeed after cooldown");
    let PyRep::KeyVal(mod_kv) = mod_rep else { panic!("Expected KeyVal") };
    let relist_fee = mod_kv.get("relistFeeISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(relist_fee > 0.0, "Relist fee must be charged on order modification");

    ctx.db.with_snapshot(|snap| {
        let order = snap.market_orders.get(&buy_order_id).expect("Order must exist");
        assert_eq!(order.price, IskCents::from_f64_isk(1.10));
        assert_eq!(order.escrow, IskCents::from_isk(22_000));
    });

    // CancelCharOrder: cancels order and refunds escrow cleanly
    let cancel_res = rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(buy_order_id)]).await;
    assert!(cancel_res.is_ok(), "CancelCharOrder must succeed");

    let bal_after_cancel = ctx.db.get_character(90_000_001).unwrap().balance;
    let expected_net_cost = IskCents::from_f64_isk(300.0 + relist_fee);
    assert_eq!(
        bal_after_cancel,
        bal_before_buy.checked_sub(expected_net_cost).unwrap(),
        "All escrow must be refunded on cancellation; net cost is broker fee + relist fee"
    );

    // Part C (E-2): Order matching & partial fill
    let seller_item_id = 100_000_701;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            seller_item_id,
            InventoryItemRecord {
                item_id: seller_item_id,
                type_id: OVERDRIVE_II_TYPE_ID,
                owner_id: 90_000_002,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Seller places resting sell order at 500,000.0 ISK for 10 units of Overdrive II
    let sell_res = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(OVERDRIVE_II_TYPE_ID as i64)),
                ("price", PyRep::Real(500_000.0)),
                ("quantity", PyRep::Integer(10)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder resting sell order");

    let PyRep::KeyVal(sell_kv) = sell_res else { panic!("Expected KeyVal") };
    assert_eq!(sell_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));

    // Buyer places matching buy order for 25 units at 500,000.0 ISK (matches 10 immediately, 15 remaining)
    let match_buy = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(OVERDRIVE_II_TYPE_ID as i64),
            PyRep::Real(500_000.0),
            PyRep::Integer(25),
            PyRep::Integer(90),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("PlaceBuyOrder for matching");
    let PyRep::KeyVal(match_buy_kv) = match_buy else { panic!("Expected KeyVal") };
    let match_buy_id = match_buy_kv.get("orderID").and_then(|v| v.as_i64()).unwrap();

    ctx.db.with_snapshot(|snap| {
        let order = snap.market_orders.get(&match_buy_id).expect("Buy order exists");
        assert_eq!(order.vol_remaining, 15, "Buy order must have 15 units remaining");
        let buyer_has_mod = snap.items.values().any(|it| {
            it.owner_id == 90_000_001
                && it.type_id == OVERDRIVE_II_TYPE_ID
                && it.location_id == JITA_STATION_ID
                && it.quantity >= 10
        });
        assert!(buyer_has_mod, "Buyer must have received purchased Overdrives in hangar");
    });

    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(match_buy_id)]).await.expect("CancelCharOrder remaining");
}

// ====================================================================
// Test 5: E-3: Contracts Lifecycle, ItemExchange and Courier
// ====================================================================
// plan: E-3
#[tokio::test]
async fn test_plan_e3_contracts_item_exchange_and_courier() {
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 301, "demo", "demo-secret", 90_000_001).await;
    let mut s2 = setup_pilot(&ctx, 302, "other", "other-secret", 90_000_002).await;

    let contract_item_id = 100_000_881;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            contract_item_id,
            InventoryItemRecord {
                item_id: contract_item_id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Part A (E-3): ItemExchange Contract
    let create_exchange = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1), // ItemExchange
            PyRep::Integer(0), // Public
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(50_000.0), // price
            PyRep::Real(0.0),      // reward
            PyRep::Real(0.0),      // collateral
            PyRep::String("ItemExchange Contract Test".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(contract_item_id)]),
            PyRep::List(vec![]),
        ],
    )
    .await
    .expect("CreateContract ItemExchange");

    let exchange_contract_id = match create_exchange {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("contractID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!("Expected contract ID"),
    };

    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&contract_item_id).expect("Item exists");
        assert_eq!(it.flag, InventoryFlag::Locked, "Item must be escrowed in contract locked flag");
    });

    let s1_bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance;

    let accept_res = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(exchange_contract_id)]).await;
    assert!(accept_res.is_ok(), "AcceptContract ItemExchange must succeed");

    let s1_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    let s2_bal_after = ctx.db.get_character(90_000_002).unwrap().balance;

    assert_eq!(s1_bal_after, s1_bal_before.checked_add(IskCents::from_isk(50_000)).unwrap(), "Issuer must receive contract price");
    assert_eq!(s2_bal_after, s2_bal_before.checked_sub(IskCents::from_isk(50_000)).unwrap(), "Acceptor must pay contract price");

    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&contract_item_id).expect("Item exists");
        assert_eq!(it.owner_id, 90_000_002, "Item ownership must transfer to acceptor");
        assert_eq!(it.flag, InventoryFlag::Hangar, "Item must be released to acceptor hangar");
        let contract = snap.contracts.get(&exchange_contract_id).expect("Contract exists");
        assert_eq!(contract.status, "Completed");
    });

    // Part B (E-3): Courier Contract & Delivery
    let courier_item_id = 100_000_882;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            courier_item_id,
            InventoryItemRecord {
                item_id: courier_item_id,
                type_id: TRITANIUM_TYPE_ID,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 20_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let create_courier = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3), // Courier
            PyRep::Integer(0), // Public
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Real(0.0),       // price
            PyRep::Real(25_000.0),  // reward
            PyRep::Real(100_000.0), // collateral
            PyRep::String("Courier Contract Test".into()),
            PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(courier_item_id)]),
            PyRep::List(vec![]),
        ],
    )
    .await
    .expect("CreateContract Courier");

    let courier_contract_id = match create_courier {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("contractID").and_then(|v| v.as_i64()).unwrap(),
        _ => panic!("Expected contract ID"),
    };

    let s2_bal_pre_courier = ctx.db.get_character(90_000_002).unwrap().balance;
    rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(courier_contract_id)])
        .await
        .expect("AcceptContract Courier");

    let s2_bal_accepted = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_accepted, s2_bal_pre_courier.checked_sub(IskCents::from_isk(100_000)).unwrap(), "Acceptor must pay 100,000 ISK collateral into escrow");

    rpc(&ctx, &mut s2, "contractProxy", "CompleteContract", &[PyRep::Integer(courier_contract_id)])
        .await
        .expect("CompleteContract Courier");

    let s2_bal_completed = ctx.db.get_character(90_000_002).unwrap().balance;
    assert_eq!(s2_bal_completed, s2_bal_accepted.checked_add(IskCents::from_isk(100_000 + 25_000)).unwrap(), "Acceptor must receive collateral refund + reward");

    ctx.db.with_snapshot(|snap| {
        let contract = snap.contracts.get(&courier_contract_id).expect("Courier contract");
        assert_eq!(contract.status, "Completed");
    });
}

// ====================================================================
// Test 6: C-5: Asteroid Belts, Mining Laser Cycle, Ore Delivery & Depletion
// ====================================================================
#[tokio::test]
async fn test_plan_c5_mining_belts_lasers_and_depletion() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 401, "demo", "demo-secret", char_id).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            MINING_SKILL,
            TrainedSkillRecord { type_id: MINING_SKILL, level: 5, skill_points: 256_000.0 },
        );
    });

    // 1. Undock character to space
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock to space");

    let ship_id = sess.ship_id.expect("Active ship ID");
    let sys_id = sess.solar_system_id.expect("Active solar system ID");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball");
    let ship_pos = (ship_ball.position.x, ship_ball.position.y, ship_ball.position.z);

    // 2. Spawn an asteroid in the system near the ship (1,000m away, within Miner I 10km range)
    let initial_ore_qty = 2_000;
    let ast_id = 1_000_005_001;
    let ast_pos = (ship_pos.0 + 1_000.0, ship_pos.1, ship_pos.2);
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: sys_id,
        belt_id: 1001,
        type_id: VELDSPAR_TYPE_ID,
        quantity: initial_ore_qty,
        initial_quantity: initial_ore_qty,
        position: ast_pos,
        radius: 100.0,
    });

    // 3. Fit Miner I (type 483) to ship in HiSlot2 (flag 29)
    let module_id = 100_000_811;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            module_id,
            InventoryItemRecord {
                item_id: module_id,
                type_id: MINER_I_TYPE_ID,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // 4. Activate Miner I laser targeting the asteroid via dogmaIM.Activate
    let act_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(module_id), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.Activate Miner I");

    let yield_units = get_dict_val(&act_res, "yieldUnits").and_then(|v| v.as_i64()).expect("yieldUnits");
    let ore_type_id = get_dict_val(&act_res, "oreTypeID").and_then(|v| v.as_i64()).expect("oreTypeID");
    let cycle_time_ms = get_dict_val(&act_res, "cycleTimeMs").and_then(|v| v.as_i64()).expect("cycleTimeMs");

    assert!(yield_units > 0, "Mining cycle must yield positive ore units");
    assert_eq!(ore_type_id, VELDSPAR_TYPE_ID as i64, "Mined ore must be Veldspar");
    assert!(cycle_time_ms > 0, "Cycle time must be positive");

    // 5. Verify ore delivered to ship hold (SpecializedMaterialBay or Cargo)
    ctx.db.with_snapshot(|snap| {
        let holds = [InventoryFlag::SpecializedMaterialBay, InventoryFlag::Cargo];
        let has_mined_ore = snap.items.values().any(|it| {
            it.owner_id == char_id
                && it.location_id == ship_id
                && it.type_id == VELDSPAR_TYPE_ID
                && holds.contains(&it.flag)
                && it.quantity >= yield_units as i32
        });
        assert!(has_mined_ore, "Ship hold must contain mined Veldspar ore");
    });

    // 6. Verify asteroid quantity depleted in database
    let ast_after = ctx.db.get_asteroid(ast_id).expect("Asteroid must exist in DB");
    assert_eq!(ast_after.quantity, initial_ore_qty - yield_units, "Asteroid quantity must be depleted by mined yield units");
}

// ====================================================================
// Test 7: C-6: Reprocessing Yield, Station Tax & Material Outputs
// ====================================================================
#[tokio::test]
async fn test_plan_c6_reprocessing_yield_tax_and_materials() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 501, "demo", "demo-secret", char_id).await;

    // Train Reprocessing V (3385) and Reprocessing Efficiency V (3389)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(REPROCESSING_SKILL, TrainedSkillRecord { type_id: REPROCESSING_SKILL, level: 5, skill_points: 256_000.0 });
        skills.insert(REPROC_EFFICIENCY_SKILL, TrainedSkillRecord { type_id: REPROC_EFFICIENCY_SKILL, level: 5, skill_points: 256_000.0 });
    });

    // Seed pilot hangar at Jita 4-4 (station 60003760) with 1,000 units of Veldspar (portion size 100)
    let veldspar_item_id = 100_000_901;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            veldspar_item_id,
            InventoryItemRecord {
                item_id: veldspar_item_id,
                type_id: VELDSPAR_TYPE_ID,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. GetQuotes on Veldspar item via reprocessingSvc.GetQuotes
    let quote_res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "GetQuotes",
            &[PyRep::Integer(veldspar_item_id)],
            &kw,
            None,
        )
        .await
        .expect("reprocessingSvc.GetQuotes");

    let quote_entry = match &quote_res {
        PyRep::List(l) => l.first().expect("quote entry"),
        _ => &quote_res,
    };
    let quote_yield = get_dict_val(quote_entry, "yieldRate")
        .and_then(|v| match v { PyRep::Real(r) => Some(*r), _ => None })
        .expect("yieldRate");
    let quote_tax = get_dict_val(quote_entry, "stationTaxRate")
        .and_then(|v| match v { PyRep::Real(r) => Some(*r), _ => None })
        .expect("stationTaxRate");

    assert!(quote_yield > 0.50, "Yield rate must exceed base 50% facility yield with skills");
    assert!((quote_tax - 0.035).abs() < 1e-4, "Station tax rate must reflect standings: got {quote_tax}");

    let _ = ctx.drain_notifications_for_char(char_id);

    // 2. Execute Reprocess on Veldspar via reprocessingSvc.Reprocess
    let reproc_res = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(veldspar_item_id)],
            &kw,
            None,
        )
        .await
        .expect("reprocessingSvc.Reprocess Veldspar");

    let outputs_rep = get_dict_val(&reproc_res, "outputs").expect("outputs list");
    let mat_outputs = match outputs_rep {
        PyRep::List(l) => l,
        _ => panic!("Expected list of output items"),
    };
    assert!(!mat_outputs.is_empty(), "Outputs list must contain produced materials");

    let trit_out = mat_outputs.iter().find(|entry| {
        get_dict_val(entry, "typeID").and_then(|v| v.as_i64()) == Some(TRITANIUM_TYPE_ID as i64)
    });
    assert!(trit_out.is_some(), "Reprocessing Veldspar must produce Tritanium");

    let trit_qty = get_dict_val(trit_out.unwrap(), "quantity")
        .and_then(|v| v.as_i64())
        .expect("Tritanium output quantity");
    assert!(trit_qty > 0, "Produced Tritanium quantity must be positive");

    ctx.db.with_snapshot(|snap| {
        let has_trit = snap.items.values().any(|it| {
            it.owner_id == char_id
                && it.location_id == JITA_STATION_ID
                && it.type_id == TRITANIUM_TYPE_ID
                && it.flag == InventoryFlag::Hangar
                && it.quantity >= trit_qty as i32
        });
        assert!(has_trit, "Pilot hangar must contain reprocessed Tritanium");
    });

    // 3. Reject non-reprocessable item: seed item with type that lacks typeMaterials (PLEX 44992)
    let non_reproc_item_id = 100_000_902;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            non_reproc_item_id,
            InventoryItemRecord {
                item_id: non_reproc_item_id,
                type_id: 44_992, // PLEX (has no SDE typeMaterials)
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let non_reproc_quote = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "GetQuotes",
            &[PyRep::Integer(non_reproc_item_id)],
            &kw,
            None,
        )
        .await;

    if let Ok(PyRep::List(quotes)) = non_reproc_quote {
        assert!(quotes.is_empty(), "Non-reprocessable items must produce empty quotes");
    }

    let non_reproc_exec = ctx
        .dispatch_rpc(
            &mut sess,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(non_reproc_item_id)],
            &kw,
            None,
        )
        .await;
    let err = non_reproc_exec.unwrap_err();
    assert_eq!(err.code, "ItemNotReprocessable", "Reprocessing item without typeMaterials must fail with ItemNotReprocessable");
}
