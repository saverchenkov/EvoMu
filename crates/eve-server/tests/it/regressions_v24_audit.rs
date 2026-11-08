//! Integration tests across V24 sov skyhooks,
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
                last_damage_unix: 0, self_destruct_at_unix: None,
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
                last_damage_unix: 0, self_destruct_at_unix: None,
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

    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    if let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        let p = [ship_ball.position.x, ship_ball.position.y, ship_ball.position.z];
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(s) = snap.structures.get_mut(&ihub_id) {
                s.position = [p[0] + 1000.0, p[1], p[2]];
            }
        });
    }

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
    let cur_sys = 30_000_002u32; // Lashesih (0.8 high-sec)

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

    // 3. Dynamic abyssal weather application in pocket (feat:pve.abyssal-weather) without polluting item dynamic_attributes (#584)
    let base_armor = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&265)).copied().unwrap_or(0.0);
    let boosted_armor = ctx
        .systems
        .get_or_spawn(pocket_sys_id as u32)
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .map(|cs| cs.max_armor_hp)
        .unwrap_or(0.0);
    assert!(
        boosted_armor > base_armor,
        "Weather must boost armor HP dynamically in abyssal pocket ({boosted_armor} > {base_armor})"
    );
    assert!(
        ctx.db.with_snapshot(|s| s.items.get(&ship_id).and_then(|it| it.dynamic_attributes.get(&265)).is_none()),
        "Declarative abyssal weather must not mutate item dynamic_attributes[ARMOR_HP] (#584)"
    );
}


