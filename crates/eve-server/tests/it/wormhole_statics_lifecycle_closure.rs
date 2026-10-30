//! End-to-end V24.01 integration tests closing Wormhole Statics, Lifetime Decay Stages,
//! Static Wormhole Respawn on Mass/Time Exhaustion, and Destination K162 Exit Materialization.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::space_destiny::{
    ensure_system_signatures_and_anomalies, wormhole_exit_item_id_for_signature,
    wormhole_item_id_for_signature,
};
use eve_server::services::wormhole_lifecycle_ops::{
    is_k162_materialized_in_snap, statics_for_system_sde, system_wormhole_profile_sde,
    wspace_static_specs_for_system,
};
use glam::DVec3;
use std::collections::{BTreeSet, HashMap};
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

#[allow(clippy::too_many_arguments)]
async fn setup_pilot_in_space(
    ctx: &ServerContext,
    session_id: u64,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
    mass_kg: f64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
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
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
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
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: mass_kg,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
        )
        .await;
    sess
}

// parity: feat:navigation.wormhole-statics, feat:wormholes.lifetime-decay-stages, feat:wormholes.static-wormhole-respawn, feat:navigation.wormhole-k162-exit-materialization
#[tokio::test]
async fn test_wormhole_statics_decay_stages_respawn_and_k162_materialization() {
    eve_testkit::attest(&[
        "feat:navigation.wormhole-statics",
        "feat:wormholes.lifetime-decay-stages",
        "feat:wormholes.static-wormhole-respawn",
        "feat:navigation.wormhole-k162-exit-materialization",
    ]);
    let ctx = ctx_with_real_sde();
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let ship_id = 610_000_001_i64;

    // -------------------------------------------------------------------------
    // 1. Statics per W-space class (including C2 dual K+W statics & C4 dual W statics),
    //    constellation-varied SDE wormhole types (not single min_by_key), and
    //    probabilistic subset spawning for wandering/Thera/Pochven wormholes
    // -------------------------------------------------------------------------
    let class_samples = [
        (31_000_010_u32, 1u8, 1usize),
        (31_000_400_u32, 2u8, 2usize), // C2 has 2 statics: 1 W-space + 1 K-space
        (31_000_900_u32, 3u8, 1usize),
        (31_001_500_u32, 4u8, 2usize), // C4 has 2 distinct W-space statics
        (31_002_000_u32, 5u8, 1usize),
        (31_002_450_u32, 6u8, 1usize),
    ];
    for (sys_id, expected_cls, expected_static_cnt) in class_samples {
        let (wh_c, _, _, _) = system_wormhole_profile_sde(&ctx.sde, sys_id);
        assert_eq!(wh_c, expected_cls, "System {sys_id} must resolve to C{expected_cls}");
        let statics = statics_for_system_sde(&ctx.sde, sys_id);
        let static_specs = wspace_static_specs_for_system(&ctx.sde, sys_id);
        assert_eq!(
            statics.len(),
            expected_static_cnt,
            "Standard C{expected_cls} system {sys_id} must have {expected_static_cnt} static(s)"
        );
        for &(_, _, tid, _) in &static_specs {
            assert_ne!(tid, 26_272, "Static wormhole type must never be K162");
            assert!(ctx.sde.wormhole_types.contains_key(&tid), "Static {tid} must exist in SDE");
        }
        let _ = ensure_system_signatures_and_anomalies(&ctx, sys_id).await;
        let sigs = ctx
            .db
            .with_snapshot(|s| s.cosmic_signatures.get(&sys_id).cloned().unwrap_or_default());
        assert!(sigs.iter().any(|s| s.signature_id.starts_with("WST-")));
        if expected_static_cnt == 2 {
            assert!(
                sigs.iter().any(|s| s.signature_id.starts_with("WST2-")),
                "Dual-static C{expected_cls} system {sys_id} must seed both WST- and WST2-"
            );
        }
    }

    // Verify C2 has 1 W-space + 1 K-space static, and C4 has 2 distinct W-space target classes
    let c2_sigs = ctx
        .db
        .with_snapshot(|s| s.cosmic_signatures.get(&31_000_400).cloned().unwrap_or_default());
    let c2_wst_dest = c2_sigs.iter().find(|s| s.signature_id.starts_with("WST-")).unwrap().destination_system_id.unwrap();
    let c2_wst2_dest = c2_sigs.iter().find(|s| s.signature_id.starts_with("WST2-")).unwrap().destination_system_id.unwrap();
    assert!(c2_wst_dest >= 31_000_000 && c2_wst2_dest < 31_000_000, "C2 dual statics must connect to 1 W-space + 1 K-space system");

    let c4_sigs = ctx
        .db
        .with_snapshot(|s| s.cosmic_signatures.get(&31_001_500).cloned().unwrap_or_default());
    let c4_d1 = c4_sigs.iter().find(|s| s.signature_id.starts_with("WST-")).unwrap().destination_system_id.unwrap();
    let c4_d2 = c4_sigs.iter().find(|s| s.signature_id.starts_with("WST2-")).unwrap().destination_system_id.unwrap();
    let c4_cls1 = system_wormhole_profile_sde(&ctx.sde, c4_d1).0;
    let c4_cls2 = system_wormhole_profile_sde(&ctx.sde, c4_d2).0;
    assert!(c4_cls1 >= 1 && c4_cls2 >= 1 && c4_cls1 != c4_cls2, "C4 dual statics must target 2 distinct W-space classes");

    // Verify constellation variety across C2 systems (not collapsed to a single min_by_key type_id)
    let mut distinct_c2_static_types = BTreeSet::new();
    for sid in (31_000_380_u32..31_000_620_u32).step_by(17) {
        if system_wormhole_profile_sde(&ctx.sde, sid).0 == 2 {
            for (_, _, tid, _) in wspace_static_specs_for_system(&ctx.sde, sid) {
                distinct_c2_static_types.insert(tid);
            }
        }
    }
    assert!(
        distinct_c2_static_types.len() >= 4,
        "C2 constellations must select across multiple SDE static wormhole types, got {distinct_c2_static_types:?}"
    );

    // Verify wandering, Thera, and Pochven wormholes spawn probabilistically on a subset (not 100% of systems)
    let (mut wnd_cnt, mut wth_cnt, mut wpc_cnt) = (0usize, 0usize, 0usize);
    let sample_systems: Vec<u32> = (31_000_401_u32..=31_000_424_u32).collect();
    for &sid in &sample_systems {
        let _ = ensure_system_signatures_and_anomalies(&ctx, sid).await;
        let sigs = ctx
            .db
            .with_snapshot(|s| s.cosmic_signatures.get(&sid).cloned().unwrap_or_default());
        if sigs.iter().any(|s| s.signature_id.starts_with("WND-")) {
            wnd_cnt += 1;
        }
        if sigs.iter().any(|s| s.signature_id.starts_with("WTH-")) {
            wth_cnt += 1;
        }
        if sigs.iter().any(|s| s.signature_id.starts_with("WPC-")) {
            wpc_cnt += 1;
        }
    }
    assert!(wnd_cnt > 0 && wnd_cnt < sample_systems.len(), "Wandering WHs must spawn on a subset, got {wnd_cnt}/{}", sample_systems.len());
    assert!(wth_cnt > 0 && wth_cnt < sample_systems.len(), "Thera WHs must spawn on a subset, got {wth_cnt}/{}", sample_systems.len());
    assert!(wpc_cnt > 0 && wpc_cnt < sample_systems.len(), "Pochven WHs must spawn on a subset, got {wpc_cnt}/{}", sample_systems.len());

    // -------------------------------------------------------------------------
    // 2. Destination-side K162 signature does NOT exist before first jump through,
    //    and materializes + becomes probeable in destination system after first jump
    // -------------------------------------------------------------------------
    let origin_sys = 31_001_500_u32; // C4
    let origin_sigs = ctx
        .db
        .with_snapshot(|s| s.cosmic_signatures.get(&origin_sys).cloned().unwrap_or_default());
    let static_wh = origin_sigs
        .iter()
        .find(|s| s.signature_id.starts_with("WST-"))
        .expect("static wormhole in C4")
        .clone();
    let dest_sys = static_wh.destination_system_id.expect("static destination");
    let orig_sig_id = static_wh.signature_id.clone();
    let k162_sig_id = format!("K162-{orig_sig_id}");
    let entrance_wh_id = wormhole_item_id_for_signature(origin_sys, &orig_sig_id);
    let exit_wh_id = wormhole_exit_item_id_for_signature(dest_sys, &orig_sig_id);

    let pre_jump_k162 = ctx
        .db
        .with_snapshot(|snap| is_k162_materialized_in_snap(snap, dest_sys, &orig_sig_id));
    assert!(!pre_jump_k162, "K162 signature must NOT exist before first ship jumps through");
    let pre_jump_exit_ball = ctx
        .systems
        .get_or_spawn(dest_sys)
        .get_ball_snapshot(exit_wh_id)
        .await
        .expect("ball query");
    assert!(pre_jump_exit_ball.is_none(), "Destination exit ball must NOT exist before first jump");

    let wh_pos = DVec3::from_array(static_wh.position);
    let mut sess = setup_pilot_in_space(
        &ctx,
        920_001,
        char_id,
        origin_sys,
        ship_id,
        621, // Caracal cruiser (~11.3M kg)
        wh_pos + DVec3::new(500.0, 0.0, 0.0),
        11_300_000.0,
    )
    .await;
    ctx.advance_secs(15);
    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(entrance_wh_id)],
            &kw,
            None,
        )
        .await
        .expect("first ship jumps through wormhole");
    assert_eq!(
        jump_rep.dict_get("destinationSystemID").and_then(|v| v.as_i64()),
        Some(dest_sys as i64)
    );
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    let post_jump_k162 = ctx
        .db
        .with_snapshot(|snap| is_k162_materialized_in_snap(snap, dest_sys, &orig_sig_id));
    assert!(post_jump_k162, "K162 signature MUST materialize in destination after first jump");
    let k162_rec = ctx
        .db
        .with_snapshot(|snap| {
            snap.cosmic_signatures
                .get(&dest_sys)
                .and_then(|l| l.iter().find(|s| s.signature_id == k162_sig_id))
                .cloned()
        })
        .expect("K162 signature record in dest_sys");
    assert_eq!(k162_rec.wormhole_type_id, Some(26_272));
    assert_eq!(k162_rec.destination_system_id, Some(origin_sys));

    let post_jump_exit_ball = ctx
        .systems
        .get_or_spawn(dest_sys)
        .get_ball_snapshot(exit_wh_id)
        .await
        .expect("exit ball query");
    assert!(post_jump_exit_ball.is_some(), "Destination exit ball must exist after first jump");

    ctx.advance_secs(15);
    let scan_rows = common::probe_scan(
        &ctx,
        &mut sess,
        DVec3::from_array(k162_rec.position),
        0.25 * 149_597_870_700.0,
    )
    .await;
    let k162_strength = common::scan_row_field(&scan_rows, &k162_sig_id, "signalStrength")
        .as_f64()
        .unwrap_or(0.0);
    assert!(
        k162_strength >= 1.0,
        "Materialized K162 signature must be probeable to 1.0 in destination system, got {k162_strength}"
    );

    // -------------------------------------------------------------------------
    // 3. Lifetime decay stages across 16h, 24h, 36h, 48h & canonical show-info strings
    //    (>24h, 4h-24h, <4h), plus SystemActor tick-driven expiration despawning both
    //    entrance & exit balls and respawning static to a newly rolled destination system
    // -------------------------------------------------------------------------
    let decay_sys = 31_002_000_u32; // C5
    let c5_static_spec = wspace_static_specs_for_system(&ctx.sde, decay_sys)[0].clone();
    let static_type_id = c5_static_spec.2;
    let decay_dest = 31_002_001_u32;
    let now0 = ctx.now_unix();
    let mut test_sigs = vec![CosmicSignatureRecord {
        signature_id: "WST-DEC-48H".into(),
        solar_system_id: decay_sys,
        sig_group: "Wormhole".into(),
        type_name: format!("Wormhole {}", c5_static_spec.1),
        position: [15_000_000_000.0, 0.0, 0.0],
        signal_strength: 1.0,
        wormhole_type_id: Some(static_type_id),
        destination_system_id: Some(decay_dest),
        created_unix: now0,
        lifetime_seconds: 48 * 3_600,
        max_mass_kg: 3_000_000_000.0,
        remaining_mass_kg: 3_000_000_000.0,
        max_jump_mass_kg: 1_800_000_000.0,
        collapsed: false,
    }];
    for (idx, hours) in [16_u64, 24, 36].into_iter().enumerate() {
        test_sigs.push(CosmicSignatureRecord {
            signature_id: format!("WND-DEC-{hours}H"),
            solar_system_id: decay_sys,
            sig_group: "Wormhole".into(),
            type_name: "Wormhole H296".into(),
            position: [15_005_000_000.0 + (idx as f64) * 5_000_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(static_type_id),
            destination_system_id: Some(decay_dest),
            created_unix: now0,
            lifetime_seconds: hours * 3_600,
            max_mass_kg: 3_000_000_000.0,
            remaining_mass_kg: 3_000_000_000.0,
            max_jump_mass_kg: 1_800_000_000.0,
            collapsed: false,
        });
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(decay_sys, test_sigs);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = decay_sys;
        }
    });
    sess.solar_system_id = Some(decay_sys);

    let wh48_ent = wormhole_item_id_for_signature(decay_sys, "WST-DEC-48H");
    let wh48_ext = wormhole_exit_item_id_for_signature(decay_dest, "WST-DEC-48H");
    let wh16_ent = wormhole_item_id_for_signature(decay_sys, "WND-DEC-16H");
    assert!(wh48_ent >= 40_000_000 && wh16_ent >= 40_000_000);
    for (bsys, bid) in [(decay_sys, wh48_ent), (decay_sys, wh16_ent), (decay_dest, wh48_ext)] {
        let _ = ctx
            .systems
            .get_or_spawn(bsys)
            .spawn_ball(Ball {
                id: bid,
                type_id: 26272,
                position: DVec3::new(10_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 3_000_000_000.0,
                radius: 1500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }

    // At t = +1h: step SystemActor tick and verify SlimItem (wormholeAge/wormholeSize) + GetWormholePolarization (None when unpolarized)
    ctx.advance_secs(3_600);
    let _ = ctx.systems.get_or_spawn(decay_sys).step_manual(1.0).await;
    let pol_unpol = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "GetWormholePolarization", &[PyRep::Integer(wh48_ent)], &kw, None)
        .await
        .expect("GetWormholePolarization unpolarized");
    assert_eq!(pol_unpol, PyRep::None);

    let slim_48_early = ctx.systems.get_or_spawn(decay_sys).get_ball_slim_item(wh48_ent).await.unwrap().expect("slim 48h");
    let slim_16_early = ctx.systems.get_or_spawn(decay_sys).get_ball_slim_item(wh16_ent).await.unwrap().expect("slim 16h");
    assert_eq!(slim_48_early.dict_get("wormholeAge"), Some(&PyRep::Integer(0)));
    assert_eq!(slim_48_early.dict_get("wormholeSize"), Some(&PyRep::Real(1.0)));
    assert_eq!(slim_16_early.dict_get("wormholeAge"), Some(&PyRep::Integer(1)));

    ctx.db.with_mut_snapshot(|snap| {
        for s in snap.cosmic_signatures.get_mut(&decay_sys).into_iter().flatten() {
            s.signal_strength = 1.0;
        }
    });
    let scan_early = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(decay_sys as i64)], &kw, None)
        .await
        .expect("GetSystemScanMgr early");
    let PyRep::List(early_sigs) = scan_early.dict_get("signatures").unwrap() else { panic!() };
    let find_sig_desc = |sigs: &[PyRep], sid: &str| -> Option<String> {
        sigs.iter()
            .find(|s| s.dict_get("signatureID") == Some(&PyRep::String(sid.into())))
            .and_then(|s| s.dict_get("lifetimeDescription"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    assert_eq!(
        find_sig_desc(early_sigs, "WST-DEC-48H").as_deref(),
        Some("This wormhole is beginning to decay, and probably won't last another day")
    );
    assert_eq!(
        find_sig_desc(early_sigs, "WND-DEC-36H").as_deref(),
        Some("This wormhole is beginning to decay, and probably won't last another day")
    );
    assert_eq!(
        find_sig_desc(early_sigs, "WND-DEC-24H").as_deref(),
        Some("This wormhole is not yet beginning to decay")
    );
    assert_eq!(
        find_sig_desc(early_sigs, "WND-DEC-16H").as_deref(),
        Some("This wormhole is not yet beginning to decay")
    );

    // Advance +13h (total +14h): 16h wormhole has 2h remaining (<4h -> reaching end of natural lifetime, wormholeAge = 2)
    // and both 16h (2h remaining) and 24h (10h remaining) are now <15h remaining lifetime, so their
    // destination K162 signatures & exit balls materialize on the SystemActor tick without any ship transit,
    // while 36h (22h remaining) and 48h (34h remaining) remain unmaterialized (>= 15h).
    ctx.advance_secs(13 * 3_600);
    let _ = ctx.systems.get_or_spawn(decay_sys).step_manual(1.0).await;
    let slim_16_eol = ctx.systems.get_or_spawn(decay_sys).get_ball_slim_item(wh16_ent).await.unwrap().expect("slim 16h eol");
    assert_eq!(slim_16_eol.dict_get("wormholeAge"), Some(&PyRep::Integer(2)));
    ctx.db.with_snapshot(|snap| {
        assert!(
            is_k162_materialized_in_snap(snap, decay_dest, "WND-DEC-16H"),
            "16h wormhole at 2h remaining (<15h) must materialize K162 without ship transit"
        );
        assert!(
            is_k162_materialized_in_snap(snap, decay_dest, "WND-DEC-24H"),
            "24h wormhole at 10h remaining (<15h) must materialize K162 without ship transit"
        );
        assert!(
            !is_k162_materialized_in_snap(snap, decay_dest, "WND-DEC-36H"),
            "36h wormhole at 22h remaining (>=15h) must NOT materialize K162 yet"
        );
        assert!(
            !is_k162_materialized_in_snap(snap, decay_dest, "WST-DEC-48H"),
            "48h wormhole at 34h remaining (>=15h) must NOT materialize K162 yet"
        );
    });
    let k16_exit_id = wormhole_exit_item_id_for_signature(decay_dest, "WND-DEC-16H");
    assert!(
        ctx.systems.get_or_spawn(decay_dest).get_ball_snapshot(k16_exit_id).await.unwrap().is_some(),
        "Destination exit wormhole ball must spawn when remaining lifetime drops below 15h"
    );
    let scan_eol = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(decay_sys as i64)], &kw, None)
        .await
        .expect("GetSystemScanMgr eol");
    let PyRep::List(eol_sigs) = scan_eol.dict_get("signatures").unwrap() else { panic!() };
    assert_eq!(
        find_sig_desc(eol_sigs, "WND-DEC-16H").as_deref(),
        Some("This wormhole is reaching the end of its natural lifetime")
    );

    // Advance +35h (total +49h): step ONLY the SystemActor periodic tick (no client scan/jump RPC!)
    ctx.advance_secs(35 * 3_600);
    let _ = ctx.systems.get_or_spawn(decay_sys).step_manual(1.0).await;

    let ent_after_exp = ctx.systems.get_or_spawn(decay_sys).get_ball_snapshot(wh48_ent).await.expect("ent ball check");
    let ext_after_exp = ctx.systems.get_or_spawn(decay_dest).get_ball_snapshot(wh48_ext).await.expect("ext ball check");
    assert!(ent_after_exp.is_none(), "Entrance ball must despawn automatically on SystemActor tick");
    assert!(ext_after_exp.is_none(), "Exit ball must despawn automatically on SystemActor tick");

    // Static replacement must spawn with newly rolled destination (!= decay_dest); wandering WND- stays closed
    let post_exp_sigs = ctx
        .db
        .with_snapshot(|s| s.cosmic_signatures.get(&decay_sys).cloned().unwrap_or_default());
    assert!(
        post_exp_sigs.iter().filter(|s| s.signature_id.starts_with("WND-")).all(|s| s.collapsed),
        "Expired wandering wormholes must remain permanently closed without respawn"
    );
    let active_replacements: Vec<_> = post_exp_sigs
        .iter()
        .filter(|s| !s.collapsed && s.signature_id.starts_with("WST-"))
        .collect();
    assert_eq!(active_replacements.len(), 1, "Expired static wormhole must respawn exactly 1 replacement");
    assert_eq!(active_replacements[0].wormhole_type_id, Some(static_type_id));
    let rolled_exp_dest = active_replacements[0].destination_system_id.expect("rolled destination");
    assert_ne!(rolled_exp_dest, decay_dest, "Respawned static wormhole must roll a new destination solar system");

    // -------------------------------------------------------------------------
    // 4. Static wormhole mass exhaustion collapse immediately respawns identical static
    //    wormhole type connected to a newly rolled destination system of target class
    // -------------------------------------------------------------------------
    let mass_sys = 31_000_900_u32; // C3
    let _ = ensure_system_signatures_and_anomalies(&ctx, mass_sys).await;
    let initial_c3_static = ctx
        .db
        .with_snapshot(|s| {
            s.cosmic_signatures
                .get(&mass_sys)
                .and_then(|l| l.iter().find(|w| w.signature_id.starts_with("WST-") && !w.collapsed))
                .cloned()
        })
        .expect("initial C3 static");
    let c3_static_tid = initial_c3_static.wormhole_type_id.expect("C3 static type_id");
    let c3_static_sig = initial_c3_static.signature_id.clone();
    let old_c3_dest = initial_c3_static.destination_system_id.expect("initial C3 dest");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(list) = snap.cosmic_signatures.get_mut(&mass_sys) {
            if let Some(w) = list.iter_mut().find(|w| !w.collapsed && w.signature_id == c3_static_sig) {
                w.created_unix = ctx.now_unix();
                w.remaining_mass_kg = 11_300_000.0;
            }
        }
    });
    let mass_wh_pos = DVec3::from_array(initial_c3_static.position);
    let _ = ctx.systems.get_or_spawn(dest_sys).remove_ball(ship_id).await;
    ctx.advance_secs(15);
    let _ = ctx.db.set_character_solar_system(char_id, mass_sys, &ctx.sde);
    sess.solar_system_id = Some(mass_sys);
    sess.station_id = None;
    sess.structure_id = None;
    let mass_handle = ctx.systems.get_or_spawn(mass_sys);
    mass_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 621,
                position: mass_wh_pos + DVec3::new(300.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 11_300_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 621),
        )
        .await
        .expect("spawn_ball_with_combat in mass_sys");
    let c3_ent_id = wormhole_item_id_for_signature(mass_sys, &c3_static_sig);
    let mass_jump_rep = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(c3_ent_id)], &kw, None)
        .await
        .expect("ship jumping through critical-mass static collapses it");
    assert_eq!(mass_jump_rep.dict_get("collapsed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        mass_jump_rep.dict_get("replacementWormholeTypeID").and_then(|v| v.as_i64()).map(|v| v as u32),
        Some(c3_static_tid)
    );
    let rep_c3_static = ctx
        .db
        .with_snapshot(|s| {
            s.cosmic_signatures
                .get(&mass_sys)
                .and_then(|l| l.iter().find(|w| w.signature_id.starts_with("WST-") && !w.collapsed && w.signature_id != c3_static_sig))
                .cloned()
        })
        .expect("respawned C3 static signature");
    let new_c3_dest = rep_c3_static.destination_system_id.expect("new C3 dest");
    assert_ne!(new_c3_dest, old_c3_dest, "Mass-collapsed static respawn must roll a new destination solar system");
    assert!(new_c3_dest < 31_000_000, "Respawned C3 K-space static must target a K-space system");
    assert!(
        !ctx.db.with_snapshot(|snap| is_k162_materialized_in_snap(snap, new_c3_dest, &rep_c3_static.signature_id)),
        "Respawned static's exit K162 must start unmaterialized"
    );
}

