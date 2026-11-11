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

fn extract_keyval(rep: &PyRep) -> std::collections::HashMap<&str, &PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().map(|(k, v)| (k.as_str(), v)).collect(),
        other => panic!("expected PyRep::KeyVal, got {other:?}"),
    }
}

#[test]
fn resolve_mission_item_reward_covers_all_divisions_modulo_and_sde_fallbacks() {
    let mut sde = SdeRegistry::with_canonical_seed();
    // Verify Career div 19 all 4 modulo indices
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde.clone()),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let div19 = make_agent(1, 12, 19);
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(0), Some(&div19)), Some((30_013, 8)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(1), Some(&div19)), Some((30_832, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(2), Some(&div19)), Some((30_834, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(3), Some(&div19)), Some((605, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(4), Some(&div19)), Some((30_013, 8)));

    // Divisions 22, 23, 25 share mining/hauler candidate table
    for div in [22, 23, 25] {
        let ag = make_agent(2, 12, div);
        assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(0), Some(&ag)), Some((483, 1)));
        assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(1), Some(&ag)), Some((439, 1)));
        assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(2), Some(&ag)), Some((32_880, 1)));
        assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(3), Some(&ag)), Some((650, 1)));
    }

    // Default division on Career agent
    let div_def = make_agent(3, 12, 99);
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(0), Some(&div_def)), Some((439, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(1), Some(&div_def)), Some((561, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(2), Some(&div_def)), Some((222, 100)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(3), Some(&div_def)), Some((587, 1)));

    // Storyline (type 5) all 5 slots + wraparound
    let story = make_agent(4, 5, 24);
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(0), Some(&story)), Some((9_899, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(1), Some(&story)), Some((9_941, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(2), Some(&story)), Some((9_942, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(3), Some(&story)), Some((9_943, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(4), Some(&story)), Some((9_950, 1)));
    assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(5), Some(&story)), Some((9_899, 1)));

    // COSMOS (8) and Epic Arc (10)
    for t in [8, 10] {
        let ag = make_agent(5, t, 1);
        assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(0), Some(&ag)), Some((9_899, 1)));
        assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(1), Some(&ag)), Some((439, 1)));
        assert_eq!(resolve_mission_item_reward(&ctx, &make_mission(2), Some(&ag)), Some((9_899, 1)));
    }

    // Missing primary SDE type skips cleanly to next present candidate; all missing returns None.
    sde.types.remove(&30_013);
    let ctx_partial = ServerContext::with_sde_and_repository(
        Arc::new(sde.clone()),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    assert_eq!(
        resolve_mission_item_reward(&ctx_partial, &make_mission(0), Some(&div19)),
        Some((30_832, 1))
    );
    for tid in [30_832, 30_834, 605] {
        sde.types.remove(&tid);
    }
    let ctx_empty = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    assert!(resolve_mission_item_reward(&ctx_empty, &make_mission(0), Some(&div19)).is_none());
}

