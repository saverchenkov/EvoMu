//! Deep branch-coverage and mutation-killing unit tests for `abyssal_void_mgr` and `abyssal_void_gates`:
//! trace lifetime expiration (explicit & fallback TTLs), fleet ship capacity validation,
//! highsec tier suspect flags, room gate progression (1 -> 2 -> 3 -> exit & collapse),
//! and Void Space jump, relog (`OnVoidSpaceRelog`), and end gate activation.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::abyssal_void_mgr::{
    handle, ABYSSAL_SPACE_SYSTEM_ID, ABYSSAL_SYSTEM_RANGE, VOID_SPACE_SYSTEM_ID,
};
use crate::services::pve_special_sites::with_pve_special_reg;
use crate::services::pve_special_sites_abyssal::{
    check_abyssal_in_pocket_rpc_restrictions, evaluate_highsec_abyssal_tier_policy,
    is_abyssal_local_channel_disabled, is_abyssal_system, AbyssalTraceRecord,
    UNENTERED_GATE_LIFETIME_SEC,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx_at(now_secs: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_secs)));
    ctx
}

fn seed_pilot_and_ship(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.item_positions.insert(ship_id, pos.to_array());
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h.try_spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
        SlimItemMeta {
            owner_id: char_id,
            char_id: Some(char_id),
            ..Default::default()
        },
    );
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s
}

#[test]
fn pocket_bounds_rpc_restrictions_and_highsec_tier_suspect_policy_boundaries() {
    assert!(!is_abyssal_system(31_999_999));
    assert!(is_abyssal_system(32_000_000));
    assert!(is_abyssal_system(32_999_999));
    assert!(!is_abyssal_system(33_000_000));
    assert!(ABYSSAL_SYSTEM_RANGE.contains(&ABYSSAL_SPACE_SYSTEM_ID));
    assert!(ABYSSAL_SYSTEM_RANGE.contains(&VOID_SPACE_SYSTEM_ID));

    assert!(is_abyssal_local_channel_disabled("solarsystemid2:32000001", None));
    assert!(is_abyssal_local_channel_disabled("local", Some(32_000_001)));
    assert!(!is_abyssal_local_channel_disabled("local", Some(30_000_142)));

    let mut sess = SessionState::new(1);
    sess.solar_system_id = Some(30_000_142);
    assert!(check_abyssal_in_pocket_rpc_restrictions(&sess, "map", "GetMap", &[]).is_none());

    sess.solar_system_id = Some(32_000_001);
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(&sess, "map", "GetMap", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "MapDisabledInAbyssalSpace"
    );
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(&sess, "scanMgr", "ConeScan", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDscanInThisSystem"
    );
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(
            &sess,
            "LSC",
            "SendMessage",
            &[PyRep::String("local".into())],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "LocalChannelDisabledInAbyssalSpace"
    );

    // Highsec tier policy boundaries:
    // - >= 0.85 (0.9, 1.0): T0 -> Ok(false), T1+ -> Err("FilamentDisallowedInHighSec")
    // - 0.75..0.85 (0.8): T3 -> Ok(false), T4 -> Ok(true)
    // - 0.65..0.75 (0.7): T4 -> Ok(false), T5 -> Ok(true)
    // - 0.55..0.65 (0.6): T5 -> Ok(false), T6 -> Ok(true)
    // - < 0.55 (0.5, low/null): T6 -> Ok(false)
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.9, 0).unwrap(), false);
    assert_eq!(
        evaluate_highsec_abyssal_tier_policy(0.85, 1).unwrap_err().code,
        "FilamentDisallowedInHighSec"
    );
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.8, 3).unwrap(), false);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.8, 4).unwrap(), true);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.7, 4).unwrap(), false);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.7, 5).unwrap(), true);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.6, 5).unwrap(), false);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.6, 6).unwrap(), true);
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.5, 6).unwrap(), false);
}

