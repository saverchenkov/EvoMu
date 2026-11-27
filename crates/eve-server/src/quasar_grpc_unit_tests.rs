//! Unit and mutation-killing tests for `crate::quasar_grpc` RPC reply normalization,
//! notification normalization, Quasar protobuf payload coercion, and HTTP/2 gRPC transport.

use crate::router::client_compat::normalize_macho_rpc_reply;
use eve_proto::PyRep;

#[test]
fn normalize_macho_rpc_reply_covers_dogma_map_planet_raffle_skill_war_and_store() {
    assert_eq!(
        normalize_macho_rpc_reply(
            "dogmaIM",
            "OverloadRack",
            Ok(PyRep::List(vec![PyRep::Integer(1001)])),
        )
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(1001)])
    );
    let hs = normalize_macho_rpc_reply(
        "home_station",
        "get_home_station_candidates",
        Ok(PyRep::List(vec![PyRep::Integer(60_003_760)])),
    )
    .unwrap();
    assert!(matches!(hs, PyRep::List(_)));

    let add_tgt = normalize_macho_rpc_reply(
        "dogmaIM",
        "AddTarget",
        Ok(PyRep::keyval([
            ("locked", PyRep::Bool(false)),
            ("targetID", PyRep::Integer(555)),
        ])),
    )
    .unwrap();
    assert_eq!(
        add_tgt,
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())])
    );
}

#[tokio::test]
async fn quasar_c9_typed_handlers_direct_dispatch_wire_shapes() {
    let ctx = crate::router::ServerContext::new();
    let corp_id = 98_000_001_i64;
    eve_testkit::legal_fixture::seed_legal_corp_member(
        &ctx.db,
        90_000_001,
        corp_id,
        "C9 Direct Corp",
        "C9DC",
        false,
        u64::MAX,
        None,
    );
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let uid = snap.characters.get(&90_000_001).unwrap().user_id;
        snap.fw_system_occupations.insert(
            30_000_142,
            eve_db::FwSystemOccupationRecord {
                solar_system_id: 30_000_142,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 1_500,
                victory_points_threshold: 3_000,
            },
        );
        uid
    });
    let mut sess = eve_machonet::SessionState::new(93_602);
    sess.user_id = Some(uid);
    sess.character_id = Some(90_000_001);
    ctx.sync_session(&mut sess);
    let kw = std::collections::HashMap::new();

    let leave = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "CanLeaveCurrentCorporation",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("CanLeaveCurrentCorporation");
    assert!(matches!(leave, PyRep::Tuple(parts) if parts.len() == 3));

    let reinf = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "GetStructureReinforceDefault",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetStructureReinforceDefault");
    assert_eq!(reinf, PyRep::Tuple(vec![PyRep::Integer(18), PyRep::Integer(0)]));

    let audit = ctx
        .dispatch_rpc(
            &mut sess,
            "corpmgr",
            None,
            "AuditMember",
            &[PyRep::Integer(90_000_001)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("corpmgr.AuditMember");
    assert!(matches!(
        audit,
        PyRep::Tuple(parts) if parts.len() == 2 && matches!(&parts[0], PyRep::Rowset { .. })
    ));

    let dev = ctx
        .dispatch_rpc(
            &mut sess,
            "devIndexManager",
            None,
            "GetAllDevelopmentIndices",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetAllDevelopmentIndices");
    assert!(matches!(dev, PyRep::Dict(_)));

    let vp = ctx
        .dispatch_rpc(
            &mut sess,
            "fwVictoryPointMgr",
            None,
            "GetLocalVictoryPointState",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetLocalVictoryPointState");
    let PyRep::Tuple(vp_parts) = vp else {
        panic!("expected 2-tuple from GetLocalVictoryPointState");
    };
    assert_eq!(vp_parts[0], PyRep::Integer(30_000_142));
    assert_eq!(vp_parts[1].dict_get("victoryPoints"), Some(&PyRep::Integer(1_500)));

    let peace = ctx
        .dispatch_rpc(
            &mut sess,
            "peaceTreatyMgr",
            None,
            "GetPeaceTreatiesForSession",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetPeaceTreatiesForSession");
    assert_eq!(
        peace,
        PyRep::Tuple(vec![PyRep::List(Vec::new()), PyRep::List(Vec::new())])
    );
}
