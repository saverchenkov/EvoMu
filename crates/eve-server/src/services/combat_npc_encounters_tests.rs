//! Unit and mutation-killing tests for `combat_npc_encounters`.

use super::*;
use crate::combat::npc_ai::npc_test_helpers::seed_npc_entity;
use crate::services::gm_soak_commands::combat_ephemeral_test_helpers::get_container_lifetime;
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
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .pending_bounty_ticks
            .insert((sys_id, char_id), 50_000.0);
        snap.combat_crimewatch_state
            .pending_bounty_tick_started_unix
            .insert((sys_id, char_id), now);
    });
    // Not overdue at now + 1199
    tick_overdue_npc_bounty_payouts(&ctx, now + 1_199);
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .combat_crimewatch_state
            .pending_bounty_ticks
            .contains_key(&(sys_id, char_id)));
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
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .pending_bounty_ticks
            .insert((sys_id, char_id), 200_000.0);
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
    let mut ent = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
        960_101,
        16_997,
        sys_id,
        "Pirate",
    );
    ent.faction_id = 500_012; // Wrong regional fallback must yield to Guristas group/type/wreck derivation.
    seed_npc_entity(&ctx, ent);
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

    ctx.db.with_mut_snapshot(|snap| {
        crate::services::combat_crimewatch::set_char_safety_level_in_snap(
            snap,
            char_id,
            crate::services::combat_crimewatch::SafetyLevel::Green,
        );
        crate::services::combat_crimewatch::set_char_safety_level_in_snap(
            snap,
            other_char,
            crate::services::combat_crimewatch::SafetyLevel::Green,
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
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
    });
    let lifetime = get_container_lifetime(&ctx, wreck_id)
        .expect("container lifetime registered for cycle-killed NPC wreck");
    assert_eq!(lifetime.owner_id, char_id);
    assert_eq!(lifetime.type_id, 26_573);
    assert!(!lifetime.is_abandoned);

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

    let lifetime =
        get_container_lifetime(&ctx, unknown_wreck_id).expect("abandoned lifetime registered");
    assert_eq!(lifetime.owner_id, 0);
    assert!(lifetime.is_abandoned);

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

#[tokio::test]
async fn npc_destruction_paths_apply_faction_standing_loss_ticks_and_emit_client_socket_notifications() {
    // parity: feat:standings.standing-loss, feat:standings.npc-kill-standing-loss, plan:PVE-3
    let (mut ctx, _session, char_id, _ship_id, sys_a) = make_fighter_ctx_and_session();
    let sys_b: u32 = 30_000_144;
    let sys_c: u32 = 30_000_145;
    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(1, 701, Some(char_id), true, now)
        .unwrap();

    // Ensure Caldari Navy Raven (12391), Minmatar Sentry Gun (3743), and Pithi Invader (16997)
    // exist in SDE with authentic attribute 562 (`entityFactionLoss`) values.
    let mut sde = (*ctx.sde).clone();
    sde.groups.entry(674).or_insert(eve_sde::GroupInfo {
        group_id: 674,
        category_id: 11,
        name: "Mission Caldari State Battleship".into(),
        anchorable: false,
        anchored: false,
        fittable_non_singleton: false,
        use_base_price: false,
        published: true,
    });
    sde.groups.entry(99).or_insert(eve_sde::GroupInfo {
        group_id: 99,
        category_id: 11,
        name: "Sentry Gun".into(),
        anchorable: false,
        anchored: false,
        fittable_non_singleton: false,
        use_base_price: false,
        published: true,
    });
    sde.groups.entry(615).or_insert(eve_sde::GroupInfo {
        group_id: 615,
        category_id: 11,
        name: "Deadspace Guristas Frigate".into(),
        anchorable: false,
        anchored: false,
        fittable_non_singleton: false,
        use_base_price: false,
        published: true,
    });
    let mut navy_bs = make_fighter_type(12_391, 674, 11, 10_000.0, vec![(562, 0.03)]);
    navy_bs.name = "Caldari Navy Raven".into();
    sde.types.insert(12_391, navy_bs);
    let mut sentry = make_fighter_type(3_743, 99, 11, 5_000.0, vec![]);
    sentry.name = "Minmatar Sentry Gun".into();
    sde.types.insert(3_743, sentry);
    let mut pithi = make_fighter_type(16_997, 615, 11, 2_000.0, vec![(481, 50_000.0), (562, 0.00175)]);
    pithi.name = "Pithi Invader".into();
    sde.types.insert(16_997, pithi);
    ctx.sde = Arc::new(sde);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.clear();
        ch.security_status = 0.0;
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let mut notify_rx = ctx.notify_tx.subscribe();

    // Path 1: `NpcSpawnManager::handle_npc_destroyed` (combat/npc_ai.rs)
    // 3 Caldari Navy kills in ONE system inside 10 minutes -> standings change ONCE, 1 client notification pair.
    let mut mgr = crate::combat::npc_ai::NpcSpawnManager::new();
    for (idx, bid) in [970_001_i64, 970_002, 970_003].into_iter().enumerate() {
        mgr.entities.insert(
            bid,
            crate::combat::npc_ai::NpcCombatEntity {
                ball_id: bid,
                type_id: 12_391,
                faction_id: 500_001,
                state: crate::combat::npc_ai::NpcAiState::Idle,
                spawn_group_id: 1,
                spawn_pos: DVec3::ZERO,
                target_id: None,
                bounty_isk: 0.0,
                optimal_range: 10_000.0,
                orbit_range: 5_000.0,
                base_dps: 50.0,
                damage_vector: eve_destiny::DamageVector::default(),
                last_attack_tick: 0,
                has_shield_rep: false,
                has_armor_rep: false,
            },
        );
        mgr.handle_npc_destroyed(&ctx, sys_a, bid, char_id, now + (idx as u64) * 60)
            .expect("handle_npc_destroyed navy npc");
    }
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!((st[&500_001] - (-0.03)).abs() < 1e-9, "3 kills in 1 system inside 10m count once");
        assert!((st[&500_003] - (-0.015)).abs() < 1e-9, "Caldari ally Amarr drops once");
        assert_eq!(st.get(&500_004).copied().unwrap_or(0.0), 0.0, "Caldari enemy Gallente unchanged");
    });
    let mut rx_set = 0usize;
    let mut rx_mod = 0usize;
    while let Ok((cid, notif)) = notify_rx.try_recv() {
        if cid == char_id {
            if notif.event == "OnStandingSet" {
                rx_set += 1;
            } else if notif.event == "OnStandingsModified" {
                rx_mod += 1;
            }
        }
    }
    assert_eq!((rx_set, rx_mod), (1, 1), "live socket broadcast must carry exactly 1 OnStandingSet + 1 OnStandingsModified");
    let _ = ctx.drain_notifications_for_char(char_id);

    // Same 3 kills across 3 systems inside 10 minutes -> standings change 3 times!
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });
    let t_3sys = now + 1_200;
    for (idx, (sid, bid)) in [(sys_a, 970_011_i64), (sys_b, 970_012), (sys_c, 970_013)]
        .into_iter()
        .enumerate()
    {
        mgr.entities.insert(
            bid,
            crate::combat::npc_ai::NpcCombatEntity {
                ball_id: bid,
                type_id: 12_391,
                faction_id: 500_001,
                state: crate::combat::npc_ai::NpcAiState::Idle,
                spawn_group_id: 2,
                spawn_pos: DVec3::ZERO,
                target_id: None,
                bounty_isk: 0.0,
                optimal_range: 10_000.0,
                orbit_range: 5_000.0,
                base_dps: 50.0,
                damage_vector: eve_destiny::DamageVector::default(),
                last_attack_tick: 0,
                has_shield_rep: false,
                has_armor_rep: false,
            },
        );
        mgr.handle_npc_destroyed(&ctx, sid, bid, char_id, t_3sys + (idx as u64) * 30)
            .expect("handle_npc_destroyed across 3 systems");
    }
    let expected_3x = -10.0 * (1.0 - (1.0 - 0.003_f64).powi(3));
    let actual_3x = ctx.db.with_snapshot(|s| s.characters[&char_id].standings[&500_001]);
    assert!((actual_3x - expected_3x).abs() < 1e-9, "3 systems inside 10m must apply 3x");
    let mut rx3_set = 0usize;
    let mut rx3_mod = 0usize;
    while let Ok((cid, notif)) = notify_rx.try_recv() {
        if cid == char_id {
            if notif.event == "OnStandingSet" {
                rx3_set += 1;
            } else if notif.event == "OnStandingsModified" {
                rx3_mod += 1;
            }
        }
    }
    assert_eq!((rx3_set, rx3_mod), (3, 3), "3 system ticks must push 3 standing notification pairs to socket");
    let _ = ctx.drain_notifications_for_char(char_id);

    // Path 2: `handle_npc_destroyed_in_space_with_ctx` (combat_npc_encounters.rs) on a pirate rat
    // preserves security status gain + bounty tick AND applies Guristas standing loss + notifications,
    // while subsequent `populate_npc_wreck_on_turret_kill` in the same 10-minute window deduplicates.
    ctx.advance_secs(2_000);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.clear();
        ch.security_status = 0.0;
    });
    let mut ent = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
        970_101,
        16_997,
        sys_a,
        "Pirate",
    );
    ent.faction_id = 500_010;
    ent.bounty_isk = 50_000.0;
    seed_npc_entity(&ctx, ent);
    handle_npc_destroyed_in_space_with_ctx(&ctx, sys_a, 970_101, Some(char_id)).await;
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert!(ch.security_status > 0.0, "pirate kill sec status gain must remain intact");
        assert!((ch.standings[&500_010] - (-0.00175)).abs() < 1e-9, "Guristas standing loss applied");
        assert!(ch.standings[&500_012] < 0.0, "Guristas ally Blood Raiders derived loss");
    });
    let notes_in_space = ctx.drain_notifications_for_char(char_id);
    assert!(notes_in_space.iter().any(|n| n.event == "OnStandingSet"));
    assert!(notes_in_space.iter().any(|n| n.event == "OnStandingsModified"));
    while notify_rx.try_recv().is_ok() {}

    // Subsequent turret wreck finalization for the same kill in the same 10-minute window must not double-apply.
    let mut dropped = Vec::new();
    let _ = populate_npc_wreck_on_turret_kill(&ctx, sys_a, 970_101, 16_997, DVec3::ZERO, 0, char_id, &mut dropped).await;
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    let guristas_after = ctx.db.with_snapshot(|s| s.characters[&char_id].standings[&500_010]);
    assert!((guristas_after - (-0.00175)).abs() < 1e-9);

    // Path 3: `death_pipeline::on_ball_destroyed_sync` on BOTH an ExtendedNpcEntity (step 2)
    // and an NPC Sentry Gun structure stored in `snap.items` (step 5).
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        snap.items.insert(
            970_202,
            make_fighter_item(970_202, 3_743, 500_002, sys_b as i64, InventoryFlag::None, 1),
        );
    });
    let mut ent = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
        970_201,
        12_391,
        sys_b,
        "Navy",
    );
    ent.faction_id = 500_001;
    seed_npc_entity(&ctx, ent);
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(&ctx, sys_b, 970_201, Some(char_id), DVec3::ZERO)
        .expect("on_ball_destroyed_sync ext npc");
    let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(&ctx, sys_b, 970_202, Some(char_id), DVec3::ZERO)
        .expect("on_ball_destroyed_sync npc sentry structure");
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!((st[&500_001] - (-0.03)).abs() < 1e-9, "death_pipeline ext navy NPC loss");
        assert!((st[&500_002] - (-0.01)).abs() < 1e-9, "death_pipeline sentry structure loss");
    });
    let dp_notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(dp_notes.iter().filter(|n| n.event == "OnStandingSet").count(), 2);
    assert_eq!(dp_notes.iter().filter(|n| n.event == "OnStandingsModified").count(), 2);
    while notify_rx.try_recv().is_ok() {}

    // Path 4: Live `npc_death_hook` (router/context.rs) -> `handle_npc_destroyed_in_space_with_ctx`
    // emits live `OnStandingSet` + `OnStandingsModified` on `notify_tx` without waiting for wreck sweep.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });
    let mut ent = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
        970_301,
        12_391,
        sys_c,
        "Navy",
    );
    ent.faction_id = 500_001;
    seed_npc_entity(&ctx, ent);
    ctx.systems
        .invoke_npc_death_hook(sys_c, 970_301, Some(char_id))
        .await;
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!((st[&500_001] - (-0.03)).abs() < 1e-9, "live npc_death_hook must lower Caldari standing");
        assert!((st[&500_003] - (-0.015)).abs() < 1e-9, "live npc_death_hook must lower ally Amarr");
    });
    let mut hook_events = Vec::new();
    while let Ok((cid, notif)) = notify_rx.try_recv() {
        if cid == char_id {
            hook_events.push(notif.event);
        }
    }
    assert!(hook_events.contains(&"OnStandingSet".to_string()));
    assert!(hook_events.contains(&"OnStandingsModified".to_string()));
    let _ = ctx.drain_notifications_for_char(char_id);

    // Path 5: Direct turret/entropic `handle_ship_or_npc_destruction` on a `category_id == 11` NPC
    // structure stored in `snap.items` (where `is_non_ship_structure_or_deployable == true`).
    let sentry_ball_id = 970_302_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            sentry_ball_id,
            make_fighter_item(sentry_ball_id, 3_743, 500_002, sys_c as i64, InventoryFlag::None, 1),
        );
    });
    let sentry_ball = eve_destiny::Ball {
        id: sentry_ball_id,
        type_id: 3_743,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let sys_c_handle = ctx.systems.get_or_spawn(sys_c);
    let _ = crate::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
        &ctx,
        &sys_c_handle,
        char_id,
        _ship_id,
        587,
        2905,
        sentry_ball_id,
        &sentry_ball,
        None,
        sys_c,
        1_000.0,
        ctx.now_unix(),
    )
    .await;
    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!((st[&500_002] - (-0.01)).abs() < 1e-9, "handle_ship_or_npc_destruction NPC structure loss");
    });
    let spool_notes = ctx.drain_notifications_for_char(char_id);
    assert!(spool_notes.iter().any(|n| n.event == "OnStandingSet"));
    assert!(spool_notes.iter().any(|n| n.event == "OnStandingsModified"));

    // Path 5b (F-1): Missile launcher kill via `activate_missile_or_breacher_launcher` ->
    // `step_missiles_and_breachers` -> `handle_destroyed_ball_from_missile_or_aoe` ->
    // `destroy_specialized_npc_with_hint`, plus non-double-apply check.
    {
        let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
        let seed_ship = sde.types.get(&587).expect("seed Rifter").clone();
        sde.groups.insert(
            674,
            eve_sde::GroupInfo {
                group_id: 674,
                category_id: 11,
                name: "Mission Caldari State Battleship".to_string(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: true,
            },
        );
        let mut bs_type = seed_ship.clone();
        bs_type.type_id = 12_391;
        bs_type.group_id = 674;
        bs_type.category_id = 11;
        bs_type.name = "Caldari Navy Raven".to_string();
        bs_type.faction_id = Some(500_001);
        bs_type.default_attributes.insert(562, 0.03);
        bs_type.default_attributes.insert(263, 1.0);
        bs_type.default_attributes.insert(265, 1.0);
        bs_type.default_attributes.insert(9, 1.0);
        sde.types.insert(12_391, bs_type);

        let mut launcher_type = seed_ship.clone();
        launcher_type.type_id = 2_410;
        launcher_type.group_id = 509;
        launcher_type.category_id = 7;
        launcher_type.name = "Light Missile Launcher I".to_string();
        launcher_type.default_attributes.insert(51, 2_000.0);
        launcher_type.default_attributes.insert(6, 1.0);
        sde.types.insert(2_410, launcher_type);

        let mut missile_type = seed_ship.clone();
        missile_type.type_id = 209;
        missile_type.group_id = 384;
        missile_type.category_id = 8;
        missile_type.name = "Scourge Light Missile".to_string();
        missile_type.volume = 0.015;
        missile_type.default_attributes.insert(37, 3_750.0);
        missile_type.default_attributes.insert(281, 6_000.0);
        missile_type.default_attributes.insert(9, 75.0);
        missile_type.default_attributes.insert(653, 170.0);
        missile_type.default_attributes.insert(654, 40.0);
        missile_type.default_attributes.insert(1353, 0.604);
        sde.types.insert(209, missile_type.clone());

        let mut wreck_type = seed_ship;
        wreck_type.type_id = 26_473;
        wreck_type.group_id = 186;
        wreck_type.category_id = 2;
        wreck_type.name = "Caldari Battleship Wreck".to_string();
        sde.types.insert(26_473, wreck_type);

        let m_ctx = ServerContext::with_sde_and_repository(
            Arc::new(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        );
        let m_sys = 30_000_142_u32;
        let m_char = 98_200_001_i64;
        let m_ship = 98_200_101_i64;
        let m_launcher = 98_200_201_i64;
        let m_charge = 98_200_301_i64;
        let m_npc_ball = 98_200_401_i64;

        m_ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(
                m_char,
                eve_db::CharacterRecord {
                    character_id: m_char,
                    user_id: 1,
                    name: "Missile Pilot".to_string(),
                    corporation_id: 1_000_044,
                    solar_system_id: m_sys,
                    active_ship_id: m_ship,
                    ..Default::default()
                },
            );
            snap.items.insert(
                m_ship,
                eve_db::InventoryItemRecord {
                    item_id: m_ship,
                    type_id: 587,
                    owner_id: m_char,
                    location_id: m_sys as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                m_launcher,
                eve_db::InventoryItemRecord {
                    item_id: m_launcher,
                    type_id: 2_410,
                    owner_id: m_char,
                    location_id: m_ship,
                    flag: eve_db::InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                m_charge,
                eve_db::InventoryItemRecord {
                    item_id: m_charge,
                    type_id: 209,
                    owner_id: m_char,
                    location_id: m_ship,
                    flag: eve_db::InventoryFlag::HiSlot0,
                    quantity: 20,
                    singleton: false,
                    ..Default::default()
                },
            );
        });
        m_ctx
            .db
            .acquire_online_lock(1, 98_201, Some(m_char), true, m_ctx.now_unix())
            .expect("missile pilot online lock");

        let m_sys_handle = m_ctx.systems.get_or_spawn(m_sys);
        let attacker_ball = eve_destiny::Ball {
            id: m_ship,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 400.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        let _ = m_sys_handle.try_spawn_ball_with_combat(
            attacker_ball.clone(),
            eve_destiny::CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                capacitor: 500.0,
                max_capacitor: 500.0,
                ..Default::default()
            },
        );
        let npc_ball = eve_destiny::Ball {
            id: m_npc_ball,
            type_id: 12_391,
            position: glam::DVec3::new(1_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 2.0,
            mass: 50_000_000.0,
            radius: 400.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        let _ = m_sys_handle.try_spawn_ball_with_combat(
            npc_ball,
            eve_destiny::CombatState {
                shield_hp: 0.0,
                max_shield_hp: 10.0,
                armor_hp: 0.0,
                max_armor_hp: 10.0,
                structure_hp: 1.0,
                max_structure_hp: 10.0,
                signature_radius: 400.0,
                ..Default::default()
            },
        );
        let _ = m_sys_handle
            .add_target_lock(
                m_ship,
                eve_destiny::TargetLock {
                    target_id: m_npc_ball,
                    lock_time_ms: 100.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(100_000.0),
                },
            )
            .await;

        let fitted_launcher = eve_dogma::FittedModule {
            item_id: m_launcher,
            type_id: 2_410,
            flag_id: 27,
            state: eve_dogma::ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: Some(209),
            charge_quantity: 20,
            dynamic_attribute_overrides: std::collections::HashMap::new(),
        };
        let mut fit = eve_dogma::ShipFittingEngine::from_sde(m_ship, 587, &m_ctx.sde);
        fit.modules.insert(m_launcher, fitted_launcher.clone());

        // Notice `m_npc_ball` is intentionally NOT seeded into NPC entity runtime tracking so `npc_death_hook`
        // is a no-op and `handle_destroyed_ball_from_missile_or_aoe -> destroy_specialized_npc_with_hint`
        // performs the standing loss directly.
        crate::services::combat_crimewatch::activate_missile_or_breacher_launcher(
            &m_ctx,
            m_char,
            m_ship,
            m_sys,
            m_ctx.now_unix(),
            m_launcher,
            &fitted_launcher,
            &fit,
            209,
            &missile_type,
            &attacker_ball,
            m_npc_ball,
            eve_destiny::DamageVector::new(0.0, 0.0, 180.0, 0.0),
            false,
            false,
            true,
        )
        .await
        .expect("missile launcher activation succeeds");
        let _ = crate::services::combat_crimewatch::step_missiles_and_breachers(&m_ctx, m_sys, 2.0).await;

        m_ctx.db.with_snapshot(|snap| {
            let st = snap.characters[&m_char].standings.get(&500_001).copied().unwrap_or(0.0);
            assert!(
                (st - (-0.03)).abs() < 1e-9,
                "missile kill via destroy_specialized_npc_with_hint must apply -0.03 standing loss, got {st}"
            );
        });
        let m_notes = m_ctx.drain_notifications_for_char(m_char);
        assert!(m_notes.iter().any(|n| n.event == "OnStandingSet"));
        assert!(m_notes.iter().any(|n| n.event == "OnStandingsModified"));

        // Re-invoking `destroy_specialized_npc_with_hint` for the same tick/NPC (simulating when `npc_death_hook`
        // fired just before `handle_destroyed_ball_from_missile_or_aoe`) must not double-apply standing loss.
        crate::services::combat_crimewatch::destroy_specialized_npc_with_hint(
            &m_ctx,
            m_sys,
            m_npc_ball,
            m_char,
            Some((12_391, glam::DVec3::new(1_000.0, 0.0, 0.0))),
        )
        .await;
        m_ctx.db.with_snapshot(|snap| {
            let st = snap.characters[&m_char].standings.get(&500_001).copied().unwrap_or(0.0);
            assert!((st - (-0.03)).abs() < 1e-9, "same-tick re-entry must not double-apply, got {st}");
        });
        let re_notes = m_ctx.drain_notifications_for_char(m_char);
        assert!(
            !re_notes
                .iter()
                .any(|n| matches!(n.event.as_str(), "OnStandingSet" | "OnStandingsModified")),
            "same-tick re-entry must emit zero duplicate standing notifications: {re_notes:?}"
        );
    }

    // Path 5c (F-1): Combat drone `CmdEngage` -> `destroy_specialized_npc_with_hint` + drone ball_id killer resolution.
    {
        let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
        let seed_ship = sde.types.get(&587).expect("seed Rifter").clone();
        sde.groups.insert(
            674,
            eve_sde::GroupInfo {
                group_id: 674,
                category_id: 11,
                name: "Mission Caldari State Battleship".to_string(),
                anchorable: false,
                anchored: false,
                fittable_non_singleton: false,
                use_base_price: false,
                published: true,
            },
        );
        let mut bs_type = seed_ship.clone();
        bs_type.type_id = 12_391;
        bs_type.group_id = 674;
        bs_type.category_id = 11;
        bs_type.name = "Caldari Navy Raven".to_string();
        bs_type.faction_id = Some(500_001);
        bs_type.default_attributes.insert(562, 0.03);
        sde.types.insert(12_391, bs_type);

        let mut drone_type = seed_ship;
        drone_type.type_id = 2_454;
        drone_type.group_id = 100;
        drone_type.category_id = 18;
        drone_type.name = "Hobgoblin I".to_string();
        drone_type.volume = 5.0;
        sde.types.insert(2_454, drone_type);

        let d_ctx = ServerContext::with_sde_and_repository(
            Arc::new(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        );
        let d_sys = 30_000_142_u32;
        let d_char = 98_300_001_i64;
        let d_ship = 98_300_101_i64;
        let d_drone = 98_300_201_i64;
        let d_npc = 98_300_301_i64;
        let d_char2 = 98_300_002_i64;
        let d_ship2 = 98_300_102_i64;
        let d_drone2 = 98_300_202_i64;
        let d_npc2 = 98_300_302_i64;

        d_ctx.db.with_mut_snapshot(|snap| {
            for (cid, uid, sid, name) in [
                (d_char, 1_u64, d_ship, "Drone Pilot"),
                (d_char2, 98_302_u64, d_ship2, "Drone Hint Pilot"),
            ] {
                snap.characters.insert(
                    cid,
                    eve_db::CharacterRecord {
                        character_id: cid,
                        user_id: uid,
                        name: name.to_string(),
                        corporation_id: 1_000_044,
                        solar_system_id: d_sys,
                        active_ship_id: sid,
                        ..Default::default()
                    },
                );
                snap.items.insert(
                    sid,
                    eve_db::InventoryItemRecord {
                        item_id: sid,
                        type_id: 587,
                        owner_id: cid,
                        location_id: d_sys as i64,
                        flag: eve_db::InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            }
        });
        d_ctx
            .db
            .acquire_online_lock(1, 98_301, Some(d_char), true, d_ctx.now_unix())
            .expect("drone pilot online lock");

        let d_sys_handle = d_ctx.systems.get_or_spawn(d_sys);
        let mk_ball = |id, type_id, x| eve_destiny::Ball {
            id,
            type_id,
            position: glam::DVec3::new(x, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 50_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        let _ = d_sys_handle.spawn_ball(mk_ball(d_ship, 587, 0.0)).await;
        let _ = d_sys_handle.spawn_ball(mk_ball(d_drone, 2_454, 500.0)).await;
        let _ = d_sys_handle.try_spawn_ball_with_combat(
            mk_ball(d_npc, 12_391, 1_000.0),
            eve_destiny::CombatState {
                shield_hp: 0.0,
                max_shield_hp: 10.0,
                armor_hp: 0.0,
                max_armor_hp: 10.0,
                structure_hp: 1.0,
                max_structure_hp: 10.0,
                signature_radius: 400.0,
                ..Default::default()
            },
        );
        let _ = d_sys_handle
            .add_target_lock(
                d_ship,
                eve_destiny::TargetLock {
                    target_id: d_npc,
                    lock_time_ms: 100.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await;

        crate::services::drone_test_helpers::seed_active_drones(
            &d_ctx,
            [(d_drone, d_char, d_ship), (d_drone2, d_char2, d_ship2)].map(
                |(did, owner, pship)| crate::services::combat_crimewatch::DroneSpaceState {
                    ball_id: did,
                    drone_item_id: did,
                    type_id: 2_454,
                    group_id: 100,
                    owner_char_id: owner,
                    original_owner_char_id: owner,
                    parent_ship_id: pship,
                    solar_system_id: d_sys,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: false,
                    bandwidth_used: 5.0,
                    max_velocity: 1_000.0,
                    optimal_range: 20_000.0,
                    damage_vector: eve_destiny::DamageVector {
                        em: 0.0,
                        thermal: 200.0,
                        kinetic: 0.0,
                        explosive: 0.0,
                    },
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 0.0,
                    neut_amount: 0.0,
                    ecm_strength: 0.0,
                    tp_sig_bonus_pct: 0.0,
                },
            ),
        );

        let mut d_session = eve_machonet::SessionState::new(98_300);
        d_session.user_id = Some(1);
        d_session.character_id = Some(d_char);
        d_session.solar_system_id = Some(d_sys);
        d_session.ship_id = Some(d_ship);

        let engage_res = crate::services::combat_crimewatch::handle_npcs_drones(
            &d_ctx,
            &mut d_session,
            "entity",
            "CmdEngage",
            &[
                eve_proto::PyRep::Integer(d_npc),
                eve_proto::PyRep::List(vec![eve_proto::PyRep::Integer(d_drone)]),
            ],
            &std::collections::HashMap::new(),
        )
        .await
        .expect("CmdEngage dispatch");
        assert!(engage_res.is_ok(), "CmdEngage must succeed: {engage_res:?}");

        if d_sys_handle.get_combat_state(d_npc).await.ok().flatten().is_some() {
            crate::services::combat_crimewatch::step_engaged_drones(&d_ctx, d_ctx.now_unix() + 10).await;
        }

        d_ctx.db.with_snapshot(|snap| {
            let st = snap.characters[&d_char].standings.get(&500_001).copied().unwrap_or(0.0);
            assert!(
                (st - (-0.03)).abs() < 1e-9,
                "combat drone CmdEngage kill must award -0.03 standing loss, got {st}"
            );
        });
        let d_notes = d_ctx.drain_notifications_for_char(d_char);
        assert!(d_notes.iter().any(|n| n.event == "OnStandingSet"));
        assert!(d_notes.iter().any(|n| n.event == "OnStandingsModified"));

        // Also test passing drone `ball_id` directly as `killer_char_id` to `destroy_specialized_npc_with_hint`.
        crate::services::combat_crimewatch::destroy_specialized_npc_with_hint(
            &d_ctx,
            d_sys,
            d_npc2,
            d_drone2,
            Some((12_391, glam::DVec3::ZERO)),
        )
        .await;
        d_ctx.db.with_snapshot(|snap| {
            let st2 = snap.characters[&d_char2].standings.get(&500_001).copied().unwrap_or(0.0);
            assert!(
                (st2 - (-0.03)).abs() < 1e-9,
                "destroy_specialized_npc_with_hint must resolve drone ball_id killer to owner char, got {st2}"
            );
        });
    }

    // Path 6: End-to-end TCP client socket delivery over `macho_listener::handle_macho_client`.
    // Verifies framed `OnStandingSet` + `OnStandingsModified` MachoNet packets arrive on the live
    // TCP socket when an NPC is destroyed in space.
    use crate::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
    use eve_machonet::{
        placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
        CryptoRequestPacket, CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType,
        VersionExchange, MACHO_PROXY_NODE_ID,
    };

    drop(notify_rx);
    let ctx = Arc::new(ctx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test macho tcp listener");
    let addr = listener.local_addr().unwrap();
    let srv_ctx = Arc::clone(&ctx);
    let accept_task = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = handle_macho_client(stream, srv_ctx).await;
        }
    });

    let mut sock = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to macho tcp listener");
    let _hello = read_framed_pyrep(&mut sock).await.unwrap().unwrap();
    write_framed_pyrep(&mut sock, &VersionExchange::default().to_pyrep())
        .await
        .unwrap();
    write_framed_pyrep(&mut sock, &CryptoRequestPacket::placebo().to_pyrep())
        .await
        .unwrap();
    let cc_rep = read_framed_pyrep(&mut sock).await.unwrap().unwrap();
    assert_eq!(cc_rep.as_str(), Some("OK CC"));
    let mut live_upd = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    live_upd.user_password = None;
    write_framed_pyrep(&mut sock, &live_upd.to_pyrep())
        .await
        .unwrap();
    let _ = read_framed_pyrep(&mut sock).await.unwrap().unwrap();
    write_framed_pyrep(
        &mut sock,
        &CryptoChallengePacket::new_password_login("demo", "demo-secret").to_pyrep(),
    )
    .await
    .unwrap();
    let srv_hs = CryptoServerHandshake::from_pyrep(&read_framed_pyrep(&mut sock).await.unwrap().unwrap())
        .expect("CryptoServerHandshake");
    write_framed_pyrep(
        &mut sock,
        &(CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: eve_proto::PyRep::Integer(0),
        })
        .to_pyrep(),
    )
    .await
    .unwrap();
    let ack = CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut sock).await.unwrap().unwrap())
        .expect("CryptoHandshakeAck");
    let init_pkt =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut sock).await.unwrap().unwrap()).unwrap();
    let MachoAddress::Client { client_id, .. } = init_pkt.destination else {
        panic!("expected Client destination");
    };
    let _sess_chg =
        MachoPacket::from_pyrep(&read_framed_pyrep(&mut sock).await.unwrap().unwrap()).unwrap();

    let sel_call = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id,
            call_id: 1,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: Some("charUnboundMgr".to_string()),
            call_id: 0,
        },
        user_id: Some(ack.user_id),
        payload: eve_proto::PyRep::Tuple(vec![eve_proto::PyRep::Tuple(vec![
            eve_proto::PyRep::Integer(0),
            eve_proto::PyRep::SubStream(Box::new(eve_proto::PyRep::Tuple(vec![
                eve_proto::PyRep::ascii_bytes("SelectCharacterID"),
                eve_proto::PyRep::Tuple(vec![eve_proto::PyRep::Integer(char_id)]),
                eve_proto::PyRep::Dict(Vec::new()),
            ]))),
        ])]),
        named_payload: std::collections::HashMap::new(),
    };
    write_framed_pyrep(&mut sock, &sel_call.to_pyrep())
        .await
        .unwrap();
    loop {
        let pkt = MachoPacket::from_pyrep(
            &tokio::time::timeout(
                std::time::Duration::from_secs(10),
                read_framed_pyrep(&mut sock),
            )
            .await
            .expect("SelectCharacterID timeout")
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        if pkt.packet_type == MachoPacketType::CallRsp {
            break;
        }
    }

    // Clear stale queued notifications and standings, then destroy a Caldari Navy Raven in sys_d.
    let _ = ctx.drain_notifications_for_char(char_id);
    let sys_d: u32 = 30_000_149;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });
    let mut ent = crate::services::combat_crimewatch::combat_npc_ai::combat_npc_ai_tests::make_test_npc_entity(
        970_401,
        12_391,
        sys_d,
        "Navy",
    );
    ent.faction_id = 500_001;
    seed_npc_entity(&ctx, ent);
    ctx.systems
        .invoke_npc_death_hook(sys_d, 970_401, Some(char_id))
        .await;

    let extract_broadcast_args = |pkt: &MachoPacket| -> Option<(String, Vec<eve_proto::PyRep>)> {
        if pkt.packet_type != MachoPacketType::Notification {
            return None;
        }
        let MachoAddress::Broadcast { broadcast_id, .. } = &pkt.destination else {
            return None;
        };
        let eve_proto::PyRep::Tuple(outer) = &pkt.payload else {
            return None;
        };
        let eve_proto::PyRep::Tuple(inner) = outer.first()? else {
            return None;
        };
        let eve_proto::PyRep::SubStream(sub) = inner.get(1)? else {
            return None;
        };
        let eve_proto::PyRep::Tuple(sub_t) = sub.as_ref() else {
            return None;
        };
        let eve_proto::PyRep::Tuple(body_t) = sub_t.get(1)? else {
            return None;
        };
        let eve_proto::PyRep::Tuple(args) = body_t.get(1)? else {
            return None;
        };
        Some((broadcast_id.clone(), args.clone()))
    };

    let mut saw_wire_set = false;
    let mut saw_wire_mod = false;
    let wire_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !(saw_wire_set && saw_wire_mod) {
        let rem = wire_deadline.saturating_duration_since(tokio::time::Instant::now());
        let rep = tokio::time::timeout(rem, read_framed_pyrep(&mut sock))
            .await
            .expect("timed out waiting for standing notification on TCP client socket")
            .unwrap()
            .expect("client socket closed prematurely");
        let pkt = MachoPacket::from_pyrep(&rep).unwrap();
        if let Some((ev, args)) = extract_broadcast_args(&pkt) {
            if ev == "OnStandingSet" {
                assert_eq!(args.first().and_then(|v| v.as_i64()), Some(500_001));
                assert_eq!(args.get(1).and_then(|v| v.as_i64()), Some(char_id));
                let st_val = args.get(2).and_then(|v| v.as_f64()).unwrap();
                assert!((st_val - (-0.03)).abs() < 1e-9);
                saw_wire_set = true;
            } else if ev == "OnStandingsModified" {
                let eve_proto::PyRep::List(mods) = args.first().expect("OnStandingsModified[0]") else {
                    panic!("expected List payload in OnStandingsModified");
                };
                assert!(mods.iter().any(|entry| {
                    matches!(entry, eve_proto::PyRep::Tuple(t)
                        if t.first().and_then(|v| v.as_i64()) == Some(500_001)
                            && t.get(1).and_then(|v| v.as_i64()) == Some(char_id)
                            && t.get(2).and_then(|v| v.as_f64()).is_some_and(|d| (d - (-0.03)).abs() < 1e-9))
                }));
                assert!(mods.iter().any(|entry| {
                    matches!(entry, eve_proto::PyRep::Tuple(t)
                        if t.first().and_then(|v| v.as_i64()) == Some(500_003)
                            && t.get(2).and_then(|v| v.as_f64()).is_some_and(|d| d < 0.0))
                }));
                saw_wire_mod = true;
            }
        }
    }
    assert!(saw_wire_set && saw_wire_mod);
    drop(sock);
    accept_task.abort();
}