#[tokio::test]
async fn trace_lifetime_expiration_explicit_and_unentered_1800s_vs_entered_1200s_fallback() {
    let base_now = 1_800_000_000u64;
    let ctx = make_ctx_at(base_now);
    let sys_id = 30_004_759u32;
    let char_id = 92_001i64;
    let ship_id = 892_001i64;
    let far_pos = DVec3::new(500_000_000.0, 500_000_000.0, 500_000_000.0);
    // Caracal (type 621, Cruiser group 26)
    let mut sess = seed_pilot_and_ship(&ctx, char_id, ship_id, 621, sys_id, far_pos);
    let kw = HashMap::new();

    // Seed 3 traces:
    // - trace 701: explicit expires_at_sec = base_now + 500
    // - trace 702: expires_at_sec = 0, unentered (pocket_id == None) -> 1800s fallback TTL
    // - trace 703: expires_at_sec = 0, entered (pocket_id == Some(99)) -> 1200s fallback TTL
    with_pve_special_reg(&ctx, |reg| {
        for (tid, exp, pid) in [
            (701i64, base_now + 500, None),
            (702i64, 0, None),
            (703i64, 0, Some(99i64)),
        ] {
            reg.abyssal_traces.insert(
                tid,
                AbyssalTraceRecord {
                    trace_ball_id: tid,
                    char_id,
                    filament_type_id: 47_762,
                    origin_solar_system_id: sys_id,
                    position: far_pos.to_array(),
                    created_unix: base_now,
                    expires_at_sec: exp,
                    max_ships: 1,
                    entered_char_ids: Vec::new(),
                    pocket_id: pid,
                    fleet_id: None,
                },
            );
        }
    });

    // At base_now + 500, trace 701 is expired (now >= expires_at_sec) -> TraceExpired and removed
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now + 500)));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "abyssalMgr",
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(701)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TraceExpired"
    );
    assert!(!with_pve_special_reg(&ctx, |reg| reg.abyssal_traces.contains_key(&701)));

    // At base_now + 1200, entered trace 703 with expires_at_sec == 0 expires!
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now + 1_200)));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "abyssalMgr",
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(703)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TraceExpired"
    );

    // At base_now + 1800 (UNENTERED_GATE_LIFETIME_SEC), unentered trace 702 expires!
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
        base_now + UNENTERED_GATE_LIFETIME_SEC,
    )));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "abyssalMgr",
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(702)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TraceExpired"
    );
}

