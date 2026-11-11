//! Edge-case tests for `tourney_common`: team name resolution, countdown syncing,
//! ship point tiers, telemetry with fitted modules, home station fallbacks, and pilot validation.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_sde::InvType;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn tourney_common_team_names_countdowns_and_store_fallbacks() {
    let ctx = test_ctx();
    let now = 2_000_000_000u64;

    // 1. Corrupt store JSON falls back to default
    ctx.db.with_mut_snapshot(|snap| {
        snap.tourney_state = TourneyStore::default();
        let loaded = load_store(snap);
        assert!(loaded.matches.is_empty());
    });

    // 2. TourneyStore resolve_match / resolve_match_mut edges
    let mut store = TourneyStore::default();
    let mut m1 = TourneyMatch::new(201, 2, 30_000_142, 50, 600, 120, 1);
    m1.state = TourneyMatchState::Countdown;
    m1.countdown_active = true;
    m1.countdown_start_unix = now;
    m1.countdown_seconds = 0; // clamped to .max(1)
    store.matches.insert(201, m1.clone());
    store.active_match_id = Some(201);

    // Non-positive target_id uses active_match_id
    assert!(store.resolve_match(Some(0)).is_some());
    assert!(store.resolve_match_mut(Some(-5)).is_ok());
    // Missing match_id returns None / MatchNotFound
    assert!(store.resolve_match(Some(999)).is_none());
    assert_eq!(store.resolve_match_mut(Some(999)).unwrap_err().code, "MatchNotFound");

    // 3. save_store sets earliest countdown exp; maybe_sync_expired_countdowns skips before expiry, syncs after
    ctx.db.with_mut_snapshot(|snap| {
        save_store(snap, &store, now);
        assert_eq!(snap.tourney_state.active_match_id, Some(201));
    });
    maybe_sync_expired_countdowns(&ctx, now);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(load_store(snap).matches[&201].state, TourneyMatchState::Countdown);
    });
    maybe_sync_expired_countdowns(&ctx, now + 1);
    ctx.db.with_snapshot(|snap| {
        let m = &load_store(snap).matches[&201];
        assert_eq!(m.state, TourneyMatchState::InProgress);
        assert!(!m.countdown_active);
    });
    // Sync when countdown_active == false is a no-op
    let mut inactive_cd = m1;
    inactive_cd.countdown_active = false;
    assert!(!sync_match_countdown(&mut inactive_cd, now + 100));

    // 4. resolve_team_name across alliance, corp, char, captain alliance/corp/char, and missing
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            99_000_111,
            AllianceRecord {
                alliance_id: 99_000_111,
                name: "Alliance Alpha".into(),
                short_name: "ALPH".into(),
                executor_corp_id: 98_000_111,
                member_corp_ids: vec![98_000_111].into_iter().collect(),
            },
        );
        snap.corporations.insert(
            98_000_111,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = 98_000_111; __corp.name = "Corp Bravo".into(); __corp.ticker = "CBRV".into(); __corp.ceo_id = 90_000_001; __corp.ensure_wallet_divisions(); __corp },
        );
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = Some(99_000_111);
            ch.corporation_id = 98_000_111;
        }
        assert_eq!(resolve_team_name(snap, 99_000_111, None), (99_000_111, "Alliance Alpha".into()));
        assert_eq!(resolve_team_name(snap, 98_000_111, None), (98_000_111, "Corp Bravo".into()));
        assert_eq!(
            resolve_team_name(snap, 90_000_001, None),
            (90_000_001, snap.characters[&90_000_001].name.clone())
        );
        // team_id = 0, captain has alliance
        assert_eq!(resolve_team_name(snap, 0, Some(90_000_001)), (99_000_111, "Alliance Alpha".into()));
        // team_id > 0 (not found), captain has alliance -> keeps team_id
        assert_eq!(resolve_team_name(snap, 777, Some(90_000_001)), (777, "Alliance Alpha".into()));
        // captain has no alliance, has corp
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.alliance_id = None;
        }
        assert_eq!(resolve_team_name(snap, 0, Some(90_000_001)), (98_000_111, "Corp Bravo".into()));
        // captain corp missing -> falls back to captain char name
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_999_999;
        }
        assert_eq!(
            resolve_team_name(snap, 0, Some(90_000_001)),
            (90_000_001, snap.characters[&90_000_001].name.clone())
        );
        // unknown team & unknown captain
        assert_eq!(resolve_team_name(snap, 0, Some(999_999_999)), (0, String::new()));
        assert_eq!(resolve_team_name(snap, -1, None), (-1, String::new()));
    });
}

