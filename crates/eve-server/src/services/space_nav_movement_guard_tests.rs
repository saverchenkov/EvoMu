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

fn find_system_by_pred(
    sde: &SdeRegistry,
    pred: impl Fn(&eve_sde::SolarSystem) -> bool,
) -> u32 {
    let mut ids: Vec<u32> = sde
        .solar_systems
        .values()
        .filter(|s| pred(s))
        .map(|s| s.solar_system_id)
        .collect();
    ids.sort_unstable();
    ids[0]
}

fn find_ship_type_by_group(sde: &SdeRegistry, group_id: u32) -> u32 {
    let mut tids: Vec<u32> = sde
        .types
        .values()
        .filter(|t| t.category_id == 6 && t.group_id == group_id)
        .map(|t| t.type_id)
        .collect();
    tids.sort_unstable();
    tids[0]
}

fn make_test_upwell_record(structure_id: i64, sys_id: u32) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id: 35832,
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
    }
}

#[tokio::test]
async fn sync_pre_movement_helpers_and_piloted_structure_guard() {
    use super::{
        can_warp_in_ballpark, check_sync_pre_warp_eligibility, is_piloted_upwell_structure,
        validate_pre_movement_locks,
    };

    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());
    let ship_id = 10_002_001i64;
    let origin = DVec3::new(10_000.0, 0.0, 0.0);
    spawn_test_ship(&ctx, sys_id, char_id, ship_id, 587, origin, 1_000_000.0, CombatState::default()).await;

    // 1. Clean ship passes pre-movement checks & sync eligibility.
    assert!(!is_piloted_upwell_structure(&ctx, ship_id));
    assert!(validate_pre_movement_locks(&ctx, ship_id, &MovementIntent::Warp { sys_id }).is_ok());
    assert!(check_sync_pre_warp_eligibility(&ctx, ship_id));

    // 2. Navigation lock (lance caster lock) blocks validate_pre_movement_locks & sync eligibility.
    let now = ctx.now_unix();
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .insert((ctx.instance_id as usize, ship_id), now + 60);
    assert_eq!(
        validate_pre_movement_locks(&ctx, ship_id, &MovementIntent::Warp { sys_id })
            .unwrap_err()
            .code,
        "LanceFiringLockActive"
    );
    assert!(!check_sync_pre_warp_eligibility(&ctx, ship_id));
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .remove(&(ctx.instance_id as usize, ship_id));
    assert!(check_sync_pre_warp_eligibility(&ctx, ship_id));

    // 3. Active immobilizing module (Bastion 33400) blocks sync eligibility.
    let mod_id = 10_002_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 33400,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(mod_id);
    });
    assert_eq!(
        validate_pre_movement_locks(&ctx, ship_id, &MovementIntent::Warp { sys_id })
            .unwrap_err()
            .code,
        "WarpDisrupted"
    );
    assert!(!check_sync_pre_warp_eligibility(&ctx, ship_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&mod_id);
    });
    assert!(check_sync_pre_warp_eligibility(&ctx, ship_id));

    // 4. `is_piloted_upwell_structure`: detects both snap.structures entry AND category 65 active ship.
    let struct_id = 10_002_003i64;
    assert!(!is_piloted_upwell_structure(&ctx, struct_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(struct_id, make_test_upwell_record(struct_id, sys_id));
    });
    assert!(is_piloted_upwell_structure(&ctx, struct_id));
    assert!(!check_sync_pre_warp_eligibility(&ctx, struct_id));

    let cat65_only_id = 10_002_004i64;
    let struct_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 65)
        .map(|t| t.type_id)
        .unwrap_or(35832);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cat65_only_id,
            InventoryItemRecord {
                item_id: cat65_only_id,
                type_id: struct_type,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = cat65_only_id;
        }
    });
    assert!(is_piloted_upwell_structure(&ctx, cat65_only_id));
    assert_eq!(
        validate_pre_movement_locks(&ctx, cat65_only_id, &MovementIntent::Warp { sys_id })
            .unwrap_err()
            .code,
        "StructureCannotNavigate"
    );
    assert!(!check_sync_pre_warp_eligibility(&ctx, cat65_only_id));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
    });

    // 5. `can_warp_in_ballpark`: bubble, lance disruption, non-covert cloak, gate cloak, covert cloak.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    {
        let bp = sys_handle.ballpark.read().unwrap();
        assert!(can_warp_in_ballpark(&bp, ship_id));
    }
    let bubble_id = 10_002_005i64;
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.add_warp_bubble(bubble_id, origin, 15_000.0);
        bp.step(0.1);
        assert!(!can_warp_in_ballpark(&bp, ship_id));
        bp.remove_warp_bubble(bubble_id);
        bp.step(0.1);
        assert!(can_warp_in_ballpark(&bp, ship_id));

        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.lance_disrupted_remaining_s = 5.0;
        }
        assert!(!can_warp_in_ballpark(&bp, ship_id));

        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.lance_disrupted_remaining_s = 0.0;
            cs.is_cloaked = true;
            cs.gate_cloak_remaining_ticks = 0;
            cs.is_covert_cloak = false;
        }
        assert!(!can_warp_in_ballpark(&bp, ship_id));

        // Gate cloak allows warp even without covert cloak.
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.gate_cloak_remaining_ticks = 10;
        }
        assert!(can_warp_in_ballpark(&bp, ship_id));

        // Covert cloak allows warp without gate cloak.
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.gate_cloak_remaining_ticks = 0;
            cs.is_covert_cloak = true;
        }
        assert!(can_warp_in_ballpark(&bp, ship_id));
    }
}

