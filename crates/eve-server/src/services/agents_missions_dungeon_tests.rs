//! Regression coverage for canonical mission pocket dungeon runtime, kill-progress
//! preservation across restarts/downtime, unified kill sync, and terminal cleanup (`#1496`).

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::agents_missions::{handle_with_bound, on_mission_npc_destroyed_core};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use crate::services::mission_fixture::{self, MissionFixture};
use crate::services::pve_dungeon_content::{
    install_wiki_pve_catalog_for_test, parse_and_validate_wiki_pve_json,
};
use crate::services::pve_dungeon_runtime::{
    apply_finish_dungeon_instance_snap, on_dungeon_npc_killed, read_dungeon_runtime_store,
    tick_dungeon_runtime_system_step, wake_dungeon_system_handle, COMPLETED_INSTANCE_RETENTION_SECS,
};
use crate::services::pve_dungeon_runtime::actions::{
    apply_dungeon_trigger_actions_impl, DungeonRuntimeDeps,
};
use crate::services::pve_dungeon_runtime_rpc::{
    is_ship_in_mwd_disallowed_dungeon_pocket, try_activate_dungeon_runtime_gate,
};
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use crate::sp6_lifecycle::{
    cleanup_npc_wrecks_and_reset_missions_on_downtime,
    spawn_mission_wave_on_pocket_entry_for_mission_if_needed,
};
use eve_db::clock::VirtualServerClock;
use eve_db::types::CosmicSignatureRecord;
use eve_db::{ServerClock, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DungeonTemplate, MissionTemplate, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        if let Some(env_path) = std::env::var_os("EVOMU_SDE_PATH").filter(|v| !v.is_empty()) {
            let p = std::path::PathBuf::from(env_path);
            if p.exists() {
                return Arc::new(SdeRegistry::open(&p).expect("open EVOMU_SDE_PATH sqlite"));
            }
        }
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join("data/evomu_sde.sqlite");
            if candidate.exists() {
                return Arc::new(SdeRegistry::open(&candidate).expect("open data/evomu_sde.sqlite"));
            }
            if !dir.pop() {
                break;
            }
        }
        Arc::new(SdeRegistry::with_canonical_seed())
    }))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn bound_agent(char_id: i64, agent_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: agent_id,
        oid_str: format!("N=1:{agent_id}"),
        ref_id: 1,
    }
}

fn docked_session(char_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;
    sess.ship_id = Some(100_000_001);
    sess
}

#[tokio::test]
async fn mission_pocket_entry_creates_linked_dungeon_runtime_with_exact_npc_count_and_bounties() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mid = 991_101_u32;
    let origin = DVec3::new(10_000_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid, 3_018_601, sys_id)
                .site_origin_m(Some([origin.x as i64, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![34_155, 34_154])),
        );
    });

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    let spawned = materialize_or_reconcile_mission_dungeon(
        &ctx,
        &sys_handle,
        char_id,
        sys_id,
        Some(mid),
        origin,
    )
    .await;
    assert_eq!(spawned, 2);

    let (inst_id, spawned_bids) = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).expect("accepted mission present");
        (
            m.dungeon_instance_id.expect("dungeon_instance_id linked"),
            m.spawned_npc_ball_ids,
        )
    });
    assert_eq!(spawned_bids.len(), 2);

    read_dungeon_runtime_store(&ctx, |store| {
        let inst = store.instances.get(&inst_id).expect("linked dungeon instance");
        assert_eq!(inst.category, "Mission");
        assert_eq!(inst.owner_char_id, char_id);
        assert_eq!(inst.solar_system_id, sys_id);
        assert_eq!(inst.rooms.len(), 1);
        assert!(inst.rooms[0].entered);
        assert_eq!(inst.rooms[0].npc_groups[0].npc_ball_ids, spawned_bids);
    });

    for &bid in &spawned_bids {
        let pos = sys_handle
            .get_ball_position_sync(bid)
            .expect("mission NPC ball spawned in ballpark");
        assert!((pos.distance(origin) - 4_500.0).abs() <= 5.0);
    }

    with_combat_runtime(&ctx, |rt| {
        let e0 = &rt.npc_entities[&spawned_bids[0]];
        let e1 = &rt.npc_entities[&spawned_bids[1]];
        assert_eq!(e0.type_id, 34_155);
        assert_eq!(e1.type_id, 34_154);
        assert!(e0.bounty_isk >= 10_000.0);
        assert!(e1.bounty_isk >= 10_000.0);
    });
}

#[tokio::test]
async fn mission_partial_kill_progress_persists_across_downtime_and_never_resets_zero_upward() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mid_partial = 991_201_u32;
    let mid_cleared = 991_202_u32;
    let origin_partial = DVec3::new(15_000_000.0, 0.0, 0.0);
    let origin_cleared = DVec3::new(25_000_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_partial, 3_019_001, sys_id)
                .site_origin_m(Some([origin_partial.x as i64, 0, 0]))
                .npcs_remaining(4)
                .initial_wave_count(Some(4))
                .initial_wave_types(Some(vec![10_019, 10_020, 10_021, 10_022])),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_cleared, 3_019_002, sys_id)
                .site_origin_m(Some([origin_cleared.x as i64, 0, 0]))
                .npcs_remaining(1)
                .initial_wave_count(Some(1))
                .initial_wave_types(Some(vec![10_019])),
        );
    });

    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin_partial,
            Some(mid_partial),
        ),
        4
    );
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin_cleared,
            Some(mid_cleared),
        ),
        1
    );

    let (partial_bids, cleared_bid, partial_inst_id) = ctx.db.with_snapshot(|snap| {
        let mp = mission_fixture::get(snap, char_id, mid_partial).unwrap();
        let mc = mission_fixture::get(snap, char_id, mid_cleared).unwrap();
        (
            mp.spawned_npc_ball_ids,
            mc.spawned_npc_ball_ids[0],
            mp.dungeon_instance_id.unwrap(),
        )
    });
    let killed_partial = &partial_bids[..3];
    let survivor_bid = partial_bids[3];

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    for &kid in killed_partial {
        sys_handle.try_remove_ball(kid);
        on_mission_npc_destroyed_core(&ctx.db, sys_id, kid, Some(char_id));
    }
    sys_handle.try_remove_ball(cleared_bid);
    on_mission_npc_destroyed_core(&ctx.db, sys_id, cleared_bid, Some(char_id));

    let (_wrecks, reset_count) = cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(reset_count, 1, "only the uncompleted partial mission resets");

    assert!(sys_handle.get_ball_position_sync(survivor_bid).is_none());
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.npc_entities.contains_key(&survivor_bid));
    });

    ctx.db.with_snapshot(|snap| {
        let mp = mission_fixture::get(snap, char_id, mid_partial).unwrap();
        assert_eq!(mp.npcs_remaining, 1, "PVE-019: must preserve remaining=1");
        assert!(mp.awaiting_downtime_respawn);
        assert!(mp.spawned_npc_ball_ids.is_empty());
        assert_eq!(mp.dungeon_instance_id, Some(partial_inst_id));

        let mc = mission_fixture::get(snap, char_id, mid_cleared).unwrap();
        assert_eq!(mc.npcs_remaining, 0, "PVE-019: 0 remaining must stay 0");
        assert!(!mc.awaiting_downtime_respawn);
    });

    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin_partial,
            Some(mid_partial),
        ),
        1
    );
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin_cleared,
            Some(mid_cleared),
        ),
        0
    );

    ctx.db.with_snapshot(|snap| {
        let mp = mission_fixture::get(snap, char_id, mid_partial).unwrap();
        assert_eq!(mp.npcs_remaining, 1);
        assert_eq!(mp.spawned_npc_ball_ids, vec![survivor_bid]);
        assert!(!mp.awaiting_downtime_respawn);
    });
    for &kid in killed_partial {
        assert!(sys_handle.get_ball_position_sync(kid).is_none());
    }
    assert!(sys_handle.get_ball_position_sync(survivor_bid).is_some());
}

