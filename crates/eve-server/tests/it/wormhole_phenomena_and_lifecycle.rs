//! Integration tests for Wormhole phenomena and lifecycle (`feat:wormholes.*`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::space_destiny::space_docking_cloaking::{
    compute_wormhole_stages_with_sde, derive_wormhole_buckets,
    ensure_system_signatures_and_anomalies, expire_and_respawn_wormholes_in_cluster,
};
use eve_server::services::space_destiny::wormhole_item_id_for_signature;
use eve_server::services::wormhole_lifecycle_ops::{
    compute_effective_ship_jump_mass, wormhole_exit_item_id_for_signature,
    wspace_static_specs_for_system,
};
use eve_server::system_actor::SystemHandle;
use glam::DVec3;
use std::collections::HashMap;

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() }
}

async fn spawn_wh_ball(h: &SystemHandle, id: i64, pos: DVec3, mass: f64) {
    let _ = h.spawn_ball(Ball {
        id, type_id: 26272, position: pos, velocity: DVec3::ZERO, max_velocity: 0.0,
        inertia: 1.0, mass, radius: 1500.0, speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await;
}

async fn spawn_ship_ball(ctx: &ServerContext, h: &SystemHandle, char_id: i64, ship_id: i64, tid: u32, pos: DVec3, mass: f64) {
    let _ = h.spawn_ball_with_combat(
        Ball {
            id: ship_id, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: 350.0,
            inertia: 1.0, mass, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, tid),
    ).await;
}

async fn setup_pilot_in_space(
    ctx: &ServerContext, session_id: u64, char_id: i64, solar_system_id: u32, ship_id: i64, ship_type_id: u32, pos: DVec3, mass_kg: f64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, ship_type_id, char_id, solar_system_id as i64, InventoryFlag::Hangar, 1, true));
    });
    spawn_ship_ball(ctx, &ctx.systems.get_or_spawn(solar_system_id), char_id, ship_id, ship_type_id, pos, mass_kg).await;
    ctx.advance_secs(15);
    sess
}

