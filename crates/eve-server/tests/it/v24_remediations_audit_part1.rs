//! Integration tests (part 1) remediating auditor-flagged rows across V24 sov skyhooks,
//! FW occupation, system interference decay, and abyssal trace/weather.

use std::collections::HashMap;
use eve_db::{
    corp_roles, CorporationRecord, FwSystemOccupationRecord, InventoryFlag,
    InventoryItemRecord, SystemDynamicState, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use crate::common;

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    user_name: &str,
    secret: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user_name, secret).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn as_list<'a>(rep: &'a PyRep) -> &'a [PyRep] {
    match rep {
        PyRep::List(l) => l.as_slice(),
        _ => panic!("Expected PyRep::List, got {rep:?}"),
    }
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 1000.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn test_sov_skyhooks_and_structures_audit() {
    // parity: rpc:sovMgr.AcquireSkyhooks, rpc:sovMgr.GetSovStructuresInfoForLocalSolarSystem, rpc:colonyResourcesMgr.GetMyCorporationSkyhooks, rpc:insurgencySolarsystem.GetAllVisibleCampaigns
    eve_testkit::attest(&[
        "rpc:sovMgr.AcquireSkyhooks",
        "rpc:sovMgr.GetSovStructuresInfoForLocalSolarSystem",
        "rpc:colonyResourcesMgr.GetMyCorporationSkyhooks",
        "rpc:insurgencySolarsystem.GetAllVisibleCampaigns",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let null_sys = 30_004_759u32;
    let corp_id = 98_000_101i64;
    let alliance_id = 99_000_101i64;

    let uid = ctx.db.provision_player_account("sov_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "Sov Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;

    let ship_id = 910_000_101i64;
    let skyhook_id = 920_000_101i64;
    let ihub_id = 920_000_102i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = null_sys;
            c.corporation_id = corp_id;
            c.alliance_id = Some(alliance_id);
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Audit Corp".into(),
                ticker: "AUDIT".into(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                member_roles: HashMap::from([(char_id, corp_roles::ROLE_DIRECTOR)]),
                ..Default::default()
            },
        );
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                sov_alliance_id: Some(alliance_id),
                last_sov_claim_tick: 100,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: null_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: 81080,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Test Skyhook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.structures.insert(
            ihub_id,
            UpwellStructureRecord {
                structure_id: ihub_id,
                type_id: 32458,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Test IHub".into(),
                position: [1000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    eve_server::services::sov_services::set_skyhook_reagents(
        &ctx,
        skyhook_id,
        eve_server::services::sov_services::TYPE_MAGMATIC_GAS,
        250,
    );

    let mut sess = setup_pilot(&ctx, 1001, "sov_tester", "sec123", char_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = null_sys;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
    });
    sess.solar_system_id = Some(null_sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);

    // 1. AcquireSkyhooks (read-only verification)
    let initial_claim_tick = ctx.db.with_snapshot(|s| {
        s.system_states.get(&null_sys).map(|st| st.last_sov_claim_tick)
    });
    let skyhook_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "AcquireSkyhooks",
            &[PyRep::Integer(null_sys as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("sovMgr.AcquireSkyhooks");

    let post_claim_tick = ctx.db.with_snapshot(|s| {
        s.system_states.get(&null_sys).map(|st| st.last_sov_claim_tick)
    });
    assert_eq!(
        initial_claim_tick, post_claim_tick,
        "AcquireSkyhooks must be strictly read-only"
    );

    let sk_rep = skyhook_res.dict_get("skyhooks").expect("skyhooks list");
    let list = as_list(sk_rep);
    assert!(list.iter().any(|item| {
        item.dict_get("skyhookID").and_then(|v| v.as_i64()) == Some(skyhook_id)
    }));

    // 2. GetSovStructuresInfoForLocalSolarSystem (filters to TCU/IHub only)
    let sov_info = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSovStructuresInfoForLocalSolarSystem",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("sovMgr.GetSovStructuresInfoForLocalSolarSystem");

    let structs_rep = sov_info.dict_get("structures").expect("structures dict in sov info");
    let structs_list = as_list(structs_rep);
    assert_eq!(
        structs_list.len(),
        1,
        "Only IHub (32458) should be in sov structures, not skyhook (81080)"
    );
    assert_eq!(
        structs_list[0].dict_get("structureID").and_then(|v| v.as_i64()),
        Some(ihub_id)
    );
    assert_eq!(
        sov_info.dict_get("infrastructureHubID").and_then(|v| v.as_i64()),
        Some(ihub_id)
    );

    // 3. colonyResourcesMgr.GetMyCorporationSkyhooks
    let my_skyhooks = ctx
        .dispatch_rpc(
            &mut sess,
            "colonyResourcesMgr",
            None,
            "GetMyCorporationSkyhooks",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("colonyResourcesMgr.GetMyCorporationSkyhooks");

    let sk_list = as_list(&my_skyhooks);
    assert_eq!(sk_list.len(), 1);
    assert_eq!(
        sk_list[0].dict_get("skyhookID").and_then(|v| v.as_i64()),
        Some(skyhook_id)
    );
    assert_eq!(
        sk_list[0].dict_get("reagentQuantity").and_then(|v| v.as_i64()),
        Some(250)
    );

    // 4. GetAllVisibleCampaigns
    let camps = ctx
        .dispatch_rpc(
            &mut sess,
            "insurgencySolarsystem",
            None,
            "GetAllVisibleCampaigns",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("insurgencySolarsystem.GetAllVisibleCampaigns");
    assert!(matches!(camps, PyRep::List(_)));
}

#[tokio::test]
async fn test_fw_occupation_and_interference_decay_audit() {
    // parity: rpc:fwWarzoneSolarsystem.GetLocalOccupationState, rpc:solarsystemInterferenceMgr.GetLocalInterferenceState
    eve_testkit::attest(&[
        "rpc:fwWarzoneSolarsystem.GetLocalOccupationState",
        "rpc:solarsystemInterferenceMgr.GetLocalInterferenceState",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid = ctx.db.provision_player_account("fw_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "FW Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;

    let mut sess = setup_pilot(&ctx, 1002, "fw_tester", "sec123", char_id).await;

    // 1. fwWarzoneSolarsystem.GetLocalOccupationState outside warzone
    let non_wz_sys = 30_000_142u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = non_wz_sys;
            c.station_id = None;
        }
    });
    sess.solar_system_id = Some(non_wz_sys);
    sess.station_id = None;
    sess.structure_id = None;

    let occ_outside = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "GetLocalOccupationState",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("fwWarzoneSolarsystem outside warzone");
    match occ_outside {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].as_i64(), Some(non_wz_sys as i64));
            assert_eq!(items[1], PyRep::None, "Non-warzone system must return None state");
        }
        _ => panic!("Expected Tuple, got {occ_outside:?}"),
    }

    // Inside warzone system with occupier
    let wz_sys = 30_002_084u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = wz_sys;
        }
        snap.fw_system_occupations.insert(
            wz_sys,
            FwSystemOccupationRecord {
                solar_system_id: wz_sys,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_004,
                victory_points: 100,
                victory_points_threshold: 1000,
            },
        );
    });
    sess.solar_system_id = Some(wz_sys);
    let occ_inside = ctx
        .dispatch_rpc(
            &mut sess,
            "fwWarzoneSolarsystem",
            None,
            "GetLocalOccupationState",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("fwWarzoneSolarsystem inside warzone");
    match occ_inside {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].as_i64(), Some(wz_sys as i64));
            let dict = &items[1];
            assert_eq!(dict.dict_get("occupierID").and_then(|v| v.as_i64()), Some(500_004));
            assert!(dict.dict_get("frontlineStatus").is_some());
            assert!(dict.dict_get("contestedPercentage").is_some());
        }
        _ => panic!("Expected Tuple, got {occ_inside:?}"),
    }

    // 2. solarsystemInterferenceMgr.GetLocalInterferenceState with linear decay
    eve_server::services::pve_special_sites::record_system_interference_activity(
        &ctx, &mut sess, wz_sys, 0.85,
    );
    let int_state_high = ctx
        .dispatch_rpc(
            &mut sess,
            "solarsystemInterferenceMgr",
            None,
            "GetLocalInterferenceState",
            &[PyRep::Integer(wz_sys as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetLocalInterferenceState high");

    let level_high = int_state_high
        .dict_get("interferenceLevel")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let band_high = int_state_high
        .dict_get("interferenceBand")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(level_high >= 0.70, "Level {level_high} should be >= 0.70");
    assert_eq!(band_high, 3, "Band should be High (3)");

    ctx.advance_secs(600);

    let int_state_decayed = ctx
        .dispatch_rpc(
            &mut sess,
            "solarsystemInterferenceMgr",
            None,
            "GetLocalInterferenceState",
            &[PyRep::Integer(wz_sys as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetLocalInterferenceState decayed");
    let level_decayed = int_state_decayed
        .dict_get("interferenceLevel")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        level_decayed < level_high,
        "Interference level must have decayed from {level_high} to {level_decayed}"
    );
}

#[tokio::test]
async fn test_abyssal_deployment_and_weather_audit() {
    // parity: rpc:abyssalMgr.AbyssalEntranceDeployment, feat:pve.abyssal-pocket, feat:pve.abyssal-weather
    eve_testkit::attest(&[
        "rpc:abyssalMgr.AbyssalEntranceDeployment",
        "feat:pve.abyssal-pocket",
        "feat:pve.abyssal-weather",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid = ctx.db.provision_player_account("abyssal_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "Abyssal Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;

    let ship_id = 910_000_103i64;
    let filament_item_id = 920_000_103i64;
    let filament_type_id = 47763u32; // Calm Firestorm Filament (tier 1)
    let cur_sys = 30_000_142u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = cur_sys;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 621, // Caracal Cruiser (group 26)
                owner_id: char_id,
                location_id: cur_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            filament_item_id,
            InventoryItemRecord {
                item_id: filament_item_id,
                type_id: filament_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = setup_pilot(&ctx, 1003, "abyssal_tester", "sec123", char_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = cur_sys;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
    });
    sess.solar_system_id = Some(cur_sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(cur_sys);
    let ship_ball = make_ball(ship_id, 621, DVec3::ZERO);
    sys_handle.spawn_ball(ship_ball).await.expect("spawn ship ball");

    // 1. AbyssalEntranceDeployment: spawns trace without moving ship
    let deploy_res = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(filament_item_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("AbyssalEntranceDeployment");

    let trace_id = deploy_res
        .dict_get("traceBallID")
        .and_then(|v| v.as_i64())
        .expect("traceBallID");
    assert_eq!(
        sess.solar_system_id,
        Some(cur_sys),
        "Pilot remains in initial system after deploying trace"
    );

    let item_in_cargo = ctx.db.with_snapshot(|s| s.items.contains_key(&filament_item_id));
    assert!(!item_in_cargo, "Filament must be consumed upon deployment");

    // 2. AbyssalEntranceGateActivation enters pocket (feat:pve.abyssal-pocket)
    let gate_res = ctx
        .dispatch_rpc(
            &mut sess,
            "abyssalMgr",
            None,
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(trace_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("AbyssalEntranceGateActivation");

    let pocket_sys_id = gate_res
        .dict_get("solarSystemID")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(
        (32_000_000..=32_999_999).contains(&pocket_sys_id),
        "Entered pocket solar system {pocket_sys_id}"
    );

    // 3. Dynamic abyssal weather application in pocket (feat:pve.abyssal-weather)
    let mut attrs: HashMap<u16, f64> = HashMap::from([(265u16, 1000.0), (110u16, 0.5)]);
    eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(
        &ctx,
        ship_id,
        &mut attrs,
    );
    assert!(
        attrs[&265] > 1000.0,
        "Weather must boost armor HP dynamically in abyssal pocket"
    );
}
