//! Deep branch-coverage and mutation-killing unit tests for `space_destiny.rs`
//! and its submodules (`space_warp_jump.rs`, `space_cyno_filaments.rs`, `space_docking_cloaking.rs`).

#![forbid(unsafe_code)]

use super::space_destiny::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_item, make_fighter_type,
};
use eve_db::{BookmarkRecord, InventoryFlag, IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystem, StargateJump};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(721);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s.role = role;
    s
}

fn bound_sys(sys_id: u32) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 721,
        user_id: 1,
        character_id: 90_000_001,
        service: "beyonce".to_string(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    }
}

fn mk_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn ess_reserve_bank_blob_helpers_unlock_with_key_and_unlink_branches() {
    let ctx = test_ctx();
    let (char_id, ship_id) = (90_000_001_i64, 100_000_001_i64);
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), true, 1_700_000_000);
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.5 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap();
    let mut sess = make_session(char_id, 98_000_001, Some(null_sys), Some(ship_id), service_roles::ROLE_PLAYER);
    let empty_kw = HashMap::new();

    // 1. get_ess_reserve_pulses / set_ess_reserve_pulses roundtrip
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(get_ess_reserve_pulses(snap, null_sys).pulses_remaining, 0);
        let st = EssReservePulseState {
            pulses_total: 15,
            pulses_remaining: 9,
            last_pulse_sec: Some(123),
            reserve_linked_character_id: Some(char_id),
            reserve_link_started_tick: Some(5),
            link_started_unix: Some(100),
            unlocked_until_unix: 9999,
        };
        set_ess_reserve_pulses(snap, null_sys, &st);
        let loaded = get_ess_reserve_pulses(snap, null_sys);
        assert_eq!((loaded.pulses_remaining, loaded.pulses_total), (9, 15));
        assert_eq!(loaded.reserve_linked_character_id, Some(char_id));
    });

    // 2. RequestReserveBankUnlink clears reserve link & system_state.ess_linked_character_id and emits OnReserveBankPlayerLinkDisconnected
    ctx.db.with_mut_snapshot(|snap| {
        let ss = snap.system_states.entry(null_sys).or_default();
        ss.ess_linked_character_id = Some(char_id);
        ss.ess_link_started_tick = Some(10);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let unlink_res = handle(&ctx, &mut sess, None, "essMgr", "RequestReserveBankUnlink", &[], &empty_kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unlink_res, PyRep::Bool(true));
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnReserveBankPlayerLinkDisconnected")
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(get_ess_reserve_pulses(snap, null_sys).reserve_linked_character_id, None);
        assert_eq!(snap.system_states[&null_sys].ess_linked_character_id, None);
    });

    // 3. RequestUnlockReserveBank success path: consumes 1 reserve key from ship cargo and sets pulses
    ctx.db.with_mut_snapshot(|snap| {
        set_ess_reserve_pulses(snap, null_sys, &EssReservePulseState::default());
        let ss = snap.system_states.entry(null_sys).or_default();
        ss.ess_reserve_bank = IskCents::from_isk(50_000_000);
        for (idx, ktid) in [57_714_u32, 57_716, 57_717, 57_720].into_iter().enumerate() {
            let iid = 880_001 + idx as i64;
            snap.items.insert(
                iid,
                make_fighter_item(iid, ktid, char_id, ship_id, InventoryFlag::Cargo, 2),
            );
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle.spawn_ball(mk_ball(820_000_000 + null_sys as i64, 32_882, DVec3::ZERO)).await;
    let _ = sys_handle.spawn_ball(mk_ball(ship_id, 587, DVec3::new(1000.0, 0.0, 0.0))).await;

    let unlock_res = handle(
        &ctx, &mut sess, None, "essMgr", "RequestUnlockReserveBank", &[], &empty_kw,
    ).await.unwrap().unwrap();
    assert_eq!(unlock_res.dict_get("unlocked"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let pst = get_ess_reserve_pulses(snap, null_sys);
        assert_eq!((pst.pulses_total, pst.pulses_remaining), (15, 15));
    });
}

#[tokio::test]
async fn session_change_timer_ticks_invuln_and_warp_speed_multiplier_calculations() {
    let mut ctx = test_ctx();
    let (char_id, sys_id, other_sys) = (90_000_001_i64, 30_000_142_u32, 30_002_187_u32);
    let now = ctx.now_unix();

    // 1. record_session_change_timer("jump_drive") grants 60s location-change invulnerability
    record_session_change_timer(&ctx, char_id, now, sys_id, 10, "jump_drive");
    assert!(is_location_change_invulnerable(&ctx, char_id, now + 5));
    assert!(check_session_change_timer_active(&ctx, char_id, now + 1, Some((sys_id, 19))).is_some());
    assert!(check_session_change_timer_active(&ctx, char_id, now + 1, Some((sys_id, 20))).is_none());
    assert!(check_session_change_timer_active(&ctx, char_id, now + 1, Some((other_sys, 25))).is_some());
    assert!(check_session_change_timer_active(&ctx, char_id, now + 10, None).is_none());

    clear_session_change_timer(&ctx, char_id);
    assert!(check_session_change_timer_active(&ctx, char_id, now, None).is_none());
    assert!(!is_location_change_invulnerable(&ctx, char_id, now));

    // 2. record_client_session_check_timer sets timer without granting location-change invuln
    record_client_session_check_timer(&ctx, char_id, now, sys_id, 5, "set_account_key");
    assert!(!is_location_change_invulnerable(&ctx, char_id, now));
    assert!(check_session_change_timer_active(&ctx, char_id, now + 1, None).is_some());
    clear_session_change_timer(&ctx, char_id);

    // 3. compute_ship_warp_speed_au_s & compute_pilot_warp_speed_au_s with attr 1281, WARP_SPEED_MULTIPLIER, attr 2789
    let custom_ship_tid = 991_001_u32;
    Arc::make_mut(&mut ctx.sde).types.insert(
        custom_ship_tid,
        make_fighter_type(
            custom_ship_tid,
            25,
            6,
            1_000_000.0,
            vec![(1281, 4.0), (eve_sde::attr_ids::WARP_SPEED_MULTIPLIER, 1.5), (2789, 25.0)],
        ),
    );
    assert!((compute_ship_warp_speed_au_s(&ctx, custom_ship_tid) - 7.5).abs() < 1e-6);
    assert!((compute_pilot_warp_speed_au_s(&ctx, 999_999, sys_id, custom_ship_tid) - 7.5).abs() < 1e-6);
}

#[tokio::test]
async fn cmd_warp_to_stuff_bookmarks_cloak_scram_distance_and_capacitor_truncation() {
    let ctx = test_ctx();
    let (char_id, ship_id, sys_id) = (90_000_001_i64, 100_000_001_i64, 30_000_142_u32);
    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER);
    let bnd = bound_sys(sys_id);
    let empty_kw = HashMap::new();

    // 1. ShipNotInSpace when ballpark has no ship ball
    let err_no_ball = handle(
        &ctx, &mut sess, Some(&bnd), "beyonce", "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(60_003_760)], &empty_kw,
    ).await.unwrap().unwrap_err();
    assert_eq!(err_no_ball.code, "ShipNotInSpace");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(mk_ball(ship_id, 587, DVec3::ZERO), CombatState::default())
        .await;

    // 2. Non-finite min_range rejected with InvalidArgument; self-warp rejected with InvalidTarget
    let nan_kw = HashMap::from([("minRange".to_string(), PyRep::Real(f64::NAN))]);
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(60_003_760)], &nan_kw)
            .await.unwrap().unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ship_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "InvalidTarget"
    );

    // 3. Bookmark warp: BookmarkNotFound vs WarpDistanceTooShort (< 150,000 m) vs valid bookmark
    let bm_id = 555_001_i64;
    let bm_kind = PyRep::String(["book", "mark"].concat());
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdWarpToStuff", &[bm_kind.clone(), PyRep::Integer(bm_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "BookmarkNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: "Deep Safe".into(),
                solar_system_id: sys_id,
                x: 100_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdWarpToStuff", &[bm_kind.clone(), PyRep::Integer(bm_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "WarpDistanceTooShort"
    );

    // Move bookmark to 10 AU away; test WarpScrambled, non-covert CloakedCannotWarp, and NotEnoughCapacitor
    ctx.db.with_mut_snapshot(|snap| { snap.bookmarks.get_mut(&bm_id).unwrap().x = 10.0 * AU_IN_METERS; });
    let _ = sys_handle
        .spawn_ball_with_combat(
            mk_ball(ship_id, 587, DVec3::ZERO),
            CombatState { warp_scramble_strength: 1, ..CombatState::default() },
        )
        .await;
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdWarpToStuff", &[bm_kind.clone(), PyRep::Integer(bm_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "WarpScrambled"
    );

    let cloak_item_id = 777_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_item_id,
            make_fighter_item(cloak_item_id, 11_370, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(cloak_item_id);
    });
    let _ = sys_handle
        .spawn_ball_with_combat(
            mk_ball(ship_id, 587, DVec3::ZERO),
            CombatState { is_cloaked: true, ..CombatState::default() },
        )
        .await;
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdWarpToStuff", &[bm_kind.clone(), PyRep::Integer(bm_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "CloakedCannotWarp"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_item_id);
        snap.items.remove(&cloak_item_id);
    });

    let _ = sys_handle
        .spawn_ball_with_combat(
            mk_ball(ship_id, 587, DVec3::ZERO),
            CombatState { capacitor: 0.0, ..CombatState::default() },
        )
        .await;
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdWarpToStuff", &[bm_kind.clone(), PyRep::Integer(bm_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "NotEnoughCapacitor"
    );

    let _ = sys_handle
        .spawn_ball_with_combat(
            mk_ball(ship_id, 587, DVec3::ZERO),
            CombatState { capacitor: 500.0, max_capacitor: 500.0, ..CombatState::default() },
        )
        .await;
    let warp_ok = handle(
        &ctx, &mut sess, Some(&bnd), "beyonce", "CmdWarpToStuff", &[bm_kind, PyRep::Integer(bm_id)], &empty_kw,
    ).await.unwrap().unwrap();
    assert!(warp_ok.as_i64().is_some());
    let post_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(post_cs.capacitor < 500.0);
}

