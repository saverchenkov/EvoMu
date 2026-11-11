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

#[cfg(test)]
#[path = "fitting_mgr_service_extra_unit_tests.rs"]
mod fitting_mgr_service_extra_unit_tests;