pub(crate) mod ess_fighters_and_structure_ops_audit {
    #[allow(unused_imports)]
    use super::*;

// Integration tests across ESS reserve pulses,
// fighters launching/abilities, and structure ops/asset safety.

use std::collections::HashMap;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
};
use eve_destiny::{Ball, CombatState, MovementMode};
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
async fn test_ess_reserve_pulses_audit() {
    // parity: rpc:essMgr.RequestUnlockReserveBank
    eve_testkit::attest(&["rpc:essMgr.RequestUnlockReserveBank"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid = ctx.db.provision_player_account("ess_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "ESS Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;

    let sys_id = 30_004_759u32;
    let ship_id = 910_000_104i64;
    let key_item_id = 920_000_104i64;
    let key_type_id = 57721u32; // 45-minute SW key for Delve (30_004_759)
    let ess_ball_id = 930_000_104i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
        snap.system_states.insert(
            sys_id,
            eve_db::SystemDynamicState {
                solar_system_id: sys_id,
                ess_reserve_bank: IskCents::from_isk(100_000_000),
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            key_item_id,
            InventoryItemRecord {
                item_id: key_item_id,
                type_id: key_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = setup_pilot(&ctx, 1004, "ess_tester", "sec123", char_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = make_ball(ship_id, 587, DVec3::new(100.0, 0.0, 0.0));
    sys_handle.spawn_ball(ship_ball).await.expect("spawn ship ball");

    let ess_ball = make_ball(ess_ball_id, 56002, DVec3::new(0.0, 0.0, 0.0));
    sys_handle
        .spawn_ball_with_combat(ess_ball, CombatState::default())
        .await
        .expect("spawn ess ball");

    // Call RequestUnlockReserveBank
    let unlock_res = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "RequestUnlockReserveBank",
            &[PyRep::Integer(key_type_id as i64)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("RequestUnlockReserveBank");

    assert_eq!(
        unlock_res.dict_get("unlocked").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        unlock_res.dict_get("durationMinutes").and_then(|v| v.as_i64()),
        Some(45)
    );

    let has_key = ctx.db.with_snapshot(|s| s.items.contains_key(&key_item_id));
    assert!(!has_key, "Key must be consumed upon unlocking reserve bank");

    // Verify pulses in GetDataForClientSolarSystem
    let ess_data = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "GetDataForClientSolarSystem",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetDataForClientSolarSystem");

    let pulses_rem = ess_data
        .dict_get("reserveBankPulsesRemaining")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let pulses_total = ess_data
        .dict_get("reserveBankPulsesTotal")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert_eq!(pulses_total, 45, "45-minute key gives 45 pulses");
    assert_eq!(pulses_rem, 45);
    assert!(ess_data.dict_get("reserveBankLastPulseInitiated").is_some());
}

#[tokio::test]
async fn test_fighters_tubes_and_multi_activation_audit() {
    // parity: rpc:fighterMgr.LaunchFightersFromTubes, rpc:fighterMgr.CmdActivateAbilitySlots
    eve_testkit::attest(&[
        "rpc:fighterMgr.LaunchFightersFromTubes",
        "rpc:fighterMgr.CmdActivateAbilitySlots",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid = ctx.db.provision_player_account("fighter_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "Fighter Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;

    let sys_id = 30_000_142u32;
    let carrier_id = 910_000_105i64;
    let squadron_id = 920_000_105i64;
    let fighter_type_id = 23057u32; // Dragonfly I Light Fighter

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = carrier_id;
        }
        snap.items.insert(
            carrier_id,
            InventoryItemRecord {
                item_id: carrier_id,
                type_id: 23757, // Archon Carrier
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            squadron_id,
            InventoryItemRecord {
                item_id: squadron_id,
                type_id: fighter_type_id,
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Load squadron into tube 0
    ctx.fighter_tube_loads.write().unwrap().insert((carrier_id, 0), squadron_id);

    let mut sess = setup_pilot(&ctx, 1005, "fighter_tester", "sec123", char_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = carrier_id;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(carrier_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let carrier_ball = make_ball(carrier_id, 23757, DVec3::new(0.0, 0.0, 0.0));
    sys_handle.spawn_ball(carrier_ball).await.expect("spawn carrier ball");

    // 1. LaunchFightersFromTubes with tube flag 159
    let launch_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(159)])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("LaunchFightersFromTubes");

    assert!(
        matches!(launch_res, PyRep::Dict(_)),
        "Flag launching must return PyRep::Dict"
    );

    let item_loc = ctx.db.with_snapshot(|s| s.items.get(&squadron_id).map(|it| it.location_id));
    assert_eq!(
        item_loc,
        Some(sys_id as i64),
        "Fighter item must have moved into space"
    );

    // 2. CmdActivateAbilitySlots fighter ability activation
    let fighter_ball_id = ctx
        .active_fighters
        .read()
        .unwrap()
        .keys()
        .copied()
        .next()
        .expect("active fighter ball");

    let act_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(fighter_ball_id)]),
                PyRep::Integer(1),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots");

    assert_eq!(
        act_res.dict_get("success").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn test_structure_ops_and_asset_safety_audit() {
    // parity: rpc:structureDeployment.Anchor, rpc:structureControl.TakeControl, rpc:structureAssetSafety.GetStructuresICanDeliverTo, feat:structures.asset-safety-manual, feat:lifecycle.asset_safety-auto_delivery
    eve_testkit::attest(&[
        "rpc:structureDeployment.Anchor",
        "rpc:structureControl.TakeControl",
        "rpc:structureAssetSafety.GetStructuresICanDeliverTo",
        "feat:structures.asset-safety-manual",
        "feat:lifecycle.asset_safety-auto_delivery",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let corp_id = 98_000_106i64;
    let uid = ctx.db.provision_player_account("struct_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "Struct Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;

    let sys_id = 30_000_144u32;
    let ship_id = 910_000_106i64;
    let astrahus_item_id = 920_000_106i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.corporation_id = corp_id;
            c.balance = IskCents::from_f64_isk(1_000_000_000.0);
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Struct Corp".into(),
                ticker: "STRCT".into(),
                ceo_id: char_id,
                alliance_id: None,
                member_roles: HashMap::from([(
                    char_id,
                    corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER,
                )]),
                ..Default::default()
            },
        );
        snap.system_states.insert(
            sys_id,
            eve_db::SystemDynamicState {
                solar_system_id: sys_id,
                ess_reserve_bank: IskCents::from_isk(100_000_000),
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            astrahus_item_id,
            InventoryItemRecord {
                item_id: astrahus_item_id,
                type_id: 35832, // Astrahus Citadels (group 1657)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = setup_pilot(&ctx, 1006, "struct_tester", "sec123", char_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.active_ship_id = ship_id;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(corp_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = make_ball(ship_id, 587, DVec3::new(0.0, 5.0e9, 0.0));
    sys_handle.spawn_ball(ship_ball).await.expect("spawn ship ball");

    // 1. structureDeployment.Anchor with V24 client tuple
    // (cargo_item_id, [x,y,z], profile_id, (weekday, hour), name)
    let anchor_args = vec![
        PyRep::Integer(astrahus_item_id),
        PyRep::List(vec![
            PyRep::Real(60_000.0),
            PyRep::Real(5.0e9),
            PyRep::Real(0.0),
        ]),
        PyRep::Integer(1),
        PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(18)]),
        PyRep::String("Alpha Astrahus".into()),
    ];
    let anchor_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &anchor_args,
            &HashMap::new(),
            None,
        )
        .await
        .expect("structureDeployment.Anchor");

    let structure_id = anchor_res
        .dict_get("structureID")
        .and_then(|v| v.as_i64())
        .expect("structureID");
    assert!(structure_id > 0);

    let has_astrahus_item = ctx.db.with_snapshot(|s| s.items.contains_key(&astrahus_item_id));
    assert!(!has_astrahus_item, "Deployed item must be consumed from cargo");

    // 2. structureControl.TakeControl
    let take_ctrl_undocked = ctx
        .dispatch_rpc(
            &mut sess,
            "structureControl",
            None,
            "TakeControl",
            &[PyRep::Integer(structure_id)],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(take_ctrl_undocked.is_err(), "TakeControl must fail when not docked");

    sess.structure_id = Some(structure_id);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(structure_id);
            ch.station_id = None;
        }
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.state = "ShieldVulnerable".into();
            st.quantum_core_installed = true;
        }
    });
    let take_ctrl_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureControl",
            None,
            "TakeControl",
            &[PyRep::Integer(structure_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("structureControl.TakeControl while docked with defense role");

    assert_eq!(
        sess.ship_id,
        Some(structure_id),
        "Pilot ship ID becomes structure ID upon taking control"
    );
    assert_eq!(
        take_ctrl_res.dict_get("success").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 3. structureAssetSafety.GetStructuresICanDeliverTo
    let deliver_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureAssetSafety",
            None,
            "GetStructuresICanDeliverTo",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetStructuresICanDeliverTo");
    assert!(matches!(deliver_res, PyRep::List(_)));

    // 4. MoveSafetyWrapToStructure with same-system 0.5% fee (feat:structures.asset-safety-manual)
    let wrap_id = 940_000_106i64;
    let wrapped_item_id = 950_000_106i64;
    let initial_balance = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wrapped_item_id,
            InventoryItemRecord {
                item_id: wrapped_item_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 0,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            wrap_id,
            eve_server::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                wrap_id,
                owner_id: char_id,
                is_corp: false,
                source_structure_id: structure_id,
                source_structure_name: "Test Structure".into(),
                solar_system_id: sys_id,
                created_unix: ctx.now_unix() - 100,
                deliverable_after_unix: ctx.now_unix() - 10,
                delivered_to_id: None,
                estimated_value_isk: 10_000_000.0,
                item_ids: vec![wrapped_item_id],
                nearest_npc_station_id: Some(60003760),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        let auto_wrap_id = 940_000_107i64;
        let auto_item_id = 950_000_107i64;
        snap.items.insert(
            auto_item_id,
            InventoryItemRecord {
                item_id: auto_item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: 0,
                flag: InventoryFlag::None,
                quantity: 50,
                ..Default::default()
            },
        );
        ext.asset_safety_wraps.insert(
            auto_wrap_id,
            eve_server::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord {
                wrap_id: auto_wrap_id,
                owner_id: char_id,
                is_corp: false,
                source_structure_id: structure_id,
                source_structure_name: "Test Structure".into(),
                solar_system_id: sys_id,
                created_unix: ctx.now_unix() - (21 * 86_400),
                deliverable_after_unix: ctx.now_unix() - (15 * 86_400),
                delivered_to_id: None,
                estimated_value_isk: 500_000.0,
                item_ids: vec![auto_item_id],
                nearest_npc_station_id: Some(60003760),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    let move_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureAssetSafety",
            None,
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(wrap_id), PyRep::Integer(structure_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("MoveSafetyWrapToStructure");

    // feat:lifecycle.asset_safety-auto_delivery: wrap older than 20 days is lazily auto-delivered
    let auto_del = ctx.db.with_snapshot(|snap| {
        let ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.asset_safety_wraps.get(&940_000_107i64).map(|w| (w.auto_delivered, w.delivered_to_id))
    });
    assert_eq!(auto_del, Some((true, Some(60003760))));

    let fee_paid = move_res
        .dict_get("feePaidISK")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        (fee_paid - 50_000.0).abs() < 1.0,
        "Expected 0.5% fee (50,000 ISK), got {fee_paid}"
    );

    let post_balance = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    assert_eq!(
        initial_balance.0 - post_balance.0,
        5_000_000,
        "50,000.00 ISK in cents"
    );
}

#[tokio::test]
async fn playtest_batch_i_autofit_slash_notifications_and_dogma_get_all_info() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid = ctx.db.provision_player_account("batch_i_tester", "sec123");
    let char_id = ctx
        .db
        .create_character(uid, "BatchI Tester", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;
    let rifter_id = 945_000_100_i64;
    let station_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        let st = ch.station_id.expect("starter station");
        ch.active_ship_id = rifter_id;
        snap.items.insert(
            rifter_id,
            InventoryItemRecord {
                item_id: rifter_id,
                type_id: 587, // Rifter (3 Hi, 3 Med, 3 Lo)
                owner_id: char_id,
                location_id: st,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        st
    });
    let mut sess = setup_pilot(&ctx, 77_001, "batch_i_tester", "sec123", char_id).await;
    sess.role |= eve_machonet::service_roles::ROLE_GM_MASK;
    let kw = HashMap::new();
    let ship_id = rifter_id;

    // Clear initial login notifications
    let _ = ctx.drain_notifications_for_char(char_id);

    // #648: /createitem <module> 1 creates singleton module and emits OnItemChange
    let created = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/createitem 484 1".into())],
            &kw,
            None,
        )
        .await
        .expect("/createitem");
    let gun_id = created.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert!(ctx.db.with_snapshot(|s| s.items[&gun_id].singleton));
    let item_notifs: Vec<String> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| n.event)
        .collect();
    assert!(
        item_notifs.iter().any(|e| e == "OnItemChange"),
        "expected OnItemChange from /createitem, got {item_notifs:?}"
    );

    // #647: Train turret skill so gun fits online, bind ship inventory via invbroker, and call Add with mismatched args[1] and flag=0 (flagAutoFit)
    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/giveskill 3302 5".into())],
        &kw,
        None,
    )
    .await
    .expect("/giveskill 3302");
    let bind = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject");
    let PyRep::Tuple(parts) = &bind else { panic!("MachoBindObject 2-tuple") };
    let mgr = parts[0].as_i64().expect("mgr bound id") as u64;
    let ship_inv = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr),
            "GetInventoryFromId",
            &[PyRep::Integer(ship_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("GetInventoryFromId(ship)");
    let ship_bid = ship_inv.as_i64().expect("ship bound id") as u64;

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(ship_bid),
        "Add",
        &[
            PyRep::Integer(gun_id),
            PyRep::Integer(station_id + 123),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("autofit bound Add");
    ctx.db.with_snapshot(|snap| {
        let g = &snap.items[&gun_id];
        assert_eq!(g.location_id, ship_id);
        assert_eq!(g.flag, InventoryFlag::HiSlot0);
    });

    // #648: Train Hull Upgrades 4 and /fit DCU II (2048); verify online=true and notifications emitted
    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/giveskill 3394 4".into())],
        &kw,
        None,
    )
    .await
    .expect("/giveskill 3394");
    let _ = ctx.drain_notifications_for_char(char_id);
    let fit_dcu = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/fit 2048".into())],
            &kw,
            None,
        )
        .await
        .expect("/fit 2048");
    let dcu_id = fit_dcu.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(fit_dcu.dict_get("online"), Some(&PyRep::Bool(true)));
    let fit_notifs: Vec<String> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| n.event)
        .collect();
    assert!(fit_notifs.iter().any(|e| e == "OnItemChange"));
    assert!(fit_notifs.iter().any(|e| e == "OnModuleFitted"));
    assert!(fit_notifs.iter().any(|e| e == "OnModuleAttributeChanges"));
    assert!(fit_notifs.iter().any(|e| e == "OnGodmaShipEffect"));

    // #649 + #651: Remove turret skills (if any) so gun_id is offline, then call dogmaIM.GetAllInfo
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&3301);
            sk.remove(&3302);
        }
        snap.module_online_states.insert(gun_id, false);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let all_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.GetAllInfo");
    let PyRep::Dict(ship_info) = all_info.dict_get("shipInfo").expect("shipInfo") else {
        panic!("shipInfo dict");
    };
    assert!(
        ship_info.iter().any(|(k, _)| k.as_i64() == Some(gun_id)),
        "offline fitted gun must still be present in shipInfo"
    );
    assert!(
        ship_info.iter().any(|(k, _)| k.as_i64() == Some(dcu_id)),
        "online DCU must be present in shipInfo"
    );
    let (_, ship_entry) = ship_info
        .iter()
        .find(|(k, _)| k.as_i64() == Some(ship_id))
        .expect("ship entry");
    let PyRep::Dict(attrs) = ship_entry.dict_get("attributes").expect("attributes") else {
        panic!("attributes dict");
    };
    let cpu_load = attrs
        .iter()
        .find(|(k, _)| k.as_i64() == Some(eve_dogma::attr_ids::CPU_LOAD as i64))
        .and_then(|(_, v)| v.as_f64())
        .unwrap();
    let pg_load = attrs
        .iter()
        .find(|(k, _)| k.as_i64() == Some(eve_dogma::attr_ids::POWER_LOAD as i64))
        .and_then(|(_, v)| v.as_f64())
        .unwrap();
    assert_eq!(cpu_load, 0.0, "GetAllInfo shipInfo.attributes CPU_LOAD must be 0.0");
    assert_eq!(pg_load, 0.0, "GetAllInfo shipInfo.attributes POWER_LOAD must be 0.0");
}

#[tokio::test]
async fn inventory_autofit_slash_fit_undock_dock_and_mining_ledger_regressions() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let uid = ctx.db.provision_player_account("v24audit724", "pass");
    let char_id = ctx
        .db
        .create_character(uid, "Audit Pilot 724", 1, 1, 11, 17, &ctx.sde)
        .expect("create_character")
        .character_id;
    let ship_id = 200_724_001_i64;

    // Seed character with Venture (32880: 3 hiSlots, 2 turret hardpoints) and full mining/gunnery skills.
    let ship_tid = if ctx.sde.types.contains_key(&32880) && ctx.sde.types.contains_key(&483) {
        32880_u32
    } else {
        587_u32
    };
    let turret_tid = if ship_tid == 32880 { 483_u32 } else { 484_u32 };
    let max_turrets = ctx
        .sde
        .types
        .get(&ship_tid)
        .map(|t| {
            let hi = t.default_attributes.get(&eve_sde::attr_ids::HI_SLOTS).copied().unwrap_or(0.0) as usize;
            let tur = t.default_attributes.get(&eve_sde::attr_ids::TURRET_SLOTS_LEFT).copied().unwrap_or(0.0) as usize;
            hi.min(tur)
        })
        .unwrap_or(2);

    let (station_id, sys_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        let st = ch.station_id.expect("starter station");
        let sys = ch.solar_system_id;
        ch.active_ship_id = ship_id;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_tid,
                owner_id: char_id,
                location_id: st,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for sk in [3300_u32, 3301, 3302, 3386, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                eve_db::TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        (st, sys)
    });

    let mut sess = setup_pilot(&ctx, 724_001, "v24audit724", "pass", char_id).await;
    sess.role |= eve_machonet::service_roles::ROLE_GM_MASK;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    let kw = HashMap::new();

    // 1. /createitem two unpackaged turrets & stack operations never emit ixQuantity (5)
    let created = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/createitem {turret_tid} 1"))],
            &kw,
            None,
        )
        .await
        .expect("/createitem turret 1");
    let turret1_id = created.dict_get("itemID").and_then(PyRep::as_i64).unwrap();
    let created_stack = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/createitem {turret_tid} 2"))],
            &kw,
            None,
        )
        .await
        .expect("/createitem turret stack");
    let stack_id = created_stack.dict_get("itemID").and_then(PyRep::as_i64).unwrap();

    let bind_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject invbroker");
    let PyRep::Tuple(bind_parts) = &bind_rep else {
        panic!("expected MachoBindObject 2-tuple");
    };
    let mgr_bid = bind_parts[0].as_i64().expect("invbroker mgr bound id") as u64;

    let ship_inv = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr_bid),
            "GetInventoryFromId",
            &[PyRep::Integer(ship_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("bind ship inv");
    let ship_bid = ship_inv.as_i64().expect("ship bound id") as u64;
    let hangar_inv = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr_bid),
            "GetInventory",
            &[PyRep::Integer(10_004), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("bind station hangar inv");
    let hangar_bid = hangar_inv.as_i64().expect("station hangar bound id") as u64;

    let _ = ctx.drain_notifications_for_char(char_id);
    // Fit first unpackaged qty=1 turret via MultiAdd flagAutoFit (0)
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(ship_bid),
        "MultiAdd",
        &[
            PyRep::List(vec![PyRep::Integer(turret1_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(0),
        ],
        &kw,
        None,
    )
    .await
    .expect("MultiAdd autofit unpackaged turret");

    let n_multi = ctx.drain_notifications_for_char(char_id);
    assert!(
        n_multi
            .iter()
            .filter(|n| n.event == "OnItemChange")
            .all(|n| match &n.args[1] {
                PyRep::Dict(d) => !d.iter().any(|(k, _)| k.as_i64() == Some(5)),
                _ => true,
            }),
        "MultiAdd OnItemChange must never include key 5 (ixQuantity)"
    );
    assert!(
        n_multi.iter().any(|n| {
            n.event == "OnModuleAttributeChanges"
                && matches!(n.args.first(), Some(PyRep::List(items)) if items.iter().any(|it| {
                    matches!(
                        it,
                        PyRep::Tuple(t)
                            if t.get(2).and_then(PyRep::as_i64) == Some(turret1_id)
                            && t.get(3).and_then(PyRep::as_i64) == Some(2)
                            && t.get(5).and_then(PyRep::as_f64) == Some(1.0)
                    )
                }))
        }),
        "MultiAdd autofit must emit OnModuleAttributeChanges (online=1.0)"
    );
    assert!(
        n_multi.iter().any(|n| n.event == "OnGodmaShipEffect" && n.args.get(1).and_then(PyRep::as_i64) == Some(16)),
        "MultiAdd autofit must emit OnGodmaShipEffect(16)"
    );

    // Split + StackAll on station hangar stack must use ixStackSize (9), never ixQuantity (5)
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(hangar_bid),
        "SplitStack",
        &[PyRep::Integer(stack_id), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("SplitStack ok");
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(hangar_bid),
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
        &kw,
        None,
    )
    .await
    .expect("StackAll ok");
    let n_stack = ctx.drain_notifications_for_char(char_id);
    for n in n_stack.iter().filter(|n| n.event == "OnItemChange") {
        if let PyRep::Dict(d) = &n.args[1] {
            assert!(!d.iter().any(|(k, _)| k.as_i64() == Some(5)));
        }
    }

    // 2. Fill remaining turret hardpoints via /fit, verify extra turret fails cleanly without creating item
    for _ in 1..max_turrets {
        let fit_res = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(format!("/fit me {turret_tid}"))],
                &kw,
                None,
            )
            .await
            .expect("/fit turret within limit");
        assert_eq!(fit_res.dict_get("online"), Some(&PyRep::Bool(true)));
    }
    let over_err = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/fit me {turret_tid}"))],
            &kw,
            None,
        )
        .await
        .expect_err("3rd turret /fit beyond hull hardpoints must fail");
    assert_eq!(over_err.code, "FittingValidationFailed");

    // 3. Undock -> GetAllInfo -> Dock keeps ship modules present & synced for both godma & dogmaLocation
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let all_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("GetAllInfo in space");
    let PyRep::Dict(ship_info) = all_info.dict_get("shipInfo").expect("shipInfo") else {
        panic!("shipInfo dict");
    };
    assert!(
        ship_info.iter().any(|(k, _)| k.as_i64() == Some(turret1_id)),
        "fitted module must be present in GetAllInfo.shipInfo"
    );

    // 4. corpMiningLedger.GetObserverLedger(None) returns [], /spawn asteroid sets ownerID=1, /unspawn clears snap.asteroids
    let obs_ledger = ctx
        .dispatch_rpc(
            &mut sess,
            "corpMiningLedger",
            None,
            "GetObserverLedger",
            &[PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("GetObserverLedger(None)");
    assert_eq!(obs_ledger, PyRep::List(vec![]));

    let ast_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 25)
        .map(|t| t.type_id)
        .unwrap_or(1228);
    let spawned_ast = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/spawn {ast_tid}"))],
            &kw,
            None,
        )
        .await
        .expect("/spawn asteroid");
    let ast_id = spawned_ast.dict_get("spawnedID").and_then(PyRep::as_i64).unwrap();
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&ast_id].owner_id, 1);
        assert!(s.asteroids.contains_key(&ast_id));
    });
    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String(format!("/unspawn {ast_id}"))],
        &kw,
        None,
    )
    .await
    .expect("/unspawn asteroid");
    ctx.db.with_snapshot(|s| {
        assert!(!s.asteroids.contains_key(&ast_id));
    });

    // Re-dock at starter station and confirm GetAllInfo keeps fitted modules synced (#699).
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, char_id);
    ctx.dispatch_rpc(
        &mut sess,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(station_id)],
        &kw,
        None,
    )
    .await
    .expect("Dock back at station");
    let post_dock_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("GetAllInfo after dock");
    let PyRep::Dict(dock_ship_info) = post_dock_info.dict_get("shipInfo").expect("shipInfo") else {
        panic!("shipInfo dict after dock");
    };
    assert!(
        dock_ship_info.iter().any(|(k, _)| k.as_i64() == Some(turret1_id)),
        "fitted module must remain synced after Undock -> Dock"
    );
}
}
