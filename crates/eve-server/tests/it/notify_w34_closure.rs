//! Live V24 integration tests for newly-closed server notifications (`EvoMu-w34`):
//! - `notify:OnIndustryMaterials`
//! - `notify:OnCommunityFittingsUpdated`
//! - `notify:OnTutorialSkipped`

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const COMMUNITY_FITTING_CORP_ID: i64 = 1_000_282;

async fn login_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s.station_id = Some(JITA_STATION);
    ctx.advance_secs(12);
    s
}

async fn rpc(
    ctx: &ServerContext,
    s: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn find_notifs<'a>(
    notifs: &'a [PendingMachoNotification],
    event: &str,
) -> Vec<&'a PendingMachoNotification> {
    notifs.iter().filter(|n| n.event == event).collect()
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn test_on_industry_materials_v24_dict_shape() {
    // parity: notify:OnIndustryMaterials
    eve_testkit::attest(&["notify:OnIndustryMaterials"]);

    let ctx = ServerContext::new();
    let mut sess = login_pilot(&ctx, 8801, "demo", "demo-secret", PILOT_A).await;

    let bp_info = ctx
        .sde
        .blueprints
        .get(&687)
        .cloned()
        .expect("Rifter Blueprint (687) in SDE");
    assert!(
        !bp_info.materials.is_empty(),
        "Rifter Blueprint must have non-empty SDE manufacturing BOM"
    );

    let bp_id = 920_000_001_i64;
    let initial_stack_qty: i32 = 1_000_000;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.balance = IskCents(100_000_000_00);
            ch.station_id = Some(JITA_STATION);
        }
        snap.items.retain(|_, i| {
            !(i.owner_id == PILOT_A
                && i.location_id == JITA_STATION
                && bp_info.materials.iter().any(|m| m.type_id == i.type_id))
        });
        snap.items.insert(
            bp_id,
            make_item(bp_id, 687, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
        for (idx, mat) in bp_info.materials.iter().enumerate() {
            let iid = 920_000_010 + idx as i64;
            snap.items.insert(iid, make_item(iid, mat.type_id, PILOT_A, JITA_STATION, InventoryFlag::Hangar, initial_stack_qty, false));
        }
    });
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // Negative 1: unauthenticated InstallJob fails and emits 0 OnIndustryMaterials
    let mut unauth = SessionState::new(8899);
    assert!(rpc(&ctx, &mut unauth, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintItemID", PyRep::Integer(bp_id)), ("facilityID", PyRep::Integer(JITA_STATION)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.is_err());
    assert!(find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnIndustryMaterials").is_empty());

    // Negative 2: non-existent blueprint InstallJob fails and emits 0 OnIndustryMaterials
    assert!(rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintItemID", PyRep::Integer(999_999_999)), ("facilityID", PyRep::Integer(JITA_STATION)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.is_err());
    assert!(find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnIndustryMaterials").is_empty());

    // Positive: ConnectJob(job) -> InstallJob -> OnIndustryMaterials(monitorID, {typeID: remaining_qty}) -> DisconnectJob
    let mon_rep = rpc(
        &ctx,
        &mut sess,
        "industryMonitor",
        "ConnectJob",
        &[PyRep::keyval([
            ("ownerID", PyRep::Integer(PILOT_A)),
            ("locationID", PyRep::Integer(JITA_STATION)),
            ("inputFlagID", PyRep::Integer(InventoryFlag::Hangar as i64)),
        ])],
    )
    .await
    .expect("industryMonitor.ConnectJob should succeed");
    let PyRep::Tuple(mon_parts) = mon_rep else {
        panic!("ConnectJob must return (monitorID, available_dict) tuple, got {mon_rep:?}");
    };
    let monitor_id = mon_parts[0].as_i64().expect("monitorID");
    assert!(monitor_id > 0);
    assert!(matches!(mon_parts[1], PyRep::Dict(_)));

    let job_rep = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(bp_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("InstallJob should succeed");
    let job_id = job_rep
        .dict_get("jobID")
        .and_then(|v| v.as_i64())
        .expect("jobID");
    assert!(job_id > 0);

    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let ind_mats = find_notifs(&notifs, "OnIndustryMaterials");
    assert_eq!(
        ind_mats.len(),
        1,
        "Expected exactly 1 OnIndustryMaterials notification"
    );
    assert_eq!(ind_mats[0].id_type, "charid");
    assert_eq!(ind_mats[0].args.len(), 2);
    assert_eq!(ind_mats[0].args[0], PyRep::Integer(monitor_id));

    let PyRep::Dict(mat_entries) = &ind_mats[0].args[1] else {
        panic!(
            "V24 IndustryService.OnIndustryMaterials expects Dict {{typeID: available}}, got {:?}",
            ind_mats[0].args[1]
        );
    };
    assert_eq!(mat_entries.len(), bp_info.materials.len());

    for mat in &bp_info.materials {
        let reported_avail = mat_entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(i64::from(mat.type_id)))
            .and_then(|(_, v)| v.as_i64())
            .expect("each consumed BOM typeID must appear in OnIndustryMaterials dict");
        let db_avail: i64 = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| {
                    i.owner_id == PILOT_A
                        && i.location_id == JITA_STATION
                        && i.type_id == mat.type_id
                        && i.flag == InventoryFlag::Hangar
                })
                .map(|i| i64::from(i.quantity))
                .sum()
        });
        assert_eq!(
            reported_avail, db_avail,
            "OnIndustryMaterials dict value must equal post-consumption hangar quantity for type {}",
            mat.type_id
        );
        assert!(
            reported_avail < i64::from(initial_stack_qty),
            "Material {} must have been consumed below initial stack {}",
            mat.type_id,
            initial_stack_qty
        );
    }

    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "industryMonitor",
            "DisconnectJob",
            &[PyRep::Integer(monitor_id)],
        )
        .await
        .expect("DisconnectJob should succeed"),
        PyRep::Bool(true)
    );

    // Corporate hangar division isolation: useCorp=true with inputFlag=CorpSAG1 must consume from and report ONLY CorpSAG1
    let corp_id = 98_400_001_i64;
    let corp_bp_id = 920_000_100_i64;
    let sag1_initial: i32 = 2_000_000;
    let sag2_initial: i32 = 500_000;
    let take3_only = 1u64 << 15; // Take3 (bit 15), NOT Take1 (bit 13)
    let wrong_div_roles = eve_db::corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 15); // Factory Manager + Take3
    let fm_roles = eve_db::corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 13); // Factory Manager + Take1 (bit 13)
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord {
            corporation_id: corp_id,
            name: "Industry Division Corp".into(),
            ticker: "IDIV".into(),
            ceo_id: PILOT_B,
            hq_station_id: Some(JITA_STATION),
            master_wallet: IskCents(100_000_000_00),
            ..Default::default()
        };
        corp.members.insert(PILOT_A);
        corp.members.insert(PILOT_B);
        corp.member_roles.insert(PILOT_A, take3_only);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
        }
        snap.skills.entry(PILOT_A).or_default().insert(
            3387,
            eve_db::TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            corp_bp_id,
            make_item(corp_bp_id, 687, corp_id, JITA_STATION, InventoryFlag::CorpSAG1, 1, true),
        );
        for (idx, mat) in bp_info.materials.iter().enumerate() {
            snap.items.insert(
                920_000_110 + idx as i64,
                make_item(920_000_110 + idx as i64, mat.type_id, corp_id, JITA_STATION, InventoryFlag::CorpSAG1, sag1_initial, false),
            );
            snap.items.insert(
                920_000_210 + idx as i64,
                make_item(920_000_210 + idx as i64, mat.type_id, corp_id, JITA_STATION, InventoryFlag::CorpSAG2, sag2_initial, false),
            );
        }
    });
    sess.corporation_id = Some(corp_id);
    sess.corp_role = take3_only;

    // Negative: member holding Take3 (bit 15) must be denied CorpSAG1 on ConnectJob
    let take3_conn_err = rpc(
        &ctx,
        &mut sess,
        "industryMonitor",
        "ConnectJob",
        &[PyRep::keyval([
            ("ownerID", PyRep::Integer(corp_id)),
            ("locationID", PyRep::Integer(JITA_STATION)),
            ("inputFlagID", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
    )
    .await
    .expect_err("Take3 holder must be denied CorpSAG1 ConnectJob");
    assert_eq!(take3_conn_err.code, "CrpAccessDenied");

    // Negative: member holding Factory Manager + Take3 (bit 15) must be denied CorpSAG1 on InstallJob
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(PILOT_A, wrong_div_roles);
        }
    });
    sess.corp_role = wrong_div_roles;

    let take3_inst_err = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(corp_bp_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("useCorp", PyRep::Bool(true)),
            ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
    )
    .await
    .expect_err("Take3 holder must be denied CorpSAG1 InstallJob");
    assert_eq!(take3_inst_err.code, "CrpAccessDenied");

    // Grant Take1 (bit 13): now CorpSAG1 succeeds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(PILOT_A, fm_roles);
        }
    });
    sess.corp_role = fm_roles;
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    let corp_mon_rep = rpc(
        &ctx,
        &mut sess,
        "industryMonitor",
        "ConnectJob",
        &[PyRep::keyval([
            ("ownerID", PyRep::Integer(corp_id)),
            ("locationID", PyRep::Integer(JITA_STATION)),
            ("inputFlagID", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
    )
    .await
    .expect("corp industryMonitor.ConnectJob should succeed");
    let PyRep::Tuple(corp_mon_parts) = corp_mon_rep else {
        panic!("ConnectJob must return (monitorID, available_dict) tuple");
    };
    let corp_monitor_id = corp_mon_parts[0].as_i64().expect("corp monitorID");

    let corp_job = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(corp_bp_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("useCorp", PyRep::Bool(true)),
            ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
    )
    .await
    .expect("Corporate InstallJob from CorpSAG1 should succeed");
    let corp_job_id = corp_job.dict_get("jobID").and_then(|v| v.as_i64()).expect("corp jobID");
    assert!(corp_job_id > 0);
    let corp_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let corp_ind_mats = find_notifs(&corp_notifs, "OnIndustryMaterials");
    assert_eq!(corp_ind_mats.len(), 1);
    assert_eq!(corp_ind_mats[0].args[0], PyRep::Integer(corp_monitor_id));
    let PyRep::Dict(corp_mat_entries) = &corp_ind_mats[0].args[1] else {
        panic!("expected Dict in corporate OnIndustryMaterials");
    };
    for mat in &bp_info.materials {
        let reported = corp_mat_entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(i64::from(mat.type_id)))
            .and_then(|(_, v)| v.as_i64())
            .expect("consumed material in corp OnIndustryMaterials");
        let (sag1_db, sag2_db): (i64, i64) = ctx.db.with_snapshot(|snap| {
            let s1 = snap.items.values().filter(|i| i.owner_id == corp_id && i.location_id == JITA_STATION && i.type_id == mat.type_id && i.flag == InventoryFlag::CorpSAG1).map(|i| i64::from(i.quantity)).sum();
            let s2 = snap.items.values().filter(|i| i.owner_id == corp_id && i.location_id == JITA_STATION && i.type_id == mat.type_id && i.flag == InventoryFlag::CorpSAG2).map(|i| i64::from(i.quantity)).sum();
            (s1, s2)
        });
        assert_eq!(sag2_db, i64::from(sag2_initial), "CorpSAG2 stack must not be touched by CorpSAG1 job");
        assert_eq!(
            reported, sag1_db,
            "OnIndustryMaterials for corporate job must report strictly CorpSAG1 remaining quantity (excluding personal Hangar and CorpSAG2)"
        );
        assert!(reported < i64::from(sag1_initial));
    }
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "industryMonitor",
            "DisconnectJob",
            &[PyRep::Integer(corp_monitor_id)],
        )
        .await
        .expect("corp DisconnectJob should succeed"),
        PyRep::Bool(true)
    );
}

