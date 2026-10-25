//! Modular integration tests for Fleet Object Handler (`fleetObjectHandler`) hierarchy,
//! wing/squad tree mutations, and fleet membership/roster lifecycle RPCs.

use crate::common;
use eve_db::FleetRole;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;
const SERVICE: &str = "fleetObjectHandler";

async fn setup_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
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
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, SERVICE, None, method, args, &HashMap::new(), None)
        .await
}

fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
    rep.dict_get(key).and_then(|v| v.as_i64())
}

fn get_bool(rep: &PyRep, key: &str) -> Option<bool> {
    rep.dict_get(key).and_then(|v| v.as_bool())
}

fn get_str<'a>(rep: &'a PyRep, key: &str) -> Option<&'a str> {
    rep.dict_get(key).and_then(|v| v.as_str())
}

/// 1. Fleet creation, initial state inspection, and wing/squad hierarchy tree creation.
#[tokio::test]
async fn test_fleet_object_hierarchy_creation_and_state() {
    // parity: rpc:fleetObjectHandler.CreateFleet, rpc:fleetObjectHandler.Init, rpc:fleetObjectHandler.GetFleetID, rpc:fleetObjectHandler.GetInitState, rpc:fleetObjectHandler.CreateWing, rpc:fleetObjectHandler.CreateSquad
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = setup_pilot(&ctx, 1001, "demo", "demo-secret", PILOT1).await;
    let mut sess2 = setup_pilot(&ctx, 1002, "other", "other-secret", PILOT2).await;

    // A fresh session not in a fleet returns None for GetFleetID
    let unassigned_fid = rpc(&ctx, &mut sess1, "GetFleetID", &[])
        .await
        .expect("GetFleetID before fleet creation");
    assert_eq!(unassigned_fid, PyRep::None);

    // 1. CreateFleet: creates default wing 1, squad 1, fleet channel, and sets commander
    let create_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateFleet",
        &[
            PyRep::String("Strike Force Alpha".into()),
            PyRep::String("Check MOTD before jump".into()),
        ],
    )
    .await
    .expect("CreateFleet");

    let fid1 = get_i64(&create_rep, "fleetID").expect("fleetID");
    assert!(fid1 > 0);
    assert_eq!(get_i64(&create_rep, "bossID"), Some(PILOT1));
    assert_eq!(get_i64(&create_rep, "wingID"), Some(1));
    assert_eq!(get_i64(&create_rep, "squadID"), Some(1));
    assert_eq!(get_str(&create_rep, "fleetName"), Some("Strike Force Alpha"));
    assert_eq!(get_str(&create_rep, "motd"), Some("Check MOTD before jump"));
    assert_eq!(
        get_str(&create_rep, "channelName"),
        Some(format!("fleetid:{fid1}").as_str())
    );
    assert_eq!(sess1.fleet_id, Some(fid1));

    // Verify snapshot state after CreateFleet
    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid1).expect("fleet must exist in snapshot");
        assert_eq!(fleet.boss_id, PILOT1);
        assert_eq!(fleet.members, vec![PILOT1]);
        assert_eq!(fleet.wings, vec![1]);
        assert_eq!(fleet.squads, vec![1]);
        assert_eq!(fleet.motd, "Check MOTD before jump");
        let leader = fleet.member_details.get(&PILOT1).expect("boss in member_details");
        assert_eq!(leader.role, FleetRole::FleetCommander);
        assert!(snap.chat_channels.contains_key(&format!("fleetid:{fid1}")));
    });

    // 2. GetFleetID: retrieves current fleet ID
    let fid_rep = rpc(&ctx, &mut sess1, "GetFleetID", &[])
        .await
        .expect("GetFleetID");
    assert_eq!(fid_rep.as_i64(), Some(fid1));

    // 3. CreateWing: appends a new wing to the fleet hierarchy
    let wing_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateWing",
        &[PyRep::String("Wing Bravo".into())],
    )
    .await
    .expect("CreateWing");

    let wid2 = get_i64(&wing_rep, "wingID").expect("wingID");
    assert_eq!(wid2, 2);
    assert_eq!(get_i64(&wing_rep, "fleetID"), Some(fid1));
    assert_eq!(get_str(&wing_rep, "name"), Some("Wing Bravo"));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid1).expect("fleet");
        assert_eq!(fleet.wings, vec![1, 2]);
        let w = fleet.wing_details.get(&wid2).expect("wing 2 details");
        assert_eq!(w.name, "Wing Bravo");
        assert!(w.squad_ids.is_empty());
    });

    // 4. CreateSquad: creates a squad within wing 2
    let squad_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateSquad",
        &[PyRep::Integer(wid2), PyRep::String("Squad Logistics".into())],
    )
    .await
    .expect("CreateSquad");

    let sid2 = get_i64(&squad_rep, "squadID").expect("squadID");
    assert_eq!(sid2, 2);
    assert_eq!(get_i64(&squad_rep, "fleetID"), Some(fid1));
    assert_eq!(get_i64(&squad_rep, "wingID"), Some(wid2));
    assert_eq!(get_str(&squad_rep, "name"), Some("Squad Logistics"));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid1).expect("fleet");
        assert_eq!(fleet.squads, vec![1, 2]);
        let w = fleet.wing_details.get(&wid2).expect("wing 2");
        assert_eq!(w.squad_ids, vec![sid2]);
        let s = fleet.squad_details.get(&sid2).expect("squad 2");
        assert_eq!(s.wing_id, wid2);
        assert_eq!(s.name, "Squad Logistics");
    });

    // 5. GetInitState: returns structured keyval with full fleet hierarchy and member rowsets
    let init_rep = rpc(&ctx, &mut sess1, "GetInitState", &[])
        .await
        .expect("GetInitState");

    assert_eq!(get_i64(&init_rep, "fleetID"), Some(fid1));
    assert_eq!(get_i64(&init_rep, "bossID"), Some(PILOT1));
    assert_eq!(get_str(&init_rep, "fleetName"), Some("Strike Force Alpha"));
    assert_eq!(get_str(&init_rep, "motd"), Some("Check MOTD before jump"));
    assert_eq!(get_bool(&init_rep, "isFreeMove"), Some(false));
    assert_eq!(get_bool(&init_rep, "isRegistered"), Some(false));
    assert_eq!(get_i64(&init_rep, "maxSize"), Some(256));

    let PyRep::Dict(wings_dict) = init_rep.dict_get("wings").expect("wings dict") else {
        panic!("expected wings dict");
    };
    assert_eq!(wings_dict.len(), 2);
    assert_eq!(wings_dict[0].0, PyRep::Integer(1));
    assert_eq!(wings_dict[1].0, PyRep::Integer(2));
    assert_eq!(wings_dict[1].1.dict_get("name"), Some(&PyRep::String("Wing Bravo".into())));

    let PyRep::Rowset { rows: squad_rows, .. } = init_rep.dict_get("squads").expect("squads rowset") else {
        panic!("expected squads rowset");
    };
    assert_eq!(squad_rows.len(), 2);
    assert_eq!(squad_rows[1][0], PyRep::Integer(2));
    assert_eq!(squad_rows[1][1], PyRep::Integer(wid2));
    assert_eq!(squad_rows[1][2], PyRep::String("Squad Logistics".into()));

    let PyRep::Dict(members_dict) = init_rep.dict_get("members").expect("members dict") else {
        panic!("expected members dict");
    };
    assert_eq!(members_dict.len(), 1);
    assert_eq!(members_dict[0].0, PyRep::Integer(PILOT1));
    assert_eq!(members_dict[0].1.dict_get("role"), Some(&PyRep::Integer(1))); // FleetCommander role

    // 6. Init: when called on bound fleet, updates ship and does not create a new fleet
    let init_bound_rep = rpc(
        &ctx,
        &mut sess1,
        "Init",
        &[PyRep::Integer(587)],
    )
    .await
    .expect("Init on bound fleet");
    assert_eq!(init_bound_rep, PyRep::None);

    let fid_after_init = rpc(&ctx, &mut sess1, "GetFleetID", &[]).await.expect("GetFleetID");
    assert_eq!(fid_after_init.as_i64(), Some(fid1));

    // Pilot 2 creates a new fleet via CreateFleet
    let init_create_rep = rpc(
        &ctx,
        &mut sess2,
        "CreateFleet",
        &[
            PyRep::String("Vanguard Fleet".into()),
            PyRep::String("Vanguard MOTD".into()),
        ],
    )
    .await
    .expect("CreateFleet for pilot 2");

    let fid2 = get_i64(&init_create_rep, "fleetID").expect("fleetID for pilot 2");
    assert!(fid2 > 0);
    assert_ne!(fid1, fid2);
    assert_eq!(get_i64(&init_create_rep, "bossID"), Some(PILOT2));
    assert_eq!(get_str(&init_create_rep, "fleetName"), Some("Vanguard Fleet"));
    assert_eq!(get_str(&init_create_rep, "motd"), Some("Vanguard MOTD"));
    assert_eq!(sess2.fleet_id, Some(fid2));
}