#[tokio::test]
async fn mission_restart_reconciles_surviving_npcs_without_regenerating_ids_or_resurrecting_killed()
{
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mid = 991_301_u32;
    let origin = DVec3::new(35_000_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid, 3_019_001, sys_id)
                .site_origin_m(Some([origin.x as i64, 0, 0]))
                .npcs_remaining(3)
                .initial_wave_count(Some(3))
                .initial_wave_types(Some(vec![10_019, 10_020, 10_021])),
        );
    });

    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin,
            Some(mid),
        ),
        3
    );

    let (inst_id, first_bids) = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        (m.dungeon_instance_id.unwrap(), m.spawned_npc_ball_ids)
    });
    let killed_id = first_bids[0];
    let survivors = first_bids[1..].to_vec();

    on_dungeon_npc_killed(&ctx, sys_id, killed_id, char_id).await;

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    for &bid in &first_bids {
        sys_handle.try_remove_ball(bid);
    }
    with_mut_combat_runtime(&ctx, |rt| {
        for &bid in &first_bids {
            rt.npc_entities.remove(&bid);
        }
    });

    let reconciled = materialize_or_reconcile_mission_dungeon(
        &ctx,
        &sys_handle,
        char_id,
        sys_id,
        Some(mid),
        origin,
    )
    .await;
    assert_eq!(reconciled, 2);

    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        assert_eq!(m.npcs_remaining, 2);
        assert_eq!(m.spawned_npc_ball_ids, survivors);
    });
    read_dungeon_runtime_store(&ctx, |store| {
        let grp = &store.instances[&inst_id].rooms[0].npc_groups[0];
        assert_eq!(grp.npc_ball_ids, survivors);
        assert_eq!(grp.dead_npc_ball_ids, vec![killed_id]);
    });
    assert!(sys_handle.get_ball_position_sync(killed_id).is_none());
    for &bid in &survivors {
        assert!(sys_handle.get_ball_position_sync(bid).is_some());
        assert!(with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&bid)));
    }
}

#[tokio::test]
async fn unified_kill_sync_avoids_double_decrement_across_mission_and_dungeon_kill_hooks() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mid = 991_401_u32;
    let origin = DVec3::new(45_000_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid, 3_019_001, sys_id)
                .site_origin_m(Some([origin.x as i64, 0, 0]))
                .npcs_remaining(3)
                .initial_wave_count(Some(3))
                .initial_wave_types(Some(vec![10_019, 10_020, 10_021])),
        );
    });

    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin,
            Some(mid),
        ),
        3
    );

    let (inst_id, bids) = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        (m.dungeon_instance_id.unwrap(), m.spawned_npc_ball_ids)
    });

    // Order A: mission kill hook runs before dungeon kill hook for the same NPC.
    on_mission_npc_destroyed_core(&ctx.db, sys_id, bids[0], Some(char_id));
    on_dungeon_npc_killed(&ctx, sys_id, bids[0], char_id).await;

    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        assert_eq!(m.npcs_remaining, 2);
        assert_eq!(m.spawned_npc_ball_ids, vec![bids[1], bids[2]]);
    });
    read_dungeon_runtime_store(&ctx, |store| {
        let grp = &store.instances[&inst_id].rooms[0].npc_groups[0];
        assert_eq!(grp.kills_recorded, 1);
        assert_eq!(grp.dead_npc_ball_ids, vec![bids[0]]);
        assert_eq!(grp.npc_ball_ids, vec![bids[1], bids[2]]);
    });

    // Order B: dungeon kill hook runs before mission kill hook for the same NPC.
    on_dungeon_npc_killed(&ctx, sys_id, bids[1], char_id).await;
    on_mission_npc_destroyed_core(&ctx.db, sys_id, bids[1], Some(char_id));

    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        assert_eq!(m.npcs_remaining, 1);
        assert_eq!(m.spawned_npc_ball_ids, vec![bids[2]]);
    });
    read_dungeon_runtime_store(&ctx, |store| {
        let grp = &store.instances[&inst_id].rooms[0].npc_groups[0];
        assert_eq!(grp.kills_recorded, 2);
        assert_eq!(grp.dead_npc_ball_ids, vec![bids[0], bids[1]]);
        assert_eq!(grp.npc_ball_ids, vec![bids[2]]);
    });

    // Final kill completes the linked dungeon instance and leaves npcs_remaining == 0.
    on_mission_npc_destroyed_core(&ctx.db, sys_id, bids[2], Some(char_id));
    on_dungeon_npc_killed(&ctx, sys_id, bids[2], char_id).await;
    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        assert_eq!(m.npcs_remaining, 0);
        assert!(m.spawned_npc_ball_ids.is_empty());
    });
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(store.instances[&inst_id].completed);
    });
}

