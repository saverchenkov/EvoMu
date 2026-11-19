//! Unit and mutation-killing tests for `combat_npc_encounters`.

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::InventoryFlag;
use glam::DVec3;

#[tokio::test]
async fn multi_wave_encounter_site_spawns_progresses_and_completes_on_trigger_kills() {
    let (ctx, _session, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // Empty wave list returns empty
    let empty = create_custom_encounter_site(&ctx, 1, sys_id, Vec::new(), DVec3::ZERO).await;
    assert!(empty.is_empty());

    // 2-wave site
    let wave1 = create_custom_encounter_site(
        &ctx,
        77,
        sys_id,
        vec![vec![10_001, 10_001], vec![10_002]],
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    assert_eq!(wave1.len(), 2);

    // Killing trigger ball (wave1[0]) spawns wave 2 and advances current_wave to 2
    step_encounter_wave_on_kill(&ctx, sys_id, wave1[0], DVec3::ZERO).await;
    let remaining_balls = with_combat_runtime(&ctx, |rt| {
        let enc = rt.encounter_waves.get(&77).unwrap();
        assert_eq!(enc.current_wave, 2);
        assert!(!enc.completed);
        enc.active_wave_ball_ids.clone()
    });
    assert_eq!(remaining_balls.len(), 2); // wave1[1] + wave2[0]

    // Killing remaining balls completes the encounter site
    for bid in remaining_balls {
        step_encounter_wave_on_kill(&ctx, sys_id, bid, DVec3::ZERO).await;
    }
    with_combat_runtime(&ctx, |rt| {
        let enc = rt.encounter_waves.get(&77).unwrap();
        assert!(enc.completed);
    });

    // Also test create_encounter_wave_site and handle_npc_destroyed_in_space
    let auto_wave1 =
        create_encounter_wave_site(&ctx, 88, sys_id, 10_001, 2, DVec3::ZERO).await;
    assert_eq!(auto_wave1.len(), 2);
    handle_npc_destroyed_in_space(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        sys_id,
        auto_wave1[0],
        Some(90_000_001),
    )
    .await;
}

#[test]
fn bounty_tick_threshold_tax_and_overdue_flush_mechanics() {
    let (ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = 1_750_000_000_u64;

    // 1. Empty tick returns zeroed payout
    let empty = flush_npc_bounty_tick(&ctx, sys_id, char_id, 0.0);
    assert_eq!((empty.pilot_isk, empty.main_ess_isk, empty.reserve_ess_isk), (0.0, 0.0, 0.0));

    // 2. Below 100k gross with default tax threshold -> 0% tax applied
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, char_id), 50_000.0);
        rt.pending_bounty_tick_started_unix
            .insert((sys_id, char_id), now);
    });
    // Not overdue at now + 1199
    tick_overdue_npc_bounty_payouts(&ctx, now + 1_199);
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.pending_bounty_ticks.contains_key(&(sys_id, char_id)));
    });

    // Overdue at now + 1200 -> flushes full 50,000 ISK (0% tax under 100k threshold)
    let bal_before = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance.to_wire_f64());
    tick_overdue_npc_bounty_payouts(&ctx, now + 1_200);
    let bal_after_small = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).unwrap().balance.to_wire_f64());
    assert!((bal_after_small - bal_before - 50_000.0).abs() < 1e-2);

    // 3. >= 100k gross with 10% explicit tax -> 90% to pilot, 10% tax
    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, char_id), 200_000.0);
    });
    let payout = flush_npc_bounty_tick(&ctx, sys_id, char_id, 0.10);
    assert!((payout.pilot_isk - 180_000.0).abs() < 1e-2);
    assert_eq!(payout.main_ess_isk, 0.0);
    assert_eq!(payout.reserve_ess_isk, 0.0);
}

