//! Adversarial regression tests for Round 8 Shard D:
//! - `#487`: `invbroker.SplitStack` / `contractProxy.SplitStack` ALSC item locks, `InventoryFlag::Locked` /
//!   `AssetSafety` / `Impounded`, container passwords, `Plastic Wrap` (`3468`), active `fighter_tube_loads`,
//!   corp hangar division Take roles, physical co-location, and `i32` overflow rejection.
//! - `#492`: `SystemCommand::SetCombatState` non-compounding `base_scramble_strengths` / `base_core_strengths`,
//!   Support Fighter `Siren` (`40346`) tackle lifecycle cleanup, and `SpatialGtfo` warp suppression cleanup.

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::doomsday_aoe_tick::{
    mutate_superweapon_runtime, step_bosonic_doomsday_system, PersistedSuperweaponCycle,
    SuperweaponCycleKind,
};
use eve_server::system_actor::ActiveModuleCycle;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_STATION: i64 = 60_003_760;
const AMARR_STATION: i64 = 60_008_494;
const NULLSEC_SYS: u32 = 30_000_021;
const ARCHON: u32 = 23_757;
const VEXOR: u32 = 626;
const SIREN_I: u32 = 40_346;

fn paused_ctx() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

fn seed_stack(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                ..Default::default()
            },
        );
    });
}