#[tokio::test]
async fn wiki_backed_mission_dungeon_spawns_via_dungeon_engine_when_wave_types_unseeded() {
    let mut sde = (*shared_sde()).clone();
    let custom_mid = 991_501_u32;
    let custom_did = 991_501_u32;
    let npc_tid = sde
        .types
        .values()
        .filter(|t| t.category_id == 11 && t.faction_id.is_some())
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(10_019);

    sde.missions.insert(
        custom_mid,
        MissionTemplate {
            mission_id: custom_mid,
            title: "Multi-Wave Wiki Mission".into(),
            is_courier: false,
            lp_alpha: 100,
            lp_omega: 100,
            has_standing_rewards: true,
            content_template: Some("BasicKill".into()),
            dungeon_id: Some(custom_did),
        },
    );
    sde.dungeons.insert(
        custom_did,
        DungeonTemplate {
            dungeon_id: custom_did,
            name: "Multi-Wave Wiki Mission".into(),
            archetype_id: Some(20),
            faction_id: Some(500_010),
            difficulty: Some(2),
        },
    );

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let json = format!(
        r#"{{
  "header":{{"schema_version":2,"source":"EVE University Wiki (CC BY-SA 4.0)","license":"CC BY-SA 4.0"}},
  "entries":[{{
    "entry_id":"mission_wiki_multiwave",
    "base_title":"Multi-Wave Wiki Mission",
    "sde_matches":{{"dungeon_ids":[{custom_did}],"mission_ids":[{custom_mid}]}},
    "rooms":[{{
      "room_index":1,
      "waves":[
        {{
          "wave_index":1,
          "timing":"initial_spawn",
          "spawns":[{{"raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}]
        }},
        {{
          "wave_index":2,
          "timing":"sequential_wave",
          "wave_trigger":{{"kind":"wave_cleared","source_wave_index":1}},
          "spawns":[{{"raw_class":"Cruiser","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}]
        }}
      ]
    }}]
  }}]
}}"#
    );
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&json).expect("valid wiki json"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let origin = DVec3::new(55_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, custom_mid, 3_019_001, sys_id)
                .site_origin_m(Some([origin.x as i64, 0, 0]))
                .npcs_remaining(1)
                .initial_wave_count(Some(1)),
        );
    });

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    let spawned = materialize_or_reconcile_mission_dungeon(
        &ctx,
        &sys_handle,
        char_id,
        sys_id,
        Some(custom_mid),
        origin,
    )
    .await;
    assert_eq!(spawned, 1);

    let (inst_id, w1_npc) = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        (m.dungeon_instance_id.unwrap(), m.spawned_npc_ball_ids[0])
    });
    read_dungeon_runtime_store(&ctx, |store| {
        let room = &store.instances[&inst_id].rooms[0];
        assert_eq!(room.npc_groups.len(), 2);
        assert!(room.npc_groups[0].spawned);
        assert!(!room.npc_groups[1].spawned);
    });

    on_dungeon_npc_killed(&ctx, sys_id, w1_npc, char_id).await;

    let w2_bids = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert!(
            m.npcs_remaining >= 1,
            "reinforcement wave 2 must keep mission encounter incomplete"
        );
        m.spawned_npc_ball_ids
    });
    assert_eq!(w2_bids.len(), 1);
    assert_ne!(w2_bids[0], w1_npc);
    assert!(sys_handle.get_ball_position_sync(w2_bids[0]).is_some());
}

#[tokio::test]
async fn terminal_mission_decline_quit_and_expiry_despawn_linked_dungeon_balls_and_entities() {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_500_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    let char_id = 90_000_001_i64;
    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("SDE agent")
        .clone();
    let sys_id = agent.solar_system_id.unwrap();
    let station_id = agent.station_id.unwrap();

    // 1. Quit an accepted mission clears its linked DungeonRuntimeInstance, ballpark balls, and npc_entities.
    let mid_quit = 991_601_u32;
    let origin_q = DVec3::new(65_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_quit, agent.agent_id, sys_id)
                .site_origin_m(Some([origin_q.x as i64, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![10_019, 10_020])),
        );
    });
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin_q,
            Some(mid_quit),
        ),
        2
    );
    let (quit_inst_id, quit_bids) = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid_quit).unwrap();
        (m.dungeon_instance_id.unwrap(), m.spawned_npc_ball_ids)
    });

    let mut sess = docked_session(char_id, sys_id, Some(station_id));
    let bound = bound_agent(char_id, agent.agent_id);
    let kw = HashMap::new();
    let _quit_reply = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(11)],
        &kw,
    )
    .await
    .expect("Quit DoAction must be handled")
    .expect("Quit DoAction must succeed");

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    ctx.db.with_snapshot(|snap| {
        assert!(mission_fixture::get(snap, char_id, mid_quit).is_none());
    });
    read_dungeon_runtime_store(&ctx, |store| {
        let inst = &store.instances[&quit_inst_id];
        assert!(inst.failed && inst.despawned);
    });
    for &bid in &quit_bids {
        assert!(sys_handle.get_ball_position_sync(bid).is_none());
        assert!(with_combat_runtime(&ctx, |rt| !rt.npc_entities.contains_key(&bid)));
    }

    // 2. Journal expiry of an accepted mission cleans up linked dungeon runtime, balls, and npc_entities.
    let mid_exp = 991_602_u32;
    let origin_e = DVec3::new(75_000_000.0, 0.0, 0.0);
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_exp, agent.agent_id, sys_id)
                .site_origin_m(Some([origin_e.x as i64, 0, 0]))
                .accept_expiry_unix(Some(now + 60))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![10_019, 10_020])),
        );
    });
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin_e,
            Some(mid_exp),
        ),
        2
    );
    let (exp_inst_id, exp_bids) = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid_exp).unwrap();
        (m.dungeon_instance_id.unwrap(), m.spawned_npc_ball_ids)
    });

    clock.advance_secs(120);
    let _journal = handle_with_bound(
        &ctx,
        &mut sess,
        None,
        "agentMgr",
        "GetMyJournalDetails",
        &[],
        &kw,
    )
    .await
    .expect("GetMyJournalDetails must be handled")
    .expect("GetMyJournalDetails must succeed");

    ctx.db.with_snapshot(|snap| {
        assert!(mission_fixture::get(snap, char_id, mid_exp).is_none());
    });
    read_dungeon_runtime_store(&ctx, |store| {
        let inst = &store.instances[&exp_inst_id];
        assert!(inst.failed && inst.despawned);
    });
    for &bid in &exp_bids {
        assert!(sys_handle.get_ball_position_sync(bid).is_none());
        assert!(with_combat_runtime(&ctx, |rt| !rt.npc_entities.contains_key(&bid)));
    }

    // 3. Declining an offered mission archives it and tears down any linked dungeon runtime instance.
    let mid_dec = 991_603_u32;
    let dec_inst_id = 999_603_i64;
    let now_dec = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.dungeon_runtime.instances.insert(
            dec_inst_id,
            crate::services::pve_dungeon_runtime::DungeonRuntimeInstance {
                instance_id: dec_inst_id,
                dungeon_id: mid_dec,
                dungeon_name: "Offered Dungeon".into(),
                category: "Mission".into(),
                archetype: "Mission".into(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                last_actor_char_id: char_id,
                created_unix: now_dec,
                ..Default::default()
            },
        );
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, mid_dec, agent.agent_id, sys_id)
                .dungeon_instance_id(Some(dec_inst_id)),
        );
    });
    let _decline_reply = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(9)],
        &kw,
    )
    .await
    .expect("Decline DoAction must be handled")
    .expect("Decline DoAction must succeed");
    ctx.db.with_snapshot(|snap| {
        assert!(mission_fixture::get(snap, char_id, mid_dec).is_none());
    });
    read_dungeon_runtime_store(&ctx, |store| {
        let inst = &store.instances[&dec_inst_id];
        assert!(inst.failed && inst.despawned);
    });
}