#[test]
fn mint_and_notify_distinguish_ships_stacks_missing_rewards_and_unknown_sde_types() {
    let ctx = make_test_context();
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    ctx.db
        .acquire_online_lock(1, 502, Some(char_id), true, 1_775_400_000)
        .unwrap();

    // Unrewarded agent type must not allocate entity IDs or insert items.
    let before_next = ctx.db.with_snapshot(|s| s.next_entity_id);
    let before_count = ctx.db.with_snapshot(|s| s.items.len());
    ctx.db.with_mut_snapshot(|snap| {
        assert!(
            mint_mission_item_reward_in_snap(&ctx, snap, char_id, station_id, &make_mission(0), None)
                .is_none()
        );
        assert!(mint_mission_item_reward_in_snap(
            &ctx,
            snap,
            char_id,
            station_id,
            &make_mission(0),
            Some(&make_agent(1, 2, 24)),
        )
        .is_none());
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.next_entity_id), before_next);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.len()), before_count);

    // Non-ship stack reward: Career div 19 mission 0 -> Core Scanner Probe I x8 (singleton == false).
    let stack = ctx.db.with_mut_snapshot(|snap| {
        mint_mission_item_reward_in_snap(
            &ctx,
            snap,
            char_id,
            station_id,
            &make_mission(0),
            Some(&make_agent(1, 12, 19)),
        )
        .expect("stack minted")
    });
    assert_eq!(stack.item_id, before_next + 1);
    assert_eq!(stack.type_id, 30_013);
    assert_eq!(stack.quantity, 8);
    assert_eq!(stack.station_id, station_id);
    ctx.db.with_snapshot(|snap| {
        let rec = &snap.items[&stack.item_id];
        assert_eq!(rec.type_id, 30_013);
        assert_eq!(rec.owner_id, char_id);
        assert_eq!(rec.location_id, station_id);
        assert_eq!(rec.quantity, 8);
        assert!(!rec.singleton);
        assert_eq!(rec.flag, InventoryFlag::Hangar);
    });

    // Ship reward: Rifter (587, category 6) emits quantity == -1 and singleton == true.
    let ship = ctx.db.with_mut_snapshot(|snap| {
        mint_mission_item_reward_in_snap(
            &ctx,
            snap,
            char_id,
            station_id,
            &make_mission(3),
            Some(&make_agent(2, 12, 24)),
        )
        .expect("ship minted")
    });
    let expected_ship_group = i64::from(ctx.sde.types[&587].group_id);
    let expected_probe_group = i64::from(ctx.sde.types[&30_013].group_id);
    let expected_probe_cat = i64::from(ctx.sde.types[&30_013].category_id);

    emit_mission_item_reward_notification(&ctx, char_id, stack);
    emit_mission_item_reward_notification(&ctx, char_id, ship);
    emit_mission_item_reward_notification(
        &ctx,
        char_id,
        MintedMissionItemReward {
            item_id: 9_999_888,
            type_id: 999_999_999,
            quantity: 7,
            station_id,
        },
    );

    let notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes.len(), 3);
    for note in &notes {
        assert_eq!(note.event, "OnItemChange");
        assert_eq!(note.id_type, "charid");
        assert_eq!(note.args.len(), 2);
        assert_eq!(note.args[1], PyRep::Dict(Vec::new()));
    }

    let kv_stack = extract_keyval(&notes[0].args[0]);
    assert_eq!(kv_stack["itemID"], &PyRep::Integer(stack.item_id));
    assert_eq!(kv_stack["typeID"], &PyRep::Integer(30_013));
    assert_eq!(kv_stack["ownerID"], &PyRep::Integer(char_id));
    assert_eq!(kv_stack["locationID"], &PyRep::Integer(station_id));
    assert_eq!(kv_stack["flagID"], &PyRep::Integer(InventoryFlag::Hangar as i64));
    assert_eq!(kv_stack["quantity"], &PyRep::Integer(8));
    assert_eq!(kv_stack["groupID"], &PyRep::Integer(expected_probe_group));
    assert_eq!(kv_stack["categoryID"], &PyRep::Integer(expected_probe_cat));
    assert_eq!(kv_stack["singleton"], &PyRep::Bool(false));

    let kv_ship = extract_keyval(&notes[1].args[0]);
    assert_eq!(kv_ship["itemID"], &PyRep::Integer(ship.item_id));
    assert_eq!(kv_ship["typeID"], &PyRep::Integer(587));
    assert_eq!(kv_ship["quantity"], &PyRep::Integer(-1));
    assert_eq!(kv_ship["groupID"], &PyRep::Integer(expected_ship_group));
    assert_eq!(kv_ship["categoryID"], &PyRep::Integer(6));
    assert_eq!(kv_ship["singleton"], &PyRep::Bool(true));

    let kv_unknown = extract_keyval(&notes[2].args[0]);
    assert_eq!(kv_unknown["itemID"], &PyRep::Integer(9_999_888));
    assert_eq!(kv_unknown["quantity"], &PyRep::Integer(7));
    assert_eq!(kv_unknown["groupID"], &PyRep::Integer(0));
    assert_eq!(kv_unknown["categoryID"], &PyRep::Integer(0));
    assert_eq!(kv_unknown["singleton"], &PyRep::Bool(false));
}