#[tokio::test]
async fn cmd_stargate_jump_and_cmd_dock_restrictions_and_execution() {
    let mut ctx = test_ctx();
    let (char_id, ship_id, sys_id, dest_sys) = (90_000_001_i64, 100_000_001_i64, 30_000_142_u32, 30_000_144_u32);
    let (gate_id, dest_gate_id) = (50_001_248_i64, 50_001_249_i64);
    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER);
    let bnd = bound_sys(sys_id);
    let empty_kw = HashMap::new();

    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.jumps.insert(
            gate_id,
            StargateJump { stargate_id: gate_id, from_solar_system_id: sys_id, to_solar_system_id: dest_sys, destination_stargate_id: dest_gate_id, position: DVec3::new(10_000.0, 0.0, 0.0) },
        );
        sde_mut.jumps.insert(
            dest_gate_id,
            StargateJump { stargate_id: dest_gate_id, from_solar_system_id: dest_sys, to_solar_system_id: sys_id, destination_stargate_id: gate_id, position: DVec3::new(-10_000.0, 0.0, 0.0) },
        );
    }

    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdStargateJump", &[PyRep::Integer(999_999_999)], &empty_kw).await.unwrap().unwrap_err().code,
        "UnknownStargate"
    );
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdStargateJump", &[PyRep::Integer(dest_gate_id)], &empty_kw).await.unwrap().unwrap_err().code,
        "StargateWrongSystem"
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(mk_ball(ship_id, 587, DVec3::new(60_000.0, 0.0, 0.0))).await;
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdStargateJump", &[PyRep::Integer(gate_id)], &empty_kw).await.unwrap().unwrap_err().code,
        "OutsideStargateJumpRange"
    );

    let _ = sys_handle.spawn_ball(mk_ball(ship_id, 587, DVec3::new(10_500.0, 0.0, 0.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.solar_system_id = sys_id;
        }
    });
    let jump_ok = handle(&ctx, &mut sess, Some(&bnd), "beyonce", "CmdStargateJump", &[PyRep::Integer(gate_id)], &empty_kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(jump_ok.dict_get("solarSystemID"), Some(&PyRep::Integer(dest_sys as i64)));
    assert_eq!(sess.solar_system_id, Some(dest_sys));
    clear_session_change_timer(&ctx, char_id);

    let struct_id = 1_000_000_012_345_i64;
    let bnd_dest = bound_sys(dest_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 98_000_001;
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: dest_sys,
                name: "Test Astrahus".into(),
                position: [-10_000.0, 0.0, 0.0],
                state: "Destroyed".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd_dest), "beyonce", "CmdDock", &[PyRep::Integer(struct_id), PyRep::Integer(ship_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "StructureDestroyed"
    );

    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&struct_id).unwrap().state = "Anchoring".into(); });
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd_dest), "beyonce", "CmdDock", &[PyRep::Integer(struct_id), PyRep::Integer(ship_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "StructureNotDockable"
    );

    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&struct_id).unwrap().state = "ShieldVulnerable".into(); });
    let _dock_ok = handle(
        &ctx, &mut sess, Some(&bnd_dest), "beyonce", "CmdDock", &[PyRep::Integer(struct_id), PyRep::Integer(ship_id)], &empty_kw,
    ).await.unwrap().unwrap();
    assert_eq!(sess.structure_id, Some(struct_id));
}