#[tokio::test]
async fn test_on_community_fittings_updated_auth_and_broadcast() {
    // parity: notify:OnCommunityFittingsUpdated
    eve_testkit::attest(&["notify:OnCommunityFittingsUpdated"]);

    let ctx = ServerContext::new();
    let mut sess_a = login_pilot(&ctx, 8802, "demo", "demo-secret", PILOT_A).await;
    let _sess_b = login_pilot(&ctx, 8803, "other", "other-secret", PILOT_B).await;
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    let fit_kv = PyRep::keyval([
        ("name", PyRep::String("Community Rifter".into())),
        ("shipTypeID", PyRep::Integer(587)),
        ("description", PyRep::String("Curated V24 community fit".into())),
        ("fitData", PyRep::List(vec![])),
    ]);

    // Negative 1: unprivileged player cannot save to COMMUNITY_FITTING_CORP (1000282) or owner 1
    sess_a.role = 0;
    for owner in [COMMUNITY_FITTING_CORP_ID, 1_i64] {
        let denied = rpc(
            &ctx,
            &mut sess_a,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(owner), fit_kv.clone()],
        )
        .await
        .expect_err("unprivileged player must not save community fittings");
        assert_eq!(denied.code, "AccessDenied");
    }
    assert!(
        find_notifs(
            &ctx.drain_notifications_for_char(PILOT_A),
            "OnCommunityFittingsUpdated"
        )
        .is_empty()
    );

    // Negative 2: personal charFittingMgr.SaveFitting emits OnFittingsUpdated, NOT OnCommunityFittingsUpdated
    let personal_fid = rpc(
        &ctx,
        &mut sess_a,
        "charFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(PILOT_A), fit_kv.clone()],
    )
    .await
    .expect("personal SaveFitting")
    .as_i64()
    .expect("fitting id");
    assert!(personal_fid > 0);
    assert!(
        find_notifs(
            &ctx.drain_notifications_for_char(PILOT_A),
            "OnCommunityFittingsUpdated"
        )
        .is_empty()
    );
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // Positive 1: Content/GM role saving to COMMUNITY_FITTING_CORP (1000282) broadcasts 0-arg OnCommunityFittingsUpdated
    sess_a.role = service_roles::ROLE_CONTENT;
    let comm_fid = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(COMMUNITY_FITTING_CORP_ID), fit_kv],
    )
    .await
    .expect("Content SaveFitting to COMMUNITY_FITTING_CORP")
    .as_i64()
    .expect("community fitting id");
    assert!(comm_fid > 0);

    for cid in [PILOT_A, PILOT_B] {
        let notifs = ctx.drain_notifications_for_char(cid);
        let comm_notifs = find_notifs(&notifs, "OnCommunityFittingsUpdated");
        assert_eq!(
            comm_notifs.len(),
            1,
            "Online pilot {cid} must receive broadcast OnCommunityFittingsUpdated"
        );
        assert_eq!(comm_notifs[0].id_type, "clientID");
        assert!(comm_notifs[0].args.is_empty());
    }

    let comm_dict = rpc(&ctx, &mut sess_a, "corpFittingMgr", "GetCommunityFittings", &[])
        .await
        .expect("GetCommunityFittings");
    let PyRep::Dict(entries) = comm_dict else {
        panic!("expected Dict from GetCommunityFittings");
    };
    assert!(
        entries.iter().any(|(k, _)| k.as_i64() == Some(comm_fid)),
        "GetCommunityFittings must include newly saved community fitting"
    );

    // Negative 3: unprivileged caller cannot rename or delete community fitting
    sess_a.role = 0;
    let upd_denied = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "UpdateNameAndDescription",
        &[
            PyRep::Integer(comm_fid),
            PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
            PyRep::String("Unauthorized Rename".into()),
            PyRep::String("Should fail".into()),
        ],
    )
    .await
    .expect_err("unprivileged player must not update community fittings");
    assert_eq!(upd_denied.code, "AccessDenied");
    let del_denied = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "DeleteFitting",
        &[
            PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
            PyRep::Integer(comm_fid),
        ],
    )
    .await
    .expect_err("unprivileged player must not delete community fittings");
    assert_eq!(del_denied.code, "AccessDenied");
    assert!(
        find_notifs(
            &ctx.drain_notifications_for_char(PILOT_B),
            "OnCommunityFittingsUpdated"
        )
        .is_empty()
    );

    // Positive 2: Content/GM role renaming community fitting updates metadata and broadcasts 0-arg OnCommunityFittingsUpdated
    sess_a.role = service_roles::ROLE_CONTENT;
    let renamed = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "UpdateNameAndDescription",
        &[
            PyRep::Integer(comm_fid),
            PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
            PyRep::String("Updated Community Rifter".into()),
            PyRep::String("Updated curated V24 community fit".into()),
        ],
    )
    .await
    .expect("Content UpdateNameAndDescription");
    assert_eq!(renamed.dict_get("name"), Some(&PyRep::String("Updated Community Rifter".into())));
    let b_upd_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let b_upd_comm = find_notifs(&b_upd_notifs, "OnCommunityFittingsUpdated");
    assert_eq!(b_upd_comm.len(), 1);
    assert!(b_upd_comm[0].args.is_empty());

    // Positive 3: Content/GM role deleting community fitting broadcasts 0-arg OnCommunityFittingsUpdated
    sess_a.role = service_roles::ROLE_CONTENT;
    let deleted = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "DeleteFitting",
        &[
            PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
            PyRep::Integer(comm_fid),
        ],
    )
    .await
    .expect("Content DeleteFitting");
    assert_eq!(deleted, PyRep::Bool(true));

    let b_del_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let b_comm = find_notifs(&b_del_notifs, "OnCommunityFittingsUpdated");
    assert_eq!(b_comm.len(), 1);
    assert!(b_comm[0].args.is_empty());
}

