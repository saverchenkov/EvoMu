use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use std::collections::HashMap;

#[test]
fn test_tourney_common_states_countdown_points_telemetry_and_validation() {
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let mut gm = sess.clone();
    gm.role |= eve_machonet::service_roles::ROLE_GML | eve_machonet::service_roles::ROLE_GMH;

    // 1. TourneyMatchState::as_str
    assert_eq!(TourneyMatchState::Setup.as_str(), "Setup");
    assert_eq!(TourneyMatchState::Banning.as_str(), "Banning");
    assert_eq!(TourneyMatchState::Countdown.as_str(), "Countdown");
    assert_eq!(TourneyMatchState::InProgress.as_str(), "InProgress");
    assert_eq!(TourneyMatchState::Completed.as_str(), "Completed");

    // 2. TourneyMatch::new & sync_match_countdown
    let mut m = TourneyMatch::new(101, 1, 30_000_142, 100, 600, 300, 1);
    assert!(m.drone_restrict && m.lock_restrict && m.move_restrict && m.warp_restrict);
    assert!(!sync_match_countdown(&mut m, 1000));
    m.state = TourneyMatchState::Countdown;
    m.countdown_active = true;
    m.countdown_start_unix = 1000;
    m.countdown_seconds = 60;
    assert!(!sync_match_countdown(&mut m, 1059));
    assert!(sync_match_countdown(&mut m, 1060));
    assert_eq!(m.state, TourneyMatchState::InProgress);
    assert!(!m.countdown_active && !m.drone_restrict && !m.lock_restrict && !m.move_restrict && !m.warp_restrict);

    // 3. register_tourney_setup validation & store persistence
    assert!(register_tourney_setup(&ctx, 1, "AT", 999_999_999, TourneyArenaSystem::default()).is_err());
    assert!(register_tourney_setup(&ctx, 0, "AT", 30_000_142, TourneyArenaSystem::default()).is_err());
    assert!(register_tourney_setup(&ctx, 1, "  ", 30_000_142, TourneyArenaSystem::default()).is_err());
    let sname = register_tourney_setup(&ctx, 1, "AT XXI", 30_000_142, TourneyArenaSystem::default()).unwrap();
    assert!(!sname.is_empty());

    let mut store = TourneyStore::default();
    assert!(store.resolve_match(None).is_none());
    assert!(store.resolve_match_mut(None).is_err());
    store.matches.insert(101, m.clone());
    store.active_match_id = Some(101);
    assert!(store.resolve_match(None).is_some());
    assert!(store.resolve_match_mut(Some(101)).is_ok());

    // 4. Arg extractors & referee auth
    let args = vec![
        PyRep::Integer(42),
        PyRep::Bool(true),
        PyRep::Integer(0),
        PyRep::List(vec![PyRep::Integer(7), PyRep::Integer(8)]),
    ];
    assert_eq!(extract_id(&args, 0), Some(42));
    assert!(extract_bool(&args, 1));
    assert!(!extract_bool(&args, 2));
    assert_eq!(extract_list_i64(args.get(3)), vec![7, 8]);
    assert_eq!(extract_list_i64(args.first()), vec![42]);

    assert!(check_referee_auth(&sess, 90_000_001, &m).is_err());
    assert!(check_referee_auth(&gm, 90_000_001, &m).is_ok());
    m.referees.push(90_000_001);
    assert!(check_referee_auth(&sess, 90_000_001, &m).is_ok());

    // 5. Ship points, bans, team validation, telemetry, and home station resolution
    assert_eq!(compute_ship_points(&ctx, 587), 2); // Rifter (Frigate group 25)
    assert_eq!(compute_ship_points(&ctx, 645), 19); // Dominix (Battleship group 27)
    assert_eq!(compute_ship_points(&ctx, 999_999_999), 10);

    m.banned_ships.push(645);
    assert!(is_ship_type_banned(&m, 645));
    assert!(!is_ship_type_banned(&m, 587));

    let banned_pilot = TourneyPilot {
        character_id: 90_000_001,
        ship_type_id: 645,
        ship_id: 1,
        points: 19,
    };
    assert!(validate_pilot_for_team(&mut m, 0, &banned_pilot).is_err());

    m.max_points = 10;
    let expensive_pilot = TourneyPilot {
        character_id: 90_000_001,
        ship_type_id: 587,
        ship_id: 1,
        points: 15,
    };
    assert!(validate_pilot_for_team(&mut m, 0, &expensive_pilot).is_err());

    let valid_pilot = TourneyPilot {
        character_id: 90_000_001,
        ship_type_id: 587,
        ship_id: 1,
        points: 2,
    };
    assert!(validate_pilot_for_team(&mut m, 0, &valid_pilot).is_ok());

    ctx.db.with_snapshot(|snap| {
        let (_, tname) = resolve_team_name(snap, 0, Some(90_000_001));
        assert!(!tname.is_empty());
        let p = resolve_pilot(&ctx, snap, 90_000_001);
        assert_eq!(p.character_id, 90_000_001);
        let telem = compute_team_telemetry(&ctx, snap, &[valid_pilot], 15, 2);
        assert!(matches!(telem, PyRep::Tuple(ref t) if t.len() == 6));
        let (st_id, sys_id) = resolve_valid_home_station(&ctx, snap, 60_003_760, 30_000_142);
        assert_eq!(st_id, 60_003_760);
        assert_eq!(sys_id, 30_000_142);
    });
    maybe_sync_expired_countdowns(&ctx, ctx.now_unix());
}

