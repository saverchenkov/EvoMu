//! Deep branch-coverage and mutation-killing unit tests for Fleet hierarchy, Wing/Squad limits,
//! Watchlist damage streaming, Broadcast scopes/cooldowns, Mail/CSPA/Labels, Calendar, and Corp Contacts.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_with_clock,
};
use crate::services::fleet_social::fleet_social_notify::{
    handle_character_disconnect, remove_character_from_fleet, tick_fleet_lifecycle,
    FLEET_DISCONNECT_GRACE_SECS,
};
use crate::services::fleet_social::{
    create_ccp_calendar_event, handle, handle_fleet_watchlist_ops, stream_fleet_watchlist_damage,
};
use eve_db::{AllianceRecord, CorporationRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

async fn social_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, sess, svc, method, args, &HashMap::new())
        .await
        .expect("handled")
}

fn make_sess(char_id: i64, corp_id: i64, fleet_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(8800 + (char_id as u64 % 1000));
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.fleet_id = fleet_id;
    s.solar_system_id = Some(30_000_142);
    s.corp_role = role;
    s
}

#[tokio::test]
async fn fleet_wing_squad_limits_free_move_occupancy_and_disconnect_handover_kill_mutations() {
    let ctx = test_ctx_with_clock(1_700_000_000);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;
    let wing_id = 710_001_i64;
    let squad_id = 720_001_i64;
    let boss_id = 90_000_001_i64;
    let wc_id = 90_000_002_i64;
    let sc_id = 90_000_003_i64;
    let sm_id = 90_000_004_i64;

    let mut sm_sess = make_sess(sm_id, 98_000_001, Some(fleet_id), 0);

    // 1. Wing cap (max 5 wings: 2 seeded + 3 created = 5; 6th fails with MaxWingsReached)
    let mut extra_wings = Vec::new();
    for _ in 0..3 {
        let w = social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "CreateWing", &[])
            .await
            .unwrap();
        let wid = w
            .as_i64()
            .or_else(|| w.dict_get("wingID").and_then(|v| v.as_i64()))
            .unwrap();
        extra_wings.push(wid);
    }
    assert_eq!(
        social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "CreateWing", &[])
            .await
            .unwrap_err()
            .code,
        "MaxWingsReached"
    );

    // Non-empty wing cannot be deleted (WingNotEmpty); empty wing can be renamed & deleted
    assert_eq!(
        social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "DeleteWing", &[PyRep::Integer(wing_id)])
            .await
            .unwrap_err()
            .code,
        "WingNotEmpty"
    );
    let empty_wing = extra_wings[0];
    social_call(
        &ctx,
        &mut boss_sess,
        "fleetObjectHandler",
        "ChangeWingName",
        &[PyRep::Integer(empty_wing), PyRep::String("Alpha Wing".into())],
    )
    .await
    .unwrap();
    social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "DeleteWing", &[PyRep::Integer(empty_wing)])
        .await
        .unwrap();

    // 2. Squad cap per wing (max 5 squads in wing_id: 1 seeded + 4 created = 5; 6th fails with MaxSquadsReached)
    let mut extra_squads = Vec::new();
    for _ in 0..4 {
        let sq = social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "CreateSquad", &[PyRep::Integer(wing_id)])
            .await
            .unwrap();
        let sid = sq
            .as_i64()
            .or_else(|| sq.dict_get("squadID").and_then(|v| v.as_i64()))
            .unwrap();
        extra_squads.push(sid);
    }
    assert_eq!(
        social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "CreateSquad", &[PyRep::Integer(wing_id)])
            .await
            .unwrap_err()
            .code,
        "MaxSquadsReached"
    );

    // Non-empty squad cannot be deleted (SquadNotEmpty); empty squad can be renamed & deleted
    assert_eq!(
        social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "DeleteSquad", &[PyRep::Integer(squad_id)])
            .await
            .unwrap_err()
            .code,
        "SquadNotEmpty"
    );
    let open_squad = extra_squads[0];
    let del_squad = extra_squads[1];
    social_call(
        &ctx,
        &mut boss_sess,
        "fleetObjectHandler",
        "ChangeSquadName",
        &[PyRep::Integer(open_squad), PyRep::String("Vanguard Squad".into())],
    )
    .await
    .unwrap();
    social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "DeleteSquad", &[PyRep::Integer(del_squad)])
        .await
        .unwrap();

    // 3. Role occupancy conflicts (PositionOccupied) & free-move toggle
    for occupied_role in [1_i64, 2, 3] {
        assert_eq!(
            social_call(
                &ctx,
                &mut boss_sess,
                "fleetObjectHandler",
                "MoveMember",
                &[
                    PyRep::Integer(sm_id),
                    PyRep::Integer(wing_id),
                    PyRep::Integer(squad_id),
                    PyRep::Integer(occupied_role),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "PositionOccupied"
        );
    }

    assert_eq!(
        social_call(
            &ctx,
            &mut sm_sess,
            "fleetObjectHandler",
            "MoveMember",
            &[
                PyRep::Integer(sm_id),
                PyRep::Integer(wing_id),
                PyRep::Integer(open_squad),
                PyRep::Integer(4),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // Enable free_move via SetOptions and verify self-move + MassMoveMembers + SetFleetMaxSize
    social_call(
        &ctx,
        &mut boss_sess,
        "fleetObjectHandler",
        "SetOptions",
        &[PyRep::keyval([
            ("isFreeMove", PyRep::Bool(true)),
            ("isVoiceEnabled", PyRep::Bool(true)),
        ])],
    )
    .await
    .unwrap();
    social_call(
        &ctx,
        &mut sm_sess,
        "fleetObjectHandler",
        "MoveMember",
        &[
            PyRep::Integer(sm_id),
            PyRep::Integer(wing_id),
            PyRep::Integer(open_squad),
            PyRep::Integer(4),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.fleets[&fleet_id].member_details[&sm_id].squad_id),
        Some(open_squad)
    );

    social_call(
        &ctx,
        &mut boss_sess,
        "fleetObjectHandler",
        "MassMoveMembers",
        &[
            PyRep::List(vec![PyRep::Integer(sm_id)]),
            PyRep::Integer(wing_id),
            PyRep::Integer(squad_id),
            PyRep::Integer(4),
        ],
    )
    .await
    .unwrap();
    social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "SetFleetMaxSize", &[PyRep::Integer(120)])
        .await
        .unwrap();
    let max_sz = social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "GetFleetMaxSize", &[])
        .await
        .unwrap();
    assert_eq!(max_sz, PyRep::Integer(120));

    // 4. Disconnect grace timer & boss handover priority (boss disconnects -> after 120s wc_id becomes boss)
    handle_character_disconnect(&ctx, boss_id);
    ctx.advance_secs(FLEET_DISCONNECT_GRACE_SECS + 5);
    tick_fleet_lifecycle(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.fleets[&fleet_id].boss_id), wc_id);

    remove_character_from_fleet(&ctx, fleet_id, wc_id);
    assert_eq!(ctx.db.with_snapshot(|s| s.fleets[&fleet_id].boss_id), sc_id);
}

#[tokio::test]
async fn fleet_watchlist_damage_streaming_and_broadcast_cooldown_scopes_kill_mutations() {
    let ctx = test_ctx_with_clock(1_700_000_000);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;
    let wc_id = 90_000_002_i64;
    let sm_id = 90_000_004_i64;

    let mut sm_sess = make_sess(sm_id, 98_000_001, Some(fleet_id), 0);

    // Watchlist rejects non-fleet member (MemberNotFound) and enforces 15-member cap (WatchlistFull)
    assert_eq!(
        social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "AddToWatchlist", &[PyRep::Integer(99_999_999)])
            .await
            .unwrap_err()
            .code,
        "MemberNotFound"
    );
    let sixteen_ids: Vec<PyRep> = (0..16).map(|i| PyRep::Integer(90_000_100 + i)).collect();
    assert_eq!(
        social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "RegisterForDamageUpdates", &[PyRep::List(sixteen_ids)])
            .await
            .unwrap_err()
            .code,
        "WatchlistFull"
    );

    // RegisterForDamageUpdates + AddToWatchlist + GetWatchlist + stream_fleet_watchlist_damage + RemoveFromWatchlist
    social_call(
        &ctx,
        &mut boss_sess,
        "fleetObjectHandler",
        "RegisterForDamageUpdates",
        &[PyRep::List(vec![PyRep::Integer(wc_id)])],
    )
    .await
    .unwrap();
    social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "AddToWatchlist", &[PyRep::Integer(sm_id)])
        .await
        .unwrap();
    let wl = handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "GetWatchlist", &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wl.as_list().unwrap().len(), 2);

    stream_fleet_watchlist_damage(&ctx, sm_id, 0.75, 0.50, 0.90);

    social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "RemoveFromWatchlist", &[PyRep::Integer(sm_id)])
        .await
        .unwrap();
    let wl_after = handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "GetWatchlist", &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wl_after.as_list().unwrap().len(), 1);

    // LockFromBroadcast: in-space target in same solar system succeeds; docked target fails (TargetNotInSystem)
    let lock_ok = handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "LockFromBroadcast", &[PyRep::Integer(sm_id)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lock_ok.dict_get("locked"), Some(&PyRep::Bool(true)));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&sm_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    assert_eq!(
        handle_fleet_watchlist_ops(&ctx, &mut boss_sess, "LockFromBroadcast", &[PyRep::Integer(sm_id)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotInSystem"
    );

    // Broadcast spam cooldown within 2s on identical broadcast
    social_call(
        &ctx,
        &mut sm_sess,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("NeedArmor".into()), PyRep::Integer(1), PyRep::Integer(sm_id)],
    )
    .await
    .unwrap();
    assert_eq!(
        social_call(
            &ctx,
            &mut sm_sess,
            "fleetObjectHandler",
            "SendBroadcast",
            &[PyRep::String("NeedArmor".into()), PyRep::Integer(1), PyRep::Integer(sm_id)],
        )
        .await
        .unwrap_err()
        .code,
        "BroadcastSpamCooldown"
    );

    ctx.advance_secs(5);
    ctx.fleet_extras
        .lock()
        .unwrap()
        .broadcast_filters
        .entry(90_000_001)
        .or_default()
        .insert("NeedArmor".into());
    social_call(
        &ctx,
        &mut sm_sess,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("NeedArmor".into()), PyRep::Integer(2), PyRep::Integer(sm_id)],
    )
    .await
    .unwrap();
    let hist = social_call(&ctx, &mut boss_sess, "fleetObjectHandler", "GetBroadcastHistory", &[])
        .await
        .unwrap();
    assert_eq!(hist.as_list().unwrap().len(), 2);
}