/// 2. Renaming wings and squads, querying wings, deleting squads/wings, and fleet disbandment.
#[tokio::test]
async fn test_fleet_object_hierarchy_renaming_and_teardown() {
    // parity: rpc:fleetObjectHandler.ChangeWingName, rpc:fleetObjectHandler.ChangeSquadName, rpc:fleetObjectHandler.GetWings, rpc:fleetObjectHandler.DeleteSquad, rpc:fleetObjectHandler.DeleteWing, rpc:fleetObjectHandler.DisbandFleet
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = setup_pilot(&ctx, 1011, "demo", "demo-secret", PILOT1).await;

    // Create base fleet with wing 2 and squad 2
    let create_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateFleet",
        &[PyRep::String("Lifecycle Fleet".into()), PyRep::String("Teardown MOTD".into())],
    )
    .await
    .expect("CreateFleet");
    let fid = get_i64(&create_rep, "fleetID").expect("fleetID");

    let wing_rep = rpc(&ctx, &mut sess1, "CreateWing", &[PyRep::String("Wing Original".into())])
        .await
        .expect("CreateWing");
    let wid = get_i64(&wing_rep, "wingID").expect("wingID");

    let squad_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateSquad",
        &[PyRep::Integer(wid), PyRep::String("Squad Original".into())],
    )
    .await
    .expect("CreateSquad");
    let sid = get_i64(&squad_rep, "squadID").expect("squadID");

    // 1. ChangeWingName: update the display label of wing 2
    let rename_wing_rep = rpc(
        &ctx,
        &mut sess1,
        "ChangeWingName",
        &[PyRep::Integer(wid), PyRep::String("Assault Wing".into())],
    )
    .await
    .expect("ChangeWingName");

    assert_eq!(get_i64(&rename_wing_rep, "wingID"), Some(wid));
    assert_eq!(get_str(&rename_wing_rep, "name"), Some("Assault Wing"));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert_eq!(fleet.wing_details[&wid].name, "Assault Wing");
    });

    // 2. ChangeSquadName: update the display label of squad 2
    let rename_squad_rep = rpc(
        &ctx,
        &mut sess1,
        "ChangeSquadName",
        &[PyRep::Integer(sid), PyRep::String("Heavy Assault Squad".into())],
    )
    .await
    .expect("ChangeSquadName");

    assert_eq!(get_i64(&rename_squad_rep, "squadID"), Some(sid));
    assert_eq!(get_str(&rename_squad_rep, "name"), Some("Heavy Assault Squad"));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert_eq!(fleet.squad_details[&sid].name, "Heavy Assault Squad");
    });

    // 3. GetWings: retrieves the updated hierarchy tree as a wings dict
    let wings_rep = rpc(&ctx, &mut sess1, "GetWings", &[]).await.expect("GetWings");
    let PyRep::Dict(wings_dict) = wings_rep else {
        panic!("expected wings dict");
    };
    let wing2_entry = wings_dict.iter().find(|(k, _)| k == &PyRep::Integer(wid)).expect("wing 2 in dict");
    assert_eq!(wing2_entry.1.dict_get("name"), Some(&PyRep::String("Assault Wing".into())));

    let PyRep::Dict(squads_dict) = wing2_entry.1.dict_get("squads").expect("squads dict") else {
        panic!("expected squads dict");
    };
    let squad2_entry = squads_dict.iter().find(|(k, _)| k == &PyRep::Integer(sid)).expect("squad 2 in dict");
    assert_eq!(squad2_entry.1.dict_get("name"), Some(&PyRep::String("Heavy Assault Squad".into())));

    // 4. DeleteSquad: removes empty squad 2 from wing 2 and fleet roster
    let del_squad_rep = rpc(&ctx, &mut sess1, "DeleteSquad", &[PyRep::Integer(sid)])
        .await
        .expect("DeleteSquad");

    assert_eq!(get_i64(&del_squad_rep, "squadID"), Some(sid));
    assert_eq!(get_bool(&del_squad_rep, "deleted"), Some(true));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert!(!fleet.squads.contains(&sid));
        assert!(!fleet.squad_details.contains_key(&sid));
        assert!(!fleet.wing_details[&wid].squad_ids.contains(&sid));
    });

    // Deleting the squad again fails with SquadNotFound
    let err_sq = rpc(&ctx, &mut sess1, "DeleteSquad", &[PyRep::Integer(sid)])
        .await
        .expect_err("DeleteSquad on nonexistent squad");
    assert_eq!(err_sq.code, "SquadNotFound");

    // 5. DeleteWing: removes empty wing 2 from fleet
    let del_wing_rep = rpc(&ctx, &mut sess1, "DeleteWing", &[PyRep::Integer(wid)])
        .await
        .expect("DeleteWing");

    assert_eq!(get_i64(&del_wing_rep, "wingID"), Some(wid));
    assert_eq!(get_bool(&del_wing_rep, "deleted"), Some(true));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert!(!fleet.wings.contains(&wid));
        assert!(!fleet.wing_details.contains_key(&wid));
    });

    // Deleting the wing again fails with WingNotFound
    let err_w = rpc(&ctx, &mut sess1, "DeleteWing", &[PyRep::Integer(wid)])
        .await
        .expect_err("DeleteWing on nonexistent wing");
    assert_eq!(err_w.code, "WingNotFound");

    // 6. DisbandFleet: destroys fleet entirely, removes chat channel, clears session
    let disband_rep = rpc(&ctx, &mut sess1, "DisbandFleet", &[])
        .await
        .expect("DisbandFleet");

    assert_eq!(get_i64(&disband_rep, "fleetID"), Some(fid));
    assert_eq!(get_bool(&disband_rep, "disbanded"), Some(true));
    assert_eq!(sess1.fleet_id, None);

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fleets.contains_key(&fid));
        assert!(!snap.chat_channels.contains_key(&format!("fleetid:{fid}")));
    });
}

