//! Unit tests for `fleet_social_fleet_ops` broadcasts, warps, MOTD, options, and preferences.

#[path = "fleet_social_fleet_ops_warp_tests.rs"]
mod fleet_social_fleet_ops_warp_tests;

use super::*;
use eve_db::{
    CharacterRecord, CorporationRecord, FleetRecord, FleetSquadRecord, FleetWingRecord,
    InventoryFlag, InventoryItemRecord, IskCents, VirtualServerClock, WorldStateSnapshot,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) fn test_ctx_and_clock(ts: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(ts));
    let ctx = ServerContext::new();
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

pub(crate) fn test_ctx_with_clock(ts: u64) -> ServerContext {
    test_ctx_and_clock(ts).0
}

pub(crate) fn clone_char_with(
    snap: &WorldStateSnapshot,
    cid: i64,
    name: &str,
    corp_id: i64,
    sys_id: u32,
    ship_id: i64,
) -> CharacterRecord {
    let mut ch = snap.characters.values().next().expect("demo char seeded").clone();
    ch.character_id = cid;
    ch.user_id = cid as u64;
    ch.name = name.to_string();
    ch.corporation_id = corp_id;
    ch.alliance_id = None;
    ch.station_id = None;
    ch.structure_id = None;
    ch.solar_system_id = sys_id;
    ch.active_ship_id = ship_id;
    ch.balance = IskCents::from_isk(100_000_000);
    ch
}