#[tokio::test]
async fn officer_commander_mordu_sde_drops_and_finalize_cycle_destroyed_npcs() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();

    // Mordu groups 1285, 1286, 1287
    sde.types
        .insert(930_001, make_fighter_type(930_001, 1_285, 11, 1_000.0, vec![]));
    sde.types
        .insert(930_002, make_fighter_type(930_002, 1_286, 11, 1_000.0, vec![]));
    sde.types
        .insert(930_003, make_fighter_type(930_003, 1_287, 11, 1_000.0, vec![]));

    // Officer NPC (group 553) + matching Officer module (cat 7, meta_group_id 5)
    let mut off_npc = make_fighter_type(930_004, 553, 11, 5_000.0, vec![]);
    off_npc.name = "Cormack Vaaja".into();
    sde.types.insert(930_004, off_npc);
    let mut off_mod = make_fighter_type(930_005, 60, 7, 5.0, vec![]);
    off_mod.name = "Cormack's Modified Damage Control".into();
    off_mod.meta_group_id = Some(5);
    sde.types.insert(930_005, off_mod);

    // Faction commander NPC + matching Faction module (cat 7, meta_group_id 4)
    let mut fac_npc = make_fighter_type(930_006, 550, 11, 2_000.0, vec![]);
    fac_npc.name = "Dread Guristas Invader".into();
    sde.types.insert(930_006, fac_npc);
    let mut fac_mod = make_fighter_type(930_007, 39, 7, 5.0, vec![]);
    fac_mod.name = "Dread Guristas Multispectrum Shield Hardener".into();
    fac_mod.meta_group_id = Some(4);
    sde.types.insert(930_007, fac_mod);

    ctx.sde = std::sync::Arc::new(sde);

    assert_eq!(
        resolve_officer_or_commander_sde_drop(&ctx, 930_001),
        Some((33_817, true))
    );
    assert_eq!(
        resolve_officer_or_commander_sde_drop(&ctx, 930_002),
        Some((33_819, true))
    );
    assert_eq!(
        resolve_officer_or_commander_sde_drop(&ctx, 930_003),
        Some((33_821, true))
    );
    let (off_drop, off_bpc) = resolve_officer_or_commander_sde_drop(&ctx, 930_004).unwrap();
    assert!(!off_bpc && off_drop > 0);
    let (fac_drop, fac_bpc) = resolve_officer_or_commander_sde_drop(&ctx, 930_006).unwrap();
    assert!(!fac_bpc && fac_drop > 0);

    // Populate wreck on turret kill & finalize_cycle_destroyed_npcs
    let wreck_id = 940_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            make_fighter_item(wreck_id, 26_557, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let mut dropped = Vec::new();
    let arch = populate_npc_wreck_on_turret_kill(
        &ctx,
        sys_id,
        950_001,
        930_001,
        DVec3::ZERO,
        wreck_id,
        char_id,
        &mut dropped,
    )
    .await;
    assert_eq!(arch, "MorduCommander");
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].type_id, 33_817);
    assert!(dropped[0].singleton);

    // Queue a destroyed NPC in destroyed_npc_cache and finalize_cycle_destroyed_npcs
    with_mut_combat_runtime(&ctx, |rt| {
        let mut ent = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
            950_002,
            930_002,
            sys_id,
            "Sleeper",
        );
        ent.threat_table.insert(ship_id, 500.0);
        rt.destroyed_npc_cache.insert(950_002, ent);
    });
    let prev_next_id = ctx.db.with_snapshot(|snap| snap.next_entity_id);
    finalize_cycle_destroyed_npcs(&ctx, sys_id).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.destroyed_npc_cache.contains_key(&950_002));
    });
    let sleeper_wreck_id = ctx
        .db
        .with_snapshot(|snap| {
            snap.items
                .values()
                .find(|i| i.item_id > prev_next_id && i.location_id == sys_id as i64)
                .map(|i| (i.item_id, i.owner_id))
        })
        .expect("sleeper wreck item persisted");
    assert_eq!(sleeper_wreck_id.1, char_id);
    let sleeper_meta = ctx
        .systems
        .get_or_spawn(sys_id)
        .view()
        .slim_meta(sleeper_wreck_id.0)
        .cloned()
        .expect("sleeper wreck slim_meta");
    assert_eq!(sleeper_meta.group_id, 186);
    assert_eq!(sleeper_meta.category_id, 2);
    assert_eq!(sleeper_meta.owner_id, char_id);
    assert_eq!(sleeper_meta.corp_id, -1);
    assert_eq!(sleeper_meta.alliance_id, -1);

    record_npc_kill_progression(&ctx, char_id, Some(930_001));
}

