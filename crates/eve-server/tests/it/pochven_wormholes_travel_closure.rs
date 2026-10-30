//! End-to-end V24.01 integration tests closing Pochven Wormholes, Pochven Travel,
//! Pochven Filaments (SDE Border/Krai/Home/Proximity-5/Devana), and Pochven Capital & Cyno Restrictions.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_cyno_portal::active_cyno_jammer;
use eve_server::services::pve_special_sites::triglavian_minor_victory_systems;
use eve_server::services::space_destiny::space_nav_travel::{distance_ly, plan_jump_drive};
use eve_server::services::space_destiny::space_pochven_wormholes::{
    is_pochven_wormhole, nearest_pochven_system_within_ly, pochven_kspace_neighbors_within_ly,
    pochven_static_wormholes, pochven_topology_pools, pochven_wormhole_ball_position,
};
use eve_server::services::space_destiny::{
    ensure_system_signatures_and_anomalies, wormhole_exit_item_id_for_signature,
    wormhole_item_id_for_signature,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn ctx_with_real_sde() -> ServerContext {
    let sde = match eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        Ok(reg) => Arc::new(reg),
        Err(_) => Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
    };
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    common::pin_clock(&ctx);
    ctx
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
async fn place_ship_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
    mass_kg: f64,
) {
    let old_sys = sess.solar_system_id.unwrap_or(sys_id);
    if old_sys != sys_id {
        let _ = ctx.systems.get_or_spawn(old_sys).remove_ball(ship_id).await;
    }
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    let (const_id, reg_id) = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| (s.constellation_id, s.region_id))
        .unwrap_or((0, 0));
    sess.constellation_id = Some(const_id);
    sess.region_id = Some(reg_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.constellation_id = const_id;
            ch.region_id = reg_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true),
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: mass_kg,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
        )
        .await;
}

