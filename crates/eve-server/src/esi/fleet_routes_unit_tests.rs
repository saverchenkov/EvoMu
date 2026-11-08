//! Unit and mutation-killing tests for `esi::fleet_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

fn make_claims(char_id: i64) -> SsoTokenClaims {
    SsoTokenClaims {
        sub: format!("CHARACTER:EVE:{char_id}"),
        name: "FleetTester".to_string(),
        owner: "hash".to_string(),
        user_id: 1,
        character_id: char_id,
        scp: Vec::new(),
        iss: "evomu".to_string(),
        aud: vec!["EVE Online".to_string()],
        iat: 1_700_000_000,
        exp: 1_800_000_000,
        jti: "jti-1".to_string(),
    }
}

fn build_test_fleet() -> FleetRecord {
    let mut member_details = HashMap::new();
    member_details.insert(
        90_000_001,
        FleetMemberRecord {
            character_id: 90_000_001,
            role: FleetRole::FleetCommander,
            wing_id: None,
            squad_id: None,
            accepts_regroup: true,
            accepts_conduit_jumps: true,
            job: 0,
            role_name: "Fleet Commander".to_string(),
            join_time_unix: 1_700_000_000,
            takes_fleet_warp: true,
        },
    );
    member_details.insert(
        90_000_002,
        FleetMemberRecord {
            character_id: 90_000_002,
            role: FleetRole::WingCommander,
            wing_id: Some(10),
            squad_id: None,
            accepts_regroup: true,
            accepts_conduit_jumps: true,
            job: 0,
            role_name: "Wing Commander".to_string(),
            join_time_unix: 1_700_000_010,
            takes_fleet_warp: true,
        },
    );
    member_details.insert(
        90_000_003,
        FleetMemberRecord {
            character_id: 90_000_003,
            role: FleetRole::SquadCommander,
            wing_id: Some(10),
            squad_id: Some(100),
            accepts_regroup: true,
            accepts_conduit_jumps: true,
            job: 0,
            role_name: "Squad Commander".to_string(),
            join_time_unix: 1_700_000_020,
            takes_fleet_warp: true,
        },
    );
    member_details.insert(
        90_000_004,
        FleetMemberRecord {
            character_id: 90_000_004,
            role: FleetRole::SquadMember,
            wing_id: Some(10),
            squad_id: Some(100),
            accepts_regroup: true,
            accepts_conduit_jumps: true,
            job: 0,
            role_name: "Squad Member".to_string(),
            join_time_unix: 1_700_000_030,
            takes_fleet_warp: true,
        },
    );

    let mut wing_details = HashMap::new();
    wing_details.insert(
        10,
        FleetWingRecord {
            wing_id: 10,
            name: "Wing 1".to_string(),
            commander_id: Some(90_000_002),
            squad_ids: vec![100],
        },
    );
    wing_details.insert(
        20,
        FleetWingRecord {
            wing_id: 20,
            name: "Wing 2".to_string(),
            commander_id: None,
            squad_ids: vec![200],
        },
    );

    let mut squad_details = HashMap::new();
    squad_details.insert(
        100,
        FleetSquadRecord {
            squad_id: 100,
            wing_id: 10,
            name: "Squad 1".to_string(),
            commander_id: Some(90_000_003),
        },
    );
    squad_details.insert(
        200,
        FleetSquadRecord {
            squad_id: 200,
            wing_id: 20,
            name: "Squad 2".to_string(),
            commander_id: None,
        },
    );

    FleetRecord {
        fleet_id: 1,
        boss_id: 90_000_001,
        members: vec![90_000_001, 90_000_002, 90_000_003, 90_000_004],
        wings: vec![10, 20],
        squads: vec![100, 200],
        free_move: false,
        is_registered: false,
        motd: "Initial MOTD".to_string(),
        member_details,
        wing_details,
        squad_details,
        ..Default::default()
    }
}

