//! Batch 39 open `feat:` parity integration tests (part 3: Clone Vat Bay & Energy Nosferatu).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

async fn setup_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
    corp_id: i64,
) -> SessionState {
    let mut sess = SessionState::new((900_000 + (char_id % 100_000)) as u64);
    sess.user_id = Some(char_id as u64);
    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.name = format!("Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.security_status = 0.0;
        ch.balance = IskCents::from_isk(500_000_000);
        s.characters.insert(char_id, ch);
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

    let ship_type = ctx.sde.types.get(&ship_type_id);
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 0.5,
                mass: ship_type.map(|t| t.mass).unwrap_or(1_500_000.0),
                radius: ship_type.map(|t| t.radius).unwrap_or(50.0),
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
    sess
}

#[tokio::test]
async fn test_b39_clone_vat_bay_and_nosferatu_relative_cap() {
    // parity: feat:modules.clone-vat-bay-fuel, feat:modules.energy-nosferatu-relative-capacitor
    eve_testkit::attest(&[
        "feat:modules.clone-vat-bay-fuel",
        "feat:modules.energy-nosferatu-relative-capacitor",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let sys_a = 30_000_012_u32;

    // 1. Clone Vat Bay on Titan consumes Liquid Ozone, immobilizes ship, and allows fleet clone install while active
    let c_titan = 91_000_051_i64;
    let s_titan = 101_000_051_i64;
    let c_mate = 91_000_052_i64;
    let s_mate = 101_000_052_i64;
    let mut sess_titan = setup_pilot(&ctx, c_titan, s_titan, 11567, sys_a, DVec3::new(200_000.0, 0.0, 0.0), 1_000_009).await;
    let mut sess_mate = setup_pilot(&ctx, c_mate, s_mate, 587, sys_a, DVec3::new(200_500.0, 0.0, 0.0), 1_000_009).await;
    let cvb_mod = 101_000_701_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(c_mate).or_default().insert(
            24242, // Infomorph Psychology I
            eve_db::TrainedSkillRecord { type_id: 24242, level: 1, skill_points: 1_000.0 },
        );
        s.fleets.insert(
            889,
            eve_db::FleetRecord {
                fleet_id: 889,
                boss_id: c_titan,
                members: vec![c_titan, c_mate],
                ..Default::default()
            },
        );
        s.items.insert(
            cvb_mod,
            InventoryItemRecord {
                item_id: cvb_mod,
                type_id: 23735, // Clone Vat Bay I
                owner_id: c_titan,
                location_id: s_titan,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            101_000_702,
            InventoryItemRecord {
                item_id: 101_000_702,
                type_id: 16273, // Liquid Ozone
                owner_id: c_titan,
                location_id: s_titan,
                flag: InventoryFlag::Cargo,
                quantity: 2_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    sess_titan.fleet_id = Some(889);
    sess_mate.fleet_id = Some(889);

    // Negative test: clone offer is rejected when Clone Vat Bay is not active
    let inactive_err = ctx
        .dispatch_rpc(
            &mut sess_titan,
            "jumpCloneSvc",
            None,
            "OfferShipCloneInstallation",
            &[PyRep::Integer(c_mate), PyRep::Integer(s_titan)],
            &kw,
            None,
        )
        .await
        .expect_err("Clone offer refused before module activation");
    assert_eq!(inactive_err.code, "NoActiveCloneVatBay");

    ctx.dispatch_rpc(&mut sess_titan, "dogmaIM", None, "Activate", &[PyRep::Integer(cvb_mod)], &kw, None)
        .await
        .expect("Titan activates Clone Vat Bay");
    let titan_lo = ctx.db.with_snapshot(|s| s.items.get(&101_000_702).map(|i| i.quantity).unwrap_or(0));
    assert!(titan_lo < 2_000, "Clone Vat Bay consumed Liquid Ozone ({titan_lo} < 2000)");
    let sys_handle = ctx.systems.get_or_spawn(sys_a);
    let (_, titan_ball) = sys_handle.get_ball_snapshot(s_titan).await.unwrap().unwrap();
    assert_eq!(titan_ball.max_velocity, 0.0, "Titan is immobilized while Clone Vat Bay is active");
    ctx.dispatch_rpc(
        &mut sess_titan,
        "jumpCloneSvc",
        None,
        "OfferShipCloneInstallation",
        &[PyRep::Integer(c_mate), PyRep::Integer(s_titan)],
        &kw,
        None,
    )
    .await
    .expect("OfferShipCloneInstallation while Clone Vat Bay active");
    ctx.dispatch_rpc(
        &mut sess_mate,
        "jumpCloneSvc",
        None,
        "AcceptShipCloneInstallation",
        &[],
        &kw,
        None,
    )
    .await
    .expect("Fleetmate installs clone in active Clone Vat Bay");

    // 2. Energy Nosferatu: percentage-based capacitor comparison + Blood Raider hull override via real module cycle
    let c_nos = 91_000_053_i64;
    let s_nos = 101_000_053_i64;
    let c_vic = 91_000_054_i64;
    let s_vic = 101_000_054_i64;
    let mut sess_nos = setup_pilot(&ctx, c_nos, s_nos, 587, sys_a, DVec3::new(300_000.0, 0.0, 0.0), 1_000_009).await;
    let _sess_vic = setup_pilot(&ctx, c_vic, s_vic, 587, sys_a, DVec3::new(301_000.0, 0.0, 0.0), 1_000_010).await;
    let nos_mod = 101_000_703_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            nos_mod,
            InventoryItemRecord {
                item_id: nos_mod,
                type_id: 530, // Small Energy Nosferatu I
                owner_id: c_nos,
                location_id: s_nos,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_a);
    let _ = sys_handle
        .add_target_lock(
            s_nos,
            eve_destiny::TargetLock {
                target_id: s_vic,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                max_range_m: Some(50_000.0),
                locked: true,
            },
        )
        .await;

    // Case A: Activator has higher raw GJ (600 / 2000 = 30%) than target (400 / 800 = 50%),
    // but lower percentage (30% < 50%) -> drains capacitor!
    let mut nos_cs = sys_handle.get_combat_state(s_nos).await.unwrap().unwrap();
    nos_cs.capacitor_recharge_time_ms = 0.0;
    nos_cs.max_capacitor = 2_000.0;
    nos_cs.capacitor = 600.0;
    sys_handle.set_combat_state(s_nos, nos_cs).await.unwrap();

    let mut vic_cs = sys_handle.get_combat_state(s_vic).await.unwrap().unwrap();
    vic_cs.capacitor_recharge_time_ms = 0.0;
    vic_cs.max_capacitor = 800.0;
    vic_cs.capacitor = 400.0;
    sys_handle.set_combat_state(s_vic, vic_cs).await.unwrap();

    ctx.dispatch_rpc(
        &mut sess_nos,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(nos_mod), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(s_vic), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Energy Nosferatu I");
    sys_handle.step_manual(10.0).await.expect("step nosferatu cycle");
    let vic_after_a = sys_handle.get_combat_state(s_vic).await.unwrap().unwrap().capacitor;
    let nos_after_a = sys_handle.get_combat_state(s_nos).await.unwrap().unwrap().capacitor;
    assert!(vic_after_a < 400.0 && nos_after_a > 600.0, "Lower cap% drains despite higher raw GJ ({vic_after_a}, {nos_after_a})");

    // Case B: Activator has higher cap% (1400 / 2000 = 70%) than target (400 / 800 = 50%) -> no drain!
    let _ = ctx
        .dispatch_rpc(
            &mut sess_nos,
            "dogmaIM",
            None,
            "Deactivate",
            &[PyRep::Integer(nos_mod), PyRep::String("energyNosferatuFalloff".into())],
            &kw,
            None,
        )
        .await;
    sys_handle.set_capacitor(s_nos, 1_400.0).await.unwrap();
    sys_handle.set_capacitor(s_vic, 400.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_nos,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(nos_mod), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(s_vic), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Activate Nosferatu at higher cap%");
    sys_handle.step_manual(10.0).await.expect("step nosferatu cycle at higher cap%");
    let vic_after_b = sys_handle.get_combat_state(s_vic).await.unwrap().unwrap().capacitor;
    assert!((vic_after_b - 400.0).abs() < 1e-3, "Higher cap% does not drain on standard hull ({vic_after_b})");

    // Case C: Blood Raider hull (Bhaalgorn 17920) overrides percentage restriction and drains at 70% vs 50%!
    let c_bhaal = 91_000_055_i64;
    let s_bhaal = 101_000_055_i64;
    let mut sess_bhaal = setup_pilot(&ctx, c_bhaal, s_bhaal, 17920, sys_a, DVec3::new(300_500.0, 0.0, 0.0), 1_000_009).await;
    let bhaal_nos_mod = 101_000_704_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            bhaal_nos_mod,
            InventoryItemRecord {
                item_id: bhaal_nos_mod,
                type_id: 530, // Small Energy Nosferatu I
                owner_id: c_bhaal,
                location_id: s_bhaal,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = sys_handle
        .add_target_lock(
            s_bhaal,
            eve_destiny::TargetLock {
                target_id: s_vic,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                max_range_m: Some(50_000.0),
                locked: true,
            },
        )
        .await;
    let mut bhaal_cs = sys_handle.get_combat_state(s_bhaal).await.unwrap().unwrap();
    bhaal_cs.capacitor_recharge_time_ms = 0.0;
    bhaal_cs.max_capacitor = 2_000.0;
    bhaal_cs.capacitor = 1_400.0;
    sys_handle.set_combat_state(s_bhaal, bhaal_cs).await.unwrap();
    sys_handle.set_capacitor(s_vic, 400.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_bhaal,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(bhaal_nos_mod), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(s_vic), PyRep::Integer(0)],
        &kw,
        None,
    )
    .await
    .expect("Activate Nosferatu on Bhaalgorn");
    sys_handle.step_manual(10.0).await.expect("step Bhaalgorn nosferatu cycle");
    let vic_after_c = sys_handle.get_combat_state(s_vic).await.unwrap().unwrap().capacitor;
    assert!(vic_after_c < 400.0, "Blood Raider hull overrides cap% restriction ({vic_after_c})");
}
