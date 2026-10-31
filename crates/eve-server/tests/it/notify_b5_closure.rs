//! Real server-side `notify:On*` emission integration tests (Batch 5).
//!
//! Covers Bookmarks, Implants & Boosters, Structure Docking & Undocking, Corporation Office
//! Rentals, NPC Standings, and Skill / Skill Queue updates via live RPCs and state mutations.

#![forbid(unsafe_code)]

use eve_db::{corp_roles::ROLE_RENT_OFFICE, InventoryFlag, InventoryItemRecord, IskCents, ServerClock, TrainedSkillRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(ev, _)| ev == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

fn has_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> bool {
    notifs.iter().any(|(ev, _)| ev == event)
}

async fn setup_docked_pilot(ctx: &ServerContext, char_id: i64, ship_id: i64) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_JITA;
            ch.station_id = Some(JITA_STATION);
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, 622, char_id, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    let mut sess = SessionState::new(char_id as u64);
    crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = Some(JITA_STATION);
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

/// parity: notify:OnBookmarkCreated
/// parity: notify:OnRefreshBookmarks
/// parity: notify:OnOfficeRentalChanged
#[tokio::test]
async fn batch5_bookmarks_and_office_rental_notifications() {
    let ctx = ServerContext::new();
    let mut sess = setup_docked_pilot(&ctx, PILOT_A, 710_001).await;

    // 1. Negative: empty bookmark label fails and does not emit OnBookmarkCreated.
    let err = rpc(
        &ctx,
        &mut sess,
        "bookmark",
        "BookmarkLocation",
        &[PyRep::String("   ".into())],
    )
    .await
    .expect_err("empty label must fail");
    assert_eq!(err.code, "InvalidBookmarkLabel");
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnBookmarkCreated"));

    // 2. Positive: BookmarkLocation creates bookmark and emits OnBookmarkCreated(bookmarkID, comment, typeID).
    let res = rpc(
        &ctx,
        &mut sess,
        "bookmark",
        "BookmarkLocation",
        &[PyRep::String("Jita Safe".into())],
    )
    .await
    .expect("BookmarkLocation");
    let bm_id = res
        .dict_get("bookmarkID")
        .and_then(|v| v.as_i64())
        .expect("bookmarkID");
    let notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let args = find_notif(&notifs, "OnBookmarkCreated");
    assert_eq!(args.len(), 3);
    assert_eq!(args[0].as_i64(), Some(bm_id));
    assert_eq!(args[1].as_str(), Some("Jita Safe"));
    assert!(args[2].as_i64().unwrap_or(0) > 0);
    assert!(ctx.db.with_snapshot(|s| s.bookmarks.contains_key(&bm_id)));

    // 3. Negative: DeleteBookmarks with non-existent ID fails and does not emit OnRefreshBookmarks.
    let del_err = rpc(
        &ctx,
        &mut sess,
        "bookmark",
        "DeleteBookmarks",
        &[PyRep::List(vec![PyRep::Integer(999_999_999)])],
    )
    .await
    .expect_err("non-existent bookmark must fail");
    assert_eq!(del_err.code, "BookmarkNotFound");
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnRefreshBookmarks"));

    // 4. Positive: DeleteBookmarks removes bookmark and emits OnRefreshBookmarks().
    rpc(
        &ctx,
        &mut sess,
        "bookmark",
        "DeleteBookmarks",
        &[PyRep::List(vec![PyRep::Integer(bm_id)])],
    )
    .await
    .expect("DeleteBookmarks");
    let del_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&del_notifs, "OnRefreshBookmarks").len(), 0);
    assert!(!ctx.db.with_snapshot(|s| s.bookmarks.contains_key(&bm_id)));

    // 5. Negative: RentOffice without ROLE_RENT_OFFICE fails and does not emit OnOfficeRentalChanged.
    let corp_id = 98_000_001;
    sess.corporation_id = Some(corp_id);
    sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Test Corp B5".into(),
                ticker: "TCB5".into(),
                ceo_id: 90_000_002,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents(1_000_000_000),
                members: [PILOT_A].into_iter().collect(),
                member_roles: HashMap::new(),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                hq_station_id: Some(JITA_STATION),
                url: String::new(),
            },
        );
    });
    let role_err = rpc(
        &ctx,
        &mut sess,
        "officeManager",
        "RentOffice",
        &[PyRep::Integer(JITA_STATION)],
    )
    .await
    .expect_err("RentOffice without role must fail");
    assert_eq!(role_err.code, "CorpRoleDenied");
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnOfficeRentalChanged"));

    // 6. Positive: RentOffice with ROLE_RENT_OFFICE and corp ISK emits OnOfficeRentalChanged(corporationID, officeID).
    sess.corp_role = ROLE_RENT_OFFICE;
    let rent_res = rpc(
        &ctx,
        &mut sess,
        "officeManager",
        "RentOffice",
        &[PyRep::Integer(JITA_STATION)],
    )
    .await
    .expect("RentOffice");
    let office_id = rent_res
        .dict_get("officeFolderID")
        .and_then(|v| v.as_i64())
        .expect("officeFolderID");
    let corp_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let office_args = find_notif(&corp_notifs, "OnOfficeRentalChanged");
    assert_eq!(office_args.len(), 2);
    assert_eq!(office_args[0].as_i64(), Some(corp_id));
    assert_eq!(office_args[1].as_i64(), Some(office_id));
}

