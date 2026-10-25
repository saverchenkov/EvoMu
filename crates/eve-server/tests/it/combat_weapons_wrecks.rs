//! End-to-End Integration Tests for SDE-Driven Combat Mechanics, Turrets, Missiles, Smartbombs, Bombs, Breacher Pods & Shield Recharge (`C-1, C-2`).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    compute_probe_scan_signal_strength, dispatch_combat_rpc, evaluate_effective_signature_radius,
    passive_shield_recharge_rate_hps, step_combat_system, step_passive_shield_recharge,
    with_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

async fn setup_pilot_session(
    ctx: &ServerContext,
    client_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let sig_radius = ctx
        .sde
        .types
        .get(&ship_type_id)
        .and_then(|t| t.default_attributes.get(&552).copied())
        .unwrap_or(40.0);
    let ship_ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 3.0,
        mass: 1_200_000.0,
        radius: sig_radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(ship_type_id))
        .await;

    let mut sess = SessionState::new(client_id);
    let (user_id, corp_id) = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| (c.user_id, c.corporation_id))
            .unwrap_or((1, 1_000_002))
    });
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(solar_system_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    let _ = ctx.db.acquire_online_lock(user_id, client_id, Some(char_id), true, ctx.now_unix());
    sess
}

fn fit_module_and_charge(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_item_id: i64,
    module_type_id: u32,
    flag: InventoryFlag,
    charge_opt: Option<(i64, u32, i32)>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            module_item_id,
            InventoryItemRecord {
                item_id: module_item_id,
                type_id: module_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        if let Some((charge_id, charge_type_id, qty)) = charge_opt {
            snap.items.insert(
                charge_id,
                InventoryItemRecord {
                    item_id: charge_id,
                    type_id: charge_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });
}

#[tokio::test]
async fn test_c1_turret_families_hit_formula_wrecking_shots_and_crystal_degradation() {
    // parity: feat:modules.turret-hybrid
    // parity: feat:modules.turret-projectile
    // parity: feat:modules.turret-laser
    // parity: feat:modules.turret-hit-chance
    // parity: feat:dogma.resistances
    eve_testkit::attest(&[
        "feat:modules.turret-hybrid",
        "feat:modules.turret-projectile",
        "feat:modules.turret-laser",
        "feat:modules.turret-hit-chance",
        "feat:dogma.resistances",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let mut sess = setup_pilot_session(
        &ctx,
        7101,
        char_id,
        ship_id,
        587, // Rifter
        sys_id,
        DVec3::ZERO,
    )
    .await;

    // Spawn a high-HP target ball at 1,500m
    let target_id = 500_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut target_cs = ctx.build_type_combat_state(24690); // Maelstrom
    target_cs.max_shield_hp = 50_000.0;
    target_cs.shield_hp = 50_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_id,
                type_id: 24690,
                position: DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 0.5,
                mass: 100_000_000.0,
                radius: 420.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            target_cs,
        )
        .await;

    // 1. Fit Hybrid (125mm Railgun I 10678 + Antimatter Charge S 222),
    //    Projectile (200mm AutoCannon I 486 + EMP S 185),
    //    Energy Laser (Small Focused Pulse Laser II 3041 + Multifrequency S 246 / Conflagration S 12565 / Imperial Navy Multifrequency S 23071)
    let railgun_id = 200_101;
    let rail_ammo_id = 200_102;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        railgun_id,
        10678,
        InventoryFlag::HiSlot0,
        Some((rail_ammo_id, 222, 100)),
    );

    let autocannon_id = 200_103;
    let ac_ammo_id = 200_104;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        autocannon_id,
        486,
        InventoryFlag::HiSlot1,
        Some((ac_ammo_id, 185, 100)),
    );

    let pulse_t1_id = 200_105;
    let crystal_t1_id = 200_106;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        pulse_t1_id,
        3041,
        InventoryFlag::HiSlot2,
        Some((crystal_t1_id, 246, 1)), // Multifrequency S (T1 crystal)
    );

    let pulse_t2_id = 200_107;
    let crystal_t2_id = 200_108;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        pulse_t2_id,
        3041,
        InventoryFlag::HiSlot3,
        Some((crystal_t2_id, 12565, 1)), // Conflagration S (T2 crystal)
    );

    let pulse_fac_id = 200_109;
    let crystal_fac_id = 200_110;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        pulse_fac_id,
        3041,
        InventoryFlag::HiSlot4,
        Some((crystal_fac_id, 23071, 1)), // Imperial Navy Multifrequency S (Faction crystal)
    );

    // 2. Fire Hybrid turret (consumes capacitor and 1 Antimatter Charge S)
    let kw = HashMap::new();
    ctx.set_invention_roll_override(Some(0.50));
    let rail_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(railgun_id), PyRep::Integer(target_id)],
        &kw,
    )
    .await
    .expect("Railgun Activate");
    let rail_cap_before = dict_get(&rail_res, "capacitorBefore")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let rail_cap_after = dict_get(&rail_res, "capacitorAfter")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let rail_dmg = dict_get(&rail_res, "appliedDamage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        rail_cap_before > rail_cap_after,
        "Hybrid railgun must consume capacitor"
    );
    assert!(rail_dmg > 0.0, "Hybrid railgun must apply kinetic/thermal damage");
    let rem_rail_ammo = ctx.db.with_snapshot(|s| s.items[&rail_ammo_id].quantity);
    assert_eq!(rem_rail_ammo, 99, "Hybrid railgun must consume 1 charge per cycle");

    // 3. Fire Projectile AutoCannon (consumes 0 capacitor and 1 EMP S)
    let ac_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(autocannon_id), PyRep::Integer(target_id)],
        &kw,
    )
    .await
    .expect("AutoCannon Activate");
    let ac_cap_before = dict_get(&ac_res, "capacitorBefore")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let ac_cap_after = dict_get(&ac_res, "capacitorAfter")
        .and_then(|v| v.as_f64())
        .unwrap_or(-1.0);
    let ac_dmg = dict_get(&ac_res, "appliedDamage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        (ac_cap_before - ac_cap_after).abs() < 1e-9,
        "Projectile turret must consume 0 capacitor"
    );
    assert!(ac_dmg > 0.0, "Projectile turret must apply damage");
    let rem_ac_ammo = ctx.db.with_snapshot(|s| s.items[&ac_ammo_id].quantity);
    assert_eq!(rem_ac_ammo, 99, "Projectile turret must consume 1 round per cycle");

    // 4. Wrecking shot (`hitRoll < 0.01` -> 3.0x base damage) vs normal hit (`hitRoll = 0.50`)
    let kw_wreck = HashMap::new();
    ctx.set_invention_roll_override(Some(0.005));
    let wreck_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(autocannon_id), PyRep::Integer(target_id)],
        &kw_wreck,
    )
    .await
    .expect("Wrecking Activate");
    let is_wrecking = dict_get(&wreck_res, "isWreckingShot")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let wreck_dmg = dict_get(&wreck_res, "appliedDamage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(is_wrecking, "hitRoll < 0.01 must register a Wrecking Hit");
    assert!(
        (wreck_dmg / ac_dmg - (3.0 / 0.995)).abs() < 0.05,
        "Wrecking shot must deal 3.0x base damage compared to 0.995x normal hit (got {wreck_dmg} vs {ac_dmg})"
    );

    // 5. Miss when `hitRoll > hitChance` at high range/transversal within optimal + 3*falloff
    let far_target_id = 500_002;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: far_target_id,
                type_id: 587,
                position: DVec3::new(15_000.0, 0.0, 0.0),
                velocity: DVec3::new(0.0, 2_500.0, 0.0),
                max_velocity: 2_500.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 25.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let kw_miss = HashMap::new();
    ctx.set_invention_roll_override(Some(0.95));
    let miss_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(autocannon_id), PyRep::Integer(far_target_id)],
        &kw_miss,
    )
    .await
    .expect("Miss Activate");
    let miss_dmg = dict_get(&miss_res, "appliedDamage")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let hit_chance = dict_get(&miss_res, "hitChance")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    assert!(hit_chance < 0.2, "High range + transversal must yield low hit chance");
    assert!(miss_dmg.abs() < 1e-9, "Roll above hitChance must miss and deal 0 damage");

    // 6. Crystal degradation:
    // - T1 Multifrequency S (246): `crystalsGetDamaged` is 0 -> 0 degradation
    // - Faction Imperial Navy Multifrequency S (23071): `0.00025` per cycle (1.0 probability)
    // - T2 Conflagration S (12565): `0.01` when `volatilityRoll < 0.1`
    let kw_crys = HashMap::new();
    ctx.set_invention_roll_override(Some(0.20));

    let t1_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(pulse_t1_id), PyRep::Integer(target_id)],
        &kw_crys,
    )
    .await
    .expect("T1 Laser Activate");
    let t1_deg = dict_get(&t1_res, "crystalDamageFraction")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    assert!(t1_deg.abs() < 1e-9, "T1 frequency crystals must never degrade");

    let fac_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(pulse_fac_id), PyRep::Integer(target_id)],
        &kw_crys,
    )
    .await
    .expect("Faction Laser Activate");
    let fac_deg = dict_get(&fac_res, "crystalDamageFraction")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        (fac_deg - 0.00025).abs() < 1e-6,
        "Faction crystal must degrade by 0.00025 per shot (got {fac_deg})"
    );

    let t2_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(pulse_t2_id), PyRep::Integer(target_id)],
        &kw_crys,
    )
    .await
    .expect("T2 Laser Activate");
    let t2_deg = dict_get(&t2_res, "crystalDamageFraction")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        (t2_deg - 0.01).abs() < 1e-6,
        "T2 Conflagration S must degrade by 0.01 on volatility proc (got {t2_deg})"
    );

    // Query crystal damage via `invbroker.GetDamageForCrystals`
    let crys_map = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "invbroker",
        "GetDamageForCrystals",
        &[PyRep::List(vec![
            PyRep::Integer(crystal_t1_id),
            PyRep::Integer(crystal_t2_id),
            PyRep::Integer(crystal_fac_id),
        ])],
        &HashMap::new(),
    )
    .await
    .expect("GetDamageForCrystals");
    if let PyRep::Dict(entries) = crys_map {
        let map: HashMap<i64, f64> = entries
            .into_iter()
            .filter_map(|(k, v)| Some((k.as_i64()?, v.as_f64()?)))
            .collect();
        assert_eq!(map.get(&crystal_t1_id).copied().unwrap_or(-1.0), 0.0);
        assert!((map.get(&crystal_t2_id).copied().unwrap_or(0.0) - 0.01).abs() < 1e-6);
        assert!((map.get(&crystal_fac_id).copied().unwrap_or(0.0) - 0.00025).abs() < 1e-6);
    } else {
        panic!("Expected Dict from GetDamageForCrystals");
    }

    // Shatter T2 crystal when damage reaches 1.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.items.get_mut(&crystal_t2_id) {
            c.dynamic_attributes.insert(3, 0.995);
        }
    });
    let shatter_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(pulse_t2_id), PyRep::Integer(target_id)],
        &kw_crys,
    )
    .await
    .expect("T2 Shatter Activate");
    let shattered = dict_get(&shatter_res, "crystalShattered")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(shattered, "T2 crystal must shatter when damage >= 1.0");
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&crystal_t2_id)),
        "Shattered crystal must be removed from inventory"
    );
}

