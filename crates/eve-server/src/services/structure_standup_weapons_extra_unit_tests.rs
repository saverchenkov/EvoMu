//! Deep branch-coverage and mutation-killing unit tests for `structure_standup_weapons`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, DroneSpaceState, SafetyLevel};
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, InFlightGuidedBombRecord,
};
use crate::services::structure_standup_weapons::{
    ensure_gunner_not_immobilized, step_guided_bombs_in_system, try_activate_standup_weapon,
    GROUP_STANDUP_ARCING_VORTON, GROUP_STANDUP_FLAK_AMMO, GROUP_STANDUP_GUIDED_BOMB,
    GROUP_STANDUP_GUIDED_BOMB_LAUNCHER, GROUP_STANDUP_POINT_DEFENSE_BATTERY, TYPE_KEEPSTAR,
};
use eve_db::{
    CorporationRecord, FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta, TargetLock};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn register_type(
    sde: &mut Arc<SdeRegistry>,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.get(&587).expect("seed type").clone();
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("StandupTest_{new_tid}");
    rec.radius = 100.0;
    rec.mass = 1.0e6;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_ball(id: i64, type_id: u32, pos: DVec3, radius: f64) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

async fn spawn_combat_ball(
    sys: &crate::system_actor::SystemHandle,
    id: i64,
    type_id: u32,
    pos: DVec3,
    hp: f64,
    cap: f64,
    cloaked: bool,
    char_id: Option<i64>,
    corp_id: i64,
) {
    let ball = make_ball(id, type_id, pos, 50.0);
    let mut cs = CombatState::new();
    cs.shield_hp = hp;
    cs.max_shield_hp = hp.max(100.0);
    cs.armor_hp = hp;
    cs.max_armor_hp = hp.max(100.0);
    cs.structure_hp = hp;
    cs.max_structure_hp = hp.max(100.0);
    cs.capacitor = cap;
    cs.max_capacitor = cap.max(100.0);
    cs.is_cloaked = cloaked;
    let meta = SlimItemMeta {
        owner_id: char_id.unwrap_or(corp_id),
        char_id,
        corp_id,
        alliance_id: -1,
        group_id: 25,
        category_id: 6,
        security_status: -5.0,
        is_cloaked: cloaked,
        ..Default::default()
    };
    let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
}

#[tokio::test(flavor = "current_thread")]
async fn standup_weapon_preconditions_and_point_defense_hostility_and_ammo_branches() {
    let mut ctx = new_ctx();
    let pdb_tid = 951_001u32;
    let flak_tid = 951_002u32;
    register_type(
        &mut ctx.sde,
        pdb_tid,
        GROUP_STANDUP_POINT_DEFENSE_BATTERY,
        66,
        &[(6, 500.0), (54, 10_000.0), (158, 5_000.0), (64, 5.0), (56, 2.0), (73, 10_000.0), (1970, 1.0)],
    );
    register_type(
        &mut ctx.sde,
        flak_tid,
        GROUP_STANDUP_FLAK_AMMO,
        8,
        &[(114, 200.0), (118, 200.0), (117, 200.0), (116, 200.0)],
    );

    let hs_sys = 30_000_142u32;
    let ns_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    let sid = 10_000_960_001i64;
    let gunner_id = 96_001i64;
    let st_corp = 98_601i64;
    let mod_id = 86_001i64;
    let st_ball = make_ball(sid, TYPE_KEEPSTAR, DVec3::ZERO, 5_000.0);
    let sys_ns = ctx.systems.get_or_spawn(ns_sys);

    // 1. Missing module returns None
    assert!(try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, 999_999, None, &st_ball).await.is_none());

    // Insert module without structure -> StructureNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: pdb_tid,
                owner_id: st_corp,
                location_id: sid,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "StructureNotFound"
    );

    // 2. StructureDestroyed, ModuleNotFitted, ModuleOffline
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35832,
                owner_corp_id: st_corp,
                solar_system_id: ns_sys,
                name: "Test Structure".into(),
                state: "Destroyed".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0e6,
                armor_hp: 1.0e6,
                hull_hp: 1.0e6,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "StructureDestroyed"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().state = "ShieldVulnerable".into();
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, gunner_id);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "ModuleNotFitted"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::HiSlot0;
    });
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "ModuleOffline"
    );

    // 3. CannotActivateInHighSec, WeaponCycleImmobilized, ModuleAlreadyCycling, RequiresXLStructure
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, true);
    });
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, hs_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "CannotActivateInHighSec"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.gunner_immobilized_until_unix.insert(sid, now + 60);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "WeaponCycleImmobilized"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| ensure_gunner_not_immobilized(s, gunner_id, now)).unwrap_err().code,
        "GunnerImmobilized"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.gunner_immobilized_until_unix.remove(&sid);
        ext.standup_module_cycle_until_unix.insert(mod_id, now + 60);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "ModuleAlreadyCycling"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.standup_module_cycle_until_unix.remove(&mod_id);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "RequiresXLStructure"
    );

    // 4. Upgrade to Keepstar: NoAmmoLoaded, NotEnoughCapacitor, then fire using SpecializedAmmoHold ammo & test hostility filters
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().type_id = TYPE_KEEPSTAR;
    });
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "NoAmmoLoaded"
    );

    let ammo_id = 86_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ammo_id,
            InventoryItemRecord {
                item_id: ammo_id,
                type_id: flak_tid,
                owner_id: st_corp,
                location_id: sid,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });
    spawn_combat_ball(&sys_ns, sid, TYPE_KEEPSTAR, DVec3::ZERO, 1.0e6, 50.0, false, None, st_corp).await;
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball).await.unwrap().unwrap_err().code,
        "NotEnoughCapacitor"
    );

    // Set structure max_capacitor = 0.0 to exercise SDE fallback initialization (600,000 GJ)
    let mut st_cs = CombatState::new();
    st_cs.max_capacitor = 0.0;
    st_cs.capacitor = 0.0;
    let _ = sys_ns.set_combat_state(sid, st_cs).await;

    let ally_corp = 98_602i64;
    let fleet_mate = 96_002i64;
    let hostile_char = 96_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        let base_c = snap.characters.values().next().cloned().unwrap();
        for (cid, corp) in [(gunner_id, st_corp), (fleet_mate, 98_699), (hostile_char, 98_688)] {
            let mut c = base_c.clone();
            c.character_id = cid;
            c.corporation_id = corp;
            snap.characters.insert(cid, c);
        }
        for corp in [st_corp, ally_corp] {
            snap.corporations.insert(corp, { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp;
                __corp.alliance_id = Some(99_001);
                __corp });
        }
        snap.fleets.insert(701, FleetRecord { fleet_id: 701, boss_id: gunner_id, members: vec![gunner_id, fleet_mate], ..Default::default() });
        for (iid, oid) in [(67_001, st_corp), (67_002, ally_corp)] {
            snap.items.insert(
                iid,
                InventoryItemRecord { item_id: iid, type_id: 587, owner_id: oid, location_id: ns_sys as i64, ..Default::default() },
            );
        }
    });
    spawn_combat_ball(&sys_ns, 67_001, 587, DVec3::new(6_000.0, 0.0, 0.0), 500.0, 100.0, false, None, st_corp).await;
    spawn_combat_ball(&sys_ns, 67_002, 587, DVec3::new(6_000.0, 0.0, 0.0), 500.0, 100.0, false, None, ally_corp).await;
    spawn_combat_ball(&sys_ns, 67_003, 587, DVec3::new(6_000.0, 0.0, 0.0), 500.0, 100.0, false, Some(fleet_mate), 98_699).await;
    spawn_combat_ball(&sys_ns, 67_004, 587, DVec3::new(6_000.0, 0.0, 0.0), 500.0, 100.0, true, Some(hostile_char), 98_688).await;
    spawn_combat_ball(&sys_ns, 67_005, 587, DVec3::new(50_000.0, 0.0, 0.0), 500.0, 100.0, false, Some(hostile_char), 98_688).await;
    spawn_combat_ball(&sys_ns, 67_006, 2454, DVec3::new(16_000.0, 0.0, 0.0), 100.0, 100.0, false, Some(hostile_char), 98_688).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            67_006,
            DroneSpaceState {
                ball_id: 67_006,
                drone_item_id: 67_006,
                type_id: 2454,
                group_id: 100,
                owner_char_id: hostile_char,
                original_owner_char_id: hostile_char,
                parent_ship_id: 67_005,
                solar_system_id: ns_sys,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 300.0,
                optimal_range: 5_000.0,
                damage_vector: eve_destiny::DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    let pdb_res = try_activate_standup_weapon(&ctx, &sys_ns, gunner_id, sid, ns_sys, mod_id, None, &st_ball)
        .await
        .unwrap()
        .expect("pdb fire");
    assert_eq!(pdb_res.dict_get("targetsHit"), Some(&PyRep::Integer(1)));
    assert_eq!(pdb_res.dict_get("destroyedCount"), Some(&PyRep::Integer(1)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ammo_id)));
}

