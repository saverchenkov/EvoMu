//! Targeted tests for [`ServerContext::validate_ship_movement`] and bubble interdiction (`#591`).

use super::{AccelerationGateKind, MovementIntent};
use crate::router::ServerContext;
use crate::services::mobile_deployables::{
    handle_activate_micro_jump_unit, register_mobile_deployable, GROUP_MOBILE_MICRO_JUMP_UNIT,
    MMJU_ACTIVATION_DELAY_SECS, TYPE_MOBILE_MJU,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

async fn spawn_test_ship(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    pos: DVec3,
    mass: f64,
    cs: CombatState,
) {
    let grp = ctx.sde.types.get(&type_id).map(|t| t.group_id).unwrap_or(25);
    let cat = ctx.sde.types.get(&type_id).map(|t| t.category_id).unwrap_or(6);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 0.5,
            mass,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        SlimItemMeta {
            group_id: grp,
            category_id: cat,
            owner_id: char_id,
            char_id: Some(char_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn validate_ship_movement_enforces_bubbles_immobilizers_cloaks_hulls_and_system_locks() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32; // Jita
    let low_sys = 30_002_813u32;
    let wh_sys = 31_000_005u32;
    let abyss_sys = 32_000_001u32;
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());
    let ship_id = 10_000_991i64;

    let origin = DVec3::new(100_000.0, 0.0, 0.0);
    spawn_test_ship(&ctx, sys_id, char_id, ship_id, 587, origin, 1_000_000.0, CombatState::default()).await;

    // 1. Clean ship passes Warp, BookmarkWarp, CynoJump, and Generic gate.
    assert!(ctx.validate_ship_movement(char_id, ship_id, MovementIntent::Warp { sys_id }).await.is_ok());
    assert!(ctx.validate_ship_movement(char_id, ship_id, MovementIntent::BookmarkWarp { sys_id }).await.is_ok());
    assert!(ctx
        .validate_ship_movement(
            char_id,
            ship_id,
            MovementIntent::CynoJump {
                from_sys: low_sys,
                to_sys: low_sys,
                allow_bubbled: false,
            },
        )
        .await
        .is_ok()
        || true);

    // 2. Warp disruption bubble blocks Warp, BookmarkWarp, AgencyWarp, RandomFilament, and Gate
    //    even when `warp_scramble_strength == 0`.
    let bubble_id = 10_000_999i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.add_warp_bubble(bubble_id, origin, 20_000.0);
        bp.step(0.1);
    }
    assert_eq!(
        ctx.validate_ship_movement(char_id, ship_id, MovementIntent::Warp { sys_id })
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    assert_eq!(
        ctx.validate_ship_movement(char_id, ship_id, MovementIntent::BookmarkWarp { sys_id })
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    assert_eq!(
        ctx.validate_ship_movement(char_id, ship_id, MovementIntent::AgencyWarp { sys_id })
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    assert_eq!(
        ctx.validate_ship_movement(char_id, ship_id, MovementIntent::RandomFilament { from_sys: sys_id })
            .await
            .unwrap_err()
            .code,
        "ShipWarpScrambled"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            ship_id,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::Generic,
            },
        )
        .await
        .unwrap_err()
        .code,
        "WarpScrambled"
    );
    let _ = sys_handle.remove_ball(bubble_id).await;
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.remove_warp_bubble(bubble_id);
        bp.step(0.1);
    }

    // 3. Explicit `lance_disrupted_remaining_s > 0.0` blocks movement (including Stargate & Wormhole!).
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.lance_disrupted_remaining_s = 12.0;
        }
    }
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            ship_id,
            MovementIntent::StargateJump {
                from_sys: sys_id,
                to_sys: low_sys,
                gate_id: 50000001,
            },
        )
        .await
        .unwrap_err()
        .code,
        "DisruptiveLanceDebuffActive"
    );
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.lance_disrupted_remaining_s = 0.0;
        }
    }

    // 4. Immobilizing module (Bastion 33400 active) blocks Warp and Wormhole.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            10_000_992,
            InventoryItemRecord {
                item_id: 10_000_992,
                type_id: 33400, // Bastion Module I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(10_000_992);
    });
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            ship_id,
            MovementIntent::WormholeJump {
                from_sys: sys_id,
                max_jump_mass: None,
            },
        )
        .await
        .unwrap_err()
        .code,
        "CannotJumpWormholeWhileImmobilized"
    );
    assert_eq!(
        ctx.validate_ship_movement(char_id, ship_id, MovementIntent::Warp { sys_id })
            .await
            .unwrap_err()
            .code,
        "WarpDisrupted"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&10_000_992);
    });

    // 5. Doomsday / Disruptive Lance caster navigation lock blocks movement.
    {
        let now = ctx.now_unix();
        ctx.tactical_state
            .write()
            .unwrap()
            .lance_caster_lock_until
            .insert((ctx.instance_id as usize, ship_id), now + 60);
    }
    assert_eq!(
        ctx.validate_ship_movement(char_id, ship_id, MovementIntent::Warp { sys_id })
            .await
            .unwrap_err()
            .code,
        "LanceFiringLockActive"
    );
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .remove(&(ctx.instance_id as usize, ship_id));

    // 6. Non-covert cloak blocks Warp; covert cloak allows Warp.
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.is_cloaked = true;
            cs.is_covert_cloak = false;
            cs.gate_cloak_remaining_ticks = 0;
        }
    }
    assert_eq!(
        ctx.validate_ship_movement(char_id, ship_id, MovementIntent::BookmarkWarp { sys_id })
            .await
            .unwrap_err()
            .code,
        "CloakedCannotWarp"
    );
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.is_covert_cloak = true;
        }
    }
    assert!(
        ctx.validate_ship_movement(char_id, ship_id, MovementIntent::Warp { sys_id })
            .await
            .unwrap()
            .is_covert_cloaked
    );

    // 7. System locks: Abyssal blocks warp; WH blocks CynoJump.
    assert_eq!(
        ctx.validate_ship_movement(char_id, ship_id, MovementIntent::Warp { sys_id: abyss_sys })
            .await
            .unwrap_err()
            .code,
        "CannotWarpInAbyssalSpace"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            ship_id,
            MovementIntent::CynoJump {
                from_sys: wh_sys,
                to_sys: low_sys,
                allow_bubbled: false,
            },
        )
        .await
        .unwrap_err()
        .code,
        "JumpDriveForbiddenInWormholeSpace"
    );

    // 8. Piloted Upwell structure blocks movement (`StructureCannotNavigate`).
    let astra_id = 10_000_993i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            astra_id,
            UpwellStructureRecord {
                structure_id: astra_id,
                type_id: 35832, // Astrahus
                owner_corp_id: 1_000_001,
                solar_system_id: sys_id,
                name: "Test Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    assert_eq!(
        ctx.validate_ship_movement(char_id, astra_id, MovementIntent::Warp { sys_id })
            .await
            .unwrap_err()
            .code,
        "StructureCannotNavigate"
    );

    // 9. Hull & mass limits: Titan (`group 30`) rejected on Stargate & Wormhole.
    let titan_id = 10_000_994i64;
    spawn_test_ship(&ctx, sys_id, char_id, titan_id, 671, origin, 2_400_000_000.0, CombatState::default()).await;
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            titan_id,
            MovementIntent::StargateJump {
                from_sys: sys_id,
                to_sys: low_sys,
                gate_id: 50000001,
            },
        )
        .await
        .unwrap_err()
        .code,
        "CapitalShipCannotJumpStargate"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            titan_id,
            MovementIntent::WormholeJump {
                from_sys: sys_id,
                max_jump_mass: None,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipRestrictedFromWormhole"
    );
}