#[tokio::test]
async fn test_r8_487_split_stack_locks_passwords_plastic_wrap_tubes_colocation_and_overflow() {
    // parity: rpc:invbroker.SplitStack, rpc:contractProxy.SplitStack, rpc:invbroker.MultiMerge, rpc:invbroker.ALSCLockItems, rpc:invbroker.SetPassword
    let ctx = paused_ctx();
    let kw = HashMap::new();

    let mut s1 = SessionState::new(84_871);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut s1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("select 90_000_001");

    let mut s2 = SessionState::new(84_872);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut s2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("select 90_000_002");

    // 1. i64 -> i32 truncation guard: (1 << 32) + 1 == 4_294_967_297 must NOT truncate to 1!
    let base_trit = 848_700_001_i64;
    let merge_dst_trit = 848_700_002_i64;
    seed_stack(&ctx, base_trit, 34, 90_000_001, JITA_STATION, InventoryFlag::Hangar, 100, false);
    seed_stack(&ctx, merge_dst_trit, 34, 90_000_001, JITA_STATION, InventoryFlag::Hangar, 50, false);

    let trunc_qty = (1_i64 << 32) + 1;
    let split_inv_err = ctx
        .dispatch_rpc(&mut s1, "invbroker", None, "SplitStack", &[PyRep::Integer(base_trit), PyRep::Integer(trunc_qty)], &kw, None)
        .await
        .expect_err("invbroker.SplitStack must reject > i32::MAX quantity");
    assert_eq!(split_inv_err.code, "SplitFailed");
    let split_cp_err = ctx
        .dispatch_rpc(&mut s1, "contractProxy", None, "SplitStack", &[PyRep::Integer(JITA_STATION), PyRep::Integer(base_trit), PyRep::Integer(trunc_qty), PyRep::Integer(4)], &kw, None)
        .await
        .expect_err("contractProxy.SplitStack must reject > i32::MAX quantity");
    assert_eq!(split_cp_err.code, "SplitFailed");
    let merge_trunc_err = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            None,
            "MultiMerge",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(base_trit),
                PyRep::Integer(merge_dst_trit),
                PyRep::Integer(trunc_qty),
            ])])],
            &kw,
            None,
        )
        .await
        .expect_err("MultiMerge must reject > i32::MAX quantity");
    assert_eq!(merge_trunc_err.code, "StackFailed");
    assert_eq!(ctx.db.get_item(base_trit).unwrap().quantity, 100);

    // 2. ALSC item lock: splitting a locked stack inside an ALSC container must NOT mint unlocked units
    let alsc_id = 848_700_010_i64;
    let alsc_trit = 848_700_011_i64;
    seed_stack(&ctx, alsc_id, 17_366, 90_000_001, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_stack(&ctx, alsc_trit, 34, 90_000_001, alsc_id, InventoryFlag::Cargo, 50, false);
    ctx.dispatch_rpc(&mut s1, "invbroker", None, "ALSCLockItems", &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(alsc_trit)])], &kw, None)
        .await
        .expect("ALSCLockItems");
    let count_before = ctx.db.with_snapshot(|s| s.items.len());
    let alsc_split_err = ctx
        .dispatch_rpc(&mut s1, "invbroker", None, "SplitStack", &[PyRep::Integer(alsc_trit), PyRep::Integer(1)], &kw, None)
        .await
        .expect_err("ALSC locked stack must not split");
    assert_eq!(alsc_split_err.code, "ALSCItemLocked");
    let cp_alsc_err = ctx
        .dispatch_rpc(&mut s1, "contractProxy", None, "SplitStack", &[PyRep::Integer(JITA_STATION), PyRep::Integer(alsc_trit), PyRep::Integer(1), PyRep::Integer(4)], &kw, None)
        .await
        .expect_err("contractProxy.SplitStack must also reject ALSC locked stack");
    assert_eq!(cp_alsc_err.code, "ALSCItemLocked");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.len()), count_before);

    // 3. InventoryFlag::Locked, Impounded, AssetSafetyWrapRecord, and Plastic Wrap (3468) rejected at both RPC and DB layers
    for (idx, flag) in [InventoryFlag::Locked, InventoryFlag::Impounded]
        .into_iter()
        .enumerate()
    {
        let iid = 848_700_020 + idx as i64;
        seed_stack(&ctx, iid, 34, 90_000_001, JITA_STATION, flag, 20, false);
        assert!(ctx.db.split_stack(90_000_001, iid, 5).is_err(), "DB split_stack must reject {flag:?}");
        let rpc_err = ctx
            .dispatch_rpc(&mut s1, "invbroker", None, "SplitStack", &[PyRep::Integer(iid), PyRep::Integer(5)], &kw, None)
            .await
            .expect_err("RPC SplitStack must reject locked/impounded flag");
        assert_eq!(rpc_err.code, "SplitFailed");
    }

    let as_wrap_id = 848_700_025_i64;
    let as_trit_id = 848_700_026_i64;
    seed_stack(&ctx, as_wrap_id, 60, 90_000_001, JITA_STATION, InventoryFlag::Deliveries, 1, true);
    seed_stack(&ctx, as_trit_id, 34, 90_000_001, as_wrap_id, InventoryFlag::Cargo, 30, false);
    ctx.db.with_mut_snapshot(|snap| {
        use eve_server::services::station_structure::structure_upwell_ops::{
            load_ext_state, save_ext_state, AssetSafetyWrapRecord,
        };
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            as_wrap_id,
            AssetSafetyWrapRecord {
                wrap_id: as_wrap_id,
                owner_id: 90_000_001,
                is_corp: false,
                source_structure_id: 1_000_000_000_001,
                source_structure_name: "Destroyed Citadel".into(),
                solar_system_id: 30_000_142,
                created_unix: 1_700_000_000,
                deliverable_after_unix: 1_700_500_000,
                delivered_to_id: Some(JITA_STATION),
                item_ids: vec![as_trit_id],
                estimated_value_isk: 1_000.0,
                nearest_npc_station_id: Some(JITA_STATION),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        save_ext_state(snap, &ext);
    });
    let as_err = ctx
        .dispatch_rpc(&mut s1, "invbroker", None, "SplitStack", &[PyRep::Integer(as_trit_id), PyRep::Integer(5)], &kw, None)
        .await
        .expect_err("Asset Safety wrap contents must not split");
    assert_eq!(as_err.code, "SplitFailed");

    let wrap_id = 848_700_030_i64;
    let wrapped_trit = 848_700_031_i64;
    seed_stack(&ctx, wrap_id, 3468, 90_000_001, JITA_STATION, InventoryFlag::Hangar, 1, true);
    seed_stack(&ctx, wrapped_trit, 34, 90_000_001, wrap_id, InventoryFlag::Cargo, 40, false);
    assert!(ctx.db.split_stack(90_000_001, wrapped_trit, 10).is_err());
    let wrap_err = ctx
        .dispatch_rpc(&mut s1, "invbroker", None, "SplitStack", &[PyRep::Integer(wrapped_trit), PyRep::Integer(10)], &kw, None)
        .await
        .expect_err("Plastic Wrap contents must not split");
    assert_eq!(wrap_err.code, "SplitFailed");

    // 4. Password-protected corp Secure Container + corporate hangar Take roles
    let corp_id = 98_487_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut base_corp = snap.corporations.values().next().cloned().expect("seeded corp");
        base_corp.corporation_id = corp_id;
        base_corp.ceo_id = 90_000_001;
        base_corp.member_roles.clear();
        base_corp.member_roles.insert(90_000_001, i64::MAX as u64);
        base_corp.member_roles.insert(90_000_002, 0); // zero roles initially
        snap.corporations.insert(corp_id, base_corp);
        snap.characters.get_mut(&90_000_001).unwrap().corporation_id = corp_id;
        snap.characters.get_mut(&90_000_002).unwrap().corporation_id = corp_id;
    });
    s1.corporation_id = Some(corp_id);
    s2.corporation_id = Some(corp_id);

    let corp_div_stack = 848_700_040_i64;
    seed_stack(&ctx, corp_div_stack, 34, corp_id, JITA_STATION, InventoryFlag::CorpSAG2, 60, false);
    let role_err = ctx
        .dispatch_rpc(&mut s2, "invbroker", None, "SplitStack", &[PyRep::Integer(corp_div_stack), PyRep::Integer(10)], &kw, None)
        .await
        .expect_err("unprivileged corp member must not split CorpSAG2 stack");
    assert_ne!(role_err.code, "");
    assert_eq!(ctx.db.get_item(corp_div_stack).unwrap().quantity, 60);

    // Grant s2 full Take roles, but put stack inside a passworded ALSC container set by s1
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(90_000_002, i64::MAX as u64);
    });
    let pw_cont = 848_700_041_i64;
    let pw_stack = 848_700_042_i64;
    seed_stack(&ctx, pw_cont, 17_366, corp_id, JITA_STATION, InventoryFlag::CorpSAG1, 1, true);
    seed_stack(&ctx, pw_stack, 34, corp_id, pw_cont, InventoryFlag::Cargo, 40, false);
    ctx.dispatch_rpc(&mut s1, "invbroker", None, "SetPassword", &[PyRep::Integer(pw_cont), PyRep::String("vault-pw".into())], &kw, None)
        .await
        .expect("SetPassword on corp ALSC");
    let pw_err = ctx
        .dispatch_rpc(&mut s2, "invbroker", None, "SplitStack", &[PyRep::Integer(pw_stack), PyRep::Integer(15)], &kw, None)
        .await
        .expect_err("passworded container must block SplitStack before password entry");
    assert_eq!(pw_err.code, "ALSCPasswordRequired");

    // 5. Active fighter tube load & remote station co-location checks
    let remote_stack = 848_700_050_i64;
    seed_stack(&ctx, remote_stack, 34, 90_000_001, AMARR_STATION, InventoryFlag::Hangar, 30, false);
    let coloc_err = ctx
        .dispatch_rpc(&mut s1, "invbroker", None, "SplitStack", &[PyRep::Integer(remote_stack), PyRep::Integer(10)], &kw, None)
        .await
        .expect_err("remote station stack must not split while docked in Jita");
    assert_eq!(coloc_err.code, "SplitFailed");

    let active_ship = ctx.db.get_character(90_000_001).unwrap().active_ship_id;
    let tube_fighter = 848_700_060_i64;
    seed_stack(&ctx, tube_fighter, SIREN_I, 90_000_001, active_ship, InventoryFlag::FighterBay, 6, false);
    ctx.fighter_tube_loads.write().unwrap().insert((active_ship, 0), tube_fighter);
    let tube_err = ctx
        .dispatch_rpc(&mut s1, "invbroker", None, "SplitStack", &[PyRep::Integer(tube_fighter), PyRep::Integer(2)], &kw, None)
        .await
        .expect_err("loaded fighter tube squadron must not split via invbroker");
    assert_eq!(tube_err.code, "SplitFailed");
    let cp_tube_err = ctx
        .dispatch_rpc(&mut s1, "contractProxy", None, "SplitStack", &[PyRep::Integer(active_ship), PyRep::Integer(tube_fighter), PyRep::Integer(2), PyRep::Integer(158)], &kw, None)
        .await
        .expect_err("loaded fighter tube squadron must not split via contractProxy");
    assert_eq!(cp_tube_err.code, "SplitFailed");
    assert_eq!(ctx.db.get_item(tube_fighter).unwrap().quantity, 6);
}

