#![forbid(unsafe_code)]

use super::*;
use eve_db::{CorporationRecord, InventoryFlag, VirtualServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn char_corp_alliance_and_community_fittings_save_get_update_delete_lifecycle() {
    let ctx = test_ctx();
    let ceo_id = 95_600_001;
    let member_id = 95_600_002;
    let corp_id = 98_600_001;
    let alliance_id = 99_600_001;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        for cid in [ceo_id, member_id] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            snap.characters.insert(cid, ch);
            snap.accounts.insert(
                format!("user_{cid}"),
                eve_db::AccountRecord {
                    user_id: cid as u64,
                    username: format!("user_{cid}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Fitting Corp".into();
                __corp.ticker = "FIT".into();
                __corp.ceo_id = ceo_id;
                __corp },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(ceo_id as u64, ceo_id as u64, Some(ceo_id), true, ctx.now_unix_secs());

    let kw = HashMap::new();
    let mut ceo_sess = SessionState {
        user_id: Some(ceo_id as u64),
        character_id: Some(ceo_id),
        corporation_id: Some(corp_id),
        alliance_id: Some(alliance_id),
        ..Default::default()
    };
    let mut member_sess = SessionState {
        user_id: Some(member_id as u64),
        character_id: Some(member_id),
        corporation_id: Some(corp_id),
        alliance_id: Some(alliance_id),
        ..Default::default()
    };

    let fit_obj = PyRep::keyval([
        ("name", PyRep::String("Solo Rifter".into())),
        ("shipTypeID", PyRep::Integer(587)),
        ("description", PyRep::String("Fast tackle".into())),
        (
            "fitData",
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(439),
                PyRep::Integer(InventoryFlag::MedSlot0 as i64),
                PyRep::Integer(1),
            ])]),
        ),
    ]);

    // 1. charFittingMgr.SaveFitting -> GetFittings -> UpdateNameAndDescription -> DeleteFitting
    let saved_char = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "charFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(ceo_id), fit_obj.clone()],
        &kw,
    )
    .await
    .unwrap()
    .expect("SaveFitting char");
    let fid = saved_char.as_i64().unwrap();
    assert!(fid > 0);

    let char_fits = handle_fitting_mgr(&ctx, &mut ceo_sess, None, "charFittingMgr", "GetFittings", &[PyRep::Integer(ceo_id)], &kw)
        .await
        .unwrap()
        .expect("GetFittings char");
    assert!(matches!(char_fits, PyRep::Dict(ref d) if d.len() == 1));

    let upd = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "charFittingMgr",
        "UpdateNameAndDescription",
        &[
            PyRep::Integer(fid),
            PyRep::Integer(ceo_id),
            PyRep::String("Updated Rifter".into()),
            PyRep::String("Updated desc".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("UpdateNameAndDescription");
    assert_eq!(upd.dict_get("name").and_then(|v| v.as_str()), Some("Updated Rifter"));

    let del = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "charFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(ceo_id), PyRep::Integer(fid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("DeleteFitting");
    assert_eq!(del, PyRep::Bool(true));

    // 2. corpFittingMgr role check: unprivileged member gets CorpRoleDenied; CEO succeeds with SaveManyFittings & DeleteManyFittings
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut member_sess,
            None,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(corp_id), fit_obj.clone()],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    let saved_many = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "corpFittingMgr",
        "SaveManyFittings",
        &[PyRep::Integer(corp_id), PyRep::List(vec![fit_obj.clone()])],
        &kw,
    )
    .await
    .unwrap()
    .expect("SaveManyFittings corp");
    let corp_fid = match saved_many {
        PyRep::List(ref v) => v[0].as_i64().unwrap(),
        _ => panic!("expected list of fitting IDs"),
    };

    let del_many = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "corpFittingMgr",
        "DeleteManyFittings",
        &[PyRep::Integer(corp_id), PyRep::List(vec![PyRep::Integer(corp_fid)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("DeleteManyFittings corp");
    assert_eq!(del_many, PyRep::List(vec![PyRep::Integer(corp_fid)]));

    // 3. Community fittings (1000282): denied without GM/Content role, allowed with GM role
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut ceo_sess,
            None,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(1_000_282), fit_obj.clone()],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    ceo_sess.role = 0x7FFF_FFFF_FFFF_FFFF;
    let comm_fid = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "corpFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(1_000_282), fit_obj],
        &kw,
    )
    .await
    .unwrap()
    .expect("GM SaveFitting community")
    .as_i64()
    .unwrap();
    let comm_fits = handle_fitting_mgr(
        &ctx,
        &mut member_sess,
        None,
        "corpFittingMgr",
        "GetCommunityFittings",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetCommunityFittings");
    assert!(matches!(comm_fits, PyRep::Dict(ref d) if d.iter().any(|(k, _)| k.as_i64() == Some(comm_fid))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_fitting_rbac_ownership_and_malformed_list_errors() {
    let ctx = test_ctx();
    let ceo_id = 95_601_001_i64;
    let fit_mgr_id = 95_601_002_i64;
    let director_id = 95_601_003_i64;
    let plain_id = 95_601_004_i64;
    let outsider_id = 95_601_005_i64;
    let corp_id = 98_601_001_i64;
    let other_corp_id = 98_601_002_i64;
    let alliance_id = 99_601_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        for (cid, coid, aid) in [
            (ceo_id, corp_id, Some(alliance_id)),
            (fit_mgr_id, corp_id, Some(alliance_id)),
            (director_id, corp_id, Some(alliance_id)),
            (plain_id, corp_id, Some(alliance_id)),
            (outsider_id, other_corp_id, None),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = coid;
            ch.alliance_id = aid;
            snap.characters.insert(cid, ch);
        }

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Fitting RBAC Corp".into();
        corp.ticker = "FRBC".into();
        corp.ceo_id = ceo_id;
        corp.alliance_id = Some(alliance_id);
        corp.members
            .extend([ceo_id, fit_mgr_id, director_id, plain_id]);
        corp.member_roles
            .insert(fit_mgr_id, eve_db::corp_roles::ROLE_FITTING_MANAGER);
        corp.member_roles
            .insert(director_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);

        let mut other_corp = CorporationRecord::default();
        other_corp.corporation_id = other_corp_id;
        other_corp.name = "Foreign Corp".into();
        other_corp.ticker = "FRGN".into();
        other_corp.ceo_id = outsider_id;
        other_corp.members.insert(outsider_id);
        snap.corporations.insert(other_corp_id, other_corp);

        let mut ally = eve_db::AllianceRecord::default();
        ally.alliance_id = alliance_id;
        ally.name = "Fitting Alliance".into();
        ally.short_name = "FALY".into();
        ally.executor_corp_id = corp_id;
        ally.member_corp_ids.insert(corp_id);
        snap.alliances.insert(alliance_id, ally);
    });

    let kw = HashMap::new();
    let make_sess = |cid: i64, coid: i64, aid: Option<i64>, corp_role: u64| SessionState {
        user_id: Some(cid as u64),
        character_id: Some(cid),
        corporation_id: Some(coid),
        alliance_id: aid,
        corp_role,
        ..Default::default()
    };
    let mut ceo_sess = make_sess(ceo_id, corp_id, Some(alliance_id), 0);
    let mut fit_mgr_sess = make_sess(
        fit_mgr_id,
        corp_id,
        Some(alliance_id),
        eve_db::corp_roles::ROLE_FITTING_MANAGER,
    );
    let mut director_sess = make_sess(
        director_id,
        corp_id,
        Some(alliance_id),
        eve_db::corp_roles::ROLE_DIRECTOR,
    );
    let mut plain_sess = make_sess(plain_id, corp_id, Some(alliance_id), 0);
    let mut outsider_sess = make_sess(outsider_id, other_corp_id, None, 0);

    let fit_kv = PyRep::keyval([
        ("name", PyRep::String("RBAC Rifter".into())),
        ("shipTypeID", PyRep::Integer(587)),
        ("description", PyRep::String("RBAC fit".into())),
        (
            "fitData",
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(3170),
                PyRep::Integer(InventoryFlag::HiSlot0 as i32 as i64),
                PyRep::Integer(1),
            ])]),
        ),
    ]);

    let ceo_char_fid = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "charFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(ceo_id), fit_kv.clone()],
        &kw,
    )
    .await
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();
    let plain_char_fid = handle_fitting_mgr(
        &ctx,
        &mut plain_sess,
        None,
        "charFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(plain_id), fit_kv.clone()],
        &kw,
    )
    .await
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();

    let mut corp_fids = Vec::new();
    for _ in 0..3 {
        let fid = handle_fitting_mgr(
            &ctx,
            &mut ceo_sess,
            None,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(corp_id), fit_kv.clone()],
            &kw,
        )
        .await
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
        corp_fids.push(fid);
    }
    let ally_fid_1 = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "allianceFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(alliance_id), fit_kv.clone()],
        &kw,
    )
    .await
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();
    let ally_fid_2 = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "allianceFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(alliance_id), fit_kv],
        &kw,
    )
    .await
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();

    // Negative 1: cross-owner DeleteFitting must return FittingOwnershipViolation (not Ok(false))
    let err_char_del = handle_fitting_mgr(
        &ctx,
        &mut plain_sess,
        None,
        "charFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(ceo_char_fid)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("foreign char DeleteFitting must fail");
    assert_eq!(err_char_del.code, "FittingOwnershipViolation");

    // Negative 2: malformed DeleteManyFittings (non-list argument) must error on all 3 services
    for (svc, arg) in [
        ("charFittingMgr", ceo_char_fid),
        ("corpFittingMgr", corp_id),
        ("allianceFittingMgr", ceo_id),
    ] {
        let err = handle_fitting_mgr(
            &ctx,
            &mut ceo_sess,
            None,
            svc,
            "DeleteManyFittings",
            &[PyRep::Integer(arg)],
            &kw,
        )
        .await
        .unwrap()
        .expect_err("non-list DeleteManyFittings must error");
        assert!(err.code == "UserError" || err.code == "CorpRoleDenied");
    }

    // Negative 3: atomic DeleteManyFittings rejects mixed own+foreign list without partial delete
    let err_mixed = handle_fitting_mgr(
        &ctx,
        &mut plain_sess,
        None,
        "charFittingMgr",
        "DeleteManyFittings",
        &[PyRep::List(vec![
            PyRep::Integer(plain_char_fid),
            PyRep::Integer(ceo_char_fid),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("mixed DeleteManyFittings must reject foreign fitting");
    assert_eq!(err_mixed.code, "FittingOwnershipViolation");
    assert!(ctx
        .db
        .with_snapshot(|s| s.saved_fittings.contains_key(&plain_char_fid)
            && s.saved_fittings.contains_key(&ceo_char_fid)));

    // Negative 4: roleless corp member denied DeleteFitting & DeleteManyFittings on corp & alliance
    for (svc, oid, fid) in [
        ("corpFittingMgr", corp_id, corp_fids[0]),
        ("allianceFittingMgr", alliance_id, ally_fid_1),
    ] {
        let e1 = handle_fitting_mgr(
            &ctx,
            &mut plain_sess,
            None,
            svc,
            "DeleteFitting",
            &[PyRep::Integer(oid), PyRep::Integer(fid)],
            &kw,
        )
        .await
        .unwrap()
        .expect_err("roleless DeleteFitting denied");
        assert_eq!(e1.code, "CorpRoleDenied");

        let e2 = handle_fitting_mgr(
            &ctx,
            &mut plain_sess,
            None,
            svc,
            "DeleteManyFittings",
            &[PyRep::Integer(oid), PyRep::List(vec![PyRep::Integer(fid)])],
            &kw,
        )
        .await
        .unwrap()
        .expect_err("roleless DeleteManyFittings denied");
        assert_eq!(e2.code, "CorpRoleDenied");
    }

    // Negative 5: unaligned caller denied allianceFittingMgr.DeleteFitting/DeleteManyFittings
    let e_unaligned = handle_fitting_mgr(
        &ctx,
        &mut outsider_sess,
        None,
        "allianceFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(outsider_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("unaligned caller cannot delete alliance fittings");
    assert_eq!(e_unaligned.code, "UserError");

    // Negative 6: 1-arg corpFittingMgr.DeleteFitting([corp_id]) must error instead of Ok(false)
    let e_corp_id_only = handle_fitting_mgr(
        &ctx,
        &mut outsider_sess,
        None,
        "corpFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(corp_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("1-arg foreign corp ID must fail");
    assert_eq!(e_corp_id_only.code, "UserError");

    let e_self_corp_id_only = handle_fitting_mgr(
        &ctx,
        &mut ceo_sess,
        None,
        "corpFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(corp_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("1-arg own corp ID without fittingID must fail");
    assert_eq!(e_self_corp_id_only.code, "UserError");

    // Positive: ROLE_FITTING_MANAGER (1 << 59) deletes corp & alliance fits; missing id still returns false
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut fit_mgr_sess,
            None,
            "corpFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(corp_id), PyRep::Integer(corp_fids[0])],
            &kw,
        )
        .await
        .unwrap()
        .expect("ROLE_FITTING_MANAGER corp DeleteFitting"),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut fit_mgr_sess,
            None,
            "corpFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(corp_id), PyRep::Integer(999_999)],
            &kw,
        )
        .await
        .unwrap()
        .expect("authorized missing DeleteFitting returns false"),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut fit_mgr_sess,
            None,
            "allianceFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(alliance_id), PyRep::Integer(ally_fid_1)],
            &kw,
        )
        .await
        .unwrap()
        .expect("ROLE_FITTING_MANAGER alliance DeleteFitting"),
        PyRep::Bool(true)
    );

    // Positive: ROLE_DIRECTOR bulk-deletes corp fittings and deletes alliance fitting
    let dir_bulk = handle_fitting_mgr(
        &ctx,
        &mut director_sess,
        None,
        "corpFittingMgr",
        "DeleteManyFittings",
        &[
            PyRep::Integer(corp_id),
            PyRep::List(vec![
                PyRep::Integer(corp_fids[1]),
                PyRep::Integer(corp_fids[2]),
            ]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("ROLE_DIRECTOR corp DeleteManyFittings");
    assert_eq!(
        dir_bulk,
        PyRep::List(vec![
            PyRep::Integer(corp_fids[1]),
            PyRep::Integer(corp_fids[2]),
        ])
    );
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut director_sess,
            None,
            "allianceFittingMgr",
            "DeleteFitting",
            &[PyRep::Integer(alliance_id), PyRep::Integer(ally_fid_2)],
            &kw,
        )
        .await
        .unwrap()
        .expect("ROLE_DIRECTOR alliance DeleteFitting"),
        PyRep::Bool(true)
    );
}

#[cfg(test)]
#[path = "fitting_mgr_service_extra_unit_tests.rs"]
mod fitting_mgr_service_extra_unit_tests;