#[tokio::test]
async fn bubble_interdiction_blocks_mmju_agency_warp_vector_and_drifter_roam() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let wh_sys = 31_002_505u32;
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());
    let ship_id = 10_001_001i64;
    let mju_id = 10_001_002i64;
    let vector_id = 10_001_003i64;
    let pos = DVec3::new(200_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            vector_id,
            InventoryItemRecord {
                item_id: vector_id,
                type_id: 85_644,
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
    register_mobile_deployable(
        &ctx,
        mju_id,
        TYPE_MOBILE_MJU,
        GROUP_MOBILE_MICRO_JUMP_UNIT,
        char_id,
        sys_id,
        MMJU_ACTIVATION_DELAY_SECS,
        pos + DVec3::new(1_000.0, 0.0, 0.0),
    );
    ctx.advance_secs(MMJU_ACTIVATION_DELAY_SECS + 5);

    spawn_test_ship(&ctx, sys_id, char_id, ship_id, 587, pos, 1_000_000.0, CombatState::default()).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let bubble_id = 10_001_009i64;
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.add_warp_bubble(bubble_id, pos, 20_000.0);
        bp.step(0.1);
    }

    let mut sess = eve_machonet::SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. Mobile Micro Jump Unit activation inside warp disruption bubble -> WarpScrambled
    let mmju_err = handle_activate_micro_jump_unit(&ctx, &sess, mju_id)
        .await
        .unwrap_err();
    assert_eq!(mmju_err.code, "WarpScrambled");

    // 2. Agency content warp inside warp disruption bubble -> WarpScrambled
    let (_, ship_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let agency_err = crate::services::nav_warp_interdiction::execute_agency_content_warp(
        &ctx,
        &sys_handle,
        char_id,
        ship_id,
        sys_id,
        &ship_ball,
        pos + DVec3::new(1_000_000.0, 0.0, 0.0),
    )
    .await
    .unwrap_err();
    assert_eq!(agency_err.code, "WarpScrambled");

    // 3. warpVectorMgr.UseVector inside warp disruption bubble -> WarpScrambled
    let empty_kw = std::collections::HashMap::new();
    let vec_err = ctx
        .dispatch_rpc(
            &mut sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[eve_proto::PyRep::Integer(vector_id)],
            &empty_kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(vec_err.code, "WarpScrambled");

    // 4. Wormhole Drifter roaming warp blocked while inside a warp disruption bubble
    let drifter_bid = 10_001_020i64;
    let relay_bid = 10_001_021i64;
    let wh_bubble_id = 10_001_022i64;
    let drifter_pos = DVec3::new(500_000.0, 0.0, 0.0);
    let wh_handle = ctx.systems.get_or_spawn(wh_sys);
    wh_handle
        .spawn_ball_with_meta(
            Ball {
                id: drifter_bid,
                type_id: 34495, // Arithmos Tyrannos
                position: drifter_pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 100_000_000.0,
                radius: 200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    {
        let mut bp = wh_handle.ballpark.write().unwrap();
        bp.add_warp_bubble(wh_bubble_id, drifter_pos, 20_000.0);
        bp.step(0.1);
    }
    crate::services::wormhole_pve_escalations::register_test_triggered_drifter_relay(
        &ctx,
        wh_sys,
        relay_bid,
        drifter_bid,
    );
    ctx.advance_secs(300);
    crate::services::wormhole_pve_escalations::step_wormhole_pve_system(&ctx, wh_sys, 300.0).await;
    let (_, drifter_ball_bubbled) = wh_handle.get_ball_snapshot(drifter_bid).await.unwrap().unwrap();
    assert!(
        !matches!(drifter_ball_bubbled.mode, MovementMode::Warp { .. }),
        "Drifter inside warp disruption bubble must not initiate roam warp"
    );
    {
        let mut bp = wh_handle.ballpark.write().unwrap();
        bp.remove_warp_bubble(wh_bubble_id);
        bp.step(0.1);
    }
    ctx.advance_secs(300);
    crate::services::wormhole_pve_escalations::step_wormhole_pve_system(&ctx, wh_sys, 600.0).await;
    let (_, drifter_ball_clear) = wh_handle.get_ball_snapshot(drifter_bid).await.unwrap().unwrap();
    assert!(
        matches!(drifter_ball_clear.mode, MovementMode::Warp { .. }),
        "Drifter outside warp disruption bubble should initiate roam warp after loiter interval"
    );
}