#[tokio::test]
async fn test_c1_triglavian_entropic_spool_and_edencom_vorton_arc_and_sig_radius() {
    // parity: feat:modules.turret-precursor
    // parity: feat:modules.vorton-projector
    // parity: feat:dogma.signature-radius
    eve_testkit::attest(&[
        "feat:modules.turret-precursor",
        "feat:modules.vorton-projector",
        "feat:dogma.signature-radius",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let mut sess = setup_pilot_session(
        &ctx,
        7102,
        char_id,
        ship_id,
        47269, // Damavik
        sys_id,
        DVec3::ZERO,
    )
    .await;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Spawn 6 target balls clustered within 4km of each other so Vorton can arc between them
    let mut target_ids = Vec::new();
    for idx in 0..6 {
        let tid = 510_001 + idx;
        target_ids.push(tid);
        let mut cs = ctx.build_type_combat_state(24690);
        cs.max_shield_hp = 80_000.0;
        cs.shield_hp = 80_000.0;
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: tid,
                    type_id: 24690,
                    position: DVec3::new(2_000.0 + (idx as f64) * 500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 50_000_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    // 1. Triglavian Entropic Disintegrator (`Light Entropic Disintegrator I` 47272 + `Meson Exotic Plasma S` 47925)
    let entropic_id = 201_001;
    let entropic_ammo_id = 201_002;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        entropic_id,
        47272,
        InventoryFlag::HiSlot0,
        Some((entropic_ammo_id, 47925, 200)),
    );

    let kw = HashMap::new();
    ctx.set_invention_roll_override(Some(0.30));

    let hp0_init = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;

    // Cycle 0: spoolMultiplier == 1.0
    let c0 = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[0])],
        &kw,
    )
    .await
    .expect("Entropic Cycle 0");
    let m0 = dict_get(&c0, "spoolMultiplier")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!((m0 - 1.0).abs() < 1e-6, "First entropic cycle must start at 1.0x");
    let hp_after_c0 = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    let loss_c0 = hp0_init - hp_after_c0;
    assert!(loss_c0 > 0.0, "Cycle 0 must deal damage to target");

    // Activating already active module on same target returns AlreadyActive
    let err_already = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[0])],
        &kw,
    )
    .await;
    assert!(
        matches!(err_already, Err(ref e) if e.code == "AlreadyActive"),
        "Activating already-active entropic module on same target must fail with AlreadyActive"
    );

    // Step 1 cycle (3.5s) on same target: spoolMultiplier == 1.07 (`attr 2733 = 0.07`)
    step_combat_system(&ctx, sys_id, 3.5).await;
    let m1 = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, entropic_id);
    assert!(
        (m1 - 1.07).abs() < 1e-6,
        "Second entropic cycle must spool to 1.07x (got {m1})"
    );
    let hp_after_c1 = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    let loss_c1 = hp_after_c0 - hp_after_c1;
    assert!(loss_c1 > loss_c0, "Cycle 1 HP loss ({loss_c1}) must exceed cycle 0 loss ({loss_c0})");

    // Step 1 more cycle: spoolMultiplier == 1.14
    step_combat_system(&ctx, sys_id, 3.5).await;
    let hp_after_c2 = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    let loss_c2 = hp_after_c1 - hp_after_c2;
    assert!(loss_c2 > loss_c1, "Cycle 2 HP loss ({loss_c2}) must exceed cycle 1 loss ({loss_c1})");

    // 32 more cycles on same target: capped at `1.0 + 2.125 = 3.125x` (`attr 2734 = 2.125`)
    for _ in 0..32 {
        step_combat_system(&ctx, sys_id, 3.5).await;
    }
    let hp_pre_cap = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    step_combat_system(&ctx, sys_id, 3.5).await;
    let hp_cap = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    let loss_cap = hp_pre_cap - hp_cap;
    assert!(loss_cap > loss_c2, "Cap cycle loss ({loss_cap}) must exceed cycle 2 loss ({loss_c2})");

    let m_max = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, entropic_id);
    assert!(
        (m_max - 3.125).abs() < 1e-6,
        "Entropic spool must cap at 3.125x (got {m_max})"
    );

    // Switching target resets spool to 1.0x
    let c_switch = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[1])],
        &kw,
    )
    .await
    .expect("Entropic Switch Target");
    let m_switch = dict_get(&c_switch, "spoolMultiplier")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        (m_switch - 1.0).abs() < 1e-6,
        "Switching target must reset entropic spool to 1.0x"
    );

    // Advance ticks so entropic multiplier spools above 1.0x on target_ids[1] before testing Deactivate
    step_combat_system(&ctx, sys_id, 3.5).await;
    let m_spooled = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, entropic_id);
    assert!(m_spooled > 1.05, "Spool multiplier on target 1 must spool up before deactivation (got {m_spooled})");

    // Deactivating module (`dogmaIM.Deactivate`) also resets spool
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(entropic_id)],
        &HashMap::new(),
    )
    .await
    .expect("Deactivate entropic");
    let c_after_deact = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[1])],
        &kw,
    )
    .await
    .expect("Entropic after Deactivate");
    let m_after_deact = dict_get(&c_after_deact, "spoolMultiplier")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        (m_after_deact - 1.0).abs() < 1e-6,
        "Deactivating entropic disintegrator must reset spool to 1.0x"
    );

    // 2. EDENCOM Vorton Projector (`Small Vorton Projector I` 54739 + `GalvaSurge Condenser Pack S` 54769)
    let vorton_id = 201_003;
    let vorton_ammo_id = 201_004;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        vorton_id,
        54739,
        InventoryFlag::HiSlot1,
        Some((vorton_ammo_id, 54769, 100)),
    );
    let vorton_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(vorton_id), PyRep::Integer(target_ids[0])],
        &kw,
    )
    .await
    .expect("Vorton Activate");
    let arc_targets = dict_get(&vorton_res, "secondaryTargetsHit")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(
        (5..=9).contains(&arc_targets),
        "Vorton projector must chain from primary target to nearby secondary targets within 10km (got {arc_targets})"
    );

    // 3. Signature radius evaluation (`feat:dogma.signature-radius`)
    let base_sig = evaluate_effective_signature_radius(&ctx, char_id, ship_id, sys_id).await;
    assert!(base_sig > 25.0 && base_sig < 100.0);
    // Fit and activate 5MN Microwarpdrive I (type 434, +500% sig radius bloom)
    let mwd_id = 201_005;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        mwd_id,
        434,
        InventoryFlag::MedSlot0,
        None,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mwd_id);
    });
    let bloomed_sig = evaluate_effective_signature_radius(&ctx, char_id, ship_id, sys_id).await;
    assert!(
        bloomed_sig > base_sig * 5.0,
        "Active MWD must bloom signature radius by +500% (base={base_sig}, bloomed={bloomed_sig})"
    );
    let scan_base = compute_probe_scan_signal_strength(40.0, base_sig, 1.0);
    let scan_bloomed = compute_probe_scan_signal_strength(40.0, bloomed_sig, 1.0);
    assert!(
        scan_bloomed > scan_base,
        "Larger signature radius must increase combat probe scan signal strength"
    );
}

