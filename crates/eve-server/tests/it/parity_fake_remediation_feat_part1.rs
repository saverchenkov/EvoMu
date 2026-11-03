//! Integration tests remediating `FLAGGED_FAKE` `feat:*` parity rows.
//!
//! Covers:
//! - `feat:wormholes.lifetime-decay-stages`
//! - `feat:navigation.wormhole-info-buckets`
//! - `feat:pve.hacking-node-types`
//! - `feat:pve.hacking-failure-outcome`
//! - `feat:sovereignty.skyhook-reagent-maturity`

use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg, HackingNodeKind, HackingUtilityKind,
};
use eve_server::services::pve_hacking_board::build_hacking_grid_procedural;
use eve_server::services::sov_gameplay::sov_gameplay_skyhook::advance_skyhook_reagents_in_snap;
use eve_server::services::space_destiny::derive_wormhole_buckets_raw;
use std::collections::HashMap;
use std::sync::Arc;

fn make_context() -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    ServerContext::with_repository(db)
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
    let now = 1_750_000_000_u64;
    let lifetime = 48 * 3_600_u64;

    // > 24h remaining (36h left): >1 day bucket, not yet begun to decay
    let fresh = derive_wormhole_buckets_raw(
        30_000_142,
        20_000_000.0,
        1_000_000_000.0,
        1_000_000_000.0,
        now,
        lifetime,
        false,
        now + 12 * 3_600,
        &ctx.sde,
    );
    assert_eq!(fresh.lifetime_bucket, ">1 day");
    assert_eq!(
        fresh.lifetime_description,
        "This wormhole has not yet begun to decay"
    );

    // 4h..=24h remaining (12h left): <1 day bucket, beginning to decay
    let decaying = derive_wormhole_buckets_raw(
        30_000_142,
        20_000_000.0,
        1_000_000_000.0,
        1_000_000_000.0,
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
        20_000_000.0,
        1_000_000_000.0,
        1_000_000_000.0,
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
