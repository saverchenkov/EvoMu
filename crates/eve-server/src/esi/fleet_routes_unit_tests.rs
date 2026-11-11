//! Unit and mutation-killing tests for `esi::fleet_routes`.

use super::*;
use crate::domain::fleets::{
    can_act_on_member, can_invite_member, can_manage_squad, can_manage_wing,
    fleet_role_to_esi_str as fleet_role_to_str, is_fleet_boss_or_fc, is_fleet_member,
    member_effective_position, parse_esi_fleet_role as parse_fleet_role, squad_parent_wing_id,
};
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use eve_db::{FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord, FleetWingRecord};

fn validate_wing_and_squad(
    f: &FleetRecord,
    fleet_id: i64,
    role: FleetRole,
    wing_id: Option<i64>,
    squad_id: Option<i64>,
    is_move: bool,
) -> Result<(Option<i64>, Option<i64>), RouteExecutionResult> {
    fleets::validate_wing_and_squad(f, fleet_id, role, wing_id, squad_id, is_move)
        .map_err(domain_err_to_esi)
}

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

#[test]
fn fleet_routes_machonet_notifications_db_sync_and_none_boundaries() {
    use eve_proto::PyRep;

    let (ctx, _token, boss_id, _corp_id, _planet_id) = seed_full_esi_context();
    let member_id = 90_000_002_i64;
    let extra_id = 90_000_003_i64;

    let uid_boss = ctx.db.with_snapshot(|s| s.characters[&boss_id].user_id);
    let uid_member = ctx.db.provision_player_account("fleet_notif_m", "pw");
    let uid_extra = ctx.db.provision_player_account("fleet_notif_e", "pw");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(1, build_test_fleet());
        if let Some(ch) = snap.characters.get_mut(&boss_id) {
            ch.fleet_id = Some(1);
            ch.station_id = None;
            ch.solar_system_id = 30_000_142;
        }
        if let Some(m) = snap.characters.get_mut(&member_id) {
            m.user_id = uid_member;
            m.fleet_id = Some(1);
            m.station_id = Some(60_003_760);
            m.solar_system_id = 30_000_142;
        }
        let mut extra_ch = snap.characters[&boss_id].clone();
        extra_ch.character_id = extra_id;
        extra_ch.user_id = uid_extra;
        extra_ch.name = "ExtraInvitee".into();
        extra_ch.fleet_id = None;
        snap.characters.insert(extra_id, extra_ch);
    });

    let now = ctx.now_unix();
    let _ = ctx
        .db
        .acquire_online_lock(uid_boss, 9101, Some(boss_id), false, now);
    let _ = ctx
        .db
        .acquire_online_lock(uid_member, 9102, Some(member_id), false, now);
    let _ = ctx
        .db
        .acquire_online_lock(uid_extra, 9103, Some(extra_id), false, now);

    let claims_boss = make_claims(boss_id);
    let empty_q = HashMap::new();
    let p_fleet = params(&[("fleet_id", "1")]);

    // 1. Boundary: GET /characters/{id}/fleet uses -1 fallback when wing_id/squad_id are None
    let boss_fleet = dispatch_fleet_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/fleet",
        &params(&[("character_id", &boss_id.to_string())]),
        &empty_q,
        Some(&claims_boss),
        b"",
    )
    .unwrap();
    assert_eq!(boss_fleet.status_code, 200);
    assert_eq!(body(&boss_fleet)["wing_id"], -1);
    assert_eq!(body(&boss_fleet)["squad_id"], -1);

    let wc_fleet = dispatch_fleet_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/fleet",
        &params(&[("character_id", &member_id.to_string())]),
        &empty_q,
        Some(&claims_boss),
        b"",
    )
    .unwrap();
    assert_eq!(wc_fleet.status_code, 200);
    assert_eq!(body(&wc_fleet)["wing_id"], 10);
    assert_eq!(body(&wc_fleet)["squad_id"], -1);

    // 2. Boundary: GET /fleets/{id}/members omits "station_id" when member is in space (None)
    let members_res = dispatch_fleet_routes(
        &ctx,
        "GET",
        "/fleets/{fleet_id}/members",
        &p_fleet,
        &empty_q,
        Some(&claims_boss),
        b"",
    )
    .unwrap();
    assert_eq!(members_res.status_code, 200);
    let arr = body(&members_res).as_array().unwrap();
    let boss_entry = arr.iter().find(|m| m["character_id"] == boss_id).unwrap();
    let wc_entry = arr.iter().find(|m| m["character_id"] == member_id).unwrap();
    assert!(
        boss_entry.as_object().unwrap().get("station_id").is_none(),
        "in-space fleet member must omit 'station_id'"
    );
    assert_eq!(wc_entry["station_id"], 60_003_760);

    let _ = ctx.drain_notifications_for_char(boss_id);
    let _ = ctx.drain_notifications_for_char(member_id);
    let _ = ctx.drain_notifications_for_char(extra_id);

    // 3. PUT /fleets/{id}: OnFleetMotdChanged + OnFleetOptionsChanged + sync_fleet_options_to_db
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.voice_enabled.insert(1, true);
        ex.loot_logging.insert(1, true);
    }
    let put_fleet = dispatch_fleet_routes(
        &ctx,
        "PUT",
        "/fleets/{fleet_id}",
        &p_fleet,
        &empty_q,
        Some(&claims_boss),
        br#"{"motd":"Op Fleet MOTD","is_free_move":true}"#,
    )
    .unwrap();
    assert_eq!(put_fleet.status_code, 204);
    let m_notifs = ctx.drain_notifications_for_char(member_id);
    let motd_n = m_notifs
        .iter()
        .find(|n| n.event == "OnFleetMotdChanged")
        .expect("OnFleetMotdChanged must be emitted");
    assert_eq!(
        motd_n.args,
        vec![PyRep::String("Op Fleet MOTD".into()), PyRep::Bool(true)]
    );
    assert!(
        m_notifs.iter().any(|n| n.event == "OnFleetOptionsChanged"),
        "OnFleetOptionsChanged must be emitted when is_free_move toggles"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleet_and_social_ops.fleet_voice_enabled.get(&1), Some(&true));
        assert_eq!(snap.fleet_and_social_ops.fleet_loot_logging.get(&1), Some(&true));
    });

    // Sending identical is_free_move does NOT emit OnFleetOptionsChanged
    let put_same = dispatch_fleet_routes(
        &ctx,
        "PUT",
        "/fleets/{fleet_id}",
        &p_fleet,
        &empty_q,
        Some(&claims_boss),
        br#"{"is_free_move":true}"#,
    )
    .unwrap();
    assert_eq!(put_same.status_code, 204);
    assert!(
        !ctx.drain_notifications_for_char(member_id)
            .iter()
            .any(|n| n.event == "OnFleetOptionsChanged")
    );

    // 4. Wings & Squads notifications (Added, NameChanged, Deleted)
    let wing_create = dispatch_fleet_routes(
        &ctx,
        "POST",
        "/fleets/{fleet_id}/wings",
        &p_fleet,
        &empty_q,
        Some(&claims_boss),
        b"",
    )
    .unwrap();
    assert_eq!(wing_create.status_code, 201);
    let new_wing_id = body(&wing_create)["wing_id"].as_i64().unwrap();
    let w_add = ctx.drain_notifications_for_char(member_id);
    assert!(w_add
        .iter()
        .any(|n| n.event == "OnFleetWingAdded" && n.args == vec![PyRep::Integer(new_wing_id)]));

    let p_new_wing = params(&[("fleet_id", "1"), ("wing_id", &new_wing_id.to_string())]);
    let wing_ren = dispatch_fleet_routes(
        &ctx,
        "PUT",
        "/fleets/{fleet_id}/wings/{wing_id}",
        &p_new_wing,
        &empty_q,
        Some(&claims_boss),
        br#"{"name":"AlphaWing"}"#,
    )
    .unwrap();
    assert_eq!(wing_ren.status_code, 204);
    let w_ren = ctx.drain_notifications_for_char(member_id);
    assert!(w_ren.iter().any(|n| {
        n.event == "OnFleetWingNameChanged"
            && n.args == vec![PyRep::Integer(new_wing_id), PyRep::String("AlphaWing".into())]
    }));

    let sq_create = dispatch_fleet_routes(
        &ctx,
        "POST",
        "/fleets/{fleet_id}/wings/{wing_id}/squads",
        &p_new_wing,
        &empty_q,
        Some(&claims_boss),
        b"",
    )
    .unwrap();
    assert_eq!(sq_create.status_code, 201);
    let new_squad_id = body(&sq_create)["squad_id"].as_i64().unwrap();
    let s_add = ctx.drain_notifications_for_char(member_id);
    assert!(s_add.iter().any(|n| {
        n.event == "OnFleetSquadAdded"
            && n.args == vec![PyRep::Integer(new_wing_id), PyRep::Integer(new_squad_id)]
    }));

    let p_new_squad = params(&[("fleet_id", "1"), ("squad_id", &new_squad_id.to_string())]);
    let sq_ren = dispatch_fleet_routes(
        &ctx,
        "PUT",
        "/fleets/{fleet_id}/squads/{squad_id}",
        &p_new_squad,
        &empty_q,
        Some(&claims_boss),
        br#"{"name":"AlphaSqd"}"#,
    )
    .unwrap();
    assert_eq!(sq_ren.status_code, 204);
    let s_ren = ctx.drain_notifications_for_char(member_id);
    assert!(s_ren.iter().any(|n| {
        n.event == "OnFleetSquadNameChanged"
            && n.args == vec![PyRep::Integer(new_squad_id), PyRep::String("AlphaSqd".into())]
    }));

    // 5. Invite notification (OnFleetInvite)
    let _ = ctx.drain_notifications_for_char(extra_id);
    let invite_res = dispatch_fleet_routes(
        &ctx,
        "POST",
        "/fleets/{fleet_id}/members",
        &p_fleet,
        &empty_q,
        Some(&claims_boss),
        &serde_json::to_vec(&json!({
            "character_id": extra_id,
            "role": "squad_member",
            "wing_id": new_wing_id,
            "squad_id": new_squad_id
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(invite_res.status_code, 204);
    let inv_notifs = ctx.drain_notifications_for_char(extra_id);
    let inv_n = inv_notifs
        .iter()
        .find(|n| n.event == "OnFleetInvite")
        .expect("OnFleetInvite must be emitted to invitee");
    assert_eq!(
        inv_n.args,
        vec![
            PyRep::Integer(1),
            PyRep::Integer(boss_id),
            PyRep::Integer(new_wing_id),
            PyRep::Integer(new_squad_id),
            PyRep::Integer(4),
        ]
    );

    // 6. Move member notifications (OnFleetMemberChanged + OnFleetMove)
    let _ = ctx.drain_notifications_for_char(boss_id);
    let _ = ctx.drain_notifications_for_char(member_id);
    let p_member = params(&[("fleet_id", "1"), ("member_id", &member_id.to_string())]);
    let move_res = dispatch_fleet_routes(
        &ctx,
        "PUT",
        "/fleets/{fleet_id}/members/{member_id}",
        &p_member,
        &empty_q,
        Some(&claims_boss),
        &serde_json::to_vec(&json!({
            "role": "squad_member",
            "wing_id": new_wing_id,
            "squad_id": new_squad_id
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(move_res.status_code, 204);
    let boss_move_notifs = ctx.drain_notifications_for_char(boss_id);
    let target_move_notifs = ctx.drain_notifications_for_char(member_id);
    assert!(boss_move_notifs.iter().any(|n| n.event == "OnFleetMemberChanged"));
    assert!(target_move_notifs.iter().any(|n| n.event == "OnFleetMove"));

    // 7. Kick/remove member notifications (OnFleetLeave + OnSessionChanged)
    let kick_res = dispatch_fleet_routes(
        &ctx,
        "DELETE",
        "/fleets/{fleet_id}/members/{member_id}",
        &p_member,
        &empty_q,
        Some(&claims_boss),
        b"",
    )
    .unwrap();
    assert_eq!(kick_res.status_code, 204);
    let boss_kick_notifs = ctx.drain_notifications_for_char(boss_id);
    let target_kick_notifs = ctx.drain_notifications_for_char(member_id);
    assert!(boss_kick_notifs
        .iter()
        .any(|n| n.event == "OnFleetLeave" && n.args == vec![PyRep::Integer(member_id)]));
    assert!(target_kick_notifs
        .iter()
        .any(|n| n.event == "OnFleetLeave" && n.args == vec![PyRep::Integer(member_id)]));
    assert!(target_kick_notifs.iter().any(|n| n.event == "OnSessionChanged"));

    // 8. Delete squad & delete wing notifications (sent to remaining online boss)
    let _ = ctx.drain_notifications_for_char(boss_id);
    let sq_del = dispatch_fleet_routes(
        &ctx,
        "DELETE",
        "/fleets/{fleet_id}/squads/{squad_id}",
        &p_new_squad,
        &empty_q,
        Some(&claims_boss),
        b"",
    )
    .unwrap();
    assert_eq!(sq_del.status_code, 204);
    assert!(ctx
        .drain_notifications_for_char(boss_id)
        .iter()
        .any(|n| n.event == "OnFleetSquadDeleted" && n.args == vec![PyRep::Integer(new_squad_id)]));

    let wing_del = dispatch_fleet_routes(
        &ctx,
        "DELETE",
        "/fleets/{fleet_id}/wings/{wing_id}",
        &p_new_wing,
        &empty_q,
        Some(&claims_boss),
        b"",
    )
    .unwrap();
    assert_eq!(wing_del.status_code, 204);
    assert!(ctx
        .drain_notifications_for_char(boss_id)
        .iter()
        .any(|n| n.event == "OnFleetWingDeleted" && n.args == vec![PyRep::Integer(new_wing_id)]));
}

#[test]
fn fleet_routes_validation_permissions_and_ui_openwindow_cover_all_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_p = HashMap::new();
    let empty_q = HashMap::new();

    // Seed fleet 10 with wing 10, wing 20 (with wing_details), squad 100 (details), squad 200 (bare in f.squads),
    // and members exercising FC, WC, SC, SM, boss-without-details, member-without-details, and non-member.
    let boss_id = 90_000_010_i64;
    let fc_id = 90_000_011_i64;
    let wc_id = 90_000_012_i64;
    let sc_id = 90_000_013_i64;
    let sm_id = 90_000_014_i64;
    let bare_mem_id = 90_000_015_i64;
    let outsider_id = 90_000_016_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters[&char_id].clone();
        for &cid in &[boss_id, fc_id, wc_id, sc_id, sm_id, bare_mem_id, outsider_id] {
            let mut c = base_char.clone();
            c.character_id = cid;
            c.name = format!("Pilot {cid}");
            snap.characters.insert(cid, c);
        }
        let mut fleet = build_test_fleet();
        fleet.fleet_id = 10;
        fleet.boss_id = boss_id;
        fleet.members = vec![fc_id, wc_id, sc_id, sm_id, bare_mem_id];
        fleet.wings = vec![10, 20];
        fleet.squads = vec![100, 200];
        fleet.wing_details.insert(
            20,
            FleetWingRecord {
                wing_id: 20,
                name: "Wing 20".to_string(),
                commander_id: None,
                squad_ids: vec![201],
            },
        );
        fleet.squad_details.remove(&200);
        fleet.member_details.clear();
        fleet.member_details.insert(
            fc_id,
            FleetMemberRecord {
                character_id: fc_id,
                role: FleetRole::FleetCommander,
                wing_id: None,
                squad_id: None,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 100,
                takes_fleet_warp: true,
            },
        );
        fleet.member_details.insert(
            wc_id,
            FleetMemberRecord {
                character_id: wc_id,
                role: FleetRole::WingCommander,
                wing_id: Some(10),
                squad_id: None,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "WC".to_string(),
                join_time_unix: 100,
                takes_fleet_warp: true,
            },
        );
        fleet.member_details.insert(
            sc_id,
            FleetMemberRecord {
                character_id: sc_id,
                role: FleetRole::SquadCommander,
                wing_id: Some(10),
                squad_id: Some(100),
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "SC".to_string(),
                join_time_unix: 100,
                takes_fleet_warp: true,
            },
        );
        fleet.member_details.insert(
            sm_id,
            FleetMemberRecord {
                character_id: sm_id,
                role: FleetRole::SquadMember,
                wing_id: Some(10),
                squad_id: Some(100),
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "SM".to_string(),
                join_time_unix: 100,
                takes_fleet_warp: true,
            },
        );
        // Orphan member_details not in fleet.members and missing from snap.characters -> exercises line 493 & 499
        fleet.member_details.insert(
            90_999_888,
            FleetMemberRecord {
                character_id: 90_999_888,
                role: FleetRole::SquadMember,
                wing_id: Some(10),
                squad_id: Some(100),
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 0,
                takes_fleet_warp: false,
            },
        );
        snap.fleets.insert(10, fleet);
        snap.contracts.insert(
            500_001,
            eve_db::ContractRecord {
                contract_id: 500_001,
                issuer_id: char_id,
                acceptor_id: None,
                contract_type: "ItemExchange".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: eve_db::IskCents(100),
                reward: eve_db::IskCents(0),
                collateral: eve_db::IskCents(0),
                item_ids: Vec::new(),
                expiration_unix: 1_800_000_000,
                issuer_corp_id: corp_id,
                assignee_id: char_id,
                availability: "Personal".to_string(),
                title: "UI Contract".to_string(),
                for_corp: false,
                ..Default::default()
            },
        );
    });

    let c_boss = make_claims(boss_id);
    let c_fc = make_claims(fc_id);
    let _c_wc = make_claims(wc_id);
    let _c_sc = make_claims(sc_id);
    let c_sm = make_claims(sm_id);
    let c_out = make_claims(outsider_id);
    let p_f10 = params(&[("fleet_id", "10")]);
    let p_missing_f = params(&[("fleet_id", "99999")]);

    // Helper coverage: member_effective_position, can_act_on_member, validate_wing_and_squad
    ctx.db.with_snapshot(|snap| {
        let f = &snap.fleets[&10];
        assert_eq!(member_effective_position(f, boss_id).0, FleetRole::FleetCommander);
        assert_eq!(member_effective_position(f, bare_mem_id).0, FleetRole::SquadMember);
        assert!(can_act_on_member(f, boss_id, sm_id, None));
        assert!(!can_act_on_member(f, outsider_id, sm_id, None));
        assert!(can_act_on_member(
            f,
            fc_id,
            sm_id,
            Some((FleetRole::SquadCommander, Some(10), Some(100)))
        ));
        assert!(!can_act_on_member(
            f,
            fc_id,
            boss_id,
            Some((FleetRole::SquadMember, Some(10), Some(100)))
        ));
        assert!(can_act_on_member(
            f,
            wc_id,
            sm_id,
            Some((FleetRole::SquadCommander, Some(10), Some(100)))
        ));
        assert!(!can_act_on_member(
            f,
            wc_id,
            sm_id,
            Some((FleetRole::SquadCommander, Some(20), Some(100)))
        ));
        assert!(can_act_on_member(
            f,
            sc_id,
            sm_id,
            Some((FleetRole::SquadMember, Some(10), Some(100)))
        ));
        assert!(!can_act_on_member(f, sm_id, bare_mem_id, None));
        assert!(!can_act_on_member(
            f,
            sm_id,
            bare_mem_id,
            Some((FleetRole::SquadMember, Some(10), Some(100)))
        ));
        // squad 200 not in wing 20.squad_ids([201]) -> 400
        assert_eq!(
            validate_wing_and_squad(f, 10, FleetRole::SquadMember, Some(20), Some(200), true)
                .unwrap_err()
                .status_code,
            400
        );
    });

    // /characters/{character_id}/fleet edge cases
    assert_eq!(
        dispatch_fleet_routes(&ctx, "GET", "/characters/{character_id}/fleet", &empty_p, &empty_q, None, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "GET",
            "/characters/{character_id}/fleet",
            &params(&[("character_id", "90999777")]),
            &empty_q,
            None,
            b""
        )
        .unwrap()
        .status_code,
        404
    );
    let bare_fleet = dispatch_fleet_routes(
        &ctx,
        "GET",
        "/characters/{character_id}/fleet",
        &params(&[("character_id", &bare_mem_id.to_string())]),
        &empty_q,
        None,
        b"",
    )
    .unwrap();
    assert_eq!(bare_fleet.status_code, 200);
    assert_eq!(body(&bare_fleet)["role"], "squad_member");

    // GET/PUT /fleets/{fleet_id} and GET /fleets/{fleet_id}/members error + 200 branches
    for (method, path) in [
        ("GET", "/fleets/{fleet_id}"),
        ("PUT", "/fleets/{fleet_id}"),
        ("GET", "/fleets/{fleet_id}/members"),
        ("POST", "/fleets/{fleet_id}/members"),
        ("GET", "/fleets/{fleet_id}/wings"),
        ("POST", "/fleets/{fleet_id}/wings"),
    ] {
        assert_eq!(
            dispatch_fleet_routes(&ctx, method, path, &empty_p, &empty_q, None, b"{}")
                .unwrap()
                .status_code,
            400,
            "bad fleet_id on {method} {path}"
        );
    }
    assert_eq!(
        dispatch_fleet_routes(&ctx, "GET", "/fleets/{fleet_id}", &p_f10, &empty_q, Some(&c_out), b"")
            .unwrap()
            .status_code,
        403
    );
    for bad_put in [
        b"123".as_slice(),
        b"{\"motd\":123}",
        b"{}",
    ] {
        assert_eq!(
            dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}", &p_f10, &empty_q, Some(&c_boss), bad_put)
                .unwrap()
                .status_code,
            400
        );
    }
    assert_eq!(
        dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}", &p_missing_f, &empty_q, Some(&c_boss), b"{\"motd\":\"x\"}")
            .unwrap()
            .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}", &p_f10, &empty_q, Some(&c_sm), b"{\"motd\":\"x\"}")
            .unwrap()
            .status_code,
        403
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}",
            &p_f10,
            &empty_q,
            Some(&c_fc),
            b"{\"motd\":\"New MOTD\",\"is_free_move\":false}",
        )
        .unwrap()
        .status_code,
        204
    );

    assert_eq!(
        dispatch_fleet_routes(&ctx, "GET", "/fleets/{fleet_id}/members", &p_missing_f, &empty_q, None, b"")
            .unwrap()
            .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "GET", "/fleets/{fleet_id}/members", &p_f10, &empty_q, Some(&c_out), b"")
            .unwrap()
            .status_code,
        403
    );
    let mems = dispatch_fleet_routes(&ctx, "GET", "/fleets/{fleet_id}/members", &p_f10, &empty_q, Some(&c_boss), b"").unwrap();
    assert_eq!(mems.status_code, 200);
    assert!(body(&mems).as_array().unwrap().iter().any(|m| m["character_id"] == boss_id));
    assert!(body(&mems).as_array().unwrap().iter().any(|m| m["character_id"] == bare_mem_id));

    // POST /fleets/{fleet_id}/members error cases
    for bad_inv in [
        b"not-json".as_slice(),
        b"{}",
        b"{\"character_id\":90000016}",
        b"{\"character_id\":90000016,\"role\":\"invalid\"}",
        b"{\"character_id\":90000016,\"role\":\"squad_member\",\"wing_id\":0}",
        b"{\"character_id\":90000016,\"role\":\"squad_member\",\"squad_id\":0}",
    ] {
        assert_eq!(
            dispatch_fleet_routes(&ctx, "POST", "/fleets/{fleet_id}/members", &p_f10, &empty_q, Some(&c_boss), bad_inv)
                .unwrap()
                .status_code,
            400
        );
    }
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/fleets/{fleet_id}/members",
            &p_f10,
            &empty_q,
            Some(&c_sm),
            b"{\"character_id\":90000016,\"role\":\"squad_member\",\"wing_id\":10,\"squad_id\":100}",
        )
        .unwrap()
        .status_code,
        403
    );

    // PUT & DELETE /fleets/{fleet_id}/members/{member_id}
    let p_m = params(&[("fleet_id", "10"), ("member_id", &sm_id.to_string())]);
    assert_eq!(
        dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}/members/{member_id}", &empty_p, &empty_q, None, b"{}")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}/members/{member_id}", &p_f10, &empty_q, None, b"{}")
            .unwrap()
            .status_code,
        400
    );
    for bad_mv in [
        b"bad".as_slice(),
        b"{}",
        b"{\"role\":\"bad\"}",
        b"{\"role\":\"squad_member\",\"wing_id\":-1}",
        b"{\"role\":\"squad_member\",\"squad_id\":0}",
    ] {
        assert_eq!(
            dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}/members/{member_id}", &p_m, &empty_q, Some(&c_boss), bad_mv)
                .unwrap()
                .status_code,
            400
        );
    }
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/members/{member_id}",
            &params(&[("fleet_id", "999"), ("member_id", &sm_id.to_string())]),
            &empty_q,
            Some(&c_boss),
            b"{\"role\":\"fleet_commander\"}",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/members/{member_id}",
            &params(&[("fleet_id", "10"), ("member_id", &outsider_id.to_string())]),
            &empty_q,
            Some(&c_boss),
            b"{\"role\":\"fleet_commander\"}",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/members/{member_id}",
            &p_m,
            &empty_q,
            Some(&c_out),
            b"{\"role\":\"squad_member\",\"wing_id\":10,\"squad_id\":100}",
        )
        .unwrap()
        .status_code,
        403
    );

    assert_eq!(
        dispatch_fleet_routes(&ctx, "DELETE", "/fleets/{fleet_id}/members/{member_id}", &empty_p, &empty_q, None, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "DELETE", "/fleets/{fleet_id}/members/{member_id}", &p_f10, &empty_q, None, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "DELETE",
            "/fleets/{fleet_id}/members/{member_id}",
            &params(&[("fleet_id", "999"), ("member_id", &sm_id.to_string())]),
            &empty_q,
            None,
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "DELETE",
            "/fleets/{fleet_id}/members/{member_id}",
            &params(&[("fleet_id", "10"), ("member_id", &outsider_id.to_string())]),
            &empty_q,
            None,
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "DELETE", "/fleets/{fleet_id}/members/{member_id}", &p_m, &empty_q, Some(&c_out), b"")
            .unwrap()
            .status_code,
        403
    );

    // Wings & Squads CRUD error & edge branches
    assert_eq!(
        dispatch_fleet_routes(&ctx, "GET", "/fleets/{fleet_id}/wings", &p_missing_f, &empty_q, None, b"")
            .unwrap()
            .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "GET", "/fleets/{fleet_id}/wings", &p_f10, &empty_q, Some(&c_out), b"")
            .unwrap()
            .status_code,
        403
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "GET", "/fleets/{fleet_id}/wings", &p_f10, &empty_q, Some(&c_boss), b"")
            .unwrap()
            .status_code,
        200
    );

    assert_eq!(
        dispatch_fleet_routes(&ctx, "POST", "/fleets/{fleet_id}/wings", &p_missing_f, &empty_q, None, b"")
            .unwrap()
            .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "POST", "/fleets/{fleet_id}/wings", &p_f10, &empty_q, Some(&c_sm), b"")
            .unwrap()
            .status_code,
        403
    );
    let created_w = dispatch_fleet_routes(&ctx, "POST", "/fleets/{fleet_id}/wings", &p_f10, &empty_q, Some(&c_boss), b"").unwrap();
    assert_eq!(created_w.status_code, 201);
    let new_wid = body(&created_w)["wing_id"].as_i64().unwrap();

    let p_w = params(&[("fleet_id", "10"), ("wing_id", &new_wid.to_string())]);
    assert_eq!(
        dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}/wings/{wing_id}", &empty_p, &empty_q, None, b"{}")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}/wings/{wing_id}", &p_f10, &empty_q, None, b"{}")
            .unwrap()
            .status_code,
        400
    );
    for bad_nm in [b"bad".as_slice(), b"{}", b"{\"name\":\"\"}", b"{\"name\":\"too_long_name_123\"}"] {
        assert_eq!(
            dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}/wings/{wing_id}", &p_w, &empty_q, Some(&c_boss), bad_nm)
                .unwrap()
                .status_code,
            400
        );
    }
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/wings/{wing_id}",
            &params(&[("fleet_id", "999"), ("wing_id", "10")]),
            &empty_q,
            Some(&c_boss),
            b"{\"name\":\"Alpha\"}",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/wings/{wing_id}",
            &params(&[("fleet_id", "10"), ("wing_id", "99999")]),
            &empty_q,
            Some(&c_boss),
            b"{\"name\":\"Alpha\"}",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/wings/{wing_id}",
            &p_w,
            &empty_q,
            Some(&c_sm),
            b"{\"name\":\"Alpha\"}",
        )
        .unwrap()
        .status_code,
        403
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/wings/{wing_id}",
            &p_w,
            &empty_q,
            Some(&c_boss),
            b"{\"name\":\"Alpha\"}",
        )
        .unwrap()
        .status_code,
        204
    );

    // Squads create/rename/delete + Wing delete
    assert_eq!(
        dispatch_fleet_routes(&ctx, "POST", "/fleets/{fleet_id}/wings/{wing_id}/squads", &empty_p, &empty_q, None, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "POST", "/fleets/{fleet_id}/wings/{wing_id}/squads", &p_f10, &empty_q, None, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/fleets/{fleet_id}/wings/{wing_id}/squads",
            &params(&[("fleet_id", "999"), ("wing_id", "10")]),
            &empty_q,
            Some(&c_boss),
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/fleets/{fleet_id}/wings/{wing_id}/squads",
            &params(&[("fleet_id", "10"), ("wing_id", "99999")]),
            &empty_q,
            Some(&c_boss),
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "POST", "/fleets/{fleet_id}/wings/{wing_id}/squads", &p_w, &empty_q, Some(&c_sm), b"")
            .unwrap()
            .status_code,
        403
    );
    let created_sq = dispatch_fleet_routes(&ctx, "POST", "/fleets/{fleet_id}/wings/{wing_id}/squads", &p_w, &empty_q, Some(&c_boss), b"").unwrap();
    assert_eq!(created_sq.status_code, 201);
    let new_sid = body(&created_sq)["squad_id"].as_i64().unwrap();

    let p_sq = params(&[("fleet_id", "10"), ("squad_id", &new_sid.to_string())]);
    let p_bare_sq = params(&[("fleet_id", "10"), ("squad_id", "200")]);
    assert_eq!(
        dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}/squads/{squad_id}", &empty_p, &empty_q, None, b"{}")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}/squads/{squad_id}", &p_f10, &empty_q, None, b"{}")
            .unwrap()
            .status_code,
        400
    );
    for bad_nm in [b"bad".as_slice(), b"{}", b"{\"name\":\"\"}"] {
        assert_eq!(
            dispatch_fleet_routes(&ctx, "PUT", "/fleets/{fleet_id}/squads/{squad_id}", &p_sq, &empty_q, Some(&c_boss), bad_nm)
                .unwrap()
                .status_code,
            400
        );
    }
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/squads/{squad_id}",
            &params(&[("fleet_id", "999"), ("squad_id", "100")]),
            &empty_q,
            Some(&c_boss),
            b"{\"name\":\"S1\"}",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/squads/{squad_id}",
            &params(&[("fleet_id", "10"), ("squad_id", "99999")]),
            &empty_q,
            Some(&c_boss),
            b"{\"name\":\"S1\"}",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/squads/{squad_id}",
            &p_sq,
            &empty_q,
            Some(&c_sm),
            b"{\"name\":\"S1\"}",
        )
        .unwrap()
        .status_code,
        403
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "PUT",
            "/fleets/{fleet_id}/squads/{squad_id}",
            &p_bare_sq,
            &empty_q,
            Some(&c_boss),
            b"{\"name\":\"BareSq\"}",
        )
        .unwrap()
        .status_code,
        204
    );

    assert_eq!(
        dispatch_fleet_routes(&ctx, "DELETE", "/fleets/{fleet_id}/squads/{squad_id}", &empty_p, &empty_q, None, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "DELETE", "/fleets/{fleet_id}/squads/{squad_id}", &p_f10, &empty_q, None, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "DELETE",
            "/fleets/{fleet_id}/squads/{squad_id}",
            &params(&[("fleet_id", "999"), ("squad_id", "100")]),
            &empty_q,
            Some(&c_boss),
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "DELETE",
            "/fleets/{fleet_id}/squads/{squad_id}",
            &p_sq,
            &empty_q,
            Some(&c_sm),
            b"",
        )
        .unwrap()
        .status_code,
        403
    );

    assert_eq!(
        dispatch_fleet_routes(&ctx, "DELETE", "/fleets/{fleet_id}/wings/{wing_id}", &empty_p, &empty_q, None, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "DELETE", "/fleets/{fleet_id}/wings/{wing_id}", &p_f10, &empty_q, None, b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "DELETE",
            "/fleets/{fleet_id}/wings/{wing_id}",
            &params(&[("fleet_id", "999"), ("wing_id", "10")]),
            &empty_q,
            Some(&c_boss),
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "DELETE", "/fleets/{fleet_id}/wings/{wing_id}", &p_w, &empty_q, Some(&c_sm), b"")
            .unwrap()
            .status_code,
        403
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "DELETE",
            "/fleets/{fleet_id}/wings/{wing_id}",
            &params(&[("fleet_id", "10"), ("wing_id", "99999")]),
            &empty_q,
            Some(&c_boss),
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(&ctx, "DELETE", "/fleets/{fleet_id}/wings/{wing_id}", &p_w, &empty_q, Some(&c_boss), b"")
            .unwrap()
            .status_code,
        204
    );

    // /ui/openwindow/*: first offline (no active session -> 400), then online
    let c_char = make_claims(char_id);
    for ui_path in [
        "/ui/openwindow/contract",
        "/ui/openwindow/information",
        "/ui/openwindow/marketdetails",
        "/ui/openwindow/newmail",
    ] {
        assert_eq!(
            dispatch_fleet_routes(&ctx, "POST", ui_path, &empty_p, &empty_q, Some(&c_char), b"")
                .unwrap()
                .status_code,
            400
        );
    }

    let uid = ctx.db.with_snapshot(|snap| snap.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 77_001, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    // /ui/openwindow/contract
    assert_eq!(
        dispatch_fleet_routes(&ctx, "POST", "/ui/openwindow/contract", &empty_p, &empty_q, Some(&c_char), b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/contract",
            &empty_p,
            &params(&[("contract_id", "bad")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/contract",
            &empty_p,
            &params(&[("contract_id", "999999")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/contract",
            &empty_p,
            &params(&[("contract_id", "500001")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        204
    );

    // /ui/openwindow/information
    assert_eq!(
        dispatch_fleet_routes(&ctx, "POST", "/ui/openwindow/information", &empty_p, &empty_q, Some(&c_char), b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/information",
            &empty_p,
            &params(&[("target_id", "0")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/information",
            &empty_p,
            &params(&[("target_id", "98999888")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/information",
            &empty_p,
            &params(&[("target_id", &char_id.to_string())]),
            Some(&c_char),
            b"",
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
            &empty_p,
            &params(&[("target_id", "30000142")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        501
    );

    // /ui/openwindow/marketdetails
    assert_eq!(
        dispatch_fleet_routes(&ctx, "POST", "/ui/openwindow/marketdetails", &empty_p, &empty_q, Some(&c_char), b"")
            .unwrap()
            .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/marketdetails",
            &empty_p,
            &params(&[("type_id", "0")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/marketdetails",
            &empty_p,
            &params(&[("type_id", "999999999999")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/marketdetails",
            &empty_p,
            &params(&[("type_id", "99999999")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/marketdetails",
            &empty_p,
            &params(&[("type_id", "34")]),
            Some(&c_char),
            b"",
        )
        .unwrap()
        .status_code,
        204
    );

    // /ui/openwindow/newmail
    for bad_nm_body in [
        b"not-json".as_slice(),
        b"{}",
        b"{\"subject\":\"\",\"body\":\"b\",\"recipients\":[1]}",
        b"{\"subject\":\"s\",\"recipients\":[1]}",
        b"{\"subject\":\"s\",\"body\":\"\",\"recipients\":[1]}",
        b"{\"subject\":\"s\",\"body\":\"b\"}",
        b"{\"subject\":\"s\",\"body\":\"b\",\"recipients\":[]}",
        b"{\"subject\":\"s\",\"body\":\"b\",\"recipients\":[0]}",
    ] {
        assert_eq!(
            dispatch_fleet_routes(&ctx, "POST", "/ui/openwindow/newmail", &empty_p, &empty_q, Some(&c_char), bad_nm_body)
                .unwrap()
                .status_code,
            400
        );
    }
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/newmail",
            &empty_p,
            &empty_q,
            Some(&c_char),
            b"{\"subject\":\"hi\",\"body\":\"hello\",\"recipients\":[90000010],\"to_corp_or_alliance_id\":98000001,\"to_mailing_list_id\":1}",
        )
        .unwrap()
        .status_code,
        422
    );
    assert_eq!(
        dispatch_fleet_routes(
            &ctx,
            "POST",
            "/ui/openwindow/newmail",
            &empty_p,
            &empty_q,
            Some(&c_char),
            b"{\"subject\":\"hi\",\"body\":\"hello\",\"recipients\":[90000010]}",
        )
        .unwrap()
        .status_code,
        204
    );
}