#[tokio::test]
async fn test_on_tutorial_skipped_air_npe_and_operations() {
    // parity: notify:OnTutorialSkipped
    eve_testkit::attest(&["notify:OnTutorialSkipped"]);

    let ctx = ServerContext::new();
    let mut sess_a = login_pilot(&ctx, 8804, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = login_pilot(&ctx, 8805, "other", "other-secret", PILOT_B).await;
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // Negative 1: unauthenticated skip_air_npe fails and emits 0 OnTutorialSkipped
    let mut unauth = SessionState::new(8898);
    assert!(rpc(&ctx, &mut unauth, "air_npe", "skip_air_npe", &[]).await.is_err());
    assert!(
        find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnTutorialSkipped").is_empty()
    );

    // Negative 2: non-skip client event does not emit OnTutorialSkipped
    rpc(
        &ctx,
        &mut sess_a,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("start_tutorial".into())],
    )
    .await
    .expect("start_tutorial");
    assert!(
        find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnTutorialSkipped").is_empty()
    );

    // Positive 1: air_npe.skip_air_npe marks tutorial completed and emits 0-arg OnTutorialSkipped
    let skip_rep = rpc(&ctx, &mut sess_a, "air_npe", "skip_air_npe", &[])
        .await
        .expect("air_npe.skip_air_npe");
    assert_eq!(skip_rep.dict_get("completed"), Some(&PyRep::Bool(true)));
    let state_rep = rpc(&ctx, &mut sess_a, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("air_npe.get_air_npe_state");
    assert_eq!(state_rep.dict_get("completed"), Some(&PyRep::Bool(true)));

    let a_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let a_skipped = find_notifs(&a_notifs, "OnTutorialSkipped");
    assert_eq!(a_skipped.len(), 1);
    assert_eq!(a_skipped[0].id_type, "charid");
    assert!(a_skipped[0].args.is_empty());

    // Positive 2: operationsManager.process_client_event("skip_tutorial") also emits 0-arg OnTutorialSkipped
    rpc(
        &ctx,
        &mut sess_b,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .await
    .expect("operationsManager skip_tutorial");
    let b_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let b_skipped = find_notifs(&b_notifs, "OnTutorialSkipped");
    assert_eq!(b_skipped.len(), 1);
    assert_eq!(b_skipped[0].id_type, "charid");
    assert!(b_skipped[0].args.is_empty());
}
