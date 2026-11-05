use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, WorldRepository};
use eve_sde::{AgentInfo, MissionTemplate, SdeRegistry};
use std::sync::Arc;

fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn make_agent(agent_id: i64, agent_type_id: u32, division_id: u32) -> AgentInfo {
    AgentInfo {
        agent_id,
        name: "Reward Agent".into(),
        corporation_id: 1_000_009,
        division_id,
        level: 1,
        station_id: Some(60_003_760),
        solar_system_id: Some(30_000_142),
        agent_type_id,
        is_locator: false,
        research_skills: vec![],
    }
}

fn make_mission(mission_id: u32) -> MissionTemplate {
    MissionTemplate {
        mission_id,
        title: "Test Mission".into(),
        content_template: None,
        is_courier: false,
        lp_omega: 100,
        lp_alpha: 50,
        has_standing_rewards: true,
        dungeon_id: None,
    }
}

#[test]
fn resolve_mint_and_emit_mission_item_rewards_cover_all_agent_types() {
    let ctx = make_test_context();
    ctx.db
        .acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000)
        .unwrap();

    let tmpl0 = make_mission(0);
    let tmpl3 = make_mission(3);
    assert!(resolve_mission_item_reward(&ctx, &tmpl0, None).is_none());
    assert!(resolve_mission_item_reward(&ctx, &tmpl0, Some(&make_agent(1, 2, 24))).is_none());

    assert_eq!(
        resolve_mission_item_reward(&ctx, &tmpl0, Some(&make_agent(1, 12, 19))),
        Some((30_013, 8))
    );
    assert_eq!(
        resolve_mission_item_reward(&ctx, &tmpl0, Some(&make_agent(1, 12, 22))),
        Some((483, 1))
    );
    assert_eq!(
        resolve_mission_item_reward(&ctx, &tmpl3, Some(&make_agent(1, 12, 24))),
        Some((587, 1))
    );
    assert_eq!(
        resolve_mission_item_reward(&ctx, &tmpl0, Some(&make_agent(1, 5, 24))),
        Some((9_899, 1))
    );
    assert_eq!(
        resolve_mission_item_reward(&ctx, &tmpl0, Some(&make_agent(1, 10, 24))),
        Some((9_899, 1))
    );

    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let minted_ship = ctx.db.with_mut_snapshot(|snap| {
        mint_mission_item_reward_in_snap(
            &ctx,
            snap,
            char_id,
            station_id,
            &tmpl3,
            Some(&make_agent(1, 12, 24)),
        )
        .unwrap()
    });
    assert_eq!(minted_ship.type_id, 587);
    assert_eq!(minted_ship.quantity, 1);
    ctx.db.with_snapshot(|snap| {
        let rec = &snap.items[&minted_ship.item_id];
        assert!(rec.singleton);
        assert_eq!(rec.flag, InventoryFlag::Hangar);
    });

    emit_mission_item_reward_notification(&ctx, char_id, minted_ship);
    let notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].event, "OnItemChange");
}