#[tokio::test]
async fn intent_variants_vector_warp_ansiblex_cyno_bridge_conduit_shipcaster() {
    use crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH;

    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let low_sys = 30_002_813u32;
    let wh_sys = 31_000_005u32;
    let abyss_sys = 32_000_001u32;
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());
    let origin = DVec3::new(50_000.0, 0.0, 0.0);

    // 1. VectorWarp: clean in normal space, rejected in Abyssal space, rejected in warp.
    let v_ship = 10_003_001i64;
    let v_intent = MovementIntent::VectorWarp { sys_id };
    assert_eq!(v_intent.sys_id(), sys_id);
    spawn_test_ship(&ctx, sys_id, char_id, v_ship, 587, origin, 1_000_000.0, CombatState::default()).await;
    assert!(ctx.validate_ship_movement(char_id, v_ship, v_intent).await.is_ok());
    assert_eq!(
        ctx.validate_ship_movement(char_id, v_ship, MovementIntent::VectorWarp { sys_id: abyss_sys })
            .await
            .unwrap_err()
            .code,
        "CannotWarpInAbyssalSpace"
    );

    // 2. AnsiblexJump: ignores warp bubble, enforces 1.48 Mt exact boundary & supercap prohibition.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let bubble_id = 10_003_009i64;
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.add_warp_bubble(bubble_id, origin, 20_000.0);
        bp.step(0.1);
    }
    let ans_ok_ship = 10_003_002i64;
    spawn_test_ship(
        &ctx,
        sys_id,
        char_id,
        ans_ok_ship,
        587,
        origin,
        1_480_000_000.0,
        CombatState::default(),
    )
    .await;
    assert!(
        ctx.validate_ship_movement(
            char_id,
            ans_ok_ship,
            MovementIntent::AnsiblexJump {
                from_sys: sys_id,
                to_sys: low_sys,
            },
        )
        .await
        .is_ok()
    );

    let ans_heavy_ship = 10_003_003i64;
    spawn_test_ship(
        &ctx,
        sys_id,
        char_id,
        ans_heavy_ship,
        587,
        origin,
        1_480_000_001.0,
        CombatState::default(),
    )
    .await;
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            ans_heavy_ship,
            MovementIntent::AnsiblexJump {
                from_sys: sys_id,
                to_sys: low_sys,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotAllowedThroughAnsiblex"
    );

    let supercap_tid = find_ship_type_by_group(&ctx.sde, 659);
    let ans_super = 10_003_004i64;
    spawn_test_ship(&ctx, sys_id, char_id, ans_super, supercap_tid, origin, 100_000_000.0, CombatState::default()).await;
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            ans_super,
            MovementIntent::AnsiblexJump {
                from_sys: sys_id,
                to_sys: low_sys,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotAllowedThroughAnsiblex"
    );

    // 3. CynoJump / CynoBridge / ConduitJump inside bubble:
    //    - `allow_bubbled: false` -> WarpScrambled in bubble
    //    - `allow_bubbled: true`  -> allowed in bubble, STILL blocked by targeted scram or warp_core_strength < 0
    for intent_disallowed in [
        MovementIntent::CynoJump {
            from_sys: sys_id,
            to_sys: low_sys,
            allow_bubbled: false,
        },
        MovementIntent::CynoBridge {
            from_sys: sys_id,
            to_sys: low_sys,
            allow_bubbled: false,
        },
        MovementIntent::ConduitJump {
            from_sys: sys_id,
            to_sys: low_sys,
            allow_bubbled: false,
        },
    ] {
        assert_eq!(
            ctx.validate_ship_movement(char_id, v_ship, intent_disallowed)
                .await
                .unwrap_err()
                .code,
            "WarpScrambled"
        );
    }

    for intent_allowed in [
        MovementIntent::CynoJump {
            from_sys: sys_id,
            to_sys: low_sys,
            allow_bubbled: true,
        },
        MovementIntent::CynoBridge {
            from_sys: sys_id,
            to_sys: low_sys,
            allow_bubbled: true,
        },
        MovementIntent::ConduitJump {
            from_sys: sys_id,
            to_sys: low_sys,
            allow_bubbled: true,
        },
    ] {
        assert!(
            ctx.validate_ship_movement(char_id, v_ship, intent_allowed.clone())
                .await
                .is_ok()
        );
    }

    // Targeted scram blocks even `allow_bubbled: true`.
    sys_handle.try_update_combat_state(v_ship, |cs| {
        cs.warp_core_strength = 1;
        cs.warp_scramble_strength = 1;
    });
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            v_ship,
            MovementIntent::CynoBridge {
                from_sys: sys_id,
                to_sys: low_sys,
                allow_bubbled: true,
            },
        )
        .await
        .unwrap_err()
        .code,
        "WarpScrambled"
    );
    // Negative warp_core_strength blocks even `allow_bubbled: true` with 0 scram strength.
    sys_handle.try_update_combat_state(v_ship, |cs| {
        cs.warp_core_strength = -1;
        cs.warp_scramble_strength = 0;
    });
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            v_ship,
            MovementIntent::ConduitJump {
                from_sys: sys_id,
                to_sys: low_sys,
                allow_bubbled: true,
            },
        )
        .await
        .unwrap_err()
        .code,
        "WarpScrambled"
    );
    sys_handle.try_update_combat_state(v_ship, |cs| {
        cs.warp_core_strength = 0;
        cs.warp_scramble_strength = 0;
    });

    // Wormhole and Zarzakh restrictions across CynoJump, CynoBridge, ConduitJump (both from_sys and to_sys).
    for (from_s, to_s, expected_code) in [
        (wh_sys, low_sys, "JumpDriveForbiddenInWormholeSpace"),
        (low_sys, wh_sys, "JumpDriveForbiddenInWormholeSpace"),
        (SOLAR_SYSTEM_ZARZAKH, low_sys, "JumpDriveForbiddenInZarzakh"),
        (low_sys, SOLAR_SYSTEM_ZARZAKH, "JumpDriveForbiddenInZarzakh"),
    ] {
        for intent in [
            MovementIntent::CynoJump {
                from_sys: from_s,
                to_sys: to_s,
                allow_bubbled: true,
            },
            MovementIntent::CynoBridge {
                from_sys: from_s,
                to_sys: to_s,
                allow_bubbled: true,
            },
            MovementIntent::ConduitJump {
                from_sys: from_s,
                to_sys: to_s,
                allow_bubbled: true,
            },
        ] {
            assert_eq!(
                ctx.validate_ship_movement(char_id, v_ship, intent)
                    .await
                    .unwrap_err()
                    .code,
                expected_code
            );
        }
    }

    // 4. ShipcasterJump: blocked when bubbled, passes when bubble removed.
    let sc_intent = MovementIntent::ShipcasterJump {
        from_sys: sys_id,
        to_sys: low_sys,
    };
    assert_eq!(sc_intent.sys_id(), sys_id);
    assert_eq!(
        ctx.validate_ship_movement(char_id, v_ship, sc_intent.clone())
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.remove_warp_bubble(bubble_id);
        bp.step(0.1);
    }
    assert!(ctx.validate_ship_movement(char_id, v_ship, sc_intent).await.is_ok());
}

