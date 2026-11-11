//! Corporation Creation, Public Info, Applications, Invitations, Roles & Titles parity suite.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{corp_roles, CharacterRecord, CorporationRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

const JITA_STATION: i64 = 60_003_760;
const NPC_CORP_CALDARI: i64 = 1_000_009;

fn make_session(session_id: u64, user_id: i32, char_id: i64, corp_id: i64, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30_000_142);
    s.role = eve_machonet::service_roles::ROLE_PLAYER;
    s.corp_role = corp_role;
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn setup_pilot(ctx: &ServerContext, char_id: i64, name: &str, corp_id: i64, balance_isk: i64, corp_mgmt_level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap
            .characters
            .get(&char_id)
            .or_else(|| snap.characters.values().min_by_key(|c| c.character_id))
            .cloned()
            .unwrap();
        let ch = CharacterRecord {
            character_id: char_id,
            user_id: 1,
            name: name.into(),
            corporation_id: corp_id,
            balance: IskCents::from_isk(balance_isk),
            station_id: Some(JITA_STATION),
            solar_system_id: 30_000_142,
            ..base_char
        };
        snap.characters.insert(char_id, ch);
        if corp_mgmt_level > 0 {
            snap.skills.entry(char_id).or_default().insert(
                3363,
                TrainedSkillRecord { type_id: 3363, level: corp_mgmt_level, skill_points: 256_000.0 },
            );
        }
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.members.insert(char_id);
        }
    });
}

fn setup_test_corp(ctx: &ServerContext, corp_id: i64, name: &str, ticker: &str, ceo_id: i64, members: &[i64]) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = name.into();
        corp.ticker = ticker.into();
        corp.ceo_id = ceo_id;
        corp.members = members.iter().copied().collect();
        for &m in members {
            corp.member_roles.insert(m, if m == ceo_id { u64::MAX } else { 0 });
        }
        snap.corporations.insert(corp_id, corp);
    });
}

fn member_update_args(char_id: i64, title: &str, roles: u64, hq: u64, base: u64, tmask: u64) -> Vec<PyRep> {
    vec![
        PyRep::Integer(char_id), PyRep::String(title.into()), PyRep::Integer(1), PyRep::Integer(0),
        PyRep::Integer(roles as i64), PyRep::Integer(0), PyRep::Integer(hq as i64), PyRep::Integer(0),
        PyRep::Integer(base as i64), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0),
        PyRep::Integer(JITA_STATION), PyRep::Integer(tmask as i64),
    ]
}

