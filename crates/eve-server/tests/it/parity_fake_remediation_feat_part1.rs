//! Integration tests remediating `FLAGGED_FAKE` `feat:*` parity rows.
//!
//! Covers:
//! - `feat:wormholes.lifetime-decay-stages`
//! - `feat:navigation.wormhole-info-buckets`
//! - `feat:pve.hacking-node-types`
//! - `feat:pve.hacking-failure-outcome`
//! - `feat:sovereignty.skyhook-reagent-maturity`

use eve_db::{CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg, HackingNodeKind, HackingUtilityKind,
};
use eve_server::services::pve_hacking_board::{
    build_hacking_grid_procedural, build_hacking_grid_procedural_with_sde,
    resolve_hacking_node_stats,
};
use eve_server::services::sov_gameplay::sov_gameplay_skyhook::advance_skyhook_reagents_in_snap;
use eve_server::services::space_destiny::{
    derive_wormhole_buckets, derive_wormhole_buckets_raw, wormhole_item_id_for_signature,
};
use std::collections::HashMap;

fn make_context() -> ServerContext {
    ServerContext::new()
}

fn make_session(char_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut sess = SessionState::new(990_001);
    sess.user_id = Some(99_001);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess
}

#[tokio::test]
async fn test_wormhole_lifetime_decay_stages_and_info_buckets() {
    // parity: feat:wormholes.lifetime-decay-stages, feat:navigation.wormhole-info-buckets
    eve_testkit::attest(&[
        "feat:wormholes.lifetime-decay-stages",
        "feat:navigation.wormhole-info-buckets",
    ]);

    let ctx = make_context();
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();
    let b041 = ctx.sde.wormhole_types.get(&30_645).expect("Wormhole B041 in SDE");
    let lifetime = (b041.max_stable_time_min * 60.0) as u64; // 2880 min = 48h

    // > 24h remaining (36h left): >1 day bucket, not yet begun to decay
    let fresh = derive_wormhole_buckets_raw(
        30_000_142,
        b041.max_jump_mass,
        b041.max_stable_mass,
        b041.max_stable_mass,
        now,
        lifetime,
        false,
        now + 12 * 3_600,
        &ctx.sde,
    );
    assert_eq!(fresh.lifetime_bucket, ">1 day");
    assert_eq!(fresh.max_ship_size, "Extra Large");
    assert_eq!(
        fresh.lifetime_description,
        "This wormhole has not yet begun to decay"
    );

    // 4h..=24h remaining (12h left): <1 day bucket, beginning to decay
    let decaying = derive_wormhole_buckets_raw(
        30_000_142,
        b041.max_jump_mass,
        b041.max_stable_mass,
        b041.max_stable_mass,
        now,
        lifetime,
        false,
        now + 36 * 3_600,
        &ctx.sde,
    );
    assert_eq!(decaying.lifetime_bucket, "<1 day");
    assert_eq!(
        decaying.lifetime_description,
        "This wormhole is beginning to decay, and probably won't last another day"
    );

    // < 4h remaining (2h left): <4h bucket, reaching end of natural lifetime
    let eol = derive_wormhole_buckets_raw(
        30_000_142,
        b041.max_jump_mass,
        b041.max_stable_mass,
        b041.max_stable_mass,
        now,
        lifetime,
        false,
        now + 46 * 3_600,
        &ctx.sde,
    );
    assert_eq!(eol.lifetime_bucket, "<4h");
    assert_eq!(
        eol.lifetime_description,
        "This wormhole is reaching the end of its natural lifetime"
    );

    // Verify SDE wormhole type attributes drive ship size & destination class buckets
    for (wtid, expected_size, expected_dest) in [
        (34_134_u32, "Small", "C1-3"),       // Wormhole E004 (5M kg -> C1)
        (30_579_u32, "Medium", "C1-3"),      // Wormhole Z971 (62M kg -> C1)
        (30_677_u32, "Large", "High-Sec"),   // Wormhole B274 (375M kg -> High-Sec)
        (56_026_u32, "Large", "Null-Sec"),   // Wormhole C729 (410M kg)
        (30_645_u32, "Extra Large", "C6"),   // Wormhole B041 (1B kg -> C6)
    ] {
        let sig = CosmicSignatureRecord {
            signature_id: format!("SDE-{wtid}"),
            solar_system_id: 30_000_142,
            sig_group: "Wormhole".into(),
            type_name: ctx.sde.wormhole_types[&wtid].name.clone(),
            position: [1_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(wtid),
            destination_system_id: None,
            created_unix: now,
            lifetime_seconds: 0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 500_000_000.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        };
        let b = derive_wormhole_buckets(&sig, now + 60, &ctx.sde);
        assert_eq!(b.max_ship_size, expected_size, "wtid {wtid}");
        assert_eq!(b.destination_class, expected_dest, "wtid {wtid}");
    }

    // Verify live MachoNet scanMgr.GetSystemScanMgr & wormholeMgr.WormholeJump RPCs
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_099_i64;
    let mut sess = make_session(char_id, sys_id, ship_id);
    let kw: HashMap<String, PyRep> = HashMap::new();
    let b274 = &ctx.sde.wormhole_types[&30_677];
    let wh_sig_id = "WH-SDE-LIVE".to_string();
    let wh_item_id = wormhole_item_id_for_signature(sys_id, &wh_sig_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 605, // Heron
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.cosmic_signatures.insert(
            sys_id,
            vec![CosmicSignatureRecord {
                signature_id: wh_sig_id.clone(),
                solar_system_id: sys_id,
                sig_group: "Wormhole".into(),
                type_name: b274.name.clone(),
                position: [500.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: Some(30_677),
                destination_system_id: Some(30_002_187),
                created_unix: now.saturating_sub(22 * 3_600), // 2h left of 24h -> <4h
                lifetime_seconds: (b274.max_stable_time_min * 60.0) as u64,
                max_mass_kg: b274.max_stable_mass,
                remaining_mass_kg: b274.max_stable_mass * 0.25, // <50%
                max_jump_mass_kg: b274.max_jump_mass,
                collapsed: false,
            }],
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 605,
            position: glam::DVec3::new(500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: wh_item_id,
            type_id: 26_272,
            position: glam::DVec3::new(500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: b274.max_stable_mass,
            radius: 1_500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Rigid,
        })
        .await;

    let scan_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetSystemScanMgr",
            &[PyRep::Integer(i64::from(sys_id))],
            &kw,
            None,
        )
        .await
        .expect("scanMgr.GetSystemScanMgr");
    let sig_kv = scan_rep
        .dict_get("signatures")
        .and_then(|s| match s {
            PyRep::List(l) => l
                .iter()
                .find(|item| item.dict_get("signatureID") == Some(&PyRep::String(wh_sig_id.clone()))),
            _ => None,
        })
        .expect("WH-SDE-LIVE in GetSystemScanMgr");
    assert_eq!(
        sig_kv.dict_get("maxShipSize"),
        Some(&PyRep::String("Large".into()))
    );
    assert_eq!(
        sig_kv.dict_get("lifetimeBucket"),
        Some(&PyRep::String("<4h".into()))
    );
    assert_eq!(
        sig_kv.dict_get("massStabilityBucket"),
        Some(&PyRep::String("<50%".into()))
    );

    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect("wormholeMgr.WormholeJump");
    assert_eq!(
        jump_rep.dict_get("lifetimeBucket"),
        Some(&PyRep::String("<4h".into()))
    );
    assert_eq!(
        jump_rep.dict_get("lifetimeDescription"),
        Some(&PyRep::String(
            "This wormhole is reaching the end of its natural lifetime".into()
        ))
    );
}

#[tokio::test]
async fn test_hacking_node_types_and_failure_outcome() {
    // parity: feat:pve.hacking-node-types, feat:pve.hacking-failure-outcome
    eve_testkit::attest(&[
        "feat:pve.hacking-node-types",
        "feat:pve.hacking-failure-outcome",
    ]);

    // 1. Difficulty 1 (Very Easy) and Difficulty 2 (Easy) must not spawn RestorationNode or VirusSuppressor
    for diff in [1_u8, 2_u8] {
        let tiles = build_hacking_grid_procedural(77_001, false, diff, 1_750_000_000);
        assert!(
            !tiles.iter().any(|t| matches!(
                t.kind,
                HackingNodeKind::RestorationNode | HackingNodeKind::VirusSuppressor
            )),
            "Difficulty {diff} board must not spawn RestorationNode or VirusSuppressor"
        );
    }
    let diff3_tiles = build_hacking_grid_procedural(77_003, false, 3, 1_750_000_000);
    assert!(
        diff3_tiles
            .iter()
            .any(|t| t.kind == HackingNodeKind::RestorationNode),
        "Difficulty 3 board must include RestorationNode"
    );

    // 2. SelfRepair heals 5..=10 coherence per turn for 3 turns without a max_virus_coherence cap,
    //    and 2nd hack failure on a standard Data/Relic container self-destructs with 0.0 ship damage.
    let ctx = make_context();
    let (tier1, stats1) = resolve_hacking_node_stats(Some(&ctx.sde), Some(21_683), false, 0);
    let (tier3, stats3) = resolve_hacking_node_stats(Some(&ctx.sde), Some(21_685), false, 0);
    assert_eq!(tier1, 1);
    assert_eq!(tier3, 3);
    assert!(stats3.core_coh > stats1.core_coh && stats3.fw_coh > stats1.fw_coh);
    let sde_tiles =
        build_hacking_grid_procedural_with_sde(Some(&ctx.sde), 77_004, Some(21_685), false, 0, 1_750_000_000);
    assert!(sde_tiles
        .iter()
        .any(|t| t.kind == HackingNodeKind::RestorationNode));
    let kw: HashMap<String, PyRep> = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_002_813_u32; // Low-sec
    let ship_id = 100_000_001_i64;
    let analyzer_id = 100_000_002_i64;
    let mut sess = make_session(char_id, sys_id, ship_id);

    ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    let (can_id, can_fail, site_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let relic = reg
            .sites_by_system
            .get(&sys_id)
            .and_then(|list| {
                list.iter().find(|s| {
                    s.site_category.eq_ignore_ascii_case("Relic") && s.hackable_can_ids.len() >= 2
                })
            })
            .expect("Seeded standard Relic site with >= 2 cans");
        (relic.hackable_can_ids[0], relic.hackable_can_ids[1], relic.position)
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 605, // Heron
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            analyzer_id,
            InventoryItemRecord {
                item_id: analyzer_id,
                type_id: 22_177, // Relic Analyzer I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(analyzer_id, true);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 605,
            position: glam::DVec3::from_array(site_pos),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "StartNewGameInstance",
            &[PyRep::Integer(can_id)],
            &kw,
            None,
        )
        .await
        .expect("StartNewGameInstance");

    with_pve_exploration_reg(&ctx, |reg| {
        let g = reg.hacking_games.get_mut(&(char_id, can_id)).unwrap();
        g.virus_coherence = (g.max_virus_coherence - 5).max(1);
        g.utilities[0] = Some(HackingUtilityKind::SelfRepair);
        for t in g.tiles.iter_mut().take(4) {
            t.kind = HackingNodeKind::Empty;
            t.coherence = 0;
            t.strength = 0;
        }
    });

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "hackingMgr",
            None,
            "UsedUtilityElement",
            &[PyRep::Integer(can_id), PyRep::Integer(0), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("Activate SelfRepair");

    let (base_coh, max_coh) = with_pve_exploration_reg(&ctx, |reg| {
        let g = &reg.hacking_games[&(char_id, can_id)];
        (g.virus_coherence, g.max_virus_coherence)
    });
    let mut prev_coh = base_coh;
    for turn in 1..=3_u32 {
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "ClickedOnTile",
                &[PyRep::Integer(can_id), PyRep::Integer(i64::from(turn))],
                &kw,
                None,
            )
            .await
            .expect("Click tile");
        let cur_coh = with_pve_exploration_reg(&ctx, |reg| {
            reg.hacking_games[&(char_id, can_id)].virus_coherence
        });
        let delta = cur_coh - prev_coh;
        assert!(
            (5..=10).contains(&delta),
            "SelfRepair turn {turn} must heal 5..=10 coherence, got {delta}"
        );
        prev_coh = cur_coh;
    }
    let (after_coh, rem_sr) = with_pve_exploration_reg(&ctx, |reg| {
        (
            reg.hacking_games[&(char_id, can_id)].virus_coherence,
            reg.self_repair_turns.get(&(char_id, can_id)).copied(),
        )
    });
    assert!(after_coh > base_coh, "SelfRepair must heal virus coherence");
    assert!(
        after_coh > max_coh,
        "SelfRepair must allow virus coherence to exceed max_virus_coherence (got {base_coh} -> {after_coh}, max {max_coh})"
    );
    assert_eq!(rem_sr, None, "self_repair_turns must be cleaned up after 3 turns");

    // Trigger 2nd failure on a standard container in low-sec -> self-destructs with 0.0 explosionDamage
    for attempt in 1..=2 {
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "StartNewGameInstance",
                &[PyRep::Integer(can_fail)],
                &kw,
                None,
            )
            .await
            .expect("StartNewGameInstance");
        with_pve_exploration_reg(&ctx, |reg| {
            let g = reg.hacking_games.get_mut(&(char_id, can_fail)).unwrap();
            g.virus_coherence = 5;
            g.tiles[1].kind = HackingNodeKind::Antivirus;
            g.tiles[1].coherence = 500;
            g.tiles[1].strength = 50;
        });
        let rep = ctx
            .dispatch_rpc(
                &mut sess,
                "hackingMgr",
                None,
                "ClickedOnTile",
                &[PyRep::Integer(can_fail), PyRep::Integer(1)],
                &kw,
                None,
            )
            .await
            .expect("ClickedOnTile fail");
        if attempt == 2 {
            assert_eq!(rep.dict_get("containerExploded"), Some(&PyRep::Bool(true)));
            assert_eq!(rep.dict_get("explosionDamage"), Some(&PyRep::Real(0.0)));
        }
    }
}

#[tokio::test]
async fn test_skyhook_reagent_harvest_secure_surplus_split() {
    // parity: feat:sovereignty.skyhook-reagent-maturity
    eve_testkit::attest(&["feat:sovereignty.skyhook-reagent-maturity"]);

    let ctx = make_context();
    let skyhook_id = 1_040_000_001_i64;
    let t0 = 1_750_000_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (81_143, 0, 0, t0));
        snap.sov_runtime
            .skyhook_last_harvest_unix
            .insert(skyhook_id, t0);
        let (_, sec_1h, sur_1h) = advance_skyhook_reagents_in_snap(snap, skyhook_id, t0 + 3_600)
            .expect("1h skyhook harvest");
        assert!(sec_1h > 0, "Newly harvested reagents must split into Secure bay");
        assert!(sur_1h > 0, "Newly harvested reagents must split into Surplus bay");
        assert_eq!(sec_1h, sur_1h, "Harvested reagents split 50/50 between Secure and Surplus");
    });
}