#[test]
fn role_hierarchy_permissions_and_wing_squad_validation() {
    for (role, api_str, display_str) in [
        (FleetRole::FleetCommander, "fleet_commander", "Fleet Commander"),
        (FleetRole::WingCommander, "wing_commander", "Wing Commander"),
        (FleetRole::SquadCommander, "squad_commander", "Squad Commander"),
        (FleetRole::SquadMember, "squad_member", "Squad Member"),
    ] {
        assert_eq!(fleet_role_to_str(role), (api_str, display_str));
        assert_eq!(parse_fleet_role(api_str), Some(role));
    }
    assert_eq!(parse_fleet_role("admiral"), None);

    let f = build_test_fleet();
    assert!(is_fleet_member(&f, 90_000_001));
    assert!(is_fleet_member(&f, 90_000_004));
    assert!(!is_fleet_member(&f, 90_000_099));
    assert!(is_fleet_boss_or_fc(&f, 90_000_001));
    assert!(!is_fleet_boss_or_fc(&f, 90_000_002));

    assert_eq!(squad_parent_wing_id(&f, 100), Some(10));
    assert_eq!(squad_parent_wing_id(&f, 200), Some(20));
    assert_eq!(squad_parent_wing_id(&f, 999), None);

    assert!(can_manage_wing(&f, 90_000_001, 20));
    assert!(can_manage_wing(&f, 90_000_002, 10));
    assert!(!can_manage_wing(&f, 90_000_002, 20));
    assert!(!can_manage_wing(&f, 90_000_003, 10));

    assert!(can_manage_squad(&f, 90_000_001, 200));
    assert!(can_manage_squad(&f, 90_000_002, 100));
    assert!(!can_manage_squad(&f, 90_000_002, 200));
    assert!(can_manage_squad(&f, 90_000_003, 100));
    assert!(!can_manage_squad(&f, 90_000_003, 200));
    assert!(!can_manage_squad(&f, 90_000_004, 100));

    assert!(can_invite_member(
        &f,
        90_000_001,
        FleetRole::FleetCommander,
        None,
        None
    ));
    assert!(can_invite_member(
        &f,
        90_000_002,
        FleetRole::SquadCommander,
        Some(10),
        Some(100)
    ));
    assert!(!can_invite_member(
        &f,
        90_000_002,
        FleetRole::WingCommander,
        Some(10),
        None
    ));
    assert!(can_invite_member(
        &f,
        90_000_003,
        FleetRole::SquadMember,
        Some(10),
        Some(100)
    ));
    assert!(!can_invite_member(
        &f,
        90_000_004,
        FleetRole::SquadMember,
        Some(10),
        Some(100)
    ));

    assert!(!can_act_on_member(&f, 90_000_002, 90_000_001, None));
    assert!(can_act_on_member(&f, 90_000_002, 90_000_003, None));
    assert!(can_act_on_member(&f, 90_000_003, 90_000_004, None));
    assert!(!can_act_on_member(&f, 90_000_003, 90_000_002, None));

    assert_eq!(
        validate_wing_and_squad(&f, 1, FleetRole::WingCommander, Some(999), None, true)
            .expect_err("missing wing")
            .status_code,
        404
    );
    assert_eq!(
        validate_wing_and_squad(&f, 1, FleetRole::SquadMember, Some(10), Some(999), true)
            .expect_err("missing squad")
            .status_code,
        404
    );
    assert_eq!(
        validate_wing_and_squad(&f, 1, FleetRole::SquadMember, Some(10), Some(200), true)
            .expect_err("wrong parent wing")
            .status_code,
        400
    );
    assert_eq!(
        validate_wing_and_squad(&f, 1, FleetRole::WingCommander, None, None, true)
            .expect_err("move to WC without wing")
            .status_code,
        400
    );
    assert_eq!(
        validate_wing_and_squad(&f, 1, FleetRole::SquadMember, Some(10), None, true)
            .expect_err("move to SM without squad")
            .status_code,
        400
    );
    assert_eq!(
        validate_wing_and_squad(&f, 1, FleetRole::SquadMember, Some(10), Some(100), true).unwrap(),
        (Some(10), Some(100))
    );
}