// parity: feat:corporation.create-corp, feat:corporation.corp-public-info, feat:corporation.corp-edit-details
#[tokio::test]
async fn test_corp_creation_public_info_and_edit_details() {
    eve_testkit::attest(&[
        "feat:corporation.create-corp",
        "feat:corporation.corp-public-info",
        "feat:corporation.corp-edit-details",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let founder_id = 91_000_001i64;
    let outsider_id = 91_000_002i64;

    setup_pilot(&ctx, founder_id, "Founder Pilot", NPC_CORP_CALDARI, 10_000_000, 0);
    setup_pilot(&ctx, outsider_id, "Outsider Pilot", NPC_CORP_CALDARI, 10_000_000, 5);

    let mut sess_founder = make_session(1001, 101, founder_id, NPC_CORP_CALDARI, 0);
    let mut sess_outsider = make_session(1002, 102, outsider_id, NPC_CORP_CALDARI, 0);

    let corp_args = [
        PyRep::String("Apex Industrial Logistics".into()),
        PyRep::String("AILOG".into()),
        PyRep::Real(0.05),
    ];

    // Fail: Missing Corporation Management skill
    let err_skill = rpc(&ctx, &mut sess_founder, "corpRegistry", "AddCorporation", &corp_args)
        .await
        .expect_err("creating corp without Corp Mgmt skill must fail");
    assert_eq!(err_skill.code, "MissingCorporationManagementSkill");

    // Give founder skill, but empty balance
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(founder_id).or_default().insert(
            3363,
            TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 },
        );
        snap.characters.get_mut(&founder_id).unwrap().balance = IskCents::from_isk(100);
    });

    let err_funds = rpc(&ctx, &mut sess_founder, "corpRegistry", "AddCorporation", &corp_args)
        .await
        .expect_err("creating corp without funds must fail");
    assert_eq!(err_funds.code, "InsufficientFunds");

    // Restore balance and succeed
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&founder_id).unwrap().balance = IskCents::from_isk(5_000_000);
    });

    let new_corp_rep = rpc(&ctx, &mut sess_founder, "corpRegistry", "AddCorporation", &corp_args)
        .await
        .expect("AddCorporation must succeed");
    let new_corp_id = match new_corp_rep {
        PyRep::KeyVal(kv) => kv.get("corporationID").and_then(|v| v.as_i64()).unwrap(),
        PyRep::Integer(id) => id,
        other => panic!("expected KeyVal or Integer, got {other:?}"),
    };
    assert!(new_corp_id >= 98_000_000);

    // Verify creator is CEO, fee charged (1,599,800 ISK), creator moved to new corp
    let (rem_bal, is_ceo, char_corp_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&founder_id).unwrap();
        let corp = snap.corporations.get(&new_corp_id).unwrap();
        (ch.balance.0, corp.ceo_id == founder_id, ch.corporation_id)
    });
    assert_eq!(rem_bal, IskCents::from_isk(5_000_000 - 1_599_800).0);
    assert!(is_ceo);
    assert_eq!(char_corp_id, new_corp_id);

    // Fail: Name or ticker already taken
    let err_dup = rpc(&ctx, &mut sess_outsider, "corpRegistry", "AddCorporation", &corp_args)
        .await
        .expect_err("duplicate name/ticker must fail");
    assert_eq!(err_dup.code, "CorpNameOrTickerTaken");

    // 2. feat:corporation.corp-public-info
    let pub_info = rpc(&ctx, &mut sess_outsider, "corpmgr", "GetPublicInfo", &[PyRep::Integer(new_corp_id)])
        .await
        .expect("corpmgr.GetPublicInfo");
    let PyRep::KeyVal(info_kv) = pub_info else { panic!("expected KeyVal") };
    assert_eq!(info_kv["corporationID"], PyRep::Integer(new_corp_id));
    assert_eq!(info_kv["corporationName"], PyRep::String("Apex Industrial Logistics".into()));
    assert_eq!(info_kv["tickerName"], PyRep::String("AILOG".into()));
    assert_eq!(info_kv["ceoID"], PyRep::Integer(founder_id));
    assert_eq!(info_kv["memberCount"], PyRep::Integer(1));
    assert_eq!(info_kv["taxRate"], PyRep::Real(0.05));
    assert_eq!(info_kv["stationID"], PyRep::Integer(JITA_STATION));

    // Update session
    sess_founder.corporation_id = Some(new_corp_id);
    sess_founder.corp_role = corp_roles::ROLE_DIRECTOR;

    // 3. feat:corporation.corp-edit-details
    let edit_res = rpc(
        &ctx,
        &mut sess_founder,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Premier industrial network".into()),
            PyRep::String("https://apex.space".into()),
            PyRep::Real(0.08),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("UpdateCorporation");
    let PyRep::KeyVal(edit_kv) = edit_res else { panic!("expected KeyVal") };
    assert_eq!(edit_kv["description"], PyRep::String("Premier industrial network".into()));
    assert_eq!(edit_kv["url"], PyRep::String("https://apex.space".into()));
    assert_eq!(edit_kv["taxRate"], PyRep::Real(0.08));

    let logo_res = rpc(
        &ctx,
        &mut sess_founder,
        "corpRegistry",
        "UpdateLogo",
        &[PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(30), PyRep::Integer(5), PyRep::Integer(1), PyRep::Integer(2)],
    )
    .await
    .expect("UpdateLogo");
    let PyRep::KeyVal(logo_kv) = logo_res else { panic!("expected KeyVal from UpdateLogo") };
    assert_eq!(logo_kv["corporationID"], PyRep::Integer(new_corp_id));
    assert_eq!(logo_kv["shape1"], PyRep::Integer(10));

    // Member without Director role denied UpdateCorporation
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&new_corp_id).unwrap().members.insert(outsider_id);
    });
    sess_outsider.corporation_id = Some(new_corp_id);
    sess_outsider.corp_role = 0;

    let err_perm = rpc(
        &ctx,
        &mut sess_outsider,
        "corpRegistry",
        "UpdateCorporation",
        &[PyRep::String("Unauthorized".into()), PyRep::String("https://bad.com".into()), PyRep::Real(0.0)],
    )
    .await
    .expect_err("regular member denied UpdateCorporation");
    assert_eq!(err_perm.code, "CorpRoleDenied");
}

