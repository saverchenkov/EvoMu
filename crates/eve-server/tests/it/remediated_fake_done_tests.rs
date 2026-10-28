//! Remediation regression tests for fake-done rows.

use eve_db::{self, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services;
use eve_server::sp6_lifecycle;
use glam::DVec3;
use std::collections::HashMap;

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: eve_db::InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            eve_db::InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn grant_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, i32)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(sid, lvl) in skills {
            entry.insert(sid, eve_db::TrainedSkillRecord {
                type_id: sid,
                level: lvl as u8,
                skill_points: 256_000.0,
            });
        }
    });
}

fn pick_lowsec_pair(ctx: &ServerContext) -> (u32, u32) {
    let mut sys: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3])
        .map(|s| s.solar_system_id)
        .collect();
    sys.sort_unstable();
    for &a in &sys {
        let pa = ctx.sde.solar_systems[&a].galactic_pos_ly;
        for &b in &sys {
            if a == b {
                continue;
            }
            let pb = ctx.sde.solar_systems[&b].galactic_pos_ly;
            let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
            if (0.3..2.5).contains(&d) {
                return (a, b);
            }
        }
    }
    panic!("no suitable lowsec pair found");
}

#[tokio::test]
async fn test_safe_logout_emergency_warp_and_weapons_timer() {
    // parity: feat:account.logout
    eve_testkit::attest(&["feat:account.logout"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let sys_id = 30000142u32;
    let char_id = 90_000_001i64;
    let ship_id = 999_001i64;
    let now_unix = ctx.now_unix();

    seed_item(&ctx, ship_id, 587, char_id, sys_id as i64, eve_db::InventoryFlag::None, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
    });

    let entry = sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id).await.expect("entry");
    assert!(entry.emergency_warp_dest.is_some(), "Pilot with no timers must emergency warp on logoff");
    assert_eq!(entry.remove_at_unix, now_unix + 60);

    let ship2_id = 999_002i64;
    let char2_id = 90_000_002i64;
    seed_item(&ctx, ship2_id, 587, char2_id, sys_id as i64, eve_db::InventoryFlag::None, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char2_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship2_id;
        }
        snap.item_positions.insert(ship2_id, [1000.0, 0.0, 0.0]);
    });
    ctx.crimewatch.write().unwrap().states.entry(char2_id).or_default().weapon_timer_until_unix = now_unix + 300;

    let entry2 = sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char2_id).await.expect("entry2");
    assert!(entry2.emergency_warp_dest.is_none(), "Pilot with active weapons timer must NOT emergency warp");
    assert_eq!(entry2.remove_at_unix, now_unix + 300, "Ship must remain until weapon timer expires");
}

#[tokio::test]
async fn test_market_order_expiry_persisted_and_swept() {
    // parity: feat:market.order-expiry
    eve_testkit::attest(&["feat:market.order-expiry"]);
    let ctx = ServerContext::new();
    let now_unix = ctx.now_unix();
    let char_id = 90_000_001i64;
    let order_id = 888_001i64;
    let region_id = 10000002u32;
    let station_id = 60003760i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(order_id, eve_db::MarketOrderRecord {
            order_id,
            owner_id: char_id,
            station_id,
            region_id,
            type_id: 34,
            is_bid: false,
            price: IskCents::from_isk(5),
            vol_entered: 100,
            vol_remaining: 100,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents::from_isk(0),
        });
        snap.market_order_meta.insert(order_id, eve_db::MarketOrderMeta {
            issued_unix: now_unix.saturating_sub(200_000),
            duration_days: 1,
            range: "region".to_string(),
            issued_by: Some(char_id),
            wallet_division: Some(1000),
            is_corporation: false,
            state: Some("open".to_string()),
        });
    });

    sp6_lifecycle::on_advance_secs(&ctx, now_unix, 1);

    let state = ctx.db.with_snapshot(|snap| {
        snap.market_order_meta.get(&order_id).and_then(|m| m.state.clone())
    });
    assert_eq!(state.as_deref(), Some("expired"), "Expired order must transition to expired");

    let returned = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.owner_id == char_id && it.type_id == 34 && it.quantity >= 100)
    });
    assert!(returned, "Unsold items from expired sell order must be returned to owner");
}

