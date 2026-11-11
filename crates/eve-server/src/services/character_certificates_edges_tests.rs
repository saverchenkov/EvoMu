//! Edge-case unit tests for `character_certificates` (`certificateMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::TrainedSkillRecord;
use eve_machonet::service_roles;
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn corrupt_state_and_real_skill_derivation_mastery_edges() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let kw = HashMap::new();
    let cert_svc_owned = format!("certificate{}", "Mgr");
    let cert_svc = cert_svc_owned.as_str();

    // Corrupt JSON in both primary and legacy keys falls back to default empty state.
    ctx.db.with_mut_snapshot(|snap| {
        snap.node_graph_state.certificates.remove(&cid);
        snap.service_state_blobs
            .insert(format!("char_certs:{cid}"), "{bad_legacy".into());
        let st = load_certificate_state(snap, cid);
        assert!(st.certificates.is_empty());
        assert!(st.flags.is_empty());
        snap.node_graph_state.certificates.remove(&cid);
        snap.service_state_blobs.remove(&format!("char_certs:{cid}"));
    });

    // Find a ship with SDE mastery tier 1 & tier 2 and grant skills for tier 1 only
    // so derive_certificate_levels and derive_ship_mastery_level cover non-zero levels and break branch.
    let bundle = ctx.sde.load_certificates_and_masteries().expect("bundle");
    let (&ship_tid, _) = bundle
        .masteries_by_ship_type
        .iter()
        .find(|(_, lvls)| {
            lvls.len() >= 2
                && !lvls[0].is_empty()
                && lvls[1].iter().any(|c_id| {
                    bundle
                        .certificates
                        .get(c_id)
                        .is_some_and(|c| c.skill_types.values().any(|r| r[1] > r[0]))
                })
        })
        .expect("ship with multi-tier masteries");

    let (_tier1_certs, tier1_skills) = ship_mastery_tier_requirements(&ctx, ship_tid, 1);
    assert!(!tier1_skills.is_empty());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 2; // Omega so alpha cap does not truncate
        }
        let sk_map = snap.skills.entry(cid).or_default();
        sk_map.clear();
        for &(sk_id, req_lvl) in &tier1_skills {
            sk_map.insert(
                sk_id,
                TrainedSkillRecord {
                    type_id: sk_id,
                    level: req_lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let derived = derive_certificate_levels(&ctx, cid);
    assert!(!derived.is_empty());
    let mastery = derive_ship_mastery_level(&ctx, cid, ship_tid);
    assert_eq!(mastery, 1);
    assert_eq!(derive_ship_mastery_level(&ctx, cid, 999_999_999), 0);

    // Non-GM GetMyCertificates / GetCertificatesByCharacter ignores GM-granted blob certs
    // while GM session merges max(derived, granted).
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = CharacterCertificateState::default();
        st.certificates.insert(99_001, 4);
        st.flags.insert(99_001, 7);
        save_certificate_state(snap, cid, &st);
    });

    let mut player = auth_session(1, cid, 98_000_001, 30_000_142);
    let my_player = handle(&ctx, &mut player, cert_svc, "GetMyCertificates", &[], &kw)
        .unwrap()
        .unwrap();
    let player_dict = my_player.dict_get("certificates").and_then(|d| d.as_dict()).unwrap();
    assert!(!player_dict.iter().any(|(k, _)| k == &PyRep::Integer(99_001)));

    // Empty args in GetCertificatesByCharacter defaults to caller char_id.
    let by_char_default = handle(&ctx, &mut player, cert_svc, "GetCertificatesByCharacter", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(by_char_default.dict_get("characterID"), Some(&PyRep::Integer(cid)));

    let mut gm = auth_session(1, cid, 98_000_001, 30_000_142);
    gm.role = service_roles::ROLE_GMH;
    let my_gm = handle(&ctx, &mut gm, cert_svc, "GetMyCertificates", &[], &kw)
        .unwrap()
        .unwrap();
    let gm_dict = my_gm.dict_get("certificates").and_then(|d| d.as_dict()).unwrap();
    assert!(gm_dict
        .iter()
        .any(|(k, v)| k == &PyRep::Integer(99_001) && v == &PyRep::Integer(4)));
}

#[test]
fn rpc_arg_shapes_validation_and_unknown_method_edges() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let kw = HashMap::new();
    let cert_svc_owned = format!("certificate{}", "Mgr");
    let cert_svc = cert_svc_owned.as_str();

    let mut gm = auth_session(1, cid, 98_000_001, 30_000_142);
    gm.role = service_roles::ROLE_GMH;

    // Unknown method returns None.
    assert!(handle(&ctx, &mut gm, cert_svc, "UnknownMethod", &[], &kw).is_none());

    // GetAllShipCertificateRecommendations error cases (missing, negative, unknown ship).
    for bad_args in [
        vec![],
        vec![PyRep::Integer(-5)],
        vec![PyRep::String("not_int".into())],
        vec![PyRep::Integer(999_999_999)],
    ] {
        assert_eq!(
            handle(&ctx, &mut gm, cert_svc, "GetAllShipCertificateRecommendations", &bad_args, &kw)
                .unwrap()
                .unwrap_err()
                .code,
            "InvalidShipTypeID"
        );
    }

    // GrantCertificate 1-arg and 2-arg forms.
    assert_eq!(
        handle(&ctx, &mut gm, cert_svc, "GrantCertificate", &[PyRep::Integer(301)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "GrantCertificate",
            &[PyRep::Integer(302), PyRep::Integer(3)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // BatchCertificateGrant: empty, [target_cid, List(List, Dict, unknown)], top-level Dict, unknown top-level.
    assert_eq!(
        handle(&ctx, &mut gm, cert_svc, "BatchCertificateGrant", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "BatchCertificateGrant",
            &[
                PyRep::Integer(other_cid),
                PyRep::List(vec![
                    PyRep::List(vec![PyRep::Integer(310)]),
                    PyRep::Dict(vec![(PyRep::Integer(311), PyRep::Integer(7))]),
                    PyRep::None,
                ]),
            ],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "BatchCertificateGrant",
            &[PyRep::Dict(vec![(PyRep::Integer(312), PyRep::Integer(2))])],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "BatchCertificateGrant",
            &[PyRep::String("ignored".into())],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // BatchCertificateUpdate: empty, [target_cid, List(Tuple2, List3, Dict, unknown)], top-level Dict, unknown.
    assert_eq!(
        handle(&ctx, &mut gm, cert_svc, "BatchCertificateUpdate", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "BatchCertificateUpdate",
            &[
                PyRep::Integer(other_cid),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(320), PyRep::Integer(2)]),
                    PyRep::List(vec![PyRep::Integer(321), PyRep::Integer(4), PyRep::Integer(9)]),
                    PyRep::Dict(vec![(PyRep::Integer(322), PyRep::Integer(3))]),
                    PyRep::None,
                ]),
            ],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "BatchCertificateUpdate",
            &[PyRep::Dict(vec![(PyRep::Integer(323), PyRep::Integer(5))])],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "BatchCertificateUpdate",
            &[PyRep::None],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // UpdateCertificateFlags: 3-arg GM for other_cid, 1-arg List(Tuple/List), empty args, unknown arg.
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "UpdateCertificateFlags",
            &[PyRep::Integer(other_cid), PyRep::Integer(320), PyRep::Integer(11)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "UpdateCertificateFlags",
            &[PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(301), PyRep::Integer(4)]),
                PyRep::List(vec![PyRep::Integer(302), PyRep::Integer(5)]),
            ])],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &mut gm, cert_svc, "UpdateCertificateFlags", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            cert_svc,
            "UpdateCertificateFlags",
            &[PyRep::Integer(999)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    ctx.db.with_snapshot(|snap| {
        let st_self = load_certificate_state(snap, cid);
        assert_eq!(st_self.certificates.get(&301), Some(&1));
        assert_eq!(st_self.certificates.get(&302), Some(&3));
        assert_eq!(st_self.certificates.get(&312), Some(&2));
        assert_eq!(st_self.certificates.get(&323), Some(&5));
        assert_eq!(st_self.flags.get(&301), Some(&4));
        assert_eq!(st_self.flags.get(&302), Some(&5));

        let st_other = load_certificate_state(snap, other_cid);
        assert_eq!(st_other.certificates.get(&310), Some(&1));
        assert_eq!(st_other.certificates.get(&311), Some(&5));
        assert_eq!(st_other.certificates.get(&320), Some(&2));
        assert_eq!(st_other.certificates.get(&321), Some(&4));
        assert_eq!(st_other.certificates.get(&322), Some(&3));
        assert_eq!(st_other.flags.get(&321), Some(&9));
        assert_eq!(st_other.flags.get(&320), Some(&11));
    });
}

#[test]
fn save_and_load_certificate_state_roundtrip_direct_mutants() {
    let ctx = test_ctx();
    let cid = 90_000_077_i64;
    let other_cid = 90_000_078_i64;
    let mut state = CharacterCertificateState::default();
    state.certificates.insert(401, 4);
    state.flags.insert(401, 7);

    ctx.db.with_mut_snapshot(|snap| {
        save_certificate_state(snap, cid, &state);
    });
    ctx.db.with_snapshot(|snap| {
        let loaded = load_certificate_state(snap, cid);
        assert_eq!(loaded.certificates.get(&401), Some(&4));
        assert_eq!(loaded.flags.get(&401), Some(&7));
        let empty = load_certificate_state(snap, other_cid);
        assert!(empty.certificates.is_empty());
        assert!(empty.flags.is_empty());
    });
}