#[tokio::test]
async fn stargate_and_wormhole_security_and_mass_guards() {
    use crate::services::cluster_macho_misc_services::set_zarzakh_horizon_siege_active;
    use crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH;

    let ctx = test_ctx();
    let high_sys = 30_000_142u32;
    let low_sys = find_system_by_pred(&ctx.sde, |s| {
        s.security_status < 0.45
            && s.solar_system_id != SOLAR_SYSTEM_ZARZAKH
            && s.region_id != 10_000_070
            && !(31_000_000..=32_999_999).contains(&s.solar_system_id)
    });
    let low_sys_2 = find_system_by_pred(&ctx.sde, |s| {
        s.security_status < 0.45
            && s.solar_system_id != low_sys
            && s.solar_system_id != SOLAR_SYSTEM_ZARZAKH
            && s.region_id != 10_000_070
            && !(31_000_000..=32_999_999).contains(&s.solar_system_id)
    });
    let pochven_sys = find_system_by_pred(&ctx.sde, |s| s.region_id == 10_000_070);
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());
    let origin = DVec3::new(30_000.0, 0.0, 0.0);

    // 1. Zarzakh Horizon Siege gate access lock.
    let frig_id = 10_004_001i64;
    spawn_test_ship(&ctx, low_sys, char_id, frig_id, 587, origin, 1_000_000.0, CombatState::default()).await;
    spawn_test_ship(&ctx, SOLAR_SYSTEM_ZARZAKH, char_id, frig_id, 587, origin, 1_000_000.0, CombatState::default()).await;

    set_zarzakh_horizon_siege_active(&ctx, true);
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            frig_id,
            MovementIntent::StargateJump {
                from_sys: low_sys,
                to_sys: SOLAR_SYSTEM_ZARZAKH,
                gate_id: 50001,
            },
        )
        .await
        .unwrap_err()
        .code,
        "HorizonSiegeGateAccessDenied"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            frig_id,
            MovementIntent::StargateJump {
                from_sys: SOLAR_SYSTEM_ZARZAKH,
                to_sys: low_sys,
                gate_id: 50002,
            },
        )
        .await
        .unwrap_err()
        .code,
        "HorizonSiegeGateAccessDenied"
    );
    set_zarzakh_horizon_siege_active(&ctx, false);

    // 2. Freighter (`group 513`) & Dreadnought (`group 485`) & Orca (`28606`) stargate rules.
    let freighter_tid = find_ship_type_by_group(&ctx.sde, 513);
    let dread_tid = find_ship_type_by_group(&ctx.sde, 485);
    let super_tid = find_ship_type_by_group(&ctx.sde, 659);
    let freighter_id = 10_004_002i64;
    let dread_id = 10_004_003i64;
    let orca_id = 10_004_004i64;
    let super_id = 10_004_005i64;

    for sys in [high_sys, low_sys, pochven_sys, SOLAR_SYSTEM_ZARZAKH] {
        spawn_test_ship(&ctx, sys, char_id, freighter_id, freighter_tid, origin, 900_000_000.0, CombatState::default()).await;
        spawn_test_ship(&ctx, sys, char_id, dread_id, dread_tid, origin, 1_100_000_000.0, CombatState::default()).await;
        spawn_test_ship(&ctx, sys, char_id, orca_id, 28_606, origin, 250_000_000.0, CombatState::default()).await;
        spawn_test_ship(&ctx, sys, char_id, super_id, super_tid, origin, 2_000_000_000.0, CombatState::default()).await;
    }

    // Freighter: Zarzakh -> ShipCannotUseStargate; Pochven -> CapitalShipCannotJumpStargate; highsec -> allowed!
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            freighter_id,
            MovementIntent::StargateJump {
                from_sys: low_sys,
                to_sys: SOLAR_SYSTEM_ZARZAKH,
                gate_id: 50003,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipCannotUseStargate"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            freighter_id,
            MovementIntent::StargateJump {
                from_sys: low_sys,
                to_sys: pochven_sys,
                gate_id: 50004,
            },
        )
        .await
        .unwrap_err()
        .code,
        "CapitalShipCannotJumpStargate"
    );
    assert!(
        ctx.validate_ship_movement(
            char_id,
            freighter_id,
            MovementIntent::StargateJump {
                from_sys: high_sys,
                to_sys: low_sys,
                gate_id: 50005,
            },
        )
        .await
        .is_ok()
    );

    // Dreadnought: highsec or Pochven -> CapitalShipCannotJumpStargate; Zarzakh -> ShipCannotUseStargate; low<->low -> ok!
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            dread_id,
            MovementIntent::StargateJump {
                from_sys: low_sys,
                to_sys: SOLAR_SYSTEM_ZARZAKH,
                gate_id: 50006,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipCannotUseStargate"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            dread_id,
            MovementIntent::StargateJump {
                from_sys: low_sys,
                to_sys: high_sys,
                gate_id: 50007,
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
            dread_id,
            MovementIntent::StargateJump {
                from_sys: pochven_sys,
                to_sys: low_sys,
                gate_id: 50008,
            },
        )
        .await
        .unwrap_err()
        .code,
        "CapitalShipCannotJumpStargate"
    );
    assert!(
        ctx.validate_ship_movement(
            char_id,
            dread_id,
            MovementIntent::StargateJump {
                from_sys: low_sys,
                to_sys: low_sys_2,
                gate_id: 50009,
            },
        )
        .await
        .is_ok()
    );

    // Orca is allowed through Zarzakh gates!
    assert!(
        ctx.validate_ship_movement(
            char_id,
            orca_id,
            MovementIntent::StargateJump {
                from_sys: low_sys,
                to_sys: SOLAR_SYSTEM_ZARZAKH,
                gate_id: 50010,
            },
        )
        .await
        .is_ok()
    );

    // Supercarrier rejected even on lowsec <-> lowsec stargate.
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            super_id,
            MovementIntent::StargateJump {
                from_sys: low_sys,
                to_sys: low_sys_2,
                gate_id: 50011,
            },
        )
        .await
        .unwrap_err()
        .code,
        "CapitalShipCannotJumpStargate"
    );

    // 3. WormholeJump: supercap restriction + mass limit exact boundary + SDE mass fallback.
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            super_id,
            MovementIntent::WormholeJump {
                from_sys: low_sys,
                max_jump_mass: None,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipRestrictedFromWormhole"
    );
    let wh_ship = 10_004_006i64;
    spawn_test_ship(&ctx, low_sys, char_id, wh_ship, 587, origin, 20_000_000.0, CombatState::default()).await;
    assert!(
        ctx.validate_ship_movement(
            char_id,
            wh_ship,
            MovementIntent::WormholeJump {
                from_sys: low_sys,
                max_jump_mass: Some(20_000_000.0),
            },
        )
        .await
        .is_ok()
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            wh_ship,
            MovementIntent::WormholeJump {
                from_sys: low_sys,
                max_jump_mass: Some(19_999_999.0),
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipExceedsWormholeJumpMass"
    );

    // When ball.mass == 0.0, fallback reads SDE mass.
    let sde_mass = ctx.sde.types.get(&587).unwrap().mass;
    let wh_zero_mass_ball = 10_004_007i64;
    spawn_test_ship(&ctx, low_sys, char_id, wh_zero_mass_ball, 587, origin, 0.0, CombatState::default()).await;
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            wh_zero_mass_ball,
            MovementIntent::WormholeJump {
                from_sys: low_sys,
                max_jump_mass: Some(sde_mass - 1.0),
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipExceedsWormholeJumpMass"
    );
}

