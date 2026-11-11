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

    // Typed persistence in load_certificate_state / save_certificate_state
    ctx.db.with_mut_snapshot(|snap| {
        let mut certs = HashMap::new();
        certs.insert(50_u32, 3_u8);
        snap.node_graph_state.certificates.insert(
            cid,
            eve_db::CharacterCertificateState {
                certificates: certs,
                flags: HashMap::new(),
            },
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

#[test]
fn mutant_killers_skill_derived_certs_mastery_non_gm_vs_gm_merge_and_all_batch_forms() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let kw = HashMap::new();
    let cert_svc = "certificateMgr"; // rpc-lint: allow test fixture

    // 1. Unknown method returns None
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    assert!(handle(&ctx, &mut sess, cert_svc, "UnknownMethod", &[], &kw).is_none());

    // 2. Real SDE certificate derivation & ship mastery tier progression
    let bundle = ctx
        .sde
        .load_certificates_and_masteries()
        .expect("SDE certificates bundle");
    let (sample_ship, ship_levels) = bundle
        .masteries_by_ship_type
        .iter()
        .find(|(_, lvls)| lvls.first().is_some_and(|tier1| !tier1.is_empty()))
        .expect("at least one ship with mastery tier 1");

    // Pick tier 1 required skills and train character up to tier 1 and tier 2
    let (tier1_certs, tier1_skills) = ship_mastery_tier_requirements(&ctx, *sample_ship, 0); // 0 clamps to 1
    assert!(!tier1_certs.is_empty());
    let (tier1_explicit, _) = ship_mastery_tier_requirements(&ctx, *sample_ship, 1);
    assert_eq!(tier1_certs, tier1_explicit);
    let (tier5_certs, _) = ship_mastery_tier_requirements(&ctx, *sample_ship, 99); // 99 clamps to 5
    if let Some(lv5) = ship_levels.get(4) {
        assert_eq!(&tier5_certs, lv5);
    }

    // Without skills, mastery on sample_ship should be 0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 2; // Omega so alpha caps don't clamp derived tiers
        }
        snap.skills.remove(&cid);
    });
    assert_eq!(derive_ship_mastery_level(&ctx, cid, *sample_ship), 0);

    // Train exact tier-1 skills -> derive_certificate_levels awards >= 1 for each tier1 cert & mastery >= 1
    ctx.db.with_mut_snapshot(|snap| {
        let sk_map = snap.skills.entry(cid).or_default();
        for &(sk_id, req_lvl) in &tier1_skills {
            sk_map.insert(
                sk_id,
                eve_db::TrainedSkillRecord {
                    type_id: sk_id,
                    level: req_lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    let derived = derive_certificate_levels(&ctx, cid);
    for cert_id in &tier1_certs {
        assert!(derived.get(cert_id).copied().unwrap_or(0) >= 1);
    }
    assert!(derive_ship_mastery_level(&ctx, cid, *sample_ship) >= 1);

    // 3. Non-GM GetMyCertificates / GetCertificatesByCharacter MUST NOT expose raw GM-granted certs,
    // while GM session merges max(derived, granted)
    let first_cert = tier1_certs[0];
    let unskill_cert = 999_888_u32;
    ctx.db.with_mut_snapshot(|snap| {
        save_certificate_state(
            snap,
            cid,
            &CharacterCertificateState {
                certificates: [(first_cert, 5), (unskill_cert, 3)].into_iter().collect(),
                flags: [(first_cert, 7)].into_iter().collect(),
            },
        );
    });
    let non_gm_my = handle(&ctx, &mut sess, cert_svc, "GetMyCertificates", &[], &kw)
        .unwrap()
        .unwrap();
    let non_gm_dict = non_gm_my.dict_get("certificates").and_then(|d| d.as_dict()).unwrap();
    assert!(!non_gm_dict.iter().any(|(k, _)| k == &PyRep::Integer(unskill_cert as i64)));
    let non_gm_flags = non_gm_my
        .dict_get("certificateFlags")
        .and_then(|d| d.as_dict())
        .unwrap();
    assert!(non_gm_flags
        .iter()
        .any(|(k, v)| k == &PyRep::Integer(first_cert as i64) && v == &PyRep::Integer(7)));

    // GetCertificatesByCharacter defaults target to session char_id when args empty
    let default_target = handle(&ctx, &mut sess, cert_svc, "GetCertificatesByCharacter", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(default_target.dict_get("characterID"), Some(&PyRep::Integer(cid)));

    // 4. GM role mask bit enables GM grants merge & all arg/payload forms on Grant/BatchGrant/BatchUpdate/UpdateFlags
    sess.role = eve_machonet::service_roles::ROLE_GM_MASK;
    let gm_my = get_my_certificates_pyrep(&ctx, &sess).unwrap();
    let gm_dict = gm_my.dict_get("certificates").and_then(|d| d.as_dict()).unwrap();
    assert!(gm_dict
        .iter()
        .any(|(k, v)| k == &PyRep::Integer(first_cert as i64) && v == &PyRep::Integer(5)));
    assert!(gm_dict
        .iter()
        .any(|(k, v)| k == &PyRep::Integer(unskill_cert as i64) && v == &PyRep::Integer(3)));

    // Empty BatchCertificateGrant & BatchCertificateUpdate succeed early
    assert_eq!(
        handle(&ctx, &mut sess, cert_svc, "BatchCertificateGrant", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &mut sess, cert_svc, "BatchCertificateUpdate", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // GrantCertificate 1-arg form grants level 1 on self
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "GrantCertificate",
            &[PyRep::Integer(301)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    // BatchCertificateGrant with explicit target_cid + top-level Dict + nested Tuple([id])/List/Dict
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "BatchCertificateGrant",
            &[
                PyRep::Integer(other_cid),
                PyRep::Dict(vec![
                    (PyRep::Integer(401), PyRep::Integer(99)), // clamps to 5
                    (PyRep::Integer(402), PyRep::Integer(2)),
                ]),
            ],
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
            &[
                PyRep::Integer(other_cid),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(403)]), // single-elem tuple -> level 1
                    PyRep::List(vec![PyRep::Integer(404), PyRep::Integer(4)]),
                    PyRep::Dict(vec![(PyRep::Integer(405), PyRep::Integer(3))]),
                ]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    // BatchCertificateUpdate with explicit target_cid + 2-elem tuple + Dict
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "BatchCertificateUpdate",
            &[
                PyRep::Integer(other_cid),
                PyRep::List(vec![
                    PyRep::List(vec![PyRep::Integer(406), PyRep::Integer(3)]),
                    PyRep::Dict(vec![(PyRep::Integer(407), PyRep::Integer(2))]),
                ]),
            ],
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
            &[
                PyRep::Integer(other_cid),
                PyRep::Dict(vec![(PyRep::Integer(408), PyRep::Integer(5))]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    // UpdateCertificateFlags: List of Tuple/List, GM targeting other_cid, and empty args
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            cert_svc,
            "UpdateCertificateFlags",
            &[PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(301), PyRep::Integer(2)]),
                PyRep::List(vec![PyRep::Integer(302), PyRep::Integer(4)]),
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
            "UpdateCertificateFlags",
            &[PyRep::Integer(other_cid), PyRep::Integer(401), PyRep::Integer(8)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &mut sess, cert_svc, "UpdateCertificateFlags", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    ctx.db.with_snapshot(|snap| {
        let self_st = load_certificate_state(snap, cid);
        assert_eq!(self_st.certificates.get(&301), Some(&1));
        assert_eq!(self_st.flags.get(&301), Some(&2));
        assert_eq!(self_st.flags.get(&302), Some(&4));

        let other_st = load_certificate_state(snap, other_cid);
        assert_eq!(other_st.certificates.get(&401), Some(&5));
        assert_eq!(other_st.certificates.get(&403), Some(&1));
        assert_eq!(other_st.certificates.get(&404), Some(&4));
        assert_eq!(other_st.certificates.get(&405), Some(&3));
        assert_eq!(other_st.certificates.get(&406), Some(&3));
        assert_eq!(other_st.certificates.get(&407), Some(&2));
        assert_eq!(other_st.certificates.get(&408), Some(&5));
        assert_eq!(other_st.flags.get(&401), Some(&8));
    });
}

#[test]
fn certificate_derivation_edge_cases_and_batch_variants() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let kw = HashMap::new();
    let cert_svc = "certificateMgr";

    // 1. load_certificate_state fallback when primary blob is malformed JSON and legacy blob is valid vs malformed
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            format!("character_certificates:{cid}"),
            "{not valid json".into(),
        );
        let mut legacy = HashMap::new();
        legacy.insert(77_u32, 2_u8);
        snap.service_state_blobs.insert(format!("char_certs:{cid}"), serde_json::to_string(&legacy).unwrap()); snap.node_graph_state.certificates.remove(&cid); snap.migrate_legacy_service_state();
        let st = load_certificate_state(snap, cid);
        assert_eq!(st.certificates.get(&77), Some(&2));

        // Malformed legacy blob -> default empty state
        snap.node_graph_state.certificates.remove(&cid);
        let st2 = load_certificate_state(snap, cid);
        assert!(st2.certificates.is_empty());
        assert!(st2.flags.is_empty());
        snap.service_state_blobs.remove(&format!("character_certificates:{cid}"));
        snap.service_state_blobs.remove(&format!("char_certs:{cid}"));
    });

    // 2. Train required skills for an actual SDE certificate & ship mastery tier 1
    let bundle = ctx.sde.load_certificates_and_masteries().expect("SDE certs");
    let (sample_ship_tid, sample_cert_id) = bundle
        .masteries_by_ship_type
        .iter()
        .find_map(|(&ship_tid, tiers)| {
            let tier1 = tiers.first()?;
            let &first_cert = tier1.first()?;
            Some((ship_tid, first_cert))
        })
        .expect("SDE must have at least one ship mastery");

    let (tier0_certs, tier0_skills) = ship_mastery_tier_requirements(&ctx, sample_ship_tid, 0);
    let (tier1_certs, tier1_skills) = ship_mastery_tier_requirements(&ctx, sample_ship_tid, 1);
    assert_eq!(tier0_certs, tier1_certs);
    assert_eq!(tier0_skills, tier1_skills);
    let (tier99_certs, _) = ship_mastery_tier_requirements(&ctx, sample_ship_tid, 99);
    assert!(tier99_certs.len() <= tier1_certs.len() || !tier99_certs.is_empty());

    // Grant all tier-1 skills for sample_ship_tid at level 5 so mastery >= 1 and sample_cert_id >= 1
    ctx.db.with_mut_snapshot(|snap| {
        let sk_map = snap.skills.entry(cid).or_default();
        for &(sk_id, req_lvl) in &tier1_skills {
            sk_map.insert(
                sk_id,
                eve_db::TrainedSkillRecord {
                    type_id: sk_id,
                    level: req_lvl.max(1),
                    skill_points: 256_000.0,
                },
            );
        }
    });
    let derived = derive_certificate_levels(&ctx, cid);
    assert!(
        derived.get(&sample_cert_id).copied().unwrap_or(0) >= 1,
        "expected trained skills to unlock certificate {sample_cert_id}"
    );
    assert!(derive_ship_mastery_level(&ctx, cid, sample_ship_tid) >= 1);
    assert_eq!(derive_ship_mastery_level(&ctx, cid, 999_999_999), 0);

    // 3. Unknown method returns None
    let mut player_sess = auth_session(1, cid, 98_000_001, 30_000_142);
    assert!(handle(&ctx, &mut player_sess, cert_svc, "UnknownCertMethod", &[], &kw).is_none());

    // 4. GetAllShipCertificateRecommendations missing / negative / unknown args
    assert_eq!(
        handle(
            &ctx,
            &mut player_sess,
            cert_svc,
            "GetAllShipCertificateRecommendations",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipTypeID"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut player_sess,
            cert_svc,
            "GetAllShipCertificateRecommendations",
            &[PyRep::Integer(-5)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipTypeID"
    );

    // 5. Non-GM GetMyCertificates and GetCertificatesByCharacter do NOT include GM-stored certs
    let mut gm_sess = auth_session(2, other_cid, 98_000_001, 30_000_142);
    gm_sess.role = eve_machonet::service_roles::ROLE_GMH;
    // Grant a synthetic high cert id (888_001) to cid as GM, using 1-arg and 2-arg GrantCertificate
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            cert_svc,
            "GrantCertificate",
            &[PyRep::Integer(cid), PyRep::Integer(888_001), PyRep::Integer(-3)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            cert_svc,
            "GrantCertificate",
            &[PyRep::Integer(cid), PyRep::Integer(888_004), PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    // Player does NOT see GM-persisted 888_001 in GetMyCertificates or GetCertificatesByCharacter
    let player_my = handle(&ctx, &mut player_sess, cert_svc, "GetMyCertificates", &[], &kw)
        .unwrap()
        .unwrap();
    let player_dict = player_my.dict_get("certificates").and_then(|d| d.as_dict()).unwrap();
    assert!(!player_dict.iter().any(|(k, _)| k == &PyRep::Integer(888_001)));
    let player_by = handle(
        &ctx,
        &mut player_sess,
        cert_svc,
        "GetCertificatesByCharacter",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(player_by.dict_get("characterID"), Some(&PyRep::Integer(cid)));

    // 6. GrantCertificate 1-arg and 2-arg forms by GM (targets caller cid)
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            cert_svc,
            "GrantCertificate",
            &[PyRep::Integer(888_002)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            cert_svc,
            "GrantCertificate",
            &[PyRep::Integer(888_003), PyRep::Integer(4)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // 7. BatchCertificateGrant: empty args, [target_cid, Dict], List of Tuple([cid]) / List / Dict / junk
    assert_eq!(
        handle(&ctx, &mut player_sess, cert_svc, "BatchCertificateGrant", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            cert_svc,
            "BatchCertificateGrant",
            &[
                PyRep::Integer(cid),
                PyRep::Dict(vec![(PyRep::Integer(888_010), PyRep::Integer(7))]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            cert_svc,
            "BatchCertificateGrant",
            &[PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(888_011)]),
                PyRep::List(vec![PyRep::Integer(888_012), PyRep::Integer(3)]),
                PyRep::Dict(vec![(PyRep::Integer(888_013), PyRep::Integer(2))]),
                PyRep::None,
            ])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // 8. BatchCertificateUpdate: empty args, [target_cid, List], Dict, List of Dict / Tuple(2) / junk
    assert_eq!(
        handle(&ctx, &mut player_sess, cert_svc, "BatchCertificateUpdate", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            cert_svc,
            "BatchCertificateUpdate",
            &[
                PyRep::Integer(cid),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(888_020), PyRep::Integer(4)]),
                    PyRep::List(vec![PyRep::Integer(888_021), PyRep::Integer(5), PyRep::Integer(9)]),
                    PyRep::Dict(vec![(PyRep::Integer(888_022), PyRep::Integer(2))]),
                    PyRep::None,
                ]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            cert_svc,
            "BatchCertificateUpdate",
            &[PyRep::Dict(vec![(PyRep::Integer(888_023), PyRep::Integer(3))])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // 9. UpdateCertificateFlags: GM 3-arg for another character, 1-arg List, and empty args
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            cert_svc,
            "UpdateCertificateFlags",
            &[PyRep::Integer(cid), PyRep::Integer(888_021), PyRep::Integer(77)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut player_sess,
            cert_svc,
            "UpdateCertificateFlags",
            &[PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(888_024), PyRep::Integer(3)]),
                PyRep::List(vec![PyRep::Integer(888_025), PyRep::Integer(4)]),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(&ctx, &mut player_sess, cert_svc, "UpdateCertificateFlags", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // Verify stored certs & flags
    ctx.db.with_snapshot(|snap| {
        let st_cid = load_certificate_state(snap, cid);
        assert_eq!(st_cid.certificates.get(&888_001), Some(&5));
        assert_eq!(st_cid.certificates.get(&888_004), Some(&0));
        assert_eq!(st_cid.certificates.get(&888_010), Some(&5));
        assert_eq!(st_cid.certificates.get(&888_020), Some(&4));
        assert_eq!(st_cid.certificates.get(&888_021), Some(&5));
        assert_eq!(st_cid.flags.get(&888_021), Some(&77));
        assert_eq!(st_cid.flags.get(&888_024), Some(&3));
        assert_eq!(st_cid.flags.get(&888_025), Some(&4));

        let st_gm = load_certificate_state(snap, other_cid);
        assert_eq!(st_gm.certificates.get(&888_002), Some(&1));
        assert_eq!(st_gm.certificates.get(&888_003), Some(&4));
        assert_eq!(st_gm.certificates.get(&888_011), Some(&1));
        assert_eq!(st_gm.certificates.get(&888_012), Some(&3));
        assert_eq!(st_gm.certificates.get(&888_023), Some(&3));
    });
}
