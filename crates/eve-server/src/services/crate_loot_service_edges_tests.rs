//! Edge-case behavioral unit tests for `crate_loot_service` (`crateService`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::cosmetics_ui_ops::{load_cosmetics_ui_state, save_cosmetics_ui_state};
use eve_db::{AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;

fn find_spec_where<F: Fn(&CrateLootSpec) -> bool>(
    ctx: &ServerContext,
    pred: F,
) -> Option<(u32, CrateLootSpec)> {
    get_crate_specs(ctx)
        .iter()
        .filter(|(&tid, spec)| {
            ctx.sde.types.contains_key(&tid) && crate_loot_spec(ctx, tid).is_some() && pred(spec)
        })
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, spec)| (tid, spec.clone()))
}

fn seed_char_and_crate(
    ctx: &ServerContext,
    char_id: i64,
    station_id: Option<i64>,
    ship_id: i64,
    crate_id: i64,
    type_id: u32,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    let uid = char_id as u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("crate_{uid}"),
            AccountRecord {
                user_id: uid,
                username: format!("crate_{uid}"),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.characters.insert(
            char_id,
            CharacterRecord {
                activity_status: None,
                character_id: char_id,
                user_id: uid,
                name: format!("CrateTester_{char_id}"),
                corporation_id: 98_000_001,
                alliance_id: None,
                war_faction_id: None,
                station_id,
                structure_id: None,
                solar_system_id: 30_000_142,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: eve_db::IskCents(0),
                plex_balance: 0,
                loyalty_points: Default::default(),
                standings: Default::default(),
                security_status: 0.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 11,
                neural: eve_db::NeuralAttributes::default(),
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 0,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id.unwrap_or(30_000_142),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            crate_id,
            InventoryItemRecord {
                item_id: crate_id,
                type_id,
                owner_id: char_id,
                location_id,
                flag,
                quantity,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.db
        .acquire_online_lock(uid, uid + 1000, Some(char_id), true, ctx.now_unix())
        .unwrap();
}

#[test]
fn auth_unknown_method_invalid_ids_and_require_owned_crate_edges() {
    let ctx = test_ctx();
    let mut unauth = SessionState::default();

    for method in [
        "OpenCrate",
        "ClaimLoot",
        "ClaimSkillBundle",
        "ClaimFitting",
        "ClaimLootFromCrateStack",
        "GetCrateLocation",
        "PeekInsideFixedCrate",
    ] {
        assert!(
            handle_crate_service(&ctx, &mut unauth, method, &[PyRep::Integer(1)])
                .unwrap()
                .is_err(),
            "{method} must reject unauthenticated session"
        );
    }
    assert!(handle_crate_service(&ctx, &mut unauth, "unknown_method_fallback", &[]).is_none());

    let char_id = 90_200_001_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 1_000_200_001_i64;
    let sess = auth_session(1, char_id, 98_000_001, 30_000_142);

    // Invalid <= 0 crate / secondary IDs
    for method in [
        "OpenCrate",
        "ClaimLoot",
        "ClaimLootFromCrateStack",
        "GetCrateLocation",
        "PeekInsideFixedCrate",
    ] {
        assert_eq!(
            handle_crate_service(&ctx, &sess, method, &[PyRep::Integer(0)])
                .unwrap()
                .unwrap_err()
                .code,
            "CrateNotFound"
        );
    }
    assert_eq!(
        handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(1), PyRep::Integer(0)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidFittingClaim"
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "ClaimSkillBundle", &[PyRep::Integer(0), PyRep::Integer(1)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkillBundle"
    );

    let (loot_tid, _) = find_spec_where(&ctx, |s| !s.items.is_empty()).expect("loot crate in SDE");

    // Missing crate
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(999_888)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrateNotFound"
    );

    // Wrong owner
    seed_char_and_crate(
        &ctx,
        char_id + 1,
        Some(station_id),
        ship_id,
        700_001,
        loot_tid,
        station_id,
        InventoryFlag::Hangar,
        1,
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(700_001)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrateNotFound"
    );

    // Owned by char_id, reachable via snap.characters fallback (session has station_id=None, ship_id=None),
    // but in wrong station -> InvalidCrateLocation
    seed_char_and_crate(
        &ctx,
        char_id,
        Some(station_id),
        ship_id,
        700_002,
        loot_tid,
        station_id + 99,
        InventoryFlag::Hangar,
        1,
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(700_002)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCrateLocation"
    );

    // Non-crate type (Tritanium 34) in reachable hangar -> InvalidCrateType
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_003,
            InventoryItemRecord {
                item_id: 700_003,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(700_003)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCrateType"
    );
}

#[test]
fn claim_loot_get_location_and_stack_edges() {
    let ctx = test_ctx();
    let char_id = 90_200_002_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 1_000_200_002_i64;
    let (loot_tid, spec) = find_spec_where(&ctx, |s| !s.items.is_empty()).expect("loot crate");

    // In space (station_id = None), crate in ship cargo
    seed_char_and_crate(
        &ctx,
        char_id,
        None,
        ship_id,
        700_101,
        loot_tid,
        ship_id,
        InventoryFlag::Cargo,
        2,
    );
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);
    sess.ship_id = Some(ship_id);

    // GetCrateLocation when undocked falls back to crate_item.location_id (ship_id)
    let loc = handle_crate_service(&ctx, &sess, "GetCrateLocation", &[PyRep::Integer(700_101)])
        .unwrap()
        .unwrap();
    assert_eq!(loc, PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Integer(4)]));

    // ClaimLoot before OpenCrate -> CrateNotOpened
    assert_eq!(
        handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(700_101)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrateNotOpened"
    );

    // OpenCrate succeeds while in space cargo
    let opened = handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(700_101)])
        .unwrap()
        .unwrap();
    assert_eq!(opened.dict_get("opened"), Some(&PyRep::Bool(true)));

    // ClaimLoot while undocked -> DockingRequired
    assert_eq!(
        handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(700_101)])
            .unwrap()
            .unwrap_err()
            .code,
        "DockingRequired"
    );
    // ClaimLootFromCrateStack while undocked -> DockingRequired
    assert_eq!(
        handle_crate_service(&ctx, &sess, "ClaimLootFromCrateStack", &[PyRep::Integer(700_101), PyRep::Integer(1)])
            .unwrap()
            .unwrap_err()
            .code,
        "DockingRequired"
    );

    // Dock character and move crate to hangar
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(station_id);
        let it = snap.items.get_mut(&700_101).unwrap();
        it.location_id = station_id;
        it.flag = InventoryFlag::Hangar;
    });
    sess.station_id = Some(station_id);

    // Wrong flag arg -> InvalidCrateLocation
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimLoot",
            &[PyRep::Integer(700_101), PyRep::Integer(station_id), PyRep::Integer(5)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCrateLocation"
    );

    // Quantity == 2 claim decrements to quantity == 1 and delivers items
    let _ = ctx.drain_notifications_for_char(char_id);
    let claimed1 = handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(700_101)])
        .unwrap()
        .unwrap();
    assert_eq!(claimed1.dict_get("crateID"), Some(&PyRep::Integer(700_101)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&700_101).unwrap().quantity), 1);
    assert!(!ctx.drain_notifications_for_char(char_id).is_empty());

    // Simulate already-claimed entry while item & opened state exist -> LootAlreadyClaimed
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.opened_crates.insert(700_101, ctx.now_unix());
        ext.claimed_crate_loot.entry(char_id).or_default().push(700_101);
        save_cosmetics_ui_state(snap, &ext);
    });
    assert_eq!(
        handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(700_101)])
            .unwrap()
            .unwrap_err()
            .code,
        "LootAlreadyClaimed"
    );

    // ClaimLootFromCrateStack with qty > actual removes crate cleanly
    seed_char_and_crate(
        &ctx,
        char_id,
        Some(station_id),
        ship_id,
        700_102,
        loot_tid,
        station_id,
        InventoryFlag::Hangar,
        2,
    );
    let stack_res = handle_crate_service(
        &ctx,
        &sess,
        "ClaimLootFromCrateStack",
        &[PyRep::Integer(700_102), PyRep::Integer(10)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(stack_res, PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(10)]));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&700_102)));

    // If SDE has a crate without fixed items, ClaimLootFromCrateStack returns InvalidCrateType
    if let Some((no_loot_tid, _)) = find_spec_where(&ctx, |s| s.items.is_empty()) {
        let _ = spec;
        seed_char_and_crate(
            &ctx,
            char_id,
            Some(station_id),
            ship_id,
            700_103,
            no_loot_tid,
            station_id,
            InventoryFlag::Hangar,
            1,
        );
        assert_eq!(
            handle_crate_service(
                &ctx,
                &sess,
                "ClaimLootFromCrateStack",
                &[PyRep::Integer(700_103), PyRep::Integer(1)],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidCrateType"
        );
    }
}