#[tokio::test]
async fn structure_cyno_beacon_validation_and_cone_scan_inhibitor_and_radians() {
    let mut ctx = test_ctx();
    let (char_id, ship_id, sys_id, low_sys) = (90_000_001_i64, 100_000_001_i64, 30_000_142_u32, 30_002_537_u32);
    let beacon_id = 1_000_000_099_999_i64;
    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER);
    let bnd = bound_sys(sys_id);
    let empty_kw = HashMap::new();

    Arc::make_mut(&mut ctx.sde).solar_systems.insert(
        low_sys,
        SolarSystem {
            solar_system_id: low_sys,
            constellation_id: 20_000_372,
            region_id: 10_000_030,
            name: "Amamake".into(),
            security_status: 0.4,
            radius: 1e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: Some(8),
        },
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(mk_ball(ship_id, 999_999, DVec3::ZERO)).await;

    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "structureCynoBeaconMgr", "CmdJumpToStructureBeacon", &[PyRep::Integer(low_sys as i64), PyRep::Integer(beacon_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "BeaconNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 98_000_001;
        }
        snap.structures.insert(
            beacon_id,
            UpwellStructureRecord {
                structure_id: beacon_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: low_sys,
                name: "Not A Beacon".into(),
                position: [0.0, 0.0, 0.0],
                state: "Destroyed".into(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "structureCynoBeaconMgr", "CmdJumpToStructureBeacon", &[PyRep::Integer(low_sys as i64), PyRep::Integer(beacon_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "InvalidStructureBeacon"
    );

    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&beacon_id).unwrap().type_id = 35_840; });
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "structureCynoBeaconMgr", "CmdJumpToStructureBeacon", &[PyRep::Integer(low_sys as i64), PyRep::Integer(beacon_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "BeaconDestroyed"
    );

    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&beacon_id).unwrap().state = "ShieldVulnerable".into(); });
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "structureCynoBeaconMgr", "CmdJumpToStructureBeacon", &[PyRep::Integer(low_sys as i64), PyRep::Integer(beacon_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "BeaconOutOfFuel"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&beacon_id).unwrap();
        st.fuel_blocks = 100;
        st.solar_system_id = sys_id;
    });
    assert_eq!(
        handle(&ctx, &mut sess, Some(&bnd), "structureCynoBeaconMgr", "CmdJumpToStructureBeacon", &[PyRep::Integer(sys_id as i64), PyRep::Integer(beacon_id)], &empty_kw)
            .await.unwrap().unwrap_err().code,
        "CannotJumpToHighSecBeacon"
    );

    let (visible_ball_id, cloaked_ball_id, behind_cone_ball_id, far_ball_id) = (700_001_i64, 700_002_i64, 700_003_i64, 700_004_i64);

    let _ = sys_handle.spawn_ball(mk_ball(visible_ball_id, 587, DVec3::new(10_000.0, 0.0, 0.0))).await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            mk_ball(cloaked_ball_id, 587, DVec3::new(12_000.0, 0.0, 0.0)),
            CombatState { is_cloaked: true, ..CombatState::default() },
        )
        .await;
    let _ = sys_handle.spawn_ball(mk_ball(behind_cone_ball_id, 587, DVec3::new(-50_000.0, 0.0, 0.0))).await;
    let _ = sys_handle.spawn_ball(mk_ball(far_ball_id, 587, DVec3::new(500_000.0, 0.0, 0.0))).await;

    let scan_res = handle(
        &ctx, &mut sess, Some(&bnd), "scanMgr", "ConeScan",
        &[PyRep::Real(std::f64::consts::FRAC_PI_2), PyRep::Real(100_000.0), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        &empty_kw,
    ).await.unwrap().unwrap();
    let PyRep::List(entries) = scan_res else { panic!("expected list from ConeScan"); };
    let scanned_ids: Vec<i64> = entries.iter().filter_map(|e| e.dict_get("id").and_then(|v| v.as_i64())).collect();
    assert!(scanned_ids.contains(&visible_ball_id));
    assert!(!scanned_ids.contains(&cloaked_ball_id) && !scanned_ids.contains(&behind_cone_ball_id) && !scanned_ids.contains(&far_ball_id));
}