// parity: feat:corporation.member-limit, feat:corporation.applications
#[tokio::test]
async fn test_corp_member_limit_and_applications() {
    eve_testkit::attest(&[
        "feat:corporation.member-limit",
        "feat:corporation.applications",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let ceo_id = 91_000_101i64;
    let applicant_id = 91_000_102i64;
    let corp_id = 98_000_101i64;

    setup_pilot(&ctx, ceo_id, "Corp CEO", corp_id, 10_000_000, 1);
    setup_pilot(&ctx, applicant_id, "Applicant Pilot", NPC_CORP_CALDARI, 1_000_000, 0);
    setup_test_corp(&ctx, corp_id, "Frontier Industries", "FROIN", ceo_id, &[ceo_id]);

    let mut sess_ceo = make_session(2001, 201, ceo_id, corp_id, u64::MAX);
    let mut sess_app = make_session(2002, 202, applicant_id, NPC_CORP_CALDARI, 0);

    let parse_app_id = |rep: PyRep| match rep {
        PyRep::KeyVal(kv) => kv.get("applicationID").and_then(|v| v.as_i64()).unwrap(),
        PyRep::Integer(id) => id,
        other => panic!("expected KeyVal or Integer, got {other:?}"),
    };

    // 1. Submit application with message
    let app1_rep = rpc(
        &ctx,
        &mut sess_app,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("Looking to mine and build".into())],
    )
    .await
    .expect("InsertApplication");
    let app1_id = parse_app_id(app1_rep);

    let apps_rep = rpc(&ctx, &mut sess_ceo, "corpRegistry", "GetApplications", &[])
        .await
        .expect("GetApplications");
    let PyRep::Rowset { rows, .. } = apps_rep else { panic!("expected Rowset") };
    assert!(rows.iter().any(|r| r[0] == PyRep::Integer(app1_id)));

    // 2. Applicant withdraws application
    rpc(
        &ctx,
        &mut sess_app,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app1_id), PyRep::String("Withdrawn".into())],
    )
    .await
    .expect("Withdraw");
    let status = ctx.db.with_snapshot(|s| s.corporations.get(&corp_id).unwrap().applications.get(&app1_id).unwrap().status.clone());
    assert_eq!(status, "Withdrawn");

    // 3. Re-apply and reject
    let app2_rep = rpc(
        &ctx,
        &mut sess_app,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("Second attempt".into())],
    )
    .await
    .expect("InsertApplication 2");
    let app2_id = parse_app_id(app2_rep);

    rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app2_id), PyRep::String("Rejected".into())],
    )
    .await
    .expect("Reject");

    // 4. Re-apply and test Member Limit enforcement
    let app3_rep = rpc(
        &ctx,
        &mut sess_app,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("Third attempt".into())],
    )
    .await
    .expect("InsertApplication 3");
    let app3_id = parse_app_id(app3_rep);

    // Cap member limit to 1
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance.entry(corp_id).or_default().member_limit = Some(1);
    });

    let err_full = rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app3_id), PyRep::String("Accepted".into())],
    )
    .await
    .expect_err("corp full must fail");
    assert_eq!(err_full.code, "CorporationFull");

    // Lift member limit and accept
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance.entry(corp_id).or_default().member_limit = Some(20);
    });

    rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app3_id), PyRep::String("Accepted".into())],
    )
    .await
    .expect("Accept after lifting limit");

    let (is_member, char_corp) = ctx.db.with_snapshot(|snap| {
        let corp = snap.corporations.get(&corp_id).unwrap();
        let ch = snap.characters.get(&applicant_id).unwrap();
        (corp.members.contains(&applicant_id), ch.corporation_id)
    });
    assert!(is_member);
    assert_eq!(char_corp, corp_id);
}

