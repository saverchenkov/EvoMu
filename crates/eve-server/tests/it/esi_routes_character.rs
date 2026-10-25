//! `esi_routes_character` — `ESI-4` character route integration test (Clones, Skills, Location,
//! Fittings, Assets, Wallet, Loyalty, Mail), split out of `esi_routes.rs` unchanged to keep each
//! test file under the line limit. Shares that module's SSO token and schema-validation helpers.

use crate::esi_routes::{issue_token_with_scopes, validate_and_json};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord,
    SkillQueueEntry, UpwellStructureRecord,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[tokio::test]
async fn test_esi4_character_clones_skills_location_fittings_assets_wallet_loyalty_and_mail() {
    // parity-lint: allow ESI sweep test covering character endpoints (clones/skills/location/fittings/assets/wallet/loyalty/mail)
    // parity: esi:POST /characters/affiliation, feat:esi.character.affiliation
    // parity: esi:GET /characters/{character_id}/portrait, feat:esi.character.portrait
    // parity: esi:GET /characters/{character_id}/fatigue, feat:esi.character.fatigue
    // parity: esi:GET /characters/{character_id}/blueprints, feat:esi.character.blueprints
    // parity: feat:esi.character.standings, feat:esi.clones.clones
    // parity: esi:GET /characters/{character_id}/implants, feat:esi.clones.implants
    // parity: esi:GET /characters/{character_id}/attributes, esi:GET /characters/{character_id}/skillqueue, esi:GET /characters/{character_id}/skills, feat:esi.skills.skills
    // parity: esi:GET /characters/{character_id}/location, feat:esi.location.location, esi:POST /characters/{character_id}/assets/locations, esi:POST /characters/{character_id}/assets/names
    // parity: esi:GET /characters/{character_id}/online, esi:POST /corporations/{corporation_id}/assets/locations, esi:POST /corporations/{corporation_id}/assets/names
    // parity: esi:GET /characters/{character_id}/ship, feat:esi.location.ship, esi:GET /corporations/{corporation_id}/wallets
    // parity: esi:GET /characters/{character_id}/fittings, feat:esi.fittings.list, esi:GET /characters/{character_id}/mail, esi:POST /characters/{character_id}/mail, esi:GET /characters/{character_id}/mail/{mail_id}
    // parity: esi:POST /characters/{character_id}/fittings, esi:DELETE /characters/{character_id}/fittings/{fitting_id}, feat:esi.fittings.write, rpc:invbroker.SetLabel, rpc:structureCynoBeaconMgr.CmdJumpToStructureBeacon
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();

    let corp_id = 98_000_701;
    let alliance_id = 99_000_701;
    let bpo_item_id = 990_007_001;
    let bpc_item_id = 990_007_002;
    let corp_asset_id = 990_007_003;
    let space_asset_id = 990_007_004;
    let (&bp_type_id, _) = ctx.sde.blueprints.iter().next().expect("SDE blueprint");

    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        let active_ship_id = snap
            .characters
            .get(&90_000_001)
            .map(|c| c.active_ship_id)
            .unwrap_or(0);
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.war_faction_id = Some(500_001);
            ch.clone_grade = 1; // Alpha clone to verify active_skill_level clamping
            ch.last_clone_jump_unix = now.saturating_sub(3600);
            ch.loyalty_points.insert(1_000_035, 12_500);
            ch.standings.insert(1_000_035, 5.25);
        }
        // Persisted login/logout stats
        snap.character_login_stats.insert(
            90_000_001,
            eve_db::CharacterLoginStats {
                last_login_unix: now.saturating_sub(600),
                last_logout_unix: now.saturating_sub(1200),
                logins_count: 7,
            },
        );
        // Custom ship name persisted on active_ship_id
        if active_ship_id > 0 {
            snap.item_custom_names
                .insert(active_ship_id, "U.S.S. Defiant".to_string());
        }
        // Trained Cloaking IV (11579, Omega-only) to verify Alpha active_skill_level=0 clamping
        snap.skills.entry(90_000_001).or_default().insert(
            11579,
            eve_db::TrainedSkillRecord {
                type_id: 11579,
                level: 4,
                skill_points: 271_530.0,
            },
        );
        // Recipient CSPA charge for POST /characters/{id}/mail
        snap.character_cspa_charges
            .insert(90_000_002, IskCents::from_isk(2_950));

        let wallet_divisions = vec![
            eve_db::CorpWalletDivisionRecord {
                division_id: 1,
                name: "Master Wallet".to_string(),
                balance: IskCents::from_isk(250_000_000),
            },
            eve_db::CorpWalletDivisionRecord {
                division_id: 2,
                name: "2nd Wallet".to_string(),
                balance: IskCents::from_isk(15_000_000),
            },
            eve_db::CorpWalletDivisionRecord {
                division_id: 3,
                name: "3rd Wallet".to_string(),
                balance: IskCents::from_isk(12_500_000),
            },
            eve_db::CorpWalletDivisionRecord {
                division_id: 4,
                name: "4th Wallet".to_string(),
                balance: IskCents::from_isk(9_750_000),
            },
            eve_db::CorpWalletDivisionRecord {
                division_id: 5,
                name: "5th Wallet".to_string(),
                balance: IskCents::from_isk(8_200_000),
            },
            eve_db::CorpWalletDivisionRecord {
                division_id: 6,
                name: "6th Wallet".to_string(),
                balance: IskCents::from_isk(5_100_000),
            },
            eve_db::CorpWalletDivisionRecord {
                division_id: 7,
                name: "7th Wallet".to_string(),
                balance: IskCents::from_isk(3_500_000),
            },
        ];
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "ESI4 Director Corp".to_string(),
                ticker: "E4DC".to_string(),
                ceo_id: 90_000_001,
                alliance_id: Some(alliance_id),
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(250_000_000),
                wallet_divisions,
                members: HashSet::from([90_000_001]),
                ..Default::default()
            },
        );
        snap.active_implants.insert(90_000_001, vec![9899, 9941]);
        snap.jump_clones.insert(
            90_000_001,
            vec![JumpCloneRecord {
                jump_clone_id: 7001,
                character_id: 90_000_001,
                location_id: 60_003_760,
                implants: vec![9899],
            }],
        );
        let queue_start = now.saturating_sub(600);
        snap.skill_queues.insert(
            90_000_001,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 4,
                start_sp: 15_000.0,
                end_sp: 45_255.0,
                duration_seconds: 1800.0,
                started_at_unix: Some(queue_start),
            }],
        );
        snap.items.insert(
            bpo_item_id,
            InventoryItemRecord {
                item_id: bpo_item_id,
                type_id: bp_type_id,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(1112, 8.0), (1113, 16.0)]),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            bpc_item_id,
            InventoryItemRecord {
                item_id: bpc_item_id,
                type_id: bp_type_id,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(10),
            },
        );
        snap.items.insert(
            corp_asset_id,
            InventoryItemRecord {
                item_id: corp_asset_id,
                type_id: 587,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // In-space deployed asset with persisted 3D coordinates
        snap.items.insert(
            space_asset_id,
            InventoryItemRecord {
                item_id: space_asset_id,
                type_id: 33474, // Mobile Depot
                owner_id: 90_000_001,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(space_asset_id, [1250.0, -2500.0, 5000.0]);
        snap.items.insert(
            880_011,
            InventoryItemRecord {
                item_id: 880_011,
                type_id: 587,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(880_011, [-4500.0, 8250.0, -1100.0]);
        snap.corporations.insert(
            98_000_702,
            CorporationRecord {
                corporation_id: 98_000_702,
                name: "Other CEO Corp".to_string(),
                ticker: "OTHR".to_string(),
                ceo_id: 90_000_002,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(1_000_000),
                members: HashSet::from([90_000_002]),
                ..Default::default()
            },
        );
        Ok(())
    });

    ctx.db
        .acquire_online_lock(1, 42, Some(90_000_001), true, now)
        .expect("acquire_online_lock");

    let token = issue_token_with_scopes(
        &ctx,
        90_000_001,
        vec![
            "esi-assets.read_assets.v1".to_string(),
            "esi-assets.read_corporation_assets.v1".to_string(),
            "esi-characters.read_blueprints.v1".to_string(),
            "esi-characters.read_fatigue.v1".to_string(),
            "esi-characters.read_loyalty.v1".to_string(),
            "esi-characters.read_standings.v1".to_string(),
            "esi-clones.read_clones.v1".to_string(),
            "esi-clones.read_implants.v1".to_string(),
            "esi-fittings.read_fittings.v1".to_string(),
            "esi-fittings.write_fittings.v1".to_string(),
            "esi-location.read_location.v1".to_string(),
            "esi-location.read_online.v1".to_string(),
            "esi-location.read_ship_type.v1".to_string(),
            "esi-mail.organize_mail.v1".to_string(),
            "esi-mail.read_mail.v1".to_string(),
            "esi-mail.send_mail.v1".to_string(),
            "esi-skills.read_skills.v1".to_string(),
            "esi-skills.read_skillqueue.v1".to_string(),
            "esi-wallet.read_character_wallet.v1".to_string(),
            "esi-wallet.read_corporation_wallets.v1".to_string(),
        ],
    );

    // 1. Character affiliation, portrait, standings, fatigue, blueprints
    let aff = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/affiliation/")
            .with_body(serde_json::to_vec(&json!([90_000_001])).unwrap()),
        200,
    );
    assert_eq!(aff[0]["character_id"], json!(90_000_001));
    assert_eq!(aff[0]["corporation_id"], json!(corp_id));
    assert_eq!(aff[0]["alliance_id"], json!(alliance_id));
    assert_eq!(aff[0]["faction_id"], json!(500_001));

    let portrait = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/portrait/"),
        200,
    );
    assert!(portrait["px128x128"]
        .as_str()
        .is_some_and(|u| u.contains("90000001") && !u.contains("images.evetech.net")));

    let standings = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/standings/").with_bearer_token(&token),
        200,
    );
    assert!(standings
        .as_array()
        .expect("standings")
        .iter()
        .any(|s| s["from_id"] == json!(1_000_035) && s["standing"] == json!(5.25)));

    // Trigger jump fatigue via an actual capital ship jump (`structureCynoBeaconMgr.CmdJumpToStructureBeacon`)
    let jump_beacon_id = 1_040_000_007_701_i64;
    let jump_fuel_item_id = 990_007_701_i64;
    let jump_dest_sys_id = 30_000_204_u32; // Ohkunen (0.38 low-sec, 4.80 LY from Jita)
    let jump_ship_id = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&90_000_001)
            .map(|c| c.active_ship_id)
            .unwrap_or(100_000_001)
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_142;
            ch.station_id = None;
            ch.structure_id = None;
            ch.jump_cooldown_until_unix = 0;
            ch.jump_fatigue_until_unix = 0;
            ch.jump_fatigue_minutes = 0.0;
        }
        snap.structures.insert(
            jump_beacon_id,
            UpwellStructureRecord {
                structure_id: jump_beacon_id,
                type_id: 35840, // Pharolux Cyno Beacon
                owner_corp_id: corp_id,
                solar_system_id: jump_dest_sys_id,
                name: "Ohkunen Pharolux Cyno Beacon".to_string(),
                position: [15_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            jump_fuel_item_id,
            InventoryItemRecord {
                item_id: jump_fuel_item_id,
                type_id: 16274, // Helium Isotopes
                owner_id: 90_000_001,
                location_id: jump_ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    jita_handle
        .spawn_ball(eve_destiny::Ball {
            id: jump_ship_id,
            type_id: 23757, // Archon carrier
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 85.0,
            inertia: 0.04,
            mass: 1_230_000_000.0,
            radius: 1_600.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn capital ship in Jita for beacon jump");
    let mut jump_sess = eve_machonet::SessionState::new(42);
    jump_sess.user_id = Some(1);
    jump_sess.character_id = Some(90_000_001);
    jump_sess.corporation_id = Some(corp_id);
    jump_sess.alliance_id = Some(alliance_id);
    jump_sess.ship_id = Some(jump_ship_id);
    jump_sess.solar_system_id = Some(30_000_142);
    ctx.dispatch_rpc(
        &mut jump_sess,
        "structureCynoBeaconMgr",
        None,
        "CmdJumpToStructureBeacon",
        &[
            eve_proto::PyRep::Integer(jump_dest_sys_id as i64),
            eve_proto::PyRep::Integer(jump_beacon_id),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdJumpToStructureBeacon must compute and record jump fatigue & cooldown");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
        }
        snap.items.remove(&jump_fuel_item_id);
    });

    let (last_update_unix, jump_cooldown_until_unix, jump_fatigue_until_unix) =
        ctx.db.with_snapshot(|snap| {
            let last_jump = snap
                .character_last_jump_unix
                .get(&90_000_001)
                .copied()
                .unwrap_or(0);
            let ch = snap.characters.get(&90_000_001).expect("char 90_000_001");
            (
                last_jump,
                ch.jump_cooldown_until_unix,
                ch.jump_fatigue_until_unix,
            )
        });
    assert!(
        last_update_unix < jump_cooldown_until_unix
            && jump_cooldown_until_unix < jump_fatigue_until_unix,
        "Capital jump must record last_update_unix ({last_update_unix}) < jump_cooldown_until_unix ({jump_cooldown_until_unix}) < jump_fatigue_until_unix ({jump_fatigue_until_unix})"
    );

    let fatigue = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/fatigue/").with_bearer_token(&token),
        200,
    );
    let last_jump_date = fatigue["last_jump_date"].as_str().expect("last_jump_date");
    let last_update_date = fatigue["last_update_date"]
        .as_str()
        .expect("last_update_date");
    let jump_fatigue_expire_date = fatigue["jump_fatigue_expire_date"]
        .as_str()
        .expect("jump_fatigue_expire_date");
    let cooldown_iso =
        eve_server::esi::sample_routes::format_iso8601_utc(jump_cooldown_until_unix);
    assert_eq!(
        last_update_date, last_jump_date,
        "last_update_date must be the jump timestamp, never jump_cooldown_until_unix"
    );
    assert!(
        last_update_date < cooldown_iso.as_str()
            && cooldown_iso.as_str() < jump_fatigue_expire_date,
        "Must satisfy last_update_date ({last_update_date}) < jump_cooldown_until_unix ({cooldown_iso}) < jump_fatigue_expire_date ({jump_fatigue_expire_date})"
    );

    // When fatigue expires or is 0, jump_fatigue_expire_date must be omitted while last_update_date remains the jump timestamp (never jump_cooldown_until_unix)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.jump_fatigue_until_unix = 0;
            ch.jump_fatigue_minutes = 0.0;
            ch.jump_cooldown_until_unix = now + 900;
        }
    });
    let fatigue_zero = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/fatigue/").with_bearer_token(&token),
        200,
    );
    assert!(
        fatigue_zero.get("jump_fatigue_expire_date").is_none(),
        "Must NOT fall back to jump_cooldown_until_unix when fatigue is 0"
    );
    assert_eq!(
        fatigue_zero["last_update_date"].as_str(),
        Some(last_update_date),
        "last_update_date must remain the jump timestamp, never jump_cooldown_until_unix"
    );
    assert_ne!(
        fatigue_zero["last_update_date"],
        json!(eve_server::esi::sample_routes::format_iso8601_utc(
            now + 900
        )),
        "last_update_date must NEVER equal jump_cooldown_until_unix"
    );

    let bps = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/blueprints/")
            .with_bearer_token(&token),
        200,
    );
    let bps_arr = bps.as_array().expect("blueprints");
    assert!(bps_arr.iter().any(|b| {
        b["item_id"] == json!(bpo_item_id)
            && b["quantity"] == json!(-1)
            && b["runs"] == json!(-1)
            && b["material_efficiency"] == json!(8)
            && b["time_efficiency"] == json!(16)
    }));
    assert!(bps_arr
        .iter()
        .any(|b| b["item_id"] == json!(bpc_item_id) && b["quantity"] == json!(-2) && b["runs"] == json!(10)));

    // 2. Clones & Implants
    let clones = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v3/characters/90000001/clones/").with_bearer_token(&token),
        200,
    );
    assert_eq!(clones["jump_clones"][0]["jump_clone_id"], json!(7001));
    assert!(clones["last_clone_jump_date"].as_str().is_some());
    assert_ne!(
        clones["last_clone_jump_date"],
        fatigue["last_jump_date"],
        "Fatigue last_jump_date must track jump drive usage, never last_clone_jump_date"
    );

    let implants = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/implants/").with_bearer_token(&token),
        200,
    );
    assert_eq!(implants, json!([9899, 9941]));

    // 3. Skills (including Alpha active_skill_level clamping), Attributes, Skillqueue
    let skills_json = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v4/characters/90000001/skills/").with_bearer_token(&token),
        200,
    );
    let cloaking_skill = skills_json["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .find(|s| s["skill_id"] == json!(11579))
        .expect("Cloaking skill in /skills");
    assert_eq!(cloaking_skill["trained_skill_level"], json!(4));
    assert_eq!(
        cloaking_skill["active_skill_level"],
        json!(0),
        "Alpha clone must clamp active_skill_level against clonegrades.fsdbinary"
    );

    let attrs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/attributes/")
            .with_bearer_token(&token),
        200,
    );
    assert!(attrs["intelligence"].as_i64().unwrap_or(0) >= 15);

    let sq = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/skillqueue/")
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(sq[0]["skill_id"], json!(3300));
    assert_eq!(sq[0]["finished_level"], json!(4));
    assert_eq!(sq[0]["level_start_sp"], json!(8000));
    assert_eq!(sq[0]["training_start_sp"], json!(15000));
    assert_ne!(
        sq[0]["level_start_sp"],
        sq[0]["training_start_sp"],
        "Partially trained queue entry must report distinct level_start_sp and training_start_sp"
    );
    assert!(sq[0]["start_date"].as_str().is_some());
    assert!(sq[0]["finish_date"].as_str().is_some());

    // Consecutive GET calls must return identical start_date and finish_date (no drift)
    let sq2 = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/skillqueue/")
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(
        sq[0]["start_date"], sq2[0]["start_date"],
        "start_date must be anchored to persisted queue start and not drift"
    );
    assert_eq!(
        sq[0]["finish_date"], sq2[0]["finish_date"],
        "finish_date must be anchored to persisted queue start and not drift"
    );

    // 4. Location (station & structure), Online, Ship
    let loc = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/location/").with_bearer_token(&token),
        200,
    );
    assert_eq!(loc["solar_system_id"], json!(30_000_142));
    assert_eq!(loc["station_id"], json!(60_003_760));
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(1_000_000_000_701);
        }
        Ok(())
    });
    let loc_struct = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/location/").with_bearer_token(&token),
        200,
    );
    assert_eq!(loc_struct["structure_id"], json!(1_000_000_000_701_i64));
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
        }
        Ok(())
    });

    let online = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/online/").with_bearer_token(&token),
        200,
    );
    assert_eq!(online["online"], json!(true));
    assert_eq!(online["logins"], json!(8));
    assert!(online["last_login"].as_str().is_some());
    assert!(online["last_logout"].as_str().is_some());

    let ship = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/ship/").with_bearer_token(&token),
        200,
    );
    assert!(ship["ship_item_id"].as_i64().unwrap_or(0) > 0);
    assert!(ship["ship_type_id"].as_i64().unwrap_or(0) > 0);
    assert_eq!(ship["ship_name"], json!("U.S.S. Defiant"));

    // 5. Fittings CRUD (POST -> GET -> DELETE -> GET)
    let created_fit = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v2/characters/90000001/fittings/")
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "name": "ESI4 Rifter Fit",
                    "description": "Created via ESI POST",
                    "ship_type_id": 587,
                    "items": [
                        { "type_id": 440, "flag": "MedSlot0", "quantity": 1 }
                    ]
                }))
                .unwrap(),
            ),
        201,
    );
    let fit_id = created_fit["fitting_id"].as_i64().expect("fitting_id");

    let fits_after_post = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/fittings/").with_bearer_token(&token),
        200,
    );
    assert!(fits_after_post
        .as_array()
        .expect("fits")
        .iter()
        .any(|f| f["fitting_id"] == json!(fit_id) && f["name"] == json!("ESI4 Rifter Fit")));

    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "DELETE",
            format!("/v1/characters/90000001/fittings/{fit_id}/"),
        )
        .with_bearer_token(&token),
        204,
    );

    let fits_after_del = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v2/characters/90000001/fittings/").with_bearer_token(&token),
        200,
    );
    assert!(!fits_after_del
        .as_array()
        .expect("fits")
        .iter()
        .any(|f| f["fitting_id"] == json!(fit_id)));

    // 6. Character & Corporation Assets + Locations + Names (+ Director/CEO 403 check)
    let char_assets = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v5/characters/90000001/assets/").with_bearer_token(&token),
        200,
    );
    assert!(char_assets
        .as_array()
        .expect("char assets")
        .iter()
        .any(|a| a["item_id"] == json!(bpo_item_id)));

    let live_ball_item_id = 880_012_i64;
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        snap.items.insert(
            live_ball_item_id,
            InventoryItemRecord {
                item_id: live_ball_item_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        Ok(())
    });
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    jita_sys
        .spawn_ball(eve_destiny::Ball {
            id: live_ball_item_id,
            type_id: 587,
            position: glam::DVec3::new(3400.0, -7100.0, 9200.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_067_000.0,
            radius: 38.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn live ball in Jita");

    let char_locs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v2/characters/90000001/assets/locations/")
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!([bpo_item_id, space_asset_id, live_ball_item_id]))
                    .unwrap(),
            ),
        200,
    );
    let loc_arr = char_locs.as_array().expect("char_locs array");
    let station_loc = loc_arr
        .iter()
        .find(|l| l["item_id"] == json!(bpo_item_id))
        .expect("station item location");
    assert_eq!(
        station_loc["position"],
        json!({ "x": 0.0, "y": 0.0, "z": 0.0 }),
        "Items inside stations must report (0, 0, 0) coordinates"
    );
    let space_loc = loc_arr
        .iter()
        .find(|l| l["item_id"] == json!(space_asset_id))
        .expect("in-space item location");
    assert_eq!(
        space_loc["position"],
        json!({ "x": 1250.0, "y": -2500.0, "z": 5000.0 }),
        "In-space deployed items must report persisted 3D coordinates"
    );
    let live_ball_loc = loc_arr
        .iter()
        .find(|l| l["item_id"] == json!(live_ball_item_id))
        .expect("live ballpark in-space item location");
    assert_eq!(
        live_ball_loc["position"],
        json!({ "x": 3400.0, "y": -7100.0, "z": 9200.0 }),
        "In-space items in the live ballpark must report their live ballpark coordinates"
    );

    let mut label_sess = eve_machonet::SessionState::new(42);
    label_sess.user_id = Some(1);
    label_sess.character_id = Some(90_000_001);
    label_sess.corporation_id = Some(corp_id);
    ctx.dispatch_rpc(
        &mut label_sess,
        "invbroker",
        None,
        "SetLabel",
        &[
            eve_proto::PyRep::Integer(space_asset_id),
            eve_proto::PyRep::String("Alpha Forward Depot".into()),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("invbroker.SetLabel character singleton asset");
    ctx.dispatch_rpc(
        &mut label_sess,
        "invbroker",
        None,
        "SetLabel",
        &[
            eve_proto::PyRep::Integer(corp_asset_id),
            eve_proto::PyRep::String("Apex Flagship Rifter".into()),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("invbroker.SetLabel corp singleton asset");

    let char_names = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/90000001/assets/names/")
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([space_asset_id, bpo_item_id])).unwrap()),
        200,
    );
    let char_names_arr = char_names.as_array().expect("char_names array");
    let depot_name = char_names_arr
        .iter()
        .find(|n| n["item_id"] == json!(space_asset_id))
        .expect("space_asset_id name");
    assert_eq!(depot_name["name"], json!("Alpha Forward Depot"));

    let corp_assets = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v5/corporations/{corp_id}/assets/"))
            .with_bearer_token(&token),
        200,
    );
    assert!(corp_assets
        .as_array()
        .expect("corp assets")
        .iter()
        .any(|a| a["item_id"] == json!(corp_asset_id)));

    // Non-CEO / non-director gets 403 on corporation assets
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v5/corporations/98000702/assets/").with_bearer_token(&token),
        403,
    );

    let corp_space_asset_id = 880_011_i64;
    let corp_locs = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new(
            "POST",
            format!("/v2/corporations/{corp_id}/assets/locations/"),
        )
        .with_bearer_token(&token)
        .with_body(serde_json::to_vec(&json!([corp_asset_id, corp_space_asset_id])).unwrap()),
        200,
    );
    let corp_locs_arr = corp_locs.as_array().expect("corp_locs array");
    let corp_hangar_loc = corp_locs_arr
        .iter()
        .find(|l| l["item_id"] == json!(corp_asset_id))
        .expect("corp hangar asset location");
    assert_eq!(
        corp_hangar_loc["position"],
        json!({ "x": 0.0, "y": 0.0, "z": 0.0 })
    );
    let corp_space_loc = corp_locs_arr
        .iter()
        .find(|l| l["item_id"] == json!(corp_space_asset_id))
        .expect("corp in-space asset location");
    assert_eq!(
        corp_space_loc["position"],
        json!({ "x": -4500.0, "y": 8250.0, "z": -1100.0 })
    );

    let corp_names = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", format!("/v1/corporations/{corp_id}/assets/names/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!([corp_asset_id, corp_space_asset_id])).unwrap()),
        200,
    );
    let corp_names_arr = corp_names.as_array().expect("corp_names array");
    let corp_custom_name = corp_names_arr
        .iter()
        .find(|n| n["item_id"] == json!(corp_asset_id))
        .expect("corp_asset_id custom name");
    assert_eq!(corp_custom_name["name"], json!("Apex Flagship Rifter"));
    let corp_default_name = corp_names_arr
        .iter()
        .find(|n| n["item_id"] == json!(corp_space_asset_id))
        .expect("corp_space_asset_id default name");
    assert_eq!(corp_default_name["name"], json!("Rifter"));

    // 7. Corporation Wallets (all 7 divisions) & Character Loyalty Points
    let corp_wallets = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/corporations/{corp_id}/wallets/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(corp_wallets.as_array().expect("divisions").len(), 7);
    let expected_div_balances = [
        (1, 250_000_000.0),
        (2, 15_000_000.0),
        (3, 12_500_000.0),
        (4, 9_750_000.0),
        (5, 8_200_000.0),
        (6, 5_100_000.0),
        (7, 3_500_000.0),
    ];
    for (idx, (div_num, expected_bal)) in expected_div_balances.into_iter().enumerate() {
        assert_eq!(corp_wallets[idx]["division"], json!(div_num));
        assert_eq!(corp_wallets[idx]["balance"], json!(expected_bal));
    }

    let lp = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/loyalty/points/")
            .with_bearer_token(&token),
        200,
    );
    assert!(lp
        .as_array()
        .expect("lp")
        .iter()
        .any(|e| e["corporation_id"] == json!(1_000_035) && e["loyalty_points"] == json!(12_500)));

    // 8. Mail CRUD (CSPA check -> POST -> GET list with labels/last_mail_id -> GET detail -> DELETE -> 404 for sender, still visible to recipient)
    // Recipient 90_000_002 requires 2,950 ISK CSPA charge; approved_cost=0 must be rejected with 400
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/90000001/mail/")
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "approved_cost": 0,
                    "subject": "ESI4 Unapproved CSPA",
                    "body": "Should fail CSPA check",
                    "recipients": [
                        { "recipient_id": 90_000_002, "recipient_type": "character" }
                    ]
                }))
                .unwrap(),
            ),
        400,
    );

    let sender_bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
    let mail_id_val = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/90000001/mail/")
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "approved_cost": 5000,
                    "subject": "ESI4 Fleet Ping",
                    "body": "Forming in Jita 4-4",
                    "recipients": [
                        { "recipient_id": 90_000_002, "recipient_type": "character" }
                    ]
                }))
                .unwrap(),
            ),
        201,
    );
    let mail_id = mail_id_val.as_i64().expect("created mail_id");
    let sender_bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        sender_bal_before.0 - sender_bal_after.0,
        IskCents::from_isk(2_950).0,
        "CSPA charge of 2,950 ISK must be debited from sender"
    );

    // Contact exemption: when recipient has sender in contacts (standing > -10.0 and not blocked), CSPA is exempt (0 ISK charge)
    ctx.esi_state
        .char_contacts
        .write()
        .unwrap()
        .entry(90_000_002)
        .or_default()
        .push(eve_server::esi::runtime_state::EsiContactEntry {
            contact_id: 90_000_001,
            contact_type: "character".to_string(),
            standing: 5.0,
            label_ids: vec![],
            is_watched: false,
            is_blocked: false,
        });

    let sender_bal_pre_contact = ctx.db.get_character(90_000_001).unwrap().balance;
    let _mail_id_contact = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("POST", "/v1/characters/90000001/mail/")
            .with_bearer_token(&token)
            .with_body(
                serde_json::to_vec(&json!({
                    "approved_cost": 0,
                    "subject": "Contact Exemption Test",
                    "body": "No CSPA charge for contacts",
                    "recipients": [
                        { "recipient_id": 90_000_002, "recipient_type": "character" }
                    ]
                }))
                .unwrap(),
            ),
        201,
    );
    let sender_bal_post_contact = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(
        sender_bal_pre_contact.0, sender_bal_post_contact.0,
        "Non-blocked contact must be exempt from CSPA charge (0 ISK deducted)"
    );

    let mail_list = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", "/v1/characters/90000001/mail/?labels=2")
            .with_bearer_token(&token),
        200,
    );
    let sent_entry = mail_list
        .as_array()
        .expect("mails")
        .iter()
        .find(|m| m["mail_id"] == json!(mail_id))
        .expect("sent mail in ?labels=2");
    assert_eq!(sent_entry["subject"], json!("ESI4 Fleet Ping"));
    assert!(sent_entry["timestamp"].as_str().is_some());
    assert_eq!(sent_entry["is_read"], json!(true));
    assert_eq!(sent_entry["labels"], json!([2]));

    let mail_detail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/90000001/mail/{mail_id}/"))
            .with_bearer_token(&token),
        200,
    );
    assert_eq!(mail_detail["subject"], json!("ESI4 Fleet Ping"));
    assert_eq!(mail_detail["body"], json!("Forming in Jita 4-4"));
    assert!(mail_detail["timestamp"].as_str().is_some());
    assert_eq!(mail_detail["read"], json!(true));
    assert_eq!(mail_detail["labels"], json!([2]));

    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("DELETE", format!("/v1/characters/90000001/mail/{mail_id}/"))
            .with_bearer_token(&token),
        204,
    );
    validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/90000001/mail/{mail_id}/"))
            .with_bearer_token(&token),
        404,
    );

    let recip_token = issue_token_with_scopes(
        &ctx,
        90_000_002,
        vec![
            "esi-mail.read_mail.v1".to_string(),
            "esi-mail.organize_mail.v1".to_string(),
        ],
    );
    let recip_mail = validate_and_json(
        &router,
        &spec,
        &EsiHttpRequest::new("GET", format!("/v1/characters/90000002/mail/{mail_id}/"))
            .with_bearer_token(&recip_token),
        200,
    );
    assert_eq!(recip_mail["subject"], json!("ESI4 Fleet Ping"));
    assert_eq!(recip_mail["labels"], json!([1]));
}