#[tokio::test]
async fn mail_cspa_labels_calendar_and_corp_contacts_deep_branches_kill_mutations() {
    let ctx = test_ctx_with_clock(1_700_000_000);
    let sender_id = 90_000_001_i64;
    let recip_id = 90_000_002_i64;
    let corp_id = 98_400_001_i64;
    let ally_id = 99_400_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                alliance_id: Some(ally_id),
                name: "Social Test Corp".into(),
                ticker: "STCP".into(),
                ceo_id: sender_id,
                members: [sender_id].into_iter().collect(),
                member_roles: [(sender_id, u64::MAX)].into_iter().collect(),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Social Test Alliance".into(),
                short_name: "STAL".into(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id].into_iter().collect(),
            },
        );
        if let Some(c1) = snap.characters.get_mut(&sender_id) {
            c1.corporation_id = corp_id;
            c1.alliance_id = Some(ally_id);
            c1.balance = IskCents::from_isk(100_000);
        }
        if let Some(c2) = snap.characters.get_mut(&recip_id) {
            c2.corporation_id = 1_000_009;
            c2.alliance_id = None;
        }
        snap.character_cspa_charges
            .insert(recip_id, IskCents::from_isk(5_000));
    });

    let mut sender_sess = make_sess(sender_id, corp_id, None, u64::MAX);
    sender_sess.alliance_id = Some(ally_id);
    let mut recip_sess = make_sess(recip_id, 1_000_009, None, 0);

    // 1. Mail subject/body bounds, CSPA deduction, zlib GetBody, labels, and trash lifecycle
    assert_eq!(
        social_call(
            &ctx,
            &mut sender_sess,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(recip_id)]),
                PyRep::String(String::new()),
                PyRep::String("Body".into()),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidMailSubject"
    );

    let send_res = social_call(
        &ctx,
        &mut sender_sess,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(recip_id)]),
            PyRep::String("Fleet Ops Notice".into()),
            PyRep::String("Form up in Jita 4-4 at 20:00 EVE.".into()),
        ],
    )
    .await
    .unwrap();
    let msg_id = send_res.as_i64().unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&sender_id].balance),
        IskCents::from_isk(95_000)
    );

    let body_rep = social_call(
        &ctx,
        &mut recip_sess,
        "mailMgr",
        "GetBody",
        &[PyRep::Integer(msg_id), PyRep::Bool(false)],
    )
    .await
    .unwrap();
    assert!(matches!(body_rep, PyRep::Buffer(ref b) if !b.is_empty()));

    assert_eq!(
        social_call(&ctx, &mut recip_sess, "mailMgr", "DeleteLabel", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "CannotDeleteSystemLabel"
    );
    let lbl = social_call(
        &ctx,
        &mut recip_sess,
        "mailMgr",
        "CreateLabel",
        &[PyRep::String("Pings".into()), PyRep::Integer(3)],
    )
    .await
    .unwrap();
    let lbl_id = lbl.as_i64().unwrap();
    assert!(lbl_id >= 64);
    social_call(
        &ctx,
        &mut recip_sess,
        "mailMgr",
        "AssignLabels",
        &[PyRep::List(vec![PyRep::Integer(msg_id)]), PyRep::Integer(lbl_id)],
    )
    .await
    .unwrap();
    social_call(&ctx, &mut recip_sess, "mailMgr", "DeleteLabel", &[PyRep::Integer(lbl_id)])
        .await
        .unwrap();

    // 2. Calendar events (CCP, Corp, Alliance) and responses
    let future_ft = (ctx.now_unix() as i64 + 3_600 + 11_644_473_600) * 10_000_000;
    let ccp_ev = create_ccp_calendar_event(&ctx, 1, future_ft, 60, 1, "Live Event", "Details").unwrap();
    assert_eq!(ccp_ev.dict_get("scope").and_then(|v| v.as_str()), Some("CCP"));

    assert_eq!(
        social_call(
            &ctx,
            &mut recip_sess,
            "calendarMgr",
            "CreateCorporationEvent",
            &[PyRep::Integer(future_ft), PyRep::Integer(60), PyRep::String("Unauthorized".into())],
        )
        .await
        .unwrap_err()
        .code,
        "CalendarAccessDenied"
    );
    let ally_ev = social_call(
        &ctx,
        &mut sender_sess,
        "calendarMgr",
        "CreateAllianceEvent",
        &[
            PyRep::Integer(future_ft),
            PyRep::Integer(120),
            PyRep::String("CTA Stratop".into()),
            PyRep::String("Bring doctrines".into()),
            PyRep::Integer(1),
        ],
    )
    .await
    .unwrap();
    let ev_id = ally_ev
        .as_i64()
        .or_else(|| ally_ev.dict_get("eventID").and_then(|v| v.as_i64()))
        .unwrap();
    social_call(
        &ctx,
        &mut sender_sess,
        "calendarMgr",
        "SendEventResponse",
        &[PyRep::Integer(ev_id), PyRep::Integer(ally_id), PyRep::Integer(4)],
    )
    .await
    .unwrap();

    // 3. Corporate contacts & label bitmasks (requires Diplomat or Director)
    assert_eq!(
        social_call(
            &ctx,
            &mut recip_sess,
            "corpRegistry",
            "AddCorporateContact",
            &[PyRep::Integer(sender_id), PyRep::Real(5.0), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    let add_contact = social_call(
        &ctx,
        &mut sender_sess,
        "corpRegistry",
        "AddCorporateContact",
        &[PyRep::Integer(recip_id), PyRep::Real(15.0), PyRep::Integer(4)],
    )
    .await
    .unwrap();
    assert_eq!(add_contact.dict_get("relationshipID"), Some(&PyRep::Real(10.0)));
    assert_eq!(add_contact.dict_get("labelMask"), Some(&PyRep::Integer(4)));
}