// parity: feat:corporation.invitations, feat:corporation.leave-corp
#[tokio::test]
async fn test_corp_invitations_and_leave_rules() {
    eve_testkit::attest(&[
        "feat:corporation.invitations",
        "feat:corporation.leave-corp",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let ceo_id = 91_000_201i64;
    let invitee_id = 91_000_202i64;
    let corp_id = 98_000_201i64;

    setup_pilot(&ctx, ceo_id, "Fleet CEO", corp_id, 10_000_000, 5);
    setup_pilot(&ctx, invitee_id, "Invited Pilot", NPC_CORP_CALDARI, 1_000_000, 0);
    setup_test_corp(&ctx, corp_id, "Vanguard Armada", "VNGAR", ceo_id, &[ceo_id]);

    let mut sess_ceo = make_session(3001, 301, ceo_id, corp_id, u64::MAX);
    let mut sess_invitee = make_session(3002, 302, invitee_id, NPC_CORP_CALDARI, 0);

    // 1. Invite character
    let invite_rep = rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "InsertInvitation",
        &[PyRep::Integer(invitee_id), PyRep::String("Join our fleet".into())],
    )
    .await
    .expect("InsertInvitation");
    let invite_id = match invite_rep {
        PyRep::KeyVal(kv) => kv.get("applicationID").and_then(|v| v.as_i64()).unwrap(),
        PyRep::Integer(id) => id,
        other => panic!("expected KeyVal or Integer, got {other:?}"),
    };

    // Invitee accepts invite and joins corp
    rpc(
        &ctx,
        &mut sess_invitee,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(invite_id), PyRep::String("Accepted".into())],
    )
    .await
    .expect("Accept invitation");
    sess_invitee.corporation_id = Some(corp_id);

    let joined = ctx.db.with_snapshot(|s| s.corporations.get(&corp_id).unwrap().members.contains(&invitee_id));
    assert!(joined);

    // Clear 10s corp change session timer
    ctx.advance_secs(12);

    // 2. Member leaves to NPC corp
    let can_leave_rep = rpc(&ctx, &mut sess_invitee, "corpRegistry", "CanLeaveCurrentCorporation", &[])
        .await
        .expect("CanLeaveCurrentCorporation");
    let PyRep::KeyVal(can_leave_kv) = can_leave_rep else { panic!("expected KeyVal") };
    assert_eq!(can_leave_kv["canLeave"], PyRep::Bool(true));

    // rpc-lint: allow test helper
    rpc(&ctx, &mut sess_invitee, "corpRegistry", "LeaveCorporation", &[])
        .await
        .expect("LeaveCorporation");

    let (in_corp, new_corp_id) = ctx.db.with_snapshot(|snap| {
        let corp = snap.corporations.get(&corp_id).unwrap();
        let ch = snap.characters.get(&invitee_id).unwrap();
        (corp.members.contains(&invitee_id), ch.corporation_id)
    });
    assert!(!in_corp);
    assert!(new_corp_id < 98_000_000);

    // 3. CEO cannot leave while other members remain
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().members.insert(invitee_id);
    });

    let ceo_leave_check = rpc(&ctx, &mut sess_ceo, "corpRegistry", "CanLeaveCurrentCorporation", &[])
        .await
        .expect("CanLeave CEO");
    let PyRep::KeyVal(ceo_leave_kv) = ceo_leave_check else { panic!("expected KeyVal") };
    assert_eq!(ceo_leave_kv["canLeave"], PyRep::Bool(false));
}