#[test]
fn test_tourney_common_deep_helpers_telemetry_bans_and_countdowns() {
    use eve_db::{AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};

    let ctx = test_ctx();
    let now = ctx.now_unix();

    // 1. sync_match_countdown negative/edge cases + maybe_sync_expired_countdowns persisted transition
    let mut m = TourneyMatch::new(201, 2, 30_000_142, 50, 600, 300, 1);
    m.state = TourneyMatchState::Setup;
    m.countdown_active = true;
    m.countdown_start_unix = 100;
    m.countdown_seconds = 0; // .max(1)
    assert!(!sync_match_countdown(&mut m, 500)); // Setup != Countdown

    m.state = TourneyMatchState::Countdown;
    m.countdown_active = false;
    assert!(!sync_match_countdown(&mut m, 500)); // countdown_active == false

    m.countdown_active = true;
    assert!(!sync_match_countdown(&mut m, 100)); // 100 < 100 + 1
    assert!(sync_match_countdown(&mut m, 101)); // 101 >= 100 + 1

    // Persist an expired countdown match and verify maybe_sync_expired_countdowns flips it
    let mut m_persisted = TourneyMatch::new(202, 2, 30_000_142, 50, 600, 300, 1);
    m_persisted.state = TourneyMatchState::Countdown;
    m_persisted.countdown_active = true;
    m_persisted.countdown_start_unix = now - 120;
    m_persisted.countdown_seconds = 60;
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = TourneyStore::default();
        store.matches.insert(202, m_persisted);
        store.active_match_id = Some(202);
        save_store(snap, &store, now - 120);
        assert_eq!(snap.tourney_state.active_match_id, Some(202));
    });
    // Future expiration check first? Since item_id = now - 60 <= now, calling at now - 61 is no-op:
    maybe_sync_expired_countdowns(&ctx, now - 61);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(load_store(snap).matches[&202].state, TourneyMatchState::Countdown);
    });
    maybe_sync_expired_countdowns(&ctx, now);
    ctx.db.with_snapshot(|snap| {
        let loaded = load_store(snap);
        assert_eq!(loaded.matches[&202].state, TourneyMatchState::InProgress);
        assert!(!loaded.matches[&202].countdown_active);
    });

    // 2. TourneyStore::resolve_match / resolve_match_mut edges
    let mut store = TourneyStore::default();
    assert_eq!(store.resolve_match_mut(Some(999)).unwrap_err().code, "MatchNotFound");
    store.matches.insert(201, TourneyMatch::new(201, 2, 30_000_142, 50, 600, 300, 1));
    store.active_match_id = Some(201);
    assert_eq!(store.resolve_match(Some(0)).unwrap().match_id, 201);
    assert!(store.resolve_match(Some(999)).is_none());

    // 3. Arg extraction edges
    let args = vec![PyRep::Tuple(vec![PyRep::Integer(9), PyRep::String("skip".into()), PyRep::Integer(11)])];
    assert_eq!(extract_id(&args, 5), None);
    assert!(!extract_bool(&args, 5));
    assert!(!extract_bool(&args, 0));
    assert_eq!(extract_list_i64(args.first()), vec![9, 11]);
    assert!(extract_list_i64(None).is_empty());

    // 4. resolve_team_name & resolve_valid_home_station & resolve_pilot across alliance/corp/char/structure/fallback
    let citadel_id = 1_000_777_001_i64;
    let ship_id = 77_000_001_i64;
    let mod_dps_id = 77_000_002_i64;
    let mod_rep_id = 77_000_003_i64;
    let mod_ctrl_id = 77_000_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            99_000_010,
            AllianceRecord {
                alliance_id: 99_000_010,
                name: "Test Alliance".into(),
                short_name: "TSTA".into(),
                executor_corp_id: 98_000_010,
                member_corp_ids: [98_000_010].into_iter().collect(),
            },
        );
        snap.corporations.insert(
            98_000_010,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_000_010; __corp.name = "Test Corp".into(); __corp.alliance_id = Some(99_000_010); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.corporations.insert(
            98_000_011,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_000_011; __corp.name = "Corp No Ally".into(); __corp.alliance_id = None; __corp.ensure_wallet_divisions(); __corp },
        );
        let base_ch = snap.characters[&90_000_001].clone();
        let mut ch1 = base_ch.clone();
        ch1.character_id = 90_000_010;
        ch1.name = "Capt Ally".into();
        ch1.corporation_id = 98_000_010;
        ch1.alliance_id = Some(99_000_010);
        ch1.active_ship_id = ship_id;
        snap.characters.insert(90_000_010, ch1);

        let mut ch2 = base_ch.clone();
        ch2.character_id = 90_000_011;
        ch2.name = "Capt Corp".into();
        ch2.corporation_id = 98_000_011;
        ch2.alliance_id = None;
        ch2.active_ship_id = 0;
        snap.characters.insert(90_000_011, ch2);

        let mut ch3 = base_ch;
        ch3.character_id = 90_000_012;
        ch3.name = "Capt Solo".into();
        ch3.corporation_id = 99_999_999;
        ch3.alliance_id = None;
        ch3.active_ship_id = 0;
        snap.characters.insert(90_000_012, ch3);
        snap.structures.insert(
            citadel_id,
            UpwellStructureRecord {
                structure_id: citadel_id,
                type_id: 35832,
                owner_corp_id: 98_000_010,
                solar_system_id: 30_000_144,
                name: "Citadel".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: 90_000_010,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Fit turret/DPS (attr 64, 114, 51), rep (attr 84, 73), control (attr 105, 20)
        let mod_type_id = ctx.sde.types.keys().copied().next().unwrap();
        let mut da1 = HashMap::new();
        da1.insert(64, 3.0);
        da1.insert(114, 50.0);
        da1.insert(51, 2000.0);
        snap.items.insert(
            mod_dps_id,
            InventoryItemRecord {
                item_id: mod_dps_id,
                type_id: mod_type_id,
                owner_id: 90_000_010,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                dynamic_attributes: da1,
                ..Default::default()
            },
        );
        let mut da2 = HashMap::new();
        da2.insert(84, 120.0);
        da2.insert(73, 4000.0);
        snap.items.insert(
            mod_rep_id,
            InventoryItemRecord {
                item_id: mod_rep_id,
                type_id: mod_type_id,
                owner_id: 90_000_010,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                dynamic_attributes: da2,
                ..Default::default()
            },
        );
        let mut da3 = HashMap::new();
        da3.insert(105, 2.0);
        da3.insert(20, -60.0);
        snap.items.insert(
            mod_ctrl_id,
            InventoryItemRecord {
                item_id: mod_ctrl_id,
                type_id: mod_type_id,
                owner_id: 90_000_010,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                dynamic_attributes: da3,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        assert_eq!(resolve_team_name(snap, 99_000_010, None), (99_000_010, "Test Alliance".into()));
        assert_eq!(resolve_team_name(snap, 98_000_010, None), (98_000_010, "Test Corp".into()));
        assert_eq!(resolve_team_name(snap, 90_000_012, None), (90_000_012, "Capt Solo".into()));
        assert_eq!(resolve_team_name(snap, 0, Some(90_000_010)), (99_000_010, "Test Alliance".into()));
        assert_eq!(resolve_team_name(snap, 555, Some(90_000_011)), (555, "Corp No Ally".into()));
        assert_eq!(resolve_team_name(snap, 0, Some(90_000_012)), (90_000_012, "Capt Solo".into()));
        assert_eq!(resolve_team_name(snap, 0, Some(99_999_888)), (0, String::new()));

        // Structure home station
        assert_eq!(resolve_valid_home_station(&ctx, snap, citadel_id, 30_000_142), (citadel_id, 30_000_144));
        // Unknown home station falls back to SDE station in fallback_sys
        let (fb_st, fb_sys) = resolve_valid_home_station(&ctx, snap, 999_888_777, 30_000_142);
        assert!(fb_st > 0);
        assert_eq!(fb_sys, 30_000_142);

        // Pilot resolution & fitted telemetry
        let pilot = resolve_pilot(&ctx, snap, 90_000_010);
        assert_eq!(pilot.ship_id, ship_id);
        assert_eq!(pilot.points, 2);
        let no_ship = resolve_pilot(&ctx, snap, 90_000_011);
        assert_eq!((no_ship.ship_id, no_ship.ship_type_id, no_ship.points), (0, 0, 0));

        let PyRep::Tuple(telem) = compute_team_telemetry(&ctx, snap, &[pilot, no_ship], 42, 3) else {
            panic!("expected tuple");
        };
        assert!(telem[0].as_i64().unwrap() > 0); // ehp
        assert!(telem[1].as_i64().unwrap() > 25); // dps with fitted turret
        assert!(telem[2].as_i64().unwrap() > 0); // rep
        assert!(telem[3].as_i64().unwrap() >= 160); // control (2*50 + 60)
        assert_eq!(telem[4], PyRep::Integer(42));
        assert_eq!(telem[5], PyRep::Integer(3));
    });

    // 5. Ship point table across SDE groups + ban / team replacement / deduplicated pilot_errors
    for (gid, expected_pts) in [
        (27_u32, 19_i64),
        (540, 16),
        (358, 14),
        (419, 13),
        (26, 9),
        (1305, 6),
        (324, 4),
        (830, 3),
        (25, 2),
    ] {
        if let Some(t) = ctx.sde.types.values().find(|t| t.group_id == gid) {
            assert_eq!(compute_ship_points(&ctx, i64::from(t.type_id)), expected_pts, "group {gid}");
        }
    }

    let mut m_val = TourneyMatch::new(301, 1, 30_000_142, 10, 600, 300, 1);
    m_val.banned_ship_types_red.push(588);
    m_val.banned_ship_types_blue.push(589);
    assert!(!is_ship_type_banned(&m_val, 0));
    assert!(is_ship_type_banned(&m_val, 588));
    assert!(is_ship_type_banned(&m_val, 589));

    let blue_existing = TourneyPilot { character_id: 90_000_020, ship_type_id: 587, ship_id: 1, points: 9 };
    m_val.blue_players.push(blue_existing.clone());
    // Updating the same pilot on blue team does NOT double-count their 9 points
    assert!(validate_pilot_for_team(&mut m_val, 1, &blue_existing).is_ok());

    let banned_p = TourneyPilot { character_id: 90_000_021, ship_type_id: 588, ship_id: 2, points: 2 };
    assert_eq!(validate_pilot_for_team(&mut m_val, 1, &banned_p).unwrap_err().code, "TourneyShipTypeBanned");
    assert_eq!(validate_pilot_for_team(&mut m_val, 1, &banned_p).unwrap_err().code, "TourneyShipTypeBanned");
    // Error deduplicated
    assert_eq!(m_val.pilot_errors[&90_000_021].len(), 1);
}