#[test]
fn is_fleet_operation_implemented_and_dispatch_fleet_wing_squad_and_ui_routes() {
    assert!(is_fleet_operation_implemented("GET", "/fleets/{fleet_id}"));
    assert!(is_fleet_operation_implemented("POST", "/ui/openwindow/newmail"));
    assert!(!is_fleet_operation_implemented("DELETE", "/fleets/{fleet_id}"));

    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 999, Some(char_id), false, ctx.now_unix());
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(1, build_test_fleet());
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.fleet_id = Some(1);
        }
    });
    let claims = make_claims(char_id);
    let empty_q = HashMap::new();
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let p_fleet = params(&[("fleet_id", "1")]);

    let ch_fleet = dispatch_fleet_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/fleet",
        &p_char,
        &empty_q,
        Some(&claims),
        b"",
    )
    .unwrap();
    assert_eq!(ch_fleet.status_code, 200);
    assert_eq!(body(&ch_fleet)["fleet_id"], 1);

    let put_fleet = dispatch_fleet_routes(
        &ctx,
        "PUT",
        "/fleets/{fleet_id}",
        &p_fleet,
        &empty_q,
        Some(&claims),
        br#"{"motd":"Updated Fleet MOTD","is_free_move":true}"#,
    )
    .unwrap();
    assert_eq!(put_fleet.status_code, 204);

    let get_fleet = dispatch_fleet_routes(
        &ctx,
        "GET",
        "/fleets/{fleet_id}",
        &p_fleet,
        &empty_q,
        Some(&claims),
        b"",
    )
    .unwrap();
    assert_eq!(get_fleet.status_code, 200);
    assert_eq!(body(&get_fleet)["motd"], "Updated Fleet MOTD");
    assert_eq!(body(&get_fleet)["is_free_move"], true);

    let create_wing = dispatch_fleet_routes(
        &ctx,
        "POST",
        "/fleets/{fleet_id}/wings",
        &p_fleet,
        &empty_q,
        Some(&claims),
        b"",
    )
    .unwrap();
    assert_eq!(create_wing.status_code, 201);
    let new_wing_id = body(&create_wing)["wing_id"].as_i64().unwrap();
    let p_wing = params(&[("fleet_id", "1"), ("wing_id", &new_wing_id.to_string())]);

    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/wings/{wing_id}",
            &p_wing,
            &empty_q,
            Some(&claims),
            br#"{"name":"Strike"}"#
        )
        .unwrap()
        .status_code,
        204
    );

    let create_squad = dispatch_fleet_routes(
        &ctx,
        "POST",
        "/fleets/{fleet_id}/wings/{wing_id}/squads",
        &p_wing,
        &empty_q,
        Some(&claims),
        b"",
    )
    .unwrap();
    assert_eq!(create_squad.status_code, 201);
    let new_squad_id = body(&create_squad)["squad_id"].as_i64().unwrap();
    let p_squad = params(&[("fleet_id", "1"), ("squad_id", &new_squad_id.to_string())]);

    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/squads/{squad_id}",
            &p_squad,
            &empty_q,
            Some(&claims),
            br#"{"name":"Alpha"}"#
        )
        .unwrap()
        .status_code,
        204
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "DELETE",
            "/fleets/{fleet_id}/squads/{squad_id}",
            &p_squad,
            &empty_q,
            Some(&claims),
            b""
        )
        .unwrap()
        .status_code,
        204
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "DELETE",
            "/fleets/{fleet_id}/wings/{wing_id}",
            &p_wing,
            &empty_q,
            Some(&claims),
            b""
        )
        .unwrap()
        .status_code,
        204
    );

    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/contract",
            &empty_q,
            &params(&[("contract_id", "66001")]),
            Some(&claims),
            b""
        )
        .unwrap()
        .status_code,
        204
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/marketdetails",
            &empty_q,
            &params(&[("type_id", "34")]),
            Some(&claims),
            b""
        )
        .unwrap()
        .status_code,
        204
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/information",
            &empty_q,
            &params(&[("target_id", &char_id.to_string())]),
            Some(&claims),
            b""
        )
        .unwrap()
        .status_code,
        204
    );
    let conflict_mail = json!({
        "subject": "Subj",
        "body": "Body",
        "recipients": [90000002],
        "to_corp_or_alliance_id": 98000001,
        "to_mailing_list_id": 1
    });
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/newmail",
            &empty_q,
            &empty_q,
            Some(&claims),
            &serde_json::to_vec(&conflict_mail).unwrap()
        )
        .unwrap()
        .status_code,
        422
    );
}