/// parity: notify:OnImplantsChanged
/// parity: notify:OnBoosterUpdated
#[tokio::test]
async fn batch5_implants_and_boosters_notifications() {
    let ctx = ServerContext::new();
    let mut sess = setup_docked_pilot(&ctx, PILOT_A, 710_002).await;

    let implant_item_id = 720_001;
    let booster_item_id = 720_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            implant_item_id,
            make_item(implant_item_id, 9899, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            booster_item_id,
            make_item(booster_item_id, 9950, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });

    // 1. Negative: InjectImplant without Cybernetics I fails and does not emit OnImplantsChanged.
    let imp_err = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "InjectImplant",
        &[PyRep::Integer(implant_item_id)],
    )
    .await
    .expect_err("Cybernetics I required");
    assert_eq!(imp_err.code, "SkillPrerequisitesNotMet");
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnImplantsChanged"));

    // 2. Positive: InjectImplant with Cybernetics I succeeds and emits OnImplantsChanged().
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_A).or_default().insert(
            3411,
            TrainedSkillRecord { type_id: 3411, level: 1, skill_points: 250.0 },
        );
    });
    rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "InjectImplant",
        &[PyRep::Integer(implant_item_id)],
    )
    .await
    .expect("InjectImplant");
    let imp_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&imp_notifs, "OnImplantsChanged").len(), 0);
    assert!(ctx.db.with_snapshot(|s| s.active_implants.get(&PILOT_A).is_some_and(|v| v.contains(&9899))));

    // 3. Negative: UseBooster without Biology I fails and does not emit OnBoosterUpdated.
    let bst_err = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "UseBooster",
        &[PyRep::Integer(booster_item_id)],
    )
    .await
    .expect_err("Biology I required");
    assert_eq!(bst_err.code, "SkillPrerequisitesNotMet");
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnBoosterUpdated"));

    // 4. Positive: UseBooster with Biology I succeeds and emits OnBoosterUpdated().
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_A).or_default().insert(
            3405,
            TrainedSkillRecord { type_id: 3405, level: 1, skill_points: 250.0 },
        );
    });
    rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "UseBooster",
        &[PyRep::Integer(booster_item_id)],
    )
    .await
    .expect("UseBooster");
    let bst_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&bst_notifs, "OnBoosterUpdated").len(), 0);
}