#[tokio::test]
async fn test_wormhole_lifetime_decay_stages_and_expiry_despawn() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let origin_sys = 31_002_000u32;
    let dest_sys = 31_002_001u32;
    let now0 = ctx.now_unix();

    // 1. Verify lifetime decay bands for wormhole types with 16h, 24h, 36h, and 48h lifespans:
    let test_wh_specs: [(u32, &str, u64); 4] = [
        (30645, "WST-DEC-48H", 48 * 3600),
        (30644, "WND-DEC-24H", 24 * 3600),
        (30647, "WND-DEC-16H", 16 * 3600),
        (30644, "WND-DEC-36H", 36 * 3600),
    ];

    let mut test_sigs = Vec::new();
    for &(tid, sig_id, life_secs) in &test_wh_specs {
        test_sigs.push(CosmicSignatureRecord {
            signature_id: sig_id.into(),
            solar_system_id: origin_sys,
            sig_group: "Wormhole".into(),
            type_name: format!("Wormhole {sig_id}"),
            position: [10_000_000_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(tid),
            destination_system_id: Some(dest_sys),
            created_unix: now0,
            lifetime_seconds: life_secs,
            max_mass_kg: 3_000_000_000.0,
            remaining_mass_kg: 3_000_000_000.0,
            max_jump_mass_kg: 1_800_000_000.0,
            collapsed: false,
        });
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(origin_sys, test_sigs.clone());
    });

    // At t = now0 + 1h: 48h and 36h have >24h remaining; 24h and 16h have 4h..=24h remaining
    let t_1h = now0 + 3600;
    for sig in &test_sigs {
        let b = derive_wormhole_buckets(sig, t_1h, &ctx.sde);
        let rem_hours = sig.lifetime_seconds.saturating_sub(3600) / 3600;
        let expected = if rem_hours > 24 {
            "This wormhole has not yet begun its natural cycle of decay and is not likely to disappear for some time"
        } else {
            "This wormhole is beginning to decay, and probably won't last another day"
        };
        assert_eq!(b.lifetime_description, expected, "Sig {} rem {}h", sig.signature_id, rem_hours);
    }

    // At t = now0 + 14h: 16h wormhole has 2h remaining (<4h)
    let t_14h = now0 + 14 * 3600;
    let b_16h_eol = derive_wormhole_buckets(&test_sigs[2], t_14h, &ctx.sde);
    assert_eq!(b_16h_eol.lifetime_description, "This wormhole is reaching the end of its natural lifetime and is likely to collapse very soon");
    let (_, _, _, wh_age, _) = compute_wormhole_stages_with_sde(&test_sigs[2], t_14h, Some(&ctx.sde));
    assert_eq!(wh_age, 2, "Age stage must be 2 (<4h)");

    // At t = now0 + 33h: 36h wormhole has 3h remaining (<4h)
    let t_33h = now0 + 33 * 3600;
    assert_eq!(
        derive_wormhole_buckets(&test_sigs[3], t_33h, &ctx.sde).lifetime_description,
        "This wormhole is reaching the end of its natural lifetime and is likely to collapse very soon"
    );

    // 2. Setup pilot and verify scanMgr.GetSystemScanMgr reports lifetimeDescription
    let char_id = 90_000_001i64;
    let ship_id = 910_001i64;
    let mut sess = setup_pilot_in_space(&ctx, 801, char_id, origin_sys, ship_id, 621, DVec3::ZERO, 11_910_000.0).await;
    let kw = HashMap::new();
    let scan_rep = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(origin_sys as i64)], &kw, None)
        .await
        .expect("GetSystemScanMgr");
    let PyRep::List(scanned) = scan_rep.dict_get("signatures").unwrap() else { panic!() };
    let find_desc = |sid: &str| -> Option<String> {
        scanned.iter().find(|s| s.dict_get("signatureID") == Some(&PyRep::String(sid.into())))
            .and_then(|s| s.dict_get("lifetimeDescription"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    assert_eq!(find_desc("WST-DEC-48H").as_deref(), Some("This wormhole has not yet begun its natural cycle of decay and is not likely to disappear for some time"));
    assert_eq!(find_desc("WND-DEC-24H").as_deref(), Some("This wormhole is beginning to decay, and probably won't last another day"));

    // 3. Verify despawn of entrance and exit balls on timer expiration
    let wh48_ent = wormhole_item_id_for_signature(origin_sys, "WST-DEC-48H");
    let wh48_ext = wormhole_exit_item_id_for_signature(dest_sys, "WST-DEC-48H");
    let orig_handle = ctx.systems.get_or_spawn(origin_sys);
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    spawn_wh_ball(&orig_handle, wh48_ent, DVec3::new(10_000_000_000.0, 0.0, 0.0), 1_000.0).await;
    spawn_wh_ball(&dest_handle, wh48_ext, DVec3::new(10_000_000_000.0, 0.0, 0.0), 1_000.0).await;

    assert!(orig_handle.get_ball_snapshot(wh48_ent).await.unwrap().is_some());
    assert!(dest_handle.get_ball_snapshot(wh48_ext).await.unwrap().is_some());

    // Advance clock past 48h lifespan (total +49h = 176,400s) and run expiry sweep
    let t_expired = now0 + 49 * 3600;
    ctx.advance_secs(49 * 3600);
    expire_and_respawn_wormholes_in_cluster(&ctx, t_expired).await;

    // Both entrance and exit balls must despawn upon timer expiration
    assert!(orig_handle.get_ball_snapshot(wh48_ent).await.unwrap().is_none(), "Entrance ball must despawn");
    assert!(dest_handle.get_ball_snapshot(wh48_ext).await.unwrap().is_none(), "Exit ball must despawn");

    let post_sigs = ctx.db.with_snapshot(|s| s.cosmic_signatures.get(&origin_sys).cloned().unwrap());
    assert!(post_sigs.iter().find(|s| s.signature_id == "WST-DEC-48H").unwrap().collapsed);
}

// parity: feat:wormholes.mass-reduction-and-destabilization
#[tokio::test]
async fn test_wormhole_mass_reduction_destabilization_and_collapse() {
    eve_testkit::attest(&["feat:wormholes.mass-reduction-and-destabilization"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let char_id = 90_000_001i64;
    let ship_id = 910_002i64;
    let sys_id = 31_001_100u32;
    let dest_sys = 31_001_101u32;
    let plate_item_id = 910_010i64;
    let prop_item_id = 910_011i64;

    // 1. Effective ship jump mass calculation including armor plate & active prop additions:
    // Caracal (type 621) hull mass in SDE: 11,910,000 kg
    // 1600mm Steel Plates: +5,000,000 kg (attr 796)
    // 100MN Microwarpdrive: +50,000,000 kg (attr 796, group 46, active)
    let mut sess = setup_pilot_in_space(&ctx, 802, char_id, sys_id, ship_id, 621, DVec3::new(100.0, 0.0, 0.0), 11_910_000.0).await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut plate = make_item(plate_item_id, 20353, char_id, ship_id, InventoryFlag::LoSlot0, 1, true);
        plate.dynamic_attributes.insert(796, 5_000_000.0);
        snap.items.insert(plate_item_id, plate);
        snap.module_online_states.insert(plate_item_id, true);

        let mut prop = make_item(prop_item_id, 12058, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
        prop.dynamic_attributes.insert(796, 50_000_000.0);
        snap.items.insert(prop_item_id, prop);
        snap.module_online_states.insert(prop_item_id, true);
    });

    // Inactive prop: base + plate = 16,910,000 kg
    let (inactive_mass, prop_add0) = compute_effective_ship_jump_mass(&ctx, ship_id, 11_910_000.0);
    assert_eq!(inactive_mass, 16_910_000.0);
    assert_eq!(prop_add0, 0.0);

    // Active prop: base + plate + prop = 66,910,000 kg
    ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.insert(prop_item_id); });
    let (active_mass, prop_add1) = compute_effective_ship_jump_mass(&ctx, ship_id, 11_910_000.0);
    assert_eq!(active_mass, 66_910_000.0);
    assert_eq!(prop_add1, 50_000_000.0);

    // Deactivate prop module for jumps
    ctx.db.with_mut_snapshot(|snap| { snap.module_active_states.remove(&prop_item_id); });

    // 2. Mass stability thresholds (>50%, 10-50%, <10%)
    let max_wh_mass = 100_000_000.0;
    let mut sig = CosmicSignatureRecord {
        signature_id: "WST-MASS-100".into(), solar_system_id: sys_id, sig_group: "Wormhole".into(),
        type_name: "Wormhole H296".into(), position: [500.0, 0.0, 0.0], signal_strength: 1.0,
        wormhole_type_id: Some(30644), destination_system_id: Some(dest_sys), created_unix: ctx.now_unix(),
        lifetime_seconds: 86400, max_mass_kg: max_wh_mass, remaining_mass_kg: max_wh_mass,
        max_jump_mass_kg: 375_000_000.0, collapsed: false,
    };

    assert_eq!(
        derive_wormhole_buckets(&sig, ctx.now_unix(), &ctx.sde).mass_stability_description,
        "This wormhole has not yet had its stability significantly disrupted"
    );

    sig.remaining_mass_kg = 40_000_000.0; // 40%
    assert_eq!(
        derive_wormhole_buckets(&sig, ctx.now_unix(), &ctx.sde).mass_stability_description,
        "This wormhole has had its stability reduced by ships passing through it"
    );

    sig.remaining_mass_kg = 8_000_000.0; // 8%
    assert_eq!(
        derive_wormhole_buckets(&sig, ctx.now_unix(), &ctx.sde).mass_stability_description,
        "This wormhole has had its stability critically disrupted"
    );

    // 3. Perform jumps: mass reduction by exact ship mass (16,910,000 kg) and collapse on exhaustion
    sig.remaining_mass_kg = 40_000_000.0;
    ctx.db.with_mut_snapshot(|snap| { snap.cosmic_signatures.insert(sys_id, vec![sig.clone()]); });
    let wh_ent_id = wormhole_item_id_for_signature(sys_id, "WST-MASS-100");
    let wh_ext_id = wormhole_exit_item_id_for_signature(dest_sys, "WST-MASS-100");
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let dest_h = ctx.systems.get_or_spawn(dest_sys);
    spawn_wh_ball(&sys_h, wh_ent_id, DVec3::new(500.0, 0.0, 0.0), sig.remaining_mass_kg).await;
    spawn_wh_ball(&dest_h, wh_ext_id, DVec3::new(10_000.0, 0.0, 0.0), sig.remaining_mass_kg).await;

    let kw = HashMap::new();
    // 1st Jump: 40,000,000 - 16,910,000 = 23,090,000 kg left
    let jump1 = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_ent_id)], &kw, None).await.unwrap();
    let post_rem1 = jump1.dict_get("remainingMassKg").and_then(|v| v.as_f64()).unwrap();
    assert!((post_rem1 - 23_090_000.0).abs() < 1e-3);
    assert_eq!(jump1.dict_get("collapsed").and_then(|v| v.as_bool()), Some(false));

    // Return jump from dest_sys:
    sess.solar_system_id = Some(dest_sys);
    sess.station_id = None;
    sess.structure_id = None;
    spawn_ship_ball(&ctx, &dest_h, char_id, ship_id, 621, DVec3::new(10_100.0, 0.0, 0.0), 16_910_000.0).await;
    ctx.db.with_mut_snapshot(|snap| { snap.wh_polarizations.clear(); });
    ctx.advance_secs(15);

    // 2nd Jump: 23,090,000 - 16,910,000 = 6,180,000 kg left (<10% critical)
    let jump2 = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_ext_id)], &kw, None).await.unwrap();
    let post_rem2 = jump2.dict_get("remainingMassKg").and_then(|v| v.as_f64()).unwrap();
    assert!((post_rem2 - 6_180_000.0).abs() < 1e-3);
    assert_eq!(jump2.dict_get("massStabilityDescription").and_then(|v| v.as_str()), Some("This wormhole has had its stability critically disrupted"));

    // Reset position near entrance in sys_id:
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    spawn_ship_ball(&ctx, &sys_h, char_id, ship_id, 621, DVec3::new(600.0, 0.0, 0.0), 16_910_000.0).await;
    ctx.db.with_mut_snapshot(|snap| { snap.wh_polarizations.clear(); });
    ctx.advance_secs(15);

    // 3rd Jump: 6,180,000 kg left vs 16,910,000 kg ship -> collapses immediately!
    let jump3 = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_ent_id)], &kw, None).await.unwrap();
    assert_eq!(jump3.dict_get("collapsed").and_then(|v| v.as_bool()), Some(true));

    // Both balls despawned immediately on mass collapse
    assert!(sys_h.get_ball_snapshot(wh_ent_id).await.unwrap().is_none());
    assert!(dest_h.get_ball_snapshot(wh_ext_id).await.unwrap().is_none());

    // 4th Jump attempt fails with WormholeMassCollapsed
    ctx.advance_secs(15);
    let fail_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_ext_id)], &kw, None).await.unwrap_err();
    assert_eq!(fail_err.code, "WormholeMassCollapsed");
}