#[test]
fn esi_delete_fleet_member_clears_character_chat_commander_and_bound_objects() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let claims = make_claims(char_id);
    let member_id = 90_000_002_i64;
    let fleet_id = 1_i64;
    let squad_id = 100_i64;
    let fleet_chan_id = format!("fleetid:{fleet_id}");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(fleet_id, build_test_fleet());
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.fleet_id = Some(fleet_id);
        }
        let base = snap.characters[&char_id].clone();
        snap.characters.entry(member_id).or_insert_with(|| {
            let mut ch = base;
            ch.character_id = member_id;
            ch.name = "FleetMate".into();
            ch.corporation_id = corp_id;
            ch
        });
        if let Some(ch) = snap.characters.get_mut(&member_id) {
            ch.fleet_id = Some(fleet_id);
        }
        if let Some(f) = snap.fleets.get_mut(&fleet_id) {
            if !f.members.contains(&member_id) {
                f.members.push(member_id);
            }
            f.member_details.insert(
                member_id,
                FleetMemberRecord {
                    character_id: member_id,
                    role: FleetRole::SquadCommander,
                    wing_id: Some(10),
                    squad_id: Some(squad_id),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: String::new(),
                    join_time_unix: 0,
                },
            );
            if let Some(sq) = f.squad_details.get_mut(&squad_id) {
                sq.commander_id = Some(member_id);
            }
        }
        snap.chat_channels.insert(
            fleet_chan_id.clone(),
            eve_db::ChatChannelRecord {
                channel_id: fleet_chan_id.clone(),
                name: fleet_chan_id.clone(),
                motd: String::new(),
                owner_id: Some(char_id),
                members: std::collections::HashSet::from([char_id, member_id]),
                is_system_local: false,
                solar_system_id: None,
                is_private: true,
                operators: std::collections::HashSet::new(),
                password: None,
                is_delayed_local: false,
            },
        );
    });

    // Register a fleetObjectHandler bound object and quasar subscriber for member_id
    let mut member_sess = eve_machonet::SessionState::new(9_999);
    member_sess.user_id = Some(1);
    member_sess.character_id = Some(member_id);
    member_sess.fleet_id = Some(fleet_id);
    let bound_id = ctx
        .bind_verified_sub_object(&member_sess, "fleetObjectHandler", fleet_id)
        .expect("bind fleetObjectHandler");
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    ctx.quasar_gateway.subscribers.write().unwrap().insert(
        9_999,
        crate::quasar_gateway::GatewaySubscriber {
            session_id: 9_999,
            subscribed_at_unix: 1_700_000_000,
            application_instance_uuid: vec![0; 16],
            user_id: Some(1),
            character_id: Some(member_id as u32),
            corporation_id: Some(corp_id as u32),
            alliance_id: None,
            solar_system_id: Some(30_000_142),
            fleet_id: Some(fleet_id),
            bubble_instance_uuid: None,
            subscribed_notice_types: Vec::new(),
            sender: tx,
            inbox: std::collections::VecDeque::new(),
        },
    );

    let res = dispatch_fleet_routes(
        &ctx,
        "DELETE",
        "/fleets/{fleet_id}/members/{member_id}",
        &params(&[("fleet_id", "1"), ("member_id", &member_id.to_string())]),
        &HashMap::new(),
        Some(&claims),
        b"",
    )
    .unwrap();
    assert_eq!(res.status_code, 204);

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&member_id];
        assert_eq!(ch.fleet_id, None);
        let f = &snap.fleets[&fleet_id];
        assert!(!f.members.contains(&member_id));
        assert!(!f.member_details.contains_key(&member_id));
        assert_eq!(f.squad_details[&squad_id].commander_id, None);
        assert!(!snap.chat_channels[&fleet_chan_id].members.contains(&member_id));
    });
    assert!(
        ctx.release_bound_object(&member_sess, bound_id).is_err(),
        "fleetObjectHandler bound object must be evicted when kicked via ESI"
    );
    assert_eq!(
        ctx.quasar_gateway.subscribers.read().unwrap()[&9_999].fleet_id,
        None,
        "quasar subscriber fleet_id must be cleared when kicked via ESI"
    );
}
