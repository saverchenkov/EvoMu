//! Extra branch-coverage and mutation-killing unit tests for `pve_expeditions`.

#![forbid(unsafe_code)]

use std::sync::{Arc, OnceLock};

use crate::router::ServerContext;
use crate::services::pve_expeditions::{
    emit_expedition_destruction_notifications, enter_combat_anomaly_site,
    expedition_dungeon_chain, expedition_dungeon_for_step, expedition_sig_id,
    find_escalation_destination_system_sde, get_escalating_path_dungeons_entered,
    get_my_escalating_path_details_rowset, spawn_expedition_site_instance,
    validate_and_advance_pve_site_room, EscalatingPathEntry,
};
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use eve_db::clock::VirtualServerClock;
use eve_db::{BookmarkRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn test_all_expedition_dungeon_chains_template_ids_and_destination_fallbacks() {
    let cases = [
        // Guristas
        (1420, 1420, "Guristas Scout Outpost", 3),
        (1215, 1421, "Guristas Hallucinogen Supply Waypoint", 3),
        (1421, 1421, "Guristas Hallucinogen Supply Waypoint", 3),
        (1232, 1422, "Chasing the Dragon", 4),
        (1422, 1422, "Chasing the Dragon", 4),
        (2283, 2402, "Guristas Fleet Staging Point", 4),
        (2402, 2402, "Guristas Fleet Staging Point", 4),
        (2134, 2403, "The Maze", 4),
        (2321, 2403, "The Maze", 4),
        (2403, 2403, "The Maze", 4),
        // Blood Raiders
        (2141, 1430, "Blood Raider Scout Outpost", 3),
        (1430, 1430, "Blood Raider Scout Outpost", 3),
        (1218, 1431, "Blood Raider Crimson Hand Supply Depot", 3),
        (1431, 1431, "Blood Raider Crimson Hand Supply Depot", 3),
        (1229, 1432, "Blood Raider Fleet Staging Point", 4),
        (1432, 1432, "Blood Raider Fleet Staging Point", 4),
        (2282, 2410, "Blood Raider Fleet Staging Point 2", 4),
        (2410, 2410, "Blood Raider Fleet Staging Point 2", 4),
        (2319, 2411, "Crimson Gauntlet", 4),
        (2411, 2411, "Crimson Gauntlet", 4),
        // Sansha's Nation
        (2135, 1440, "Sansha Scout Outpost", 3),
        (1440, 1440, "Sansha Scout Outpost", 3),
        (1216, 1441, "Sansha Supply Yard", 3),
        (1441, 1441, "Sansha Supply Yard", 3),
        (1230, 1442, "Sansha Fleet Staging Point", 4),
        (1442, 1442, "Sansha Fleet Staging Point", 4),
        (2290, 2420, "Centus Assembly Complex", 4),
        (2420, 2420, "Centus Assembly Complex", 4),
        (2327, 2421, "True Creations Research Facility", 4),
        (2421, 2421, "True Creations Research Facility", 4),
        // Serpentis
        (2140, 1450, "Serpentis Scout Outpost", 3),
        (1450, 1450, "Serpentis Scout Outpost", 3),
        (1217, 1451, "Serpentis Supply Depot", 3),
        (1451, 1451, "Serpentis Supply Depot", 3),
        (1231, 1452, "Serpentis Fleet Staging Point", 4),
        (1452, 1452, "Serpentis Fleet Staging Point", 4),
        (2300, 2430, "Serpentis Fleet Staging Point 2", 4),
        (2430, 2430, "Serpentis Fleet Staging Point 2", 4),
        (2330, 2431, "Serpentis Corporation Hydroponics Site", 4),
        (2431, 2431, "Serpentis Corporation Hydroponics Site", 4),
        // Angel Cartel
        (2132, 1410, "Angel Scout Outpost", 3),
        (1410, 1410, "Angel Scout Outpost", 3),
        (1214, 1411, "Angel Cartel Supply Waypoint", 3),
        (1411, 1411, "Angel Cartel Supply Waypoint", 3),
        (1228, 1412, "Angel Fleet Staging Point", 4),
        (1412, 1412, "Angel Fleet Staging Point", 4),
        (2115, 2525, "Angel Domination Fleet Staging Point 2.", 4),
        (2525, 2525, "Angel Domination Fleet Staging Point 2.", 4),
        (2311, 2396, "Angel Cartel Naval Shipyard", 4),
        (2396, 2396, "Angel Cartel Naval Shipyard", 4),
        // Range fallbacks
        (1000, 1420, "Pirate Scout Outpost", 3),
        (1499, 1420, "Pirate Scout Outpost", 3),
        (1500, 1422, "Pirate Outpost Escalation", 4),
        (2199, 1422, "Pirate Outpost Escalation", 4),
        (2200, 2402, "Pirate Fleet Staging Point", 4),
        (2299, 2402, "Pirate Fleet Staging Point", 4),
        (2301, 2403, "The Maze", 4),
        (9999, 2403, "The Maze", 4),
    ];
    for (src, exp_id, exp_name, steps) in cases {
        assert_eq!(expedition_dungeon_chain(src), (exp_id, exp_name, steps));
    }

    assert_eq!(expedition_dungeon_for_step(1420, 0), 1420);
    assert_eq!(expedition_dungeon_for_step(1420, 1), 1420);
    assert_eq!(expedition_dungeon_for_step(1420, 4), 1423);

    // effective_journal_entry_template_id branches
    let mut entry = EscalatingPathEntry {
        instance_id: -555,
        character_id: 90_000_001,
        source_dungeon_id: 1420,
        expedition_dungeon_id: 1420,
        expedition_name: "Guristas Scout Outpost".into(),
        destination_solar_system_id: 30_000_144,
        step_index: 0,
        total_steps: 3,
        journal_text: "Test".into(),
        created_at_sec: 1_000,
        expires_at_sec: 2_000,
        dungeons_entered: vec![1420],
        journal_entry_template_id: 0,
    };
    assert_eq!(entry.effective_journal_entry_template_id(), 280_001);
    entry.step_index = 3;
    assert_eq!(entry.effective_journal_entry_template_id(), 280_003);
    entry.journal_entry_template_id = 299_123;
    assert_eq!(entry.effective_journal_entry_template_id(), 299_123);

    let sig_neg = expedition_sig_id(-555, 3);
    assert_eq!(sig_neg.len(), 7);
    assert_eq!(&sig_neg[3..4], "-");

    // Empty jumps SDE exercises hub fallback branches in find_escalation_destination_system_sde
    let mut empty_sde = (*shared_sde()).clone();
    empty_sde.jumps.clear();
    assert_eq!(find_escalation_destination_system_sde(&empty_sde, 30_000_142), 30_000_144);
    assert_eq!(find_escalation_destination_system_sde(&empty_sde, 30_000_144), 30_000_145);
    assert_eq!(find_escalation_destination_system_sde(&empty_sde, 30_002_187), 30_002_188);
    assert_eq!(find_escalation_destination_system_sde(&empty_sde, 30_002_659), 30_002_660);
    assert_eq!(find_escalation_destination_system_sde(&empty_sde, 30_002_510), 30_002_511);
    assert_eq!(find_escalation_destination_system_sde(&empty_sde, 0), 30_000_001);
    assert_eq!(find_escalation_destination_system_sde(&empty_sde, 30_005_000), 30_005_001);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_expiry_pruning_destruction_notifications_and_ded_sleeper_gate_checks() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_240_001;
    let ship_id: i64 = 1_240_000_001;

    let sde = shared_sde();
    let frigate_tid: u32 = 587;

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_extra", "pw", char_id, "ExpExtra");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_800_000)));

    let mut session = SessionState::new(701);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    // 1. Seed one expired escalation and one active escalation + bookmark for the expired one
    let expired_entry = EscalatingPathEntry {
        instance_id: 910_501,
        character_id: char_id,
        source_dungeon_id: 1420,
        expedition_dungeon_id: 1420,
        expedition_name: "Expired Outpost".into(),
        destination_solar_system_id: sys_id,
        step_index: 1,
        total_steps: 3,
        journal_text: "Expired".into(),
        created_at_sec: 1_775_000_000,
        expires_at_sec: 1_775_700_000, // < clock (1_775_800_000)
        dungeons_entered: vec![1420],
        journal_entry_template_id: 0,
    };
    let active_entry = EscalatingPathEntry {
        instance_id: 910_502,
        character_id: char_id,
        source_dungeon_id: 1430,
        expedition_dungeon_id: 1431,
        expedition_name: "Active Blood Depot".into(),
        destination_solar_system_id: sys_id,
        step_index: 2,
        total_steps: 3,
        journal_text: "Active".into(),
        created_at_sec: 1_775_750_000,
        expires_at_sec: 1_775_900_000, // > clock
        dungeons_entered: vec![1430],
        journal_entry_template_id: 0,
    };
    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char
            .insert(char_id, vec![expired_entry.clone(), active_entry.clone()]);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            77_001,
            BookmarkRecord {
                bookmark_id: 77_001,
                owner_id: char_id,
                label: "Expired BM".into(),
                solar_system_id: sys_id,
                x: 0.0,
                y: 0.0,
                z: 0.0,
                item_id: Some(910_501),
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    // get_my_escalating_path_details_rowset prunes 910_501 and its bookmark, keeping 910_502
    let _ = get_my_escalating_path_details_rowset(&ctx, char_id);
    assert!(ctx.db.with_snapshot(|s| !s.bookmarks.contains_key(&77_001)));
    let remaining_entered = with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.escalations_by_char[&char_id].len(), 1);
        get_escalating_path_dungeons_entered(reg, char_id)
    });
    assert_eq!(remaining_entered.len(), 1);

    // 2. Test spawn_expedition_site_instance and emit_expedition_destruction_notifications
    spawn_expedition_site_instance(&ctx, &active_entry).await;
    emit_expedition_destruction_notifications(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        Some((sys_id, 1430)),
        Some(&active_entry),
        Some(&active_entry),
    );
    // Also test final step notification branch (where dungeons_entered.len() >= total_steps)
    let mut final_entry = active_entry.clone();
    final_entry.step_index = 3;
    final_entry.dungeons_entered = vec![1430, 1431, 1432];
    emit_expedition_destruction_notifications(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        None,
        None,
        Some(&final_entry),
    );

    // 3. enter_combat_anomaly_site: unknown site returns error, valid site with lifetime_seconds == 0 records dungeon_id once
    assert!(enter_combat_anomaly_site(&ctx, char_id, sys_id, "NO_SUCH_SITE").is_err());
    with_pve_exploration_reg(&ctx, |reg| {
        if let Some(site) = reg
            .sites_by_system
            .get_mut(&sys_id)
            .and_then(|l| l.iter_mut().find(|s| s.site_id == "EXP-910502-2"))
        {
            site.lifetime_seconds = 0;
            site.active_npc_ball_ids.clear();
        }
    });
    let enter1 = enter_combat_anomaly_site(&ctx, char_id, sys_id, "EXP-910502-2").expect("enter ok");
    assert_eq!(enter1.dict_get("dungeonID").and_then(|v| v.as_i64()), Some(1431));
    // Entering a second time does not duplicate 1431 in dungeons_entered
    let _ = enter_combat_anomaly_site(&ctx, char_id, sys_id, "EXP-910502-2").expect("enter2 ok");
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.escalations_by_char[&char_id][0].dungeons_entered, vec![1430, 1431]);
    });

    // 4. validate_and_advance_pve_site_room: DED site ship/space/range checks & SleeperCache hazard branch
    crate::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    let ded_site_id = "DED_SITE_1".to_string();
    let sleeper_site_id = "SLEEPER_SITE_1".to_string();
    with_pve_exploration_reg(&ctx, |reg| {
        let list = reg.sites_by_system.entry(sys_id).or_default();
        list.push(PveSiteInstance {
            site_id: ded_site_id.clone(),
            dungeon_id: 7701,
            dungeon_name: "DED 1/10".into(),
            solar_system_id: sys_id,
            site_category: "DEDComplex".into(),
            ded_rating: Some(4),
            position: [10_000.0, 0.0, 0.0],
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 1,
            ..Default::default()
        });
        list.push(PveSiteInstance {
            site_id: sleeper_site_id.clone(),
            dungeon_id: 8801,
            dungeon_name: "Standard Sleeper Cache".into(),
            solar_system_id: sys_id,
            site_category: "SleeperCache".into(),
            position: [10_000.0, 0.0, 0.0],
            sleeper_rift_stabilized: true,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 1,
            ..Default::default()
        });
    });

    // 4a. Missing ship item in snapshot -> ShipNotFound
    assert!(validate_and_advance_pve_site_room(&ctx, &mut session, sys_id, "7701").await.is_err());

    // 4b. Ship item in snapshot, but no ball in space -> NotInSpace
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: frigate_tid,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert!(validate_and_advance_pve_site_room(&ctx, &mut session, sys_id, "7701").await.is_err());

    // 4c. Ship ball in space at (50_000, 0, 0) (> 2,500m from gate at 10_000) -> GateOutOfRange
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut ship_ball = Ball {
        id: ship_id,
        type_id: frigate_tid,
        position: DVec3::new(50_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 30.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball(ship_ball.clone()).await;
    assert!(validate_and_advance_pve_site_room(&ctx, &mut session, sys_id, "7701").await.is_err());

    // 4d. Move ship within 2,500m of gate (11_000, 0, 0 -> dist = 1,000m) -> DED gate and SleeperCache succeed!
    ship_ball.position = DVec3::new(11_000.0, 0.0, 0.0);
    let _ = sys_handle.spawn_ball(ship_ball).await;
    assert!(validate_and_advance_pve_site_room(&ctx, &mut session, sys_id, "7701").await.is_ok());
    assert!(validate_and_advance_pve_site_room(&ctx, &mut session, sys_id, "8801").await.is_ok());
}

mod w4_ops_pve_mut_extra_tests {
    use crate::router::ServerContext;
    use crate::services::pve_expeditions::*;
    use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
    use eve_db::clock::VirtualServerClock;
    use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_sde::{DungeonTemplate, SdeRegistry, SolarSystemId};
    use glam::DVec3;
    use std::sync::{Arc, OnceLock};

    fn shared_sde() -> Arc<SdeRegistry> {
        static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
        Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
    }

    fn make_ctx(sys_id: SolarSystemId, char_id: i64, ship_id: i64) -> (ServerContext, SessionState) {
        make_ctx_with_sde(shared_sde(), sys_id, char_id, ship_id)
    }

    fn make_ctx_with_sde(
        sde: Arc<SdeRegistry>,
        sys_id: SolarSystemId,
        char_id: i64,
        ship_id: i64,
    ) -> (ServerContext, SessionState) {
        let db = Arc::new(WorldRepository::new_empty());
        db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
        db.provision_gm_account(&format!("exp_w4_{char_id}"), "pw", char_id, "ExpW4Pilot");
        let uid = db
            .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
        let _ = db.acquire_online_lock_with_computer_hash(
            uid,
            char_id as u64,
            Some(char_id),
            None,
            true,
            1_800_000_000,
        );
        db.with_mut_snapshot(|snap| {
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
                    type_id: 587, // Rifter (Frigate)
                    owner_id: char_id,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let ctx = ServerContext::with_sde_and_repository(sde, db)
            .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        let mut sess = SessionState::new(720);
        sess.user_id = Some(uid);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        (ctx, sess)
    }

    #[tokio::test]
    async fn pve_expeditions_ops_faction_inference_chains_and_empty_sde_fallback() {
        let mut sde = (*shared_sde()).clone();
        let sys_id: SolarSystemId = 30_000_142;

        // Add synthetic source dungeons with each faction keyword & difficulty tier to exercise infer_faction_id,
        // resolve_escalation_journal_template_and_hint, resolve_sde_escalation_chain, and format_escalation_journal_text
        let custom_cases = [
            (9_999_201, "Angel Cartel Yard", None, Some(20), Some(1)),
            (9_999_202, "Blood Raider Altar", None, Some(20), Some(4)),
            (9_999_203, "Sansha Nation Vigil", None, Some(20), Some(2)),
            (9_999_204, "Serpentis Refuge", None, Some(20), Some(3)),
            (9_999_205, "Guristas Hideaway", None, Some(20), Some(1)),
            (9_999_206, "Rogue Drone Alvu Hive", None, Some(20), Some(4)),
            (9_999_207, "Unrecognized Site", Some(500_021), Some(20), Some(2)),
        ];
        for (did, nm, fid, arch, diff) in custom_cases {
            sde.dungeons.insert(
                did,
                DungeonTemplate {
                    dungeon_id: did,
                    name: nm.to_string(),
                    archetype_id: arch,
                    faction_id: fid,
                    difficulty: diff,
                },
            );
        }
        let sde_arc = Arc::new(sde);

        for (idx, (did, nm, _, _, _)) in custom_cases.iter().enumerate() {
            let cid = 94_201_000 + idx as i64;
            let sid = 1_042_010_000 + idx as i64;
            let (ctx, _) = make_ctx_with_sde(Arc::clone(&sde_arc), sys_id, cid, sid);
            let e = create_escalation_expedition(&ctx, cid, sys_id, *did, nm, [1_000.0, 0.0, 0.0])
                .await
                .expect("create custom faction escalation");
            assert!(e.total_steps >= 1);
            assert!(!e.journal_text.is_empty());
            assert!(e.effective_journal_entry_template_id() > 0);
        }

        // Direct archetype 31 escalation template in SDE
        if let Some((&esc_id, esc_def)) = sde_arc.dungeons.iter().find(|(_, d)| d.archetype_id == Some(31)) {
            let (ctx, _) = make_ctx_with_sde(Arc::clone(&sde_arc), sys_id, 94_201_100, 1_042_010_100);
            let e = create_escalation_expedition(
                &ctx,
                94_201_100,
                sys_id,
                esc_id,
                &esc_def.name,
                [0.0, 0.0, 0.0],
            )
            .await
            .expect("arch 31 direct template");
            assert!(e.total_steps >= 1);
        }

        // Empty SDE dungeons falls back cleanly to expedition_dungeon_chain
        let mut empty_sde = (*sde_arc).clone();
        empty_sde.dungeons.clear();
        let (empty_ctx, _) = make_ctx_with_sde(Arc::new(empty_sde), sys_id, 94_201_200, 1_042_010_200);
        let fb = create_escalation_expedition(
            &empty_ctx,
            94_201_200,
            sys_id,
            1420,
            "Guristas Scout Outpost",
            [0.0, 0.0, 0.0],
        )
        .await
        .expect("empty sde fallback");
        assert_eq!(fb.expedition_dungeon_id, 1420);
        assert_eq!(fb.total_steps, 3);
    }

    #[tokio::test]
    async fn escalation_roll_kv_rowset_signatures_and_step_completion_effects() {
        let sys_id: SolarSystemId = 30_000_142;
        let char_id = 94_200_001;
        let ship_id = 1_042_000_001;
        let (ctx, mut session) = make_ctx(sys_id, char_id, ship_id);

        // 1. Seeded RNG roll determinism yields booleans cleanly
        let mut saw_true = false;
        let mut saw_false = false;
        for _ in 0..200 {
            if roll_escalation_chance(&ctx) {
                saw_true = true;
            } else {
                saw_false = true;
            }
            if saw_true && saw_false {
                break;
            }
        }
        assert!(saw_true && saw_false);

        // 2. Capture char notifications on create -> advance -> final step
        let _ = ctx.drain_notifications_for_char(char_id);
        let entry = create_escalation_expedition(
            &ctx,
            char_id,
            sys_id,
            1420,
            "Guristas Scout Outpost",
            [1_000_000.0, 0.0, 0.0],
        )
        .await
        .expect("create escalation");

        // Bookmark + cosmic signature created
        let (bm_count_1, has_sig_1) = ctx.db.with_snapshot(|snap| {
            let bms = snap
                .bookmarks
                .values()
                .filter(|b| b.owner_id == char_id)
                .count();
            (bms, !snap.cosmic_signatures.is_empty())
        });
        assert!(bm_count_1 >= 1);
        assert!(has_sig_1 || !matches!(get_my_escalating_path_details_rowset(&ctx, char_id), PyRep::None));

        // Add synthetic entry with template_id == 0 and unknown dungeon/system to cover fallbacks in rowset & KV
        with_pve_exploration_reg(&ctx, |reg| {
            reg.escalations_by_char
                .entry(char_id)
                .or_default()
                .push(EscalatingPathEntry {
                    instance_id: 99_887_766,
                    character_id: char_id,
                    source_dungeon_id: 9_998_881,
                    expedition_dungeon_id: 9_998_882,
                    expedition_name: "Unknown Esc".into(),
                    destination_solar_system_id: 99_999_991,
                    step_index: 1,
                    total_steps: 2,
                    journal_text: "Unknown dest".into(),
                    created_at_sec: 1_800_000_000,
                    expires_at_sec: 1_800_086_400,
                    dungeons_entered: vec![9_998_882],
                    journal_entry_template_id: 0,
                });
        });
        let eff_tpl = with_pve_exploration_reg(&ctx, |reg| {
            reg.escalations_by_char
                .get(&char_id)
                .and_then(|list| list.iter().find(|e| e.instance_id == 99_887_766))
                .map(|e| e.effective_journal_entry_template_id())
                .unwrap()
        });
        assert!(eff_tpl > 0);
        let rowset = get_my_escalating_path_details_rowset(&ctx, char_id);
        assert!(!matches!(rowset, PyRep::None));
        with_pve_exploration_reg(&ctx, |reg| {
            if let Some(list) = reg.escalations_by_char.get_mut(&char_id) {
                list.retain(|e| e.instance_id != 99_887_766);
            }
        });

        // Advance step 1 -> 2 -> 3 -> completed
        let mut cur = entry.clone();
        while cur.step_index < cur.total_steps {
            cur = advance_expedition_step(&ctx, char_id, cur.instance_id)
                .await
                .expect("advance")
                .expect("step entry");
        }
        let final_done = advance_expedition_step(&ctx, char_id, cur.instance_id)
            .await
            .expect("final advance")
            .expect("completed summary");
        assert!(final_done.journal_text.contains("fully completed"));

        let missing = advance_expedition_step(&ctx, char_id, 999_111_222)
            .await
            .expect("missing ok");
        assert!(missing.is_none());

        // Verify notifications emitted across create/advance
        let drained = ctx.drain_notifications_for_char(char_id);
        assert!(
            drained.iter().any(|n| n.event == "OnDungeonInstanceAdded" || n.event == "OnEscalatingPathMessage" || n.event == "OnBookmarkAdd"), // rpc-lint: allow test fixture
            "expected expedition notifications, got: {:?}",
            drained.iter().map(|n| n.event.as_str()).collect::<Vec<_>>()
        );

        // 3. enter_combat_anomaly_site matching by destination_solar_system_id + expedition_dungeon_id
        let entry2 = create_escalation_expedition(
            &ctx,
            char_id,
            sys_id,
            1420,
            "Guristas Scout Outpost",
            [5_000_000.0, 0.0, 0.0],
        )
        .await
        .expect("create entry2");
        let dest_sys = entry2.destination_solar_system_id;
        let _ = ctx.systems.get_or_spawn(dest_sys);
        session.solar_system_id = Some(dest_sys);
        let anom_key = format!("SITE_LIFETIME0_{dest_sys}");
        with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system.entry(dest_sys).or_default().push(PveSiteInstance {
                site_id: anom_key.clone(),
                dungeon_id: entry2.expedition_dungeon_id,
                dungeon_name: "Lifetime0 Site".into(),
                solar_system_id: dest_sys,
                site_category: "CombatAnomaly".into(),
                position: [5_000_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                current_room: 1,
                total_rooms: 2,
                current_wave: 1,
                total_waves: 1,
                created_unix: 1_800_000_000,
                lifetime_seconds: 0, // hits 86_400 default fallback!
                expedition_instance_id: None,
                ..Default::default()
            });
        });
        let enter = enter_combat_anomaly_site(
            &ctx,
            char_id,
            entry2.destination_solar_system_id,
            &anom_key,
        )
        .expect("enter combat anomaly");
        assert!(!matches!(enter, PyRep::None));

        // validate_and_advance_pve_site_room using numeric dungeon_id string instead of site_id
        let dest_sys_h = ctx.systems.get_or_spawn(dest_sys);
        let _ = dest_sys_h
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(5_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 400.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        // Too far from gate -> GateOutOfRange, then move ship within 2,500m and advance via numeric dungeon_id
        let err_far = validate_and_advance_pve_site_room(
            &ctx,
            &mut session,
            dest_sys,
            &entry2.expedition_dungeon_id.to_string(),
        )
        .await
        .expect_err("far from gate must fail GateOutOfRange");
        assert_eq!(err_far.code, "GateOutOfRange");

        let (gate_pos, wave_npcs) = with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&dest_sys)
                .and_then(|v| v.iter().find(|s| s.dungeon_id == entry2.expedition_dungeon_id))
                .map(|s| (DVec3::from(s.position), s.active_npc_ball_ids.clone()))
                .unwrap()
        });
        let _ = dest_sys_h.remove_ball(ship_id).await;
        let _ = dest_sys_h
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 587,
                position: gate_pos,
                velocity: DVec3::ZERO,
                max_velocity: 400.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        // While wave NPCs remain alive -> WaveNpcsRemaining
        if !wave_npcs.is_empty() {
            let err_wave = validate_and_advance_pve_site_room(
                &ctx,
                &mut session,
                dest_sys,
                &entry2.expedition_dungeon_id.to_string(),
            )
            .await
            .expect_err("wave NPCs alive must block advance with WaveNpcsRemaining");
            assert_eq!(err_wave.code, "WaveNpcsRemaining");
            for npc_id in wave_npcs {
                let _ = dest_sys_h.remove_ball(npc_id).await;
            }
        }
        let ok_num = validate_and_advance_pve_site_room(
            &ctx,
            &mut session,
            dest_sys,
            &entry2.expedition_dungeon_id.to_string(),
        )
        .await;
        assert!(ok_num.is_ok(), "validate_and_advance_pve_site_room failed: {ok_num:?}");
    }

    #[tokio::test]
    async fn batch3_pve_expeditions_ops_destinations_null_wh_difficulties_and_chain_edge_cases() {
        let mut sde = (*shared_sde()).clone();
        // Ensure diverse synthetic dungeons covering remaining keywords, unknown faction, and difficulty variants
        let extra_dungeons = [
            (9_999_301, "Mordu's Legion Stronghold", None, Some(20), Some(5)),
            (9_999_302, "Triglavian Pochven Vault", None, Some(20), Some(3)),
            (9_999_303, "Sleeper Forgotten Cache", None, Some(20), None),
            (9_999_304, "Mercenary Syndicate Outpost", None, Some(20), Some(1)),
            (9_999_305, "Generic Mystery Anomaly", None, Some(20), None),
            (9_999_306, "Guristas Deep Complex", None, Some(31), None),
            (9_999_307, "Guristas High Complex", None, Some(31), Some(6)),
        ];
        for (did, nm, fid, arch, diff) in extra_dungeons {
            sde.dungeons.insert(
                did,
                DungeonTemplate {
                    dungeon_id: did,
                    name: nm.to_string(),
                    archetype_id: arch,
                    faction_id: fid,
                    difficulty: diff,
                },
            );
        }
        let sde_arc = Arc::new(sde);

        let low_sys = sde_arc
            .solar_systems
            .values()
            .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
            .map(|s| s.solar_system_id)
            .unwrap();
        let null_sys = sde_arc
            .solar_systems
            .values()
            .find(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
            .map(|s| s.solar_system_id)
            .unwrap();
        let wh_sys = sde_arc
            .solar_systems
            .values()
            .find(|s| s.solar_system_id >= 31_000_000)
            .map(|s| s.solar_system_id);

        // Lowsec and nullsec origins across all difficulty/faction combos
        for (idx, (did, nm, _, _, _)) in extra_dungeons.iter().enumerate() {
            let origin = if idx % 2 == 0 { low_sys } else { null_sys };
            let cid = 94_205_000 + idx as i64;
            let sid = 1_042_050_000 + idx as i64;
            let (ctx, _) = make_ctx_with_sde(Arc::clone(&sde_arc), origin, cid, sid);
            let mut cur = create_escalation_expedition(&ctx, cid, origin, *did, nm, [0.0, 0.0, 0.0])
                .await
                .expect("create low/null escalation");
            assert!(cur.total_steps >= 1);
            assert!(!cur.journal_text.is_empty());
            for _ in 0..=cur.total_steps {
                let Some(next) = advance_expedition_step(&ctx, cid, cur.instance_id).await.expect("step") else {
                    break;
                };
                if next.journal_text.contains("fully completed") {
                    break;
                }
                cur = next;
            }
        }

        // Wormhole origin if present
        if let Some(wh) = wh_sys {
            let (ctx, _) = make_ctx_with_sde(Arc::clone(&sde_arc), wh, 94_205_100, 1_042_050_100);
            let e = create_escalation_expedition(&ctx, 94_205_100, wh, 9_999_303, "Sleeper Forgotten Cache", [0.0, 0.0, 0.0])
                .await
                .expect("wh escalation");
            assert!(e.destination_solar_system_id > 0);
        }

        // Single-system SDE with empty jumps hits fallback destination selection!
        let mut iso_sde = (*sde_arc).clone();
        iso_sde.solar_systems.retain(|&id, _| id == low_sys);
        iso_sde.jumps.clear();
        iso_sde.dungeons.retain(|&id, _| id == 9_999_306);
        assert_eq!(find_escalation_destination_system_sde(&iso_sde, 30_000_144), 30_000_145);
        assert_eq!(find_escalation_destination_system_sde(&iso_sde, 30_002_187), 30_002_188);
        assert_eq!(find_escalation_destination_system_sde(&iso_sde, 30_002_659), 30_002_660);
        assert_eq!(find_escalation_destination_system_sde(&iso_sde, 30_002_510), 30_002_511);
        let (iso_ctx, _) = make_ctx_with_sde(Arc::new(iso_sde), low_sys, 94_205_200, 1_042_050_200);
        let iso_e = create_escalation_expedition(&iso_ctx, 94_205_200, low_sys, 9_999_306, "Guristas Deep Complex", [0.0, 0.0, 0.0])
            .await
            .expect("isolated sys escalation");
        assert_eq!(iso_e.destination_solar_system_id, (low_sys + 1).max(30_000_001));
    }

    #[tokio::test]
    async fn batch4_pve_expeditions_ops_faction_hint_objectives_and_rooms() {
        let mut sde = (*shared_sde()).clone();
        sde.dungeons.insert(
            9_999_401,
            DungeonTemplate {
                dungeon_id: 9_999_401,
                name: "Unknown Complex".into(),
                archetype_id: Some(20),
                faction_id: Some(500_010),
                difficulty: Some(3),
            },
        );
        sde.dungeons.insert(
            9_999_402,
            DungeonTemplate {
                dungeon_id: 9_999_402,
                name: "Rogue Drone Hive Anomaly".into(),
                archetype_id: Some(20),
                faction_id: None,
                difficulty: Some(3),
            },
        );
        sde.dungeons.insert(
            9_999_403,
            DungeonTemplate {
                dungeon_id: 9_999_403,
                name: "Rogue Drone Escalation Hub".into(),
                archetype_id: Some(31),
                faction_id: Some(500_021),
                difficulty: Some(3),
            },
        );
        let sde_arc = Arc::new(sde.clone());
        let (ctx1, _) = make_ctx_with_sde(Arc::clone(&sde_arc), 30_000_142, 94_206_001, 1_042_060_001);
        let e1 = create_escalation_expedition(&ctx1, 94_206_001, 30_000_142, 9_999_401, "Unknown Complex", [0.0, 0.0, 0.0])
            .await
            .expect("explicit faction escalation");
        assert!(e1.total_steps >= 1);

        let e2 = create_escalation_expedition(&ctx1, 94_206_001, 30_000_142, 9_999_402, "Rogue Drone Hive Anomaly", [0.0, 0.0, 0.0])
            .await
            .expect("drone hive faction escalation");
        assert!(e2.total_steps >= 1);

        // Drone archetype_id == 31 explicit faction retention (line 45)
        let e_drone = create_escalation_expedition(
            &ctx1,
            94_206_001,
            30_000_142,
            9_999_403,
            "Rogue Drone Escalation Hub",
            [0.0, 0.0, 0.0],
        )
        .await
        .expect("drone 31 escalation");
        assert!(e_drone.total_steps >= 1);
        assert_eq!(
            sde_arc
                .dungeons
                .get(&e_drone.expedition_dungeon_id)
                .and_then(|d| d.faction_id),
            Some(500_021)
        );

        // Pirate faction system fallbacks (BloodRaiders / Sansha / Serpentis -> lines 70-72)
        use crate::services::pve_exploration_seeding::{pirate_faction_for_system, PirateFaction};
        for (idx, (target_pf, expected_fid)) in [
            (PirateFaction::BloodRaiders, 500_012_u32),
            (PirateFaction::Sansha, 500_019_u32),
            (PirateFaction::Serpentis, 500_020_u32),
        ]
        .into_iter()
        .enumerate()
        {
            let sys_id = sde_arc
                .solar_systems
                .keys()
                .copied()
                .find(|&sid| pirate_faction_for_system(&sde_arc, sid) == target_pf)
                .expect("system for pirate faction");
            let cid = 94_206_100 + idx as i64;
            let (sys_ctx, _) = make_ctx_with_sde(
                Arc::clone(&sde_arc),
                sys_id,
                cid,
                1_042_060_100 + idx as i64,
            );
            let e_pf = create_escalation_expedition(
                &sys_ctx,
                cid,
                sys_id,
                9_999_998,
                "Mystery Uncharted Anomaly",
                [0.0, 0.0, 0.0],
            )
            .await
            .expect("pirate faction escalation");
            assert!(e_pf.total_steps >= 1);
            assert!(!e_pf.expedition_name.is_empty());
            if sde_arc
                .dungeons
                .values()
                .any(|d| d.archetype_id == Some(31) && d.faction_id == Some(expected_fid))
            {
                assert_eq!(
                    sde_arc
                        .dungeons
                        .get(&e_pf.expedition_dungeon_id)
                        .and_then(|d| d.faction_id),
                    Some(expected_fid)
                );
            }
        }

        // Empty SDE hits sde.dungeon_objectives() Err fallback cleanly (line 87)
        let (empty_ctx, _) = make_ctx_with_sde(Arc::new(SdeRegistry::default()), 30_000_142, 94_206_002, 1_042_060_002);
        let e_empty = create_escalation_expedition(&empty_ctx, 94_206_002, 30_000_142, 1420, "Guristas Scout Outpost", [0.0, 0.0, 0.0])
            .await
            .expect("empty sde escalation");
        assert!(e_empty.total_steps >= 1);
        assert!(e_empty.journal_text.contains("Guristas Scout Outpost"));
    }
}
