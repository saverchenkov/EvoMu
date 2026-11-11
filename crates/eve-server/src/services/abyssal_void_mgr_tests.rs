use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};

#[tokio::test]
async fn test_abyssal_void_mgr_dispatch_and_filament_activation() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    std::sync::Arc::make_mut(&mut ctx.sde)
        .solar_systems
        .get_mut(&sys_id)
        .unwrap()
        .security_status = 0.7;
    let kw = HashMap::new();

    // Ensure ship is a Caracal (621) in space far from celestials, and seed a Calm Electrical Filament (47762)
    let fil_id = 910_001;
    let far_pos = [500_000_000.0, 500_000_000.0, 500_000_000.0];
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 621;
        }
        snap.item_positions.insert(ship_id, far_pos);
        snap.items.insert(
            fil_id,
            InventoryItemRecord {
                item_id: fil_id,
                type_id: 47_762,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 621,
            position: DVec3::from_array(far_pos),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Missing argument checks
    assert!(handle(
        &ctx,
        &mut session,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[],
        &kw
    )
    .await
    .unwrap()
    .is_err());
    assert!(handle(
        &ctx,
        &mut session,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[],
        &kw
    )
    .await
    .unwrap()
    .is_err());

    // Direct inventory filament activation via AbyssalEntranceGateActivation
    let enter = handle(
        &ctx,
        &mut session,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(fil_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        enter.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(ABYSSAL_SPACE_SYSTEM_ID as i64)
    );
    assert_eq!(session.solar_system_id, Some(ABYSSAL_SPACE_SYSTEM_ID));

    // ClientIsReady inside Abyssal space
    let ready = handle(&ctx, &mut session, "abyssalMgr", "ClientIsReady", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready.dict_get("ready").and_then(|v| v.as_bool()), Some(true));

    // ClientIsReady for voidSpaceMgr
    session.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    let vready = handle(&ctx, &mut session, "voidSpaceMgr", "ClientIsReady", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(vready.dict_get("ready").and_then(|v| v.as_bool()), Some(true));
    let real_sde = &crate::services::account_services::account_services_tests::test_ctx().sde;
    assert_eq!(resolve_void_space_nebula_graphic_id(real_sde, 62_448), 25_126);
    assert_eq!(resolve_void_space_nebula_graphic_id(real_sde, 79_997), 22_041);
    assert_eq!(resolve_void_space_nebula_graphic_id(real_sde, 79_998), 22_042);
    assert_eq!(resolve_void_space_nebula_graphic_id(&eve_sde::SdeRegistry::default(), 62_448), 22_041);
    assert_eq!(VOID_SPACE_ENCOUNTER_LIFE_TIME_SECS, 1_800);
    assert_eq!(TIMER_TYPE_ABYSSAL_CONTENT_EXPIRATION, 11);
    assert_eq!(TIMER_TYPE_ABYSSAL_PVP_EXPIRATION, 14);
    assert_eq!(TIMER_TYPE_VOID_SPACE_CONTENT_EXPIRATION, 16);
}

#[tokio::test]
async fn test_abyssal_void_mgr_errors_and_trace_entry_branches() {
    use crate::services::pve_special_sites::{with_abyssal_state, AbyssalTraceRecord};
    use eve_db::{FleetMemberRecord, FleetRecord, FleetRole};

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    std::sync::Arc::make_mut(&mut ctx.sde)
        .solar_systems
        .get_mut(&sys_id)
        .unwrap()
        .security_status = 0.7;
    let kw = HashMap::new();
    let other_char = 90_000_777_i64;
    let other_ship = 100_000_777_i64;
    let far_pos = [500_000_000.0, 500_000_000.0, 500_000_000.0];

    // Error helpers
    assert!(err_no_solar_system().code.starts_with("NoSolar"));
    assert!(err_no_active_ship().code.starts_with("NoActive"));
    assert!(err_ship_docked("x").code.starts_with("ShipDocked"));

    // Unknown service/method -> None
    let unknown_method = "UnknownMethod";
    assert!(handle(&ctx, &mut session, "abyssalMgr", unknown_method, &[], &kw).await.is_none());

    for (svc, m) in [
        ("abyssalMgr", "AbyssalGateActivation"),
        ("abyssalMgr", "AbyssalEndGateActivation"),
        ("abyssalMgr", "ClientIsReady"),
        ("voidSpaceMgr", "VoidSpaceEndGateActivation"),
    ] {
        let mut s_nosys = session.clone();
        s_nosys.solar_system_id = None;
        assert_eq!(
            handle(&ctx, &mut s_nosys, svc, m, &[], &kw).await.unwrap().unwrap_err().code,
            "NoSolarSystem"
        );
        let mut s_wrong = session.clone();
        s_wrong.solar_system_id = Some(sys_id);
        assert!(handle(&ctx, &mut s_wrong, svc, m, &[], &kw).await.unwrap().is_err());
    }

    assert_eq!(
        handle(&ctx, &mut session, "voidSpaceMgr", "VoidSpacePlayerJump", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MissingRequiredArgument"
    );

    // Unauthenticated session checks
    let mut unauth = SessionState::default();
    for (svc, m, args) in [
        ("abyssalMgr", "AbyssalEntranceGateActivation", vec![PyRep::Integer(1)]),
        ("abyssalMgr", "AbyssalGateActivation", vec![]),
        ("abyssalMgr", "AbyssalEndGateActivation", vec![]),
        ("abyssalMgr", "ClientIsReady", vec![]),
        ("voidSpaceMgr", "ClientIsReady", vec![]),
        ("voidSpaceMgr", "VoidSpacePlayerJump", vec![PyRep::Integer(1)]),
        ("voidSpaceMgr", "VoidSpaceEndGateActivation", vec![]),
    ] {
        assert!(handle(&ctx, &mut unauth, svc, m, &args, &kw).await.unwrap().is_err());
    }

    // Seed Cruiser (621) and Frigate (587) + Fleet + Expired/Wrong-System/Valid AbyssalTraceRecords
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 621;
        }
        snap.item_positions.insert(ship_id, far_pos);
        let base = snap.characters.get(&char_id).cloned().unwrap();
        let mut ch2 = base.clone();
        ch2.character_id = other_char;
        ch2.solar_system_id = sys_id;
        ch2.active_ship_id = other_ship;
        snap.characters.insert(other_char, ch2);
        snap.items.insert(
            other_ship,
            InventoryItemRecord {
                item_id: other_ship,
                type_id: 587, // Frigate
                owner_id: other_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(other_ship, far_pos);
        let mut fl = FleetRecord {
            fleet_id: 7001,
            boss_id: char_id,
            ..Default::default()
        };
        fl.members.extend([char_id, other_char]);
        for m in [char_id, other_char] {
            fl.member_details.insert(
                m,
                FleetMemberRecord {
                    character_id: m,
                    role: FleetRole::SquadMember,
                    wing_id: None,
                    squad_id: None,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: "M".into(),
                    join_time_unix: 1000,
                },
            );
        }
        snap.fleets.insert(7001, fl);
    });
    assert!(is_in_trace_fleet(&ctx, char_id, other_char));

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 621,
                position: DVec3::from_array(far_pos),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::new(),
        )
        .await;
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: other_ship,
                type_id: 587,
                position: DVec3::from_array(far_pos),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::new(),
        )
        .await;

    let now = ctx.now_unix();
    let trace_wrong_sys = 1_897_000_501_i64;
    let trace_expired = 1_897_000_502_i64;
    let trace_solo_other = 1_897_000_503_i64;
    let trace_far = 1_897_000_504_i64;
    let trace_frig_fleet = 1_897_000_505_i64;
    with_abyssal_state(&ctx, |reg| {
        reg.traces.insert(
            trace_wrong_sys,
            AbyssalTraceRecord {
                trace_ball_id: trace_wrong_sys,
                char_id,
                filament_type_id: 47_762,
                origin_solar_system_id: 30_000_144,
                position: far_pos,
                created_unix: now,
                expires_at_sec: now + 1000,
                max_ships: 1,
                entered_char_ids: vec![],
                pocket_id: None,
                fleet_id: None,
            },
        );
        reg.traces.insert(
            trace_expired,
            AbyssalTraceRecord {
                trace_ball_id: trace_expired,
                char_id,
                filament_type_id: 47_762,
                origin_solar_system_id: sys_id,
                position: far_pos,
                created_unix: now.saturating_sub(2000),
                expires_at_sec: now.saturating_sub(10),
                max_ships: 1,
                entered_char_ids: vec![],
                pocket_id: None,
                fleet_id: None,
            },
        );
        reg.traces.insert(
            trace_solo_other,
            AbyssalTraceRecord {
                trace_ball_id: trace_solo_other,
                char_id: other_char,
                filament_type_id: 47_762,
                origin_solar_system_id: sys_id,
                position: far_pos,
                created_unix: now,
                expires_at_sec: now + 1000,
                max_ships: 1,
                entered_char_ids: vec![],
                pocket_id: None,
                fleet_id: None,
            },
        );
        reg.traces.insert(
            trace_far,
            AbyssalTraceRecord {
                trace_ball_id: trace_far,
                char_id,
                filament_type_id: 47_762,
                origin_solar_system_id: sys_id,
                position: [far_pos[0] + 10_000.0, far_pos[1], far_pos[2]],
                created_unix: now,
                expires_at_sec: now + 1000,
                max_ships: 1,
                entered_char_ids: vec![],
                pocket_id: None,
                fleet_id: None,
            },
        );
        reg.traces.insert(
            trace_frig_fleet,
            AbyssalTraceRecord {
                trace_ball_id: trace_frig_fleet,
                char_id: other_char,
                filament_type_id: 47_762,
                origin_solar_system_id: sys_id,
                position: far_pos,
                created_unix: now,
                expires_at_sec: now + 1000,
                max_ships: 3,
                entered_char_ids: vec![],
                pocket_id: None,
                fleet_id: Some(7001),
            },
        );
    });

    // Docked / NoSolarSystem / NoActiveShip on AbyssalEntranceGateActivation -> activate_trace
    let mut s_docked = session.clone();
    s_docked.station_id = Some(60_003_760);
    assert_eq!(
        handle(&ctx, &mut s_docked, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_solo_other)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    let mut s_no_ship = session.clone();
    s_no_ship.ship_id = None;
    assert_eq!(
        handle(&ctx, &mut s_no_ship, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_solo_other)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // WrongSolarSystem, TraceExpired, OutOfRange, AccessDenied (solo), DisallowedShipType (cruiser on frig trace)
    assert_eq!(
        handle(&ctx, &mut session, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_wrong_sys)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WrongSolarSystem"
    );
    assert_eq!(
        handle(&ctx, &mut session, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_expired)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TraceExpired"
    );
    assert_eq!(
        handle(&ctx, &mut session, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_far)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OutOfRange"
    );
    assert_eq!(
        handle(&ctx, &mut session, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_solo_other)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        handle(&ctx, &mut session, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_frig_fleet)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DisallowedShipType"
    );

    // Cloaked / tethered guard
    let _ = sys_h
        .set_combat_state(
            ship_id,
            eve_destiny::CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        handle(&ctx, &mut session, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_frig_fleet)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipIsCloaked"
    );
    let _ = sys_h
        .set_combat_state(
            ship_id,
            eve_destiny::CombatState {
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await;

    // Wrong fleet on trace_frig_fleet using other_char's ship
    let mut sess2 = session.clone();
    sess2.character_id = Some(other_char);
    sess2.ship_id = Some(other_ship);
    sess2.fleet_id = Some(7001);
    let enter_trace = handle(
        &ctx,
        &mut sess2,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(trace_frig_fleet)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        enter_trace.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(ABYSSAL_SPACE_SYSTEM_ID as i64)
    );

    // AbyssalEntranceDeployment with valid filament (clear nearby test traces & provide 3 filaments)
    with_abyssal_state(&ctx, |reg| {
        reg.traces.clear();
    });
    let fil_id = 910_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fil_id,
            InventoryItemRecord {
                item_id: fil_id,
                type_id: 47_762,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 3,
                ..Default::default()
            },
        );
    });
    let dep = handle(
        &ctx,
        &mut session,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[PyRep::Integer(fil_id)],
        &kw,
    )
    .await
    .unwrap();
    assert!(dep.is_ok());

    // VoidSpaceMgr ClientIsReady when void_space_session exists -> pushes OnVoidSpaceRelog
    with_abyssal_state(&ctx, |reg| {
        reg.void_space_sessions.insert(
            char_id,
            crate::services::pve_special_sites::VoidSpaceSessionRecord {
                char_id,
                ship_id,
                origin_solar_system_id: sys_id,
                origin_position: far_pos,
                target_id: 12345,
                entered_unix: now,
                expires_at_sec: now + 600,
                ..Default::default()
            },
        );
    });
    session.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    assert!(handle(&ctx, &mut session, "voidSpaceMgr", "ClientIsReady", &[], &kw).await.unwrap().is_ok());
}

#[tokio::test]
async fn abyssal_trace_item_position_fallback_and_fleet_access_denied() {
    let (ctx, mut sess_other, other_id, ship_other, sys_id) = make_fighter_ctx_and_session();
    let owner_id = other_id + 1;
    let fil_type = 47_762_u32;
    let ship_owner = ship_other + 100;
    let frigate_tid = 587_u32;
    let pos = [500_000.0, 0.0, 0.0];
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let template_ch = snap.characters[&other_id].clone();
        for (cid, sid) in [(owner_id, ship_owner), (other_id, ship_other)] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| {
                let mut c = template_ch.clone();
                c.character_id = cid;
                c
            });
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: frigate_tid,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(sid, pos);
        }
        snap.fleets.insert(
            500_001,
            eve_db::FleetRecord {
                fleet_id: 500_001,
                boss_id: other_id,
                members: vec![other_id],
                wings: vec![],
                squads: vec![],
                free_move: false,
                is_registered: false,
                wing_details: Default::default(),
                squad_details: Default::default(),
                member_details: Default::default(),
                invites: Default::default(),
                motd: String::new(),
            },
        );
    });

    // Multi-ship frigate trace (max_ships=3, fleet_id=None) while ship_other has NO ball in space
    let trace_id = 1_897_990_001_i64;
    with_abyssal_state(&ctx, |reg| {
        reg.traces.insert(
            trace_id,
            crate::services::pve_special_sites::AbyssalTraceRecord {
                trace_ball_id: trace_id,
                char_id: owner_id,
                filament_type_id: fil_type,
                origin_solar_system_id: sys_id,
                position: pos,
                created_unix: now,
                expires_at_sec: now + 600,
                max_ships: 3,
                entered_char_ids: vec![owner_id],
                pocket_id: None,
                fleet_id: None,
            },
        );
    });

    sess_other.station_id = None;
    sess_other.structure_id = None;
    sess_other.ship_id = Some(ship_other);
    let kw = HashMap::new();
    let err = handle(&ctx, &mut sess_other, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    // Also test ShipIsTethered on Abyssal trace activation when ship ball combat state is tethered
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_other,
                type_id: frigate_tid,
                position: DVec3::from_array(pos),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState { is_tethered: true, ..Default::default() },
        )
        .await;
    let err2 = handle(&ctx, &mut sess_other, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err2.code, "ShipIsTethered");
}
