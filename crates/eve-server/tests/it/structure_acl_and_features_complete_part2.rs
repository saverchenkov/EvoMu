//! Part 2 of Upwell structure anchoring restrictions, fitting state locks, EWAR immunity,
//! and mobile cynosural inhibitors (`feat:structures.*`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops as ops;
use eve_server::services::structure_acl_defense_rules as acl_rules;
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn kv_entry<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

async fn login(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

fn create_test_structure(
    ctx: &ServerContext,
    structure_id: i64,
    type_id: u32,
    corp_id: i64,
    sys_id: u32,
    pos: [f64; 3],
    state: &str,
) {
    let profile = life::structure_profile(ctx, type_id).expect("valid structure type");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                position: pos,
                state: state.to_string(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: profile.shield_hp,
                armor_hp: profile.armor_hp,
                hull_hp: profile.hull_hp,
                name: format!("Test Structure {structure_id}"),
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            structure_id,
            InventoryItemRecord {
                item_id: structure_id,
                type_id,
                owner_id: corp_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
}

#[tokio::test]
async fn test_structure_anchor_restrictions_and_fitting_state_locks() {
    // parity: feat:structures.anchor-restrictions, feat:structures.fitting-state-locks
    eve_testkit::attest(&[
        "feat:structures.anchor-restrictions",
        "feat:structures.fitting-state-locks",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let astrahus_type = 35832;
    let keepstar_type = 35834;
    let sys_id = 30_000_144;
    let profile = life::structure_profile(&ctx, astrahus_type).expect("astrahus profile");
    let keepstar_prof = life::structure_profile(&ctx, keepstar_type).expect("keepstar profile");

    let station = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).expect("station");
    let too_close_pos = station.position + DVec3::new(500_000.0, 0.0, 0.0);
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, sys_id, too_close_pos);
        assert_eq!(res.unwrap_err().code, "TooCloseToStructureOrStation");
    });

    let valid_pos = station.position + DVec3::new(50_000_000.0, 0.0, 0.0);
    ctx.db.with_snapshot(|snap| {
        assert!(acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, sys_id, valid_pos).is_ok());
    });

    let ks_struct_id = 100_003;
    create_test_structure(&ctx, ks_struct_id, keepstar_type, 1001, sys_id, [valid_pos.x, valid_pos.y, valid_pos.z], life::STATE_SHIELD_VULNERABLE);
    let far_ks_pos = valid_pos + DVec3::new(10_000_000.0, 0.0, 0.0);
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &keepstar_prof, sys_id, far_ks_pos);
        assert_eq!(res.unwrap_err().code, "KeepstarPerSystemLimit");
    });

    ctx.db.with_snapshot(|snap| {
        let jita_res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, 30_000_142, valid_pos);
        assert_eq!(jita_res.unwrap_err().code, "AnchoringDisallowedInTradeHub");
    });
    let pochven_sys = ctx.sde.solar_systems.values().find(|s| s.region_id == life::POCHVEN_REGION_ID).map(|s| s.solar_system_id).unwrap_or(30_000_157);
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, pochven_sys, valid_pos);
        assert_eq!(res.unwrap_err().code, "AnchoringDisallowedInPochven");
    });

    let refit_struct_id = 100_004;
    create_test_structure(&ctx, refit_struct_id, astrahus_type, 1001, sys_id, [valid_pos.x, valid_pos.y, valid_pos.z], life::STATE_SHIELD_VULNERABLE);
    let ext = ops::StructureExtendedState::default();
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        assert!(acl_rules::validate_structure_fitting_state(&ctx, snap, &ext, refit_struct_id, now).is_ok());
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&refit_struct_id) {
            st.shield_hp *= 0.5;
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(acl_rules::validate_structure_fitting_state(&ctx, snap, &ext, refit_struct_id, now).unwrap_err().code, "StructureDamagedCannotRefit");
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&refit_struct_id) {
            st.shield_hp = profile.shield_hp;
            st.state = life::STATE_ANCHORING.to_string();
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(acl_rules::validate_structure_fitting_state(&ctx, snap, &ext, refit_struct_id, now).unwrap_err().code, "StructureFittingLockedInState");
    });
}

#[tokio::test]
async fn test_structure_ewar_immunity_and_mobile_cyno_inhibitor() {
    // parity: feat:structures.ewar-immunity, feat:structures.mobile-cyno-inhibitor
    eve_testkit::attest(&[
        "feat:structures.ewar-immunity",
        "feat:structures.mobile-cyno-inhibitor",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let struct_id = 100_005;
    let ship_id = 100_006;
    let sys_id = 30_000_142;

    create_test_structure(&ctx, struct_id, 35832, 1001, sys_id, [0.0, 0.0, 0.0], life::STATE_SHIELD_VULNERABLE);

    ctx.db.with_snapshot(|snap| {
        assert!(acl_rules::is_structure_immune_to_ewar(snap, struct_id));
        assert!(!acl_rules::is_structure_immune_to_ewar(snap, ship_id));
    });

    let char_id = 90_000_001;
    let mut sess = login(&ctx, 401, "demo", "demo-secret", char_id).await;
    let web_mod_id = 100_008;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            web_mod_id,
            InventoryItemRecord {
                item_id: web_mod_id,
                type_id: 526,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 3.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    let unlocked_err = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
            PyRep::Integer(struct_id),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(unlocked_err.code, "TargetNotLocked");

    let _ = sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: struct_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    let act_rep = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
            PyRep::Integer(struct_id),
        ],
    )
    .await
    .expect("dogmaIM.Activate webifier against Upwell structure");

    assert_eq!(kv_entry(&act_rep, "immune"), Some(&PyRep::Bool(true)));
    assert_eq!(kv_entry(&act_rep, "damageDealt"), Some(&PyRep::Real(0.0)));

    let inhibitor_id = 100_007;
    let inhibitor_pos = [10_000.0, 20_000.0, 30_000.0];
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            inhibitor_id,
            InventoryItemRecord {
                item_id: inhibitor_id,
                type_id: acl_rules::TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id: 1001,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(inhibitor_id, inhibitor_pos);
    });

    let test_pos_close = DVec3::new(10_000.0 + 50_000.0, 20_000.0, 30_000.0);
    let test_pos_far = DVec3::new(10_000.0 + 150_000.0, 20_000.0, 30_000.0);
    ctx.db.with_snapshot(|snap| {
        assert!(acl_rules::is_cyno_inhibited(&ctx, snap, sys_id, Some(test_pos_close)));
        assert!(!acl_rules::is_cyno_inhibited(&ctx, snap, sys_id, Some(test_pos_far)));
        assert!(!acl_rules::is_cyno_inhibited(&ctx, snap, 30_000_143, Some(test_pos_close)));
    });
}

