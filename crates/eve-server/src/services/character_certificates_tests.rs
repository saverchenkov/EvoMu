//! Unit tests for `character_certificates` (`certificateMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn certificate_state_derivation_mastery_and_rpc_dispatch() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let kw = HashMap::new();

    // Legacy fallback in load_certificate_state
    ctx.db.with_mut_snapshot(|snap| {
        let mut legacy = HashMap::new();
        legacy.insert(50_u32, 3_u8);
        snap.service_state_blobs.insert(
            format!("char_certs:{cid}"),
            serde_json::to_string(&legacy).unwrap(),
        );
        let loaded = load_certificate_state(snap, cid);
        assert_eq!(loaded.certificates.get(&50), Some(&3));
        save_certificate_state(snap, cid, &loaded);
        let loaded2 = load_certificate_state(snap, cid);
        assert_eq!(loaded2.certificates.get(&50), Some(&3));
    });

    // Mastery & tier requirements on Rifter (587)
    let _certs = derive_certificate_levels(&ctx, cid);
    let mastery = derive_ship_mastery_level(&ctx, cid, 587);
    assert!(mastery <= 5);
    let (tier_certs, tier_skills) = ship_mastery_tier_requirements(&ctx, 587, 1);
    assert!(!tier_certs.is_empty() || tier_skills.is_empty());
    let (empty_certs, _) = ship_mastery_tier_requirements(&ctx, 999_999_999, 1);
    assert!(empty_certs.is_empty());

    // Unauthenticated & wrong service
    let mut unauth = SessionState::default();
    let other_svc = "other";
    let cert_svc_owned = format!("certificate{}", "Mgr");
    let cert_svc = cert_svc_owned.as_str();
    assert!(handle(&ctx, &mut unauth, other_svc, "GetMyCertificates", &[], &kw).is_none());
    assert!(handle(&ctx, &mut unauth, cert_svc, "GetMyCertificates", &[], &kw)
        .unwrap()
        .is_err());

    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);

    // GetCertificateCategories, GetCertificateClasses, GetAllShipCertificateRecommendations
    let cats = handle(&ctx, &mut sess, cert_svc, "GetCertificateCategories", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(cats.as_list().is_some());
    let classes = handle(&ctx, &mut sess, cert_svc, "GetCertificateClasses", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(classes.as_list().is_some());

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "GetAllShipCertificateRecommendations",
            &[PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipTypeID"
    );
    let recs = handle(
        &ctx,
        &mut sess,
        cert_svc,
        "GetAllShipCertificateRecommendations",
        &[PyRep::Integer(587)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(recs.dict_get("shipTypeID"), Some(&PyRep::Integer(587)));

    // Non-GM cannot grant or batch update certificates, nor update another character's flags
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "GrantCertificate",
            &[PyRep::Integer(100), PyRep::Integer(4)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "BatchCertificateGrant",
            &[PyRep::List(vec![PyRep::Integer(101)])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "BatchCertificateUpdate",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(101),
                PyRep::Integer(3),
            ])])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "UpdateCertificateFlags",
            &[
                PyRep::Integer(other_cid),
                PyRep::Integer(100),
                PyRep::Integer(1),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // Non-GM CAN update own flags (2-arg, Dict, and List forms)
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "UpdateCertificateFlags",
            &[PyRep::Integer(100), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "UpdateCertificateFlags",
            &[PyRep::Dict(vec![(PyRep::Integer(101), PyRep::Integer(2))])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // GM grants and batch updates
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "GrantCertificate",
            &[PyRep::Integer(cid), PyRep::Integer(200), PyRep::Integer(9)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "BatchCertificateGrant",
            &[PyRep::List(vec![
                PyRep::Integer(201),
                PyRep::Tuple(vec![PyRep::Integer(202), PyRep::Integer(4)]),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "BatchCertificateUpdate",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(203),
                PyRep::Integer(5),
                PyRep::Integer(1),
            ])])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    let my_certs = handle(&ctx, &mut sess, cert_svc, "GetMyCertificates", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(my_certs.dict_get("characterID"), Some(&PyRep::Integer(cid)));

    let by_char = handle(
        &ctx,
        &mut sess,
        cert_svc,
        "GetCertificatesByCharacter",
        &[PyRep::Integer(cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let certs_dict = by_char.dict_get("certificates").and_then(|d| d.as_dict()).unwrap();
    assert!(certs_dict.iter().any(|(k, v)| k == &PyRep::Integer(200) && v == &PyRep::Integer(5)));
    assert!(certs_dict.iter().any(|(k, v)| k == &PyRep::Integer(202) && v == &PyRep::Integer(4)));
}
