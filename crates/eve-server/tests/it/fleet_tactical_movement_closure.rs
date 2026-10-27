//! Integration tests for fleet movement opt-ins, formations, setups, boss handover, join requests, watchlist, broadcasts, and target tagging.
//!
//! Parity rows:
//! - `feat:fleet.member-movement-optins`
//! - `feat:fleet.fleet-setups`
//! - `feat:fleet.boss-handover`
//! - `feat:fleet.join-requests`
//! - `feat:fleet.watchlist-broadcast-tracking`
//! - `feat:fleet.broadcast-scope-and-filter`
//! - `feat:fleet.stored-formations-and-warp-shapes`
//! - `feat:fleet.takes-fleet-warp-toggle`
//! - `feat:fleet.target-tagging`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;

async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    match rep {
        PyRep::KeyVal(map) => map.get(key).cloned().unwrap_or(PyRep::None),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then(|| v.clone()))
            .unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

#[tokio::test]
async fn test_fleet_movement_optins_formations_and_setups() {
    // parity: feat:fleet.member-movement-optins
    // parity: feat:fleet.takes-fleet-warp-toggle
    // parity: feat:fleet.stored-formations-and-warp-shapes
    // parity: feat:fleet.fleet-setups
    // parity: feat:fleet.boss-handover
    eve_testkit::attest(&[
        "feat:fleet.member-movement-optins",
        "feat:fleet.takes-fleet-warp-toggle",
        "feat:fleet.stored-formations-and-warp-shapes",
        "feat:fleet.fleet-setups",
        "feat:fleet.boss-handover",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 64_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 64_002, "other", "other-secret", PILOT_2).await;

    // Pilot 1 creates a fleet
    let create_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fid = create_rep.as_i64().expect("fleetID");
    sess1.fleet_id = Some(fid);

    // Invite and join Pilot 2 into the fleet
    rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "Invite",
        &[PyRep::Integer(PILOT_2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
    )
    .await
    .expect("Invite Pilot 2");

    rpc(
        &ctx,
        &mut sess2,
        "fleetObjectHandler",
        "AcceptInvite",
        &[PyRep::Integer(fid)],
    )
    .await
    .expect("AcceptInvite Pilot 2");
    sess2.fleet_id = Some(fid);

    // 1. feat:fleet.member-movement-optins & feat:fleet.takes-fleet-warp-toggle
    // Pilot 2 disables takes_fleet_warp
    let warp_opt = rpc(
        &ctx,
        &mut sess2,
        "fleetObjectHandler",
        "SetTakesFleetWarp",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("SetTakesFleetWarp false");
    assert_eq!(field(&warp_opt, "takesFleetWarp"), PyRep::Bool(false));

    // Pilot 2 disables regroup opt-in
    let regroup_opt = rpc(
        &ctx,
        &mut sess2,
        "fleetObjectHandler",
        "SetAcceptsRegroupValue",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("SetAcceptsRegroupValue false");
    assert_eq!(field(&regroup_opt, "acceptsRegroup"), PyRep::Bool(false));

    // Pilot 2 disables conduit jumps opt-in
    let conduit_opt = rpc(
        &ctx,
        &mut sess2,
        "fleetObjectHandler",
        "SetAcceptsConduitJumpsValue",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("SetAcceptsConduitJumpsValue false");
    assert_eq!(field(&conduit_opt, "acceptsConduitJumps"), PyRep::Bool(false));

    // Verify preferences in fleet member record
    ctx.db.with_snapshot(|s| {
        let fleet = s.fleets.get(&fid).expect("fleet");
        let m2 = fleet.member_details.get(&PILOT_2).expect("pilot 2 member");
        assert!(!m2.takes_fleet_warp, "Member takes_fleet_warp must be false");
        assert!(!m2.accepts_regroup, "Member accepts_regroup must be false");
        assert!(!m2.accepts_conduit_jumps, "Member accepts_conduit_jumps must be false");
    });

    // 2. feat:fleet.stored-formations-and-warp-shapes
    // Set Arrow formation (id 3, spacing 5000m, size 10000m)
    let form_rep = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
        "SetFleetFormation",
        &[PyRep::Integer(3), PyRep::Real(5_000.0), PyRep::Real(10_000.0)],
    )
    .await
    .expect("SetFleetFormation");
    assert_eq!(field(&form_rep, "formationID"), PyRep::Integer(3));
    assert_eq!(field(&form_rep, "formationName"), PyRep::String("Arrow".into()));

    // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
    let get_form = rpc(&ctx, &mut sess1, "fleetObjectHandler", "GetFleetFormation", &[])
        .await
        .expect("GetFleetFormation");
    assert_eq!(field(&get_form, "formationID"), PyRep::Integer(3));
    assert_eq!(field(&get_form, "formationName"), PyRep::String("Arrow".into()));

    // 3. feat:fleet.fleet-setups
    // Boss stores fleet setup
    let store_setup = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "StoreFleetSetup",
        &[
            PyRep::String("Alpha Strike Setup".into()),
            PyRep::String("Form up at gate".into()),
            PyRep::Bool(false),
            PyRep::Integer(128),
        ],
    )
    .await
    .expect("StoreFleetSetup");
    assert_eq!(
        field(&store_setup, "setupName"),
        PyRep::String("Alpha Strike Setup".into())
    );

    let get_setups = rpc(&ctx, &mut sess1, "fleetObjectHandler", "GetFleetSetups", &[])
        .await
        .expect("GetFleetSetups");
    let PyRep::Dict(setup_dict) = get_setups else { panic!("expected dict") };
    assert!(setup_dict.iter().any(|(k, _)| k == &PyRep::String("Alpha Strike Setup".into())));

    // Boss loads fleet setup
    let load_setup = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "LoadFleetSetup",
        &[PyRep::String("Alpha Strike Setup".into())],
    )
    .await
    .expect("LoadFleetSetup");
    assert_eq!(
        field(&load_setup, "setupName"),
        PyRep::String("Alpha Strike Setup".into())
    );

    // Delete fleet setup
    let del_setup = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "DeleteFleetSetup",
        &[PyRep::String("Alpha Strike Setup".into())],
    )
    .await
    .expect("DeleteFleetSetup");
    assert_eq!(field(&del_setup, "deleted"), PyRep::Bool(true));

    // 4. feat:fleet.boss-handover
    // Non-boss Pilot 2 cannot call MakeLeader
    let unauth_lead = rpc(
        &ctx,
        &mut sess2,
        "fleetObjectHandler",
        "MakeLeader",
        &[PyRep::Integer(PILOT_1)],
    )
    .await;
    assert!(unauth_lead.is_err(), "Non-boss member cannot transfer leadership");

    // Boss Pilot 1 transfers boss to Pilot 2
    let lead_rep = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "MakeLeader",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("MakeLeader");
    assert_eq!(field(&lead_rep, "newBossID"), PyRep::Integer(PILOT_2));
    ctx.db.with_snapshot(|s| {
        let fleet = s.fleets.get(&fid).unwrap();
        assert_eq!(fleet.boss_id, PILOT_2, "Pilot 2 is now fleet boss");
    });
}

#[tokio::test]
async fn test_fleet_tactical_broadcasts_watchlist_and_tagging() {
    // parity: feat:fleet.join-requests
    // parity: feat:fleet.watchlist-broadcast-tracking
    // parity: feat:fleet.broadcast-scope-and-filter
    // parity: feat:fleet.target-tagging
    eve_testkit::attest(&[
        "feat:fleet.join-requests",
        "feat:fleet.watchlist-broadcast-tracking",
        "feat:fleet.broadcast-scope-and-filter",
        "feat:fleet.target-tagging",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 64_101, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 64_102, "other", "other-secret", PILOT_2).await;

    // Pilot 1 creates fleet
    let create_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fid = create_rep.as_i64().expect("fleetID");
    sess1.fleet_id = Some(fid);

    // Boss adds fleet advert so pilots can apply
    rpc(
        &ctx,
        &mut sess1,
        "fleetProxy",
        "AddFleetFinderAdvert",
        &[
            PyRep::String("Tactical Fleet".into()),
            PyRep::String("Join for ops".into()),
            PyRep::Integer(3),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("AddFleetFinderAdvert");

    // 1. feat:fleet.join-requests
    // Pilot 2 applies to join fleet
    rpc(
        &ctx,
        &mut sess2,
        "fleetProxy",
        "ApplyToJoinFleet",
        &[PyRep::Integer(fid)],
    )
    .await
    .expect("ApplyToJoinFleet");

    // Boss views join requests
    let reqs_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "GetJoinRequests", &[])
        .await
        .expect("GetJoinRequests");
    let PyRep::Dict(reqs) = reqs_rep else { panic!("expected dict") };
    assert!(reqs.iter().any(|(k, _)| k == &PyRep::Integer(PILOT_2)), "Pilot 2 must appear in join requests");

    // Boss rejects request
    rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "RejectJoinRequest",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("RejectJoinRequest");

    // Verify rejection notification sent to Pilot 2
    let p2_notifs = ctx.drain_notifications_for_char(PILOT_2);
    assert!(
        p2_notifs.iter().any(|n| n.event == "OnFleetJoinRejected"),
        "Pilot 2 must receive OnFleetJoinRejected notification"
    );

    // Pilot 2 applies again and Boss accepts
    rpc(
        &ctx,
        &mut sess2,
        "fleetProxy",
        "ApplyToJoinFleet",
        &[PyRep::Integer(fid)],
    )
    .await
    .expect("ApplyToJoinFleet 2nd time");

    rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "AcceptJoinRequest",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("AcceptJoinRequest");
    sess2.fleet_id = Some(fid);

    // 2. feat:fleet.watchlist-broadcast-tracking
    let add_watch = rpc(
        &ctx,
        &mut sess2,
        "fleetMgr",
        "AddToWatchlist",
        &[PyRep::List(vec![PyRep::Integer(PILOT_1)])],
    )
    .await
    .expect("AddToWatchlist");
    assert_eq!(field(&add_watch, "watched"), PyRep::Bool(true));

    let reg_dam = rpc(
        &ctx,
        &mut sess2,
        "fleetMgr",
        "RegisterForDamageUpdates",
        &[PyRep::List(vec![PyRep::Integer(PILOT_1)])],
    )
    .await
    .expect("RegisterForDamageUpdates");
    assert_eq!(field(&reg_dam, "watched"), PyRep::Bool(true));

    let rem_watch = rpc(
        &ctx,
        &mut sess2,
        "fleetMgr",
        "RemoveFromWatchlist",
        &[PyRep::List(vec![PyRep::Integer(PILOT_1)])],
    )
    .await
    .expect("RemoveFromWatchlist");
    assert_eq!(field(&rem_watch, "watched"), PyRep::Bool(false));

    // 3. feat:fleet.broadcast-scope-and-filter
    // Mute NeedShield broadcasts
    let set_filt = rpc(
        &ctx,
        &mut sess2,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
        "SetBroadcastFilter",
        &[PyRep::String("NeedShield".into()), PyRep::Bool(false)],
    )
    .await
    .expect("SetBroadcastFilter");
    assert_eq!(field(&set_filt, "enabled"), PyRep::Bool(false));

    // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
    let get_filt = rpc(&ctx, &mut sess2, "fleetObjectHandler", "GetBroadcastFilter", &[])
        .await
        .expect("GetBroadcastFilter");
    let PyRep::List(muted) = field(&get_filt, "mutedBroadcasts") else { panic!("expected list") };
    assert!(muted.contains(&PyRep::String("NeedShield".into())));

    // Send broadcast
    let bc_rep = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "SendBroadcast",
        &[
            PyRep::String("NeedArmor".into()),
            PyRep::Integer(1), // Entire fleet scope
        ],
    )
    .await
    .expect("SendBroadcast");
    assert_eq!(field(&bc_rep, "broadcast"), PyRep::String("NeedArmor".into()));

    // Rapid duplicate broadcast triggers 2s spam cooldown
    let spam_bc = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "SendBroadcast",
        &[
            PyRep::String("NeedArmor".into()),
            PyRep::Integer(1),
        ],
    )
    .await;
    assert!(spam_bc.is_err(), "Duplicate broadcast within 2s must be rejected with BroadcastSpamCooldown");

    // Pilot 2 (squad member) attempts restricted broadcast ("Target") and is rejected with CannotBroadcastInFleet
    let unauth_bc = rpc(
        &ctx,
        &mut sess2,
        "fleetObjectHandler",
        "SendBroadcast",
        &[
            PyRep::String("Target".into()),
            PyRep::Integer(1),
        ],
    )
    .await;
    let err = unauth_bc.expect_err("Unauthorized member must not send restricted broadcast");
    assert_eq!(err.code, "CannotBroadcastInFleet");

    // 4. feat:fleet.target-tagging
    // Boss assigns tag 'A' to target entity 999001
    let tag_rep = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        "SetTargetTag",
        &[PyRep::Integer(999001), PyRep::String("A".into())],
    )
    .await
    .expect("SetTargetTag");
    assert_eq!(field(&tag_rep, "targetID"), PyRep::Integer(999001));
    assert_eq!(field(&tag_rep, "tag"), PyRep::String("A".into()));

    // Clear target tags
    let clear_tags = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        "ClearTargetTags",
        &[],
    )
    .await
    .expect("ClearTargetTags");
    assert_eq!(field(&clear_tags, "fleetID"), PyRep::Integer(fid));
}
