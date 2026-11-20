//! Unit and integration tests for `pve_expeditions`.

#[cfg(test)]
#[path = "pve_expeditions_ops_cov_tests.rs"]
mod pve_expeditions_ops_cov_tests;

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{ServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_expedition_ctx(sys_id: SolarSystemId, char_id: i64, ship_id: i64) -> (ServerContext, SessionState) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_pilot", "pw", char_id, "ExpPilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    let mut session = SessionState::new(402);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    (ctx, session)
}

#[test]
fn expedition_chain_step_resolution_and_destination_sde() {
    let (base_id, name, max_steps) = expedition_dungeon_chain(1420);
    assert_eq!(base_id, 1420);
    assert_eq!(name, "Guristas Scout Outpost");
    assert_eq!(max_steps, 3);
    assert_eq!(expedition_dungeon_for_step(base_id, 1), 1420);
    assert_eq!(expedition_dungeon_for_step(base_id, 2), 1421);
    assert_eq!(expedition_dungeon_for_step(base_id, 3), 1422);

    let sig1 = expedition_sig_id(101, 1);
    let sig2 = expedition_sig_id(101, 2);
    assert_ne!(sig1, sig2);
    assert!(sig1.contains('-'));

    let (ctx, _) = setup_expedition_ctx(30_000_142, 90_040_101, 1_040_000_001);
    let dest = find_escalation_destination_system(&ctx, 30_000_142);
    assert!(ctx.sde.solar_systems.contains_key(&dest));

    // Roll RNG deterministically across multiple invocations
    let _ = roll_escalation_chance(&ctx);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escalation_creation_step_advancement_and_room_gating() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_102;
    let ship_id: i64 = 1_040_000_002;
    let (ctx, mut session) = setup_expedition_ctx(sys_id, char_id, ship_id);

    // Create 3-step expedition from Guristas Scout Outpost (1420)
    let entry = create_escalation_expedition(
        &ctx,
        char_id,
        sys_id,
        1420,
        "Guristas Scout Outpost",
        [10_000_000.0, 2_000_000.0, -5_000_000.0],
    )
    .await
    .expect("create_escalation_expedition");
    assert_eq!(entry.step_index, 1);
    assert_eq!(entry.total_steps, 3);

    // Journal rowset & dungeons entered include active escalation
    let rowset = get_my_escalating_path_details_rowset(&ctx, char_id);
    assert!(!matches!(rowset, PyRep::None));
    let entered = crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        get_escalating_path_dungeons_entered(reg, char_id)
    });
    assert_eq!(entered.len(), 1);

    // Advance step 1 -> 2
    let step2 = advance_expedition_step(&ctx, char_id, entry.instance_id)
        .await
        .expect("advance step 1")
        .expect("still active");
    assert_eq!(step2.step_index, 2);

    // Advance step 2 -> 3
    let step3 = advance_expedition_step(&ctx, char_id, entry.instance_id)
        .await
        .expect("advance step 2")
        .expect("final step");
    assert_eq!(step3.step_index, 3);

    // Advance step 3 -> completed entry, then subsequent advance returns None
    let done = advance_expedition_step(&ctx, char_id, entry.instance_id)
        .await
        .expect("complete step 3")
        .expect("completed summary entry");
    assert!(done.journal_text.contains("fully completed"));
    let after = advance_expedition_step(&ctx, char_id, entry.instance_id)
        .await
        .expect("already completed");
    assert!(after.is_none());

    // Enter combat anomaly site + advance room validation
    let site_id = format!("ANOM_{sys_id}_TEST");
    crate::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(
            crate::services::pve_exploration::PveSiteInstance {
                site_id: site_id.clone(),
                dungeon_id: 2001,
                dungeon_name: "Guristas Refuge".into(),
                solar_system_id: sys_id,
                site_category: "CombatAnomaly".into(),
                position: [1_000_000.0, 0.0, 1_000_000.0],
                signal_strength: 1.0,
                current_room: 1,
                total_rooms: 2,
                current_wave: 1,
                total_waves: 1,
                created_unix: 1_775_400_000,
                lifetime_seconds: 86_400,
                ..Default::default()
            },
        );
    });

    let enter_res = enter_combat_anomaly_site(&ctx, char_id, sys_id, &site_id).expect("enter combat anomaly");
    assert!(!matches!(enter_res, PyRep::None));

    // Seed an active NPC ball so room advancement is blocked until NPCs are cleared
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        if let Some(site) = reg
            .sites_by_system
            .get_mut(&sys_id)
            .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id))
        {
            site.active_npc_ball_ids.push(1_040_999_001);
        }
    });
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(crate::services::pve_special_sites::make_site_npc_ball(
            1_040_999_001,
            23_753,
            glam::DVec3::new(1_000_000.0, 0.0, 1_000_000.0),
        ))
        .await;

    let blocked = validate_and_advance_pve_site_room(&ctx, &mut session, sys_id, &site_id).await;
    assert!(blocked.is_err());

    let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(1_040_999_001).await;
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        if let Some(site) = reg
            .sites_by_system
            .get_mut(&sys_id)
            .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id))
        {
            site.active_npc_ball_ids.clear();
        }
    });
    let advanced = validate_and_advance_pve_site_room(&ctx, &mut session, sys_id, &site_id)
        .await
        .expect("advance room once cleared");
    assert!(!matches!(advanced, PyRep::None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escalation_expiry_pruning_despawns_runtime_instance_and_clears_bookmark_expiry() {
    let origin_sys: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_039_901;
    let ship_id: i64 = 1_039_901;
    let (ctx, _session) = setup_expedition_ctx(origin_sys, char_id, ship_id);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000));
    ctx.set_clock(clock.clone());

    let entry = create_escalation_expedition(
        &ctx,
        char_id,
        origin_sys,
        1420,
        "Guristas Scout Outpost",
        [0.0; 3],
    )
    .await
    .expect("create_escalation_expedition");

    let (bm_id, runtime_inst_id) = ctx.db.with_snapshot(|snap| {
        let bm = snap
            .bookmarks
            .values()
            .find(|b| b.owner_id == char_id && b.item_id == Some(entry.instance_id))
            .expect("escalation bookmark must exist");
        assert_eq!(snap.bookmark_expiries.get(&bm.bookmark_id).copied(), Some(entry.expires_at_sec));
        let inst = snap
            .dungeon_runtime
            .instances
            .values()
            .find(|inst| {
                inst.owner_char_id == char_id
                    && inst.solar_system_id == entry.destination_solar_system_id
                    && inst.dungeon_id == entry.expedition_dungeon_id
                    && inst.category == "Expedition"
            })
            .expect("owned Expedition DungeonRuntimeInstance must exist");
        assert!(!inst.despawned);
        (bm.bookmark_id, inst.instance_id)
    });

    clock.advance_secs(86_405);
    let _ = get_my_escalating_path_details_rowset(&ctx, char_id);

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.bookmarks.contains_key(&bm_id));
        assert!(!snap.bookmark_expiries.contains_key(&bm_id));
        assert!(snap.dungeon_runtime.instances[&runtime_inst_id].despawned);
    });
    assert_eq!(
        crate::services::pve_dungeon_runtime::resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "epinstance",
            entry.instance_id,
            None,
        )
        .unwrap(),
        None
    );
    assert_eq!(
        crate::services::pve_dungeon_runtime::resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "dungeon",
            runtime_inst_id,
            None,
        )
        .unwrap(),
        None
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escalation_expiry_and_step_advance_despawn_site_npc_balls_and_ignore_cleared_kills() {
    let origin_sys: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_039_902;
    let ship_id: i64 = 1_039_902;
    let (ctx, _session) = setup_expedition_ctx(origin_sys, char_id, ship_id);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_450_000));
    ctx.set_clock(clock.clone());

    let entry = create_escalation_expedition(
        &ctx,
        char_id,
        origin_sys,
        1420,
        "Guristas Scout Outpost",
        [0.0; 3],
    )
    .await
    .expect("create_escalation_expedition");

    let site_id_1 = format!("EXP-{}-{}", entry.instance_id, entry.step_index);
    let npc_ids_1 = crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&entry.destination_solar_system_id)
            .and_then(|sites| sites.iter().find(|s| s.site_id == site_id_1))
            .map(|s| s.active_npc_ball_ids.clone())
            .expect("spawned EXP site must exist")
    });
    assert!(
        !npc_ids_1.is_empty(),
        "spawn_expedition_site_instance_core must seed Wave 1 NPC balls"
    );
    let dest_handle = ctx
        .systems
        .get_handle(entry.destination_solar_system_id)
        .expect("destination system must be loaded");
    for &bid in &npc_ids_1 {
        assert!(
            dest_handle
                .get_ball_snapshot(bid)
                .await
                .unwrap()
                .is_some(),
            "NPC ball {bid} must exist in ballpark before expiry"
        );
    }

    // Seed a timeless (`expires_at_sec == 0`) escalation on an unloaded system and verify:
    // (1) advancing clock does NOT prune timeless escalations,
    // (2) unloaded systems with expired dummy instances are NOT materialized by `get_handle`.
    let timeless_id = 9_999_001_i64;
    let unloaded_sys: SolarSystemId = 30_000_001;
    assert_ne!(unloaded_sys, entry.destination_solar_system_id);
    assert!(ctx.systems.get_handle(unloaded_sys).is_none());
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char
            .entry(char_id)
            .or_default()
            .push(EscalatingPathEntry {
                instance_id: timeless_id,
                character_id: char_id,
                source_dungeon_id: 1420,
                expedition_dungeon_id: 1420,
                expedition_name: "Timeless Escalation".into(),
                destination_solar_system_id: origin_sys,
                step_index: 1,
                total_steps: 2,
                journal_text: "Timeless".into(),
                created_at_sec: 1_775_450_000,
                expires_at_sec: 0,
                dungeons_entered: Vec::new(),
                journal_entry_template_id: 0,
            });
        reg.escalations_by_char
            .entry(char_id)
            .or_default()
            .push(EscalatingPathEntry {
                instance_id: 9_999_002,
                character_id: char_id,
                source_dungeon_id: 1420,
                expedition_dungeon_id: 1420,
                expedition_name: "Unloaded Expired".into(),
                destination_solar_system_id: unloaded_sys,
                step_index: 1,
                total_steps: 2,
                journal_text: "Expired".into(),
                created_at_sec: 1_775_450_000,
                expires_at_sec: 1_775_450_100,
                dungeons_entered: Vec::new(),
                journal_entry_template_id: 0,
            });
        reg.sites_by_system
            .entry(unloaded_sys)
            .or_default()
            .push(crate::services::pve_exploration::PveSiteInstance {
                site_id: "EXP-9999002-1".into(),
                dungeon_id: 1420,
                solar_system_id: unloaded_sys,
                active_npc_ball_ids: vec![888_001],
                expedition_instance_id: Some(9_999_002),
                ..Default::default()
            });
    });

    clock.advance_secs(86_405);
    let _ = get_my_escalating_path_details_rowset(&ctx, char_id);

    crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        let kept = &reg.escalations_by_char[&char_id];
        assert!(
            kept.iter().any(|e| e.instance_id == timeless_id),
            "expires_at_sec == 0 timeless escalation must not be pruned"
        );
        assert!(
            !kept.iter().any(|e| e.instance_id == entry.instance_id),
            "expired escalation must be pruned"
        );
        let site = reg.sites_by_system[&entry.destination_solar_system_id]
            .iter()
            .find(|s| s.site_id == site_id_1)
            .unwrap();
        assert!(site.cleared);
        assert!(
            site.active_npc_ball_ids.is_empty(),
            "expired site active_npc_ball_ids must be drained synchronously"
        );
    });
    assert!(
        ctx.systems.get_handle(unloaded_sys).is_none(),
        "expiry cleanup must not wake unloaded systems via get_or_spawn"
    );

    // Wait briefly for spawned async ball-removal task on destination SystemHandle.
    for _ in 0..200 {
        let mut remaining = 0usize;
        for &bid in &npc_ids_1 {
            if dest_handle.get_ball_snapshot(bid).await.unwrap().is_some() {
                remaining += 1;
            }
        }
        if remaining == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    for &bid in &npc_ids_1 {
        assert_eq!(
            dest_handle.get_ball_snapshot(bid).await.unwrap(),
            None,
            "expired EXP site NPC ball {bid} must be removed from ballpark"
        );
    }

    // Even if a cleared site still held an orphaned ball ID, kill processing must ignore it.
    let sentinel_ball = npc_ids_1[0];
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        let site = reg
            .sites_by_system
            .get_mut(&entry.destination_solar_system_id)
            .and_then(|sites| sites.iter_mut().find(|s| s.site_id == site_id_1))
            .unwrap();
        site.active_npc_ball_ids.push(sentinel_ball);
    });
    let kill_res = crate::services::pve_exploration::handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &*ctx.combat_ephemeral,
        entry.destination_solar_system_id,
        sentinel_ball,
        char_id,
        false,
    )
    .await;
    assert!(kill_res.0.is_none() && kill_res.1.is_none() && kill_res.2.is_none());
    crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&entry.destination_solar_system_id]
            .iter()
            .find(|s| s.site_id == site_id_1)
            .unwrap();
        assert_eq!(site.current_wave, 1, "cleared site kill must not spawn wave 2");
        assert_eq!(
            site.active_npc_ball_ids,
            vec![sentinel_ball],
            "cleared site must be skipped before mutating active_npc_ball_ids"
        );
    });

    // Direct advance_expedition_step_core before Wave 1 is killed must also clear step 1 and remove its NPCs.
    let entry2 = create_escalation_expedition(
        &ctx,
        char_id,
        origin_sys,
        1420,
        "Guristas Scout Outpost",
        [0.0; 3],
    )
    .await
    .expect("create second escalation");
    let site_id_2_step1 = format!("EXP-{}-{}", entry2.instance_id, entry2.step_index);
    let npc_ids_2 = crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&entry2.destination_solar_system_id]
            .iter()
            .find(|s| s.site_id == site_id_2_step1)
            .map(|s| s.active_npc_ball_ids.clone())
            .expect("step 1 EXP site must exist")
    });
    assert!(!npc_ids_2.is_empty());
    let dest2_handle = ctx
        .systems
        .get_handle(entry2.destination_solar_system_id)
        .expect("step 1 system loaded");
    for &bid in &npc_ids_2 {
        assert!(dest2_handle.get_ball_snapshot(bid).await.unwrap().is_some());
    }

    let advanced = advance_expedition_step_core(
        &ctx.db,
        &ctx.sde,
        &ctx.systems,
        char_id,
        entry2.instance_id,
    )
    .await
    .expect("advance_expedition_step_core")
    .expect("step 2 entry");
    assert_eq!(advanced.step_index, 2);

    crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        let old_site = reg.sites_by_system[&entry2.destination_solar_system_id]
            .iter()
            .find(|s| s.site_id == site_id_2_step1)
            .unwrap();
        assert!(old_site.cleared);
        assert!(old_site.active_npc_ball_ids.is_empty());
    });
    for &bid in &npc_ids_2 {
        assert_eq!(
            dest2_handle.get_ball_snapshot(bid).await.unwrap(),
            None,
            "previous-step NPC ball {bid} must be removed synchronously during advance_expedition_step_core"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_unpruned_escalation_cannot_resurrect_or_drop_final_loot_via_step_advance_or_npc_kill()
{
    let origin_sys: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_041_462;
    let ship_id: i64 = 1_041_462_001;
    let (ctx, _session) = setup_expedition_ctx(origin_sys, char_id, ship_id);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_600_000));
    ctx.set_clock(clock.clone());

    // 1. Intermediate expired-but-unpruned step must NOT resurrect (+86_400s) on advance_expedition_step.
    let entry1 = create_escalation_expedition(
        &ctx,
        char_id,
        origin_sys,
        1420,
        "Guristas Scout Outpost",
        [0.0; 3],
    )
    .await
    .expect("create entry1");
    assert!(entry1.step_index < entry1.total_steps);
    let site1_id = format!("EXP-{}-{}", entry1.instance_id, entry1.step_index);
    let npc_ids_1 = crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&entry1.destination_solar_system_id]
            .iter()
            .find(|s| s.site_id == site1_id)
            .map(|s| s.active_npc_ball_ids.clone())
            .unwrap()
    });
    let dest1_handle = ctx
        .systems
        .get_handle(entry1.destination_solar_system_id)
        .expect("dest1 handle");
    let uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid,
        char_id as u64,
        Some(char_id),
        None,
        true,
        1_775_600_000,
    );
    let _ = ctx.db.set_character_solar_system(
        char_id,
        entry1.destination_solar_system_id,
        &ctx.sde,
    );
    let _ = ctx.drain_notifications_for_char(char_id);

    // Advance clock past expiry WITHOUT calling get_my_escalating_path_details_rowset.
    clock.advance_secs(86_405);
    let step1_adv = advance_expedition_step(&ctx, char_id, entry1.instance_id)
        .await
        .expect("advance on expired escalation returns Ok(None)");
    assert!(
        step1_adv.is_none(),
        "expired unpruned escalation must return Ok(None) rather than resurrecting +24h"
    );

    crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        assert!(
            reg.escalations_by_char
                .get(&char_id)
                .is_none_or(|l| !l.iter().any(|e| e.instance_id == entry1.instance_id)),
            "expired escalation must be removed from escalations_by_char"
        );
        let site1 = reg.sites_by_system[&entry1.destination_solar_system_id]
            .iter()
            .find(|s| s.site_id == site1_id)
            .unwrap();
        assert!(site1.cleared);
        assert!(site1.active_npc_ball_ids.is_empty());
    });
    for &bid in &npc_ids_1 {
        assert_eq!(dest1_handle.get_ball_snapshot(bid).await.unwrap(), None);
    }
    let sig1_id = expedition_sig_id(entry1.instance_id, 1);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap
            .bookmarks
            .values()
            .any(|b| b.owner_id == char_id && b.item_id == Some(entry1.instance_id)));
        assert!(snap.cosmic_signatures[&entry1.destination_solar_system_id]
            .iter()
            .filter(|s| s.signature_id == sig1_id)
            .all(|s| s.collapsed));
        assert!(snap
            .dungeon_runtime
            .instances
            .values()
            .filter(|inst| {
                inst.owner_char_id == char_id
                    && inst.solar_system_id == entry1.destination_solar_system_id
                    && inst.dungeon_id == entry1.expedition_dungeon_id
            })
            .all(|inst| inst.despawned));
    });
    let notes1 = ctx.drain_notifications_for_char(char_id);
    assert!(notes1.iter().any(|n| n.event == "OnDungeonInstanceRemoved"));
    assert!(notes1.iter().any(|n| n.event == "OnEscalatingPathChange"));

    // 2. Final step (`step_index == total_steps`) expired-but-unpruned must NOT drop 19419 loot on advance_expedition_step_core.
    let mut entry2 = create_escalation_expedition(
        &ctx,
        char_id,
        origin_sys,
        1420,
        "Guristas Scout Outpost",
        [0.0; 3],
    )
    .await
    .expect("create entry2");
    while entry2.step_index < entry2.total_steps {
        entry2 = advance_expedition_step(&ctx, char_id, entry2.instance_id)
            .await
            .unwrap()
            .unwrap();
    }
    let final2_sys = entry2.destination_solar_system_id;
    let site2_final_id = format!("EXP-{}-{}", entry2.instance_id, entry2.step_index);
    let npc_ids_2 = crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&final2_sys]
            .iter()
            .find(|s| s.site_id == site2_final_id)
            .map(|s| s.active_npc_ball_ids.clone())
            .unwrap()
    });
    let final2_handle = ctx.systems.get_handle(final2_sys).expect("final2 handle");

    clock.advance_secs(86_405);
    let core_res = advance_expedition_step_core(
        &ctx.db,
        &ctx.sde,
        &ctx.systems,
        char_id,
        entry2.instance_id,
    )
    .await
    .expect("core advance returns Ok(None)");
    assert!(
        core_res.is_none(),
        "expired final-step escalation must return Ok(None)"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.values().any(|it| it.type_id == 19_419),
            "expired final-step advance must NEVER drop Overseer's Personal Effects (19419)"
        );
        assert!(!snap
            .bookmarks
            .values()
            .any(|b| b.owner_id == char_id && b.item_id == Some(entry2.instance_id)));
    });
    for &bid in &npc_ids_2 {
        assert_eq!(final2_handle.get_ball_snapshot(bid).await.unwrap(), None);
    }

    // 3. Final-wave NPC destruction on an expired-but-unpruned final-step escalation site must NOT complete or drop 19419 loot.
    let mut entry3 = create_escalation_expedition(
        &ctx,
        char_id,
        origin_sys,
        1420,
        "Guristas Scout Outpost",
        [0.0; 3],
    )
    .await
    .expect("create entry3");
    while entry3.step_index < entry3.total_steps {
        entry3 = advance_expedition_step(&ctx, char_id, entry3.instance_id)
            .await
            .unwrap()
            .unwrap();
    }
    let final3_sys = entry3.destination_solar_system_id;
    let site3_final_id = format!("EXP-{}-{}", entry3.instance_id, entry3.step_index);
    let last_npc3 = crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        let site = reg
            .sites_by_system
            .get_mut(&final3_sys)
            .and_then(|l| l.iter_mut().find(|s| s.site_id == site3_final_id))
            .unwrap();
        site.current_wave = site.total_waves;
        let last = *site.active_npc_ball_ids.first().unwrap();
        site.active_npc_ball_ids = vec![last];
        last
    });
    let final3_handle = ctx.systems.get_handle(final3_sys).expect("final3 handle");

    clock.advance_secs(86_405);
    let kill_res = crate::services::pve_exploration::handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &*ctx.combat_ephemeral,
        final3_sys,
        last_npc3,
        char_id,
        false,
    )
    .await;
    assert!(
        kill_res.0.is_none() && kill_res.1.is_none() && kill_res.2.is_none(),
        "killing last NPC in expired unpruned escalation must return no clear/create/advance"
    );
    assert_eq!(final3_handle.get_ball_snapshot(last_npc3).await.unwrap(), None);
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.values().any(|it| it.type_id == 19_419),
            "expired escalation NPC kill must NEVER drop final loot (19419)"
        );
        assert!(!snap
            .bookmarks
            .values()
            .any(|b| b.owner_id == char_id && b.item_id == Some(entry3.instance_id)));
    });
    crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        assert!(reg
            .escalations_by_char
            .get(&char_id)
            .is_none_or(|l| !l.iter().any(|e| e.instance_id == entry3.instance_id)));
        let site3 = reg.sites_by_system[&final3_sys]
            .iter()
            .find(|s| s.site_id == site3_final_id)
            .unwrap();
        assert!(site3.cleared);
        assert!(site3.active_npc_ball_ids.is_empty());
    });

    // 4. Timeless (`expires_at_sec == 0`) escalation still advances cleanly even after arbitrary clock advance.
    let timeless_id = 9_999_462_i64;
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char
            .entry(char_id)
            .or_default()
            .push(EscalatingPathEntry {
                instance_id: timeless_id,
                character_id: char_id,
                source_dungeon_id: 1420,
                expedition_dungeon_id: 1420,
                expedition_name: "Guristas Scout Outpost".into(),
                destination_solar_system_id: origin_sys,
                step_index: 1,
                total_steps: 3,
                journal_text: "Timeless".into(),
                created_at_sec: 1_775_600_000,
                expires_at_sec: 0,
                dungeons_entered: vec![1420],
                journal_entry_template_id: 0,
            });
    });
    let timeless_adv = advance_expedition_step(&ctx, char_id, timeless_id)
        .await
        .expect("timeless advance")
        .expect("timeless step 2");
    assert_eq!(timeless_adv.step_index, 2);
}