#[tokio::test(flavor = "current_thread")]
async fn guided_bomb_launcher_step_and_arcing_vorton_error_and_safety_branches() {
    let mut ctx = new_ctx();
    let gbl_tid = 951_010u32;
    let bomb_tid = 951_011u32;
    let vorton_tid = 951_012u32;

    register_type(&mut ctx.sde, gbl_tid, GROUP_STANDUP_GUIDED_BOMB_LAUNCHER, 66, &[(6, 100.0), (54, 50_000.0), (51, 30_000.0)]);
    register_type(
        &mut ctx.sde,
        bomb_tid,
        GROUP_STANDUP_GUIDED_BOMB,
        8,
        &[(37, 2_500.0), (281, 20_000.0), (99, 20_000.0), (654, 400.0), (97, 500.0), (114, 1_000.0)],
    );
    register_type(
        &mut ctx.sde,
        vorton_tid,
        GROUP_STANDUP_ARCING_VORTON,
        66,
        &[(6, 200.0), (54, 100_000.0), (158, 25_000.0), (114, 5_000.0), (1579, 45_000.0)],
    );

    let ls_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.05 && s.security_status < 0.40)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_505);
    let sys = ctx.systems.get_or_spawn(ls_sys);
    let sid = 10_000_960_010i64;
    let gunner_id = 96_010i64;
    let hostile_id = 96_011i64;
    let st_corp = 98_610i64;
    let gbl_mod = 86_010i64;
    let vorton_mod = 86_011i64;
    let st_ball = make_ball(sid, TYPE_KEEPSTAR, DVec3::ZERO, 5_000.0);

    ctx.db.with_mut_snapshot(|snap| {
        let base_c = snap.characters.values().next().cloned().unwrap();
        for (cid, corp) in [(gunner_id, st_corp), (hostile_id, 98_688)] {
            let mut c = base_c.clone();
            c.character_id = cid;
            c.corporation_id = corp;
            snap.characters.insert(cid, c);
        }
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35832,
                owner_corp_id: st_corp,
                solar_system_id: ls_sys,
                name: "Test Struct".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0e6,
                armor_hp: 1.0e6,
                hull_hp: 1.0e6,
                ..Default::default()
            },
        );
        for (mid, tid, flg) in [(gbl_mod, gbl_tid, InventoryFlag::HiSlot0), (vorton_mod, vorton_tid, InventoryFlag::HiSlot1)] {
            snap.items.insert(
                mid,
                InventoryItemRecord { item_id: mid, type_id: tid, owner_id: st_corp, location_id: sid, flag: flg, quantity: 1, singleton: true, ..Default::default() },
            );
            snap.module_online_states.insert(mid, true);
        }
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, gunner_id);
        save_ext_state(snap, &ext);
    });
    spawn_combat_ball(&sys, sid, TYPE_KEEPSTAR, DVec3::ZERO, 1.0e6, 100_000.0, false, None, st_corp).await;

    // 1. GBL on Medium structure -> RequiresLOrXLStructure; Vorton on non-Keepstar -> CantFitToStructure
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys, gunner_id, sid, ls_sys, gbl_mod, None, &st_ball).await.unwrap().unwrap_err().code,
        "RequiresLOrXLStructure"
    );
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys, gunner_id, sid, ls_sys, vorton_mod, None, &st_ball).await.unwrap().unwrap_err().code,
        "CantFitToStructure"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().type_id = TYPE_KEEPSTAR;
    });

    // 2. TargetRequired, TargetNotLocked, TargetNotFound, NoAmmoLoaded, TargetOutOfRange, CantDoInCurrentSafetyLevel
    for mid in [gbl_mod, vorton_mod] {
        assert_eq!(
            try_activate_standup_weapon(&ctx, &sys, gunner_id, sid, ls_sys, mid, None, &st_ball).await.unwrap().unwrap_err().code,
            "TargetRequired"
        );
        assert_eq!(
            try_activate_standup_weapon(&ctx, &sys, gunner_id, sid, ls_sys, mid, Some(77_001), &st_ball).await.unwrap().unwrap_err().code,
            "TargetNotLocked"
        );
    }

    spawn_combat_ball(&sys, 77_001, 670, DVec3::new(200_000.0, 0.0, 0.0), 5_000.0, 1_000.0, false, Some(hostile_id), 98_688).await;
    let _ = sys
        .add_target_lock(
            sid,
            TargetLock {
                target_id: 77_001,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    assert_eq!(
        try_activate_standup_weapon(&ctx, &sys, gunner_id, sid, ls_sys, gbl_mod, Some(77_001), &st_ball).await.unwrap().unwrap_err().code,
        "NoAmmoLoaded"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            86_020,
            InventoryItemRecord {
                item_id: 86_020,
                type_id: bomb_tid,
                owner_id: st_corp,
                location_id: sid,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });
    for mid in [gbl_mod, vorton_mod] {
        assert_eq!(
            try_activate_standup_weapon(&ctx, &sys, gunner_id, sid, ls_sys, mid, Some(77_001), &st_ball).await.unwrap().unwrap_err().code,
            "TargetOutOfRange"
        );
    }

    // Move target 77_001 (capsule, type 670) to 20 km; with SafetyLevel::Yellow in lowsec, capsule is protected!
    spawn_combat_ball(&sys, 77_001, 670, DVec3::new(20_000.0, 0.0, 0.0), 5_000.0, 1_000.0, false, Some(hostile_id), 98_688).await;
    let _ = sys
        .add_target_lock(
            sid,
            TargetLock {
                target_id: 77_001,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(gunner_id, SafetyLevel::Yellow);
    });
    for mid in [gbl_mod, vorton_mod] {
        assert_eq!(
            try_activate_standup_weapon(&ctx, &sys, gunner_id, sid, ls_sys, mid, Some(77_001), &st_ball).await.unwrap().unwrap_err().code,
            "CantDoInCurrentSafetyLevel"
        );
    }

    // 3. step_guided_bombs_in_system edge cases: unknown bomb type, dead bomb, missing target, flight expiry, and neut detonation
    let (b_unknown, b_dead, b_notarget, b_expire, b_detonate) = (990_001, 990_002, 990_003, 990_004, 990_005);
    spawn_combat_ball(&sys, b_dead, bomb_tid, DVec3::new(5_000.0, 0.0, 0.0), 0.0, 0.0, false, None, st_corp).await;
    spawn_combat_ball(&sys, b_notarget, bomb_tid, DVec3::new(5_000.0, 0.0, 0.0), 1000.0, 0.0, false, None, st_corp).await;
    spawn_combat_ball(&sys, b_expire, bomb_tid, DVec3::new(5_000.0, 0.0, 0.0), 1000.0, 0.0, false, None, st_corp).await;
    spawn_combat_ball(&sys, b_detonate, bomb_tid, DVec3::new(19_500.0, 0.0, 0.0), 1000.0, 0.0, false, None, st_corp).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(gunner_id, SafetyLevel::Red);
    });
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        for (bid, tid, btid, rem) in [
            (b_unknown, 77_001, 999_999u32, 10.0),
            (b_dead, 77_001, bomb_tid, 10.0),
            (b_notarget, 77_999, bomb_tid, 10.0),
            (b_expire, 77_001, bomb_tid, 0.5),
            (b_detonate, 77_001, bomb_tid, 10.0),
        ] {
            ext.in_flight_guided_bombs.insert(
                bid,
                InFlightGuidedBombRecord {
                    bomb_id: bid,
                    structure_id: sid,
                    structure_corp_id: st_corp,
                    gunner_char_id: gunner_id,
                    solar_system_id: ls_sys,
                    target_id: tid,
                    bomb_type_id: btid,
                    remaining_flight_s: rem,
                },
            );
        }
        save_ext_state(snap, &ext);
    });

    step_guided_bombs_in_system(&ctx, ls_sys, 1.0).await;
    assert!(ctx.db.with_snapshot(|s| load_ext_state(s).in_flight_guided_bombs.is_empty()));
    let post_cs = sys.get_combat_state(77_001).await.unwrap().unwrap();
    assert!(post_cs.capacitor < 1_000.0);
}