#[test]
fn tourney_common_extractors_ship_points_telemetry_validation_and_stations() {
    let mut ctx = test_ctx();
    let _sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    // Arg extractors edge cases
    let args = vec![PyRep::String("not_bool".into()), PyRep::Tuple(vec![PyRep::Integer(10), PyRep::String("x".into()), PyRep::Integer(20)])];
    assert_eq!(extract_id(&args, 5), None);
    assert!(!extract_bool(&args, 0));
    assert!(!extract_bool(&args, 5));
    assert_eq!(extract_list_i64(args.get(1)), vec![10, 20]);
    assert!(extract_list_i64(None).is_empty());

    // Inject synthetic types covering every compute_ship_points group arm + fitted module telemetry
    let mut sde_clone = (*ctx.sde).clone();
    let group_cases: &[(u32, u32, i64)] = &[
        (900_027, 27, 19),
        (900_540, 540, 16),
        (900_358, 358, 14),
        (900_832, 832, 14),
        (900_833, 833, 14),
        (900_906, 906, 14),
        (900_419, 419, 13),
        (901_201, 1201, 13),
        (901_202, 1202, 13),
        (900_026, 26, 9),
        (900_894, 894, 9),
        (901_305, 1305, 6),
        (900_324, 324, 4),
        (900_420, 420, 4),
        (900_830, 830, 3),
        (900_831, 831, 3),
        (900_834, 834, 3),
        (900_893, 893, 3),
        (900_025, 25, 2),
        (909_999, 999, 10),
    ];
    let base_type: InvType = sde_clone.types.values().next().unwrap().clone();
    for &(tid, gid, _) in group_cases {
        let mut attrs = HashMap::new();
        attrs.insert(9, 1000.0);
        attrs.insert(265, 1000.0);
        attrs.insert(263, 1000.0);
        attrs.insert(102, 4.0);
        let mut t = base_type.clone();
        t.type_id = tid;
        t.group_id = gid;
        t.category_id = 6;
        t.name = format!("TestHull{gid}");
        t.default_attributes = attrs;
        sde_clone.types.insert(tid, t);
    }
    // Synthetic weapon/rep/control module type
    let mut mod_attrs = HashMap::new();
    mod_attrs.insert(51, 2000.0); // cycle ms
    mod_attrs.insert(64, 2.0);    // damageMultiplier
    mod_attrs.insert(114, 50.0);  // emDamage
    mod_attrs.insert(84, 100.0);  // armorDamageAmount (rep)
    mod_attrs.insert(105, 1.0);   // warpScrambleStrength
    mod_attrs.insert(20, -30.0);  // speedFactor
    let mut mod_t = base_type;
    mod_t.type_id = 910_001;
    mod_t.group_id = 53;
    mod_t.category_id = 7;
    mod_t.name = "TestModule".into();
    mod_t.default_attributes = mod_attrs;
    sde_clone.types.insert(910_001, mod_t);
    ctx.sde = Arc::new(sde_clone);

    for &(tid, _, expected_pts) in group_cases {
        assert_eq!(compute_ship_points(&ctx, i64::from(tid)), expected_pts, "tid {tid}");
    }

    // Pilot validation edge cases: zero/red/blue bans, error deduplication, blue team, unlimited points
    let mut m = TourneyMatch::new(301, 1, 30_000_142, 0, 600, 0, 1);
    m.banned_ship_types_red.push(900_027);
    m.banned_ship_types_blue.push(900_540);
    assert!(!is_ship_type_banned(&m, 0));
    assert!(is_ship_type_banned(&m, 900_027));
    assert!(is_ship_type_banned(&m, 900_540));

    let p_ban = TourneyPilot { character_id: 90_000_001, ship_type_id: 900_027, ship_id: 55_001, points: 19 };
    assert!(validate_pilot_for_team(&mut m, 0, &p_ban).is_err());
    assert!(validate_pilot_for_team(&mut m, 0, &p_ban).is_err());
    assert_eq!(m.pilot_errors[&90_000_001].len(), 1); // deduplicated

    // max_points == 0 allows expensive pilot on blue team
    let p_blue = TourneyPilot { character_id: 90_000_002, ship_type_id: 900_025, ship_id: 55_002, points: 20 };
    assert!(validate_pilot_for_team(&mut m, 1, &p_blue).is_ok());

    // Enforce max_points on blue team & replace existing pilot
    m.max_points = 10;
    m.blue_players.push(TourneyPilot { character_id: 90_000_003, ship_type_id: 900_025, ship_id: 55_003, points: 8 });
    let p_blue_ok = TourneyPilot { character_id: 90_000_003, ship_type_id: 900_025, ship_id: 55_003, points: 9 };
    assert!(validate_pilot_for_team(&mut m, 1, &p_blue_ok).is_ok());
    let p_blue_over = TourneyPilot { character_id: 90_000_004, ship_type_id: 900_025, ship_id: 55_004, points: 5 };
    assert!(validate_pilot_for_team(&mut m, 1, &p_blue_over).is_err());
    assert!(validate_pilot_for_team(&mut m, 1, &p_blue_over).is_err());
    assert_eq!(m.pilot_errors[&90_000_004].len(), 1);

    // Telemetry with fitted module + unknown hull + resolve_pilot + resolve_valid_home_station fallbacks
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            77_001,
            InventoryItemRecord {
                item_id: 77_001,
                type_id: 910_001,
                owner_id: 90_000_001,
                location_id: 55_001,
                flag: InventoryFlag::HiSlot0, // 27 in 11..=34
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let p_fitted = TourneyPilot { character_id: 90_000_001, ship_type_id: 900_027, ship_id: 55_001, points: 19 };
        let p_unknown = TourneyPilot { character_id: 90_000_002, ship_type_id: 999_999_999, ship_id: 0, points: 10 };
        let telem = compute_team_telemetry(&ctx, snap, &[p_fitted, p_unknown], 42, 3);
        let PyRep::Tuple(vals) = telem else { panic!("expected tuple") };
        assert!(vals[0].as_i64().unwrap() > 1500); // EHP
        assert!(vals[1].as_i64().unwrap() > 50);   // DPS
        assert!(vals[2].as_i64().unwrap() > 0);    // Rep
        assert!(vals[3].as_i64().unwrap() > 0);    // Control
        assert_eq!(vals[4], PyRep::Integer(42));
        assert_eq!(vals[5], PyRep::Integer(3));

        // resolve_pilot for unknown char and char with active_ship_id = 0
        let missing_p = resolve_pilot(&ctx, snap, 999_888_777);
        assert_eq!(missing_p.ship_id, 0);
        assert_eq!(missing_p.points, 0);

        // resolve_valid_home_station: structure match, fallback_sys match, min station fallback
        snap.structures.insert(
            1_000_000_001_234,
            UpwellStructureRecord {
                structure_id: 1_000_000_001_234,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Arena Keep".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0, 0.0, 0.0],
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_unix: 0,
                last_damage_tick: 0,
                self_destruct_at_unix: None,
            },
        );
        assert_eq!(
            resolve_valid_home_station(&ctx, snap, 1_000_000_001_234, 30_000_142),
            (1_000_000_001_234, 30_000_142)
        );
        let (fb_st, fb_sys) = resolve_valid_home_station(&ctx, snap, 999_999_999, 30_000_142);
        assert!(fb_st > 0);
        assert_eq!(fb_sys, 30_000_142);
        let (min_st, _) = resolve_valid_home_station(&ctx, snap, 999_999_999, 30_999_999);
        assert!(min_st > 0);
    });
}
