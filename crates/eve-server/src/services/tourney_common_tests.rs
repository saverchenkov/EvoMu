use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};

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