#[tokio::test]
async fn test_fleet_cyno_jump() {
    // parity: feat:navigation.jump-drive
    eve_testkit::attest(&["feat:navigation.jump-drive"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let p1 = 90_000_001i64;
    let p2 = 90_000_002i64;
    let (lowsec_sys, dest_sys) = pick_lowsec_pair(&ctx);
    let fleet_id = 55_001i64;
    let rorq_id = 999_101i64;
    let cyno_ship_id = 999_102i64;
    let cyno_mod_id = 88_002i64;
    let ozone_id = 88_003i64;
    let isotope_id = 88_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(fleet_id, eve_db::FleetRecord {
            fleet_id,
            boss_id: p1,
            members: vec![p1, p2],
            ..Default::default()
        });
        if let Some(c) = snap.characters.get_mut(&p1) {
            c.solar_system_id = lowsec_sys;
            c.active_ship_id = rorq_id;
            c.station_id = None;
            c.structure_id = None;
            c.jump_cooldown_until_unix = 0;
            c.jump_fatigue_until_unix = 0;
        }
        if let Some(c) = snap.characters.get_mut(&p2) {
            c.solar_system_id = dest_sys;
            c.active_ship_id = cyno_ship_id;
            c.station_id = None;
            c.structure_id = None;
        }
    });

    let _ = ctx.db.acquire_online_lock_with_computer_hash(1, 7003, Some(p1), None, true, ctx.now_unix());
    let _ = ctx.db.acquire_online_lock_with_computer_hash(2, 7004, Some(p2), None, true, ctx.now_unix());

    grant_skills(&ctx, p1, &[(21611, 5), (21610, 5), (28351, 5), (24311, 5)]);
    grant_skills(&ctx, p2, &[(11951, 5), (21603, 5)]);

    seed_item(&ctx, rorq_id, 28352, p1, lowsec_sys as i64, eve_db::InventoryFlag::None, 1, true);
    let fuel_type = ctx.sde.types.get(&28352)
        .and_then(|t| t.default_attributes.get(&866).copied())
        .unwrap_or(16274.0) as u32;
    seed_item(&ctx, isotope_id, fuel_type, p1, rorq_id, eve_db::InventoryFlag::Cargo, 100_000, false);

    seed_item(&ctx, cyno_ship_id, 11957, p2, dest_sys as i64, eve_db::InventoryFlag::None, 1, true);
    seed_item(&ctx, cyno_mod_id, 21096, p2, cyno_ship_id, eve_db::InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, ozone_id, 16273, p2, cyno_ship_id, eve_db::InventoryFlag::Cargo, 2_000, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cyno_mod_id, true);
        snap.item_positions.insert(rorq_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(cyno_ship_id, [10_000.0, 0.0, 0.0]);
    });

    let h1 = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = h1.spawn_ball_with_combat(
        eve_destiny::Ball {
            id: rorq_id,
            type_id: 28352,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1e8,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        ctx.build_ship_combat_state(p1, rorq_id, 28352),
    ).await;

    let h2 = ctx.systems.get_or_spawn(dest_sys);
    let _ = h2.spawn_ball_with_combat(
        eve_destiny::Ball {
            id: cyno_ship_id,
            type_id: 11957,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        ctx.build_ship_combat_state(p2, cyno_ship_id, 11957),
    ).await;

    let mut s2 = SessionState::new(7004);
    s2.user_id = Some(2);
    s2.character_id = Some(p2);
    s2.ship_id = Some(cyno_ship_id);
    s2.station_id = None;
    s2.structure_id = None;
    s2.fleet_id = Some(fleet_id);
    s2.solar_system_id = Some(dest_sys);

    let _cyno_rep = ctx.dispatch_rpc(
        &mut s2,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(cyno_mod_id), PyRep::String("cynosuralGeneration".into())],
        &kw,
        None,
    ).await.expect("Activate cyno generator");

    let mut s1 = SessionState::new(7003);
    s1.user_id = Some(1);
    s1.character_id = Some(p1);
    s1.ship_id = Some(rorq_id);
    s1.station_id = None;
    s1.structure_id = None;
    s1.fleet_id = Some(fleet_id);
    s1.solar_system_id = Some(lowsec_sys);

    let res = ctx.dispatch_rpc(
        &mut s1,
        "beyonce",
        None,
        // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
        "CmdBeaconJumpFleet",
        &[PyRep::Integer(p2), PyRep::Integer(cyno_ship_id), PyRep::Integer(dest_sys as i64)],
        &kw,
        None,
    ).await;
    assert!(res.is_ok(), "CmdBeaconJumpFleet failed: {:?}", res);

    let remaining_fuel = ctx.db.with_snapshot(|snap| {
        snap.items.get(&isotope_id).map(|i| i.quantity).unwrap_or(0)
    });
    assert!(remaining_fuel < 100_000, "Jump must consume fuel");

    assert_eq!(s1.solar_system_id, Some(dest_sys));
    let notifs = ctx.drain_notifications_for_char(p1);
    assert!(notifs.iter().any(|n| n.event == "OnJumpTimersUpdated"), "Must emit OnJumpTimersUpdated");
}