#[test]
fn claim_fitting_and_claim_skill_bundle_edges() {
    let ctx = test_ctx();
    let char_id = 90_200_003_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 1_000_200_003_i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);

    // 1. Fitting edges (if SDE contains a fitting crate)
    if let Some((fit_tid, fit_spec)) =
        find_spec_where(&ctx, |s| s.fitting_ship_type_id.is_some() && !s.fittings.is_empty())
    {
        let valid_fit = i64::from(fit_spec.fittings[0]);
        seed_char_and_crate(
            &ctx,
            char_id,
            None,
            ship_id,
            700_201,
            fit_tid,
            ship_id,
            InventoryFlag::Cargo,
            2,
        );
        sess.ship_id = Some(ship_id);
        // Undocked -> DockingRequired
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(700_201), PyRep::Integer(valid_fit)])
                .unwrap()
                .unwrap_err()
                .code,
            "DockingRequired"
        );

        // Docked, unopened -> CrateNotOpened
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().station_id = Some(station_id);
            let it = snap.items.get_mut(&700_201).unwrap();
            it.location_id = station_id;
            it.flag = InventoryFlag::Hangar;
        });
        sess.station_id = Some(station_id);
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(700_201), PyRep::Integer(valid_fit)])
                .unwrap()
                .unwrap_err()
                .code,
            "CrateNotOpened"
        );

        // Open, then test wrong fitting ID -> InvalidCrateFitting
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(700_201)]).unwrap().unwrap();
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(700_201), PyRep::Integer(999_999_999)])
                .unwrap()
                .unwrap_err()
                .code,
            "InvalidCrateFitting"
        );

        // Claim qty=2 -> decrements to 1
        let fit_res = handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(700_201), PyRep::Integer(valid_fit)])
            .unwrap()
            .unwrap();
        assert_eq!(fit_res.dict_get("claimed"), Some(&PyRep::Bool(true)));
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&700_201).unwrap().quantity), 1);

        // Mark fitting already claimed -> FittingAlreadyClaimed
        let key = 700_201_i64 * 10_000 + valid_fit;
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_cosmetics_ui_state(snap);
            ext.claimed_crate_loot.entry(char_id).or_default().push(key);
            save_cosmetics_ui_state(snap, &ext);
        });
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(700_201), PyRep::Integer(valid_fit)])
                .unwrap()
                .unwrap_err()
                .code,
            "FittingAlreadyClaimed"
        );
    }

    // Crate without fittings returns InvalidCrateFitting
    if let Some((no_fit_tid, _)) = find_spec_where(&ctx, |s| s.fitting_ship_type_id.is_none()) {
        seed_char_and_crate(
            &ctx,
            char_id,
            Some(station_id),
            ship_id,
            700_202,
            no_fit_tid,
            station_id,
            InventoryFlag::Hangar,
            1,
        );
        sess.station_id = Some(station_id);
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(700_202)]).unwrap().unwrap();
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimFitting", &[PyRep::Integer(700_202), PyRep::Integer(1)])
                .unwrap()
                .unwrap_err()
                .code,
            "InvalidCrateFitting"
        );
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimSkillBundle", &[PyRep::Integer(700_202), PyRep::Integer(1)])
                .unwrap()
                .unwrap_err()
                .code,
            "InvalidSkillBundle"
        );
    }

    // 2. Skill bundle edges
    if let Some((sb_tid, sb_spec)) = find_spec_where(&ctx, |s| s.skill_bundle.is_some()) {
        let bundle_id = sb_spec.skill_bundle.unwrap().0;
        seed_char_and_crate(
            &ctx,
            char_id,
            None,
            ship_id,
            700_301,
            sb_tid,
            ship_id,
            InventoryFlag::Cargo,
            2,
        );
        sess.station_id = None;
        sess.ship_id = Some(ship_id);

        // Undocked -> DockingRequired
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimSkillBundle", &[PyRep::Integer(700_301), PyRep::Integer(bundle_id)])
                .unwrap()
                .unwrap_err()
                .code,
            "DockingRequired"
        );

        // Docked, unopened -> CrateNotOpened
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().station_id = Some(station_id);
            let it = snap.items.get_mut(&700_301).unwrap();
            it.location_id = station_id;
            it.flag = InventoryFlag::Hangar;
        });
        sess.station_id = Some(station_id);
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimSkillBundle", &[PyRep::Integer(700_301), PyRep::Integer(bundle_id)])
                .unwrap()
                .unwrap_err()
                .code,
            "CrateNotOpened"
        );

        // Open, then wrong bundle_id -> InvalidSkillBundle
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(700_301)]).unwrap().unwrap();
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimSkillBundle", &[PyRep::Integer(700_301), PyRep::Integer(bundle_id + 999)])
                .unwrap()
                .unwrap_err()
                .code,
            "InvalidSkillBundle"
        );

        // Claim qty=2 -> decrements to 1
        let sb_res = handle_crate_service(&ctx, &sess, "ClaimSkillBundle", &[PyRep::Integer(700_301), PyRep::Integer(bundle_id)])
            .unwrap()
            .unwrap();
        assert_eq!(sb_res.dict_get("skillBundleID"), Some(&PyRep::Integer(bundle_id)));
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&700_301).unwrap().quantity), 1);

        // Simulate already-claimed while opened -> SkillBundleAlreadyClaimed
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_cosmetics_ui_state(snap);
            ext.opened_crates.insert(700_301, ctx.now_unix());
            ext.claimed_crate_loot.entry(char_id).or_default().push(700_301);
            save_cosmetics_ui_state(snap, &ext);
        });
        assert_eq!(
            handle_crate_service(&ctx, &sess, "ClaimSkillBundle", &[PyRep::Integer(700_301), PyRep::Integer(bundle_id)])
                .unwrap()
                .unwrap_err()
                .code,
            "SkillBundleAlreadyClaimed"
        );

        // CharacterNotFound path: crate owned by missing char_id in session
        let ghost_char = 90_200_999_i64;
        let mut ghost_sess = auth_session(1, ghost_char, 98_000_001, 30_000_142);
        ghost_sess.station_id = Some(station_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                700_302,
                InventoryItemRecord {
                    item_id: 700_302,
                    type_id: sb_tid,
                    owner_id: ghost_char,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    ..Default::default()
                },
            );
        });
        handle_crate_service(&ctx, &ghost_sess, "OpenCrate", &[PyRep::Integer(700_302)]).unwrap().unwrap();
        assert_eq!(
            handle_crate_service(&ctx, &ghost_sess, "ClaimSkillBundle", &[PyRep::Integer(700_302), PyRep::Integer(bundle_id)])
                .unwrap()
                .unwrap_err()
                .code,
            "CharacterNotFound"
        );
    }
}