fn setup_wiki_mission_ctx(
    custom_mid: u32,
    custom_did: u32,
    archetype_id: Option<u32>,
    title: &str,
    wiki_json: &str,
) -> (ServerContext, Arc<VirtualServerClock>, u32) {
    let mut sde = (*shared_sde()).clone();
    let npc_tid = sde
        .types
        .values()
        .filter(|t| t.category_id == 11 && t.faction_id.is_some())
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(10_019);

    sde.missions.insert(
        custom_mid,
        MissionTemplate {
            mission_id: custom_mid,
            title: title.into(),
            is_courier: false,
            lp_alpha: 100,
            lp_omega: 100,
            has_standing_rewards: true,
            content_template: Some("BasicKill".into()),
            dungeon_id: Some(custom_did),
        },
    );
    sde.dungeons.insert(
        custom_did,
        DungeonTemplate {
            dungeon_id: custom_did,
            name: title.into(),
            archetype_id,
            faction_id: Some(500_010),
            difficulty: Some(2),
        },
    );

    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_600_000));
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    let cat = Arc::new(parse_and_validate_wiki_pve_json(wiki_json).expect("valid wiki json"));
    install_wiki_pve_catalog_for_test(&ctx, cat);
    (ctx, clock, npc_tid)
}

#[tokio::test]
async fn wiki_count_mismatch_completes_mission_prevents_respawn_farm_and_preserves_deadspace_sites() {
    let custom_mid = 991_701_u32;
    let custom_did = 991_701_u32;
    let npc_tid = shared_sde()
        .types
        .values()
        .filter(|t| t.category_id == 11 && t.faction_id.is_some())
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(10_019);
    let json = format!(
        r#"{{
  "header":{{"schema_version":2,"source":"EVE University Wiki (CC BY-SA 4.0)","license":"CC BY-SA 4.0"}},
  "entries":[{{
    "entry_id":"mission_wiki_mismatch",
    "base_title":"Mismatch Mission",
    "sde_matches":{{"dungeon_ids":[{custom_did}],"mission_ids":[{custom_mid}]}},
    "rooms":[{{
      "room_index":1,
      "waves":[{{
        "wave_index":1,
        "timing":"initial_spawn",
        "spawns":[{{"raw_class":"Frigate","count":{{"min":2,"max":2}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}]
      }}]
    }}]
  }}]
}}"#
    );
    let (ctx, _clock, _) =
        setup_wiki_mission_ctx(custom_mid, custom_did, Some(20), "Mismatch Mission", &json);

    let char_id = 90_000_001_i64;
    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("SDE agent")
        .clone();
    let sys_id = agent.solar_system_id.unwrap();
    let station_id = agent.station_id.unwrap();
    let origin = DVec3::new(82_000_000.0, 0.0, 0.0);

    let sig_id = "EXP-SITE-701";
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: sig_id.into(),
            dungeon_id: custom_did,
            dungeon_name: "Unrelated Deadspace".into(),
            solar_system_id: sys_id,
            site_category: "Deadspace".into(),
            position: [1_000_000.0, 0.0, 0.0],
            cleared: false,
            ..Default::default()
        });
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(station_id);
        }
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: sig_id.into(),
                solar_system_id: sys_id,
                sig_group: "Combat".into(),
                type_name: "Unrelated Deadspace".into(),
                position: [1_000_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                created_unix: ctx.now_unix(),
                lifetime_seconds: 86_400,
                ..Default::default()
            });
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, custom_mid, agent.agent_id, sys_id)
                .site_origin_m(Some([origin.x as i64, 0, 0]))
                .npcs_remaining(4)
                .initial_wave_count(Some(4)),
        );
    });

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    assert_eq!(
        materialize_or_reconcile_mission_dungeon(
            &ctx,
            &sys_handle,
            char_id,
            sys_id,
            Some(custom_mid),
            origin,
        )
        .await,
        2
    );
    let (inst_id, bids) = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.npcs_remaining, 2, "wiki count must override fixture initial count");
        (m.dungeon_instance_id.unwrap(), m.spawned_npc_ball_ids)
    });

    for &bid in &bids {
        on_dungeon_npc_killed(&ctx, sys_id, bid, char_id).await;
    }

    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.npcs_remaining, 0, "completed wiki dungeon must zero npcs_remaining");
        let sig = snap.cosmic_signatures[&sys_id]
            .iter()
            .find(|s| s.signature_id == sig_id)
            .unwrap();
        assert!(!sig.collapsed, "Mission completion must not collapse Deadspace signatures");
    });
    with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&sys_id]
            .iter()
            .find(|s| s.site_id == sig_id)
            .unwrap();
        assert!(!site.cleared, "Mission completion must not clear Deadspace exploration sites");
    });

    assert_eq!(
        materialize_or_reconcile_mission_dungeon(
            &ctx,
            &sys_handle,
            char_id,
            sys_id,
            Some(custom_mid),
            origin,
        )
        .await,
        0,
        "re-warping after dungeon completion must not respawn a fresh mission dungeon"
    );
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin,
            Some(custom_mid),
        ),
        0
    );
    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.dungeon_instance_id, Some(inst_id));
    });

    let mut sess = docked_session(char_id, sys_id, Some(station_id));
    let bound = bound_agent(char_id, agent.agent_id);
    let kw = HashMap::new();
    let _complete_reply = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6)],
        &kw,
    )
    .await
    .expect("Complete DoAction handled")
    .expect("CompleteMission must succeed without MissionIncomplete softlock");
    ctx.db.with_snapshot(|snap| {
        assert!(mission_fixture::get(snap, char_id, custom_mid).is_none());
    });
}

