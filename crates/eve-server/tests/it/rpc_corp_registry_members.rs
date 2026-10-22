//! Authentic state-backed integration tests for `corpRegistry` member, application, and corporation lifecycle RPC methods:
//! - Corporation lifecycle: `AddCorporation`, `GetCorporation`, `GetInfoWindowDataForChar`, `UpdateCorporation`, `UpdateLogo`, `UpdateDivisionNames`
//! - Member listings: `GetMember`, `GetMembersPaged`, `GetMembersByIds`, `GetMemberIDsByQuery`
//! - Tracking & roles: `GetMemberTrackingInfo`, `GetMemberTrackingInfoSimple`, `UpdateMember`, `UpdateMembers`
//! - Membership transitions: `CanLeaveCurrentCorporation`, `CanBeKickedOut`, `KickOutMember`, `KickOutMembers`, `GetPendingAutoKicks`
//! - Applications & invitations: `InsertApplication`, `GetApplications`, `GetMyApplications`, `GetOldApplications`, `GetMyOldApplications`, `UpdateApplicationOffer`, `InsertInvitation`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PILOT_3: i64 = 90_000_003;

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

fn rowset_data(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        _ => (Vec::new(), Vec::new()),
    }
}

#[tokio::test]
async fn test_corp_creation_info_branding_and_members() {
    // parity: rpc:corpRegistry.AddCorporation
    // parity: rpc:corpRegistry.GetCorporation
    // parity: rpc:corpRegistry.GetInfoWindowDataForChar
    // parity: rpc:corpRegistry.UpdateCorporation
    // parity: rpc:corpRegistry.UpdateLogo
    // parity: rpc:corpRegistry.UpdateDivisionNames
    // parity: rpc:corpRegistry.GetMember
    // parity: rpc:corpRegistry.GetMembersPaged
    // parity: rpc:corpRegistry.GetMembersByIds
    // parity: rpc:corpRegistry.GetMemberIDsByQuery

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 60_001, "demo", "demo-secret", PILOT_1).await;

    // 1. AddCorporation
    let created = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Astra Vanguard Holdings".into()),
            PyRep::String("ASTRA".into()),
            PyRep::Real(0.075),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");
    assert!(corp_id >= 98_000_000);
    assert_eq!(sess.corporation_id, Some(corp_id));

    // 2. GetCorporation
    let corp_info = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetCorporation",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetCorporation");
    assert_eq!(field(&corp_info, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(
        field(&corp_info, "corporationName"),
        PyRep::String("Astra Vanguard Holdings".into())
    );
    assert_eq!(field(&corp_info, "ticker"), PyRep::String("ASTRA".into()));
    assert_eq!(field(&corp_info, "ceoID"), PyRep::Integer(PILOT_1));
    assert_eq!(field(&corp_info, "taxRate"), PyRep::Real(0.075));

    // 3. GetInfoWindowDataForChar
    let char_info = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetInfoWindowDataForChar",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetInfoWindowDataForChar");
    assert_eq!(field(&char_info, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&char_info, "ticker"), PyRep::String("ASTRA".into()));

    // 4. UpdateCorporation
    let upd_corp = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Astra Vanguard - Sovereign Operations".into()),
            PyRep::String("https://astra.vanguard".into()),
            PyRep::Real(0.045),
        ],
    )
    .await
    .expect("UpdateCorporation");
    assert_eq!(field(&upd_corp, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(
        field(&upd_corp, "description"),
        PyRep::String("Astra Vanguard - Sovereign Operations".into())
    );
    assert_eq!(
        field(&upd_corp, "url"),
        PyRep::String("https://astra.vanguard".into())
    );
    assert_eq!(field(&upd_corp, "taxRate"), PyRep::Real(0.045));

    // 5. UpdateLogo
    let logo_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateLogo",
        &[
            PyRep::Integer(14),
            PyRep::Integer(28),
            PyRep::Integer(42),
            PyRep::Integer(8),
        ],
    )
    .await
    .expect("UpdateLogo");
    assert_eq!(field(&logo_res, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&logo_res, "shape1"), PyRep::Integer(14));
    assert_eq!(field(&logo_res, "shape2"), PyRep::Integer(28));
    assert_eq!(field(&logo_res, "shape3"), PyRep::Integer(42));
    assert_eq!(field(&logo_res, "color1"), PyRep::Integer(8));

    // 6. UpdateDivisionNames
    let div_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UpdateDivisionNames",
        &[
            PyRep::Integer(1),
            PyRep::String("Strategic Reserves".into()),
        ],
    )
    .await
    .expect("UpdateDivisionNames");
    assert_eq!(field(&div_res, "divisionID"), PyRep::Integer(1));
    assert_eq!(
        field(&div_res, "name"),
        PyRep::String("Strategic Reserves".into())
    );

    // Add additional members to test multi-member corporate directory, pagination, query and filtering
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_2);
        corp.member_roles.insert(PILOT_2, 0);
        corp.members.insert(PILOT_3);
        corp.member_roles.insert(PILOT_3, 0);
        if let Some(ch2) = snap.characters.get_mut(&PILOT_2) {
            ch2.corporation_id = corp_id;
            ch2.name = "Bravo Pilot".into();
        }
        if let Some(mut ch3) = snap.characters.get(&PILOT_2).cloned() {
            ch3.character_id = PILOT_3;
            ch3.corporation_id = corp_id;
            ch3.name = "Charlie Scout".into();
            snap.characters.insert(PILOT_3, ch3);
        }
    });

    // 7. GetMember
    let mem_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMember",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetMember");
    let (m_cols, m_rows) = rowset_data(&mem_res);
    assert!(m_cols.contains(&"characterID".to_string()));
    assert_eq!(m_rows.len(), 3);
    assert_eq!(m_rows[0][0], PyRep::Integer(PILOT_1));
    assert_eq!(m_rows[1][0], PyRep::Integer(PILOT_2));
    assert_eq!(m_rows[2][0], PyRep::Integer(PILOT_3));

    // 8. GetMembersPaged (authentically sliced by page and pageSize)
    let paged1 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMembersPaged",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(1),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("GetMembersPaged page 1");
    let (_, p1_rows) = rowset_data(&paged1);
    assert_eq!(p1_rows.len(), 2);
    assert_eq!(p1_rows[0][0], PyRep::Integer(PILOT_1));
    assert_eq!(p1_rows[1][0], PyRep::Integer(PILOT_2));

    let paged2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMembersPaged",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(2),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("GetMembersPaged page 2");
    let (_, p2_rows) = rowset_data(&paged2);
    assert_eq!(p2_rows.len(), 1);
    assert_eq!(p2_rows[0][0], PyRep::Integer(PILOT_3));

    let paged3 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMembersPaged",
        &[
            PyRep::Integer(corp_id),
            PyRep::Integer(3),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("GetMembersPaged page 3");
    let (_, p3_rows) = rowset_data(&paged3);
    assert_eq!(p3_rows.len(), 0);

    // 9. GetMembersByIds (authentically filtered by requested ID list)
    let by_ids = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMembersByIds",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Integer(PILOT_2)]),
        ],
    )
    .await
    .expect("GetMembersByIds");
    let (_, i_rows) = rowset_data(&by_ids);
    assert_eq!(i_rows.len(), 1);
    assert_eq!(i_rows[0][0], PyRep::Integer(PILOT_2));

    let by_multi_ids = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMembersByIds",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Integer(PILOT_1), PyRep::Integer(PILOT_3)]),
        ],
    )
    .await
    .expect("GetMembersByIds multi");
    let (_, mi_rows) = rowset_data(&by_multi_ids);
    assert_eq!(mi_rows.len(), 2);
    assert_eq!(mi_rows[0][0], PyRep::Integer(PILOT_1));
    assert_eq!(mi_rows[1][0], PyRep::Integer(PILOT_3));

    let by_absent_id = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMembersByIds",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![PyRep::Integer(999_999)]),
        ],
    )
    .await
    .expect("GetMembersByIds absent");
    let (_, no_rows) = rowset_data(&by_absent_id);
    assert_eq!(no_rows.len(), 0);

    // 10. GetMemberIDsByQuery (authentically matched by name and id)
    let by_q_bravo = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::Integer(corp_id), PyRep::String("bravo".into())],
    )
    .await
    .expect("GetMemberIDsByQuery bravo");
    let (_, q_bravo_rows) = rowset_data(&by_q_bravo);
    assert_eq!(q_bravo_rows.len(), 1);
    assert_eq!(q_bravo_rows[0][0], PyRep::Integer(PILOT_2));

    let by_q_id = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::Integer(corp_id), PyRep::String(PILOT_3.to_string())],
    )
    .await
    .expect("GetMemberIDsByQuery id");
    let (_, q_id_rows) = rowset_data(&by_q_id);
    assert_eq!(q_id_rows.len(), 1);
    assert_eq!(q_id_rows[0][0], PyRep::Integer(PILOT_3));

    let by_q_none = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::Integer(corp_id), PyRep::String("nonexistent_pilot_name".into())],
    )
    .await
    .expect("GetMemberIDsByQuery none");
    let (_, q_none_rows) = rowset_data(&by_q_none);
    assert_eq!(q_none_rows.len(), 0);

    let by_q_all = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetMemberIDsByQuery all");
    let (_, q_all_rows) = rowset_data(&by_q_all);
    assert_eq!(q_all_rows.len(), 3);
}