// parity: feat:corporation.kick-member, feat:corporation.roles
#[tokio::test]
async fn test_corp_kick_member_and_roles() {
    eve_testkit::attest(&[
        "feat:corporation.kick-member",
        "feat:corporation.roles",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let ceo_id = 91_000_301i64;
    let member_id = 91_000_302i64;
    let corp_id = 98_000_301i64;

    setup_pilot(&ctx, ceo_id, "Leader Pilot", corp_id, 10_000_000, 5);
    setup_pilot(&ctx, member_id, "Fleet Member", corp_id, 1_000_000, 0);
    setup_test_corp(&ctx, corp_id, "Solaris Syndicate", "SSYND", ceo_id, &[ceo_id, member_id]);

    let mut sess_ceo = make_session(4001, 401, ceo_id, corp_id, u64::MAX);
    let mut sess_mem = make_session(4002, 402, member_id, corp_id, 0);

    // 1. Grant roles: Accountant, Station Manager, Personnel Manager
    let target_roles = corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_STATION_MANAGER | corp_roles::ROLE_PERSONNEL_MANAGER;
    let hq_roles = corp_roles::ROLE_HANGAR_CAN_TAKE[0];
    let base_roles = corp_roles::ROLE_CONTAINER_CAN_TAKE[0];

    rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "UpdateMember",
        &member_update_args(member_id, "Treasurer & Logistics", target_roles, hq_roles, base_roles, 0),
    )
    .await
    .expect("UpdateMember roles");

    let updated_roles = ctx.db.with_snapshot(|s| s.corporations.get(&corp_id).unwrap().member_roles.get(&member_id).copied().unwrap_or(0));
    assert_eq!(updated_roles, target_roles);

    // 2. Member with active roles cannot be kicked
    let can_kick = rpc(&ctx, &mut sess_ceo, "corpRegistry", "CanBeKickedOut", &[PyRep::Integer(member_id)])
        .await
        .expect("CanBeKickedOut");
    assert_eq!(can_kick, PyRep::Bool(false));

    // 3. Strip member roles
    rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "UpdateMember",
        &member_update_args(member_id, "Regular Member", 0, 0, 0, 0),
    )
    .await
    .expect("Strip roles");

    // Immediately after stripping roles, 24h role-drop cooldown is active
    let can_kick_cd = rpc(&ctx, &mut sess_ceo, "corpRegistry", "CanBeKickedOut", &[PyRep::Integer(member_id)])
        .await
        .expect("CanBeKickedOut cooldown");
    assert_eq!(can_kick_cd, PyRep::Bool(false));

    // Advance clock past 24h cooldown (86,400s)
    ctx.advance_secs(86_401);

    let can_kick_stripped = rpc(&ctx, &mut sess_ceo, "corpRegistry", "CanBeKickedOut", &[PyRep::Integer(member_id)])
        .await
        .expect("CanBeKickedOut stripped");
    assert_eq!(can_kick_stripped, PyRep::Bool(true));

    // 4. Kick member
    let err_kick = rpc(&ctx, &mut sess_mem, "corpRegistry", "KickOutMember", &[PyRep::Integer(ceo_id)])
        .await
        .expect_err("unauthorized kick");
    assert!(err_kick.code == "CorpRoleDenied" || err_kick.code == "CannotKickCEO");

    rpc(&ctx, &mut sess_ceo, "corpRegistry", "KickOutMember", &[PyRep::Integer(member_id)])
        .await
        .expect("KickOutMember");

    let in_corp = ctx.db.with_snapshot(|s| s.corporations.get(&corp_id).unwrap().members.contains(&member_id));
    assert!(!in_corp);
}