// parity: feat:wormholes.static-wormhole-respawn
#[tokio::test]
async fn test_wormhole_static_respawn_on_collapse() {
    eve_testkit::attest(&["feat:wormholes.static-wormhole-respawn"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let sys_id = 31_000_900u32; // C3 wormhole system

    // 1. Verify static connection types assigned in SDE
    let statics = wspace_static_specs_for_system(&ctx.sde, sys_id);
    assert!(!statics.is_empty(), "C3 wormhole must have assigned static spec");
    let (prefix, _, static_tid, _) = statics[0].clone();
    assert_eq!(prefix, "WST");

    let (initial_sigs, _) = ensure_system_signatures_and_anomalies(&ctx, sys_id).await;
    let initial_static = initial_sigs.iter().find(|s| s.signature_id.starts_with("WST-") && !s.collapsed).cloned().expect("initial static");
    assert_eq!(initial_static.wormhole_type_id, Some(static_tid));
    assert!(initial_static.remaining_mass_kg > 0.0);
    let old_sig_id = initial_static.signature_id.clone();
    let old_dest = initial_static.destination_system_id.unwrap();

    // 2. Collapse via Time Exhaustion
    let now0 = ctx.now_unix();
    ctx.advance_secs(initial_static.lifetime_seconds + 3600);
    expire_and_respawn_wormholes_in_cluster(&ctx, now0 + initial_static.lifetime_seconds + 3600).await;

    let post_time_sigs = ctx.db.with_snapshot(|s| s.cosmic_signatures.get(&sys_id).cloned().unwrap());
    assert!(post_time_sigs.iter().find(|s| s.signature_id == old_sig_id).unwrap().collapsed);

    let respawned_time_static = post_time_sigs.iter().find(|s| s.signature_id.starts_with("WST-") && !s.collapsed && s.signature_id != old_sig_id).cloned().expect("respawned static");
    assert_eq!(respawned_time_static.wormhole_type_id, Some(static_tid));
    assert_eq!(respawned_time_static.remaining_mass_kg, respawned_time_static.max_mass_kg);
    assert_ne!(respawned_time_static.destination_system_id, Some(old_dest));

    // 3. Collapse via Mass Exhaustion
    let mass_sig_id = respawned_time_static.signature_id.clone();
    let char_id = 90_000_001i64;
    let ship_id = 910_003i64;
    let mut sess = setup_pilot_in_space(
        &ctx, 803, char_id, sys_id, ship_id, 621,
        DVec3::from_array(respawned_time_static.position) + DVec3::new(200.0, 0.0, 0.0), 11_910_000.0,
    ).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(w) = snap.cosmic_signatures.get_mut(&sys_id).and_then(|l| l.iter_mut().find(|w| w.signature_id == mass_sig_id)) {
            w.remaining_mass_kg = 5_000_000.0;
        }
    });

    let wh_id = wormhole_item_id_for_signature(sys_id, &mass_sig_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    spawn_wh_ball(&sys_h, wh_id, DVec3::from_array(respawned_time_static.position), 5_000_000.0).await;

    let kw = HashMap::new();
    ctx.advance_secs(15);
    let mass_jump_rep = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_id)], &kw, None).await.unwrap();
    assert_eq!(mass_jump_rep.dict_get("collapsed").and_then(|v| v.as_bool()), Some(true));

    let post_mass_sigs = ctx.db.with_snapshot(|s| s.cosmic_signatures.get(&sys_id).cloned().unwrap());
    let respawned_mass_static = post_mass_sigs.iter().find(|s| s.signature_id.starts_with("WST-") && !s.collapsed && s.signature_id != mass_sig_id && s.signature_id != old_sig_id).cloned().expect("respawned mass static");
    assert_eq!(respawned_mass_static.wormhole_type_id, Some(static_tid));
    assert_eq!(respawned_mass_static.remaining_mass_kg, respawned_mass_static.max_mass_kg);
    assert!(!respawned_mass_static.collapsed);
}