#[tokio::test]
async fn test_corp_member_tracking_roles_and_expulsion() {
    // parity: rpc:corpRegistry.GetMemberTrackingInfo
    // parity: rpc:corpRegistry.GetMemberTrackingInfoSimple
    // parity: rpc:corpRegistry.UpdateMember
    // parity: rpc:corpRegistry.UpdateMembers
    // parity: rpc:corpRegistry.CanLeaveCurrentCorporation
    // parity: rpc:corpRegistry.CanBeKickedOut
    // parity: rpc:corpRegistry.KickOutMember
    // parity: rpc:corpRegistry.KickOutMembers
    // parity: rpc:corpRegistry.GetPendingAutoKicks

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 60_002, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 60_003, "other", "other-secret", PILOT_2).await;

    // Create corporation for pilot 1
    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Sovereign Defense Force".into()),
            PyRep::String("SDF".into()),
            PyRep::Real(0.06),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");

    // Add pilot 2 to corporation members
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_2) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(PILOT_2);
            corp.member_roles.insert(PILOT_2, 0);
        }
    });
    sess2.corporation_id = Some(corp_id);

    // 1. GetMemberTrackingInfo
    let tracking = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetMemberTrackingInfo",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetMemberTrackingInfo");
    let (t_cols, t_rows) = rowset_data(&tracking);
    assert!(t_cols.contains(&"logonDateTime".to_string()));
    assert_eq!(t_rows.len(), 2);

    // 2. GetMemberTrackingInfoSimple
    let tracking_s = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetMemberTrackingInfoSimple",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetMemberTrackingInfoSimple");
    let (_, ts_rows) = rowset_data(&tracking_s);
    assert_eq!(ts_rows.len(), 2);

    // 3. UpdateMember
    let upd_mem = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Integer(1i64 << 20), // Personnel Manager role
        ],
    )
    .await
    .expect("UpdateMember");
    assert_eq!(field(&upd_mem, "characterID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&upd_mem, "delayed24h"), PyRep::Bool(true));

    // 4. UpdateMembers
    let upd_mems = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMembers",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("UpdateMembers");
    assert_eq!(field(&upd_mems, "characterID"), PyRep::Integer(PILOT_2));

    // 5. CanLeaveCurrentCorporation
    let can_leave2 = rpc(&ctx, &mut sess2, "corpRegistry", "CanLeaveCurrentCorporation", &[])
        .await
        .expect("CanLeaveCurrentCorporation");
    assert_eq!(field(&can_leave2, "canLeave"), PyRep::Bool(true));
    assert_eq!(field(&can_leave2, "reason"), PyRep::String("OK".into()));

    let ceo_leave = rpc(&ctx, &mut sess1, "corpRegistry", "CanLeaveCurrentCorporation", &[])
        .await
        .expect("CanLeaveCurrentCorporation");
    assert_eq!(field(&ceo_leave, "canLeave"), PyRep::Bool(false));
    assert_eq!(
        field(&ceo_leave, "reason"),
        PyRep::String("CEO cannot leave while other members remain".into())
    );

    // 6. CanBeKickedOut
    let can_kick2 = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CanBeKickedOut",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("CanBeKickedOut");
    assert_eq!(can_kick2, PyRep::Bool(true));

    let can_kick_ceo = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CanBeKickedOut",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("CanBeKickedOut");
    assert_eq!(can_kick_ceo, PyRep::Bool(false));

    // 7. KickOutMember
    let kick_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("KickOutMember");
    assert_eq!(field(&kick_res, "characterID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&kick_res, "corporationID"), PyRep::Integer(corp_id));

    // 8. KickOutMembers (add pilot 3 and batch kick)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_3) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(PILOT_3);
            corp.member_roles.insert(PILOT_3, 0);
        }
    });
    let kick_mems = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMembers",
        &[PyRep::List(vec![PyRep::Integer(PILOT_3)])],
    )
    .await
    .expect("KickOutMembers");
    assert_eq!(field(&kick_mems, "characterID"), PyRep::Integer(PILOT_3));
    assert_eq!(field(&kick_mems, "corporationID"), PyRep::Integer(corp_id));

    // 9. GetPendingAutoKicks
    let pending = rpc(&ctx, &mut sess1, "corpRegistry", "GetPendingAutoKicks", &[])
        .await
        .expect("GetPendingAutoKicks");
    let (p_cols, _) = rowset_data(&pending);
    assert!(p_cols.contains(&"characterID".to_string()));
}

