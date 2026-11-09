//! Semantic unit tests split from `machonet_protocol`.

use eve_machonet::{
    extract_macho_call_req, service_roles, MachoPacketType, SessionState, VersionExchange,
    HANDSHAKE_SECRET_KEY, SESSION_TYPE_GAME, V24_BOOT_BUILD,
    V24_MACHO_VERSION,
};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn test_version_exchange_parsing_and_validation() {
    let srv = VersionExchange::default();
    assert_eq!(srv.birthday, HANDSHAKE_SECRET_KEY);
    assert_eq!(srv.macho_version, V24_MACHO_VERSION);
    assert_eq!(srv.build_version, V24_BOOT_BUILD);
    assert_eq!(srv.codename(), "V24.01");
    assert_eq!(srv.region(), "ccp");

    let no_at = VersionExchange {
        project_version: "TRANQUILITY".to_string(),
        ..srv.clone()
    };
    assert_eq!(no_at.codename(), "TRANQUILITY");
    assert_eq!(no_at.region(), "");

    let hello = srv.to_server_hello_pyrep();
    let PyRep::Tuple(ref hello_items) = hello else {
        panic!("expected tuple");
    };
    assert_eq!(hello_items.len(), 7);
    assert_eq!(hello_items[6], PyRep::None);
    assert_eq!(VersionExchange::from_pyrep(&hello), Some(srv.clone()));

    // Integer version_number and None user_count accepted
    let int_ver = PyRep::Tuple(vec![
        PyRep::Integer(HANDSHAKE_SECRET_KEY as i64),
        PyRep::Integer(V24_MACHO_VERSION as i64),
        PyRep::None,
        PyRep::Integer(24),
        PyRep::Integer(V24_BOOT_BUILD as i64),
        PyRep::ascii_bytes("24.0@ccp"),
    ]);
    let parsed_int = VersionExchange::from_pyrep(&int_ver).expect("int version");
    assert_eq!(parsed_int.user_count, 0);
    assert_eq!(parsed_int.version_number, 24.0);

    // Invalid lengths
    assert!(VersionExchange::from_pyrep(&PyRep::Tuple(vec![PyRep::Integer(1)])).is_none());

    // Newer build accepted, within epsilon version accepted
    let newer_client = VersionExchange {
        build_version: V24_BOOT_BUILD + 50,
        version_number: srv.version_number + 0.00005,
        ..srv.clone()
    };
    assert!(srv.validate_client(&newer_client).is_ok());

    let mut bad_bday = srv.clone();
    bad_bday.birthday = 1;
    assert_eq!(
        srv.validate_client(&bad_bday).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLERELEASE")
    );

    let mut bad_proto = srv.clone();
    bad_proto.macho_version = 495;
    assert_eq!(
        srv.validate_client(&bad_proto).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
    );

    let mut bad_ver = srv.clone();
    bad_ver.version_number = 23.0;
    assert_eq!(
        srv.validate_client(&bad_ver).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEVERSION")
    );

    let mut old_build = srv.clone();
    old_build.build_version = V24_BOOT_BUILD - 1;
    assert_eq!(
        srv.validate_client(&old_build).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEBUILD")
    );

    let mut bad_region = srv.clone();
    bad_region.project_version = "V24.01@serenity".to_string();
    assert_eq!(
        srv.validate_client(&bad_region).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEREGION")
    );

    let mut bad_codename = srv.clone();
    bad_codename.project_version = "V23.02@ccp".to_string();
    assert_eq!(
        srv.validate_client(&bad_codename).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLERELEASE")
    );
}

#[test]
fn test_extract_macho_call_req_variants() {
    // 1. Standard V24 service call: (0, SubStream((1, SubStream(("GetTime", (10, 20), {"k": "v"})))))
    let call_3 = PyRep::Tuple(vec![
        PyRep::ascii_bytes("GetTime"),
        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(20)]),
        PyRep::Dict(vec![(PyRep::ascii_bytes("k"), PyRep::ascii_bytes("v"))]),
    ]);
    let wrapped = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::SubStream(Box::new(call_3)),
        ]))),
    ])]);
    let (oid, method, args, kwargs) = extract_macho_call_req(&wrapped);
    assert_eq!(oid, None);
    assert_eq!(method, "GetTime");
    assert_eq!(args, vec![PyRep::Integer(10), PyRep::Integer(20)]);
    assert_eq!(kwargs.get("k"), Some(&PyRep::ascii_bytes("v")));

    // 2. Bound object call with "N=888444:9002" prefix and List args
    let bound_4 = PyRep::Tuple(vec![
        PyRep::ascii_bytes("N=888444:9002"),
        PyRep::ascii_bytes("DogmaGetInfo"),
        PyRep::List(vec![PyRep::Integer(777)]),
        PyRep::Dict(vec![]),
    ]);
    let bound_payload = PyRep::Tuple(vec![PyRep::SubStream(Box::new(bound_4))]);
    let (oid2, method2, args2, kwargs2) = extract_macho_call_req(&bound_payload);
    assert_eq!(oid2, Some(9002));
    assert_eq!(method2, "DogmaGetInfo");
    assert_eq!(args2, vec![PyRep::Integer(777)]);
    assert!(kwargs2.is_empty());

    // 3. Numeric t[0] > 1 with SubStream sets bound_id, non-SubStream with t[0] > 0 sets bound_id
    let syn = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(55),
        PyRep::Tuple(vec![
            PyRep::None,
            PyRep::ascii_bytes("CmdAccelerate"),
            PyRep::Tuple(vec![]),
            PyRep::None,
        ]),
    ])]);
    let (oid3, method3, args3, _) = extract_macho_call_req(&syn);
    assert_eq!(oid3, Some(55));
    assert_eq!(method3, "CmdAccelerate");
    assert!(args3.is_empty());

    // 4. Malformed payloads return empty
    assert_eq!(
        extract_macho_call_req(&PyRep::None),
        (None, String::new(), vec![], HashMap::new())
    );
    assert_eq!(
        extract_macho_call_req(&PyRep::Tuple(vec![])),
        (None, String::new(), vec![], HashMap::new())
    );
}