/// 3. Fleet invites, application/join request handling, member kick, and voluntary departure.
#[tokio::test]
async fn test_fleet_object_membership_invites_and_roster() {
    // parity: rpc:fleetObjectHandler.AcceptInvite, rpc:fleetObjectHandler.RejectInvite, rpc:fleetObjectHandler.GetJoinRequests, rpc:fleetObjectHandler.RejectJoinRequest, rpc:fleetObjectHandler.KickMember, rpc:fleetObjectHandler.LeaveFleet
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = setup_pilot(&ctx, 1021, "demo", "demo-secret", PILOT1).await;
    let mut sess2 = setup_pilot(&ctx, 1022, "other", "other-secret", PILOT2).await;

    let create_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateFleet",
        &[PyRep::String("Roster Fleet".into()), PyRep::String("Join test MOTD".into())],
    )
    .await
    .expect("CreateFleet");
    let fid = get_i64(&create_rep, "fleetID").expect("fleetID");

    // Step A: Boss posts fleet advert with auto_join = false, and Pilot 2 applies to join
    ctx.dispatch_rpc(
        &mut sess1,
        "fleetProxy",
        None,
        "AddFleetFinderAdvert",
        &[
            PyRep::String("Roster Advert".into()),
            PyRep::String("Recruiting".into()),
            PyRep::Integer(3), // public invite scope
            PyRep::Bool(false), // auto_join = false
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("AddFleetFinderAdvert");

    let apply_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "fleetProxy",
            None,
            "ApplyToJoinFleet",
            &[PyRep::Integer(fid)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("ApplyToJoinFleet");
    assert_eq!(get_bool(&apply_rep, "joined"), Some(false));

    // 1. GetJoinRequests: boss inspects pending join requests (Pilot 2)
    let reqs_rep = rpc(&ctx, &mut sess1, "GetJoinRequests", &[])
        .await
        .expect("GetJoinRequests");

    let PyRep::Dict(reqs_dict) = reqs_rep else {
        panic!("expected dict from GetJoinRequests");
    };
    assert_eq!(reqs_dict.len(), 1);
    assert_eq!(reqs_dict[0].0, PyRep::Integer(PILOT2));

    // 2. RejectJoinRequest: boss cancels/rejects the join request
    let rej_req_rep = rpc(
        &ctx,
        &mut sess1,
        "RejectJoinRequest",
        &[PyRep::Integer(PILOT2)],
    )
    .await
    .expect("RejectJoinRequest");

    assert_eq!(get_i64(&rej_req_rep, "characterID"), Some(PILOT2));
    assert_eq!(get_bool(&rej_req_rep, "rejected"), Some(true));

    let reqs_after = rpc(&ctx, &mut sess1, "GetJoinRequests", &[])
        .await
        .expect("GetJoinRequests after reject");
    let PyRep::Dict(reqs_after_dict) = reqs_after else {
        panic!("expected dict");
    };
    assert!(reqs_after_dict.is_empty());

    // Step B: Invite Pilot 2 again and test RejectInvite from invitee perspective
    rpc(
        &ctx,
        &mut sess1,
        "Invite",
        &[
            PyRep::Integer(PILOT2),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("Invite Pilot 2 second time");

    // 3. RejectInvite: invitee declines incoming invitation
    let rej_inv_rep = rpc(
        &ctx,
        &mut sess2,
        "RejectInvite",
        &[PyRep::Integer(fid)],
    )
    .await
    .expect("RejectInvite");

    assert_eq!(get_i64(&rej_inv_rep, "fleetID"), Some(fid));
    assert_eq!(get_bool(&rej_inv_rep, "rejected"), Some(true));
    assert_eq!(sess2.fleet_id, None);

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert!(!fleet.invites.contains(&PILOT2));
        assert!(!fleet.members.contains(&PILOT2));
    });

    // Step C: Invite Pilot 2 a third time and test AcceptInvite
    rpc(
        &ctx,
        &mut sess1,
        "Invite",
        &[
            PyRep::Integer(PILOT2),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("Invite Pilot 2 third time");

    // 4. AcceptInvite: invitee joins the fleet roster and chat channel
    ctx.advance_secs(12);
    let accept_rep = rpc(
        &ctx,
        &mut sess2,
        "AcceptInvite",
        &[PyRep::Integer(fid)],
    )
    .await
    .expect("AcceptInvite");

    assert_eq!(get_i64(&accept_rep, "fleetID"), Some(fid));
    assert_eq!(get_i64(&accept_rep, "characterID"), Some(PILOT2));
    assert_eq!(get_i64(&accept_rep, "wingID"), Some(1));
    assert_eq!(get_i64(&accept_rep, "squadID"), Some(1));
    assert_eq!(get_i64(&accept_rep, "role"), Some(4)); // SquadMember
    assert_eq!(get_str(&accept_rep, "motd"), Some("Join test MOTD"));
    assert_eq!(sess2.fleet_id, Some(fid));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert!(fleet.members.contains(&PILOT2));
        assert!(fleet.member_details.contains_key(&PILOT2));
        let chan = snap.chat_channels.get(&format!("fleetid:{fid}")).expect("chat channel");
        assert!(chan.members.contains(&PILOT2));
    });

    // 5. LeaveFleet: member voluntarily departs
    let leave_rep = rpc(&ctx, &mut sess2, "LeaveFleet", &[])
        .await
        .expect("LeaveFleet");

    assert_eq!(get_i64(&leave_rep, "fleetID"), Some(fid));
    assert_eq!(get_i64(&leave_rep, "leftFleetID"), Some(fid));
    assert_eq!(get_bool(&leave_rep, "left"), Some(true));
    assert_eq!(get_bool(&leave_rep, "disbanded"), Some(false));
    assert_eq!(sess2.fleet_id, None);

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert!(!fleet.members.contains(&PILOT2));
        assert!(!fleet.member_details.contains_key(&PILOT2));
        let chan = snap.chat_channels.get(&format!("fleetid:{fid}")).expect("chat channel");
        assert!(!chan.members.contains(&PILOT2));
    });

    // Step D: Re-invite Pilot 2, accept, then test KickMember by boss
    ctx.advance_virtual_clock(10);
    rpc(
        &ctx,
        &mut sess1,
        "Invite",
        &[
            PyRep::Integer(PILOT2),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("Re-invite Pilot 2 for kick test");

    rpc(&ctx, &mut sess2, "AcceptInvite", &[PyRep::Integer(fid)])
        .await
        .expect("AcceptInvite for kick test");
    assert_eq!(sess2.fleet_id, Some(fid));

    // 6. KickMember: boss forcefully removes Pilot 2
    let kick_rep = rpc(
        &ctx,
        &mut sess1,
        "KickMember",
        &[PyRep::Integer(PILOT2)],
    )
    .await
    .expect("KickMember");

    assert_eq!(get_i64(&kick_rep, "kickedCharacterID"), Some(PILOT2));
    assert_eq!(get_i64(&kick_rep, "fleetID"), Some(fid));
    assert_eq!(get_bool(&kick_rep, "disbanded"), Some(false));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert!(!fleet.members.contains(&PILOT2));
        assert!(!fleet.member_details.contains_key(&PILOT2));
    });
}