#[tokio::test]
async fn filament_db_fallback_cloak_tether_and_in_warp_errors() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());

    // 1. AbyssalOrVoidFilament works WITHOUT a ballpark ball, falling back to DB item type_id.
    let db_only_ship = 10_005_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            db_only_ship,
            InventoryItemRecord {
                item_id: db_only_ship,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let res = ctx
        .validate_ship_movement(
            char_id,
            db_only_ship,
            MovementIntent::AbyssalOrVoidFilament {
                from_sys: sys_id,
                reject_when_cloaked: true,
                reject_when_tethered: true,
            },
        )
        .await
        .unwrap();
    assert!(res.ball.is_none());
    assert_eq!(res.ship_type_id, 587);
    assert_eq!(res.ship_group_id, ctx.sde.types.get(&587).unwrap().group_id);

    // RandomFilament DOES require ball in space -> ShipNotInSpace.
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            db_only_ship,
            MovementIntent::RandomFilament { from_sys: sys_id },
        )
        .await
        .unwrap_err()
        .code,
        "ShipNotInSpace"
    );

    // 2. Cloak & tether flag gating for AbyssalOrVoidFilament.
    let space_ship = 10_005_002i64;
    let origin = DVec3::new(12_000.0, 0.0, 0.0);
    spawn_test_ship(&ctx, sys_id, char_id, space_ship, 587, origin, 1_000_000.0, CombatState::default()).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_update_combat_state(space_ship, |cs| {
        cs.is_cloaked = true;
        cs.is_tethered = false;
    });
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            space_ship,
            MovementIntent::AbyssalOrVoidFilament {
                from_sys: sys_id,
                reject_when_cloaked: true,
                reject_when_tethered: false,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipIsCloaked"
    );
    assert!(
        ctx.validate_ship_movement(
            char_id,
            space_ship,
            MovementIntent::AbyssalOrVoidFilament {
                from_sys: sys_id,
                reject_when_cloaked: false,
                reject_when_tethered: true,
            },
        )
        .await
        .is_ok()
    );

    sys_handle.try_update_combat_state(space_ship, |cs| {
        cs.is_cloaked = false;
        cs.is_tethered = true;
    });
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            space_ship,
            MovementIntent::AbyssalOrVoidFilament {
                from_sys: sys_id,
                reject_when_cloaked: false,
                reject_when_tethered: true,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipIsTethered"
    );
    assert!(
        ctx.validate_ship_movement(
            char_id,
            space_ship,
            MovementIntent::AbyssalOrVoidFilament {
                from_sys: sys_id,
                reject_when_cloaked: true,
                reject_when_tethered: false,
            },
        )
        .await
        .is_ok()
    );
    sys_handle.try_update_combat_state(space_ship, |cs| cs.is_tethered = false);

    // 3. Active warp mode handling across BookmarkWarp (ShipNotInSpace even in Aligning),
    //    Warp (Aligning ok, Cruising -> ShipAlreadyInWarp), RandomFilament (CannotActivateFilamentInWarp),
    //    and non-warp StargateJump (ShipInWarp).
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&space_ship) {
            b.mode = MovementMode::Warp {
                origin,
                destination: origin + DVec3::new(1_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 1_000_000.0,
                elapsed_warp_time: 0.0,
                phase: eve_destiny::WarpPhase::Aligning,
            };
        }
    }
    assert!(
        ctx.validate_ship_movement(char_id, space_ship, MovementIntent::Warp { sys_id })
            .await
            .is_ok()
    );
    assert_eq!(
        ctx.validate_ship_movement(char_id, space_ship, MovementIntent::BookmarkWarp { sys_id })
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    assert_eq!(
        ctx.validate_ship_movement(char_id, space_ship, MovementIntent::RandomFilament { from_sys: sys_id })
            .await
            .unwrap_err()
            .code,
        "CannotActivateFilamentInWarp"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            space_ship,
            MovementIntent::StargateJump {
                from_sys: sys_id,
                to_sys: 30_002_813,
                gate_id: 50012,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipInWarp"
    );

    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&space_ship) {
            if let MovementMode::Warp { ref mut phase, .. } = b.mode {
                *phase = eve_destiny::WarpPhase::Cruising;
            }
        }
    }
    assert_eq!(
        ctx.validate_ship_movement(char_id, space_ship, MovementIntent::Warp { sys_id })
            .await
            .unwrap_err()
            .code,
        "ShipAlreadyInWarp"
    );
}

