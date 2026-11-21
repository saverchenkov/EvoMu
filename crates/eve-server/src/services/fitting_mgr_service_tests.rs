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

#[tokio::test]
async fn corp_and_alliance_fittings_enforce_npc_rejection_uninserted_rbac_members_and_db_first() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let fit_kv = PyRep::keyval([
        ("name", PyRep::String("RBAC Guard Fit".into())),
        ("shipTypeID", PyRep::Integer(587)),
        ("description", PyRep::String("regression".into())),
        ("fitData", PyRep::List(vec![])),
    ]);

    let npc_corp_id = 1_000_009_i64;
    let uninserted_corp_id = 98_777_881_i64;
    let ally_uninserted_corp_id = 98_777_882_i64;
    let seeded_corp_id = 98_777_883_i64;
    let stale_corp_id = 98_777_884_i64;
    let alliance_id = 99_777_882_i64;
    let stale_alliance_id = 99_777_884_i64;

    let npc_char = 91_777_881_i64;
    let uninserted_char = 91_777_882_i64;
    let ally_uninserted_char = 91_777_883_i64;
    let seeded_ceo = 91_777_884_i64;
    let omitted_char = 91_777_885_i64;
    let db_char = 91_777_886_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            npc_char,
            eve_db::CharacterRecord {
                character_id: npc_char,
                corporation_id: npc_corp_id,
                alliance_id: Some(alliance_id),
                ..Default::default()
            },
        );
        snap.characters.insert(
            uninserted_char,
            eve_db::CharacterRecord {
                character_id: uninserted_char,
                corporation_id: uninserted_corp_id,
                ..Default::default()
            },
        );
        snap.characters.insert(
            ally_uninserted_char,
            eve_db::CharacterRecord {
                character_id: ally_uninserted_char,
                corporation_id: ally_uninserted_corp_id,
                alliance_id: Some(alliance_id),
                ..Default::default()
            },
        );
        snap.characters.insert(
            omitted_char,
            eve_db::CharacterRecord {
                character_id: omitted_char,
                corporation_id: seeded_corp_id,
                alliance_id: Some(alliance_id),
                ..Default::default()
            },
        );
        snap.characters.insert(
            db_char,
            eve_db::CharacterRecord {
                character_id: db_char,
                corporation_id: seeded_corp_id,
                alliance_id: Some(alliance_id),
                ..Default::default()
            },
        );

        let mut corp = CorporationRecord::default();
        corp.corporation_id = seeded_corp_id;
        corp.name = "Fitting Members Corp".into();
        corp.ticker = "FMBR".into();
        corp.ceo_id = seeded_ceo;
        corp.alliance_id = Some(alliance_id);
        corp.members = std::collections::HashSet::from([seeded_ceo, db_char]);
        corp.member_roles
            .insert(omitted_char, eve_db::corp_roles::ROLE_FITTING_MANAGER);
        corp.member_roles
            .insert(db_char, eve_db::corp_roles::ROLE_FITTING_MANAGER);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(seeded_corp_id, corp);

        let mut stale_corp = CorporationRecord::default();
        stale_corp.corporation_id = stale_corp_id;
        stale_corp.name = "Stale Fitting Corp".into();
        stale_corp.ticker = "SFIT".into();
        stale_corp.ceo_id = db_char;
        stale_corp.alliance_id = Some(stale_alliance_id);
        stale_corp.members = std::collections::HashSet::from([db_char]);
        stale_corp.ensure_wallet_divisions();
        snap.corporations.insert(stale_corp_id, stale_corp);

        snap.alliances.insert(
            alliance_id,
            eve_db::AllianceRecord {
                alliance_id,
                name: "Fitting Guard Alliance".into(),
                short_name: "FGAL".into(),
                executor_corp_id: seeded_corp_id,
                member_corp_ids: std::collections::HashSet::from([
                    seeded_corp_id,
                    ally_uninserted_corp_id,
                ]),
            },
        );
        snap.alliances.insert(
            stale_alliance_id,
            eve_db::AllianceRecord {
                alliance_id: stale_alliance_id,
                name: "Stale Alliance".into(),
                short_name: "STAL".into(),
                executor_corp_id: stale_corp_id,
                member_corp_ids: std::collections::HashSet::from([stale_corp_id]),
            },
        );
    });

    // 1. NPC corporations allow read-only `corpFittingMgr.GetFittings(own_npc_corp)` returning `{}`
    // without touching/exposing DB fittings, while ALL mutations, foreign queries, and
    // `allianceFittingMgr` stay fail-closed even with max corp_role; curated community fittings
    // (1_000_282) remain gated strictly on GM/Content role for writes.
    let mut npc_sess = SessionState::new(91_801);
    npc_sess.user_id = Some(1);
    npc_sess.character_id = Some(npc_char);
    npc_sess.corporation_id = Some(npc_corp_id);
    npc_sess.alliance_id = Some(alliance_id);
    npc_sess.role = eve_machonet::service_roles::ROLE_PLAYER;
    npc_sess.corp_role = u64::MAX;

    // Seed a synthetic fitting under npc_corp_id to prove the NPC GetFittings fast-path
    // does not expose DB entries under NPC corporation IDs.
    let synthetic_npc_fit_id = 91_777_990_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            synthetic_npc_fit_id,
            eve_db::SavedFittingRecord {
                fitting_id: synthetic_npc_fit_id,
                owner_id: npc_corp_id,
                name: "Should Never Expose".into(),
                ship_type_id: 587,
                description: "npc-guard".into(),
                items: Vec::new(),
            },
        );
    });

    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut npc_sess,
            None,
            "corpFittingMgr",
            "GetFittings",
            &[PyRep::Integer(npc_corp_id)],
            &kw,
        )
        .await
        .unwrap()
        .expect("NPC corp GetFittings(own_npc_corp) succeeds with empty dict"),
        PyRep::Dict(Vec::new())
    );
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut npc_sess,
            None,
            "corpFittingMgr",
            "GetFittings",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .expect("NPC corp GetFittings() with default owner succeeds with empty dict"),
        PyRep::Dict(Vec::new())
    );
    // Clean up synthetic row and confirm non-empty/foreign queries stay rejected.
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.remove(&synthetic_npc_fit_id);
    });
    for foreign_oid in [seeded_corp_id, 1_000_170_i64, COMMUNITY_FITTING_CORP_ID, npc_char] {
        let mismatch_err = handle_fitting_mgr(
            &ctx,
            &mut npc_sess,
            None,
            "corpFittingMgr",
            "GetFittings",
            &[PyRep::Integer(foreign_oid)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(mismatch_err.code, "UserError");
        assert_eq!(mismatch_err.message, "FittingOwnerMismatch");
    }

    for (svc, method, args) in [
        (
            "corpFittingMgr",
            "SaveFitting",
            vec![PyRep::Integer(npc_corp_id), fit_kv.clone()],
        ),
        (
            "corpFittingMgr",
            "SaveManyFittings",
            vec![PyRep::Integer(npc_corp_id), PyRep::List(vec![fit_kv.clone()])],
        ),
        (
            "corpFittingMgr",
            "UpdateNameAndDescription",
            vec![
                PyRep::Integer(9_999),
                PyRep::Integer(npc_corp_id),
                PyRep::String("Edited".into()),
                PyRep::String("Desc".into()),
            ],
        ),
        (
            "corpFittingMgr",
            "UpdateFitting",
            vec![
                PyRep::Integer(9_999),
                PyRep::Integer(npc_corp_id),
                PyRep::String("Edited".into()),
                PyRep::String("Desc".into()),
            ],
        ),
        (
            "corpFittingMgr",
            "UpdateFitting",
            vec![
                PyRep::Integer(9_999),
                PyRep::String("Edited".into()),
                PyRep::String("Desc".into()),
            ],
        ),
        (
            "corpFittingMgr",
            "DeleteFitting",
            vec![PyRep::Integer(npc_corp_id), PyRep::Integer(9_999)],
        ),
        (
            "corpFittingMgr",
            "DeleteManyFittings",
            vec![
                PyRep::Integer(npc_corp_id),
                PyRep::List(vec![PyRep::Integer(9_999)]),
            ],
        ),
        (
            "allianceFittingMgr",
            "GetFittings",
            vec![PyRep::Integer(alliance_id)],
        ),
        (
            "allianceFittingMgr",
            "SaveFitting",
            vec![PyRep::Integer(alliance_id), fit_kv.clone()],
        ),
        (
            "allianceFittingMgr",
            "UpdateFitting",
            vec![
                PyRep::Integer(alliance_id),
                PyRep::Integer(9_999),
                PyRep::String("Edited".into()),
                PyRep::String("Desc".into()),
            ],
        ),
        (
            "allianceFittingMgr",
            "DeleteFitting",
            vec![PyRep::Integer(alliance_id), PyRep::Integer(9_999)],
        ),
    ] {
        assert_eq!(
            handle_fitting_mgr(&ctx, &mut npc_sess, None, svc, method, &args, &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "CorpRoleDenied",
            "{svc}.{method} must fail closed with CorpRoleDenied for NPC corporations"
        );
    }

    // Even if an NPC corp record exists in snap.corporations with explicit membership excluding the caller,
    // `corpFittingMgr.GetFittings` still fails closed with CorpRoleDenied.
    ctx.db.with_mut_snapshot(|snap| {
        let mut npc_rec = CorporationRecord::default();
        npc_rec.corporation_id = npc_corp_id;
        npc_rec.ceo_id = seeded_ceo;
        npc_rec.members.insert(seeded_ceo);
        snap.corporations.insert(npc_corp_id, npc_rec);
    });
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut npc_sess,
            None,
            "corpFittingMgr",
            "GetFittings",
            &[PyRep::Integer(npc_corp_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.remove(&npc_corp_id);
    });

    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut npc_sess,
            None,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(1_000_282), fit_kv.clone()],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    npc_sess.role |= eve_machonet::service_roles::ROLE_CONTENT;
    assert!(
        handle_fitting_mgr(
            &ctx,
            &mut npc_sess,
            None,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(1_000_282), fit_kv.clone()],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );

    // 2. Uninserted player corp (>= 98M) and uninserted member corp inside inserted alliance fail closed
    // even with session.corp_role = ROLE_FITTING_MANAGER; unregistered alliance also fails closed.
    let mut uninserted_sess = SessionState::new(91_802);
    uninserted_sess.user_id = Some(2);
    uninserted_sess.character_id = Some(uninserted_char);
    uninserted_sess.corporation_id = Some(uninserted_corp_id);
    uninserted_sess.corp_role = eve_db::corp_roles::ROLE_FITTING_MANAGER;
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut uninserted_sess,
            None,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(uninserted_corp_id), fit_kv.clone()],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut uninserted_sess,
            None,
            "corpFittingMgr",
            "GetFittings",
            &[PyRep::Integer(uninserted_corp_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    let mut ally_uninserted_sess = SessionState::new(91_803);
    ally_uninserted_sess.user_id = Some(3);
    ally_uninserted_sess.character_id = Some(ally_uninserted_char);
    ally_uninserted_sess.corporation_id = Some(ally_uninserted_corp_id);
    ally_uninserted_sess.alliance_id = Some(alliance_id);
    ally_uninserted_sess.corp_role = eve_db::corp_roles::ROLE_FITTING_MANAGER;
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut ally_uninserted_sess,
            None,
            "allianceFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(alliance_id), fit_kv.clone()],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle_fitting_mgr(
            &ctx,
            &mut ally_uninserted_sess,
            None,
            "allianceFittingMgr",
            "GetFittings",
            &[PyRep::Integer(alliance_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Seeded player corp claiming an unregistered alliance ID rejects allianceFittingMgr with NotInAlliance.
    let unreg_ally_id = 99_777_899_i64;
    let unreg_ally_corp_id = 98_777_899_i64;
    let unreg_ally_ceo = 91_777_899_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut uc = CorporationRecord::default();
        uc.corporation_id = unreg_ally_corp_id;
        uc.name = "Unreg Ally Corp".into();
        uc.ticker = "UAC".into();
        uc.ceo_id = unreg_ally_ceo;
        uc.alliance_id = Some(unreg_ally_id);
        uc.members.insert(unreg_ally_ceo);
        uc.ensure_wallet_divisions();
        snap.corporations.insert(unreg_ally_corp_id, uc);
        snap.characters.insert(
            unreg_ally_ceo,
            eve_db::CharacterRecord {
                character_id: unreg_ally_ceo,
                corporation_id: unreg_ally_corp_id,
                alliance_id: Some(unreg_ally_id),
                ..Default::default()
            },
        );
    });
    let mut unreg_ally_sess = SessionState::new(91_806);
    unreg_ally_sess.user_id = Some(6);
    unreg_ally_sess.character_id = Some(unreg_ally_ceo);
    unreg_ally_sess.corporation_id = Some(unreg_ally_corp_id);
    unreg_ally_sess.alliance_id = Some(unreg_ally_id);
    let unreg_ally_err = handle_fitting_mgr(
        &ctx,
        &mut unreg_ally_sess,
        None,
        "allianceFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(unreg_ally_id), fit_kv.clone()],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(unreg_ally_err.code, "UserError");
    assert_eq!(unreg_ally_err.message, "NotInAlliance");
    let unreg_ally_get_err = handle_fitting_mgr(
        &ctx,
        &mut unreg_ally_sess,
        None,
        "allianceFittingMgr",
        "GetFittings",
        &[PyRep::Integer(unreg_ally_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(unreg_ally_get_err.code, "UserError");
    assert_eq!(unreg_ally_get_err.message, "NotInAlliance");

    // 3. Character omitted from non-empty corp.members is denied on corp & alliance FittingMgr even with ROLE_FITTING_MANAGER.
    let mut omitted_sess = SessionState::new(91_804);
    omitted_sess.user_id = Some(4);
    omitted_sess.character_id = Some(omitted_char);
    omitted_sess.corporation_id = Some(seeded_corp_id);
    omitted_sess.alliance_id = Some(alliance_id);
    omitted_sess.corp_role = eve_db::corp_roles::ROLE_FITTING_MANAGER;
    for (svc, oid, method, args) in [
        (
            "corpFittingMgr",
            seeded_corp_id,
            "SaveFitting",
            vec![PyRep::Integer(seeded_corp_id), fit_kv.clone()],
        ),
        ("corpFittingMgr", seeded_corp_id, "GetFittings", vec![PyRep::Integer(seeded_corp_id)]),
        (
            "allianceFittingMgr",
            alliance_id,
            "SaveFitting",
            vec![PyRep::Integer(alliance_id), fit_kv.clone()],
        ),
        ("allianceFittingMgr", alliance_id, "GetFittings", vec![PyRep::Integer(alliance_id)]),
    ] {
        let _ = oid;
        assert_eq!(
            handle_fitting_mgr(&ctx, &mut omitted_sess, None, svc, method, &args, &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "CorpRoleDenied",
            "{svc}.{method} must deny character omitted from non-empty corp.members"
        );
    }

    // 4. Stale session.corporation_id / session.alliance_id cannot override DB character membership.
    let mut stale_sess = SessionState::new(91_805);
    stale_sess.user_id = Some(5);
    stale_sess.character_id = Some(db_char);
    stale_sess.corporation_id = Some(stale_corp_id);
    stale_sess.alliance_id = Some(stale_alliance_id);
    stale_sess.corp_role = 0;

    let stale_corp_err = handle_fitting_mgr(
        &ctx,
        &mut stale_sess,
        None,
        "corpFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(stale_corp_id), fit_kv.clone()],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(stale_corp_err.code, "UserError");
    assert_eq!(stale_corp_err.message, "FittingOwnerMismatch");
    let stale_ally_err = handle_fitting_mgr(
        &ctx,
        &mut stale_sess,
        None,
        "allianceFittingMgr",
        "GetFittings",
        &[PyRep::Integer(stale_alliance_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(stale_ally_err.code, "UserError");
    assert_eq!(stale_ally_err.message, "FittingOwnerMismatch");
    let mut stale_npc_sess = stale_sess.clone();
    stale_npc_sess.corporation_id = Some(npc_corp_id);
    let stale_npc_err = handle_fitting_mgr(
        &ctx,
        &mut stale_npc_sess,
        None,
        "corpFittingMgr",
        "GetFittings",
        &[PyRep::Integer(npc_corp_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(stale_npc_err.code, "UserError");
    assert_eq!(stale_npc_err.message, "FittingOwnerMismatch");
    assert!(
        handle_fitting_mgr(
            &ctx,
            &mut stale_sess,
            None,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(seeded_corp_id), fit_kv.clone()],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    assert!(
        handle_fitting_mgr(
            &ctx,
            &mut stale_sess,
            None,
            "allianceFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(alliance_id), fit_kv],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );

    // 5. Player corporation with no alliance fails closed on allianceFittingMgr.GetFittings.
    let no_ally_corp_id = 98_777_901_i64;
    let no_ally_char = 91_777_901_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut nac = CorporationRecord::default();
        nac.corporation_id = no_ally_corp_id;
        nac.name = "Solo Corp".into();
        nac.ticker = "SOLO".into();
        nac.ceo_id = no_ally_char;
        nac.alliance_id = None;
        nac.members.insert(no_ally_char);
        nac.ensure_wallet_divisions();
        snap.corporations.insert(no_ally_corp_id, nac);
        snap.characters.insert(
            no_ally_char,
            eve_db::CharacterRecord {
                character_id: no_ally_char,
                corporation_id: no_ally_corp_id,
                alliance_id: None,
                ..Default::default()
            },
        );
    });
    let mut no_ally_sess = SessionState::new(91_807);
    no_ally_sess.user_id = Some(7);
    no_ally_sess.character_id = Some(no_ally_char);
    no_ally_sess.corporation_id = Some(no_ally_corp_id);
    no_ally_sess.alliance_id = None;
    let no_ally_err = handle_fitting_mgr(
        &ctx,
        &mut no_ally_sess,
        None,
        "allianceFittingMgr",
        "GetFittings",
        &[PyRep::Integer(alliance_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_ally_err.code, "UserError");
    assert_eq!(no_ally_err.message, "FittingOwnerMismatch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_npc_corp_player_fitting_window_startup_sequence_succeeds_without_gm_bits() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let npc_corp_id = 1_000_170_i64; // Republic Military School (seeded demo char's NPC corp)
    let jita_station_id = 60_003_760_i64;
    let personal_fid = 950_001_i64;
    let community_fid = 950_002_i64;

    ctx.db.with_snapshot(|snap| {
        let demo_acc = &snap.accounts["demo"];
        assert_eq!(demo_acc.role, eve_db::service_roles::ROLE_PLAYER_DEFAULT);
        assert_eq!(demo_acc.role & eve_db::service_roles::ROLE_GM_MASK, 0);
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.corporation_id, npc_corp_id);
        assert_eq!(ch.alliance_id, None);
        assert_eq!(ch.station_id, Some(jita_station_id));
        assert!(!snap.corporations.contains_key(&npc_corp_id));
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            personal_fid,
            eve_db::SavedFittingRecord {
                fitting_id: personal_fid,
                owner_id: char_id,
                name: "Demo Personal Rifter".into(),
                ship_type_id: 587,
                description: "Starter fit".into(),
                items: vec![(439, InventoryFlag::MedSlot0, 1)],
            },
        );
        snap.saved_fittings.insert(
            community_fid,
            eve_db::SavedFittingRecord {
                fitting_id: community_fid,
                owner_id: COMMUNITY_FITTING_CORP_ID,
                name: "Community Starter Rifter".into(),
                ship_type_id: 587,
                description: "CCP community fit".into(),
                items: vec![(439, InventoryFlag::MedSlot0, 1)],
            },
        );
    });
    let expected_fitting_count = ctx.db.with_snapshot(|snap| snap.saved_fittings.len());
    let kw = HashMap::new();

    for role in [eve_machonet::service_roles::ROLE_PLAYER_DEFAULT, 0_u64] {
        assert_eq!(role & eve_machonet::service_roles::ROLE_GM_MASK, 0);
        let mut sess = SessionState::new(91_900 + role);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(npc_corp_id);
        sess.alliance_id = None;
        sess.solar_system_id = Some(30_000_142);
        sess.station_id = Some(jita_station_id);
        sess.ship_id = Some(100_000_001);
        sess.role = role;
        sess.corp_role = 0;

        // V24.02 Fitting window startup sequence (`fittingSvc.GetAllFittingsForCharacter`):
        // 1) charFittingMgr.GetFittings(char_id)
        let char_fits = ctx
            .dispatch_rpc(
                &mut sess,
                "charFittingMgr",
                None,
                "GetFittings",
                &[PyRep::Integer(char_id)],
                &kw,
                None,
            )
            .await
            .expect("charFittingMgr.GetFittings must succeed for ordinary NPC-corp player");
        let PyRep::Dict(char_entries) = char_fits else {
            panic!("expected Dict from charFittingMgr.GetFittings");
        };
        assert_eq!(char_entries.len(), 1);
        assert_eq!(char_entries[0].0, PyRep::Integer(personal_fid));
        assert_eq!(
            char_entries[0].1.dict_get("name").and_then(|v| v.as_str()),
            Some("Demo Personal Rifter")
        );

        // 2) corpFittingMgr.GetFittings(npc_corp_id) -> empty dict `{}` (never CorpRoleDenied)
        let corp_fits = ctx
            .dispatch_rpc(
                &mut sess,
                "corpFittingMgr",
                None,
                "GetFittings",
                &[PyRep::Integer(npc_corp_id)],
                &kw,
                None,
            )
            .await
            .expect("corpFittingMgr.GetFittings(npc_corp_id) must succeed for ordinary NPC-corp player");
        assert_eq!(corp_fits, PyRep::Dict(Vec::new()));

        // 3) corpFittingMgr.GetCommunityFittings()
        let community_fits = ctx
            .dispatch_rpc(
                &mut sess,
                "corpFittingMgr",
                None,
                "GetCommunityFittings",
                &[],
                &kw,
                None,
            )
            .await
            .expect("corpFittingMgr.GetCommunityFittings must succeed for ordinary NPC-corp player");
        let PyRep::Dict(community_entries) = community_fits else {
            panic!("expected Dict from corpFittingMgr.GetCommunityFittings");
        };
        assert_eq!(community_entries.len(), 1);
        assert_eq!(community_entries[0].0, PyRep::Integer(community_fid));
        assert_eq!(
            community_entries[0].1.dict_get("name").and_then(|v| v.as_str()),
            Some("Community Starter Rifter")
        );

        // Cross-corp / community-corp GetFittings and NPC-corp mutations remain denied.
        for foreign_corp in [98_000_001_i64, COMMUNITY_FITTING_CORP_ID] {
            let err = ctx
                .dispatch_rpc(
                    &mut sess,
                    "corpFittingMgr",
                    None,
                    "GetFittings",
                    &[PyRep::Integer(foreign_corp)],
                    &kw,
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(err.code, "UserError");
            assert_eq!(err.message, "FittingOwnerMismatch");
        }
        let save_err = ctx
            .dispatch_rpc(
                &mut sess,
                "corpFittingMgr",
                None,
                "SaveFitting",
                &[
                    PyRep::Integer(npc_corp_id),
                    PyRep::keyval([
                        ("name", PyRep::String("Denied Fit".into())),
                        ("shipTypeID", PyRep::Integer(587)),
                        ("description", PyRep::String("should fail".into())),
                        ("fitData", PyRep::List(vec![])),
                    ]),
                ],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(save_err.code, "CorpRoleDenied");
    }

    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.saved_fittings.len()),
        expected_fitting_count
    );
}

#[cfg(test)]
#[path = "fitting_mgr_service_extra_unit_tests.rs"]
mod fitting_mgr_service_extra_unit_tests;