// parity: feat:corporation.titles
#[tokio::test]
async fn test_corp_titles_and_role_bundling() {
    eve_testkit::attest(&[
        "feat:corporation.titles",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let ceo_id = 91_000_401i64;
    let member_id = 91_000_402i64;
    let corp_id = 98_000_401i64;

    setup_pilot(&ctx, ceo_id, "Title CEO", corp_id, 10_000_000, 5);
    setup_pilot(&ctx, member_id, "Title Officer", corp_id, 1_000_000, 0);
    setup_test_corp(&ctx, corp_id, "Dominion Sovereignty", "DOMSO", ceo_id, &[ceo_id, member_id]);

    let mut sess_ceo = make_session(5001, 501, ceo_id, corp_id, u64::MAX);
    let mut sess_mem = make_session(5002, 502, member_id, corp_id, 0);

    // 1. Create Title 1: "Chief Financial Officer" (Accountant + Junior Accountant)
    let cfo_roles = corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_JUNIOR_ACCOUNTANT;
    rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "UpdateTitle",
        &[PyRep::Integer(1), PyRep::String("Chief Financial Officer".into()), PyRep::Integer(cfo_roles as i64)],
    )
    .await
    .expect("Create Title 1");

    // Create Title 2: "Station Chief" (Station Manager)
    let station_roles = corp_roles::ROLE_STATION_MANAGER;
    rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "UpdateTitle",
        &[PyRep::Integer(2), PyRep::String("Station Chief".into()), PyRep::Integer(station_roles as i64)],
    )
    .await
    .expect("Create Title 2");

    // 2. Query titles via GetTitles (all 16 corporation title slots)
    let titles_rep = rpc(&ctx, &mut sess_ceo, "corpRegistry", "GetTitles", &[])
        .await
        .expect("GetTitles");
    let PyRep::Rowset { rows, .. } = titles_rep else { panic!("expected Rowset") };
    assert_eq!(rows.len(), 16);

    // Non-director cannot UpdateTitle
    let err_title = rpc(
        &ctx,
        &mut sess_mem,
        "corpRegistry",
        "UpdateTitle",
        &[PyRep::Integer(3), PyRep::String("Unauthorized".into()), PyRep::Integer(0)],
    )
    .await
    .expect_err("member cannot UpdateTitle");
    assert_eq!(err_title.code, "CorpRoleDenied");

    // 3. Assign Title 1 (mask = 1) to Member
    rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "UpdateMember",
        &member_update_args(member_id, "CFO", 0, 0, 0, 1),
    )
    .await
    .expect("Assign Title 1");

    // Verify member has title and bundled roles in DB and GetMember RPC
    let member_info1 = rpc(&ctx, &mut sess_ceo, "corpRegistry", "GetMember", &[PyRep::Integer(member_id)])
        .await
        .expect("GetMember with Title 1");
    let PyRep::KeyVal(info_kv1) = member_info1 else { panic!("expected KeyVal") };
    assert_eq!(info_kv1["titleMask"], PyRep::Integer(1));
    assert_eq!(info_kv1["title"], PyRep::String("Chief Financial Officer".into()));

    let (tmask1, title1_roles) = ctx.db.with_snapshot(|snap| {
        let gov = snap.corp_governance.get(&corp_id).unwrap();
        let tm = gov.member_titles.get(&member_id).copied().unwrap_or(0);
        let tr = gov.titles.get(&1).map(|(_, r)| *r).unwrap_or(0);
        (tm, tr)
    });
    assert_eq!(tmask1, 1);
    assert_eq!(title1_roles, cfo_roles);

    // 4. Assign Title 1 + Title 2 (mask = 1 | 2 = 3)
    rpc(
        &ctx,
        &mut sess_ceo,
        "corpRegistry",
        "UpdateMember",
        &member_update_args(member_id, "CFO & Station Chief", 0, 0, 0, 3),
    )
    .await
    .expect("Assign Title 1 and 2");

    let member_info2 = rpc(&ctx, &mut sess_ceo, "corpRegistry", "GetMember", &[PyRep::Integer(member_id)])
        .await
        .expect("GetMember with Title 1 and 2");
    let PyRep::KeyVal(info_kv2) = member_info2 else { panic!("expected KeyVal") };
    assert_eq!(info_kv2["titleMask"], PyRep::Integer(3));
    assert_eq!(info_kv2["title"], PyRep::String("Chief Financial Officer, Station Chief".into()));

    let tmask2 = ctx.db.with_snapshot(|s| s.corp_governance.get(&corp_id).unwrap().member_titles.get(&member_id).copied().unwrap_or(0));
    assert_eq!(tmask2, 3);

    // 5. Delete Title 2
    rpc(&ctx, &mut sess_ceo, "corpRegistry", "DeleteTitle", &[PyRep::Integer(2)])
        .await
        .expect("DeleteTitle");

    let titles_after = rpc(&ctx, &mut sess_ceo, "corpRegistry", "GetTitles", &[])
        .await
        .expect("GetTitles after delete");
    let PyRep::Rowset { rows: rows_after, .. } = titles_after else { panic!("expected Rowset") };
    assert_eq!(rows_after.len(), 16);
    assert_eq!(rows_after[1][1], PyRep::String("".into()));
}

#[tokio::test]
async fn npc_corporation_get_member_resolves_member_and_rejects_non_members() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let npc_corp_id = 1_000_006; // SDE NPC corp (School of Applied Knowledge)
    let pilot_id = 90_000_001;
    let outsider_id = 90_000_002;
    setup_pilot(&ctx, pilot_id, "NPC Pilot", npc_corp_id, 1_000_000, 0);
    setup_pilot(&ctx, outsider_id, "Caldari Pilot", NPC_CORP_CALDARI, 1_000_000, 0);

    let mut sess = make_session(62_001, 1, pilot_id, npc_corp_id, 0);
    let member_rep = rpc(&ctx, &mut sess, "corpRegistry", "GetMember", &[PyRep::Integer(pilot_id)])
        .await
        .expect("GetMember for NPC corp member must succeed");
    assert_eq!(member_rep.dict_get("characterID"), Some(&PyRep::Integer(pilot_id)));
    assert_eq!(member_rep.dict_get("corporationID"), Some(&PyRep::Integer(npc_corp_id)));
    assert_eq!(member_rep.dict_get("roles"), Some(&PyRep::Integer(0)));
    assert_eq!(member_rep.dict_get("accountKey"), Some(&PyRep::Integer(1000)));
    assert_eq!(member_rep.dict_get("blockRoles"), Some(&PyRep::Integer(0)));
    assert_eq!(member_rep.dict_get("blocksRoles"), Some(&PyRep::Integer(0)));

    // Calling GetMember with no args resolves session viewer
    let self_rep = rpc(&ctx, &mut sess, "corpRegistry", "GetMember", &[])
        .await
        .expect("GetMember() with no args for NPC corp member must succeed");
    assert_eq!(self_rep.dict_get("characterID"), Some(&PyRep::Integer(pilot_id)));
    assert_eq!(self_rep.dict_get("blockRoles"), Some(&PyRep::Integer(0)));
    assert_eq!(self_rep.dict_get("blocksRoles"), Some(&PyRep::Integer(0)));

    // Negative: querying pilot from a different corporation returns NotCorpMember
    let not_mem_err = rpc(&ctx, &mut sess, "corpRegistry", "GetMember", &[PyRep::Integer(outsider_id)])
        .await
        .expect_err("pilot in another corp must fail with NotCorpMember");
    assert_eq!(not_mem_err.code, "NotCorpMember");

    // Negative: nonexistent corp ID (neither player nor SDE NPC) still returns CorporationNotFound
    let mut bad_sess = make_session(62_002, 1, pilot_id, 98_999_999, 0);
    let bad_corp_err = rpc(&ctx, &mut bad_sess, "corpRegistry", "GetMember", &[PyRep::Integer(pilot_id)])
        .await
        .expect_err("unknown corp must return CorporationNotFound");
    assert_eq!(bad_corp_err.code, "CorporationNotFound");
}

