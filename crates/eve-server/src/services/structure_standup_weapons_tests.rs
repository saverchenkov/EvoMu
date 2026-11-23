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

#[tokio::test(flavor = "current_thread")]
async fn standup_pdb_guided_bomb_and_vorton_protect_invuln_ship_only_not_pilot_drone_or_corp_ship() {
    let mut ctx = test_ctx();
    let pdb_tid = 950_101u32;
    let flak_tid = 950_102u32;
    let gbl_tid = 950_103u32;
    let bomb_tid = 950_104u32;
    let vorton_tid = 950_105u32;
    let drone_tid = 2454u32;

    {
        let reg = Arc::make_mut(&mut ctx.sde);
        reg.groups.entry(100).or_insert_with(|| eve_sde::GroupInfo {
            group_id: 100,
            category_id: 18,
            name: "Combat Drone".into(),
            anchorable: false,
            anchored: false,
            fittable_non_singleton: false,
            use_base_price: false,
            published: true,
        });
    }
    clone_type_with(
        &mut ctx.sde,
        35926,
        pdb_tid,
        GROUP_STANDUP_POINT_DEFENSE_BATTERY,
        66,
        &[(6, 100.0), (73, 12_000.0), (54, 15_000.0), (158, 10_000.0), (64, 10.0), (56, 1.0), (713, 1.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        35926,
        flak_tid,
        GROUP_STANDUP_FLAK_AMMO,
        8,
        &[(114, 100.0), (118, 100.0), (117, 100.0), (116, 100.0)],
    );
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
        &[(37, 5_000.0), (108, 15_000.0), (281, 60_000.0), (99, 25_000.0), (654, 400.0), (97, 500.0), (114, 1_500.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        47329,
        vorton_tid,
        GROUP_STANDUP_ARCING_VORTON,
        66,
        &[(6, 5_000.0), (73, 60_000.0), (54, 250_000.0), (64, 5.0), (114, 1_000.0), (2104, 10.0), (2105, 50_000.0)],
    );
    clone_type_with(&mut ctx.sde, 587, drone_tid, 100, 18, &[]);
    if let Some(d) = Arc::make_mut(&mut ctx.sde).types.get_mut(&drone_tid) {
        d.radius = 5.0;
    }

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30000142);

    let sid = 10_000_975_001i64;
    let char_id = 8301i64;
    let structure_corp_id = 98830i64;
    let pdb_mod = 79001i64;
    let gbl_mod = 79002i64;
    let vorton_mod = 79003i64;

    let victim_char = 90_830_001i64;
    let unledgered_char = 90_830_002i64;
    let victim_corp_id = 98_831i64;
    let protected_ship_ledger = 68001i64;
    let protected_ship_no_ledger = 68002i64;
    let pilot_drone = 68003i64;
    let corp_ship = 68004i64;
    let anchor_hostile = 68005i64;
    let broken_jump_cloak_ship = 68006i64;
    let module_cloaked_ship = 68007i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().unwrap();
        for (cid, uid, corp, ship) in [
            (victim_char, 83001, victim_corp_id, protected_ship_ledger),
            (unledgered_char, 83002, victim_corp_id + 1, protected_ship_no_ledger),
        ] {
            let mut vc = base_ch.clone();
            vc.character_id = cid;
            vc.user_id = uid;
            vc.corporation_id = corp;
            vc.active_ship_id = ship;
            vc.solar_system_id = sys_id;
            snap.characters.insert(cid, vc);
        }
        snap.corporations.insert(
            victim_corp_id,
            eve_db::CorporationRecord::new(victim_corp_id, "Standup CEO Corp", "SCOC", victim_char),
        );
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: TYPE_KEEPSTAR,
                owner_corp_id: structure_corp_id,
                solar_system_id: sys_id,
                name: "Invuln Guard Keepstar".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 300,
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
            (pdb_mod, pdb_tid, InventoryFlag::HiSlot0),
            (gbl_mod, gbl_tid, InventoryFlag::HiSlot1),
            (vorton_mod, vorton_tid, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: structure_corp_id,
                    location_id: sid,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(iid, true);
        }
        snap.items.insert(
            79010,
            InventoryItemRecord {
                item_id: 79010,
                type_id: flak_tid,
                owner_id: structure_corp_id,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            79011,
            InventoryItemRecord {
                item_id: 79011,
                type_id: bomb_tid,
                owner_id: structure_corp_id,
                location_id: sid,
                flag: InventoryFlag::HiSlot1,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        for (iid, tid, owner) in [
            (protected_ship_ledger, 587u32, victim_char),
            (protected_ship_no_ledger, 587u32, unledgered_char),
            (pilot_drone, drone_tid, victim_char),
            (corp_ship, 587u32, victim_corp_id),
            (broken_jump_cloak_ship, 587u32, unledgered_char),
            (module_cloaked_ship, 587u32, victim_corp_id),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, char_id);
        let online = &mut ext.lifecycle.entry(sid).or_default().online_modules;
        online.extend([pdb_mod, gbl_mod, vorton_mod]);
        save_ext_state(snap, &ext);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let st_ball = make_keepstar_ball(sid, TYPE_KEEPSTAR, DVec3::ZERO);
    let (_, st_cs, st_meta) = make_target_ball_and_combat(sid, DVec3::ZERO, 200_000.0);
    let _ = sys.spawn_ball_with_meta(st_ball.clone(), st_cs, st_meta).await;

    let spawn_custom = |id: i64, tid: u32, pos: DVec3, cat: u32, grp: u32, owner: i64, cid: Option<i64>, corp: i64| {
        let (mut b, cs, mut m) = make_target_ball_and_combat(id, pos, 2_000.0);
        b.type_id = tid;
        m.category_id = cat;
        m.group_id = grp;
        m.owner_id = owner;
        m.char_id = cid;
        m.corp_id = corp;
        (b, cs, m)
    };
    for (b, cs, m) in [
        spawn_custom(protected_ship_ledger, 587, DVec3::new(10_500.0, 0.0, 0.0), 6, 25, victim_char, Some(victim_char), victim_corp_id),
        spawn_custom(protected_ship_no_ledger, 587, DVec3::new(10_600.0, 0.0, 0.0), 6, 25, unledgered_char, Some(unledgered_char), victim_corp_id + 1),
        spawn_custom(pilot_drone, drone_tid, DVec3::new(10_700.0, 0.0, 0.0), 18, 100, victim_char, Some(victim_char), victim_corp_id),
        spawn_custom(corp_ship, 587, DVec3::new(10_800.0, 0.0, 0.0), 6, 25, victim_corp_id, None, victim_corp_id),
        spawn_custom(broken_jump_cloak_ship, 587, DVec3::new(10_850.0, 0.0, 0.0), 6, 25, unledgered_char, Some(unledgered_char), victim_corp_id + 1),
        spawn_custom(module_cloaked_ship, 587, DVec3::new(10_875.0, 0.0, 0.0), 6, 25, victim_corp_id, None, victim_corp_id),
        spawn_custom(anchor_hostile, 670, DVec3::new(10_900.0, 0.0, 0.0), 6, 29, 9001, Some(9001), 98999),
    ] {
        let _ = sys.spawn_ball_with_meta(b, cs, m).await;
    }
    for bid in [protected_ship_ledger, protected_ship_no_ledger] {
        let _ = sys.modify_combat_state(bid, |cs| {
            cs.apply_jump_cloak_and_invuln_timers(60);
            cs.is_cloaked = false;
        }).await;
    }
    let _ = sys.modify_combat_state(broken_jump_cloak_ship, |cs| {
        cs.apply_jump_cloak_and_invuln_timers(60);
        cs.undock_invulnerable_remaining_ticks = 0;
    }).await;
    let _ = sys.modify_combat_state(module_cloaked_ship, |cs| {
        cs.is_cloaked = true;
    }).await;

    // 1. Point Defense Battery: protects both ships, hits drone, corp_ship, broken jump cloak,
    //    and module-cloaked ship without decloaking it (BF-02).
    try_activate_standup_weapon(&ctx, &sys, char_id, sid, sys_id, pdb_mod, None, &st_ball)
        .await
        .unwrap()
        .expect("standup pdb succeeds");
    assert_eq!(sys.get_combat_state(protected_ship_ledger).await.unwrap().unwrap().shield_hp, 20_000.0);
    assert_eq!(sys.get_combat_state(protected_ship_no_ledger).await.unwrap().unwrap().shield_hp, 20_000.0);
    assert!(sys.get_combat_state(pilot_drone).await.unwrap().unwrap().shield_hp < 20_000.0);
    assert!(sys.get_combat_state(corp_ship).await.unwrap().unwrap().shield_hp < 20_000.0);
    assert!(sys.get_combat_state(broken_jump_cloak_ship).await.unwrap().unwrap().shield_hp < 20_000.0);
    let cloaked_after_pdb = sys.get_combat_state(module_cloaked_ship).await.unwrap().unwrap();
    assert!(cloaked_after_pdb.shield_hp < 20_000.0, "BF-02: standup PDB must damage module-cloaked ship");
    assert!(cloaked_after_pdb.is_cloaked, "BF-02: standup PDB must not decloak ship");

    // 2. Guided Bomb Launcher: detonates near anchor_hostile; protected ships take 0, drone, corp ship,
    //    broken jump cloak, and module-cloaked ship take damage without decloaking (BF-02).
    for bid in [pilot_drone, corp_ship, broken_jump_cloak_ship, module_cloaked_ship] {
        let _ = sys.modify_combat_state(bid, |cs| cs.shield_hp = 20_000.0).await;
    }
    let _ = sys.add_target_lock(
        sid,
        TargetLock { target_id: anchor_hostile, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None },
    ).await;
    try_activate_standup_weapon(&ctx, &sys, char_id, sid, sys_id, gbl_mod, Some(anchor_hostile), &st_ball)
        .await
        .unwrap()
        .expect("launch standup guided bomb");
    step_guided_bombs_in_system(&ctx, sys_id, 1.0).await;
    assert_eq!(sys.get_combat_state(protected_ship_ledger).await.unwrap().unwrap().shield_hp, 20_000.0);
    assert_eq!(sys.get_combat_state(protected_ship_no_ledger).await.unwrap().unwrap().shield_hp, 20_000.0);
    assert!(sys.get_combat_state(pilot_drone).await.unwrap().unwrap().shield_hp < 20_000.0);
    assert!(sys.get_combat_state(corp_ship).await.unwrap().unwrap().shield_hp < 20_000.0);
    assert!(sys.get_combat_state(broken_jump_cloak_ship).await.unwrap().unwrap().shield_hp < 20_000.0);
    let cloaked_after_bomb = sys.get_combat_state(module_cloaked_ship).await.unwrap().unwrap();
    assert!(cloaked_after_bomb.shield_hp < 20_000.0, "BF-02: standup guided bomb must damage module-cloaked ship");
    assert!(cloaked_after_bomb.is_cloaked, "BF-02: standup guided bomb must not decloak ship");

    // 3. Arcing Vorton Projector: hops from anchor_hostile to drone, corp ship, & broken jump cloak while skipping protected & module-cloaked ships.
    for bid in [pilot_drone, corp_ship, broken_jump_cloak_ship, module_cloaked_ship] {
        let _ = sys.modify_combat_state(bid, |cs| cs.shield_hp = 20_000.0).await;
    }
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.gunner_immobilized_until_unix.remove(&sid);
        save_ext_state(snap, &ext);
    });
    try_activate_standup_weapon(&ctx, &sys, char_id, sid, sys_id, vorton_mod, Some(anchor_hostile), &st_ball)
        .await
        .unwrap()
        .expect("activate standup arcing vorton");
    assert_eq!(sys.get_combat_state(protected_ship_ledger).await.unwrap().unwrap().shield_hp, 20_000.0);
    assert_eq!(sys.get_combat_state(protected_ship_no_ledger).await.unwrap().unwrap().shield_hp, 20_000.0);
    assert_eq!(sys.get_combat_state(module_cloaked_ship).await.unwrap().unwrap().shield_hp, 20_000.0);
    assert!(sys.get_combat_state(pilot_drone).await.unwrap().unwrap().shield_hp < 20_000.0);
    assert!(sys.get_combat_state(corp_ship).await.unwrap().unwrap().shield_hp < 20_000.0);
    assert!(sys.get_combat_state(broken_jump_cloak_ship).await.unwrap().unwrap().shield_hp < 20_000.0);
}