#[tokio::test]
async fn multi_room_wiki_mission_tracks_rooms_mwd_disallow_gate_order_b_and_downtime() {
    let custom_mid = 991_702_u32;
    let custom_did = 991_702_u32;
    let npc_tid = shared_sde()
        .types
        .values()
        .filter(|t| t.category_id == 11 && t.faction_id.is_some())
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(10_019);
    let json = format!(
        r#"{{
  "header":{{"schema_version":2,"source":"EVE University Wiki (CC BY-SA 4.0)","license":"CC BY-SA 4.0"}},
  "entries":[{{
    "entry_id":"mission_wiki_multiroom",
    "base_title":"Multi-Room Deadspace Mission",
    "sde_matches":{{"dungeon_ids":[{custom_did}],"mission_ids":[{custom_mid}]}},
    "rooms":[
      {{
        "room_index":1,
        "room_name":"Room One",
        "has_acceleration_gate":true,
        "waves":[{{
          "wave_index":1,
          "timing":"initial_spawn",
          "spawns":[{{"raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}]
        }}]
      }},
      {{
        "room_index":2,
        "room_name":"Room Two",
        "has_acceleration_gate":false,
        "waves":[{{
          "wave_index":1,
          "timing":"initial_spawn",
          "spawns":[{{"raw_class":"Cruiser","count":{{"min":2,"max":2}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}]
        }}]
      }}
    ]
  }}]
}}"#
    );
    // SDE archetype 3 disallows Microwarpdrive inside the deadspace pocket.
    let (ctx, _clock, _) = setup_wiki_mission_ctx(
        custom_mid,
        custom_did,
        Some(3),
        "Multi-Room Deadspace Mission",
        &json,
    );

    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("SDE agent")
        .clone();
    let sys_id = agent.solar_system_id.unwrap();
    let station_id = agent.station_id.unwrap();
    let origin = DVec3::new(92_000_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(station_id);
            ch.active_ship_id = ship_id;
        }
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, custom_mid, agent.agent_id, sys_id)
                .site_origin_m(Some([origin.x as i64, 0, 0]))
                .npcs_remaining(1)
                .initial_wave_count(Some(1)),
        );
    });

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    let ship_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: origin,
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle.spawn_ball(ship_ball).await.expect("spawn ship");

    assert_eq!(
        materialize_or_reconcile_mission_dungeon(
            &ctx,
            &sys_handle,
            char_id,
            sys_id,
            Some(custom_mid),
            origin,
        )
        .await,
        1
    );

    let (inst_id, r1_npc, r1_gate_id, r1_gate_pos) = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.current_room_id, 1);
        assert_eq!(m.npcs_remaining, 3, "must include unentered Room 2 NPCs");
        assert_eq!(m.spawned_npc_ball_ids.len(), 1);
        let iid = m.dungeon_instance_id.unwrap();
        let inst = &snap.dungeon_runtime.instances[&iid];
        assert_eq!(inst.current_room_index, 1, "1-indexed wiki Room 1");
        let gl = &inst.rooms[0].gate_links[0];
        (iid, m.spawned_npc_ball_ids[0], gl.gate_ball_id.unwrap(), gl.gate_position)
    });

    assert!(
        is_ship_in_mwd_disallowed_dungeon_pocket(&ctx, sys_id, origin),
        "MWD disallow must match 1-indexed wiki mission pocket Room 1"
    );

    on_dungeon_npc_killed(&ctx, sys_id, r1_npc, char_id).await;

    let mut docked_sess = docked_session(char_id, sys_id, Some(station_id));
    let bound = bound_agent(char_id, agent.agent_id);
    let kw = HashMap::new();
    let premature_err = handle_with_bound(
        &ctx,
        &mut docked_sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6)],
        &kw,
    )
    .await
    .expect("DoAction handled")
    .expect_err("must reject CompleteMission while Room 2 NPCs remain");
    assert_eq!(premature_err.code, "MissionIncomplete");

    let mut space_sess = docked_session(char_id, sys_id, None);
    let _ = sys_handle
        .set_ball_position(ship_id, DVec3::from_array(r1_gate_pos))
        .await;
    let dng_arg = format!("DNG-INST-{inst_id}");
    let gate_res = try_activate_dungeon_runtime_gate(
        &ctx,
        &mut space_sess,
        sys_id,
        &[PyRep::String(dng_arg)],
    )
    .await
    .expect("DNG-INST gate handled")
    .expect("Room 1 -> Room 2 gate activation succeeds");
    assert_eq!(gate_res.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(2));

    let r2_npcs = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.current_room_id, 2);
        assert_eq!(m.npcs_remaining, 2);
        assert_eq!(m.spawned_npc_ball_ids.len(), 2);
        let inst = &snap.dungeon_runtime.instances[&inst_id];
        assert_eq!(inst.current_room_index, 2);
        m.spawned_npc_ball_ids
    });

    // Order-B kill on a Room 2 NPC absent from `enc.spawned_npc_ball_ids` (e.g. unsynced link).
    ctx.db.with_mut_snapshot(|snap| {
        let _ = mission_fixture::edit(snap, char_id, custom_mid, |m| {
            m.spawned_npc_ball_ids.retain(|&id| id != r2_npcs[0]);
        });
    });
    on_dungeon_npc_killed(&ctx, sys_id, r2_npcs[0], char_id).await;
    on_mission_npc_destroyed_core(&ctx.db, sys_id, r2_npcs[0], Some(char_id));

    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.npcs_remaining, 1, "Order-B kill must decrement 2 -> 1 without double-decrement");
        assert_eq!(m.spawned_npc_ball_ids, vec![r2_npcs[1]]);
    });

    // Downtime reset must despawn gates/NPCs and reset current_room_index = 1 alongside mission room 1.
    let (_wrecks, reset_count) = cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(reset_count, 1);
    assert!(sys_handle.get_ball_position_sync(r1_gate_id).is_none());
    assert!(sys_handle.get_ball_position_sync(r2_npcs[1]).is_none());
    read_dungeon_runtime_store(&ctx, |store| {
        assert_eq!(store.instances[&inst_id].current_room_index, 1);
    });

    // Re-materializing after downtime preserves room 1 gate & surviving Room 2 NPC at original IDs.
    let _ = materialize_or_reconcile_mission_dungeon(
        &ctx,
        &sys_handle,
        char_id,
        sys_id,
        Some(custom_mid),
        origin,
    )
    .await;
    assert!(sys_handle.get_ball_position_sync(r1_gate_id).is_some());
    assert!(sys_handle.get_ball_position_sync(r2_npcs[1]).is_some());
    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.current_room_id, 1);
        assert_eq!(m.npcs_remaining, 1);
        assert_eq!(m.spawned_npc_ball_ids, vec![r2_npcs[1]]);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_wiki_entry_runs_room_entered_delayed_wave_plain_gate_spawns_and_prunes_stale() {
    let custom_mid = 991_703_u32;
    let custom_did = 991_703_u32;
    let npc_tid = shared_sde()
        .types
        .values()
        .filter(|t| t.category_id == 11 && t.faction_id.is_some())
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(10_019);
    let json = format!(
        r#"{{
  "header":{{"schema_version":2,"source":"EVE University Wiki (CC BY-SA 4.0)","license":"CC BY-SA 4.0"}},
  "entries":[{{
    "entry_id":"mission_wiki_sync_triggers",
    "base_title":"Sync Triggers Mission",
    "sde_matches":{{"dungeon_ids":[{custom_did}],"mission_ids":[{custom_mid}]}},
    "rooms":[
      {{
        "room_index":1,
        "room_name":"Trigger Entry Room",
        "has_acceleration_gate":true,
        "waves":[
          {{
            "wave_index":1,
            "timing":"initial_spawn",
            "spawns":[
              {{"raw_class":"Bunker","count":{{"min":1,"max":1}},"destructible":false,"resolution":{{"status":"resolved_structure_faction","type_ids":[16757]}}}},
              {{"raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}
            ]
          }},
          {{
            "wave_index":2,
            "timing":"sequential_wave",
            "wave_trigger":{{"kind":"on_room_entered"}},
            "spawns":[{{"raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}]
          }},
          {{
            "wave_index":3,
            "timing":"sequential_wave",
            "wave_trigger":{{"kind":"on_timer","delay_secs":5}},
            "spawns":[{{"raw_class":"Cruiser","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}]
          }}
        ]
      }},
      {{
        "room_index":2,
        "room_name":"Exit Chamber",
        "has_acceleration_gate":false,
        "waves":[]
      }}
    ]
  }}]
}}"#
    );
    let (ctx, clock, _) =
        setup_wiki_mission_ctx(custom_mid, custom_did, Some(20), "Sync Triggers Mission", &json);

    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let origin = DVec3::new(102_000_000.0, 0.0, 0.0);
    let stale_inst_id = 999_703_i64;
    let stale_ts = ctx
        .now_unix()
        .saturating_sub(COMPLETED_INSTANCE_RETENTION_SECS + 120);

    ctx.db.with_mut_snapshot(|snap| {
        snap.dungeon_runtime.instances.insert(
            stale_inst_id,
            crate::services::pve_dungeon_runtime::DungeonRuntimeInstance {
                instance_id: stale_inst_id,
                dungeon_id: 1,
                category: "Deadspace".into(),
                solar_system_id: sys_id,
                completed: true,
                despawned: true,
                despawn_after_unix: Some(stale_ts),
                created_unix: stale_ts,
                ..Default::default()
            },
        );
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, custom_mid, 3_019_001, sys_id)
                .site_origin_m(Some([origin.x as i64, 0, 0]))
                .npcs_remaining(1)
                .initial_wave_count(Some(1)),
        );
    });

    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
            &ctx,
            char_id,
            sys_id,
            origin,
            Some(custom_mid),
        ),
        2
    );

    let (inst_id, gate_id, prop_id, initial_npcs) = ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.dungeon_runtime.instances.contains_key(&stale_inst_id),
            "N-03: stale despawned instance must be pruned before creating mission dungeon"
        );
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.npcs_remaining, 3, "must count initial + OnRoomEntered + delayed OnTimer NPCs");
        assert_eq!(
            m.spawned_npc_ball_ids.len(),
            2,
            "sync entry must evaluate OnRoomEntered and sync Wave 1 + Wave 2 NPCs"
        );
        let iid = m.dungeon_instance_id.unwrap();
        let inst = &snap.dungeon_runtime.instances[&iid];
        assert!(inst.rooms[0].entered);
        let gid = inst.rooms[0].gate_links[0].gate_ball_id.unwrap();
        let pid = inst.rooms[0].objects.iter().find(|o| !o.destructible).unwrap().ball_id;
        (iid, gid, pid, m.spawned_npc_ball_ids)
    });

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    let view = sys_handle.view();
    assert!(view.balls.contains_key(&gate_id));
    assert!(
        !sys_handle.is_ball_tethered_sync(gate_id)
            && !view.combat_states.get(&gate_id).is_some_and(|cs| cs.is_tethered),
        "F-03: sync gate spawn must not inject is_tethered: true"
    );
    assert!(view.balls.contains_key(&prop_id));
    assert!(
        !sys_handle.is_ball_tethered_sync(prop_id)
            && !view.combat_states.get(&prop_id).is_some_and(|cs| cs.is_tethered),
        "F-03: indestructible prop sync spawn must not inject is_tethered: true"
    );
    for &npc_id in &initial_npcs {
        assert!(view.balls.contains_key(&npc_id));
        assert!(view.ball_slim_meta.contains_key(&npc_id));
        let cs = view.combat_states.get(&npc_id).expect("NPC combat state");
        assert!(
            cs.base_signature_radius.is_some(),
            "F-03: sync NPC must initialize full tackle/sig baselines via spawn_ball_with_meta_sync"
        );
    }

    // Kill Wave 1 + Wave 2 NPCs before timer fires -> delayed Wave 3 must keep npcs_remaining == 1.
    for &npc_id in &initial_npcs {
        on_dungeon_npc_killed(&ctx, sys_id, npc_id, char_id).await;
    }
    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.npcs_remaining, 1, "pending delayed wave must block premature completion");
        assert!(m.spawned_npc_ball_ids.is_empty());
    });

    clock.advance_secs(10);
    tick_dungeon_runtime_system_step(&ctx, sys_id).await;
    let w3_npc = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.npcs_remaining, 1);
        assert_eq!(m.spawned_npc_ball_ids.len(), 1);
        m.spawned_npc_ball_ids[0]
    });

    on_dungeon_npc_killed(&ctx, sys_id, w3_npc, char_id).await;
    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(
            m.npcs_remaining, 1,
            "P2 #2: un-entered empty Room 2 must keep npcs_remaining >= 1 until dungeon completes"
        );
        let inst = &snap.dungeon_runtime.instances[&inst_id];
        assert!(inst.rooms[0].cleared && !inst.rooms[0].gate_locked);
        assert!(!inst.completed);
    });

    // Completing the dungeon transitions linked mission npcs_remaining to 0.
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let _ = apply_finish_dungeon_instance_snap(snap, inst_id, char_id, false, true, now_u);
        sync_mission_from_dungeon_in_snap(snap, inst_id);
    });
    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, custom_mid).unwrap();
        assert_eq!(m.npcs_remaining, 0);
    });
}

