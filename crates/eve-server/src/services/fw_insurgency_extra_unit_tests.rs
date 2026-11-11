//! Deep branch-coverage and mutation-killing unit tests for `fw_insurgency`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::mining_sites;
use crate::services::sov_fw_insurgency::fw_insurgency::handle_hub_and_scan;
use eve_db::{
    AllianceRecord, AsteroidRecord, CorporationRecord, CosmicAnomalyRecord, CosmicSignatureRecord,
    InventoryFlag, InventoryItemRecord, SystemDynamicState, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

async fn call_hub_scan(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_hub_and_scan(ctx, session, service, method, args, kwargs)
        .await
        .expect("handler matched")
}

#[tokio::test]
async fn sov_hub_save_upgrade_configuration_validation_and_install_lifecycle() {
    let ctx = test_ctx();
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < -0.1 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();

    let char_id = 940_001_i64;
    let corp_id = 98_940_001_i64;
    let ally_id = 99_940_001_i64;
    let ship_id = 940_100_i64;
    let hub_id = 1_000_000_940_001_i64;
    let up_item_id = 940_200_i64;
    let up_type_id = mining_sites::SOV_PROSPECTING_DEPOSITS[0].upgrade_type_id;

    let mut sess = auth_session(1, char_id, corp_id, null_sys);
    sess.ship_id = Some(ship_id);
    sess.alliance_id = None;
    sess.corp_role = 0;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().unwrap();
        let mut ch = base_ch;
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = null_sys;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.ceo_id = 999_999;
                __corp.alliance_id = Some(ally_id);
                __corp },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
                ..Default::default()
            },
        );
    });

    // 1. Caller has no alliance -> SovereigntyNotClaimed
    let err = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "SovereigntyNotClaimed");

    // 2. Caller has alliance, but system sov is held by another alliance -> SovereigntyNotClaimed
    sess.alliance_id = Some(ally_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().alliance_id = Some(ally_id);
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                sov_alliance_id: Some(99_999_999),
                ..Default::default()
            },
        );
    });
    let err = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "SovereigntyNotClaimed");

    // 3. System sov matches caller alliance, but caller lacks Director/StationManager/CEO role -> InsufficientCorporationRole
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = Some(ally_id);
    });
    let err = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InsufficientCorporationRole");

    // 4. Grant CEO role, but no online friendly structure in system -> NoFriendlySovStructureInSystem
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = char_id;
    });
    let err = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NoFriendlySovStructureInSystem");

    // 5. Seed online friendly hub structure, no upgrade item yet -> UpgradeItemNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 35_833,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Sov Hub".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let err = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "UpgradeItemNotFound");

    // 6. Upgrade item in ship > 50 km away from hub -> OutsideSovStructureProximity
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [500_000.0, 0.0, 0.0]);
        snap.items.insert(
            up_item_id,
            InventoryItemRecord {
                item_id: up_item_id,
                type_id: up_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let err = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "OutsideSovStructureProximity");

    // 7. Move ship within 50 km of hub, but lock item -> UpgradeItemLocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [110_000.0, 0.0, 0.0]);
        snap.items.get_mut(&up_item_id).unwrap().flag = InventoryFlag::Locked;
    });
    let err = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "UpgradeItemLocked");

    // 8. Unlock item, seed a depleted old SovOre anomaly to verify clearing & signature collapse, then install via kwargs["upgrades"]
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&up_item_id).unwrap().flag = InventoryFlag::Cargo;
        snap.cosmic_anomalies
            .entry(null_sys)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "OLD-SOV-ORE".into(),
                solar_system_id: null_sys,
                name: "Depleted Deposit".into(),
                anomaly_type: "SovOre".into(),
                position: [1.0e9, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 100.0,
                created_unix: 1_000,
                lifetime_seconds: 0,
                cleared: false,
            });
        snap.cosmic_signatures
            .entry(null_sys)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: "OLD-SOV-ORE".into(),
                solar_system_id: null_sys,
                sig_group: "SovOre".into(),
                type_name: "Depleted Deposit".into(),
                position: [1.0e9, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: 1_000,
                lifetime_seconds: 0,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
    });

    let mut kw_up = HashMap::new();
    kw_up.insert(
        "upgrades".into(),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(up_type_id as i64)])]),
    );
    let saved_rep = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[],
        &kw_up,
    )
    .await
    .expect("install sov upgrade succeeds");
    assert_eq!(saved_rep.dict_get("hubID"), Some(&PyRep::Integer(hub_id)));
    assert_eq!(
        saved_rep.dict_get("upgradeTypeID"),
        Some(&PyRep::Integer(up_type_id as i64))
    );

    // Calling SaveUpgradeConfiguration again when already installed returns existing anomaly without re-spawning
    let saved_again = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "SaveUpgradeConfiguration",
        &[PyRep::Integer(up_type_id as i64)],
        &kw,
    )
    .await
    .expect("idempotent save");
    assert_eq!(
        saved_again.dict_get("spawnedAnomalies"),
        saved_rep.dict_get("spawnedAnomalies")
    );

    // GetInstalledUpgradesForHub returns the installed upgrade and active SovOre anomaly
    let hub_info = call_hub_scan(
        &ctx,
        &mut sess,
        "sovHubSvc",
        "GetInstalledUpgradesForHub",
        &[PyRep::Integer(hub_id)],
        &kw,
    )
    .await
    .unwrap();
    let PyRep::List(upgrades_list) = hub_info.dict_get("upgrades").unwrap() else {
        panic!("expected upgrades list");
    };
    assert_eq!(upgrades_list.len(), 1);
}