#[tokio::test]
async fn populate_npc_wreck_on_turret_kill_handles_pithi_invader_and_faction_loot() {
    let (ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // Verify Pithi Invader (16997) in live SDE has faction_id == None and Guristas Small Wreck (26573).
    let pithi = ctx.sde.types.get(&16_997).expect("Pithi Invader 16997 in SDE");
    assert_eq!(pithi.faction_id, None);
    assert_eq!(ctx.sde.resolve_wreck_type_id(16_997, None), Some(26_573));

    // Negative case: wreck_id == 0 skips loot insertion even for 16997.
    let mut dropped_none = Vec::new();
    let arch_no_wreck = populate_npc_wreck_on_turret_kill(
        &ctx,
        sys_id,
        960_100,
        16_997,
        DVec3::ZERO,
        0,
        char_id,
        &mut dropped_none,
    )
    .await;
    assert_eq!(arch_no_wreck, "Pirate");
    assert!(dropped_none.is_empty());

    // Positive case: spawning/killing Pithi Invader (16997) with turret populates Guristas wreck loot
    // even if a regional npc_spawns fallback had a different faction_id (e.g. non-Caldari system).
    let wreck_id = 960_200_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            make_fighter_item(wreck_id, 26_573, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        let mut ent = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
            960_101,
            16_997,
            sys_id,
            "Pirate",
        );
        ent.faction_id = 500_012; // Wrong regional fallback must yield to Guristas group/type/wreck derivation.
        rt.npc_entities.insert(960_101, ent);
    });
    let mut dropped = Vec::new();
    let arch = populate_npc_wreck_on_turret_kill(
        &ctx,
        sys_id,
        960_101,
        16_997,
        DVec3::ZERO,
        wreck_id,
        char_id,
        &mut dropped,
    )
    .await;
    assert_eq!(arch, "Pirate");
    assert!(
        dropped.iter().any(|d| d.type_id == 15_331 && d.qty_dropped == 10),
        "Pithi Invader wreck must drop Metal Scraps (15331): {dropped:?}"
    );
    assert!(
        dropped.iter().any(|d| d.type_id == 204 && d.qty_dropped == 100),
        "Pithi Invader wreck must resolve Guristas faction ammo (204): {dropped:?}"
    );
    ctx.db.with_snapshot(|snap| {
        let wreck_items: Vec<_> = snap
            .items
            .values()
            .filter(|i| i.location_id == wreck_id)
            .collect();
        assert_eq!(wreck_items.len(), dropped.len());
        assert!(wreck_items.iter().all(|i| i.flag == InventoryFlag::Cargo && i.owner_id == char_id));
    });

    // Other pirate faction names resolve cleanly when SDE type has no explicit faction_id.
    assert_eq!(
        resolve_npc_faction_id_for_loot(&ctx, 16_997, Some(500_012), None),
        500_010
    );
    assert_eq!(
        resolve_npc_faction_id_for_loot(&ctx, 999_999_888, Some(500_011), None),
        500_011
    );

    // Negative case (F-02): empty unpiloted player ship (category_id == 6, e.g. Rifter 587)
    // with npc_opt == None must NEVER mint synthetic fallback scraps, materials, or ammo.
    let player_ship_tid = 587_u32;
    assert_eq!(ctx.sde.types[&player_ship_tid].category_id, 6);
    let player_wreck_id = 960_201_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            player_wreck_id,
            make_fighter_item(player_wreck_id, 26_557, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let mut dropped_player = Vec::new();
    let _ = populate_npc_wreck_on_turret_kill(
        &ctx,
        sys_id,
        960_102,
        player_ship_tid,
        DVec3::ZERO,
        player_wreck_id,
        char_id,
        &mut dropped_player,
    )
    .await;
    assert!(
        dropped_player.is_empty(),
        "empty unpiloted player ship wreck must not mint synthetic NPC loot: {dropped_player:?}"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.values().any(|i| i.location_id == player_wreck_id),
            "no synthetic loot items may be inserted into player ship wreck"
        );
    });
}