pub(crate) async fn seed_multi_member_fleet(ctx: &ServerContext) -> (i64, SessionState) {
    let fleet_id = 700_001;
    let wing1 = 710_001;
    let wing2 = 710_002;
    let squad1 = 720_001;
    let squad2 = 720_002;
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let perimeter = ctx.systems.get_or_spawn(30_000_144);

    let chars = [
        (90_000_001_i64, "FC Boss", 30_000_142_u32, 100_000_001_i64, 587_u32, DVec3::new(0.0, 0.0, 0.0)),
        (90_000_002_i64, "WC Bubble", 30_000_142_u32, 100_000_002_i64, 588_u32, DVec3::new(15_000.0, 0.0, 0.0)),
        (90_000_003_i64, "SC Far", 30_000_142_u32, 100_000_003_i64, 589_u32, DVec3::new(400_000.0, 0.0, 0.0)),
        (90_000_004_i64, "Mem Wing2", 30_000_142_u32, 100_000_004_i64, 590_u32, DVec3::new(25_000.0, 0.0, 0.0)),
        (90_000_005_i64, "Mem Perim", 30_000_144_u32, 100_000_005_i64, 591_u32, DVec3::new(5_000.0, 0.0, 0.0)),
    ];

    for &(_, _, sys_id, ship_id, type_id, pos) in &chars {
        let handle = if sys_id == 30_000_142 { &jita } else { &perimeter };
        let _ = handle
            .spawn_ball(Ball {
                id: ship_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, name, sys_id, ship_id, type_id, pos) in &chars {
            let ch = clone_char_with(snap, cid, name, 98_000_001, sys_id, ship_id);
            snap.characters.insert(cid, ch);
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            snap.item_positions.insert(ship_id, pos.to_array());
        }
        snap.corporations.insert(
            98_000_001,
            CorporationRecord {
                corporation_id: 98_000_001,
                name: "Test Corp".into(),
                ticker: "TSTC".into(),
                ceo_id: 90_000_001,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(100_000_000),
                members: chars.iter().map(|c| c.0).collect(),
                ..Default::default()
            },
        );
        let mut details = HashMap::new();
        details.insert(
            90_000_001,
            FleetMemberRecord {
                character_id: 90_000_001,
                role: FleetRole::FleetCommander,
                wing_id: Some(wing1),
                squad_id: Some(squad1),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: notify::FLEET_JOB_CREATOR,
                role_name: "Fleet Commander".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        details.insert(
            90_000_002,
            FleetMemberRecord {
                character_id: 90_000_002,
                role: FleetRole::WingCommander,
                wing_id: Some(wing1),
                squad_id: Some(squad1),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Wing Commander".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        details.insert(
            90_000_003,
            FleetMemberRecord {
                character_id: 90_000_003,
                role: FleetRole::SquadCommander,
                wing_id: Some(wing1),
                squad_id: Some(squad1),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Squad Commander".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        details.insert(
            90_000_004,
            FleetMemberRecord {
                character_id: 90_000_004,
                role: FleetRole::SquadMember,
                wing_id: Some(wing2),
                squad_id: Some(squad2),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Squad Member".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        details.insert(
            90_000_005,
            FleetMemberRecord {
                character_id: 90_000_005,
                role: FleetRole::SquadMember,
                wing_id: Some(wing2),
                squad_id: Some(squad2),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Squad Member".into(),
                join_time_unix: 1_700_000_000,
            },
        );
        let mut wing_details = HashMap::new();
        wing_details.insert(
            wing1,
            FleetWingRecord {
                wing_id: wing1,
                name: "Wing 1".into(),
                commander_id: Some(90_000_002),
                squad_ids: vec![squad1],
            },
        );
        wing_details.insert(
            wing2,
            FleetWingRecord {
                wing_id: wing2,
                name: "Wing 2".into(),
                commander_id: None,
                squad_ids: vec![squad2],
            },
        );
        let mut squad_details = HashMap::new();
        squad_details.insert(
            squad1,
            FleetSquadRecord {
                squad_id: squad1,
                wing_id: wing1,
                name: "Squad 1".into(),
                commander_id: Some(90_000_003),
            },
        );
        squad_details.insert(
            squad2,
            FleetSquadRecord {
                squad_id: squad2,
                wing_id: wing2,
                name: "Squad 2".into(),
                commander_id: None,
            },
        );
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: 90_000_001,
                members: chars.iter().map(|c| c.0).collect(),
                wings: vec![wing1, wing2],
                squads: vec![squad1, squad2],
                free_move: false,
                is_registered: false,
                wing_details,
                squad_details,
                member_details: details,
                invites: HashSet::new(),
                motd: String::new(),
            },
        );
    });

    let mut session = SessionState::new(1);
    session.user_id = Some(90_000_001);
    session.character_id = Some(90_000_001);
    session.corporation_id = Some(98_000_001);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(100_000_001);
    session.fleet_id = Some(fleet_id);
    (fleet_id, session)
}

#[tokio::test]
async fn fleet_ops_broadcasts_filters_and_cooldown_cover_scopes() {
    let ctx = test_ctx_with_clock(1_800_000_100);
    let (fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Unauthenticated & not-in-fleet errors
    let mut no_auth = SessionState::new(2);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut no_auth, "fleetMgr", "SendBroadcast", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    let mut no_fleet = SessionState::new(3);
    no_fleet.user_id = Some(90_000_001);
    no_fleet.character_id = Some(90_000_001);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut no_fleet, "fleetMgr", "SendBroadcast", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    // No solar system check
    let saved_sys = session.solar_system_id.take();
    assert_eq!(
        handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "SendBroadcast", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSolarSystem"
    );
    session.solar_system_id = saved_sys;

    // SquadMember denied restricted broadcast ("Target"), allowed distress ("NeedShield")
    let mut mem_sess = SessionState::new(4);
    mem_sess.user_id = Some(90_000_004);
    mem_sess.character_id = Some(90_000_004);
    mem_sess.solar_system_id = Some(30_000_142);
    mem_sess.ship_id = Some(100_000_004);
    mem_sess.fleet_id = Some(fleet_id);
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut mem_sess,
            "fleetMgr",
            "SendBroadcast",
            &[PyRep::String("Target".into()), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotBroadcastInFleet"
    );
    let need_res = handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("NeedShield".into()), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(need_kv) = need_res else {
        panic!("expected KeyVal");
    };
    assert_eq!(need_kv.get("broadcast").unwrap().as_str().unwrap(), "NeedShield");

    // Filter out "EnemySpotted" on member 90_000_004
    ctx.fleet_extras
        .lock()
        .unwrap()
        .broadcast_filters
        .entry(90_000_004)
        .or_default()
        .insert("EnemySpotted".into());

    // FC global broadcast across all 5 members (excluding muted 90_000_004)
    let g_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("EnemySpotted".into()), PyRep::Integer(0), PyRep::Integer(800_001), PyRep::Integer(587)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(g_kv) = g_res else { panic!() };
    let PyRep::List(recips) = g_kv.get("recipientIDs").unwrap() else { panic!() };
    let r_ids: HashSet<i64> = recips.iter().filter_map(|r| r.as_i64()).collect();
    assert!(r_ids.contains(&90_000_001));
    assert!(r_ids.contains(&90_000_005));
    assert!(!r_ids.contains(&90_000_004));

    // Spam cooldown within 2 seconds on identical broadcast
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetObjectHandler",
            "SendBroadcast",
            &[PyRep::String("EnemySpotted".into()), PyRep::Integer(0), PyRep::Integer(800_001), PyRep::Integer(587)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "BroadcastSpamCooldown"
    );

    ctx.advance_secs(3);

    // Bubble broadcast (location=2) includes on-grid 90_000_002 (15km) and 90_000_004 (25km), excludes 90_000_003 (400km) & 90_000_005 (Perimeter)
    let b_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "BroadcastToBubble",
        &[PyRep::String("Target".into()), PyRep::Integer(0), PyRep::Integer(800_002)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(b_kv) = b_res else { panic!() };
    let PyRep::List(b_recips) = b_kv.get("recipientIDs").unwrap() else { panic!() };
    let b_ids: HashSet<i64> = b_recips.iter().filter_map(|r| r.as_i64()).collect();
    assert!(b_ids.contains(&90_000_002));
    assert!(!b_ids.contains(&90_000_003));
    assert!(!b_ids.contains(&90_000_005));

    // System broadcast includes 90_000_003 (400km same system) and excludes 90_000_005 (Perimeter)
    let s_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetMgr",
        "BroadcastToSystem",
        &[PyRep::String("AlignTo".into()), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(s_kv) = s_res else { panic!() };
    let PyRep::List(s_recips) = s_kv.get("recipientIDs").unwrap() else { panic!() };
    let s_ids: HashSet<i64> = s_recips.iter().filter_map(|r| r.as_i64()).collect();
    assert!(s_ids.contains(&90_000_003));
    assert!(!s_ids.contains(&90_000_005));

    // Broadcast history & unmuting
    let hist = handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "GetBroadcastHistory", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::List(h_list) = hist else { panic!() };
    assert_eq!(h_list.len(), 4);
}

#[tokio::test]
async fn fleet_ops_warp_to_member_motd_options_and_composition() {
    let ctx = test_ctx_with_clock(1_800_000_200);
    let (_fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // WarpToMember error cases: close target (15km < 150km), different system (90_000_005), non-member (99_999)
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetObjectHandler",
            "WarpToMember",
            &[PyRep::Integer(90_000_002)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WarpDistanceTooClose"
    );
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetMgr",
            "WarpToMember",
            &[PyRep::Integer(90_000_005)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DifferentSolarSystem"
    );
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetMgr",
            "WarpToMember",
            &[PyRep::Integer(99_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MemberNotFound"
    );

    // Valid WarpToMember -> 90_000_003 at 400_000m with minRange 10_000m -> lands at x = 390_000
    let w_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "WarpToMember",
        &[PyRep::Integer(90_000_003), PyRep::Real(10_000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(w_kv) = w_res else { panic!() };
    assert_eq!(w_kv.get("status").unwrap().as_str().unwrap(), "Warping");
    assert!((w_kv.get("destinationX").unwrap().as_f64().unwrap() - 390_000.0).abs() < 1.0);

    // MOTD set / permission denied / readback
    let mut mem_sess = SessionState::new(9);
    mem_sess.user_id = Some(90_000_002);
    mem_sess.character_id = Some(90_000_002);
    mem_sess.fleet_id = session.fleet_id;
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut mem_sess,
            "fleetObjectHandler",
            "SetMotd",
            &[PyRep::String("Denied".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );
    let m_set = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SetMotdEx",
        &[PyRep::String("Form at Jita 4-4".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(m_kv) = m_set else { panic!() };
    assert_eq!(m_kv.get("motd").unwrap().as_str().unwrap(), "Form at Jita 4-4");
    assert_eq!(
        handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "GetMotd", &[], &kw)
            .await
            .unwrap()
            .unwrap()
            .as_str()
            .unwrap(),
        "Form at Jita 4-4"
    );

    // SetOptions KeyVal + positional, SetAutoJoinSquadID, SetFleetMaxSize / GetFleetMaxSize
    let opts = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SetOptions",
        &[PyRep::keyval([
            ("isFreeMove", PyRep::Bool(true)),
            ("isRegistered", PyRep::Bool(true)),
            ("isVoiceEnabled", PyRep::Bool(false)),
            ("isLootLogging", PyRep::Bool(true)),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(o_kv) = opts else { panic!() };
    assert_eq!(o_kv.get("isFreeMove").unwrap(), &PyRep::Bool(true));
    assert_eq!(o_kv.get("isRegistered").unwrap(), &PyRep::Bool(true));

    let aj = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SetAutoJoinSquadID",
        &[PyRep::Integer(720_002)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(aj_kv) = aj else { panic!() };
    assert_eq!(aj_kv.get("autoJoinSquadID").unwrap(), &PyRep::Integer(720_002));

    let ms = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SetFleetMaxSize",
        &[PyRep::Integer(50)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(ms_kv) = ms else { panic!() };
    assert_eq!(ms_kv.get("maxSize").unwrap(), &PyRep::Integer(50));
    assert_eq!(
        handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "GetFleetMaxSize", &[], &kw)
            .await
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap(),
        50
    );

    // Member preferences: SetTakesFleetWarp, SetAcceptsRegroupValue, SetAcceptsConduitJumpsValue
    handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetObjectHandler",
        "SetTakesFleetWarp",
        &[PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetMgr",
        "SetAcceptsRegroupValue",
        &[PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let p_res = handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetObjectHandler",
        "SetAcceptsConduitJumpsValue",
        &[PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(p_kv) = p_res else { panic!() };
    assert_eq!(p_kv.get("takesFleetWarp").unwrap(), &PyRep::Bool(false));
    assert_eq!(p_kv.get("acceptsRegroup").unwrap(), &PyRep::Bool(false));
    assert_eq!(p_kv.get("acceptsConduitJumps").unwrap(), &PyRep::Bool(false));

    // GetFleetComposition (fleet members allowed, non-fleet denied)
    let mut outsider = SessionState::default();
    outsider.user_id = Some(99_999_888);
    outsider.character_id = Some(99_999_888);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut outsider, "fleetObjectHandler", "GetFleetComposition", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );
    let comp_mem = handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "GetFleetComposition", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Rowset { rows: mem_rows, .. } = comp_mem else { panic!() };
    assert_eq!(mem_rows.len(), 5);
    let comp = handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "GetFleetComposition", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Rowset { rows: fc_rows, .. } = comp else { panic!() };
    assert_eq!(fc_rows.len(), 5);

    // Init state / wings delegation + unmatched method
    assert!(handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "GetInitState", &[], &kw)
        .await
        .unwrap()
        .is_ok());
    assert!(handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "GetWings", &[], &kw)
        .await
        .unwrap()
        .is_ok());
    assert!(handle_fleet_ops(&ctx, &mut session, "unmatched", "UnknownMethod", &[], &kw)
        .await
        .is_none());
}