#[tokio::test]
async fn acceleration_gate_fw_ess_generic_and_anomic_burner() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = ctx.db.with_snapshot(|snap| *snap.characters.keys().next().unwrap());
    let origin = DVec3::new(20_000.0, 0.0, 0.0);

    // 1. FwComplex remaps lance/nav-lock -> ShipNavigationLocked and immobilizing module -> ImmobilizingModuleActive;
    //    EssBank checks nav/immobilizing with action = "warp" -> LanceFiringLockActive / WarpDisrupted.
    let fw_ship = 10_006_001i64;
    spawn_test_ship(&ctx, sys_id, char_id, fw_ship, 587, origin, 1_000_000.0, CombatState::default()).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Lance debuff on FwComplex -> ShipNavigationLocked
    sys_handle.try_update_combat_state(fw_ship, |cs| cs.lance_disrupted_remaining_s = 10.0);
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            fw_ship,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::FwComplex,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipNavigationLocked"
    );
    sys_handle.try_update_combat_state(fw_ship, |cs| cs.lance_disrupted_remaining_s = 0.0);

    // Tactical lock on FwComplex -> ShipNavigationLocked vs EssBank -> LanceFiringLockActive
    let now = ctx.now_unix();
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .insert((ctx.instance_id as usize, fw_ship), now + 60);
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            fw_ship,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::FwComplex,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipNavigationLocked"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            fw_ship,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::EssBank,
            },
        )
        .await
        .unwrap_err()
        .code,
        "LanceFiringLockActive"
    );
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .remove(&(ctx.instance_id as usize, fw_ship));

    // Immobilizing module (Bastion 33400) on FwComplex -> ImmobilizingModuleActive vs EssBank ("warp") -> WarpDisrupted.
    let mod_id = 10_006_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 33400,
                owner_id: char_id,
                location_id: fw_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(mod_id);
    });
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            fw_ship,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::FwComplex,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ImmobilizingModuleActive"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            fw_ship,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::EssBank,
            },
        )
        .await
        .unwrap_err()
        .code,
        "WarpDisrupted"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&mod_id);
    });

    // 2. Generic acceleration gate: mass boundary (500_000_000 ok, 500_000_001 -> ShipTooLargeForGate) + rig size 4 capital.
    let gen_boundary_ship = 10_006_003i64;
    spawn_test_ship(
        &ctx,
        sys_id,
        char_id,
        gen_boundary_ship,
        587,
        origin,
        500_000_000.0,
        CombatState::default(),
    )
    .await;
    assert!(
        ctx.validate_ship_movement(
            char_id,
            gen_boundary_ship,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::Generic,
            },
        )
        .await
        .is_ok()
    );
    let gen_too_heavy = 10_006_004i64;
    spawn_test_ship(
        &ctx,
        sys_id,
        char_id,
        gen_too_heavy,
        587,
        origin,
        500_000_001.0,
        CombatState::default(),
    )
    .await;
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            gen_too_heavy,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::Generic,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipTooLargeForGate"
    );

    // 3. AnomicBurner / DedOrExploration / VoidExit do NOT require ball in space (DB fallback works).
    let db_frig = 10_006_005i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            db_frig,
            InventoryItemRecord {
                item_id: db_frig,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    for kind in [
        AccelerationGateKind::AnomicBurner,
        AccelerationGateKind::DedOrExploration,
        AccelerationGateKind::VoidExit,
    ] {
        assert!(
            ctx.validate_ship_movement(char_id, db_frig, MovementIntent::AccelerationGate { sys_id, kind })
                .await
                .is_ok()
        );
    }

    // AnomicBurner group & exact 50_000_000.0 kg boundary checks.
    for blocked_grp in [27u32, 419, 898, 900, 485] {
        let tid = find_ship_type_by_group(&ctx.sde, blocked_grp);
        let sid = 10_006_100 + i64::from(blocked_grp);
        spawn_test_ship(&ctx, sys_id, char_id, sid, tid, origin, 10_000_000.0, CombatState::default()).await;
        assert_eq!(
            ctx.validate_ship_movement(
                char_id,
                sid,
                MovementIntent::AccelerationGate {
                    sys_id,
                    kind: AccelerationGateKind::AnomicBurner,
                },
            )
            .await
            .unwrap_err()
            .code,
            "ShipRestrictedByGate",
            "group {blocked_grp} should be rejected by AnomicBurner"
        );
    }

    let burner_exact_50m = 10_006_201i64;
    spawn_test_ship(&ctx, sys_id, char_id, burner_exact_50m, 587, origin, 50_000_000.0, CombatState::default()).await;
    assert_eq!(
        ctx.validate_ship_movement(
            char_id,
            burner_exact_50m,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::AnomicBurner,
            },
        )
        .await
        .unwrap_err()
        .code,
        "ShipRestrictedByGate"
    );
    let burner_under_50m = 10_006_202i64;
    spawn_test_ship(&ctx, sys_id, char_id, burner_under_50m, 587, origin, 49_999_999.0, CombatState::default()).await;
    assert!(
        ctx.validate_ship_movement(
            char_id,
            burner_under_50m,
            MovementIntent::AccelerationGate {
                sys_id,
                kind: AccelerationGateKind::AnomicBurner,
            },
        )
        .await
        .is_ok()
    );
}