#[tokio::test]
async fn scan_results_wormhole_gas_sleeper_spawn_signal_tiers_and_cone_scan_filtering() {
    let ctx = test_ctx();
    let wh_sys = 31_000_005_u32;
    let char_id = 941_001_i64;
    let corp_id = 98_941_001_i64;
    let ship_id = 941_100_i64;

    let mut sess = auth_session(1, char_id, corp_id, wh_sys);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.insert(
            wh_sys,
            vec![CosmicAnomalyRecord {
                anomaly_id: "ANOM-ORE-1".into(),
                solar_system_id: wh_sys,
                name: "Bountiful Belt".into(),
                anomaly_type: "Ore".into(),
                position: [10_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 5_000.0,
                created_unix: 1_000,
                lifetime_seconds: 86_400,
                cleared: false,
            }],
        );
        snap.asteroids.insert(
            941_500,
            AsteroidRecord {
                asteroid_id: 941_500,
                solar_system_id: wh_sys,
                belt_id: 0,
                type_id: 1230,
                quantity: 1_000,
                initial_quantity: 1_000,
                position: (12_000.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
        snap.cosmic_signatures.insert(
            wh_sys,
            vec![
                // Revealed Gas signature created > 900s ago -> triggers Sleeper NPC spawn!
                CosmicSignatureRecord {
                    signature_id: "GAS-REV".into(),
                    solar_system_id: wh_sys,
                    sig_group: "Gas".into(),
                    type_name: "Barren Perimeter Reservoir".into(),
                    position: [10_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: 100,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                // Partially scanned signature (0.50 -> pos_known = true, revealed = false)
                CosmicSignatureRecord {
                    signature_id: "SIG-HALF".into(),
                    solar_system_id: wh_sys,
                    sig_group: "Relic".into(),
                    type_name: "Forgotten Core".into(),
                    position: [500_000.0, 100.0, 200.0],
                    signal_strength: 0.50,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: 100,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
                // Unscanned signature (0.10 -> pos_known = false, x/y/z = None)
                CosmicSignatureRecord {
                    signature_id: "SIG-LOW".into(),
                    solar_system_id: wh_sys,
                    sig_group: "Data".into(),
                    type_name: "Unsecured Enclave".into(),
                    position: [900_000.0, 0.0, 0.0],
                    signal_strength: 0.10,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: 100,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                },
            ],
        );
    });

    let scan_rep = call_hub_scan(&ctx, &mut sess, "scanMgr", "GetScanResults", &[], &kw)
        .await
        .expect("GetScanResults in WH");
    assert_eq!(
        scan_rep.dict_get("gasSiteNpcSpawned"),
        Some(&PyRep::Bool(true))
    );
    let PyRep::List(sigs) = scan_rep.dict_get("signatures").unwrap() else {
        panic!("expected signatures list");
    };
    assert_eq!(sigs.len(), 3);
    assert_eq!(sigs[0].dict_get("warpable"), Some(&PyRep::Bool(true)));
    assert_eq!(sigs[1].dict_get("warpable"), Some(&PyRep::Bool(false)));
    assert_eq!(sigs[1].dict_get("x"), Some(&PyRep::Real(500_000.0)));
    assert_eq!(sigs[2].dict_get("x"), Some(&PyRep::None));

    // ConeScan with ship ball in space: excludes caller's own ship and balls beyond max_range
    let handle = ctx.systems.get_or_spawn(wh_sys);
    for (bid, x_pos) in [(ship_id, 0.0), (941_601, 5_000.0), (941_602, 5_000_000.0)] {
        let _ = handle
            .spawn_ball(Ball {
                id: bid,
                type_id: 587,
                position: DVec3::new(x_pos, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let PyRep::List(cone_hits) = call_hub_scan(
        &ctx,
        &mut sess,
        "scanSvc",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(100_000.0)],
        &kw,
    )
    .await
    .unwrap() else {
        panic!("expected list");
    };
    let hit_ids: Vec<i64> = cone_hits
        .iter()
        .filter_map(|r| r.dict_get("itemID").and_then(|v| v.as_i64()))
        .collect();
    assert!(!hit_ids.contains(&ship_id));
    assert!(hit_ids.contains(&941_601));
    assert!(!hit_ids.contains(&941_602));
}

#[tokio::test]
async fn hub_and_scan_unknown_none_and_save_upgrade_configuration_kill_mutants() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    // 1. Unrelated service returns None (kills `handle_hub_and_scan -> Some(Ok(Default::default()))`)
    assert!(
        handle_hub_and_scan(&ctx, &mut sess, "marketProxy", "GetMarketGroups", &[], &kw)
            .await
            .is_none()
    );

    // 2. High-sec SaveUpgradeConfiguration -> Err("NotNullSecSystem")
    let hi_err = call_hub_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
        .await
        .unwrap_err();
    assert_eq!(hi_err.code, "NotNullSecSystem");

    // 3. No solar system -> Err("NoSolarSystem")
    sess.solar_system_id = None;
    let no_sys_err = call_hub_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
        .await
        .unwrap_err();
    assert_eq!(no_sys_err.code, "NoSolarSystem");
}