#[test]
fn consume_structure_charge_direct_true_false_locked_and_ammo_bay_edges() {
    let ctx = new_ctx();
    let sid = 10_000_970_001_i64;
    let st_corp = 98_701_i64;
    let mod_id = 87_001_i64;
    let charge_tid = 40_001_u32;

    // 1. No ammo items seeded -> false.
    assert!(!super::super::consume_structure_charge(
        &ctx,
        sid,
        st_corp,
        mod_id,
        InventoryFlag::HiSlot0,
        charge_tid,
        2,
    ));

    // 2. Insufficient quantity -> false, item untouched.
    let charge_slot_item = 87_101_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            charge_slot_item,
            InventoryItemRecord {
                item_id: charge_slot_item,
                type_id: charge_tid,
                owner_id: st_corp,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert!(!super::super::consume_structure_charge(
        &ctx,
        sid,
        st_corp,
        mod_id,
        InventoryFlag::HiSlot0,
        charge_tid,
        2,
    ));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_slot_item].quantity), 1);

    // 3. Sufficient quantity but locked in outstanding contract -> false, quantity untouched.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&charge_slot_item).unwrap().quantity = 5;
        snap.contracts.insert(
            77_701,
            eve_db::ContractRecord {
                contract_id: 77_701,
                status: "Outstanding".into(),
                item_ids: vec![charge_slot_item],
                ..Default::default()
            },
        );
    });
    assert!(!super::super::consume_structure_charge(
        &ctx,
        sid,
        st_corp,
        mod_id,
        InventoryFlag::HiSlot0,
        charge_tid,
        2,
    ));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_slot_item].quantity), 5);

    // 4. Unlocked in HiSlot0 -> true, consumes exact quantity (5 -> 3).
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.clear();
    });
    assert!(super::super::consume_structure_charge(
        &ctx,
        sid,
        st_corp,
        mod_id,
        InventoryFlag::HiSlot0,
        charge_tid,
        2,
    ));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&charge_slot_item].quantity), 3);

    // 5. Fallback to SpecializedAmmoHold -> true and removes stack cleanly when exact qty consumed.
    let ammo_bay_item = 87_102_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&charge_slot_item);
        snap.items.insert(
            ammo_bay_item,
            InventoryItemRecord {
                item_id: ammo_bay_item,
                type_id: charge_tid,
                owner_id: st_corp,
                location_id: sid,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 4,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert!(super::super::consume_structure_charge(
        &ctx,
        sid,
        st_corp,
        mod_id,
        InventoryFlag::HiSlot0,
        charge_tid,
        4,
    ));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ammo_bay_item)));
}
