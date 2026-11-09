use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_and_clock,
};

fn make_session(char_id: i64) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    }
}

#[test]
fn role_mappings_and_channel_member_helpers() {
    assert_eq!(role_to_i64(FleetRole::FleetCommander), 1);
    assert_eq!(role_to_i64(FleetRole::WingCommander), 2);
    assert_eq!(role_to_i64(FleetRole::SquadCommander), 3);
    assert_eq!(role_to_i64(FleetRole::SquadMember), 4);

    assert_eq!(i64_to_role(1), FleetRole::FleetCommander);
    assert_eq!(i64_to_role(2), FleetRole::WingCommander);
    assert_eq!(i64_to_role(3), FleetRole::SquadCommander);
    assert_eq!(i64_to_role(4), FleetRole::SquadMember);
    assert_eq!(i64_to_role(99), FleetRole::SquadMember);

    assert_eq!(role_name_str(FleetRole::FleetCommander), "Fleet Commander");
    assert_eq!(role_name_str(FleetRole::WingCommander), "Wing Commander");
    assert_eq!(role_name_str(FleetRole::SquadCommander), "Squad Commander");
    assert_eq!(role_name_str(FleetRole::SquadMember), "Squad Member");

    let mut channels = HashMap::new();
    ensure_fleet_channel_member(&mut channels, 42, 1001, 1002);
    let ch = channels.get("fleetid:42").unwrap();
    assert_eq!(ch.owner_id, Some(1001));
    assert!(ch.members.contains(&1002));
}

#[tokio::test]
async fn create_fleet_wing_squad_invite_and_move_lifecycle() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let boss_id = 90_000_001;
    let member_id = 90_000_002;
    let member_uid = ctx.db.with_mut_snapshot(|snap| {
        let orig_uid = snap.characters.get(&boss_id).unwrap().user_id;
        let mut m = clone_char_with(snap, member_id, "Fleet Member", 1_000_009, 30_000_142, 100_000_002);
        m.user_id = orig_uid;
        snap.characters.insert(member_id, m);
        orig_uid
    });
    ctx.db
        .acquire_online_lock(member_uid, 2, Some(member_id), false, 1_700_000_000)
        .unwrap();
    let mut boss_session = make_session(boss_id);
    let mut member_session = make_session(member_id);
    let kw = HashMap::new();
    let svc = "fleetMgr";

    let created = handle_fleets(
        &ctx,
        &mut boss_session,
        svc,
        "CreateFleet",
        &[PyRep::String("roam fleet".into()), PyRep::String("motd text".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let fleet_id = created.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(boss_session.fleet_id, Some(fleet_id));

    let init = handle_fleets(&ctx, &mut boss_session, "fleetObjectHandler", "Init", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(init, PyRep::None);

    let init_state = handle_fleets(
        &ctx,
        &mut boss_session,
        "fleetObjectHandler",
        "GetInitState",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let members = init_state.dict_get("members").unwrap();
    let PyRep::Dict(member_entries) = members else {
        panic!("expected members dict");
    };
    let boss_rec = member_entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(boss_id))
        .map(|(_, v)| v)
        .expect("boss in GetInitState members");
    assert_eq!(boss_rec.dict_get("wingID").and_then(|v| v.as_i64()), Some(-1));
    assert_eq!(boss_rec.dict_get("squadID").and_then(|v| v.as_i64()), Some(-1));
    assert_eq!(boss_rec.dict_get("role").and_then(|v| v.as_i64()), Some(1));

    let wing = handle_fleets(
        &ctx,
        &mut boss_session,
        svc,
        "CreateWing",
        &[PyRep::String("wing two".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let wing_id = wing.as_i64().or_else(|| wing.dict_get("wingID").and_then(|v| v.as_i64())).unwrap();

    let squad = handle_fleets(
        &ctx,
        &mut boss_session,
        svc,
        "CreateSquad",
        &[PyRep::Integer(wing_id), PyRep::String("squad alpha".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let squad_id = squad.as_i64().or_else(|| squad.dict_get("squadID").and_then(|v| v.as_i64())).unwrap();

    let _inv = handle_fleets(
        &ctx,
        &mut boss_session,
        svc,
        "Invite",
        &[
            PyRep::Integer(member_id),
            PyRep::Integer(wing_id),
            PyRep::Integer(squad_id),
            PyRep::Integer(4),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(invited_fleet(&ctx, member_id, Some(fleet_id)), fleet_id);

    let _acc = handle_fleets(
        &ctx,
        &mut member_session,
        "fleetObjectHandler",
        "AcceptInvite",
        &[PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(member_session.fleet_id, Some(fleet_id));

    let moved = handle_fleets(
        &ctx,
        &mut boss_session,
        svc,
        "MoveMember",
        &[
            PyRep::Integer(member_id),
            PyRep::Integer(wing_id),
            PyRep::Integer(squad_id),
            PyRep::Integer(3),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(moved.dict_get("role").and_then(|v| v.as_i64()), Some(3));

    let fin = handle_fleets(&ctx, &mut boss_session, "fleetObjectHandler", "FinishMove", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fin, PyRep::None);

    let _leave = handle_fleets(&ctx, &mut member_session, svc, "LeaveFleet", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member_session.fleet_id, None);
}