#[tokio::test]
async fn finalize_cycle_destroyed_npcs_spawns_owner_slim_meta_and_allows_green_safety_loot_all() {
    let sde = {
        let mut loaded = None;
        if let Some(env_path) = std::env::var_os("EVOMU_SDE_PATH").filter(|v| !v.is_empty()) {
            let p = std::path::PathBuf::from(env_path);
            if p.exists() {
                loaded = Some(Arc::new(
                    eve_sde::SdeRegistry::open(&p).expect("open EVOMU_SDE_PATH sqlite"),
                ));
            }
        }
        if loaded.is_none() {
            let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            for _ in 0..5 {
                let candidate = dir.join("data/evomu_sde.sqlite");
                if candidate.exists() {
                    loaded = Some(Arc::new(
                        eve_sde::SdeRegistry::open(&candidate).expect("open evomu_sde.sqlite"),
                    ));
                    break;
                }
                if !dir.pop() {
                    break;
                }
            }
        }
        loaded.unwrap_or_else(|| {
            let (_tmp_ctx, _, _, _, _) = make_fighter_ctx_and_session();
            let mut sde = (*_tmp_ctx.sde).clone();
            let mut base = sde.types.get(&23_915).cloned().expect("fighter ship template");
            base.capacity = 5_000.0;
            sde.types.insert(23_915, base.clone());

            let mut pithi = base.clone();
            pithi.type_id = 16_997;
            pithi.group_id = 552;
            pithi.category_id = 11;
            pithi.name = "Pithi Invader".into();
            pithi.faction_id = Some(500_010);
            pithi.race_id = Some(32);
            sde.types.insert(16_997, pithi);

            let mut wreck = base.clone();
            wreck.type_id = 26_573;
            wreck.group_id = 186;
            wreck.category_id = 2;
            wreck.name = "Guristas Small Wreck".into();
            wreck.capacity = 5_000.0;
            wreck.volume = 500.0;
            sde.types.insert(26_573, wreck);

            let mut scraps = base;
            scraps.type_id = 15_331;
            scraps.group_id = 18;
            scraps.category_id = 4;
            scraps.name = "Metal Scraps".into();
            scraps.volume = 0.01;
            sde.types.insert(15_331, scraps);

            sde.wrecks.insert(16_997, 26_573);
            Arc::new(sde)
        })
    };

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let char_id = 90_000_001i64;
    let ship_id = 200_000_001i64;
    let other_char = 90_000_999i64;
    let other_ship = 200_000_999i64;
    let sys_id = 30_000_142u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 23_915,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        let mut other_ch = snap.characters.get(&char_id).cloned().expect("seed char");
        other_ch.character_id = other_char;
        other_ch.name = "Other Pilot".into();
        other_ch.corporation_id = 1_000_010;
        other_ch.active_ship_id = other_ship;
        other_ch.solar_system_id = sys_id;
        other_ch.station_id = None;
        other_ch.structure_id = None;
        snap.characters.insert(other_char, other_ch);
        snap.items.insert(
            other_ship,
            eve_db::InventoryItemRecord {
                item_id: other_ship,
                type_id: 23_915,
                owner_id: other_char,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut owner_session = eve_machonet::SessionState::default();
    owner_session.user_id = Some(1);
    owner_session.character_id = Some(char_id);
    owner_session.corporation_id = Some(1_000_009);
    owner_session.ship_id = Some(ship_id);
    owner_session.solar_system_id = Some(sys_id);

    let mut other_session = eve_machonet::SessionState::default();
    other_session.user_id = Some(2);
    other_session.character_id = Some(other_char);
    other_session.corporation_id = Some(1_000_010);
    other_session.ship_id = Some(other_ship);
    other_session.solar_system_id = Some(sys_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (sid, _oid) in [(ship_id, char_id), (other_ship, other_char)] {
        sys_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: sid,
                    type_id: 23_915,
                    position: glam::DVec3::ZERO,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 120.0,
                    inertia: 0.5,
                    mass: 1_000_000_000.0,
                    radius: 500.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState::default(),
            )
            .await
            .unwrap();
    }

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels
            .insert(char_id, crate::services::combat_crimewatch::SafetyLevel::Green);
        rt.safety_levels
            .insert(other_char, crate::services::combat_crimewatch::SafetyLevel::Green);
        let mut pithi =
            crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
                951_001,
                16_997,
                sys_id,
                "Pirate",
            );
        pithi.faction_id = 500_010;
        pithi.spawn_position = glam::DVec3::new(1_000.0, 0.0, 0.0);
        pithi.threat_table.insert(ship_id, 500.0);
        pithi.aggression_threat.insert(ship_id, 500.0);
        rt.destroyed_npc_cache.insert(951_001, pithi);
    });

    let prev_next_id = ctx.db.with_snapshot(|snap| snap.next_entity_id);
    finalize_cycle_destroyed_npcs(&ctx, sys_id).await;

    let (wreck_id, wreck_rec) = ctx
        .db
        .with_snapshot(|snap| {
            snap.items
                .values()
                .find(|i| i.item_id > prev_next_id && i.location_id == sys_id as i64)
                .map(|i| (i.item_id, i.clone()))
        })
        .expect("Pithi Invader wreck persisted in DB");
    assert_eq!(wreck_rec.type_id, 26_573);
    assert_eq!(wreck_rec.owner_id, char_id);

    let view = sys_handle.view();
    let wreck_ball = view.ball(wreck_id).expect("wreck ball spawned");
    let wreck_meta = view.slim_meta(wreck_id).expect("wreck slim_meta populated");
    assert_eq!(wreck_meta.group_id, 186);
    assert_eq!(wreck_meta.category_id, 2);
    assert_eq!(wreck_meta.owner_id, char_id);
    assert_eq!(wreck_meta.corp_id, -1);
    assert_eq!(wreck_meta.alliance_id, -1);
    assert_eq!(wreck_meta.name.as_deref(), Some("Guristas Small Wreck"));

    let wire_slim = eve_destiny::wire::slim_item_dict(wreck_ball, wreck_meta);
    assert_eq!(wire_slim.dict_get("ownerID"), Some(&eve_proto::PyRep::Integer(char_id)));
    assert_eq!(wire_slim.dict_get("groupID"), Some(&eve_proto::PyRep::Integer(186)));
    assert_eq!(wire_slim.dict_get("categoryID"), Some(&eve_proto::PyRep::Integer(2)));
    assert_eq!(
        wire_slim.dict_get("name"),
        Some(&eve_proto::PyRep::String("Guristas Small Wreck".into()))
    );

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.destroyed_npc_cache.contains_key(&951_001));
        let lifetime = rt
            .container_lifetimes
            .get(&wreck_id)
            .expect("container lifetime registered for cycle-killed NPC wreck");
        assert_eq!(lifetime.owner_id, char_id);
        assert_eq!(lifetime.type_id, 26_573);
        assert!(!lifetime.is_abandoned);
    });

    let mut loot_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == wreck_id)
            .map(|i| i.item_id)
            .collect()
    });
    if loot_ids.is_empty() {
        let fallback_loot_id = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let lid = snap.next_entity_id;
            snap.items.insert(
                lid,
                eve_db::InventoryItemRecord {
                    item_id: lid,
                    type_id: 15_331,
                    owner_id: char_id,
                    location_id: wreck_id,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            lid
        });
        loot_ids.push(fallback_loot_id);
    }

    let kwargs = std::collections::HashMap::new();
    // Foreign pilot with Green safety is denied looting owned NPC wreck.
    let foreign_err = crate::services::combat_crimewatch::handle_wrecks_killmails(
        &ctx,
        &mut other_session,
        "invbroker",
        "LootAll",
        &[eve_proto::PyRep::Integer(wreck_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(foreign_err.code, "CantDoInCurrentSafetyLevel");

    // Owner/killer pilot with Green safety loots cleanly without suspect flag.
    let owner_loot_res = crate::services::combat_crimewatch::handle_wrecks_killmails(
        &ctx,
        &mut owner_session,
        "invbroker",
        "LootAll",
        &[eve_proto::PyRep::Integer(wreck_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        owner_loot_res.dict_get("suspectFlagTriggered"),
        Some(&eve_proto::PyRep::Bool(false))
    );
    let Some(eve_proto::PyRep::List(moved_ids)) = owner_loot_res.dict_get("lootedItemIDs") else {
        panic!("expected lootedItemIDs list in LootAll response");
    };
    assert_eq!(moved_ids.len(), loot_ids.len());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.values().any(|i| i.location_id == wreck_id));
        for lid in &loot_ids {
            assert_eq!(
                snap.items.get(lid).map(|i| (i.location_id, i.flag)),
                Some((ship_id, eve_db::InventoryFlag::Cargo))
            );
        }
    });

    // Unknown killer (killer_char_id == 0) keeps DB owner_id == 0, maps wire ownerID -> 1, and marks abandoned.
    with_mut_combat_runtime(&ctx, |rt| {
        let mut pithi_unknown =
            crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
                951_002,
                16_997,
                sys_id,
                "Pirate",
            );
        pithi_unknown.faction_id = 500_010;
        pithi_unknown.spawn_position = glam::DVec3::new(1_200.0, 0.0, 0.0);
        rt.destroyed_npc_cache.insert(951_002, pithi_unknown);
    });
    let prev_unknown_id = ctx.db.with_snapshot(|snap| snap.next_entity_id);
    finalize_cycle_destroyed_npcs(&ctx, sys_id).await;

    let (unknown_wreck_id, unknown_rec) = ctx
        .db
        .with_snapshot(|snap| {
            snap.items
                .values()
                .find(|i| i.item_id > prev_unknown_id && i.location_id == sys_id as i64)
                .map(|i| (i.item_id, i.clone()))
        })
        .expect("unknown-killer wreck persisted");
    assert_eq!(unknown_rec.owner_id, 0);

    let view2 = sys_handle.view();
    let unknown_ball = view2.ball(unknown_wreck_id).expect("unknown wreck ball");
    let unknown_meta = view2
        .slim_meta(unknown_wreck_id)
        .expect("unknown wreck slim_meta");
    assert_eq!(unknown_meta.group_id, 186);
    assert_eq!(unknown_meta.category_id, 2);
    assert_eq!(unknown_meta.owner_id, 0);
    assert_eq!(unknown_meta.corp_id, -1);
    assert_eq!(unknown_meta.alliance_id, -1);
    let unknown_wire = eve_destiny::wire::slim_item_dict(unknown_ball, unknown_meta);
    assert_eq!(unknown_wire.dict_get("ownerID"), Some(&eve_proto::PyRep::Integer(1)));

    with_combat_runtime(&ctx, |rt| {
        let lifetime = rt
            .container_lifetimes
            .get(&unknown_wreck_id)
            .expect("abandoned lifetime registered");
        assert_eq!(lifetime.owner_id, 0);
        assert!(lifetime.is_abandoned);
    });

    // Ensure at least one item in unknown_wreck_id and confirm Green-safety pilot can loot abandoned wreck cleanly.
    let unknown_has_loot = ctx
        .db
        .with_snapshot(|snap| snap.items.values().any(|i| i.location_id == unknown_wreck_id));
    if !unknown_has_loot {
        ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let lid = snap.next_entity_id;
            snap.items.insert(
                lid,
                eve_db::InventoryItemRecord {
                    item_id: lid,
                    type_id: 15_331,
                    owner_id: 0,
                    location_id: unknown_wreck_id,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
    }
    let abandoned_loot_res = crate::services::combat_crimewatch::handle_wrecks_killmails(
        &ctx,
        &mut other_session,
        "invbroker",
        "LootAll",
        &[eve_proto::PyRep::Integer(unknown_wreck_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        abandoned_loot_res.dict_get("suspectFlagTriggered"),
        Some(&eve_proto::PyRep::Bool(false))
    );
}