#[tokio::test]
async fn test_corp_applications_and_invitations() {
    // parity: rpc:corpRegistry.InsertApplication
    // parity: rpc:corpRegistry.GetApplications
    // parity: rpc:corpRegistry.GetMyApplications
    // parity: rpc:corpRegistry.GetOldApplications
    // parity: rpc:corpRegistry.GetMyOldApplications
    // parity: rpc:corpRegistry.UpdateApplicationOffer
    // parity: rpc:corpRegistry.InsertInvitation

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 60_004, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 60_005, "other", "other-secret", PILOT_2).await;

    // Create corporation for pilot 1
    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Deep Space Explorers".into()),
            PyRep::String("DSE".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");

    // 1. InsertApplication (Pilot 2 applies to join)
    let app = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "InsertApplication",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("Veteran explorer applying to join".into()),
        ],
    )
    .await
    .expect("InsertApplication");
    let app_id = field(&app, "applicationID").as_i64().expect("applicationID");
    assert!(app_id > 0);
    assert_eq!(field(&app, "applicationID"), PyRep::Integer(app_id));
    assert_eq!(field(&app, "status"), PyRep::String("Applied".into()));

    // 2. GetApplications (Pilot 1 checks applications)
    let apps = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetApplications",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetApplications");
    let (a_cols, a_rows) = rowset_data(&apps);
    assert!(a_cols.contains(&"applicationID".to_string()));
    assert!(a_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)));

    // 3. GetMyApplications (Pilot 2 views pending applications)
    let my_apps = rpc(&ctx, &mut sess2, "corpRegistry", "GetMyApplications", &[])
        .await
        .expect("GetMyApplications");
    let (_, ma_rows) = rowset_data(&my_apps);
    assert!(ma_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)));

    // 4. UpdateApplicationOffer (Pilot 1 accepts application)
    let upd_offer = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[
            PyRep::Integer(app_id),
            PyRep::String("Accepted".into()),
        ],
    )
    .await
    .expect("UpdateApplicationOffer");
    assert_eq!(field(&upd_offer, "applicationID"), PyRep::Integer(app_id));
    assert_eq!(field(&upd_offer, "status"), PyRep::String("Accepted".into()));

    // 5. GetOldApplications
    let old_apps = rpc(&ctx, &mut sess1, "corpRegistry", "GetOldApplications", &[])
        .await
        .expect("GetOldApplications");
    let (oa_cols, _) = rowset_data(&old_apps);
    assert!(oa_cols.contains(&"applicationID".to_string()));

    // 6. GetMyOldApplications
    let my_old_apps = rpc(&ctx, &mut sess2, "corpRegistry", "GetMyOldApplications", &[])
        .await
        .expect("GetMyOldApplications");
    let (mo_cols, mo_rows) = rowset_data(&my_old_apps);
    assert!(mo_cols.contains(&"applicationID".to_string()));
    assert!(mo_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)));

    // 7. InsertInvitation (Invite pilot 3)
    let inv = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "InsertInvitation",
        &[
            PyRep::Integer(PILOT_3),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("InsertInvitation");
    assert_eq!(field(&inv, "characterID"), PyRep::Integer(PILOT_3));
    assert_eq!(field(&inv, "corporationID"), PyRep::Integer(corp_id));
    assert_eq!(field(&inv, "joined"), PyRep::Bool(false));
}