#[tokio::test]
async fn test_r8_492_set_combat_state_no_compounding_and_siren_gtfo_tackle_cleanup() {
    // parity: rpc:fighterMgr.LoadFightersToTube, rpc:fighterMgr.LaunchFightersFromTubes, rpc:fighterMgr.CmdActivateAbilitySlots, rpc:fighterMgr.CmdDeactivateAbilitySlots, rpc:fighterMgr.CmdAbandonFighter
    let ctx = paused_ctx();
    let kw = HashMap::new();

    let mut carrier = common::gm_pilot_in_space(&ctx, 1, 91_492_001, ARCHON, NULLSEC_SYS).await;
    let victim = common::gm_pilot_in_space(&ctx, 2, 91_492_002, VEXOR, NULLSEC_SYS).await;
    let sys = ctx.systems.get_or_spawn(NULLSEC_SYS);

    // 1. Verify get_combat_state -> set_combat_state never compounds active WCS or scramble/immobilize deltas
    let base_core = sys
        .get_combat_state(victim.ship_id)
        .await
        .unwrap()
        .unwrap()
        .warp_core_strength;

    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: victim.ship_id,
        module_id: 990_001,
        duration_s: 10.0,
        warp_core_stab_bonus: 2,
        immobilize_self: true,
        ..Default::default()
    })
    .await
    .expect("register WCS + immobilizer");
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: carrier.ship_id,
        module_id: 990_002,
        target_id: Some(victim.ship_id),
        duration_s: 10.0,
        warp_scramble_points: 2,
        self_disable_mwd_mjd: true,
        ..Default::default()
    })
    .await
    .expect("register external scrambler");

    for _ in 0..10 {
        let mut cs = sys.get_combat_state(victim.ship_id).await.unwrap().unwrap();
        cs.capacitor = (cs.capacitor - 5.0).max(0.0);
        sys.set_combat_state(victim.ship_id, cs).await.expect("set_combat_state");
    }

    let mid_cs = sys.get_combat_state(victim.ship_id).await.unwrap().unwrap();
    assert_eq!(mid_cs.warp_core_strength, base_core + 2, "WCS bonus must never compound across set_combat_state");
    assert_eq!(mid_cs.warp_scramble_strength, 102, "Scram + self-immobilize must never compound across set_combat_state");

    sys.remove_module_cycle(victim.ship_id, 990_001).await.expect("remove WCS + immobilizer");
    sys.remove_module_cycle(carrier.ship_id, 990_002).await.expect("remove scram");
    let cleared_cs = sys.get_combat_state(victim.ship_id).await.unwrap().unwrap();
    assert_eq!(cleared_cs.warp_core_strength, base_core);
    assert_eq!(cleared_cs.warp_scramble_strength, 0);
    assert!(!sys.is_ball_scrambled(victim.ship_id).await.unwrap());

    // 2. Support Fighter Siren I (40346) tackle clears on DeactivateAbilitySlot AND CmdAbandonFighter
    let siren_item = common::stock_ship_bay(&ctx, &mut carrier, SIREN_I, 3, InventoryFlag::FighterBay as i64).await;
    ctx.dispatch_rpc(&mut carrier.sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(siren_item)], &kw, None)
        .await
        .expect("LoadFightersToTube");
    let PyRep::List(launched) = ctx
        .dispatch_rpc(&mut carrier.sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await
        .expect("LaunchFightersFromTubes")
    else {
        panic!("expected launched fighter list");
    };
    let siren_ball = launched[0].as_i64().expect("siren ball id");

    // Lock victim on carrier and move victim adjacent so Siren Slot 0 tackle is in range
    let (_, f_ball) = sys.get_ball_snapshot(siren_ball).await.unwrap().unwrap();
    assert!(sys.set_ball_position(victim.ship_id, f_ball.position + DVec3::new(1_500.0, 0.0, 0.0)).await);
    ctx.dispatch_rpc(&mut carrier.sess, "dogmaIM", None, "AddTargetOBO", &[PyRep::Integer(carrier.ship_id), PyRep::Integer(victim.ship_id)], &kw, None)
        .await
        .expect("AddTargetOBO");
    for _ in 0..30 {
        sys.step_manual(1.0).await.unwrap();
    }

    async fn activate_siren_tackle(ctx: &ServerContext, p: &mut LivePilot, f_ball: i64, target_id: i64) -> i64 {
        for slot in [0_i64, 1, 2] {
            if let Ok(res) = ctx
                .dispatch_rpc(
                    &mut p.sess,
                    "fighterMgr",
                    None,
                    "CmdActivateAbilitySlots",
                    &[PyRep::List(vec![PyRep::Integer(f_ball)]), PyRep::Integer(slot), PyRep::Integer(target_id)],
                    &HashMap::new(),
                    None,
                )
                .await
            {
                if res.dict_get("abilityID").and_then(|v| v.as_i64()).is_some_and(|aid| matches!(aid, 10 | 36)) {
                    return slot;
                }
            }
        }
        panic!("Siren tackle slot not found");
    }

    let tackle_slot = activate_siren_tackle(&ctx, &mut carrier, siren_ball, victim.ship_id).await;
    assert!(sys.get_combat_state(victim.ship_id).await.unwrap().unwrap().warp_scramble_strength > 0);
    assert!(sys.is_ball_scrambled(victim.ship_id).await.unwrap());

    ctx.dispatch_rpc(
        &mut carrier.sess,
        "fighterMgr",
        None,
        "CmdDeactivateAbilitySlots",
        &[PyRep::List(vec![PyRep::Integer(siren_ball)]), PyRep::Integer(tackle_slot)],
        &kw,
        None,
    )
    .await
    .expect("CmdDeactivateAbilitySlots");
    assert_eq!(sys.get_combat_state(victim.ship_id).await.unwrap().unwrap().warp_scramble_strength, 0);
    assert!(!sys.is_ball_scrambled(victim.ship_id).await.unwrap());

    // Re-activate tackle, then abandon fighter -> tackle must clear immediately
    ctx.advance_secs(30);
    let _ = activate_siren_tackle(&ctx, &mut carrier, siren_ball, victim.ship_id).await;
    assert!(sys.is_ball_scrambled(victim.ship_id).await.unwrap());
    ctx.dispatch_rpc(&mut carrier.sess, "fighterMgr", None, "CmdAbandonFighter", &[PyRep::Integer(siren_ball)], &kw, None)
        .await
        .expect("CmdAbandonFighter");
    assert_eq!(sys.get_combat_state(victim.ship_id).await.unwrap().unwrap().warp_scramble_strength, 0);

    // 3. SpatialGtfo must not permanently bake temporary external tackle into orig_warp_scramble_strength
    let titan_ball_id = 990_999_i64;
    sys.spawn_ball_with_combat(
        Ball {
            id: titan_ball_id,
            type_id: 11_567,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 80.0,
            inertia: 1.0,
            mass: 2.0e9,
            radius: 5_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::new(),
    )
    .await
    .unwrap();
    let v_pos = sys.get_ball_snapshot(victim.ship_id).await.unwrap().unwrap().1.position;
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: carrier.ship_id,
        module_id: 990_003,
        target_id: Some(victim.ship_id),
        duration_s: 10.0,
        warp_scramble_points: 2,
        ..Default::default()
    })
    .await
    .unwrap();
    let now = ctx.now_unix_secs();
    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id: NULLSEC_SYS,
            char_id: carrier.char_id,
            ship_id: titan_ball_id,
            ship_type_id: 11_567,
            mod_type_id: 24_550,
            kind: SuperweaponCycleKind::SpatialGtfo {
                center: v_pos.to_array(),
                sphere_radius_m: 15_000.0,
            },
            started_at_unix: now.saturating_sub(20),
            warmup_secs: 5,
            total_ticks: 1,
            ticks_applied: 0,
            immobilize_until_unix: now,
            orig_max_velocity: 80.0,
            orig_warp_scramble_strength: 0,
            orig_disallow_assistance: false,
            mobility_restored: true,
        });
    });
    step_bosonic_doomsday_system(&ctx, NULLSEC_SYS).await;
    sys.remove_module_cycle(carrier.ship_id, 990_003).await.unwrap();
    ctx.advance_secs(65);
    step_bosonic_doomsday_system(&ctx, NULLSEC_SYS).await;
    sys.step_manual(1.0).await.unwrap();
    assert_eq!(sys.get_combat_state(victim.ship_id).await.unwrap().unwrap().warp_scramble_strength, 0);
}