// parity: feat:navigation.wormhole-k162-exit-materialization
#[tokio::test]
async fn test_k162_materializes_below_15h_remaining_lifetime_without_transit() {
    eve_testkit::attest(&["feat:navigation.wormhole-k162-exit-materialization"]);
    let ctx = ctx_with_real_sde();
    let origin_sys = 31_001_510_u32;
    let dest_sys = 31_001_511_u32;
    let now0 = ctx.now_unix();
    let orig_sig_id = "WST-K162-15H";
    let k162_sig_id = format!("K162-{orig_sig_id}");
    let exit_wh_id = wormhole_exit_item_id_for_signature(dest_sys, orig_sig_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            origin_sys,
            vec![CosmicSignatureRecord {
                signature_id: orig_sig_id.into(),
                solar_system_id: origin_sys,
                sig_group: "Wormhole".into(),
                type_name: "Wormhole X877".into(),
                position: [12_000_000_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: Some(30_699),
                destination_system_id: Some(dest_sys),
                created_unix: now0,
                lifetime_seconds: 24 * 3_600,
                max_mass_kg: 2_000_000_000.0,
                remaining_mass_kg: 2_000_000_000.0,
                max_jump_mass_kg: 375_000_000.0,
                collapsed: false,
            }],
        );
    });

    // At +9h elapsed (exact 15h = 54,000 s remaining), K162 must remain unmaterialized prior to transit
    ctx.advance_secs(9 * 3_600);
    let _ = ctx.systems.get_or_spawn(origin_sys).step_manual(1.0).await;
    assert!(
        !ctx.db.with_snapshot(|snap| is_k162_materialized_in_snap(snap, dest_sys, orig_sig_id)),
        "K162 must NOT materialize at exact 15h (54,000 s) remaining lifetime"
    );
    assert!(
        ctx.systems.get_or_spawn(dest_sys).get_ball_snapshot(exit_wh_id).await.unwrap().is_none(),
        "Destination exit wormhole ball must NOT exist at >= 15h remaining lifetime"
    );

    // Advance 1s past 15h remaining (53,999 s < 54,000 s): sweep materializes K162 signature & exit ball
    ctx.advance_secs(1);
    let _ = ctx.systems.get_or_spawn(origin_sys).step_manual(1.0).await;
    let _ = ctx.systems.get_or_spawn(origin_sys).step_manual(1.0).await;

    let k162_recs: Vec<CosmicSignatureRecord> = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&dest_sys)
            .map(|l| l.iter().filter(|s| s.signature_id == k162_sig_id && !s.collapsed).cloned().collect())
            .unwrap_or_default()
    });
    assert_eq!(k162_recs.len(), 1, "K162 signature must materialize once when remaining < 15h without ship transit");
    let exit_ball = ctx.systems.get_or_spawn(dest_sys).get_ball_snapshot(exit_wh_id).await.unwrap().expect("Destination exit wormhole ball must materialize when remaining < 15h");
    assert_eq!(exit_ball.1.position, DVec3::from_array(k162_recs[0].position));

    // Verify materialized K162 signature is probeable in dest_sys without any ship having jumped through
    let mut sess = setup_pilot_in_space(&ctx, 920_002, 90_000_001, dest_sys, 610_000_002, 621, DVec3::from_array(k162_recs[0].position) + DVec3::new(10_000.0, 0.0, 0.0), 11_300_000.0).await;
    let scan_rows = common::probe_scan(&ctx, &mut sess, DVec3::from_array(k162_recs[0].position), 0.25 * 149_597_870_700.0).await;
    let strength = common::scan_row_field(&scan_rows, &k162_sig_id, "signalStrength").as_f64().unwrap_or(0.0);
    assert!(strength >= 1.0, "Auto-materialized K162 signature must be probeable to 1.0, got {strength}");

    // Negative test (R-073441-01): warping to a C2 system's Z647 (30671) static lands within normal warp-in range (NOT +80 km)
    let c2_sys = ctx.sde.solar_systems.values().find(|s| s.wormhole_class_id == Some(2)).unwrap().solar_system_id;
    let c2_wh_pos = DVec3::new(2.0 * 149_597_870_700.0, 0.0, 0.0);
    let c2_wh_id = wormhole_item_id_for_signature(c2_sys, "WST-C2Z");
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(c2_sys, vec![CosmicSignatureRecord {
            signature_id: "WST-C2Z".into(), solar_system_id: c2_sys, sig_group: "Wormhole".into(),
            type_name: "Wormhole Z647".into(), position: c2_wh_pos.to_array(), signal_strength: 1.0,
            wormhole_type_id: Some(30_671), destination_system_id: Some(31_000_007),
            created_unix: ctx.now_unix(), lifetime_seconds: 57_600, max_mass_kg: 1_000_000_000.0,
            remaining_mass_kg: 1_000_000_000.0, max_jump_mass_kg: 20_000_000.0, collapsed: false,
        }]);
    });
    let _ = ctx.systems.get_or_spawn(c2_sys).spawn_ball(Ball {
        id: c2_wh_id, type_id: 30_671, position: c2_wh_pos, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1_000_000_000.0, radius: 1500.0, speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await;
    let mut c2_sess = setup_pilot_in_space(&ctx, 920_003, 90_000_001, c2_sys, 610_000_003, 587, DVec3::ZERO, 1_200_000.0).await;
    ctx.dispatch_rpc(&mut c2_sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(c2_wh_id), PyRep::Real(0.0)], &HashMap::new(), None).await.expect("CmdWarpToStuff C2 Z647");
    let (_, ship_b) = ctx.systems.get_or_spawn(c2_sys).get_ball_snapshot(610_000_003).await.unwrap().unwrap();
    let MovementMode::Warp { destination, .. } = ship_b.mode else { panic!("Expected Warp mode") };
    assert!((destination - c2_wh_pos).length() < 10_000.0, "C2 Z647 static must warp within normal range, got offset {}", (destination - c2_wh_pos).length());
}