#[tokio::test]
async fn do_action_accepted_wiki_and_fallback_deadspace_missions_enforce_wiki_and_mwd_rules() {
    let wiki_mid = 991_801_u32;
    let wiki_did = 991_801_u32;
    let fb_mid = 991_802_u32;
    let fb_did = 991_802_u32;
    let mut sde = (*shared_sde()).clone();
    let npc_tid = sde
        .types
        .values()
        .filter(|t| t.category_id == 11 && t.faction_id.is_some())
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(10_019);

    for (mid, did, title) in [
        (wiki_mid, wiki_did, "Real DoAction Wiki Deadspace"),
        (fb_mid, fb_did, "Real DoAction Fallback Deadspace"),
    ] {
        sde.missions.insert(
            mid,
            MissionTemplate {
                mission_id: mid,
                title: title.into(),
                is_courier: false,
                lp_alpha: 150,
                lp_omega: 150,
                has_standing_rewards: true,
                content_template: Some("BasicKill".into()),
                dungeon_id: Some(did),
            },
        );
        sde.dungeons.insert(
            did,
            DungeonTemplate {
                dungeon_id: did,
                name: title.into(),
                archetype_id: Some(3),
                faction_id: Some(500_010),
                difficulty: Some(2),
            },
        );
    }

    let json = format!(
        r#"{{
  "header":{{"schema_version":2,"source":"EVE University Wiki (CC BY-SA 4.0)","license":"CC BY-SA 4.0"}},
  "entries":[{{
    "entry_id":"mission_bf1_doaction_wiki",
    "base_title":"Real DoAction Wiki Deadspace",
    "sde_matches":{{"dungeon_ids":[{wiki_did}],"mission_ids":[{wiki_mid}]}},
    "rooms":[
      {{
        "room_index":1,
        "room_name":"Wiki Gate Pocket",
        "has_acceleration_gate":true,
        "waves":[{{"wave_index":1,"timing":"initial_spawn","spawns":[{{"raw_class":"Frigate","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}]}}]
      }},
      {{
        "room_index":2,
        "room_name":"Wiki Final Room",
        "has_acceleration_gate":false,
        "waves":[{{"wave_index":1,"timing":"initial_spawn","spawns":[{{"raw_class":"Cruiser","count":{{"min":1,"max":1}},"resolution":{{"status":"resolved_exact","type_ids":[{npc_tid}]}}}}]}}]
      }}
    ]
  }}]
}}"#
    );
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let cat = Arc::new(parse_and_validate_wiki_pve_json(&json).expect("valid wiki json"));
    install_wiki_pve_catalog_for_test(&ctx, cat);

    let char_id = 90_000_001_i64;
    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("SDE agent")
        .clone();
    let sys_id = agent.solar_system_id.unwrap();
    let station_id = agent.station_id.unwrap();
    let now = ctx.now_unix();
    let mut sess = docked_session(char_id, sys_id, Some(station_id));
    let bound = bound_agent(char_id, agent.agent_id);
    let kw = HashMap::new();

    // 1. Accept wiki-backed mission through real agentMgr.DoAction(3) -> wiki rooms + MWD disallowed.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(station_id);
        }
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert_with_sde(
            snap,
            &ctx.sde,
            MissionFixture::offered(char_id, wiki_mid, agent.agent_id, sys_id)
                .origin_station_id(Some(station_id))
                .offered_at_unix(now)
                .offer_expiry_unix(now + 86_400)
                .npcs_remaining(1),
        );
    });
    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .expect("DoAction handled")
    .expect("DoAction(3) accept wiki mission");

    let wiki_origin = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, wiki_mid).unwrap();
        assert!(
            m.initial_wave_types.is_none(),
            "BF-1: wiki-backed DoAction(3) mission must leave initial_wave_types unseeded"
        );
        let o = m.site_origin_m.expect("bookmark origin set on accept");
        DVec3::new(o[0] as f64, o[1] as f64, o[2] as f64)
    });

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    assert_eq!(
        materialize_or_reconcile_mission_dungeon(
            &ctx,
            &sys_handle,
            char_id,
            sys_id,
            Some(wiki_mid),
            wiki_origin,
        )
        .await,
        1
    );
    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, wiki_mid).unwrap();
        assert_eq!(m.npcs_remaining, 2, "must count both wiki rooms");
        let iid = m.dungeon_instance_id.unwrap();
        let inst = &snap.dungeon_runtime.instances[&iid];
        assert_eq!(inst.rooms.len(), 2, "BF-1: DoAction(3) mission must materialize wiki rooms");
        assert!(inst.disallow_mwd, "BF-1: archetype 3 wiki dungeon must disallow MWD");
    });
    assert!(is_ship_in_mwd_disallowed_dungeon_pocket(&ctx, sys_id, wiki_origin));

    // 2. Accept non-wiki SDE deadspace mission via DoAction(3) -> 1-room fallback also disallows MWD.
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert_with_sde(
            snap,
            &ctx.sde,
            MissionFixture::offered(char_id, fb_mid, agent.agent_id, sys_id)
                .origin_station_id(Some(station_id))
                .offered_at_unix(now)
                .offer_expiry_unix(now + 86_400)
                .npcs_remaining(1),
        );
    });
    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .expect("DoAction handled")
    .expect("DoAction(3) accept fallback mission");

    let fb_origin = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, fb_mid).unwrap();
        assert!(m.initial_wave_types.is_some());
        let o = m.site_origin_m.expect("bookmark origin set on accept");
        DVec3::new(o[0] as f64, o[1] as f64, o[2] as f64)
    });
    assert!(
        materialize_or_reconcile_mission_dungeon(
            &ctx,
            &sys_handle,
            char_id,
            sys_id,
            Some(fb_mid),
            fb_origin,
        )
        .await >= 1
    );
    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, fb_mid).unwrap();
        let inst = &snap.dungeon_runtime.instances[&m.dungeon_instance_id.unwrap()];
        assert_eq!(inst.rooms.len(), 1);
        assert!(inst.disallow_mwd, "BF-1: 1-room fallback must inherit archetype 3 disallow_mwd");
    });
    assert!(is_ship_in_mwd_disallowed_dungeon_pocket(&ctx, sys_id, fb_origin));
}