#[test]
fn crate_claim_exact_qty_one_vs_stack_and_active_ship_id_guard_kill_mutants() {
    let ctx = test_ctx();
    let station_id = 60_003_760_i64;
    let char_id = 90_201_001_i64;
    let ship_id = 1_201_001_i64;
    let mut sess = auth_session(201, char_id, 98_000_001, 30_000_142);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);

    // 1. ClaimLoot with quantity=2: must NOT add crate_id to claimed_crate_loot on first claim,
    //    then claiming the final unit (quantity=1) MUST push crate_id into claimed_crate_loot.
    if let Some((loot_tid, _)) = find_spec_where(&ctx, |s| !s.items.is_empty()) {
        let crate_id = 701_101_i64;
        seed_char_and_crate(
            &ctx,
            char_id,
            Some(station_id),
            ship_id,
            crate_id,
            loot_tid,
            station_id,
            InventoryFlag::Hangar,
            2,
        );
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(crate_id)])
            .unwrap()
            .unwrap();
        let first = handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(crate_id)])
            .unwrap()
            .unwrap();
        assert_eq!(first.dict_get("crateID"), Some(&PyRep::Integer(crate_id)));
        assert_eq!(
            ctx.db.with_snapshot(|s| s.items.get(&crate_id).unwrap().quantity),
            1
        );
        assert!(
            ctx.db.with_snapshot(|s| {
                !load_cosmetics_ui_state(s)
                    .claimed_crate_loot
                    .get(&char_id)
                    .is_some_and(|v| v.contains(&crate_id))
            }),
            "qty=2 ClaimLoot must not mark crate_id claimed until qty==1"
        );

        // Second claim when qty==1 succeeds and records crate_id in claimed_crate_loot
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(crate_id)])
            .unwrap()
            .unwrap();
        let second = handle_crate_service(&ctx, &sess, "ClaimLoot", &[PyRep::Integer(crate_id)])
            .unwrap()
            .unwrap();
        assert_eq!(second.dict_get("crateID"), Some(&PyRep::Integer(crate_id)));
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&crate_id)));
        assert!(
            ctx.db.with_snapshot(|s| {
                load_cosmetics_ui_state(s)
                    .claimed_crate_loot
                    .get(&char_id)
                    .is_some_and(|v| v.contains(&crate_id))
            }),
            "qty==1 ClaimLoot must record crate_id in claimed_crate_loot"
        );

        // active_ship_id guard: if session.ship_id == Some(crate_id) (while ch.active_ship_id != crate_id),
        // require_owned_crate rejects ClaimLoot and ClaimLootFromCrateStack with InvalidCrateLocation.
        let blocked_id = 701_102_i64;
        seed_char_and_crate(
            &ctx,
            char_id,
            Some(station_id),
            ship_id,
            blocked_id,
            loot_tid,
            station_id,
            InventoryFlag::Hangar,
            2,
        );
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(blocked_id)])
            .unwrap()
            .unwrap();
        let mut hijacked_sess = sess.clone();
        hijacked_sess.ship_id = Some(blocked_id);
        assert_eq!(
            handle_crate_service(&ctx, &hijacked_sess, "ClaimLoot", &[PyRep::Integer(blocked_id)])
                .unwrap()
                .unwrap_err()
                .code,
            "InvalidCrateLocation"
        );
        assert_eq!(
            handle_crate_service(
                &ctx,
                &hijacked_sess,
                "ClaimLootFromCrateStack",
                &[PyRep::Integer(blocked_id), PyRep::Integer(1)],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidCrateLocation"
        );
    }

    // 2. ClaimFitting with qty=1 vs qty=2 + active_ship_id guard
    if let Some((fit_tid, fit_spec)) =
        find_spec_where(&ctx, |s| s.fitting_ship_type_id.is_some() && !s.fittings.is_empty())
    {
        let valid_fit = i64::from(fit_spec.fittings[0]);
        let fit_stack_id = 701_201_i64;
        seed_char_and_crate(
            &ctx,
            char_id,
            Some(station_id),
            ship_id,
            fit_stack_id,
            fit_tid,
            station_id,
            InventoryFlag::Hangar,
            2,
        );
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(fit_stack_id)])
            .unwrap()
            .unwrap();
        let r1 = handle_crate_service(
            &ctx,
            &sess,
            "ClaimFitting",
            &[PyRep::Integer(fit_stack_id), PyRep::Integer(valid_fit)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(r1.dict_get("claimed"), Some(&PyRep::Bool(true)));
        let key = fit_stack_id.saturating_mul(10_000).saturating_add(valid_fit);
        assert!(
            ctx.db.with_snapshot(|s| {
                let claimed = load_cosmetics_ui_state(s).claimed_crate_loot.get(&char_id).cloned().unwrap_or_default();
                !claimed.contains(&key) && !claimed.contains(&fit_stack_id)
            }),
            "qty=2 ClaimFitting must not mark claimed before last unit"
        );

        // Remaining qty=1 unit can be opened & claimed cleanly, and then records key + crate_id
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(fit_stack_id)])
            .unwrap()
            .unwrap();
        let r2 = handle_crate_service(
            &ctx,
            &sess,
            "ClaimFitting",
            &[PyRep::Integer(fit_stack_id), PyRep::Integer(valid_fit)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(r2.dict_get("claimed"), Some(&PyRep::Bool(true)));
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&fit_stack_id)));
        assert!(ctx.db.with_snapshot(|s| {
            let claimed = load_cosmetics_ui_state(s).claimed_crate_loot.get(&char_id).cloned().unwrap_or_default();
            claimed.contains(&key) && claimed.contains(&fit_stack_id)
        }));

        // active_ship_id guard for ClaimFitting
        let fit_locked_id = 701_202_i64;
        seed_char_and_crate(
            &ctx,
            char_id,
            Some(station_id),
            ship_id,
            fit_locked_id,
            fit_tid,
            station_id,
            InventoryFlag::Hangar,
            1,
        );
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(fit_locked_id)])
            .unwrap()
            .unwrap();
        let mut hijacked_sess = sess.clone();
        hijacked_sess.ship_id = Some(fit_locked_id);
        assert_eq!(
            handle_crate_service(
                &ctx,
                &hijacked_sess,
                "ClaimFitting",
                &[PyRep::Integer(fit_locked_id), PyRep::Integer(valid_fit)],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidCrateLocation"
        );
    }

    // 3. ClaimSkillBundle with qty=2 -> qty=1 clean transition + active_ship_id guard
    if let Some((sb_tid, sb_spec)) = find_spec_where(&ctx, |s| s.skill_bundle.is_some()) {
        let (bundle_id, sp_grant) = sb_spec.skill_bundle.unwrap();
        let sb_id = 701_301_i64;
        seed_char_and_crate(
            &ctx,
            char_id,
            Some(station_id),
            ship_id,
            sb_id,
            sb_tid,
            station_id,
            InventoryFlag::Hangar,
            2,
        );
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(sb_id)])
            .unwrap()
            .unwrap();
        let res1 = handle_crate_service(
            &ctx,
            &sess,
            "ClaimSkillBundle",
            &[PyRep::Integer(sb_id), PyRep::Integer(bundle_id)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(res1.dict_get("spGranted"), Some(&PyRep::Real(sp_grant)));
        let key = sb_id.saturating_mul(1000).saturating_add(bundle_id);
        assert!(ctx.db.with_snapshot(|s| {
            let claimed = load_cosmetics_ui_state(s).claimed_crate_loot.get(&char_id).cloned().unwrap_or_default();
            !claimed.contains(&key) && !claimed.contains(&sb_id)
        }));

        // active_ship_id guard before claiming final unit
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(sb_id)])
            .unwrap()
            .unwrap();
        let mut hijacked_sess = sess.clone();
        hijacked_sess.ship_id = Some(sb_id);
        assert_eq!(
            handle_crate_service(
                &ctx,
                &hijacked_sess,
                "ClaimSkillBundle",
                &[PyRep::Integer(sb_id), PyRep::Integer(bundle_id)],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidCrateLocation"
        );

        // Final qty=1 claim records key + sb_id and consumes crate
        let res2 = handle_crate_service(
            &ctx,
            &sess,
            "ClaimSkillBundle",
            &[PyRep::Integer(sb_id), PyRep::Integer(bundle_id)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(res2.dict_get("crateID"), Some(&PyRep::Integer(sb_id)));
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&sb_id)));
        assert!(ctx.db.with_snapshot(|s| {
            let claimed = load_cosmetics_ui_state(s).claimed_crate_loot.get(&char_id).cloned().unwrap_or_default();
            claimed.contains(&key) && claimed.contains(&sb_id)
        }));
    }
}