/// parity: notify:OnUndockingStarted
/// parity: notify:OnUndockingCompleted
/// parity: notify:OnDockingRequestWasDenied
#[tokio::test]
async fn batch5_undocking_and_docking_denied_notifications() {
    let ctx = ServerContext::new();
    let ship_id = 710_003;
    let mut sess = setup_docked_pilot(&ctx, PILOT_A, ship_id).await;

    // 1. Positive: Undock from Jita station emits OnUndockingStarted(location_id) and OnUndockingCompleted(itemID).
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    let undock_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let started_args = find_notif(&undock_notifs, "OnUndockingStarted");
    assert_eq!(started_args.len(), 1);
    assert_eq!(started_args[0].as_i64(), Some(JITA_STATION));

    let completed_args = find_notif(&undock_notifs, "OnUndockingCompleted");
    assert_eq!(completed_args.len(), 1);
    assert_eq!(completed_args[0].as_i64(), Some(ship_id));
    assert!(sess.station_id.is_none());

    // 2. Negative: Undock when already in space fails and does not emit OnUndockingStarted.
    let undock_err = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect_err("Undock in space must fail");
    assert_eq!(undock_err.code, "UndockFailed");
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnUndockingStarted"));

    // 3. Positive: Docking at an Upwell structure with ACL denying PILOT_A emits OnDockingRequestWasDenied(structureID).
    let structure_id = 1_000_000_000_555;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: structure_id,
                type_id: 35832,
                position: DVec3::new(1000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 5000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            Default::default(),
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35832,
                owner_corp_id: 98_999_999,
                solar_system_id: SYS_JITA,
                name: "Denied Astrahus".into(),
                position: [1000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let clk = eve_db::VirtualServerClock::new();
    clk.advance_secs(15);
    ctx.set_clock(std::sync::Arc::new(clk));
    let dock_err = rpc(
        &ctx,
        &mut sess,
        "structureDocking",
        "Dock",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect_err("Dock at ACL-denied structure must fail");
    assert_eq!(dock_err.code, "StructureDockingAccessDenied");
    let dock_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let denied_args = find_notif(&dock_notifs, "OnDockingRequestWasDenied");
    assert_eq!(denied_args.len(), 1);
    assert_eq!(denied_args[0].as_i64(), Some(structure_id));
}

/// parity: notify:OnNPCStandingChange
/// parity: notify:OnSkillsChanged
/// parity: notify:OnSkillQueueChanged
/// parity: notify:OnSkillQueuePaused
#[tokio::test]
async fn batch5_standings_and_skill_queue_notifications() {
    let ctx = ServerContext::new();
    let mut sess = setup_docked_pilot(&ctx, PILOT_A, 710_004).await;

    // 1. Negative: zero-delta standing change does not emit OnNPCStandingChange.
    eve_server::services::agents_missions::agents_standings_arcs::notify_standing_changes(
        &ctx,
        PILOT_A,
        &[(1_000_035, 2.5, 0.0)],
    );
    assert!(!has_notif(&drain_all(&ctx, &mut sess, PILOT_A), "OnNPCStandingChange"));

    // 2. Positive: non-zero standing change emits OnNPCStandingChange(fromID, newStanding, oldStanding).
    eve_server::services::agents_missions::agents_standings_arcs::notify_standing_changes(
        &ctx,
        PILOT_A,
        &[(1_000_035, 3.0, 0.5)],
    );
    let st_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let st_args = find_notif(&st_notifs, "OnNPCStandingChange");
    assert_eq!(st_args.len(), 3);
    assert_eq!(st_args[0].as_i64(), Some(1_000_035));
    assert!((st_args[1].as_f64().unwrap_or(0.0) - 3.0).abs() < 1e-6);
    assert!((st_args[2].as_f64().unwrap_or(0.0) - 2.5).abs() < 1e-6);

    // 3. Positive: ApplyFreeSkillPoints updates skill SP and emits OnSkillsChanged(skills).
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.free_skill_points = 50_000.0;
        }
        snap.skills.entry(PILOT_A).or_default().insert(
            3402,
            TrainedSkillRecord { type_id: 3402, level: 1, skill_points: 250.0 },
        );
    });
    rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(3402), PyRep::Integer(1_200)],
    )
    .await
    .expect("ApplyFreeSkillPoints");
    let sk_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let sk_args = find_notif(&sk_notifs, "OnSkillsChanged");
    assert_eq!(sk_args.len(), 1);
    assert!(matches!(sk_args[0], PyRep::Dict(_)));

    // 4. Positive: SaveSkillQueue with a skill emits OnSkillQueueChanged(), and empty SaveSkillQueue emits OnSkillQueuePaused().
    rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3402), PyRep::Integer(3)])])],
    )
    .await
    .expect("SaveSkillQueue");
    let q_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&q_notifs, "OnSkillQueueChanged").len(), 0);

    rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![])],
    )
    .await
    .expect("SaveSkillQueue empty");
    let p_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&p_notifs, "OnSkillQueuePaused").len(), 0);
}