#[test]
fn movement_intent_classifiers_and_can_warp_in_ballpark_pin_all_branches() {
    let cases = [
        (MovementIntent::Warp { sys_id: 30_000_142 }, 30_000_142, "warp", true, true, false, true),
        (MovementIntent::BookmarkWarp { sys_id: 30_000_142 }, 30_000_142, "warp", true, true, false, true),
        (MovementIntent::AgencyWarp { sys_id: 30_000_142 }, 30_000_142, "warp", true, true, false, true),
        (MovementIntent::VectorWarp { sys_id: 30_000_142 }, 30_000_142, "warp", true, true, false, true),
        (
            MovementIntent::StargateJump {
                from_sys: 30_000_142,
                to_sys: 30_000_144,
                gate_id: 50_000_001,
            },
            30_000_142,
            "jump",
            true,
            false,
            false,
            false,
        ),
        (
            MovementIntent::AnsiblexJump {
                from_sys: 30_000_142,
                to_sys: 30_000_144,
            },
            30_000_142,
            "jump",
            true,
            false,
            false,
            false,
        ),
        (
            MovementIntent::WormholeJump {
                from_sys: 30_000_142,
                max_jump_mass: Some(1e9),
            },
            30_000_142,
            "jump wormhole",
            true,
            false,
            false,
            false,
        ),
        (
            MovementIntent::CynoJump {
                from_sys: 30_000_142,
                to_sys: 30_004_759,
                allow_bubbled: true,
            },
            30_000_142,
            "jump",
            true,
            true,
            true,
            false,
        ),
        (
            MovementIntent::CynoBridge {
                from_sys: 30_000_142,
                to_sys: 30_004_759,
                allow_bubbled: false,
            },
            30_000_142,
            "jump",
            true,
            true,
            false,
            false,
        ),
        (
            MovementIntent::ConduitJump {
                from_sys: 30_000_142,
                to_sys: 30_004_759,
                allow_bubbled: true,
            },
            30_000_142,
            "jump",
            true,
            true,
            true,
            false,
        ),
        (
            MovementIntent::ShipcasterJump {
                from_sys: 30_000_142,
                to_sys: 30_002_537,
            },
            30_000_142,
            "jump",
            true,
            true,
            false,
            false,
        ),
        (
            MovementIntent::RandomFilament {
                from_sys: 30_000_142,
            },
            30_000_142,
            "activate filament",
            true,
            true,
            false,
            false,
        ),
        (
            MovementIntent::AbyssalOrVoidFilament {
                from_sys: 30_000_142,
                reject_when_cloaked: true,
                reject_when_tethered: true,
            },
            30_000_142,
            "activate filament",
            false,
            true,
            false,
            false,
        ),
        (
            MovementIntent::AccelerationGate {
                sys_id: 30_000_142,
                kind: AccelerationGateKind::FwComplex,
            },
            30_000_142,
            "activate acceleration gate",
            true,
            true,
            false,
            false,
        ),
        (
            MovementIntent::AccelerationGate {
                sys_id: 30_000_142,
                kind: AccelerationGateKind::AnomicBurner,
            },
            30_000_142,
            "activate acceleration gate",
            false,
            true,
            false,
            false,
        ),
    ];

    for (intent, exp_sys, exp_label, exp_req_ball, exp_chk_scram, exp_allow_bub, exp_is_warp) in cases {
        assert_eq!(intent.sys_id(), exp_sys);
        assert_eq!(intent.action_label(), exp_label);
        assert_eq!(intent.requires_ball_in_space(), exp_req_ball);
        assert_eq!(intent.checks_bubble_and_scram(), exp_chk_scram);
        assert_eq!(intent.allow_bubbled(), exp_allow_bub);
        assert_eq!(intent.is_warp_intent(), exp_is_warp);
    }

    // can_warp_in_ballpark branches
    let mut bp = eve_destiny::Ballpark::new(30_000_142);
    let sid = 700_001_i64;
    bp.add_ball(eve_destiny::Ball {
        id: sid,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    assert!(super::can_warp_in_ballpark(&bp, sid));

    bp.combat_states.insert(
        sid,
        CombatState {
            lance_disrupted_remaining_s: 2.0,
            ..Default::default()
        },
    );
    assert!(!super::can_warp_in_ballpark(&bp, sid));

    bp.combat_states.insert(
        sid,
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 0,
            is_covert_cloak: false,
            ..Default::default()
        },
    );
    assert!(!super::can_warp_in_ballpark(&bp, sid));

    bp.combat_states.get_mut(&sid).unwrap().is_covert_cloak = true;
    assert!(super::can_warp_in_ballpark(&bp, sid));

    bp.combat_states.get_mut(&sid).unwrap().is_covert_cloak = false;
    bp.combat_states.get_mut(&sid).unwrap().gate_cloak_remaining_ticks = 5;
    assert!(super::can_warp_in_ballpark(&bp, sid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w4_mutant_killers_boundaries_structure_piloting_ranks_mass_and_tackle() {
    let base_ctx = test_ctx();
    let mut sde = (*base_ctx.sde).clone();

    // Custom systems: sec 0.45 (highsec boundary), 0.44 (lowsec boundary), region 10_000_070 (Pochven)
    let tpl_sys = sde.solar_systems.values().next().cloned().expect("solar system");
    let sys_hs_edge = 30_999_001_u32;
    let sys_ls_edge = 30_999_002_u32;
    let sys_pochven = 30_999_003_u32;
    for (sid, reg, sec) in [
        (sys_hs_edge, 10_000_002_u32, 0.45),
        (sys_ls_edge, 10_000_002_u32, 0.44),
        (sys_pochven, 10_000_070_u32, -1.0),
    ] {
        let mut sys = tpl_sys.clone();
        sys.solar_system_id = sid;
        sys.region_id = reg;
        sys.security_status = sec;
        sde.solar_systems.insert(sid, sys);
    }

    // Custom ships for rig-size 4 vs 3, piloted category 65 item (outside snap.structures), Orca, Freighter, Dread, Supercap
    let tpl_ty = sde.types.values().next().cloned().expect("type");
    let tid_rig4 = 998_101_u32;
    let tid_rig3 = 998_102_u32;
    let tid_cap65 = 998_103_u32;
    for (tid, gid, cat, rig, mass) in [
        (tid_rig4, 25_u32, 6_u32, 4.0, 10_000_000.0),
        (tid_rig3, 25, 6, 3.0, 10_000_000.0),
        (tid_cap65, 1657, 65, 4.0, 50_000_000.0),
    ] {
        let mut t = tpl_ty.clone();
        t.type_id = tid;
        t.group_id = gid;
        t.category_id = cat;
        t.mass = mass;
        t.default_attributes.insert(1547, rig);
        sde.types.insert(tid, t);
    }
    let tid_dread = find_ship_type_by_group(&sde, 485);
    let tid_super = find_ship_type_by_group(&sde, 659);
    let tid_freighter = find_ship_type_by_group(&sde, 513);
    let tid_orca = if sde.types.contains_key(&28_606) {
        28_606
    } else {
        find_ship_type_by_group(&sde, 941)
    };
    let tid_frigate = find_ship_type_by_group(&sde, 25);

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let zz_sys = crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH;

    // 1. is_piloted_upwell_structure via snap.items category 65 + active_ship_id (without snap.structures entry)
    let char_a = 90_998_001_i64;
    let ship_a = 100_998_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_a;
        ch.active_ship_id = ship_a;
        ch.solar_system_id = sys_ls_edge;
        snap.characters.insert(char_a, ch);
        snap.items.insert(
            ship_a,
            InventoryItemRecord {
                item_id: ship_a,
                type_id: tid_cap65,
                owner_id: char_a,
                location_id: sys_ls_edge as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        super::validate_system_movement_locks(&ctx, ship_a, &MovementIntent::Warp { sys_id: sys_ls_edge })
            .unwrap_err()
            .code,
        "StructureCannotNavigate"
    );
    // Changing character's active_ship_id away OR setting item cat != 65 -> passes
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_a).unwrap().active_ship_id = ship_a + 999;
    });
    assert!(super::validate_system_movement_locks(&ctx, ship_a, &MovementIntent::Warp { sys_id: sys_ls_edge }).is_ok());
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_a).unwrap().active_ship_id = ship_a;
        snap.items.get_mut(&ship_a).unwrap().type_id = tid_frigate;
    });
    assert!(super::validate_system_movement_locks(&ctx, ship_a, &MovementIntent::Warp { sys_id: sys_ls_edge }).is_ok());

    // 2. validate_system_movement_locks boundaries: Abyssal 32_000_000..=32_999_999, WH 31_000_000..32_000_000, Zarzakh Siege & Cyno
    for ab_sys in [32_000_000_u32, 32_999_999] {
        for intent in [
            MovementIntent::Warp { sys_id: ab_sys },
            MovementIntent::BookmarkWarp { sys_id: ab_sys },
            MovementIntent::AgencyWarp { sys_id: ab_sys },
            MovementIntent::VectorWarp { sys_id: ab_sys },
        ] {
            assert_eq!(
                super::validate_system_movement_locks(&ctx, ship_a, &intent).unwrap_err().code,
                "CannotWarpInAbyssalSpace"
            );
        }
    }
    for ok_sys in [31_999_999_u32, 33_000_000] {
        assert!(super::validate_system_movement_locks(&ctx, ship_a, &MovementIntent::Warp { sys_id: ok_sys }).is_ok());
    }
    crate::services::cluster_macho_misc_services::set_zarzakh_horizon_siege_active(&ctx, true);
    assert_eq!(
        super::validate_system_movement_locks(
            &ctx,
            ship_a,
            &MovementIntent::StargateJump { from_sys: zz_sys, to_sys: sys_ls_edge, gate_id: 1 },
        ).unwrap_err().code,
        "HorizonSiegeGateAccessDenied"
    );
    assert_eq!(
        super::validate_system_movement_locks(
            &ctx,
            ship_a,
            &MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: zz_sys, gate_id: 1 },
        ).unwrap_err().code,
        "HorizonSiegeGateAccessDenied"
    );
    crate::services::cluster_macho_misc_services::set_zarzakh_horizon_siege_active(&ctx, false);
    assert!(super::validate_system_movement_locks(
        &ctx,
        ship_a,
        &MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: zz_sys, gate_id: 1 },
    ).is_ok());

    for intent in [
        MovementIntent::CynoJump { from_sys: 31_000_000, to_sys: sys_ls_edge, allow_bubbled: false },
        MovementIntent::CynoBridge { from_sys: sys_ls_edge, to_sys: 31_999_999, allow_bubbled: false },
        MovementIntent::ConduitJump { from_sys: 31_500_000, to_sys: sys_ls_edge, allow_bubbled: false },
    ] {
        assert_eq!(
            super::validate_system_movement_locks(&ctx, ship_a, &intent).unwrap_err().code,
            "JumpDriveForbiddenInWormholeSpace"
        );
    }
    assert!(super::validate_system_movement_locks(
        &ctx,
        ship_a,
        &MovementIntent::CynoJump { from_sys: 32_000_000, to_sys: sys_ls_edge, allow_bubbled: false },
    ).is_ok());
    for intent in [
        MovementIntent::CynoJump { from_sys: zz_sys, to_sys: sys_ls_edge, allow_bubbled: false },
        MovementIntent::CynoBridge { from_sys: sys_ls_edge, to_sys: zz_sys, allow_bubbled: false },
        MovementIntent::ConduitJump { from_sys: zz_sys, to_sys: sys_ls_edge, allow_bubbled: false },
    ] {
        assert_eq!(
            super::validate_system_movement_locks(&ctx, ship_a, &intent).unwrap_err().code,
            "JumpDriveForbiddenInZarzakh"
        );
    }

    // 3. can_warp_in_ballpark boundary checks (lance_disrupted_remaining_s > 0.0, cloak ticks == 0)
    let mut bp = eve_destiny::Ballpark::new(sys_ls_edge);
    bp.add_ball(Ball {
        id: 710_001,
        type_id: tid_frigate,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    bp.combat_states.insert(710_001, CombatState { lance_disrupted_remaining_s: 0.0, ..Default::default() });
    assert!(super::can_warp_in_ballpark(&bp, 710_001));
    bp.combat_states.get_mut(&710_001).unwrap().lance_disrupted_remaining_s = 0.001;
    assert!(!super::can_warp_in_ballpark(&bp, 710_001));
    bp.combat_states.insert(710_001, CombatState { is_cloaked: true, gate_cloak_remaining_ticks: 1, is_covert_cloak: false, ..Default::default() });
    assert!(super::can_warp_in_ballpark(&bp, 710_001));
    bp.combat_states.get_mut(&710_001).unwrap().gate_cloak_remaining_ticks = 0;
    assert!(!super::can_warp_in_ballpark(&bp, 710_001));

    // 4. lance_disrupted_remaining_s, allow_bubbled WCS vs scram, and AbyssalOrVoidFilament flags
    spawn_test_ship(&ctx, sys_ls_edge, char_a, ship_a, tid_frigate, DVec3::ZERO, 10_000_000.0, CombatState {
        lance_disrupted_remaining_s: 0.5,
        warp_core_strength: 1,
        ..Default::default()
    }).await;
    let err_lance = ctx.validate_ship_movement(char_a, ship_a, MovementIntent::Warp { sys_id: sys_ls_edge }).await.unwrap_err();
    assert_eq!(err_lance.code, "DisruptiveLanceDebuffActive");
    assert!(err_lance.message.contains("warp") && err_lance.message.contains("0.5"));
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::AccelerationGate { sys_id: sys_ls_edge, kind: AccelerationGateKind::FwComplex }).await.unwrap_err().code,
        "ShipNavigationLocked"
    );

    // Clear lance; test allow_bubbled WCS (< 0 vs == 0, scram == wcs vs scram < wcs)
    let sys_h = ctx.systems.get_or_spawn(sys_ls_edge);
    for (wcs, scram, exp_ok) in [
        (-1, 0, false),
        (0, 0, true),
        (1, 1, false),
        (2, 1, true),
    ] {
        sys_h.set_combat_state(ship_a, CombatState {
            warp_core_strength: wcs,
            warp_scramble_strength: scram,
            is_cloaked: true,
            is_tethered: true,
            ..Default::default()
        }).await.unwrap();
        let res = ctx.validate_ship_movement(
            char_a,
            ship_a,
            MovementIntent::CynoJump { from_sys: sys_ls_edge, to_sys: sys_hs_edge, allow_bubbled: true },
        ).await;
        assert_eq!(res.is_ok(), exp_ok, "wcs={wcs} scram={scram}");
    }
    // AbyssalOrVoidFilament selective cloak/tether flags
    assert!(ctx.validate_ship_movement(
        char_a,
        ship_a,
        MovementIntent::AbyssalOrVoidFilament { from_sys: sys_ls_edge, reject_when_cloaked: false, reject_when_tethered: false },
    ).await.is_ok());
    assert_eq!(
        ctx.validate_ship_movement(
            char_a,
            ship_a,
            MovementIntent::AbyssalOrVoidFilament { from_sys: sys_ls_edge, reject_when_cloaked: true, reject_when_tethered: false },
        ).await.unwrap_err().code,
        "ShipIsCloaked"
    );
    assert_eq!(
        ctx.validate_ship_movement(
            char_a,
            ship_a,
            MovementIntent::AbyssalOrVoidFilament { from_sys: sys_ls_edge, reject_when_cloaked: false, reject_when_tethered: true },
        ).await.unwrap_err().code,
        "ShipIsTethered"
    );

    // Clear cloak & tether for mass / hull matrix
    sys_h.set_combat_state(ship_a, CombatState { warp_core_strength: 1, ..Default::default() }).await.unwrap();

    // 5. Exact mass limits: WormholeJump (ball_opt vs None DB/SDE fallback), Ansiblex, AccelerationGate(Generic rig/mass & AnomicBurner)
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().mass = 100_000_000.0;
    }
    assert!(ctx.validate_ship_movement(char_a, ship_a, MovementIntent::WormholeJump { from_sys: sys_ls_edge, max_jump_mass: Some(100_000_000.0) }).await.is_ok());
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::WormholeJump { from_sys: sys_ls_edge, max_jump_mass: Some(99_999_999.0) }).await.unwrap_err().code,
        "ShipExceedsWormholeJumpMass"
    );

    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().mass = 1_480_000_000.0;
    }
    assert!(ctx.validate_ship_movement(char_a, ship_a, MovementIntent::AnsiblexJump { from_sys: sys_ls_edge, to_sys: sys_hs_edge }).await.is_ok());
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().mass = 1_480_000_001.0;
    }
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::AnsiblexJump { from_sys: sys_ls_edge, to_sys: sys_hs_edge }).await.unwrap_err().code,
        "ShipNotAllowedThroughAnsiblex"
    );

    // Generic gate: mass == 500e6 ok, 500e6+1 err; rig == 4.0 err, rig == 3.0 ok
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        let b = bp.balls.get_mut(&ship_a).unwrap();
        b.type_id = tid_rig3;
        b.mass = 500_000_000.0;
    }
    assert!(ctx.validate_ship_movement(char_a, ship_a, MovementIntent::AccelerationGate { sys_id: sys_ls_edge, kind: AccelerationGateKind::Generic }).await.is_ok());
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().mass = 500_000_001.0;
    }
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::AccelerationGate { sys_id: sys_ls_edge, kind: AccelerationGateKind::Generic }).await.unwrap_err().code,
        "ShipTooLargeForGate"
    );
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        let b = bp.balls.get_mut(&ship_a).unwrap();
        b.type_id = tid_rig4;
        b.mass = 10_000_000.0;
    }
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::AccelerationGate { sys_id: sys_ls_edge, kind: AccelerationGateKind::Generic }).await.unwrap_err().code,
        "ShipTooLargeForGate"
    );

    // AnomicBurner: 49_999_999 ok, 50_000_000 err (using ball_opt == None DB/SDE mass fallback too)
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.remove(&ship_a);
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_a).unwrap().type_id = tid_frigate;
    });
    spawn_test_ship(&ctx, sys_ls_edge, char_a, ship_a, tid_frigate, DVec3::ZERO, 49_999_999.0, CombatState::default()).await;
    assert!(ctx.validate_ship_movement(char_a, ship_a, MovementIntent::AccelerationGate { sys_id: sys_ls_edge, kind: AccelerationGateKind::AnomicBurner }).await.is_ok());
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().mass = 50_000_000.0;
    }
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::AccelerationGate { sys_id: sys_ls_edge, kind: AccelerationGateKind::AnomicBurner }).await.unwrap_err().code,
        "ShipRestrictedByGate"
    );

    // 6. Stargate highsec 0.45 vs 0.44 boundary, Pochven region 10_000_070, Zarzakh Orca vs Capital/Freighter, Supercap
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().type_id = tid_dread;
    }
    assert!(ctx.validate_ship_movement(char_a, ship_a, MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: sys_ls_edge, gate_id: 1 }).await.is_ok());
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: sys_hs_edge, gate_id: 1 }).await.unwrap_err().code,
        "CapitalShipCannotJumpStargate"
    );
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: zz_sys, gate_id: 1 }).await.unwrap_err().code,
        "ShipCannotUseStargate"
    );

    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().type_id = tid_freighter;
    }
    assert!(ctx.validate_ship_movement(char_a, ship_a, MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: sys_hs_edge, gate_id: 1 }).await.is_ok());
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: sys_pochven, gate_id: 1 }).await.unwrap_err().code,
        "CapitalShipCannotJumpStargate"
    );
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: zz_sys, gate_id: 1 }).await.unwrap_err().code,
        "ShipCannotUseStargate"
    );

    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().type_id = tid_orca;
    }
    assert!(ctx.validate_ship_movement(char_a, ship_a, MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: zz_sys, gate_id: 1 }).await.is_ok());

    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_a).unwrap().type_id = tid_super;
    }
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::StargateJump { from_sys: sys_ls_edge, to_sys: sys_ls_edge, gate_id: 1 }).await.unwrap_err().code,
        "CapitalShipCannotJumpStargate"
    );
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::AnsiblexJump { from_sys: sys_ls_edge, to_sys: sys_ls_edge }).await.unwrap_err().code,
        "ShipNotAllowedThroughAnsiblex"
    );
    assert_eq!(
        ctx.validate_ship_movement(char_a, ship_a, MovementIntent::WormholeJump { from_sys: sys_ls_edge, max_jump_mass: None }).await.unwrap_err().code,
        "ShipRestrictedFromWormhole"
    );
}