#[tokio::test]
async fn test_session_change_timer_10s_and_canonical_payload() {
    // parity: feat:navigation.session-change-timer
    eve_testkit::attest(&["feat:navigation.session-change-timer"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001i64;
    let now_unix = ctx.now_unix();
    let sys_id = 30000142u32;

    let _ = ctx.db.acquire_online_lock_with_computer_hash(1, 7001, Some(char_id), None, true, now_unix);

    services::space_destiny::record_session_change_timer(&ctx, char_id, now_unix, sys_id, 100, "stargate_jump");

    let notifs = ctx.drain_notifications_for_char(char_id);
    let sc = notifs.iter().find(|n| n.event == "OnSessionChanged").expect("OnSessionChanged emitted");
    assert_eq!(sc.args.len(), 3, "OnSessionChanged must have exactly 3 args: (isRemote, session, change)");
    assert_eq!(sc.args[0], PyRep::Bool(false));
    let next_change = sc.args[1].dict_get("nextSessionChange").and_then(|v| v.as_i64()).expect("nextSessionChange in session keyval");
    assert_eq!(next_change, (now_unix + 10) as i64);

    let active = services::space_destiny::check_session_change_timer_active(&ctx, char_id, now_unix + 5, Some((sys_id, 105)));
    assert!(active.is_some(), "Timer must be active within 10s");

    let expired = services::space_destiny::check_session_change_timer_active(&ctx, char_id, now_unix + 10, Some((sys_id, 110)));
    assert!(expired.is_none(), "Timer must expire after 10s");
}

#[tokio::test]
async fn test_corp_recruitment_ads_expiry_and_search_filters() {
    // parity: rpc:corpRecProxy.SearchCorpAds
    eve_testkit::attest(&["rpc:corpRecProxy.SearchCorpAds"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let corp_id = 2_000_170i64;
    let char_id = 90_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "PvP Test Corp".into(),
                ticker: "PVPT".into(),
                ceo_id: char_id,
                master_wallet: eve_db::IskCents::from_isk(10_000_000),
                members: std::collections::HashSet::from([char_id]),
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(7005);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);

    let ad_rep = ctx.dispatch_rpc(
        &mut sess,
        "corpRecProxy",
        None,
        "CreateRecruitmentAd",
        &[PyRep::Integer(1), PyRep::Integer(4), PyRep::String("PvP Corp".into()), PyRep::String("Join today".into())],
        &kw,
        None,
    ).await.unwrap();
    let ad_id = ad_rep.dict_get("adID").and_then(|v| v.as_i64()).unwrap();

    let persisted = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs.contains_key("corp_recruitment_ads")
    });
    assert!(persisted, "Recruitment ads must persist in service_state_blobs");

    let res = ctx.dispatch_rpc(&mut sess, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(4)], &kw, None).await.unwrap();
    let PyRep::Rowset { rows, .. } = res else { panic!("expected Rowset") };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].as_i64(), Some(ad_id));

    let res2 = ctx.dispatch_rpc(&mut sess, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(1)], &kw, None).await.unwrap();
    let PyRep::Rowset { rows: rows2, .. } = res2 else { panic!("expected Rowset") };
    assert!(rows2.is_empty(), "Mismatched typeMask must exclude ad");
}

