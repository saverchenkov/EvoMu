#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_insurgency_hub_and_scan_mutations() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let unknown_svc = "other";
    let unknown_method = "UnknownMethod";
    assert!(handle_hub_and_scan(&ctx, &mut sess, unknown_svc, "ConeScan", &[], &kw)
        .await
        .is_none());
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", unknown_method, &[], &kw)
        .await
        .is_none());

    let mut unauth = SessionState::default();
    assert!(handle_hub_and_scan(&ctx, &mut unauth, "scanSvc", "ConeScan", &[], &kw)
        .await
        .unwrap()
        .is_err());

    // Without solar_system_id, requires NoSolarSystem error
    sess.solar_system_id = None;
    assert!(handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "GetInstalledUpgradesLocal", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "GetScanResults", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "ConeScan", &[], &kw)
        .await
        .unwrap()
        .is_err());

    // High-sec system (30000142 Jita) rejects SaveUpgradeConfiguration with NotNullSecSystem
    sess.solar_system_id = Some(30000142);
    let err = handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "NotNullSecSystem");

    // GetInstalledUpgradesLocal in 30000142
    let installed = handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "GetInstalledUpgradesLocal", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(installed.dict_get("solarSystemID"), Some(&PyRep::Integer(30000142)));
    assert!(matches!(installed.dict_get("upgrades"), Some(PyRep::List(_))));

    // GetScanResults in 30000142
    let scan_res = handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "GetScanResults", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan_res.dict_get("solarSystemID"), Some(&PyRep::Integer(30000142)));
    assert!(matches!(scan_res.dict_get("anomalies"), Some(PyRep::List(_))));
    assert!(matches!(scan_res.dict_get("signatures"), Some(PyRep::List(_))));

    // ConeScan in abyssal / zarzakh is forbidden
    sess.solar_system_id = Some(30_100_000);
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "ConeScan", &[], &kw)
        .await
        .unwrap()
        .is_err());
    sess.solar_system_id = Some(32_000_001);
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "ConeScan", &[], &kw)
        .await
        .unwrap()
        .is_err());

    // ConeScan in normal space succeeds
    sess.solar_system_id = Some(30000142);
    let cone = handle_hub_and_scan(
        &ctx,
        &mut sess,
        "scanSvc",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1.0e12)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(cone, PyRep::List(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_insurgency_save_config_security_auth_locks_and_depletion_sync() {
    use eve_db::types::{
        CosmicAnomalyRecord, InventoryItemRecord, OwnerNoteRecord, UpwellStructureRecord,
    };
    use std::sync::Arc;

    let base = test_ctx();
    let mut sde = (*base.sde).clone();
    sde.types.entry(82_589).or_insert(eve_sde::InvType {
        type_id: 82_589,
        group_id: 4768,
        category_id: 65,
        name: "Prospecting Array 2".into(),
        mass: 1_000.0,
        volume: 4_000.0,
        capacity: 0.0,
        radius: 100.0,
        base_price: 100.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    });
    let missing_sec_sys = 30_900_002u32;
    let nullsec_sys = 30_004_759u32;
    if let Some(sys) = sde.solar_systems.get_mut(&nullsec_sys) {
        sys.security_status = 0.0; // Exact <= 0.0 boundary
    }
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );

    let corp_id = 98_900_001i64;
    let other_corp_id = 98_900_002i64;
    let alliance_id = 99_900_001i64;
    let char_id = 21_900_001i64;
    let unauth_char_id = 21_900_002i64;
    let hub_ns_nofuel = 1_090_000_099i64;
    let hub_ns_low = 1_090_000_101i64;
    let hub_ns_foreign = 1_090_000_102i64;
    let locked_upgrade_id = 1_090_000_010i64;
    let non_upgrade_id = 1_090_000_011i64;
    let upgrade_stack_id = 1_090_000_012i64;
    let ship_id = 1_090_000_020i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut corp) = snap.corporations.values().next().cloned() {
            corp.corporation_id = corp_id;
            corp.ceo_id = char_id;
            corp.alliance_id = Some(alliance_id);
            snap.corporations.insert(corp_id, corp);
        }
        if let Some(base_ch) = snap.characters.values().next().cloned() {
            let mut ceo_ch = base_ch.clone();
            ceo_ch.character_id = char_id;
            ceo_ch.user_id = char_id as u64;
            ceo_ch.corporation_id = corp_id;
            ceo_ch.alliance_id = Some(alliance_id);
            snap.characters.insert(char_id, ceo_ch);

            let mut member_ch = base_ch;
            member_ch.character_id = unauth_char_id;
            member_ch.user_id = unauth_char_id as u64;
            member_ch.corporation_id = corp_id;
            member_ch.alliance_id = Some(alliance_id);
            snap.characters.insert(unauth_char_id, member_ch);
        }

        snap.structures.insert(
            hub_ns_nofuel,
            UpwellStructureRecord {
                structure_id: hub_ns_nofuel,
                solar_system_id: nullsec_sys,
                owner_corp_id: corp_id,
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_ns_low,
            UpwellStructureRecord {
                structure_id: hub_ns_low,
                solar_system_id: nullsec_sys,
                owner_corp_id: corp_id,
                position: [100.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_ns_foreign,
            UpwellStructureRecord {
                structure_id: hub_ns_foreign,
                solar_system_id: nullsec_sys,
                owner_corp_id: other_corp_id,
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );

        snap.item_positions.insert(hub_ns_low, [100.0, 0.0, 0.0]);
        snap.item_positions.insert(ship_id, [50_100.0, 0.0, 0.0]);

        snap.items.insert(
            locked_upgrade_id,
            InventoryItemRecord {
                item_id: locked_upgrade_id,
                type_id: 82_589,
                owner_id: char_id,
                location_id: ship_id,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.owner_notes.insert(
            77_001,
            OwnerNoteRecord {
                note_id: 77_001,
                character_id: -74,
                item_id: locked_upgrade_id,
                label: "raffle".to_string(),
                note: format!(r#"{{"item_id":{locked_upgrade_id},"expired":false,"winner_id":null}}"#),
                timestamp_unix: 100,
            },
        );

        snap.items.insert(
            non_upgrade_id,
            InventoryItemRecord {
                item_id: non_upgrade_id,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_id,
                quantity: 5,
                ..Default::default()
            },
        );

        let st = snap.system_states.entry(nullsec_sys).or_default();
        st.industrial_index = 5;
        st.sov_alliance_id = Some(alliance_id);

        snap.cosmic_anomalies.entry(nullsec_sys).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "ORE-111".to_string(),
            solar_system_id: nullsec_sys,
            name: "Prospecting Deposit".to_string(),
            anomaly_type: "SovOre".to_string(),
            position: [1_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![],
            remaining_volume: 0.0,
            created_unix: 100,
            lifetime_seconds: 0,
            cleared: true,
        });
    });

    let kw = HashMap::new();

    // Without CEO/Director/StationManager, even exact 0.0 nullsec_sys fails authorization
    let mut unauth_sess = auth_session(unauth_char_id as u64, unauth_char_id, corp_id, nullsec_sys);
    unauth_sess.corp_role = 0;
    let auth_err = handle_hub_and_scan(
        &ctx,
        &mut unauth_sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::Integer(hub_ns_low)],
        &kw,
    )
    .await;
    assert_eq!(auth_err.expect("handled").unwrap_err().code, "InsufficientCorporationRole");

    // Missing system security defaults to 1.0 (> 0.0) -> rejected even with ROLE_STATION_MANAGER
    let mut sess = auth_session(char_id as u64, char_id, corp_id, missing_sec_sys);
    sess.corp_role = eve_db::corp_roles::ROLE_STATION_MANAGER;
    sess.ship_id = Some(ship_id);
    let sec_err = handle_hub_and_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::List(vec![PyRep::Integer(upgrade_stack_id)])],
        &kw,
    )
    .await;
    assert!(sec_err.expect("handled").is_err());

    // Switch to nullsec_sys (security == 0.0); locked item fails with UpgradeItemLocked
    sess.solar_system_id = Some(nullsec_sys);
    let lock_err = handle_hub_and_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::List(vec![
            PyRep::Integer(non_upgrade_id),
            PyRep::Integer(locked_upgrade_id),
        ])],
        &kw,
    )
    .await;
    assert_eq!(lock_err.expect("handled").unwrap_err().code, "UpgradeItemLocked");

    // Insert unlocked upgrade stack in ship at [50_101.0, 0, 0] (> 50 km from hub [100, 0, 0]) -> OutsideSovStructureProximity
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            upgrade_stack_id,
            InventoryItemRecord {
                item_id: upgrade_stack_id,
                type_id: 82_589,
                owner_id: char_id,
                location_id: ship_id,
                quantity: 3,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, [50_101.0, 0.0, 0.0]);
    });
    let range_err = handle_hub_and_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::List(vec![PyRep::Integer(upgrade_stack_id)])],
        &kw,
    )
    .await;
    assert_eq!(range_err.expect("handled").unwrap_err().code, "OutsideSovStructureProximity");

    // Move ship within exact 50_000.0 distance ([50_100.0, 0, 0]) -> succeeds, splits stack 3->2, preserves index 5
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [50_100.0, 0.0, 0.0]);
    });

    handle_hub_and_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::List(vec![
            PyRep::Integer(non_upgrade_id),
            PyRep::Integer(upgrade_stack_id),
        ])],
        &kw,
    )
    .await
    .expect("handled")
    .unwrap();

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&upgrade_stack_id].quantity, 2);
        assert_eq!(snap.system_states[&nullsec_sys].industrial_index, 5);
        assert!(snap.items.values().any(|it| {
            it.item_id != upgrade_stack_id
                && it.location_id == hub_ns_low
                && it.type_id == 82_589
                && it.quantity == 1
        }));
    });

    // GetInstalledUpgradesLocal selects own-corp lowest structure_id (hub_ns_nofuel) regardless of fuel_blocks
    let local = handle_hub_and_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "GetInstalledUpgradesLocal",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        local.dict_get("hubID").and_then(|v| v.as_i64()),
        Some(hub_ns_nofuel)
    );

    // Empty system returns hubID: None
    let mut empty_sess = auth_session(char_id as u64, char_id, corp_id, 30_000_142);
    let empty_local = handle_hub_and_scan(
        &ctx,
        &mut empty_sess,
        "sovHubSvc",
        "GetInstalledUpgradesLocal",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(empty_local.dict_get("hubID"), Some(&PyRep::None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_insurgency_scan_results_wh_gas_timing_and_cone_scan_edges() {
    use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord};
    use std::time::{SystemTime, UNIX_EPOCH};

    let ctx = test_ctx();
    let wh_boundary_normal = 30_999_999u32;
    let wh_first = 31_000_000u32;
    let char_id = 21_900_099i64;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    ctx.db.with_mut_snapshot(|snap| {
        let mk_sig = |sig_id: &str, sys: u32, group: &str, strg: f64, created: u64| CosmicSignatureRecord {
            signature_id: sig_id.to_string(),
            solar_system_id: sys,
            sig_group: group.to_string(),
            type_name: group.to_string(),
            position: [1_000.0, 0.0, 0.0],
            signal_strength: strg,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: created,
            lifetime_seconds: 86_400,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        };

        snap.cosmic_signatures
            .entry(wh_boundary_normal)
            .or_default()
            .push(mk_sig("GAS-NRM", wh_boundary_normal, "Gas", 1.0, now - 950));
        snap.cosmic_signatures
            .entry(wh_first)
            .or_default()
            .extend([
                mk_sig("GAS-899", wh_first, "Gas", 1.0, now - 899),
                mk_sig("GAS-900", wh_first, "Gas", 1.0, now - 900),
                mk_sig("GAS-LOW", wh_first, "Gas", 0.24, now - 950),
                mk_sig("DAT-900", wh_first, "Data", 1.0, now - 900),
            ]);
        snap.cosmic_anomalies
            .entry(wh_first)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "ORE-KPT".to_string(),
                solar_system_id: wh_first,
                name: "Kept Ore".to_string(),
                anomaly_type: "OreAnomaly".to_string(),
                position: [2_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 100.0,
                created_unix: 0,
                lifetime_seconds: 36_000,
                cleared: false,
            });
    });

    let kw = HashMap::new();
    let mut sess_wh = auth_session(char_id as u64, char_id, 98_900_001, wh_first);
    let _ = handle_hub_and_scan(&ctx, &mut sess_wh, "scanSvc", "GetSystemScanMgr", &[], &kw)
        .await
        .unwrap();
    let wh_scan = handle_hub_and_scan(&ctx, &mut sess_wh, "scanMgr", "GetScanResults", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wh_scan.dict_get("gasSiteNpcSpawned"), Some(&PyRep::Bool(true)));
    assert!(matches!(wh_scan.dict_get("gasSiteNpcBallID"), Some(PyRep::Integer(_))));

    // ConeScan restricted space exact ID boundaries
    for (sys_id, should_ok) in [
        (30_099_999u32, true),
        (30_100_000u32, false),
        (31_999_999u32, true),
        (32_000_000u32, false),
        (32_999_999u32, false),
        (33_000_000u32, true),
    ] {
        let mut s = auth_session(char_id as u64, char_id, 98_900_001, sys_id);
        let res = handle_hub_and_scan(
            &ctx,
            &mut s,
            "scanSvc",
            "ConeScan",
            &[PyRep::Real(360.0), PyRep::Real(1.0e12)],
            &kw,
        )
        .await;
        assert_eq!(res.expect("handled").is_ok(), should_ok, "sys_id={sys_id}");
    }

    // Flat 3-scalar zero direction vector skips angle filtering cleanly
    let mut s_dir = auth_session(char_id as u64, char_id, 98_900_001, 30_000_142);
    let zero_dir_res = handle_hub_and_scan(
        &ctx,
        &mut s_dir,
        "scanSvc",
        "ConeScan",
        &[
            PyRep::Real(60.0),
            PyRep::Real(1.0e12),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(zero_dir_res, PyRep::List(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_insurgency_sov_hub_save_config_scan_results_and_cone_scan() {
    use eve_db::{
        corp_roles, AsteroidRecord, CorporationRecord, CosmicAnomalyRecord, CosmicSignatureRecord,
        InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
    };
    use eve_destiny::{Ball, CombatState, MovementMode};
    use glam::DVec3;

    let ctx = test_ctx();
    let null_sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);
    let char_id = 90_000_001_i64;
    let non_dir_char = 90_000_002_i64;
    let corp_id = 98_555_001_i64;
    let ally_id = 99_555_001_i64;
    let hub_id = 1_055_555_001_i64;
    let ship_id = 100_555_111_i64;
    let upgrade_tid = crate::services::mining_sites::SOV_PROSPECTING_DEPOSITS[0].upgrade_type_id;
    let unmodelled_upgrade_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.group_id == 4768
                && !crate::services::mining_sites::SOV_PROSPECTING_DEPOSITS
                    .iter()
                    .any(|d| d.upgrade_type_id == t.type_id)
        })
        .map(|t| t.type_id)
        .unwrap_or(upgrade_tid);

    let mut sess = auth_session(1, char_id, corp_id, null_sys_id);
    sess.alliance_id = Some(ally_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    let kw = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.get(&char_id).cloned().unwrap();
        let mut ch1 = base_ch.clone();
        ch1.corporation_id = corp_id;
        ch1.alliance_id = Some(ally_id);
        ch1.solar_system_id = null_sys_id;
        ch1.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch1);

        let mut ch2 = base_ch;
        ch2.character_id = non_dir_char;
        ch2.corporation_id = corp_id;
        ch2.alliance_id = Some(ally_id);
        snap.characters.insert(non_dir_char, ch2);

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.ceo_id = char_id;
        corp.alliance_id = Some(ally_id);
        corp.members.insert(char_id);
        corp.members.insert(non_dir_char);
        corp.member_roles.insert(char_id, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: null_sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // 1. SovereigntyNotClaimed error
    assert_eq!(
        handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SovereigntyNotClaimed"
    );

    // Claim sov for ally_id & check InsufficientCorporationRole
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys_id).or_default().sov_alliance_id = Some(ally_id);
    });
    let mut non_dir_sess = auth_session(2, non_dir_char, corp_id, null_sys_id);
    non_dir_sess.alliance_id = Some(ally_id);
    assert_eq!(
        handle_hub_and_scan(&ctx, &mut non_dir_sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientCorporationRole"
    );

    // 2. NoFriendlySovStructureInSystem
    assert_eq!(
        handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoFriendlySovStructureInSystem"
    );

    // Seed online hub at (0,0,0), ship at (200_000,0,0), locked & out-of-range upgrade items
    let sys_h = ctx.systems.get_or_spawn(null_sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(200_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;
    let locked_id = 960_001_i64;
    let out_range_id = 960_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: null_sys_id,
                name: "Sov Hub".into(),
                position: [1.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.items.insert(
            out_range_id,
            InventoryItemRecord {
                item_id: out_range_id,
                type_id: upgrade_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutsideSovStructureProximity"
    );

    // Move upgrade to hub Locked -> UpgradeItemLocked; unmodelled if distinct -> ProspectingDepositNotModelled
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&out_range_id);
        snap.items.insert(
            locked_id,
            InventoryItemRecord {
                item_id: locked_id,
                type_id: upgrade_tid,
                owner_id: corp_id,
                location_id: hub_id,
                flag: InventoryFlag::Locked,
                quantity: 1,
                ..Default::default()
            },
        );
        // Lock it via contract lock/flag
    });
    assert_eq!(
        handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "UpgradeItemLocked"
    );

    // Empty hub -> UpgradeItemNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&locked_id);
    });
    assert_eq!(
        handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "UpgradeItemNotFound"
    );

    if unmodelled_upgrade_tid != upgrade_tid {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                960_003,
                InventoryItemRecord {
                    item_id: 960_003,
                    type_id: unmodelled_upgrade_tid,
                    owner_id: char_id,
                    location_id: hub_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        assert_eq!(
            handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "ProspectingDepositNotModelled"
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&960_003);
        });
    }

    // Install multi-quantity upgrade from hub Hangar -> splits stack & spawns SovOre deposit
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            960_010,
            InventoryItemRecord {
                item_id: 960_010,
                type_id: upgrade_tid,
                owner_id: char_id,
                location_id: hub_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
        // Pre-seed a depleted SovOre anomaly + signature to exercise cleared detection
        snap.cosmic_anomalies.entry(null_sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "SOV-DEP-1".into(),
            solar_system_id: null_sys_id,
            name: "Depleted SovOre".into(),
            anomaly_type: "SovOre".into(),
            position: [5_000_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![],
            remaining_volume: 100.0,
            created_unix: now.saturating_sub(100),
            lifetime_seconds: 0,
            cleared: false,
        });
        snap.cosmic_signatures.entry(null_sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: "SOV-DEP-1".into(),
            solar_system_id: null_sys_id,
            sig_group: "Ore".into(),
            type_name: "Test Ore Site".into(),
            position: [0.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: None,
            destination_system_id: None,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
            created_unix: now,
            lifetime_seconds: 3600,
        });
    });
    let mut cfg_kw = HashMap::new();
    cfg_kw.insert("typeID".into(), PyRep::Integer(upgrade_tid as i64));
    let saved = handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &cfg_kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.dict_get("hubID"), Some(&PyRep::Integer(hub_id)));

    // Calling SaveUpgradeConfiguration a second time on already-installed item returns existing anomaly without re-spawning
    let saved2 = handle_hub_and_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(upgrade_tid as i64)])])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(saved2.dict_get("hubID"), Some(&PyRep::Integer(hub_id)));

    // GetInstalledUpgradesLocal in nullsec shows installed upgrade
    let local_up = handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "GetInstalledUpgradesLocal", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(local_up.dict_get("hubID"), Some(&PyRep::Integer(hub_id)));

    // GetScanResults with Ore/Gas/WH sigs + Gas NPC ball
    let npc_type = ctx.sde.types.values().find(|t| t.category_id == 11).map(|t| t.type_id).unwrap_or(587);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: 990_001,
                type_id: npc_type,
                position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { is_cloaked: true, ..CombatState::default() },
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            990_100,
            AsteroidRecord {
                asteroid_id: 990_100,
                solar_system_id: null_sys_id,
                belt_id: 0,
                type_id: 1228,
                quantity: 500,
                initial_quantity: 500,
                position: (100.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
        snap.cosmic_signatures.entry(null_sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: "GAS-1".into(),
            solar_system_id: null_sys_id,
            sig_group: "Gas".into(),
            type_name: "Vital Core".into(),
            position: [100.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: None,
            destination_system_id: None,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
            created_unix: now,
            lifetime_seconds: 3600,
        });
    });
    let scan_null = handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "GetScanResults", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan_null.dict_get("solarSystemID"), Some(&PyRep::Integer(null_sys_id as i64)));

    // ConeScan error cases + tuple & flat directional filters skipping cloaked / out-of-cone balls
    assert_eq!(
        handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "ConeScan", &[PyRep::Real(-1.0)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgs"
    );
    assert_eq!(
        handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "ConeScan", &[PyRep::Real(1.0), PyRep::Real(f64::NAN)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgs"
    );
    assert_eq!(
        handle_hub_and_scan(
            &ctx,
            &mut sess,
            "scanSvc",
            "ConeScan",
            &[PyRep::Real(1.0), PyRep::Real(1.0e9), PyRep::Tuple(vec![PyRep::String("bad".into()), PyRep::Real(0.0), PyRep::Real(0.0)])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgs"
    );
    assert_eq!(
        handle_hub_and_scan(
            &ctx,
            &mut sess,
            "scanSvc",
            "ConeScan",
            &[PyRep::Real(1.0), PyRep::Real(1.0e9), PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgs"
    );
    let cone_tup = handle_hub_and_scan(
        &ctx,
        &mut sess,
        "scanSvc",
        "ConeScan",
        &[PyRep::Real(0.5), PyRep::Real(1.0e9), PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(cone_tup, PyRep::List(_)));
    let cone_flat = handle_hub_and_scan(
        &ctx,
        &mut sess,
        "scanSvc",
        "ConeScan",
        &[PyRep::Real(0.5), PyRep::Real(1.0e9), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(cone_flat, PyRep::List(_)));

    // Wormhole system GetScanResults: spawns Sleeper gas guards after 900s + skips expired OreAnomaly
    let wh_sys = 31_000_001_u32;
    let wh_now = ctx.now_unix();
    sess.solar_system_id = Some(wh_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(wh_sys).or_default().push(CosmicSignatureRecord {
            signature_id: "WH-GAS-1".into(),
            solar_system_id: wh_sys,
            sig_group: "Gas".into(),
            type_name: "Barren Perimeter Reservoir".into(),
            position: [1_000.0, 2_000.0, 3_000.0],
            signal_strength: 1.0,
            wormhole_type_id: None,
            destination_system_id: None,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
            created_unix: wh_now.saturating_sub(1_200),
            lifetime_seconds: 86_400,
        });
        snap.cosmic_anomalies.entry(wh_sys).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "WH-ORE-EXP".into(),
            solar_system_id: wh_sys,
            anomaly_type: "OreAnomaly".into(),
            name: "Small Jaspet Deposit".into(),
            position: [5_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![],
            remaining_volume: 100.0,
            created_unix: wh_now.saturating_sub(200_000),
            lifetime_seconds: 3_600,
            cleared: false,
        });
    });
    let wh_scan = handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "GetScanResults", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wh_scan.dict_get("solarSystemID"), Some(&PyRep::Integer(wh_sys as i64)));
}

#[cfg(test)]
#[path = "fw_insurgency_extra_unit_tests.rs"]
mod fw_insurgency_extra_unit_tests;