#[tokio::test]
async fn test_station_standing_check_diplomacy_connections_and_corp_office_gates() {
    // parity: feat:structures.station-standing-check, rpc:corpStationMgr.DoStandingCheckForStationService
    eve_testkit::attest(&[
        "feat:structures.station-standing-check",
        "rpc:corpStationMgr.DoStandingCheckForStationService",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let pilot1 = 90_000_001i64;
    let pilot2 = 90_000_002i64;
    let mut s1 = login(&ctx, 901, "demo", "demo-secret", pilot1).await;
    let mut s2 = login(&ctx, 902, "other", "other-secret", pilot2).await;

    // Undocked caller with no explicit stationID must fail with NotDocked (no hardcoded Jita fallback)
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[]).await.expect("Undock pilot2");
    ctx.sync_session(&mut s2);
    let err_undocked = rpc(&ctx, &mut s2, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(13)])
        .await
        .expect_err("Undocked pilot without stationID must get NotDocked");
    assert_eq!(err_undocked.code, "NotDocked");

    // FSD stationstandingsrestrictions.fsdbinary gates on Triglavian Pochven station 60000355 (faction 500026):
    // - Docking (22) & Repair (13) require >= -0.05; Jump Clone Facility (24) & Office Rental (23, corp standing) require >= 3.0.
    // - Diplomacy (3357) lifts negative standing (+4%/lvl toward 10); Connections (3359) lifts positive standing (+4%/lvl toward 10).
    const STATION_POCHVEN: i64 = 60_000_355;
    const FACTION_TRIGLAVIAN: i64 = 500_026;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot1).unwrap().standings.insert(FACTION_TRIGLAVIAN, -0.20);
    });
    let err_trig_dock = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(22), PyRep::Integer(STATION_POCHVEN)],
    )
    .await
    .expect_err("Triglavian docking requires >= -0.05 standing");
    assert_eq!(err_trig_dock.code, "StandingTooLowForStationService");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(pilot1).or_default().insert(
            3357,
            eve_db::TrainedSkillRecord { type_id: 3357, level: 1, skill_points: 1_000.0 },
        );
    });
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(22), PyRep::Integer(STATION_POCHVEN)]).await.unwrap(),
        PyRep::Bool(true),
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(13), PyRep::Integer(STATION_POCHVEN)]).await.unwrap(),
        PyRep::Bool(true),
    );
    let err_trig_clone = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(24), PyRep::Integer(STATION_POCHVEN)],
    )
    .await
    .expect_err("Triglavian jump clone facility (24) requires >= 3.0 standing");
    assert_eq!(err_trig_clone.code, "StandingTooLowForStationService");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot1).unwrap().standings.insert(FACTION_TRIGLAVIAN, 2.5);
        snap.skills.entry(pilot1).or_default().insert(
            3359,
            eve_db::TrainedSkillRecord { type_id: 3359, level: 3, skill_points: 24_000.0 },
        );
    });
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(24), PyRep::Integer(STATION_POCHVEN)]).await.unwrap(),
        PyRep::Bool(true),
    );

    // Office rental (23) checks player-corporation standing (CEO/corp standing), not solo member personal standing:
    let player_corp = 98_000_777i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            player_corp,
            eve_db::CorporationRecord {
                corporation_id: player_corp,
                name: "Test Pochven Corp".into(),
                ticker: "TPCHV".into(),
                ceo_id: pilot2,
                members: [pilot1, pilot2].into_iter().collect(),
                ..Default::default()
            },
        );
        snap.characters.get_mut(&pilot1).unwrap().corporation_id = player_corp;
        snap.characters.get_mut(&pilot2).unwrap().corporation_id = player_corp;
        snap.characters.get_mut(&pilot2).unwrap().standings.insert(FACTION_TRIGLAVIAN, 1.0);
    });
    let err_trig_office = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(23), PyRep::Integer(STATION_POCHVEN)],
    )
    .await
    .expect_err("Triglavian office rental must reject corp standing < 3.0 even when caller personal standing >= 3.0");
    assert_eq!(err_trig_office.code, "StandingTooLowForStationService");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot2).unwrap().standings.insert(FACTION_TRIGLAVIAN, 3.2);
    });
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(23), PyRep::Integer(STATION_POCHVEN)]).await.unwrap(),
        PyRep::Bool(true),
    );
}