#[test]
fn test_session_state_diffs_notifications_and_gm_roles() {
    let mut s1 = SessionState::new(9001);
    assert!(!s1.is_authenticated());
    assert_eq!(s1.effective_location_id(), None);
    assert!(s1.to_session_change_pyrep(&s1, vec![1]).is_none());

    s1.push_notification("DoDestinyUpdate", vec![PyRep::Integer(1)]);
    assert_eq!(s1.drain_notifications().len(), 1);
    assert!(s1.drain_notifications().is_empty());

    // Hierarchy: station_id > structure_id > solar_system_id
    s1.solar_system_id = Some(30_000_142);
    assert_eq!(s1.effective_location_id(), Some(30_000_142));
    s1.structure_id = Some(1_000_000_000_001);
    assert_eq!(s1.effective_location_id(), Some(1_000_000_000_001));
    s1.station_id = Some(60_003_760);
    assert_eq!(s1.effective_location_id(), Some(60_003_760));

    // Undocking in the same solar system adds `solarsystemid` -> Some(30000142) without `solarsystemid2`
    let mut undocked = s1.clone();
    undocked.station_id = None;
    undocked.structure_id = None;
    let undock_diff = s1.compute_diff(&undocked);
    assert_eq!(
        undock_diff.get("solarsystemid"),
        Some(&(PyRep::None, PyRep::Integer(30_000_142)))
    );
    assert!(!undock_diff.contains_key("solarsystemid2"));

    // All remaining fields changing
    let mut s2 = undocked.clone();
    s2.user_id = Some(100);
    s2.character_id = Some(200);
    s2.corporation_id = Some(300);
    s2.alliance_id = Some(400);
    s2.solar_system_id = Some(30_002_187);
    s2.constellation_id = Some(20_000_322);
    s2.region_id = Some(10_000_032);
    s2.ship_id = Some(500);
    s2.race_id = Some(1);
    s2.corp_role = 16;
    s2.fleet_id = Some(600);
    s2.war_faction_id = Some(500_001);
    s2.role = service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_GML;
    s2.language_id = "DE".to_string();
    s2.address = "127.0.0.1:26000".to_string();

    assert!(s2.is_authenticated());
    let full_diff = undocked.compute_diff(&s2);
    for expected_key in [
        "userid",
        "charid",
        "corpid",
        "corpAccountKey",
        "allianceid",
        "locationid",
        "solarsystemid2",
        "solarsystemid",
        "constellationid",
        "regionid",
        "shipid",
        "raceID",
        "corprole",
        "fleetid",
        "warfactionid",
        "role",
        "languageID",
        "address",
    ] {
        assert!(
            full_diff.contains_key(expected_key),
            "missing diff key {expected_key}"
        );
    }

    let change_pyrep = undocked
        .to_session_change_pyrep(&s2, vec![888_444])
        .expect("change pyrep");
    let PyRep::Tuple(citems) = change_pyrep else {
        panic!("expected tuple");
    };
    assert_eq!(citems[0], PyRep::Integer(9001));
    assert_eq!(citems[2], PyRep::List(vec![PyRep::Integer(888_444)]));

    let init_pkt = s2.to_initial_session_notification(888_444, 12);
    assert_eq!(
        init_pkt.packet_type,
        MachoPacketType::SessionInitialStateNotification
    );
    let PyRep::Tuple(ip) = init_pkt.payload else {
        panic!("expected tuple");
    };
    assert_eq!(ip[1], PyRep::Integer(SESSION_TYPE_GAME));

    let login_pkt = s2.to_login_session_change_packet(888_444, 12);
    assert_eq!(
        login_pkt.packet_type,
        MachoPacketType::SessionChangeNotification
    );

    // GM role mask checks
    assert!(!SessionState::new(1).has_gm_role());
    for gm_bit in [
        service_roles::ROLE_GML,
        service_roles::ROLE_GMH,
        service_roles::ROLE_GMS,
        service_roles::ROLE_ADMIN,
        service_roles::ROLE_WORLDMOD,
    ] {
        let mut s = SessionState::new(2);
        s.role = service_roles::ROLE_PLAYER_DEFAULT | gm_bit;
        assert!(s.has_gm_role());
    }
}