// parity: feat:wormholes.system-phenomena-bonuses
#[tokio::test]
async fn test_wormhole_system_phenomena_dogma_modifiers() {
    eve_testkit::attest(&["feat:wormholes.system-phenomena-bonuses"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let base_attrs = || {
        HashMap::from([
            (64u16, 1.0),      // damageMultiplier
            (55u16, 100.0),    // rechargeRate (capacitor)
            (84u16, 100.0),    // armorDamageAmount / remote repair
            (160u16, 0.5),     // trackingSpeed
            (263u16, 1_000.0), // shieldCapacity
            (265u16, 1_000.0), // armorHP
            (267u16, 0.5),     // armorEmDamageResonance
            (271u16, 0.5),     // shieldEmDamageResonance
            (482u16, 1_000.0), // capacitorCapacity
            (552u16, 100.0),   // signatureRadius
            (654u16, 50.0),    // aoeCloudSize (missile explosion radius)
        ])
    };

    // 1. Wolf-Rayet: Armor HP bonus, small turret damage bonus, signature reduction, shield resist penalty
    let wr_tid = eve_dogma::wormhole_phenomenon_type_for_class_and_name(1, "Wolf-Rayet").unwrap();
    let mut wr = base_attrs();
    assert!(eve_dogma::apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 6, false, &mut wr));
    assert!(wr[&265] > 1_000.0, "Wolf-Rayet armor HP bonus");
    assert!(wr[&552] < 100.0, "Wolf-Rayet signature radius reduction");
    assert!(wr[&64] > 1.0, "Wolf-Rayet small turret damage bonus");
    assert!(wr[&271] > 0.5, "Wolf-Rayet shield resist penalty / resonance increase");

    // 2. Pulsar: Shield capacity bonus, signature radius penalty, armor resist penalty, cap recharge penalty
    let p_tid = eve_dogma::wormhole_phenomenon_type_for_class_and_name(1, "Pulsar").unwrap();
    let mut p = base_attrs();
    assert!(eve_dogma::apply_wormhole_phenomenon_to_attributes(&ctx.sde, p_tid, 6, false, &mut p));
    assert!(p[&263] > 1_000.0, "Pulsar shield capacity bonus");
    assert!(p[&552] > 100.0, "Pulsar signature radius penalty");
    assert!(p[&267] > 0.5, "Pulsar armor resist penalty / resonance increase");
    assert!(p[&55] < 100.0, "Pulsar capacitor recharge time modification");

    // 3. Magnetar: Damage multiplier bonus, missile explosion radius penalty, tracking penalty
    let mg_tid = eve_dogma::wormhole_phenomenon_type_for_class_and_name(1, "Magnetar").unwrap();
    let mut mg = base_attrs();
    assert!(eve_dogma::apply_wormhole_phenomenon_to_attributes(&ctx.sde, mg_tid, 6, false, &mut mg));
    assert!(mg[&64] > 1.0, "Magnetar damage multiplier bonus");
    assert!(mg[&654] > 50.0, "Magnetar missile explosion radius penalty");
    assert!(mg[&160] < 0.5, "Magnetar tracking penalty");

    // 4. Cataclysmic Variable: Remote repair bonus, local repair penalty, cap recharge penalty
    let cv_tid = eve_dogma::wormhole_phenomenon_type_for_class_and_name(1, "Cataclysmic Variable").unwrap();
    let mut cv = base_attrs();
    assert!(eve_dogma::apply_wormhole_phenomenon_to_attributes(&ctx.sde, cv_tid, 6, false, &mut cv));
    assert!(cv[&482] > 1_000.0, "Cataclysmic Variable capacitor capacity bonus");
    assert!(cv[&55] > 100.0, "Cataclysmic Variable capacitor recharge penalty");
    assert_ne!(cv[&84], 100.0, "Cataclysmic Variable repair modification");

    // 5. Structure and Sleeper NPC immunity
    let mut sleeper = base_attrs();
    assert!(!eve_dogma::apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 6, true, &mut sleeper));
    assert_eq!(sleeper[&265], 1_000.0);
    let mut structure = base_attrs();
    assert!(!eve_dogma::apply_wormhole_phenomenon_to_attributes(&ctx.sde, wr_tid, 65, false, &mut structure));
    assert_eq!(structure[&265], 1_000.0);

    // 6. Live ship in space in Class 2 Wolf-Rayet system 31000006
    let wr_sys = 31_000_006u32;
    let (raw_cls, eff_cls, name, beacon_tid) = eve_dogma::resolve_system_wormhole_phenomenon(&ctx.sde, wr_sys, None).expect("31000006 phenomenon");
    assert_eq!((raw_cls, eff_cls, name, beacon_tid), (18, 2, "Wolf-Rayet", 30875));

    let char_id = 90_000_001i64;
    let ship_id = 910_004i64;
    let mut sess = setup_pilot_in_space(&ctx, 804, char_id, wr_sys, ship_id, 587, DVec3::ZERO, 1_000_000.0).await;
    let kw = HashMap::new();
    let hud = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetSystemWideEffectsInfo", &[PyRep::Integer(wr_sys as i64)], &kw, None).await.unwrap();
    assert_eq!(hud.dict_get("phenomenonName").and_then(|v| v.as_str()), Some("Wolf-Rayet"));
    assert_eq!(hud.dict_get("appliesToCapsuleerShipsOnly").and_then(|v| v.as_bool()), Some(true));

    let all_info = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)], &kw, None).await.unwrap();
    let PyRep::Dict(entries) = all_info.dict_get("shipAttributes").unwrap() else { panic!() };
    let live_armor = entries.iter().find(|(k, _)| k.as_i64() == Some(265)).and_then(|(_, v)| v.as_f64()).unwrap();
    // Character skills provide 495.0 base armorHP; in C2 WR (1.44 multiplier), armorHP is 712.8.
    assert!((live_armor - 495.0 * 1.44).abs() < 1.0, "Live ship armorHP must reflect WR bonus: {live_armor}");
}
