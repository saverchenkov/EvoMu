//! Deep branch and mutation-killing unit tests for `fitting_module_cycles`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_module_cycles::*;
use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    name: &str,
    attrs: &[(u16, f64)],
    effects: &[u16],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.volume = volume;
    t.capacity = capacity;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t.effect_ids = effects.to_vec();
    t
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, ship_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.clone_grade = 1;
        ch.fleet_id = None;
        snap.characters.insert(char_id, ch);
    });
}

fn insert_item(
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
                ..Default::default()
            },
        );
    });
}

async fn spawn_ship_with(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    type_id: u32,
    owner_id: i64,
    cs: CombatState,
    is_cloaked: bool,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id,
        char_id: Some(owner_id),
        corp_id: 98_000_001,
        security_status: 5.0,
        is_cloaked,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(ball, cs, meta)
        .await
        .expect("spawn ship");
}

#[tokio::test]
async fn nanite_burned_tethered_zarzakh_and_covert_cloak_guards() {
    let ctx = test_ctx_with_custom(|sde| {
        // 999_601: Offensive turret (group 53, effect 12)
        sde.types.insert(
            999_601,
            custom_inv_type(
                sde,
                999_601,
                53,
                7,
                5.0,
                1.0,
                "Test Blaster",
                &[(6, 5.0), (40, 100.0), (73, 5_000.0)],
                &[12],
            ),
        );
        // 11578: Covert Ops Cloaking Device II (group 330, effect 12 + cloakingWarpSafe 980)
        let mut eff = sde.effects.values().next().unwrap().clone();
        eff.effect_id = 980;
        eff.name = "cloakingWarpSafe".into();
        eff.is_warp_safe = true;
        sde.effects.insert(980, eff);
        sde.types.insert(
            11578,
            custom_inv_type(
                sde,
                11578,
                330,
                7,
                5.0,
                0.0,
                "Covert Ops Cloaking Device II",
                &[(73, 5_000.0)],
                &[12, 980],
            ),
        );
        // 999_602: HIC Warp Disruption Field Generator (group 899, attr 5561 = 1.0 Zarzakh hazard ban)
        sde.types.insert(
            999_602,
            custom_inv_type(
                sde,
                999_602,
                899,
                7,
                5.0,
                1.0,
                "Warp Disruption Field Generator I",
                &[(6, 10.0), (73, 5_000.0), (5561, 1.0)],
                &[12],
            ),
        );
    });

    let char_id = 97_100_001;
    let sys_id = 30_002_505;
    let ship_id = 1_000_800_001;
    let gun_id = 1_000_800_002;
    let cov_cloak_id = 1_000_800_003;
    let hic_id = 1_000_800_004;

    ensure_char(&ctx, char_id, 98_000_001, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, gun_id, 999_601, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, cov_cloak_id, 11578, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, hic_id, 999_602, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);

    let mut cs = CombatState::default();
    cs.capacitor = 500.0;
    cs.max_capacitor = 500.0;
    spawn_ship_with(&ctx, sys_id, ship_id, 587, char_id, cs, false).await;

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. ModuleBeingRepaired (active nanite repair on gun_id)
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .start_nanite_repair(crate::system_actor::ActiveNaniteRepair {
            char_id,
            ship_id,
            module_id: gun_id,
            initial_damage_hp: 10.0,
            paste_total_needed: 1,
            paste_consumed_so_far: 0,
            duration_s: 10.0,
            elapsed_s: 0.0,
        })
        .await
        .unwrap();
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(gun_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleBeingRepaired"
    );
    sys_h.cancel_ship_nanite_repairs_sync(ship_id, false);

    // 2. ModuleBurnedOut (`DAMAGE >= HP`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .get_mut(&gun_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 100.0);
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(gun_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleBurnedOut"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&gun_id).unwrap().dynamic_attributes.clear();
    });

    // 3. CantActivateWhileTethered on offensive weapon
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().is_tethered = true;
    }
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(gun_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantActivateWhileTethered"
    );
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().is_tethered = false;
    }

    // 4. ShipCannotUseCovertCloak on T1 Frigate (587)
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(cov_cloak_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipCannotUseCovertCloak"
    );

    // 5. Zarzakh (30_100_000) hazard ban blocks unscripted HIC (`ModuleDisallowedInHazardSystem`)
    let mut z_sess = sess.clone();
    z_sess.solar_system_id = Some(30_100_000);
    let mut z_cs = CombatState::default();
    z_cs.capacitor = 500.0;
    z_cs.max_capacitor = 500.0;
    spawn_ship_with(&ctx, 30_100_000, ship_id, 587, char_id, z_cs, false).await;
    assert_eq!(
        handle_module_cycles(&ctx, &mut z_sess, None, "dogmaIM", "Activate", &[PyRep::Integer(hic_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .message,
        "ModuleDisallowedInHazardSystem"
    );
}

#[tokio::test]
async fn interdiction_sphere_target_locks_passive_targeter_cap_booster_and_safety() {
    let ctx = test_ctx_with_custom(|sde| {
        // 999_610: Interdiction Sphere Launcher (group 589)
        sde.types.insert(
            999_610,
            custom_inv_type(sde, 999_610, 589, 7, 5.0, 5.0, "Interdiction Sphere Launcher I", &[(73, 5_000.0)], &[12]),
        );
        // 999_611: Salvager I (group 1122) -> requires target
        sde.types.insert(
            999_611,
            custom_inv_type(sde, 999_611, 1122, 7, 5.0, 0.0, "Salvager I", &[(6, 10.0), (73, 5_000.0)], &[12]),
        );
        // 999_612: Passive Targeter I (group 82)
        sde.types.insert(
            999_612,
            custom_inv_type(sde, 999_612, 82, 7, 5.0, 0.0, "Passive Targeter I", &[(6, 5.0), (73, 5_000.0)], &[13]),
        );
        // 999_613: Small Capacitor Booster I (group 76)
        sde.types.insert(
            999_613,
            custom_inv_type(sde, 999_613, 76, 7, 5.0, 5.0, "Small Capacitor Booster I", &[(73, 5_000.0)], &[13]),
        );
        // 999_614: Cap Booster 200 (charge, attr 67 = 200.0)
        sde.types.insert(
            999_614,
            custom_inv_type(sde, 999_614, 87, 8, 1.0, 0.0, "Cap Booster 200", &[(67, 200.0)], &[]),
        );
        // 999_615: Smartbomb (group 72)
        sde.types.insert(
            999_615,
            custom_inv_type(
                sde,
                999_615,
                72,
                7,
                5.0,
                0.0,
                "Small EMP Smartbomb I",
                &[(6, 20.0), (73, 5_000.0), (114, 50.0)],
                &[12],
            ),
        );
        // 999_616: Warp Disruptor (group 52, attr 103 = 1.0)
        sde.types.insert(
            999_616,
            custom_inv_type(
                sde,
                999_616,
                52,
                7,
                5.0,
                0.0,
                "Warp Disruptor I",
                &[(6, 10.0), (73, 5_000.0), (103, 1.0)],
                &[13],
            ),
        );
    });

    let char_id = 97_200_001;
    let hs_sys = 30_000_142; // Jita (high-sec)
    let ship_id = 1_000_810_001;
    let target_ship_id = 1_000_810_099;
    let isl_id = 1_000_810_002;
    let salv_id = 1_000_810_003;
    let pass_id = 1_000_810_004;
    let capb_id = 1_000_810_005;
    let sb_id = 1_000_810_006;
    let scram_id = 1_000_810_007;

    ensure_char(&ctx, char_id, 98_000_001, hs_sys, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, hs_sys as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, isl_id, 999_610, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, salv_id, 999_611, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
    insert_item(&ctx, pass_id, 999_612, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    insert_item(&ctx, capb_id, 999_613, char_id, ship_id, InventoryFlag::MedSlot1, 1, true);
    insert_item(&ctx, sb_id, 999_615, char_id, ship_id, InventoryFlag::HiSlot2, 1, true);
    insert_item(&ctx, scram_id, 999_616, char_id, ship_id, InventoryFlag::MedSlot2, 1, true);

    let mut cs = CombatState::default();
    cs.capacitor = 100.0;
    cs.max_capacitor = 500.0;
    cs.is_cloaked = true;
    spawn_ship_with(&ctx, hs_sys, ship_id, 587, char_id, cs.clone(), true).await;
    cs.is_cloaked = false;
    spawn_ship_with(&ctx, hs_sys, target_ship_id, 587, char_id + 1, cs.clone(), false).await;

    let kw = HashMap::new();
    let mut sess = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        solar_system_id: Some(hs_sys),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. Interdiction Sphere Launcher while cloaked -> CannotActivateCloaked
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(isl_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotActivateCloaked"
    );
    spawn_ship_with(&ctx, hs_sys, ship_id, 587, char_id, cs, false).await;

    // 2. Salvager without target -> TargetRequired; Warp Disruptor on self -> CannotActivateOnSelf
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(salv_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetRequired"
    );
    assert_eq!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(scram_id), PyRep::String("default".into()), PyRep::Integer(ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotActivateOnSelf"
    );

    // 3. Warp Disruptor on unlocked target -> TargetNotLocked; Passive Targeter silently locks target!
    assert_eq!(
        handle_module_cycles(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(scram_id), PyRep::String("default".into()), PyRep::Integer(target_ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotLocked"
    );
    let pass_rep = handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(pass_id), PyRep::String("default".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("passive targeter silent lock");
    assert_eq!(pass_rep.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    let sys_handle = ctx.systems.get_or_spawn(hs_sys);
    let pending_locks = sys_handle.get_target_locks(ship_id).await.unwrap();
    assert!(
        pending_locks
            .iter()
            .any(|l| l.target_id == target_ship_id && l.lock_time_ms > 0.0 && l.remaining_lock_ms > 0.0 && !l.locked)
    );
    for _ in 0..10 {
        sys_handle.tick().await.unwrap();
    }
    let locks = sys_handle.get_target_locks(ship_id).await.unwrap();
    assert!(locks.iter().any(|l| l.target_id == target_ship_id && l.locked));

    // 4. Cap Booster without charges -> NoChargesLoaded; with charge -> injects +200 GJ capacitor!
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(capb_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoChargesLoaded"
    );
    insert_item(
        &ctx,
        1_000_810_088,
        999_614,
        char_id,
        ship_id,
        InventoryFlag::MedSlot1,
        2,
        false,
    );
    let cap_rep = handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(capb_id)], &kw)
        .await
        .unwrap()
        .expect("cap booster injects cap");
    assert_eq!(cap_rep.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    let cs_after = ctx.systems.get_or_spawn(hs_sys).get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after.capacitor >= 290.0);

    // 5. Smartbomb in high-sec with Green safety -> CantDoInCurrentSafetyLevel
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut sess, None, "dogmaIM", "Activate", &[PyRep::Integer(sb_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .message,
        "CantDoInCurrentSafetyLevel"
    );
}