#[tokio::test]
async fn select_character_restores_ceo_and_member_corp_roles_without_mutating_snapshot() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ceo_id = 90_000_001;
    let officer_id = 90_000_002;
    let npc_pilot_id = 90_000_003;
    let corp_id = 98_100_001;
    setup_pilot(&ctx, ceo_id, "CEO Pilot", NPC_CORP_CALDARI, 50_000_000, 5);
    setup_pilot(&ctx, officer_id, "Officer Pilot", NPC_CORP_CALDARI, 10_000_000, 0);
    setup_pilot(&ctx, npc_pilot_id, "Starter Pilot", 1_000_006, 1_000_000, 0);
    setup_test_corp(&ctx, corp_id, "SKINR Test Corp", "SKNR", ceo_id, &[ceo_id, officer_id]);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&ceo_id) {
            c.corporation_id = corp_id;
        }
        if let Some(c) = snap.characters.get_mut(&officer_id) {
            c.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(ceo_id, u64::MAX);
            corp.member_roles.insert(officer_id, corp_roles::ROLE_BRAND_MANAGER);
        }
    });

    // Fresh login session for CEO restores u64::MAX and computes corprole + rolesAtAll diff
    let mut ceo_login = SessionState::new(62_010);
    ceo_login.user_id = Some(1);
    ceo_login.role = eve_machonet::service_roles::ROLE_PLAYER;
    let prev_ceo = ceo_login.clone();
    rpc(&ctx, &mut ceo_login, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(ceo_id)])
        .await
        .expect("CEO SelectCharacterID");
    assert_eq!(ceo_login.corporation_id, Some(corp_id));
    assert_eq!(ceo_login.corp_role, u64::MAX);
    let ceo_diff = prev_ceo.compute_diff(&ceo_login);
    assert_eq!(ceo_diff.get("corprole"), Some(&(PyRep::Integer(0), PyRep::Integer(-1))));
    assert_eq!(ceo_diff.get("rolesAtAll"), Some(&(PyRep::Integer(0), PyRep::Integer(-1))));

    // Fresh login session for non-CEO officer restores exact role bitmask
    let mut officer_login = SessionState::new(62_011);
    officer_login.user_id = Some(1);
    officer_login.role = eve_machonet::service_roles::ROLE_PLAYER;
    rpc(&ctx, &mut officer_login, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(officer_id)])
        .await
        .expect("Officer SelectCharacterID");
    assert_eq!(officer_login.corp_role, corp_roles::ROLE_BRAND_MANAGER);

    // Negative / baseline: NPC corp pilot keeps corp_role = 0
    let mut npc_login = SessionState::new(62_012);
    npc_login.user_id = Some(1);
    npc_login.role = eve_machonet::service_roles::ROLE_PLAYER;
    npc_login.corp_role = corp_roles::ROLE_DIRECTOR;
    rpc(&ctx, &mut npc_login, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(npc_pilot_id)])
        .await
        .expect("NPC Pilot SelectCharacterID");
    assert_eq!(npc_login.corp_role, 0);
}

