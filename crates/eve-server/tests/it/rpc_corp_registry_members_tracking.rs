//! Authentic state-backed integration tests for `corpRegistry` member tracking, role updates, expulsion, and application/invitation RPCs:
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

fn rowset_data(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        _ => (Vec::new(), Vec::new()),
    }
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

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_2) {
            ch.corporation_id = corp_id;
        }
        if let Some(mut ch3) = snap.characters.get(&PILOT_2).cloned() {
            ch3.character_id = PILOT_3;
            ch3.corporation_id = corp_id;
            ch3.name = "Charlie Recruit".into();
            snap.characters.insert(PILOT_3, ch3);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(PILOT_2);
            corp.member_roles.insert(PILOT_2, 0);
            corp.members.insert(PILOT_3);
            corp.member_roles.insert(PILOT_3, 0);
        }
    });
    sess2.corporation_id = Some(corp_id);
    sess2.corp_role = 0;

    // 1. GetMemberTrackingInfo & GetMemberTrackingInfoSimple: role gate + full vs simple location redaction + single-member filter
    assert!(
        rpc(&ctx, &mut sess2, "corpRegistry", "GetMemberTrackingInfo", &[])
            .await
            .is_err(),
        "unprivileged member must be denied GetMemberTrackingInfo"
    );
    assert!(
        rpc(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "GetMemberTrackingInfoSimple",
            &[PyRep::Integer(98_999_999)]
        )
        .await
        .is_err(),
        "foreign corporation must be denied GetMemberTrackingInfoSimple"
    );

    let tracking = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetMemberTrackingInfo",
        &[],
    )
    .await
    .expect("GetMemberTrackingInfo all");
    let (t_cols, t_rows) = rowset_data(&tracking);
    assert!(t_cols.contains(&"logonDateTime".to_string()));
    assert!(t_cols.contains(&"logoffDateTime".to_string()));
    assert!(t_cols.contains(&"baseID".to_string()));
    assert!(t_cols.contains(&"shipTypeID".to_string()));
    assert!(t_cols.contains(&"grantableRoles".to_string()));
    assert_eq!(t_rows.len(), 3);
    assert!(t_rows[0][7].as_i64().is_some(), "full tracking includes locationID");

    let tracking_single = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetMemberTrackingInfo",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("GetMemberTrackingInfo single");
    let (_, ts1_rows) = rowset_data(&tracking_single);
    assert_eq!(ts1_rows.len(), 1);
    assert_eq!(ts1_rows[0][0], PyRep::Integer(PILOT_2));

    let tracking_s = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "GetMemberTrackingInfoSimple",
        &[],
    )
    .await
    .expect("GetMemberTrackingInfoSimple");
    let (ts_cols, ts_rows) = rowset_data(&tracking_s);
    assert!(ts_cols.contains(&"characterID".to_string()));
    assert_eq!(ts_rows.len(), 3);
    assert_eq!(
        ts_rows[0][7],
        PyRep::None,
        "simple tracking redacts locationID for non-directors"
    );
    assert_eq!(
        ts_rows[0][8],
        PyRep::None,
        "simple tracking redacts shipTypeID for non-directors"
    );

    // 2. UpdateMember (16-arg V24 layout + role gate)
    assert!(
        rpc(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "UpdateMember",
            &[PyRep::Integer(PILOT_2), PyRep::Integer(128)]
        )
        .await
        .is_err()
    );
    assert_eq!(rpc(&ctx, &mut sess1, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer(-1)]).await.expect_err("negative role mask must be rejected in UpdateMember").code, "CrpAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(PILOT_2, eve_db::corp_roles::ROLE_DIRECTOR);
        }
    });
    assert_eq!(rpc(&ctx, &mut sess2, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_3), PyRep::Integer(eve_db::corp_roles::ROLE_DIRECTOR as i64)]).await.expect_err("non-CEO Director cannot grant ROLE_DIRECTOR via UpdateMember").code, "CrpAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(PILOT_2, 0);
        }
    });

    let upd_mem = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(PILOT_2), PyRep::String("Logistics Officer".into()), PyRep::Integer(2), PyRep::Integer(3),
            PyRep::Integer(128 | (1 << 20)), PyRep::Integer(256), PyRep::Integer(512), PyRep::Integer(1024),
            PyRep::Integer(2048), PyRep::Integer(4096), PyRep::Integer(8192), PyRep::Integer(16384),
            PyRep::Integer(60_003_760), PyRep::Integer(3), PyRep::Integer(0),
        ],
    ).await.expect("UpdateMember 16-arg");
    assert_eq!(field(&upd_mem, "characterID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&upd_mem, "roles"), PyRep::Integer(128 | (1 << 20)));
    assert_eq!(field(&upd_mem, "delayed24h"), PyRep::Bool(true));
    // Explicitly passing roles = 128 (without 1 << 20) in full UpdateMember revokes the pending hangar grant (R-CG3-02)
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(PILOT_2), PyRep::String("Logistics Officer".into()), PyRep::Integer(2), PyRep::Integer(3),
            PyRep::Integer(128), PyRep::Integer(256), PyRep::Integer(512), PyRep::Integer(1024),
            PyRep::Integer(2048), PyRep::Integer(4096), PyRep::Integer(8192), PyRep::Integer(16384),
            PyRep::Integer(60_003_760), PyRep::Integer(1), PyRep::Integer(0),
        ],
    ).await.expect("UpdateMember revoking pending hangar grant");
    assert!(!ctx.db.with_snapshot(|s| s.corp_governance.get(&corp_id).is_some_and(|g| g.pending_role_grants.contains_key(&PILOT_2))));
    // Re-grant (128 | (1 << 20)), then update only titleMask via partial dict: pending grant and omitted role masks must be preserved
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[
            PyRep::Integer(PILOT_2), PyRep::String("Logistics Officer".into()), PyRep::Integer(2), PyRep::Integer(3),
            PyRep::Integer(128 | (1 << 20)), PyRep::Integer(256), PyRep::Integer(512), PyRep::Integer(1024),
            PyRep::Integer(2048), PyRep::Integer(4096), PyRep::Integer(8192), PyRep::Integer(16384),
            PyRep::Integer(60_003_760), PyRep::Integer(1), PyRep::Integer(0),
        ],
    ).await.expect("UpdateMember re-granting hangar role");
    rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMember",
        &[PyRep::Dict(vec![
            (PyRep::String("characterID".into()), PyRep::Integer(PILOT_2)),
            (PyRep::String("titleMask".into()), PyRep::Integer(3)),
        ])],
    ).await.expect("UpdateMember partial dict preserves pending grant and omitted role masks");
    assert_eq!(
        rpc(&ctx, &mut sess2, "invbroker", "List", &[PyRep::Integer(116)])
            .await
            .expect_err("invbroker.List(116) must be denied before 24h")
            .code,
        "CorpRoleDenied"
    );
    ctx.advance_secs(86_401);
    rpc(&ctx, &mut sess2, "invbroker", "List", &[PyRep::Integer(116)])
        .await
        .expect("invbroker.List(116) must succeed after 24h promotion");

    let mem2_after = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("GetMember PILOT_2 after UpdateMember");
    assert_eq!(field(&mem2_after, "grantableRoles"), PyRep::Integer(256));
    assert_eq!(field(&mem2_after, "rolesAtHQ"), PyRep::Integer(512));
    assert_eq!(field(&mem2_after, "titleMask"), PyRep::Integer(3));

    // 3. UpdateMembers (bulk Rowset updating PILOT_2 and PILOT_3)
    let bulk_rowset = PyRep::rowset(
        vec![
            "characterIDToChange",
            "title",
            "divisionID",
            "squadronID",
            "roles",
            "grantableRoles",
            "rolesAtHQ",
            "grantableRolesAtHQ",
            "rolesAtBase",
            "grantableRolesAtBase",
            "rolesAtOther",
            "grantableRolesAtOther",
            "baseID",
            "titleMask",
        ],
        vec![
            vec![
                PyRep::Integer(PILOT_2),
                PyRep::String("Wing Lead".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(1024),
                PyRep::Integer(0),
                PyRep::Integer(2048),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(60_003_760),
                PyRep::Integer(5),
            ],
            vec![
                PyRep::Integer(PILOT_3),
                PyRep::String("Scout".into()),
                PyRep::Integer(2),
                PyRep::Integer(2),
                PyRep::Integer(512),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(60_003_760),
                PyRep::Integer(1),
            ],
        ],
    );
    let upd_mems = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateMembers",
        &[bulk_rowset],
    )
    .await
    .expect("UpdateMembers bulk rowset");
    assert_eq!(field(&upd_mems, "updatedCount"), PyRep::Integer(2));

    let mem3_after = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetMember",
        &[PyRep::Integer(PILOT_3)],
    )
    .await
    .expect("GetMember PILOT_3 after bulk UpdateMembers");
    assert_eq!(field(&mem3_after, "roles"), PyRep::Integer(512));
    assert_eq!(field(&mem3_after, "divisionID"), PyRep::Integer(2));
    assert_eq!(field(&mem3_after, "titleMask"), PyRep::Integer(1));

    // 4. CanLeaveCurrentCorporation & CanBeKickedOut
    let can_leave2 = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "CanLeaveCurrentCorporation",
        &[],
    )
    .await
    .expect("CanLeaveCurrentCorporation");
    assert_eq!(field(&can_leave2, "canLeave"), PyRep::Bool(true));

    let ceo_leave = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CanLeaveCurrentCorporation",
        &[],
    )
    .await
    .expect("CanLeaveCurrentCorporation CEO");
    assert_eq!(field(&ceo_leave, "canLeave"), PyRep::Bool(false));

    let can_kick2 = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CanBeKickedOut",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("CanBeKickedOut with active roles");
    assert_eq!(can_kick2, PyRep::Bool(false));

    let can_kick_ceo = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "CanBeKickedOut",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("CanBeKickedOut CEO");
    assert_eq!(can_kick_ceo, PyRep::Bool(false));

    // 5. KickOutMember & KickOutMembers (reject CEO and non-member, bulk kick PILOT_2 and PILOT_3)
    assert!(
        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "KickOutMembers",
            &[PyRep::List(vec![PyRep::Integer(PILOT_1)])]
        )
        .await
        .is_err(),
        "cannot kick CEO"
    );
    assert!(
        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "KickOutMembers",
            &[PyRep::List(vec![PyRep::Integer(99_999_999)])]
        )
        .await
        .is_err(),
        "cannot kick non-member"
    );

    let kick_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("KickOutMember PILOT_2 with active roles queues auto-kick");
    assert_eq!(field(&kick_res, "characterID"), PyRep::Integer(PILOT_2));
    assert_eq!(field(&kick_res, "queuedAutoKick"), PyRep::Bool(true));

    // Clear roles on PILOT_2 and PILOT_3 and batch-kick [PILOT_2, PILOT_3]
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(PILOT_2, 0);
            corp.member_roles.insert(PILOT_3, 0);
        }
    });
    let kick_mems = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMembers",
        &[PyRep::List(vec![
            PyRep::Integer(PILOT_2),
            PyRep::Integer(PILOT_3),
        ])],
    )
    .await
    .expect("KickOutMembers batch");
    assert_eq!(field(&kick_mems, "kickedCount"), PyRep::Integer(2));
    let remaining_members = ctx.db.with_snapshot(|snap| {
        snap.corporations
            .get(&corp_id)
            .map(|c| c.members.len())
            .unwrap_or(0)
    });
    assert_eq!(remaining_members, 1, "only CEO should remain after batch kick");

    // 6. GetPendingAutoKicks
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
    assert_eq!(field(&app, "status"), PyRep::String("Applied".into()));

    // 2. While pending: present in GetApplications & GetMyApplications, absent from GetOldApplications & GetMyOldApplications
    let apps = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetApplications",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetApplications");
    let (_, a_rows) = rowset_data(&apps);
    assert!(a_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)));

    let my_apps = rpc(&ctx, &mut sess2, "corpRegistry", "GetMyApplications", &[])
        .await
        .expect("GetMyApplications");
    let (_, ma_rows) = rowset_data(&my_apps);
    assert!(ma_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)));

    let old_before = rpc(&ctx, &mut sess1, "corpRegistry", "GetOldApplications", &[])
        .await
        .expect("GetOldApplications before decision");
    let (_, ob_rows) = rowset_data(&old_before);
    assert!(
        !ob_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)),
        "pending application must not appear in GetOldApplications"
    );

    let my_old_before = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "GetMyOldApplications",
        &[],
    )
    .await
    .expect("GetMyOldApplications before decision");
    let (_, mob_rows) = rowset_data(&my_old_before);
    assert!(
        !mob_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)),
        "pending application must not appear in GetMyOldApplications"
    );

    // 3. UpdateApplicationOffer (Pilot 1 accepts application)
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

    // 4. After decision: absent from active GetApplications/GetMyApplications, present in GetOldApplications/GetMyOldApplications
    let apps_after = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetApplications",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetApplications after");
    let (_, aa_rows) = rowset_data(&apps_after);
    assert!(
        !aa_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)),
        "closed application must not appear in active GetApplications"
    );

    let my_apps_after = rpc(&ctx, &mut sess2, "corpRegistry", "GetMyApplications", &[])
        .await
        .expect("GetMyApplications after");
    let (_, maa_rows) = rowset_data(&my_apps_after);
    assert!(
        !maa_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)),
        "closed application must not appear in active GetMyApplications"
    );

    let old_apps = rpc(&ctx, &mut sess1, "corpRegistry", "GetOldApplications", &[])
        .await
        .expect("GetOldApplications");
    let (_, oa_rows) = rowset_data(&old_apps);
    assert!(oa_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)));

    let my_old_apps = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "GetMyOldApplications",
        &[],
    )
    .await
    .expect("GetMyOldApplications");
    let (_, mo_rows) = rowset_data(&my_old_apps);
    assert!(mo_rows.iter().any(|r| r[0] == PyRep::Integer(app_id)));

    // 5. InsertInvitation
    let inv = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "InsertInvitation",
        &[PyRep::Integer(PILOT_3), PyRep::Bool(false)],
    )
    .await
    .expect("InsertInvitation");
    assert_eq!(field(&inv, "characterID"), PyRep::Integer(PILOT_3));
    assert_eq!(field(&inv, "corporationID"), PyRep::Integer(corp_id));
}