// parity: feat:navigation.pochven-wormholes, feat:navigation.pochven-travel, feat:navigation.pochven-filaments, feat:navigation.pochven-capital-cyno-restrictions
#[tokio::test]
async fn test_pochven_wormholes_filaments_gates_and_capital_restrictions() {
    eve_testkit::attest(&[
        "feat:navigation.pochven-wormholes",
        "feat:navigation.pochven-travel",
        "feat:navigation.pochven-filaments",
        "feat:navigation.pochven-capital-cyno-restrictions",
    ]);
    let ctx = ctx_with_real_sde();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 630_000_001_i64;

    let mut sess = SessionState::new(884_101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // -------------------------------------------------------------------------
    // 1. Pochven Wormholes (feat:navigation.pochven-wormholes):
    //    - Home-region <= 5.0 LY proximity gating for K-space C729 (absent in distant systems)
    //    - Probabilistic W-space C2-C4 Pochven wormholes
    //    - SDE-derived K-space (<= 5.0 LY) and C4 destinations varying across Pochven systems
    //    - ~30 km offset between signature coordinates and wormhole ball (non-deadspace)
    //    - Scanning from Pochven side opens destination K162 and spawns guards
    // -------------------------------------------------------------------------
    let otela_sys = 30_000_157_u32; // Pochven (Krai Perun / Caldari proximity)
    let raravoss_sys = 30_002_225_u32; // Pochven (Krai Svarog / Amarr proximity)
    let jita_sys = 30_000_142_u32;
    let distant_kspace_sys = 30_004_759_u32; // 1DQ1-A in Delve (> 30 LY from Pochven)

    assert!(
        nearest_pochven_system_within_ly(&ctx.sde, distant_kspace_sys, 5.0, 0).is_none(),
        "Distant K-space system must be > 5.0 LY outside Pochven proximity"
    );
    let _ = ensure_system_signatures_and_anomalies(&ctx, distant_kspace_sys).await;
    let distant_has_poch_wh = ctx.db.with_snapshot(|s| {
        s.cosmic_signatures
            .get(&distant_kspace_sys)
            .is_some_and(|l| l.iter().any(|sig| is_pochven_wormhole(&ctx.sde, sig)))
    });
    assert!(
        !distant_has_poch_wh,
        "Distant K-space system outside 5.0 LY proximity must NOT spawn C729 Pochven wormholes"
    );

    // Find a K-space neighbor within <= 5.0 LY of Otela that spawns an inbound C729
    let otela_neighbors = pochven_kspace_neighbors_within_ly(&ctx.sde, otela_sys, 5.0);
    assert!(!otela_neighbors.is_empty(), "Otela must have SDE K-space neighbors within 5.0 LY");
    let mut kspace_poch_found = false;
    for &ksys in otela_neighbors.iter().take(12) {
        let _ = ensure_system_signatures_and_anomalies(&ctx, ksys).await;
        if ctx.db.with_snapshot(|s| {
            s.cosmic_signatures
                .get(&ksys)
                .is_some_and(|l| l.iter().any(|sig| is_pochven_wormhole(&ctx.sde, sig)))
        }) {
            kspace_poch_found = true;
            break;
        }
    }
    assert!(kspace_poch_found, "Home-proximity K-space neighbors must spawn C729 Pochven wormholes");

    let mut wspace_poch_count = 0usize;
    for wsys in 31_001_500_u32..31_001_550_u32 {
        let _ = ensure_system_signatures_and_anomalies(&ctx, wsys).await;
        if ctx.db.with_snapshot(|s| {
            s.cosmic_signatures
                .get(&wsys)
                .is_some_and(|l| l.iter().any(|sig| is_pochven_wormhole(&ctx.sde, sig)))
        }) {
            wspace_poch_count += 1;
        }
    }
    assert!(
        wspace_poch_count > 0 && wspace_poch_count < 50,
        "C2-C4 W-space systems must probabilistically spawn Pochven wormhole connections on a subset, got {wspace_poch_count}/50"
    );

    let poch_sigs = pochven_static_wormholes(&ctx.sde, otela_sys, 0, ctx.now_unix());
    let rara_sigs = pochven_static_wormholes(&ctx.sde, raravoss_sys, 0, ctx.now_unix());
    assert_eq!(poch_sigs.len(), 2, "Pochven static generator yields K-space C729 and C4 R081 wormholes");
    let pch_k_sig = poch_sigs[0].clone();
    let pch_dest_sys = pch_k_sig.destination_system_id.unwrap();
    let rara_dest_sys = rara_sigs[0].destination_system_id.unwrap();
    assert_ne!(pch_dest_sys, rara_dest_sys, "Pochven K-space static destinations must vary by system proximity");
    assert!(
        distance_ly(&ctx, otela_sys, pch_dest_sys) <= 5.0,
        "Otela C729 static destination must be within <= 5.0 LY"
    );
    let pcw_dest = poch_sigs[1].destination_system_id.unwrap();
    assert_eq!(
        ctx.sde.solar_systems.get(&pcw_dest).and_then(|s| s.wormhole_class_id),
        Some(4),
        "Pochven R081 static must target an SDE C4 wormhole system"
    );

    let k162_sig_id = format!("K162-{}", pch_k_sig.signature_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(otela_sys, poch_sigs.clone());
        if let Some(d_sigs) = snap.cosmic_signatures.get_mut(&pch_dest_sys) {
            d_sigs.retain(|s| s.signature_id != k162_sig_id);
        }
    });

    let sig_vec = DVec3::from_array(pch_k_sig.position);
    let wh_ball_pos = pochven_wormhole_ball_position(&ctx.sde, &pch_k_sig);
    assert!((wh_ball_pos.distance(sig_vec) - 30_000.0).abs() < 1.0, "Pochven wormhole ball sits ~30 km from signature");

    let k162_before = ctx.db.with_snapshot(|s| {
        s.cosmic_signatures.get(&pch_dest_sys).is_some_and(|l| l.iter().any(|sig| sig.signature_id == k162_sig_id))
    });
    assert!(!k162_before, "Destination K162 must not exist before scanning from Pochven side");

    place_ship_in_space(&ctx, &mut sess, char_id, otela_sys, ship_id, 621, sig_vec + DVec3::new(10_000.0, 0.0, 0.0), 11_300_000.0).await;
    let _scan_rows = common::probe_scan(&ctx, &mut sess, sig_vec, 0.25 * 149_597_870_700.0).await;

    let k162_after = ctx.db.with_snapshot(|s| {
        s.cosmic_signatures.get(&pch_dest_sys).is_some_and(|l| l.iter().any(|sig| sig.signature_id == k162_sig_id))
    });
    assert!(k162_after, "Scanning Pochven wormhole from Pochven side opens destination K162");

    let wh_id = wormhole_item_id_for_signature(otela_sys, &pch_k_sig.signature_id);
    let exit_id = wormhole_exit_item_id_for_signature(pch_dest_sys, &pch_k_sig.signature_id);
    let inside_types: Vec<u32> = {
        let h = ctx.systems.get_or_spawn(otela_sys);
        let bp = h.ballpark.read().unwrap();
        [10_i64, 11, 12, 13].iter().filter_map(|off| bp.balls.get(&(wh_id + off)).map(|b| b.type_id)).collect()
    };
    assert_eq!(inside_types, vec![48_087, 47_958, 48_245, 47_876], "Inside Pochven wormhole guarded by Pochven NPC mix");
    let outside_types: Vec<u32> = {
        let h = ctx.systems.get_or_spawn(pch_dest_sys);
        let bp = h.ballpark.read().unwrap();
        [10_i64, 11].iter().filter_map(|off| bp.balls.get(&(exit_id + off)).map(|b| b.type_id)).collect()
    };
    assert_eq!(outside_types, vec![48_087, 48_092], "Outside Pochven wormhole guarded by Triglavian NPCs");

    let otela_handle = ctx.systems.get_or_spawn(otela_sys);
    let _ = otela_handle.set_ball_position(ship_id, wh_ball_pos + DVec3::new(300_000.0, 0.0, 0.0)).await;
    let bey_otela = ctx.bind_object(&mut sess, "beyonce", otela_sys as i64).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_otela), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(wh_id), PyRep::Real(0.0)], &kw, None)
        .await
        .expect("Warp to Pochven wormhole");
    for _ in 0..30 {
        let _ = otela_handle.step_manual(1.0).await;
    }
    let landed_pos = otela_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
    assert!(landed_pos.distance(wh_ball_pos) < 5_000.0, "Pochven wormhole is non-deadspace and lands within normal warp range");

    let _ = ctx.systems.get_or_spawn(otela_sys).set_ball_position(ship_id, wh_ball_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.advance_secs(15);
    let wh_jump = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_id)], &kw, None)
        .await
        .expect("Jump through Pochven wormhole");
    assert_eq!(wh_jump.dict_get("destinationSystemID").and_then(|v| v.as_i64()), Some(pch_dest_sys as i64));
    assert_eq!(sess.solar_system_id, Some(pch_dest_sys));

    // -------------------------------------------------------------------------
    // 2. Pochven Filaments & Internal/Border Travel (feat:navigation.pochven-filaments, feat:navigation.pochven-travel):
    //    - Border-5 'Pochven' Filament (56076) -> 6 SDE border systems
    //    - Proximity-5 'Extraction' Filament (56068) -> K-space system <= 5.0 LY of Pochven origin
    //    - Cladistic-5 'Krai Veles' Filament (56072) -> Krai Veles constellation
    //    - Glorification-1 'Devana' Filament (56065) -> Triglavian Minor Victory system
    //    - Home-5 'Pochven' Filament (56080) -> requires >= 6.0 Triglavian standing -> 3 SDE home systems
    // -------------------------------------------------------------------------
    let (sde_border, _sde_internal, sde_home) = pochven_topology_pools(&ctx.sde);
    assert_eq!((sde_border.len(), sde_home.len()), (6, 3), "SDE topology must yield 6 border and 3 home systems");
    assert_eq!(ctx.sde.types.get(&56_076).map(|t| t.name.as_str()), Some("Border-5 'Pochven' Filament"));

    let _ = ctx.dispatch_rpc(&mut sess, "LSC", None, "JoinChannel", &[PyRep::String("solarsystemid2:30000157".into())], &kw, None).await.unwrap();
    let _ = ctx.dispatch_rpc(&mut sess, "LSC", None, "JoinChannel", &[PyRep::String("solarsystemid2:30000142".into())], &kw, None).await.unwrap();
    let (poch_delayed, jita_delayed) = ctx.db.with_snapshot(|s| {
        (
            s.chat_channels.get("solarsystemid2:30000157").is_some_and(|c| c.is_delayed_local),
            s.chat_channels.get("solarsystemid2:30000142").is_some_and(|c| c.is_delayed_local),
        )
    });
    assert!(poch_delayed && !jita_delayed, "Pochven uses delayed local while K-space uses immediate local");

    let (border_fil_id, krai_fil_id, home_fil_id, devana_fil_id, prox_fil_id) =
        (630_000_011_i64, 630_000_012_i64, 630_000_013_i64, 630_000_014_i64, 630_000_015_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(500_026, 2.0); // < 6.0 Triglavian standing
        }
        for (fid, ftype) in [
            (border_fil_id, 56_076_u32), // Border-5 'Pochven' Filament
            (krai_fil_id, 56_072),       // Cladistic-5 'Krai Veles' Filament
            (home_fil_id, 56_080),       // Home-5 'Pochven' Filament
            (devana_fil_id, 56_065),     // Glorification-1 'Devana' Filament
            (prox_fil_id, 56_068),       // Proximity-5 'Extraction' Filament
        ] {
            snap.items.insert(fid, make_item(fid, ftype, char_id, ship_id, InventoryFlag::Cargo, 5, false));
        }
    });

    ctx.advance_secs(15);
    let safe_spot = DVec3::new(9_000_000_000_000.0, 5_000_000_000.0, 0.0);
    place_ship_in_space(&ctx, &mut sess, char_id, jita_sys, ship_id, 621, safe_spot, 11_300_000.0).await;

    let solo_err = ctx
        .dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(devana_fil_id)], &kw, None)
        .await
        .expect_err("Solo pilot without fleet cannot activate filament");
    assert_eq!(solo_err.code, "NotInFleet");
    ctx.dispatch_rpc(&mut sess, "fleetMgr", None, "CreateFleet", &[], &kw, None).await.expect("CreateFleet");

    let exit_outside_err = ctx
        .dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(devana_fil_id)], &kw, None)
        .await
        .expect_err("Exit filament outside Pochven must fail");
    assert_eq!(exit_outside_err.code, "PochvenExitFilamentRequiresPochvenOrigin");

    let home_low_err = ctx
        .dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(home_fil_id)], &kw, None)
        .await
        .expect_err("Home filament with standing < 6.0 must fail");
    assert_eq!(home_low_err.code, "SystemCheck_JumpFailed_StandingsTooLow");

    // Activate real Border-5 'Pochven' Filament (56076) from K-space -> lands in SDE border system!
    let border_rep = ctx
        .dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(border_fil_id)], &kw, None)
        .await
        .expect("Border-5 'Pochven' Filament jump from K-space");
    let border_dest = border_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert!(sde_border.contains(&border_dest), "Border-5 filament must land in one of the 6 SDE Pochven border systems, got {border_dest}");

    // Entry filament inside Pochven fails
    ctx.advance_secs(905);
    let _ = ctx.systems.get_or_spawn(border_dest).set_ball_position(ship_id, safe_spot).await;
    let entry_inside_err = ctx
        .dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(border_fil_id)], &kw, None)
        .await
        .expect_err("Entry filament inside Pochven must fail");
    assert_eq!(entry_inside_err.code, "PochvenEntryFilamentDisallowedOrigin");

    // Proximity-5 'Extraction' Filament (56068) from Pochven -> lands in K-space <= 5.0 LY of origin!
    let prox_rep = ctx
        .dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(prox_fil_id)], &kw, None)
        .await
        .expect("Proximity-5 extraction filament from Pochven");
    let prox_dest = prox_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert_ne!(ctx.sde.solar_systems.get(&prox_dest).unwrap().region_id, 10_000_070);
    let prox_ly = distance_ly(&ctx, border_dest, prox_dest);
    assert!(prox_ly <= 5.0, "Proximity-5 extraction filament must land within <= 5.0 LY of Pochven origin, got {prox_ly:.3} LY");

    // Cladistic-5 'Krai Veles' Filament (56072) -> lands in Krai Veles (non-home when standing < 6.0)
    ctx.advance_secs(905);
    let _ = ctx.systems.get_or_spawn(prox_dest).set_ball_position(ship_id, safe_spot).await;
    let krai_rep = ctx
        .dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(krai_fil_id)], &kw, None)
        .await
        .expect("Krai Veles filament jump");
    let krai_sys = krai_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let krai_const = ctx.sde.solar_systems.get(&krai_sys).and_then(|s| ctx.sde.constellations.get(&s.constellation_id)).map(|c| c.name.as_str());
    assert_eq!(krai_const, Some("Krai Veles"));
    assert!(!sde_home.contains(&krai_sys));

    // Glorification-1 'Devana' Filament (56065) -> lands in Triglavian Minor Victory system (not trade hubs)
    ctx.advance_secs(905);
    let _ = ctx.systems.get_or_spawn(krai_sys).set_ball_position(ship_id, safe_spot).await;
    let devana_rep = ctx
        .dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(devana_fil_id)], &kw, None)
        .await
        .expect("Devana exit filament from Pochven");
    let devana_dest = devana_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert!(
        triglavian_minor_victory_systems().contains(&devana_dest),
        "Devana filament must jump to a Triglavian Minor Victory system, got {devana_dest}"
    );
    assert!(!matches!(devana_dest, 30_000_142 | 30_002_187));

    // Home-5 filament with standing >= 6.0 succeeds into one of the 3 SDE Pochven home systems
    ctx.advance_secs(905);
    let _ = ctx.systems.get_or_spawn(devana_dest).set_ball_position(ship_id, safe_spot).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(500_026, 6.5);
    });
    let home_rep = ctx
        .dispatch_rpc(&mut sess, "randomJumpMgr", None, "ActivateRandomJumpFilament", &[PyRep::Integer(home_fil_id)], &kw, None)
        .await
        .expect("Home filament jump with standing >= 6.0");
    let home_sys = home_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert!(sde_home.contains(&home_sys), "Home-5 filament must jump to SDE clade home system");

    // Internal Krai gate (50016518 Otela -> Nalvula 30001445) & home standing gate (50016520 Otela -> Kino 30001372)
    ctx.advance_secs(15);
    let gate_518_pos = ctx.sde.jumps.get(&50_016_518).map(|j| j.position).unwrap_or(DVec3::ZERO);
    place_ship_in_space(&ctx, &mut sess, char_id, otela_sys, ship_id, 621, gate_518_pos + DVec3::new(100.0, 0.0, 0.0), 11_300_000.0).await;
    let bey_otela = ctx.bind_object(&mut sess, "beyonce", otela_sys as i64).unwrap();
    let krai_gate_rep = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(bey_otela), "CmdStargateJump", &[PyRep::Integer(50_016_518)], &kw, None)
        .await
        .expect("Internal Krai gate Otela -> Nalvula");
    assert_eq!(krai_gate_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(30_001_445));

    ctx.advance_secs(15);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(500_026, 1.0);
    });
    let gate_520_pos = ctx.sde.jumps.get(&50_016_520).map(|j| j.position).unwrap_or(DVec3::ZERO);
    place_ship_in_space(&ctx, &mut sess, char_id, otela_sys, ship_id, 621, gate_520_pos + DVec3::new(100.0, 0.0, 0.0), 11_300_000.0).await;
    let bey_otela = ctx.bind_object(&mut sess, "beyonce", otela_sys as i64).unwrap();
    let home_gate_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(bey_otela), "CmdStargateJump", &[PyRep::Integer(50_016_520)], &kw, None)
        .await
        .expect_err("Pochven home gate requires standing >= 6.0");
    assert_eq!(home_gate_err.code, "SystemCheck_JumpFailed_StandingsTooLow");

    // -------------------------------------------------------------------------
    // 3. Pochven Capital & Cyno Restrictions (feat:navigation.pochven-capital-cyno-restrictions)
    // -------------------------------------------------------------------------
    assert!(active_cyno_jammer(&ctx, otela_sys).is_some(), "Region-wide cyno jammer active in Pochven");

    let (cyno_char_id, cyno_ship_id, cyno_mod_id, ozone_id) =
        (90_000_002_i64, 630_000_020_i64, 630_000_021_i64, 630_000_022_i64);
    let mut cyno_sess = SessionState::new(884_102);
    common::login_pilot(&ctx, &mut cyno_sess, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut cyno_sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(cyno_char_id)], &kw, None)
        .await
        .expect("SelectCharacterID cyno pilot");

    let nearby_lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.region_id != 10_000_070
                && s.solar_system_id < 31_000_000
                && s.security_status > 0.0
                && s.security_status < 0.45
                && distance_ly(&ctx, otela_sys, s.solar_system_id) < 3.5
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_005_005);

    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.get(&char_id).unwrap().clone();
        snap.characters.entry(cyno_char_id).or_insert_with(|| {
            let mut c = tmpl;
            c.character_id = cyno_char_id;
            c.user_id = 1002;
            c
        });
        snap.fleets.clear();
        snap.fleets.insert(88_701, eve_db::FleetRecord { fleet_id: 88_701, boss_id: char_id, members: vec![char_id, cyno_char_id], ..Default::default() });
        snap.items.insert(cyno_mod_id, make_item(cyno_mod_id, 21_096, cyno_char_id, cyno_ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ozone_id, make_item(ozone_id, 16_273, cyno_char_id, cyno_ship_id, InventoryFlag::Cargo, 5_000, false));
        snap.module_online_states.insert(cyno_mod_id, true);
    });
    sess.fleet_id = Some(88_701);
    cyno_sess.fleet_id = Some(88_701);

    place_ship_in_space(&ctx, &mut cyno_sess, cyno_char_id, otela_sys, cyno_ship_id, 11_957, DVec3::new(20_000_000.0, 0.0, 0.0), 12_000_000.0).await;
    let cyno_poch_err = ctx
        .dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod_id), PyRep::String("cynosuralGeneration".into())], &kw, None)
        .await
        .expect_err("Lighting cyno in Pochven must fail");
    assert_eq!(cyno_poch_err.code, "CynoJammedInPochven");

    let jump_in_err = plan_jump_drive(&ctx, char_id, 19_720, nearby_lowsec_sys, otela_sys)
        .expect_err("Jump drive into Pochven must fail");
    assert_eq!(jump_in_err.code, "CannotJumpToPochven");
    place_ship_in_space(&ctx, &mut sess, char_id, nearby_lowsec_sys, ship_id, 19_720, DVec3::new(10_000_000.0, 0.0, 0.0), 1_200_000_000.0).await;
    let bey_lowsec = ctx.bind_object(&mut sess, "beyonce", nearby_lowsec_sys as i64).unwrap();
    let rpc_jump_in_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(bey_lowsec), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char_id), PyRep::Integer(otela_sys as i64), PyRep::Integer(999_999)], &kw, None)
        .await
        .expect_err("CmdJumpToFleetModuleBeacon into Pochven must fail");
    assert_eq!(rpc_jump_in_err.code, "CannotJumpToPochven");

    ctx.advance_secs(15);
    place_ship_in_space(&ctx, &mut cyno_sess, cyno_char_id, nearby_lowsec_sys, cyno_ship_id, 11_957, DVec3::new(30_000_000.0, 0.0, 0.0), 12_000_000.0).await;
    let cyno_lit = ctx
        .dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod_id), PyRep::String("cynosuralGeneration".into())], &kw, None)
        .await
        .expect("Force Recon lights cyno in low-sec");
    let lit_beacon_id = cyno_lit.dict_get("beaconID").and_then(|v| v.as_i64()).expect("beaconID from cyno activation");

    let dread_ship_id = 630_000_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        for sid in [3456_u32, 21_611] {
            skills.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(630_000_030, make_item(630_000_030, 16_274, char_id, dread_ship_id, InventoryFlag::SpecializedFuelBay, 25_000, false));
    });
    place_ship_in_space(&ctx, &mut sess, char_id, otela_sys, dread_ship_id, 19_720, gate_518_pos + DVec3::new(100.0, 0.0, 0.0), 1_200_000_000.0).await;
    let bey_otela = ctx.bind_object(&mut sess, "beyonce", otela_sys as i64).unwrap();

    let cap_gate_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(bey_otela), "CmdStargateJump", &[PyRep::Integer(50_016_518)], &kw, None)
        .await
        .expect_err("Capital ship cannot use Pochven stargate");
    assert_eq!(cap_gate_err.code, "CapitalShipCannotJumpStargate");

    let jump_out_rep = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(bey_otela), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char_id), PyRep::Integer(nearby_lowsec_sys as i64), PyRep::Integer(lit_beacon_id)], &kw, None)
        .await
        .expect("Capital inside Pochven can jump out to external cyno");
    assert_eq!(jump_out_rep.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(nearby_lowsec_sys as i64));
    assert_eq!(sess.solar_system_id, Some(nearby_lowsec_sys));

    let poch_station_id = ctx
        .sde
        .stations
        .values()
        .find(|st| ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|sys| sys.region_id == 10_000_070))
        .map(|st| st.station_id)
        .unwrap_or(60_015_155);
    let poch_st_sys = ctx.sde.stations.get(&poch_station_id).map(|st| st.solar_system_id).unwrap_or(otela_sys);
    sess.station_id = Some(poch_station_id);
    sess.solar_system_id = Some(poch_st_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(poch_station_id);
            ch.solar_system_id = poch_st_sys;
        }
    });
    let cap_mfg_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([("blueprintTypeID", PyRep::Integer(19_721)), ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1))])],
            &kw,
            None,
        )
        .await
        .expect_err("Capital manufacturing in Pochven must be refused");
    assert_eq!(cap_mfg_err.code, "CapitalConstructionForbiddenInPochven");
}