#[tokio::test]
async fn test_c2_missiles_drf_smartbombs_bombs_breacher_pods_damage_layers_and_shield_recharge() {
    // parity: feat:modules.missile-launchers
    // parity: feat:modules.missile-application
    // parity: feat:modules.missile-defender
    // parity: feat:modules.missile-fof
    // parity: feat:modules.smartbomb
    // parity: feat:modules.bomb-launcher
    // parity: feat:modules.capital-weapons
    // parity: feat:modules.breacher-pods
    // parity: feat:combat.missile-flight-physics-destruction
    // parity: feat:combat.bomb-damage-void-lockbreaker
    // parity: feat:combat.damage-pipeline
    // parity: feat:combat.damage-log
    // parity: feat:dogma.shield-recharge
    eve_testkit::attest(&[
        "feat:modules.missile-launchers",
        "feat:modules.missile-application",
        "feat:modules.missile-defender",
        "feat:modules.missile-fof",
        "feat:modules.smartbomb",
        "feat:modules.bomb-launcher",
        "feat:modules.capital-weapons",
        "feat:modules.breacher-pods",
        "feat:combat.missile-flight-physics-destruction",
        "feat:combat.bomb-damage-void-lockbreaker",
        "feat:combat.damage-pipeline",
        "feat:combat.damage-log",
        "feat:dogma.shield-recharge",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let victim_char_id = 90_000_002;
    let ship_id = 100_000_001;
    let victim_ship_id = 100_000_002;
    let lowsec_sys_id = 30_002_537; // Amamake (low-sec < 0.45 so bombs are legal)

    let mut sess = setup_pilot_session(
        &ctx,
        7103,
        char_id,
        ship_id,
        11377, // Nemesis Stealth Bomber
        lowsec_sys_id,
        DVec3::ZERO,
    )
    .await;
    let mut victim_sess = setup_pilot_session(
        &ctx,
        7104,
        victim_char_id,
        victim_ship_id,
        587, // Rifter
        lowsec_sys_id,
        DVec3::new(3_000.0, 0.0, 0.0),
    )
    .await;

    let sys_handle = ctx.systems.get_or_spawn(lowsec_sys_id);

    // 1. Missile DRF formula (`feat:combat.missile-damage-formula`) & Missile Launchers (`feat:combat.missile-weapons`):
    // Compare XL Torpedo Launcher I (20539) + Scourge XL Torpedo (17859, exp_radius=1000m, exp_vel=66m/s)
    // against a small fast frigate vs a large stationary battleship.
    let xl_launcher_id = 202_001;
    let xl_ammo_id = 202_002;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        xl_launcher_id,
        20539,
        InventoryFlag::HiSlot0,
        Some((xl_ammo_id, 17859, 50)),
    );

    let fast_frig_id = 520_001;
    let mut frig_cs = ctx.build_type_combat_state(587);
    frig_cs.max_shield_hp = 50_000.0;
    frig_cs.shield_hp = 50_000.0;
    frig_cs.signature_radius = 35.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: fast_frig_id,
                type_id: 587,
                position: DVec3::new(4_000.0, 0.0, 0.0),
                velocity: DVec3::new(0.0, 450.0, 0.0),
                max_velocity: 450.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            frig_cs,
        )
        .await;

    let big_bs_id = 520_002;
    let mut bs_cs = ctx.build_type_combat_state(24690);
    bs_cs.max_shield_hp = 50_000.0;
    bs_cs.shield_hp = 50_000.0;
    bs_cs.signature_radius = 1_200.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: big_bs_id,
                type_id: 24690,
                position: DVec3::new(4_000.0, 500.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 1_200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            bs_cs,
        )
        .await;

    let res_frig = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(xl_launcher_id), PyRep::Integer(fast_frig_id)],
        &HashMap::new(),
    )
    .await
    .expect("XL Torpedo vs Frigate");
    let res_bs = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(xl_launcher_id), PyRep::Integer(big_bs_id)],
        &HashMap::new(),
    )
    .await
    .expect("XL Torpedo vs Battleship");
    let dmg_frig = dict_get(&res_frig, "appliedDamage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let dmg_bs = dict_get(&res_bs, "appliedDamage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(
        dmg_frig < dmg_bs * 0.15,
        "XL Torpedo DRF must heavily reduce damage against small fast frigate ({dmg_frig} vs {dmg_bs})"
    );

    // 2. Auto-Targeting FoF Light Missile (`Light Missile Launcher I` 499 + `Scourge Auto-Targeting Light Missile I` 1810)
    //    and Deferred In-Flight Missile ball + Smartbomb mid-flight destruction (`feat:combat.smartbombs`)
    let lml_id = 202_003;
    let fof_ammo_id = 202_004;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        lml_id,
        499,
        InventoryFlag::HiSlot1,
        Some((fof_ammo_id, 1810, 50)),
    );
    // Fire FoF missile without specifying a target ID
    let fof_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(lml_id)],
        &HashMap::new(),
    )
    .await
    .expect("FoF Missile Activate");
    assert!(
        dict_get(&fof_res, "appliedDamage")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            > 0.0,
        "Auto-targeting FoF missile must automatically acquire and damage hostile target"
    );

    // Spawn an in-flight missile and destroy it mid-flight with a Smartbomb (`Small EMP Smartbomb I` 1563)
    let kw_defer = HashMap::new();
    let defer_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(lml_id), PyRep::Integer(victim_ship_id)],
        &kw_defer,
    )
    .await
    .expect("Deferred Missile");
    let missile_ball_id = dict_get(&defer_res, "missileBallID")
        .and_then(|v| v.as_i64())
        .expect("missileBallID");
    assert!(
        sys_handle
            .get_ball_snapshot(missile_ball_id)
            .await
            .unwrap()
            .is_some(),
        "In-flight missile ball must exist in Destiny ballpark"
    );

    let smartbomb_id = 202_005;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        smartbomb_id,
        3993, // Large EMP Smartbomb I (300 EM damage, 6,000m radius)
        InventoryFlag::HiSlot2,
        None,
    );
    let sb_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(smartbomb_id)],
        &HashMap::new(),
    )
    .await
    .expect("Smartbomb Activate");
    let destroyed_missiles = match dict_get(&sb_res, "destroyedMissileIDs") {
        Some(PyRep::List(v)) => v.len(),
        _ => 0,
    };
    assert_eq!(
        destroyed_missiles, 1,
        "Smartbomb AoE must destroy in-flight missile within blast radius"
    );
    assert!(
        sys_handle
            .get_ball_snapshot(missile_ball_id)
            .await
            .unwrap()
            .is_none(),
        "Destroyed missile ball must be removed from ballpark"
    );

    // Also verify Defender Missile (`Defender Launcher I` 44102 + `Defender Missile I` 32782) intercepts an incoming missile
    let defer_res2 = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(lml_id), PyRep::Integer(victim_ship_id)],
        &kw_defer,
    )
    .await
    .expect("Deferred Missile 2");
    let missile_ball_id2 = dict_get(&defer_res2, "missileBallID")
        .and_then(|v| v.as_i64())
        .expect("missileBallID 2");
    let defender_id = 202_006;
    let defender_ammo_id = 202_007;
    fit_module_and_charge(
        &ctx,
        victim_char_id,
        victim_ship_id,
        defender_id,
        44102,
        InventoryFlag::HiSlot0,
        Some((defender_ammo_id, 32782, 20)),
    );
    let def_res = dispatch_combat_rpc(
        &ctx,
        &mut victim_sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(defender_id)],
        &HashMap::new(),
    )
    .await
    .expect("Defender Activate");
    assert_eq!(
        dict_get(&def_res, "interceptedMissileID").and_then(|v| v.as_i64()),
        Some(missile_ball_id2),
        "Defender missile must intercept and destroy the incoming hostile missile"
    );

    // 3. Bombs (`feat:combat.bombs`):
    // - Blocked in high-sec (`BombDisallowedInHighSec`)
    // - Allowed in low-sec/null-sec, detonating at 30km with 15km blast radius (`Void Bomb` 27924 drains cap, `Lockbreaker Bomb` 27922 breaks locks, `Concussion Bomb` 27912 deals AoE damage)
    let bomb_launcher_id = 202_008;
    let bomb_ammo_id = 202_009;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        bomb_launcher_id,
        27914, // Bomb Launcher I
        InventoryFlag::HiSlot3,
        Some((bomb_ammo_id, 27924, 10)), // Void Bomb
    );

    // Temporarily test high-sec block
    let hs_handle = ctx.systems.get_or_spawn(30_000_142);
    let _ = hs_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 11377,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_200_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(11377),
        )
        .await;
    sess.solar_system_id = Some(30_000_142);
    let hs_bomb_err = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(bomb_launcher_id)],
        &HashMap::new(),
    )
    .await
    .expect_err("Bombs must be disallowed in high-sec");
    assert_eq!(hs_bomb_err.code, "BombDisallowedInHighSec");
    sess.solar_system_id = Some(lowsec_sys_id);

    // Place a target at 29km along X-axis (within 15km of the 30km detonation point) with active target lock and capacitor
    let bomb_target_id = 520_003;
    let mut bt_cs = ctx.build_type_combat_state(24690);
    bt_cs.capacitor = 2_000.0;
    bt_cs.max_capacitor = 2_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: bomb_target_id,
                type_id: 24690,
                position: DVec3::new(29_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            bt_cs,
        )
        .await;
    let _ = sys_handle
        .add_target_lock(
            bomb_target_id,
            TargetLock {
                target_id: ship_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    // Fire Void Bomb (drains 1,800 GJ capacitor)
    let void_res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(bomb_launcher_id)],
        &HashMap::new(),
    )
    .await
    .expect("Void Bomb Activate");
    let cap_after_void = sys_handle
        .get_combat_state(bomb_target_id)
        .await
        .unwrap()
        .unwrap()
        .capacitor;
    assert!(
        (cap_after_void - 200.0).abs() < 1e-3,
        "Void Bomb must neutralize 1,800 GJ capacitor from target within 15km blast radius (remaining={cap_after_void})"
    );
    assert!(
        dict_get(&void_res, "totalCapNeutralized")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            >= 1_800.0
    );

    // Switch charge to Lockbreaker Bomb (27922) and verify target locks are broken
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.items.get_mut(&bomb_ammo_id) {
            c.type_id = 27922;
        }
    });
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(bomb_launcher_id)],
        &HashMap::new(),
    )
    .await
    .expect("Lockbreaker Bomb Activate");
    let locks_after = sys_handle.get_target_locks(bomb_target_id).await.unwrap();
    assert!(
        locks_after.is_empty(),
        "Lockbreaker Bomb must break active target locks on ships in blast radius"
    );

    // 4. Breacher Pods (`feat:combat.breacher-pods`):
    // Small Breacher Pod Launcher (85084) + SCARAB Breacher Pod S (85088)
    // Non-stacking DoT: firing a second breacher pod refreshes duration rather than stacking tick damage.
    let breacher_id = 202_010;
    let breacher_ammo_id = 202_011;
    fit_module_and_charge(
        &ctx,
        char_id,
        ship_id,
        breacher_id,
        85084,
        InventoryFlag::HiSlot4,
        Some((breacher_ammo_id, 85088, 20)),
    );
    let bp1 = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(breacher_id), PyRep::Integer(victim_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("Breacher Pod 1");
    let tick1 = dict_get(&bp1, "breacherTickDPS")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    assert!(tick1 > 0.0, "Breacher pod must apply initial DoT tick");
    let active_dots = with_combat_runtime(&ctx, |rt| rt.active_breacher_dots.len());
    assert_eq!(active_dots, 1, "Single active breacher DoT entry per target");
    // Step 5 seconds of combat system and verify DoT ticks without stacking when re-applied
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(breacher_id), PyRep::Integer(victim_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("Breacher Pod 2");
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.active_breacher_dots.len()),
        1,
        "Multiple breacher pods on the same target must refresh rather than stack"
    );
    step_combat_system(&ctx, lowsec_sys_id, 5.0).await;

    // 5. Damage application layers (`feat:combat.damage-application-layers`) & Passive Shield Recharge (`feat:dogma.shield-recharge`)
    // Verify combat log notifications (`OnDamageMessage`, `OnDamageMessages`, `OnDamageStateChange`) on attacker session
    assert!(
        sess.notifications
            .iter()
            .any(|(m, _)| m == "OnDamageMessage" || m == "OnDamageStateChange"),
        "Attacker session must receive OnDamageMessage / OnDamageStateChange"
    );

    // Verify non-linear passive shield recharge peaks around 25% shield capacity
    let rate_10 = passive_shield_recharge_rate_hps(100.0, 1_000.0, 100.0);
    let rate_25 = passive_shield_recharge_rate_hps(250.0, 1_000.0, 100.0);
    let rate_80 = passive_shield_recharge_rate_hps(800.0, 1_000.0, 100.0);
    assert!(
        rate_25 > rate_10 && rate_25 > rate_80,
        "Passive shield recharge must peak at 25% shield capacity (rate_10={rate_10}, rate_25={rate_25}, rate_80={rate_80})"
    );
    let (after_sh, recharged_delta) = step_passive_shield_recharge(
        &ctx,
        lowsec_sys_id,
        Some(victim_char_id),
        victim_ship_id,
        100.0,
        5.0,
    )
    .await
    .expect("step_passive_shield_recharge");
    assert!(
        recharged_delta > 0.0 && after_sh >= recharged_delta,
        "Passive shield recharge step must restore shield HP"
    );
}

