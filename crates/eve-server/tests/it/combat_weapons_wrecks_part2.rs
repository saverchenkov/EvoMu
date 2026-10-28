#![allow(unused_imports)]
//! End-to-End Integration Tests for SDE-Driven Combat Mechanics, Turrets, Missiles, Smartbombs, Bombs, Breacher Pods & Shield Recharge (`C-1, C-2`).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    compute_probe_scan_signal_strength, dispatch_combat_rpc, evaluate_effective_signature_radius, passive_shield_recharge_rate_hps, step_combat_system,
    step_passive_shield_recharge, with_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

pub(crate) async fn setup_pilot_session(
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
    let sig_radius = ctx.sde.types.get(&ship_type_id).and_then(|t| t.default_attributes.get(&552).copied()).unwrap_or(40.0);
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
    let _ = sys_handle.spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(ship_type_id)).await;

    let mut sess = SessionState::new(client_id);
    let (user_id, corp_id) = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| (c.user_id, c.corporation_id)).unwrap_or((1, 1_000_002)));
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(solar_system_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    let _ = ctx.db.acquire_online_lock(user_id, client_id, Some(char_id), true, ctx.now_unix());
    sess
}

pub(crate) fn fit_module_and_charge(
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
async fn test_c1_triglavian_entropic_spool_and_edencom_vorton_arc_and_sig_radius() {
    // parity: feat:modules.turret-precursor
    // parity: feat:modules.vorton-projector
    // parity: feat:dogma.signature-radius
    eve_testkit::attest(&["feat:modules.turret-precursor", "feat:modules.vorton-projector", "feat:dogma.signature-radius"]);

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
    fit_module_and_charge(&ctx, char_id, ship_id, entropic_id, 47272, InventoryFlag::HiSlot0, Some((entropic_ammo_id, 47925, 200)));

    let kw = HashMap::new();
    ctx.set_invention_roll_override(Some(0.30));

    for &tid in &target_ids {
        let _ = sys_handle
            .add_target_lock(ship_id, TargetLock { target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) })
            .await;
    }

    let hp0_init = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;

    // Cycle 0: spoolMultiplier == 1.0
    let c0 = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[0])], &kw)
        .await
        .expect("Entropic Cycle 0");
    let m0 = dict_get(&c0, "spoolMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((m0 - 1.0).abs() < 1e-6, "First entropic cycle must start at 1.0x");
    let hp_after_c0 = sys_handle.get_combat_state(target_ids[0]).await.unwrap().unwrap().shield_hp;
    let loss_c0 = hp0_init - hp_after_c0;
    assert!(loss_c0 > 0.0, "Cycle 0 must deal damage to target");

    // Activating already active module on same target returns AlreadyActive
    let err_already = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[0])], &kw).await;
    assert!(
        matches!(err_already, Err(ref e) if e.code == "AlreadyActive"),
        "Activating already-active entropic module on same target must fail with AlreadyActive"
    );

    // Step 1 cycle (3.5s) on same target: spoolMultiplier == 1.07 (`attr 2733 = 0.07`)
    step_combat_system(&ctx, sys_id, 3.5).await;
    let m1 = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, entropic_id);
    assert!((m1 - 1.07).abs() < 1e-6, "Second entropic cycle must spool to 1.07x (got {m1})");
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
    assert!((m_max - 3.125).abs() < 1e-6, "Entropic spool must cap at 3.125x (got {m_max})");

    // Switching target resets spool to 1.0x
    let c_switch = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[1])], &kw)
        .await
        .expect("Entropic Switch Target");
    let m_switch = dict_get(&c_switch, "spoolMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((m_switch - 1.0).abs() < 1e-6, "Switching target must reset entropic spool to 1.0x");

    // Advance ticks so entropic multiplier spools above 1.0x on target_ids[1] before testing Deactivate
    step_combat_system(&ctx, sys_id, 3.5).await;
    let m_spooled = eve_server::services::combat_weapons::get_entropic_spool_multiplier(&ctx, ship_id, entropic_id);
    assert!(m_spooled > 1.05, "Spool multiplier on target 1 must spool up before deactivation (got {m_spooled})");

    // Deactivating module (`dogmaIM.Deactivate`) also resets spool
    let _ = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Deactivate", &[PyRep::Integer(entropic_id)], &HashMap::new()).await.expect("Deactivate entropic");
    let c_after_deact = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(entropic_id), PyRep::Integer(target_ids[1])], &kw)
        .await
        .expect("Entropic after Deactivate");
    let m_after_deact = dict_get(&c_after_deact, "spoolMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((m_after_deact - 1.0).abs() < 1e-6, "Deactivating entropic disintegrator must reset spool to 1.0x");

    // 2. EDENCOM Vorton Projector (`Small Vorton Projector I` 54739 + `GalvaSurge Condenser Pack S` 54769)
    let vorton_id = 201_003;
    let vorton_ammo_id = 201_004;
    fit_module_and_charge(&ctx, char_id, ship_id, vorton_id, 54739, InventoryFlag::HiSlot1, Some((vorton_ammo_id, 54769, 100)));
    let vorton_res = dispatch_combat_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::Integer(target_ids[0])], &kw)
        .await
        .expect("Vorton Activate");
    let arc_targets = dict_get(&vorton_res, "secondaryTargetsHit").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!((5..=9).contains(&arc_targets), "Vorton projector must chain from primary target to nearby secondary targets within 10km (got {arc_targets})");

    // 3. Signature radius evaluation (`feat:dogma.signature-radius`)
    let base_sig = evaluate_effective_signature_radius(&ctx, char_id, ship_id, sys_id).await;
    assert!(base_sig > 25.0 && base_sig < 100.0);
    // Fit and activate 5MN Microwarpdrive I (type 434, +500% sig radius bloom)
    let mwd_id = 201_005;
    fit_module_and_charge(&ctx, char_id, ship_id, mwd_id, 434, InventoryFlag::MedSlot0, None);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mwd_id);
    });
    let bloomed_sig = evaluate_effective_signature_radius(&ctx, char_id, ship_id, sys_id).await;
    assert!(bloomed_sig > base_sig * 5.0, "Active MWD must bloom signature radius by +500% (base={base_sig}, bloomed={bloomed_sig})");
    let scan_base = compute_probe_scan_signal_strength(40.0, base_sig, 1.0);
    let scan_bloomed = compute_probe_scan_signal_strength(40.0, bloomed_sig, 1.0);
    assert!(scan_bloomed > scan_base, "Larger signature radius must increase combat probe scan signal strength");
}