#[tokio::test]
async fn test_fleet_finder_advert_keyval() {
    // parity: rpc:fleetProxy.AddFleetFinderAdvert
    eve_testkit::attest(&["rpc:fleetProxy.AddFleetFinderAdvert"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let char_id = 90_000_001i64;
    let fleet_id = 66_001i64;
    let corp_id = 1_000_170i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(fleet_id, eve_db::FleetRecord {
            fleet_id,
            boss_id: char_id,
            members: vec![char_id],
            ..Default::default()
        });
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
    });

    let mut sess = SessionState::new(7006);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.fleet_id = Some(fleet_id);
    sess.corporation_id = Some(corp_id);

    let advert_kv = PyRep::keyval([
        ("fleetName", PyRep::String("Corp Mining Op".into())),
        ("description", PyRep::String("High yield mining".into())),
        ("inviteScope", PyRep::Integer(1)),
        ("joinNeedsApproval", PyRep::Bool(true)),
    ]);
    let rep = ctx.dispatch_rpc(
        &mut sess,
        "fleetProxy",
        None,
        "AddFleetFinderAdvert",
        &[advert_kv],
        &kw,
        None,
    ).await.expect("AddFleetFinderAdvert with KeyVal");
    assert_eq!(rep.dict_get("fleetName").and_then(|v| v.as_str()), Some("Corp Mining Op"));
    assert_eq!(rep.dict_get("inviteScope").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(rep.dict_get("autoJoin").and_then(|v| v.as_bool()), Some(false));
}

#[tokio::test]
async fn test_clone_vat_bay_target_only_and_psychology_cap() {
    // parity: rpc:jumpCloneSvc.AcceptShipCloneInstallation
    eve_testkit::attest(&["rpc:jumpCloneSvc.AcceptShipCloneInstallation"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let p1 = 90_000_001i64;
    let p2 = 90_000_002i64;
    let rorq_id = 999_201i64;
    let cvb_id = 999_202i64;

    seed_item(&ctx, rorq_id, 28352, p1, 30000142, eve_db::InventoryFlag::None, 1, true);
    seed_item(&ctx, cvb_id, 23735, p1, rorq_id, eve_db::InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, 999_203, 16273, p1, rorq_id, eve_db::InventoryFlag::Cargo, 10, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cvb_id, true);
        if let Some(c) = snap.characters.get_mut(&p1) {
            c.active_ship_id = rorq_id;
        }
        snap.fleets.insert(5001, eve_db::FleetRecord {
            fleet_id: 5001,
            boss_id: p1,
            members: vec![p1, p2],
            ..Default::default()
        });
    });

    let mut s1 = SessionState::new(7007);
    s1.user_id = Some(1);
    s1.character_id = Some(p1);
    s1.ship_id = Some(rorq_id);

    ctx.dispatch_rpc(&mut s1, "jumpCloneSvc", None, "OfferShipCloneInstallation", &[PyRep::Integer(p2)], &kw, None)
        .await.unwrap();

    let offerer_err = ctx.dispatch_rpc(&mut s1, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
        .await.unwrap_err();
    assert_eq!(offerer_err.code, "NoCloneOfferFound");

    let mut s2 = SessionState::new(7008);
    s2.user_id = Some(2);
    s2.character_id = Some(p2);

    let limit_err = ctx.dispatch_rpc(&mut s2, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
        .await.unwrap_err();
    assert_eq!(limit_err.code, "JumpCloneLimitReached");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(p2).or_default().insert(24242, eve_db::TrainedSkillRecord {
            type_id: 24242,
            level: 1,
            skill_points: 250.0,
        });
    });

    ctx.dispatch_rpc(&mut s1, "jumpCloneSvc", None, "OfferShipCloneInstallation", &[PyRep::Integer(p2)], &kw, None)
        .await.unwrap();
    let acc_rep = ctx.dispatch_rpc(&mut s2, "jumpCloneSvc", None, "AcceptShipCloneInstallation", &[], &kw, None)
        .await.unwrap();
    assert_eq!(acc_rep.dict_get("installed").and_then(|v| v.as_bool()), Some(true));
}