#[tokio::test]
async fn fleet_ship_capacity_validation_and_room_gate_progression_with_suspect_exit() {
    let base_now = 1_800_000_000u64;
    let mut ctx = make_ctx_at(base_now);
    let sys_id = 30_000_142u32;
    // Set origin security status to 0.7 so Tier 5 triggers a suspect flag on exit
    Arc::make_mut(&mut ctx.sde)
        .solar_systems
        .get_mut(&sys_id)
        .unwrap()
        .security_status = 0.7;

    let (p1, p2, p3) = (92_101i64, 92_102i64, 92_103i64);
    let (s1, s2, s3) = (892_101i64, 892_102i64, 892_103i64);
    let far_pos = DVec3::new(500_000_000.0, 500_000_000.0, 500_000_000.0);
    // Cormorant (16238, Destroyer group 420 -> max_ships = 2), Caracal (621, Cruiser group 26 -> max_ships = 1)
    let mut sess1 = seed_pilot_and_ship(&ctx, p1, s1, 16_238, sys_id, far_pos);
    let mut sess2 = seed_pilot_and_ship(&ctx, p2, s2, 16_238, sys_id, far_pos);
    let mut sess3_cruiser = seed_pilot_and_ship(&ctx, p3, s3, 621, sys_id, far_pos);
    let kw = HashMap::new();

    let trace_id = 750i64;
    with_pve_special_reg(&ctx, |reg| {
        reg.abyssal_traces.insert(
            trace_id,
            AbyssalTraceRecord {
                trace_ball_id: trace_id,
                char_id: p1,
                filament_type_id: 47_891, // Tier 5 Exotic
                origin_solar_system_id: sys_id,
                position: far_pos.to_array(),
                created_unix: base_now,
                expires_at_sec: base_now + 1_200,
                max_ships: 2, // Destroyer trace (max 2 ships)
                entered_char_ids: Vec::new(),
                pocket_id: None,
                fleet_id: Some(5001),
            },
        );
    });

    // 1) Destroyer trace (max_ships = 2) requires active fleet -> NotInFleet
    assert_eq!(
        handle(
            &ctx,
            &mut sess1,
            "abyssalMgr",
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(trace_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotInFleet"
    );

    // 2) Wrong fleet ID -> AccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            9999,
            eve_db::types::FleetRecord {
                fleet_id: 9999,
                boss_id: p2,
                members: vec![p2],
                free_move: true,
                ..Default::default()
            },
        );
        snap.characters.get_mut(&p2).unwrap().fleet_id = Some(9999);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess2,
            "abyssalMgr",
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(trace_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // 3) Cruiser attempting to enter Destroyer trace -> DisallowedShipType
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.remove(&9999);
        snap.fleets.insert(
            5001,
            eve_db::types::FleetRecord {
                fleet_id: 5001,
                boss_id: p1,
                members: vec![p1, p2, p3],
                free_move: true,
                ..Default::default()
            },
        );
        for cid in [p1, p2, p3] {
            snap.characters.get_mut(&cid).unwrap().fleet_id = Some(5001);
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess3_cruiser,
            "abyssalMgr",
            "AbyssalEntranceGateActivation",
            &[PyRep::Integer(trace_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DisallowedShipType"
    );

    // 4) Owner p1 and wingman p2 (both in fleet 5001 in Destroyers) enter trace -> 2/2 capacity reached
    let enter1 = handle(
        &ctx,
        &mut sess1,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(trace_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("p1 enters");
    let pid = enter1.dict_get("pocketID").unwrap().as_i64().unwrap();
    assert_eq!(sess1.solar_system_id, Some(ABYSSAL_SPACE_SYSTEM_ID));

    let _ = handle(
        &ctx,
        &mut sess2,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(trace_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("p2 joins pocket");

    // 5) 3rd destroyer pilot in same fleet rejected when trace capacity (2/2) is reached
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&s3).unwrap().type_id = 16_238;
    });
    assert!(handle(
        &ctx,
        &mut sess3_cruiser,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(trace_id)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());

    // 6) Room gate progression for p1 inside ABYSSAL_SPACE_SYSTEM_ID:
    // Gate ID for room r is `1_895_000_000 + pid * 10 + r`
    let gate_r1 = 1_895_000_000 + pid * 10 + 1;
    let gate_r2 = 1_895_000_000 + pid * 10 + 2;
    let gate_r3 = 1_895_000_000 + pid * 10 + 3;

    // While room 1 NPCs are alive -> GateLocked
    assert_eq!(
        handle(&ctx, &mut sess1, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(gate_r1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "GateLocked"
    );

    // EndGate from Room 1 -> NotInFinalRoom
    assert_eq!(
        handle(&ctx, &mut sess1, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(gate_r1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFinalRoom"
    );

    let ab_h = ctx.systems.get_or_spawn(ABYSSAL_SPACE_SYSTEM_ID);
    for (r_idx, gate_id, expected_next_room) in [(0usize, gate_r1, 2i64), (1usize, gate_r2, 3i64)] {
        let npcs = with_pve_special_reg(&ctx, |reg| {
            let p = reg.abyssal_pockets.get_mut(&p1).unwrap();
            p.rooms[r_idx].cleared = true;
            p.rooms[r_idx].npc_ball_ids.clone()
        });
        for npc_id in npcs {
            ab_h.try_remove_ball(npc_id);
        }
        let (_, gb) = ab_h.get_ball_snapshot(gate_id).await.unwrap().unwrap();
        let _ = ab_h.set_ball_position(s1, gb.position).await;
        let rep = handle(&ctx, &mut sess1, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(gate_id)], &kw)
            .await
            .unwrap()
            .expect("advance room");
        assert_eq!(rep.dict_get("room"), Some(&PyRep::Integer(expected_next_room)));
    }

    // Calling AbyssalGateActivation in Room 3 -> AlreadyInFinalRoom
    assert_eq!(
        handle(&ctx, &mut sess1, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(gate_r3)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AlreadyInFinalRoom"
    );

    // Clear Room 3 and call AbyssalEndGateActivation -> exits to sys_id and triggers suspect flag (0.7 sec, Tier 5)
    let r3_npcs = with_pve_special_reg(&ctx, |reg| {
        let p = reg.abyssal_pockets.get_mut(&p1).unwrap();
        p.rooms[2].cleared = true;
        p.rooms[2].npc_ball_ids.clone()
    });
    for npc_id in r3_npcs {
        ab_h.try_remove_ball(npc_id);
    }
    let (_, gb3) = ab_h.get_ball_snapshot(gate_r3).await.unwrap().unwrap();
    let _ = ab_h.set_ball_position(s1, gb3.position).await;

    let exit_rep = handle(&ctx, &mut sess1, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(gate_r3)], &kw)
        .await
        .unwrap()
        .expect("exit abyssal pocket");
    assert_eq!(
        exit_rep.dict_get("solarSystemID"),
        Some(&PyRep::Integer(sys_id as i64))
    );
    assert_eq!(sess1.solar_system_id, Some(sys_id));
    assert!(ctx.crimewatch.read().unwrap().is_suspect(p1, base_now));
}

#[tokio::test]
async fn pocket_collapse_expiration_and_void_space_jump_relog_and_end_gate() {
    let base_now = 1_800_000_000u64;
    let ctx = make_ctx_at(base_now);
    let sys_id = 30_004_759u32;
    let char_id = 92_201i64;
    let ship_id = 892_201i64;
    let gate_ball_id = 892_250i64;
    let far_pos = DVec3::new(500_000_000.0, 500_000_000.0, 500_000_000.0);
    let mut sess = seed_pilot_and_ship(&ctx, char_id, ship_id, 621, sys_id, far_pos);
    let kw = HashMap::new();

    // 1) Seed a Void Space filament item (group 4145) in ship cargo and jump into Void Space
    let void_fil_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 4145)
        .map(|t| t.type_id)
        .expect("SDE group 4145 Void Space filament");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gate_ball_id,
            InventoryItemRecord {
                item_id: gate_ball_id,
                type_id: void_fil_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let _jump_rep = handle(
        &ctx,
        &mut sess,
        "voidSpaceMgr",
        "VoidSpacePlayerJump",
        &[PyRep::Integer(gate_ball_id), PyRep::Integer(777)],
        &kw,
    )
    .await
    .unwrap()
    .expect("VoidSpacePlayerJump");
    assert_eq!(sess.solar_system_id, Some(VOID_SPACE_SYSTEM_ID));

    // 2) voidSpaceMgr.ClientIsReady restores content_id from service_state_blobs and emits OnVoidSpaceRelog
    let ready_rep = handle(&ctx, &mut sess, "voidSpaceMgr", "ClientIsReady", &[], &kw)
        .await
        .unwrap()
        .expect("ClientIsReady");
    assert_eq!(ready_rep.dict_get("ready"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.service_state_blobs
                .get(&format!("void_space_content:{char_id}"))
                .map(String::as_str),
            Some("892250")
        );
    });

    // 3) Advance clock past session-change timer (15s), move to exit gate 888_001, and activate VoidSpaceEndGateActivation
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_now + 30)));
    let void_h = ctx.systems.get_or_spawn(VOID_SPACE_SYSTEM_ID);
    let (_, exit_gb) = void_h.get_ball_snapshot(888_001).await.unwrap().unwrap();
    let _ = void_h.set_ball_position(ship_id, exit_gb.position).await;

    let void_exit = handle(
        &ctx,
        &mut sess,
        "voidSpaceMgr",
        "VoidSpaceEndGateActivation",
        &[PyRep::Integer(888_001)],
        &kw,
    )
    .await
    .unwrap()
    .expect("VoidSpaceEndGateActivation");
    assert_eq!(
        void_exit.dict_get("solarSystemID"),
        Some(&PyRep::Integer(sys_id as i64))
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap
            .service_state_blobs
            .contains_key(&format!("void_space_content:{char_id}")));
    });
}
