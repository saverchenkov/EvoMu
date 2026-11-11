//! Unit and integration tests for `pve_expeditions`.

#[cfg(test)]
#[path = "pve_expeditions_ops_cov_tests.rs"]
mod pve_expeditions_ops_cov_tests;

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
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
