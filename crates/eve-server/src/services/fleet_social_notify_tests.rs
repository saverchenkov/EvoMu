use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_and_clock,
};
use eve_db::FleetRole;

fn mk_member(cid: i64, role: FleetRole, wing: Option<i64>, squad: Option<i64>) -> FleetMemberRecord {
    FleetMemberRecord {
        character_id: cid,
        role,
        wing_id: wing,
        squad_id: squad,
        takes_fleet_warp: true,
        accepts_regroup: false,
        accepts_conduit_jumps: true,
        job: 2,
        role_name: String::new(),
        join_time_unix: 1000,
    }
}

#[test]
fn member_options_and_broadcast_group_scope_logic() {
    let fc = mk_member(100, FleetRole::FleetCommander, Some(1), Some(10));
    let wc = mk_member(101, FleetRole::WingCommander, Some(1), Some(10));
    let sc = mk_member(102, FleetRole::SquadCommander, Some(1), Some(10));
    let sm = mk_member(103, FleetRole::SquadMember, Some(1), Some(10));
    let other_sm = mk_member(104, FleetRole::SquadMember, Some(2), Some(20));

    let opt = member_opt_outs(&fc);
    assert_eq!(opt.dict_get("acceptsFleetWarp").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(opt.dict_get("acceptsFleetRegroups").and_then(|v| v.as_bool()), Some(false));

    let mkv = member_keyval(&fc);
    assert_eq!(mkv.dict_get("charID").and_then(|v| v.as_i64()), Some(100));

    let okv = options_keyval(true, false, Some(10));
    assert_eq!(okv.dict_get("isFreeMove").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(okv.dict_get("autoJoinSquadID").and_then(|v| v.as_i64()), Some(10));

    assert!(broadcast_group_allows(BROADCAST_DOWN, &fc, &fc));
    assert!(broadcast_group_allows(BROADCAST_DOWN, &fc, &other_sm));
    assert!(broadcast_group_allows(BROADCAST_DOWN, &wc, &sm));
    assert!(!broadcast_group_allows(BROADCAST_DOWN, &wc, &other_sm));
    assert!(!broadcast_group_allows(BROADCAST_DOWN, &sm, &fc));

    assert!(broadcast_group_allows(BROADCAST_UP, &sm, &sc));
    assert!(broadcast_group_allows(BROADCAST_UP, &sm, &wc));
    assert!(broadcast_group_allows(BROADCAST_UP, &sm, &fc));
    assert!(!broadcast_group_allows(BROADCAST_UP, &other_sm, &wc));
    assert!(broadcast_group_allows(0, &other_sm, &wc));
}

#[tokio::test]
async fn fleet_removal_handover_watchlist_and_corp_contacts() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, _) = seed_multi_member_fleet(&ctx).await;
    assert_eq!(character_active_fleet_id(&ctx, 90_000_001), Some(fleet_id));

    ctx.fleet_extras
        .lock()
        .unwrap()
        .watchlist
        .entry(90_000_001)
        .or_default()
        .insert(90_000_002);
    stream_fleet_watchlist_damage(&ctx, 90_000_002, 0.8, 0.9, 1.0);

    let (disbanded, new_boss) = remove_character_from_fleet(&ctx, fleet_id, 90_000_001);
    assert!(!disbanded);
    assert!(new_boss.is_some());
    ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fleet_id).unwrap();
        assert_ne!(f.boss_id, 90_000_001);
        assert!(!build_wings_dict(f).is_empty());
    });

    let corp_id = 1_000_009;
    assert!(handle_corp_contacts_and_labels(&ctx, corp_id, false, "AddCorporateContact", &[]).unwrap().is_err());

    let added = handle_corp_contacts_and_labels(
        &ctx,
        corp_id,
        true,
        "AddCorporateContact",
        &[PyRep::Integer(90_000_002), PyRep::Real(5.0), PyRep::Integer(1)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(added.dict_get("contactID").and_then(|v| v.as_i64()), Some(90_000_002));

    let edited = handle_corp_contacts_and_labels(
        &ctx,
        corp_id,
        true,
        "EditCorporateContact",
        &[PyRep::Integer(90_000_002), PyRep::Real(10.0), PyRep::Integer(3)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(edited.dict_get("relationshipID").and_then(|v| v.as_f64()), Some(10.0));

    let assigned = handle_corp_contacts_and_labels(
        &ctx,
        corp_id,
        true,
        "AssignLabels",
        &[PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::Integer(4)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(assigned.dict_get("labelID").and_then(|v| v.as_i64()), Some(4));
}
