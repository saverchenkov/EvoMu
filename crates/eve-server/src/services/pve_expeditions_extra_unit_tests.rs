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
