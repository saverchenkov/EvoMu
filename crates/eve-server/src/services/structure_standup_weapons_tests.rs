//! Unit tests for `structure_standup_weapons` (Standup Point Defense Battery, Guided Bomb Launcher, Arcing Vorton Projector, and gunner immobilization).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use eve_db::{
    InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta, TargetLock};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "structure_standup_weapons_extra_unit_tests.rs"]
mod structure_standup_weapons_extra_unit_tests;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("WeaponType_{new_tid}");
    rec.radius = 5_000.0;
    rec.mass = 1.0e11;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_keepstar_ball(sid: i64, tid: u32, pos: DVec3) -> Ball {
    Ball {
        id: sid,
        type_id: tid,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1.0e12,
        radius: 10_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn make_target_ball_and_combat(id: i64, pos: DVec3, cap: f64) -> (Ball, CombatState, SlimItemMeta) {
    let ball = Ball {
        id,
        type_id: 670,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1.0e6,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::new();
    cs.shield_hp = 20_000.0;
    cs.max_shield_hp = 20_000.0;
    cs.armor_hp = 20_000.0;
    cs.max_armor_hp = 20_000.0;
    cs.structure_hp = 20_000.0;
    cs.max_structure_hp = 20_000.0;
    cs.capacitor = cap;
    cs.max_capacitor = cap.max(100.0);
    let meta = SlimItemMeta {
        owner_id: 9001,
        char_id: Some(9001),
        corp_id: 98999,
        alliance_id: -1,
        group_id: 25,
        category_id: 6,
        security_status: -5.0,
        is_cloaked: false,
        ..Default::default()
    };
    (ball, cs, meta)
}

#[test]
fn gunner_immobilization_blocks_release_until_timestamp_expires() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(1001, 5001);
        ext.gunner_immobilized_until_unix.insert(1001, now + 30);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        ctx.db.with_snapshot(|s| ensure_gunner_not_immobilized(s, 5001, now)).unwrap_err().code,
        "GunnerImmobilized"
    );
    assert!(ctx
        .db
        .with_snapshot(|s| ensure_gunner_not_immobilized(s, 5001, now + 31))
        .is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn point_defense_battery_validation_and_aoe_hit_on_xl_structure() {
    let mut ctx = test_ctx();
    let pdb_tid = 950_001u32;
    let flak_tid = 950_002u32;
    let non_weapon_tid = 950_003u32;
    clone_type_with(
        &mut ctx.sde,
        35926,
        pdb_tid,
        GROUP_STANDUP_POINT_DEFENSE_BATTERY,
        66,
        &[
            (6, 100.0),      // capacitorNeed
            (73, 12_000.0),  // duration ms
            (54, 15_000.0),  // maxRange (optimal)
            (158, 10_000.0), // falloff
            (64, 10.0),      // damageMultiplier
            (56, 1.0),       // chargeRate
            (713, 1.0),      // consumptionQuantity
        ],
    );
    clone_type_with(
        &mut ctx.sde,
        35926,
        flak_tid,
        GROUP_STANDUP_FLAK_AMMO,
        8,
        &[(114, 100.0), (118, 100.0), (117, 100.0), (116, 100.0)],
    );
    clone_type_with(&mut ctx.sde, 35892, non_weapon_tid, 1415, 66, &[]);

    // Use NullSec system so highsec weapon rules allow all Standup superweapons
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30000142);

    let sid = 10_000_955_001i64;
    let char_id = 8101i64;
    let corp_id = 98810i64;
    let mod_id = 77001i64;
    let other_mod_id = 77002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: TYPE_KEEPSTAR,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Keepstar Alpha".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            other_mod_id,
            InventoryItemRecord {
                item_id: other_mod_id,
                type_id: non_weapon_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: pdb_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            77003,
            InventoryItemRecord {
                item_id: 77003,
                type_id: flak_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, char_id);
        ext.lifecycle
            .entry(sid)
            .or_default()
            .online_modules
            .push(mod_id);
        snap.module_online_states.insert(mod_id, true);
        save_ext_state(snap, &ext);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let st_ball = make_keepstar_ball(sid, TYPE_KEEPSTAR, DVec3::ZERO);
    let (_, mut st_cs, st_meta) = make_target_ball_and_combat(sid, DVec3::ZERO, 50_000.0);
    let _ = sys.spawn_ball_with_meta(st_ball.clone(), st_cs.clone(), st_meta).await;

    let (t_ball, t_cs, t_meta) =
        make_target_ball_and_combat(66001, DVec3::new(12_000.0, 0.0, 0.0), 1_000.0);
    let _ = sys.spawn_ball_with_meta(t_ball, t_cs, t_meta).await;

    // Non-weapon module returns None
    assert!(try_activate_standup_weapon(&ctx, &sys, char_id, sid, sys_id, other_mod_id, None, &st_ball)
        .await
        .is_none());

    // Wrong gunner rejected
    let err_gun = try_activate_standup_weapon(&ctx, &sys, 999999, sid, sys_id, mod_id, None, &st_ball)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_gun.code, "NotControllingStructure");

    // PDB fires, consumes 1 flak charge, damages target 66001
    let ok_pdb = try_activate_standup_weapon(&ctx, &sys, char_id, sid, sys_id, mod_id, None, &st_ball)
        .await
        .unwrap()
        .expect("pdb fire");
    assert!(matches!(ok_pdb, PyRep::KeyVal(_)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&77003].quantity), 4);
    assert!(sys.get_combat_state(66001).await.unwrap().unwrap().shield_hp < 20_000.0);
    st_cs.capacitor = 50_000.0;
    let _ = sys.set_combat_state(sid, st_cs).await;
}

#[tokio::test(flavor = "current_thread")]
async fn guided_bomb_launcher_spawns_and_detonates_and_vorton_chains_and_immobilizes() {
    let mut ctx = test_ctx();
    let gbl_tid = 950_010u32;
    let bomb_tid = 950_011u32;
    let vorton_tid = 950_012u32;

    clone_type_with(
        &mut ctx.sde,
        35924,
        gbl_tid,
        GROUP_STANDUP_GUIDED_BOMB_LAUNCHER,
        66,
        &[(6, 200.0), (73, 20_000.0), (54, 300_000.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        35924,
        bomb_tid,
        GROUP_STANDUP_GUIDED_BOMB,
        8,
        &[
            (37, 5_000.0),   // maxVelocity m/s
            (108, 15_000.0), // detonationRange
            (281, 60_000.0), // explosionDelay ms
            (99, 25_000.0),  // aoeRadius
            (654, 400.0),    // expRadius
            (97, 500.0),     // neut amount
            (114, 1_500.0),  // emDamage
        ],
    );
    clone_type_with(
        &mut ctx.sde,
        47329,
        vorton_tid,
        GROUP_STANDUP_ARCING_VORTON,
        66,
        &[
            (6, 5_000.0),     // capacitorNeed
            (73, 60_000.0),   // duration ms
            (54, 250_000.0),  // maxRange
            (64, 5.0),        // dmgMultiplier
            (114, 1_000.0),   // emDamage
        ],
    );

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30000142);

    let sid = 10_000_965_001i64;
    let char_id = 8201i64;
    let corp_id = 98820i64;
    let gbl_mod = 78001i64;
    let vorton_mod = 78002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: TYPE_KEEPSTAR,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Superweapon Keepstar".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 200,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (iid, tid, flag) in [
            (gbl_mod, gbl_tid, InventoryFlag::HiSlot0),
            (vorton_mod, vorton_tid, InventoryFlag::HiSlot1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: sid,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            78003,
            InventoryItemRecord {
                item_id: 78003,
                type_id: bomb_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 4,
                singleton: false,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, char_id);
        let online = &mut ext.lifecycle.entry(sid).or_default().online_modules;
        online.push(gbl_mod);
        online.push(vorton_mod);
        snap.module_online_states.insert(gbl_mod, true);
        snap.module_online_states.insert(vorton_mod, true);
        save_ext_state(snap, &ext);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let st_ball = make_keepstar_ball(sid, TYPE_KEEPSTAR, DVec3::ZERO);
    let (_, st_cs, st_meta) = make_target_ball_and_combat(sid, DVec3::ZERO, 100_000.0);
    let _ = sys.spawn_ball_with_meta(st_ball.clone(), st_cs, st_meta).await;

    // Two hostile balls within 10km of each other (so bomb detonates fast and vorton chains both!)
    let (b1, cs1, m1) =
        make_target_ball_and_combat(67001, DVec3::new(10_500.0, 0.0, 0.0), 2_000.0);
    let (b2, cs2, m2) =
        make_target_ball_and_combat(67002, DVec3::new(12_000.0, 0.0, 0.0), 2_000.0);
    let _ = sys.spawn_ball_with_meta(b1, cs1, m1).await;
    let _ = sys.spawn_ball_with_meta(b2, cs2, m2).await;

    // Unclamped/unlocked target fails first, then lock 67001 and launch guided bomb
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys, char_id, sid, sys_id, gbl_mod, Some(67001), &st_ball)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );
    let _ = sys
        .add_target_lock(
            sid,
            TargetLock {
                target_id: 67001,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    let _ = try_activate_standup_weapon(
        &ctx,
        &sys,
        char_id,
        sid,
        sys_id,
        gbl_mod,
        Some(67001),
        &st_ball,
    )
    .await
    .unwrap()
    .expect("launch guided bomb");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&78003].quantity), 3);

    // Step guided bomb in system -> detonates near 67001
    step_guided_bombs_in_system(&ctx, sys_id, 1.0).await;
    assert!(sys.get_combat_state(67001).await.unwrap().unwrap().shield_hp < 20_000.0);
    assert!(ctx.db.with_snapshot(|s| load_ext_state(s).in_flight_guided_bombs.is_empty()));

    // Activate Arcing Vorton Projector -> chains across both hostile ships & immobilizes gunner
    let hp_before = sys.get_combat_state(67002).await.unwrap().unwrap().shield_hp;
    let vorton_res = try_activate_standup_weapon(
        &ctx,
        &sys,
        char_id,
        sid,
        sys_id,
        vorton_mod,
        Some(67001),
        &st_ball,
    )
    .await
    .unwrap()
    .expect("activate vorton");
    assert!(matches!(vorton_res, PyRep::KeyVal(_)));
    assert!(sys.get_combat_state(67002).await.unwrap().unwrap().shield_hp < hp_before);
    assert_eq!(
        ctx.db
            .with_snapshot(|s| ensure_gunner_not_immobilized(s, char_id, ctx.now_unix()))
            .unwrap_err()
            .code,
        "GunnerImmobilized"
    );
}