#[tokio::test]
async fn failed_mission_dungeon_halts_rewarp_respawn_and_downtime_bypass() {
    let mid = 991_803_u32;
    let mut sde = (*shared_sde()).clone();
    sde.missions.insert(
        mid,
        MissionTemplate {
            mission_id: mid,
            title: "Fail Halt Regression".into(),
            is_courier: false,
            lp_alpha: 100,
            lp_omega: 100,
            has_standing_rewards: true,
            content_template: Some("BasicKill".into()),
            dungeon_id: None,
        },
    );
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_700_000));
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    let char_id = 90_000_001_i64;
    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("SDE agent")
        .clone();
    let sys_id = agent.solar_system_id.unwrap();
    let station_id = agent.station_id.unwrap();
    let origin = DVec3::new(112_000_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(station_id);
        }
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid, agent.agent_id, sys_id)
                .site_origin_m(Some([origin.x as i64, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![10_019, 10_020])),
        );
    });

    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    assert_eq!(
        materialize_or_reconcile_mission_dungeon(&ctx, &sys_handle, char_id, sys_id, Some(mid), origin).await,
        2
    );

    let (inst_id, first_npc) = ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        (m.dungeon_instance_id.unwrap(), m.spawned_npc_ball_ids[0])
    });

    // Kill 1 of 2 NPCs, then fail the dungeon via FailDungeon trigger action.
    on_dungeon_npc_killed(&ctx, sys_id, first_npc, char_id).await;
    apply_dungeon_trigger_actions_impl(
        DungeonRuntimeDeps::from_ctx(&ctx),
        sys_id,
        inst_id,
        0,
        vec![(901, DungeonTriggerAction::FailDungeon { reason: "Objective destroyed".into() })],
        char_id,
    )
    .await;

    let pre_count = ctx.db.with_snapshot(|snap| {
        let inst = &snap.dungeon_runtime.instances[&inst_id];
        assert!(inst.failed && inst.despawned);
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        assert_eq!(m.dungeon_instance_id, Some(inst_id));
        assert_eq!(m.npcs_remaining, 1);
        assert!(m.spawned_npc_ball_ids.is_empty());
        snap.dungeon_runtime.instances.len()
    });

    // Re-warp (async & sync) and downtime reset must spawn 0 NPCs and preserve the failed link.
    assert_eq!(
        materialize_or_reconcile_mission_dungeon(&ctx, &sys_handle, char_id, sys_id, Some(mid), origin).await,
        0
    );
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(&ctx, char_id, sys_id, origin, Some(mid)),
        0
    );
    let _ = cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(
        materialize_or_reconcile_mission_dungeon(&ctx, &sys_handle, char_id, sys_id, Some(mid), origin).await,
        0
    );

    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        assert_eq!(m.dungeon_instance_id, Some(inst_id), "BF-2: failed link must remain attached");
        assert!(m.npcs_remaining >= 1);
        assert!(m.spawned_npc_ball_ids.is_empty());
        assert_eq!(snap.dungeon_runtime.instances.len(), pre_count, "BF-2: must not create replacement instance");
    });

    // Advance clock past 1-hour stale-retention window and spawn an unrelated SDE dungeon;
    // snapshot-level pruning must still preserve the failed mission instance and halt re-warp.
    clock.advance_secs(COMPLETED_INSTANCE_RETENTION_SECS + 120);
    let unrelated_did = ctx.sde.dungeons.keys().copied().min().unwrap_or(1);
    let unrelated_inst_id = crate::services::pve_dungeon_runtime_rpc::spawn_sde_dungeon_runtime_instance(
        &ctx,
        char_id + 1,
        sys_id,
        unrelated_did,
        "Deadspace",
        [300_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn unrelated SDE dungeon");
    assert_ne!(unrelated_inst_id, inst_id);

    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.dungeon_runtime.instances.contains_key(&inst_id),
            "OWN-01: non-mission dungeon spawn must not prune failed active mission instance"
        );
    });
    assert_eq!(
        materialize_or_reconcile_mission_dungeon(&ctx, &sys_handle, char_id, sys_id, Some(mid), origin).await,
        0
    );
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_for_mission_if_needed(&ctx, char_id, sys_id, origin, Some(mid)),
        0
    );
    ctx.db.with_snapshot(|snap| {
        let m = mission_fixture::get(snap, char_id, mid).unwrap();
        assert_eq!(m.dungeon_instance_id, Some(inst_id));
        assert!(m.npcs_remaining >= 1);
        assert!(m.spawned_npc_ball_ids.is_empty());
    });

    let mut sess = docked_session(char_id, sys_id, Some(station_id));
    let bound = bound_agent(char_id, agent.agent_id);
    let kw = HashMap::new();
    let complete_err = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6)],
        &kw,
    )
    .await
    .expect("DoAction handled")
    .expect_err("failed mission dungeon must not be completable");
    assert_eq!(complete_err.code, "MissionIncomplete");
}