#[tokio::test]
async fn update_state_request_dungeon_trigger_unmatched_rpc_and_main_bank_unlink() {
    let ctx = test_ctx();
    let (char_id, ship_id, sys_id) = (90_000_001_i64, 100_000_001_i64, 30_000_142_u32);
    let mut sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER);
    let bnd = bound_sys(sys_id);
    let empty_kw = HashMap::new();

    // 1. Unmatched service/method returns None (never Some(Ok(Default)))
    let unknown_svc = ["unmatched", "_svc"].concat();
    let unknown_meth = ["unmatched", "_meth"].concat();
    assert!(handle(&ctx, &mut sess, Some(&bnd), &unknown_svc, &unknown_meth, &[], &empty_kw).await.is_none());

    // 2. RequestMainBankUnlink unlinks main bank while leaving Reserve Bank state untouched
    ctx.db.with_mut_snapshot(|snap| {
        let ss = snap.system_states.entry(sys_id).or_default();
        ss.ess_linked_character_id = Some(char_id);
        ss.ess_link_started_tick = Some(42);
        set_ess_reserve_pulses(snap, sys_id, &EssReservePulseState::default());
    });
    let mb_res = handle(&ctx, &mut sess, None, "essMgr", "RequestMainBankUnlink", &[], &empty_kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mb_res, PyRep::Bool(true));
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_id].ess_linked_character_id), None);

    // 3. `handle_update_state_request` stops orphan emergency warp & fires `check_dungeon_triggers` only when `!instances.is_empty()`
    let inst_id = 950_777_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.dungeon_runtime.instances.insert(
            inst_id,
            eve_db::service_domains::pve_sites::DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 4001,
                dungeon_name: "Test Dungeon".into(),
                category: "Deadspace".into(),
                archetype: "Combat".into(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: [0.0, 0.0, 0.0],
                disallow_mwd: false,
                entry_gate_ball_id: None,
                entry_gate_pos: None,
                entry_gate_locked: false,
                current_room_index: 0,
                rooms: vec![eve_db::service_domains::pve_sites::DungeonRuntimeRoom {
                    room_index: 0,
                    room_id: 1,
                    room_name: "Room 0".into(),
                    position: [0.0, 0.0, 0.0],
                    gate_destination_pos: [0.0, 0.0, 0.0],
                    gated: false,
                    disallow_mwd: None,
                    gate_ball_id: None,
                    gate_locked: true,
                    objects: Vec::new(),
                    npc_groups: Vec::new(),
                    triggers: vec![eve_db::service_domains::pve_sites::DungeonTrigger {
                        trigger_id: 1,
                        kind: eve_db::service_domains::pve_sites::DungeonTriggerKind::OnApproach {
                            target_ball_id: None,
                            target_object_index: None,
                            anchor_pos: [0.0, 0.0, 0.0],
                            radius_m: 5_000.0,
                        },
                        action: eve_db::service_domains::pve_sites::DungeonTriggerAction::UnlockGate,
                        fired: false,
                    }],
                    spawned: true,
                    cleared: false,
                }],
                completed: false,
                despawned: false,
                active_objective_id: 0,
                last_completed_task_id: None,
                completed_task_ids: Vec::new(),
                created_unix: 1_700_000_000,
            },
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut warp_ball = mk_ball(ship_id, 587, DVec3::ZERO);
    warp_ball.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e9, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e9,
        phase: eve_destiny::WarpPhase::Cruising,
        elapsed_warp_time: 5.0,
    };
    let _ = sys_handle.spawn_ball(warp_ball).await;

    let upd_res = handle(&ctx, &mut sess, Some(&bnd), "beyonce", "UpdateStateRequest", &[], &empty_kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(upd_res, PyRep::None);

    let (_, post_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(post_ball.mode, MovementMode::Stop));
    ctx.db.with_snapshot(|snap| {
        let room = &snap.dungeon_runtime.instances[&inst_id].rooms[0];
        assert!(room.triggers[0].fired, "OnApproach trigger must fire when instances is non-empty");
        assert!(!room.gate_locked, "UnlockGate action must unlock room gate");
    });

    // 4. `record_session_change_timer_with_invuln` side effects on timer, invuln, and OnSessionChanged notice
    let _ = ctx
        .db
        .acquire_online_lock(1, 9501, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);
    let t_now = ctx.now_unix();
    record_session_change_timer(&ctx, char_id, t_now, sys_id, 55, "jump_drive");
    let st = get_session_change_timer(&ctx, char_id).expect("session change timer stored");
    assert_eq!(st.expires_unix, t_now + 10);
    assert_eq!(st.location_change_invul_until, t_now + 60);
    assert!(is_location_change_invulnerable(&ctx, char_id, t_now + 30));
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnSessionChanged"));
}