#[tokio::test]
async fn get_titles_returns_deterministic_10col_rowset_and_live_client_dict_of_util_rows() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let corp_id = 98_090_002;
    let ceo_id = 90_000_030;
    setup_pilot(&ctx, ceo_id, "Title CEO", NPC_CORP_CALDARI, 10_000_000, 5);
    setup_test_corp(&ctx, corp_id, "Title Shape Corp", "TSHP", ceo_id, &[ceo_id]);
    let mut sess = make_session(62_020, 30, ceo_id, corp_id, u64::MAX);

    // Player corp always seeds all 16 power-of-two title slots (1..=32768) under live client trace
    let seeded_live = ctx
        .dispatch_rpc(&mut sess, "corpRegistry", None, "GetTitles", &[], &HashMap::new(), Some("machoNet:26000"))
        .await
        .expect("live GetTitles seeded");
    let PyRep::Dict(seeded_entries) = seeded_live else {
        panic!("expected Dict of 16 seeded titles for live V24 GetTitles");
    };
    assert_eq!(seeded_entries.len(), 16);
    for (i, (k, _)) in seeded_entries.iter().enumerate() {
        assert_eq!(*k, PyRep::Integer(1i64 << i));
    }

    // Insert titles out of order (title 4 then title 1) with scoped & grantable roles
    rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateTitle",
        &[
            PyRep::Integer(4),
            PyRep::String("Brand Lead".into()),
            PyRep::Integer(corp_roles::ROLE_BRAND_MANAGER as i64),
            PyRep::Integer(corp_roles::ROLE_BRAND_MANAGER as i64),
            PyRep::Integer(corp_roles::ROLE_HANGAR_CAN_TAKE[0] as i64),
            PyRep::Integer(0),
            PyRep::Integer(corp_roles::ROLE_HANGAR_CAN_QUERY[0] as i64),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("UpdateTitle 4");
    rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateTitle",
        &[
            PyRep::Integer(1),
            PyRep::String("Treasurer".into()),
            PyRep::Integer(corp_roles::ROLE_ACCOUNTANT as i64),
        ],
    )
    .await
    .expect("UpdateTitle 1");

    // Internal caller receives sorted 10-column Rowset with all 16 power-of-two slots
    let internal_rs = rpc(&ctx, &mut sess, "corpRegistry", "GetTitles", &[])
        .await
        .expect("internal GetTitles");
    let PyRep::Rowset { columns, rows } = internal_rs else {
        panic!("expected Rowset for internal GetTitles");
    };
    assert_eq!(columns.len(), 10);
    assert_eq!(rows.len(), 16);
    assert_eq!(rows[0][0], PyRep::Integer(1));
    assert_eq!(rows[2][0], PyRep::Integer(4));

    // Live V24 client (machoNet:26000) receives Dict<titleID, util.Row> with all 16 power-of-two keys
    let live_rep = ctx
        .dispatch_rpc(&mut sess, "corpRegistry", None, "GetTitles", &[], &HashMap::new(), Some("machoNet:26000"))
        .await
        .expect("live GetTitles");
    let PyRep::Dict(entries) = live_rep else {
        panic!("expected Dict for live V24 GetTitles");
    };
    assert_eq!(entries.len(), 16);
    assert_eq!(entries[0].0, PyRep::Integer(1));
    assert_eq!(entries[2].0, PyRep::Integer(4));

    let PyRep::Object { type_name, args } = &entries[2].1 else {
        panic!("expected util.Row entry");
    };
    assert_eq!(type_name, "util.Row");
    let PyRep::Tuple(state) = args.as_ref() else { panic!("expected (header, line)") };
    let (PyRep::List(hdr), PyRep::List(line)) = (&state[0], &state[1]) else {
        panic!("expected header & line lists");
    };
    let map: HashMap<&str, &PyRep> = hdr.iter().zip(line.iter()).map(|(h, v)| (h.as_str().unwrap(), v)).collect();
    assert_eq!(map["titleID"], &PyRep::Integer(4));
    assert_eq!(map["titleName"], &PyRep::String("Brand Lead".into()));
    assert_eq!(map["roles"], &PyRep::Integer(corp_roles::ROLE_BRAND_MANAGER as i64));
    assert_eq!(map["grantableRoles"], &PyRep::Integer(corp_roles::ROLE_BRAND_MANAGER as i64));
    assert_eq!(map["rolesAtHQ"], &PyRep::Integer(corp_roles::ROLE_HANGAR_CAN_TAKE[0] as i64));
    assert_eq!(map["rolesAtBase"], &PyRep::Integer(corp_roles::ROLE_HANGAR_CAN_QUERY[0] as i64));
}
