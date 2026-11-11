use super::*;
use crate::router::ServerContext;
use eve_db::{CorporationRecord, WorldRepository};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

fn make_ctx() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000)
        .unwrap();
    ServerContext::with_sde_and_repository(sde, db)
}

#[test]
fn test_orbital_registry_get_tax_settings_update_and_revert() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let corp_id = 1_000_009;
    let planet_id = 40_009_082;

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);

    // Missing planet ID
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut session,
            "planetOrbitalRegistryBroker",
            "GetTaxRate",
            &[],
            &HashMap::new(),
            None
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPlanetID"
    );

    // Default tax rate (0.05) and GetSettingsInfo
    let rate_rep = handle_pi_orbital_registry(
        &ctx,
        &mut session,
        "planetOrbitalRegistryBroker",
        "GetTaxRate",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert!((rate_rep.as_f64().unwrap() - 0.05).abs() < 1e-6);

    let info_rep = handle_pi_orbital_registry(
        &ctx,
        &mut session,
        "planetOrbitalRegistryBroker",
        "GetSettingsInfo",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(info_rep, PyRep::Tuple(t) if t.len() == 6));

    // UpdateSettings without existing orbital and non-GM -> CrpAccessDenied
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut session,
            "planetOrbitalRegistryBroker",
            "UpdateSettings",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Seed corporation + orbital owned by corp_id
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "POCO Owners".into();
                __corp.ticker = "POCO".into();
                __corp.ceo_id = char_id;
                __corp.tax_rate = 0.05;
                __corp },
        );
        save_orbital_settings(
            snap,
            planet_id,
            &OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2233,
                owner_corp_id: corp_id,
                ..Default::default()
            },
        );
    });

    // Players cannot set orbitalTypeID to 4318 (InterBus)
    let mut bad_kw = HashMap::new();
    bad_kw.insert("orbitalTypeID".into(), PyRep::Integer(4318));
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut session,
            "planetOrbitalRegistryBroker",
            "UpdateSettings",
            &[PyRep::Integer(planet_id)],
            &bad_kw,
            None
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidOrbitalType"
    );

    // Valid UpdateSettings
    let tax_kv = PyRep::keyval([
        ("corporation", PyRep::Real(0.02)),
        ("alliance", PyRep::Real(0.03)),
        ("standingNeutral", PyRep::Real(0.09)),
    ]);
    let updated = handle_pi_orbital_registry(
        &ctx,
        &mut session,
        "planetOrbitalRegistryBroker",
        "UpdateSettings",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(12),
            tax_kv,
            PyRep::Real(-2.0),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert!(
        (updated.dict_get("corporationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.02).abs()
            < 1e-6
    );
    assert!(
        (updated.dict_get("standingNeutralTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.09).abs()
            < 1e-6
    );

    // RevertOrbitalsToInterBus by CEO
    let reverted = handle_pi_orbital_registry(
        &ctx,
        &mut session,
        "planetOrbitalRegistryBroker",
        "RevertOrbitalsToInterBus",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(reverted.dict_get("orbitalTypeID").and_then(|v| v.as_i64()), Some(4318));
    assert_eq!(reverted.dict_get("ownerCorpID").and_then(|v| v.as_i64()), Some(1_000_148));
}

#[test]
fn test_orbital_registry_bound_objects_acl_clamping_gm_and_notifications() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let other_corp = 98_000_002_i64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_i64;

    // 1. Unauthenticated session fails require_character across all methods
    let mut unauth = SessionState::default();
    for meth in [
        "GetTaxRate",
        "GetSettingsInfo",
        "UpdateSettings",
        "RevertOrbitalsToInterBus",
    ] {
        let res = handle_pi_orbital_registry(
            &ctx,
            &mut unauth,
            "planetOrbitalRegistryBroker",
            meth,
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert!(res.is_err(), "{meth} should fail without character");
    }

    // 2. Unknown service / method returns None
    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    assert!(handle_pi_orbital_registry(
        &ctx,
        &mut sess,
        "planetOrbitalRegistryBroker",
        "__unknown_method__",
        &[],
        &HashMap::new(),
        None,
    )
    .is_none());
    assert!(handle_pi_orbital_registry(
        &ctx,
        &mut sess,
        "__other_svc__",
        "GetTaxRate",
        &[],
        &HashMap::new(),
        None,
    )
    .is_none());

    // 3. Bound object resolution rules:
    // - planetOrbitalRegistryBroker bound to solar_system_id (is_sys = true) -> ignored -> InvalidPlanetID
    let sys_bound = crate::router::BoundObjectEntry {
        bound_id: 1,
        session_id: 501,
        user_id: 1,
        character_id: char_id,
        service: "planetOrbitalRegistryBroker".into(),
        object_id: sys_id,
        oid_str: String::new(),
        ref_id: 1,
    };
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "GetSettingsInfo",
            &[],
            &HashMap::new(),
            Some(&sys_bound),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "UpdateSettings",
            &[],
            &HashMap::new(),
            Some(&sys_bound),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidOrbitalID"
    );
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "RevertOrbitalsToInterBus",
            &[],
            &HashMap::new(),
            Some(&sys_bound),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPlanetID"
    );

    // - planetOrbitalRegistryBroker bound to non-system planet_id works, as does planetMgr bound
    let planet_bound = crate::router::BoundObjectEntry {
        bound_id: 2,
        session_id: 501,
        user_id: 1,
        character_id: char_id,
        service: "planetOrbitalRegistryBroker".into(),
        object_id: planet_id,
        oid_str: String::new(),
        ref_id: 1,
    };
    let mgr_bound = crate::router::BoundObjectEntry {
        bound_id: 3,
        session_id: 501,
        user_id: 1,
        character_id: char_id,
        service: "planetMgr".into(),
        object_id: planet_id,
        oid_str: String::new(),
        ref_id: 1,
    };
    let r1 = handle_pi_orbital_registry(
        &ctx,
        &mut sess,
        "planetOrbitalRegistryBroker",
        "GetTaxRate",
        &[],
        &HashMap::new(),
        Some(&planet_bound),
    )
    .unwrap()
    .unwrap();
    assert!((r1.as_f64().unwrap() - 0.05).abs() < 1e-6);

    let r2 = handle_pi_orbital_registry(
        &ctx,
        &mut sess,
        "planetOrbitalRegistryBroker",
        "GetSettingsInfo",
        &[],
        &HashMap::new(),
        Some(&mgr_bound),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(r2, PyRep::Tuple(ref t) if t.len() == 6));

    // 4. Non-GM RevertOrbitalsToInterBus without existing orbital -> CrpAccessDenied
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "RevertOrbitalsToInterBus",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Seed orbital owned by other_corp -> CrpAccessDenied for UpdateSettings and Revert
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        save_orbital_settings(
            snap,
            planet_id,
            &OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2233,
                owner_corp_id: other_corp,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "UpdateSettings",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "RevertOrbitalsToInterBus",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Reassign orbital to corp_id WITHOUT a CorporationRecord -> CrpAccessDenied (requires player corp)
    let missing_corp = 98_009_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = missing_corp;
        }
        save_orbital_settings(
            snap,
            planet_id,
            &OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2233,
                owner_corp_id: missing_corp,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "UpdateSettings",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "RevertOrbitalsToInterBus",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Restore char_id and orbital to corp_id and insert CorporationRecord where char_id is NOT CEO
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = corp_id;
        }
        save_orbital_settings(
            snap,
            planet_id,
            &OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2233,
                owner_corp_id: corp_id,
                ..Default::default()
            },
        );
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Skyhook Corp".into(); __corp.ticker = "SKYH".into(); __corp.ceo_id = other_char; __corp.tax_rate = 0.05; __corp.member_roles = HashMap::from([(
                    char_id,
                    eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG,
                )]); __corp.ensure_wallet_divisions(); __corp },
        );
    });
    // Without any role on session/member: denied
    let mut no_role_sess = sess.clone();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, 0);
    });
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut no_role_sess,
            "planetOrbitalRegistryBroker",
            "UpdateSettings",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut no_role_sess,
            "planetOrbitalRegistryBroker",
            "RevertOrbitalsToInterBus",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // ROLE_EQUIPMENT_CONFIG allows UpdateSettings, but NOT RevertOrbitalsToInterBus
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG);
    });
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "RevertOrbitalsToInterBus",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Test 2-arg (orbitalID, KeyVal) with all 7 tax rates, clamping, orbitalTypeID=81080 (Skyhook) & 4318 non-GM rejection inside KeyVal
    let bad_kv = PyRep::keyval([("orbitalTypeID", PyRep::Integer(4318))]);
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut sess,
            "planetOrbitalRegistryBroker",
            "UpdateSettings",
            &[PyRep::Integer(planet_id), bad_kv],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidOrbitalType"
    );

    let _ = ctx.drain_notifications_for_char(char_id);
    let full_kv = PyRep::keyval([
        ("corporation", PyRep::Real(-0.5)),
        ("alliance", PyRep::Real(2.0)),
        ("standingHigh", PyRep::Real(0.01)),
        ("standingGood", PyRep::Real(0.04)),
        ("standingNeutral", PyRep::Real(0.12)),
        ("standingBad", PyRep::Real(0.25)),
        ("standingHorrible", PyRep::Real(1.5)),
        ("orbitalTypeID", PyRep::Integer(81080)),
        ("minimumStanding", PyRep::Real(-25.0)),
        ("allowAlliance", PyRep::Bool(false)),
        ("allowStandings", PyRep::Bool(false)),
    ]);
    let upd = handle_pi_orbital_registry(
        &ctx,
        &mut sess,
        "planetOrbitalRegistryBroker",
        "UpdateSettings",
        &[PyRep::Integer(planet_id), full_kv],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(upd.dict_get("orbitalTypeID").and_then(|v| v.as_i64()), Some(81080));
    assert_eq!(upd.dict_get("corporationTaxRate").and_then(|v| v.as_f64()), Some(0.0));
    assert_eq!(upd.dict_get("allianceTaxRate").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(upd.dict_get("minimumStanding").and_then(|v| v.as_f64()), Some(-10.0));
    assert_eq!(upd.dict_get("allowAlliance").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(upd.dict_get("allowStandings").and_then(|v| v.as_bool()), Some(false));

    // Verify OnMajorPlanetStateUpdate notification emitted
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnMajorPlanetStateUpdate"));

    // Verify GetSettingsInfo reflects all 7 persisted rates + flags
    let info = handle_pi_orbital_registry(
        &ctx,
        &mut sess,
        "planetOrbitalRegistryBroker",
        "GetSettingsInfo",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    let PyRep::Tuple(parts) = info else {
        panic!("expected 6-tuple from GetSettingsInfo");
    };
    assert_eq!(parts[0], PyRep::Integer(0));
    assert!((parts[1].dict_get("standingHigh").and_then(|v| v.as_f64()).unwrap() - 0.01).abs() < 1e-6);
    assert!((parts[1].dict_get("standingGood").and_then(|v| v.as_f64()).unwrap() - 0.04).abs() < 1e-6);
    assert!((parts[1].dict_get("standingBad").and_then(|v| v.as_f64()).unwrap() - 0.25).abs() < 1e-6);
    assert!((parts[1].dict_get("standingHorrible").and_then(|v| v.as_f64()).unwrap() - 1.0).abs() < 1e-6);
    assert_eq!(parts[2], PyRep::Real(-10.0));
    assert_eq!(parts[3], PyRep::Bool(false));
    assert_eq!(parts[4], PyRep::Bool(false));

    // kwargs override (standingLevel, allowAlliance, allowStandings, taxRateValues) + GM setting 4318
    let mut gm = sess.clone();
    gm.role = eve_db::service_roles::ROLE_GMH;
    let mut kw = HashMap::new();
    kw.insert("orbitalTypeID".into(), PyRep::Integer(4318));
    kw.insert("standingLevel".into(), PyRep::Real(15.0));
    kw.insert("allowAlliance".into(), PyRep::Bool(true));
    kw.insert("allowStandings".into(), PyRep::Bool(true));
    let gm_upd = handle_pi_orbital_registry(
        &ctx,
        &mut gm,
        "planetOrbitalRegistryBroker",
        "UpdateSettings",
        &[PyRep::Integer(40_009_083)],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(gm_upd.dict_get("orbitalTypeID").and_then(|v| v.as_i64()), Some(4318));
    assert_eq!(gm_upd.dict_get("minimumStanding").and_then(|v| v.as_f64()), Some(10.0));
    assert_eq!(gm_upd.dict_get("allowAlliance").and_then(|v| v.as_bool()), Some(true));

    // GM RevertOrbitalsToInterBus on a planet without prior orbital succeeds & emits notification
    let _ = ctx.drain_notifications_for_char(char_id);
    let gm_rev = handle_pi_orbital_registry(
        &ctx,
        &mut gm,
        "planetOrbitalRegistryBroker",
        "RevertOrbitalsToInterBus",
        &[PyRep::Integer(40_009_084)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(gm_rev.dict_get("ownerCorpID").and_then(|v| v.as_i64()), Some(1_000_148));
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnMajorPlanetStateUpdate"));
}

#[test]
fn mutant_kill_orbital_update_and_revert_corp_role_and_npc_corp_gates() {
    use eve_db::corp_roles::{
        ROLE_AUDITOR, ROLE_DIRECTOR, ROLE_EQUIPMENT_CONFIG, ROLE_STATION_MANAGER,
    };

    let ctx = make_ctx();
    let planet_id = 40_009_091_i64;
    let npc_corp = 1_000_009_i64;
    let player_corp = 98_600_001_i64;
    let ceo_cid = 91_600_001_i64;
    let dir_cid = 91_600_002_i64;
    let sm_cid = 91_600_003_i64;
    let eq_cid = 91_600_004_i64;
    let aud_cid = 91_600_005_i64;
    let npc_cid = 91_600_006_i64;

    let seed_player_orbital = |snap: &mut eve_db::WorldStateSnapshot| {
        save_orbital_settings(
            snap,
            planet_id,
            &OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2233,
                owner_corp_id: player_corp,
                ..Default::default()
            },
        );
    };

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = player_corp;
        corp.ceo_id = ceo_cid;
        corp.name = "OrbitalAclCorp".into();
        corp.member_roles.insert(dir_cid, ROLE_DIRECTOR);
        corp.member_roles.insert(sm_cid, ROLE_STATION_MANAGER);
        corp.member_roles.insert(eq_cid, ROLE_EQUIPMENT_CONFIG);
        corp.member_roles.insert(aud_cid, ROLE_AUDITOR);
        snap.corporations.insert(player_corp, corp);

        for (cid, co) in [
            (ceo_cid, player_corp),
            (dir_cid, player_corp),
            (sm_cid, player_corp),
            (eq_cid, player_corp),
            (aud_cid, player_corp),
            (npc_cid, npc_corp),
        ] {
            let mut ch = snap.characters.values().next().unwrap().clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = co;
            snap.characters.insert(cid, ch);
        }

        // Seed NPC-corp orbital first to test !corporations.contains_key
        save_orbital_settings(
            snap,
            planet_id,
            &OrbitalCustomsSettings {
                planet_id,
                orbital_type_id: 2233,
                owner_corp_id: npc_corp,
                ..Default::default()
            },
        );
    });

    let mk_sess = |cid: i64, co: i64| {
        let mut s = SessionState::default();
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(co);
        s.role = 0;
        s.corp_role = 0;
        s
    };

    let upd_kv = PyRep::keyval([("corporationTaxRate", PyRep::Real(0.11))]);

    // NPC corp owning orbital fails both UpdateSettings and RevertOrbitalsToInterBus (non-GM)
    let mut npc_sess = mk_sess(npc_cid, npc_corp);
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut npc_sess,
            "planetOrbitalRegistryBroker",
            "UpdateSettings",
            &[PyRep::Integer(planet_id), upd_kv.clone()],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut npc_sess,
            "planetOrbitalRegistryBroker",
            "RevertOrbitalsToInterBus",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    ctx.db.with_mut_snapshot(seed_player_orbital);

    // Auditor denied on both UpdateSettings and RevertOrbitalsToInterBus
    let mut aud_sess = mk_sess(aud_cid, player_corp);
    for method in ["UpdateSettings", "RevertOrbitalsToInterBus"] {
        assert_eq!(
            handle_pi_orbital_registry(
                &ctx,
                &mut aud_sess,
                "planetOrbitalRegistryBroker",
                method,
                &[PyRep::Integer(planet_id), upd_kv.clone()],
                &HashMap::new(),
                None,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CrpAccessDenied"
        );
    }

    // EquipmentConfig: allowed for UpdateSettings, denied for RevertOrbitalsToInterBus
    let mut eq_sess = mk_sess(eq_cid, player_corp);
    assert!(handle_pi_orbital_registry(
        &ctx,
        &mut eq_sess,
        "planetOrbitalRegistryBroker",
        "UpdateSettings",
        &[PyRep::Integer(planet_id), upd_kv.clone()],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        handle_pi_orbital_registry(
            &ctx,
            &mut eq_sess,
            "planetOrbitalRegistryBroker",
            "RevertOrbitalsToInterBus",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // StationManager, Director, and CEO each succeed on both UpdateSettings and RevertOrbitalsToInterBus
    for cid in [sm_cid, dir_cid, ceo_cid] {
        ctx.db.with_mut_snapshot(seed_player_orbital);
        let mut s = mk_sess(cid, player_corp);
        let upd = handle_pi_orbital_registry(
            &ctx,
            &mut s,
            "planetOrbitalRegistryBroker",
            "UpdateSettings",
            &[PyRep::Integer(planet_id), upd_kv.clone()],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            upd.dict_get("ownerCorpID").and_then(|v| v.as_i64()),
            Some(player_corp)
        );
        let rev = handle_pi_orbital_registry(
            &ctx,
            &mut s,
            "planetOrbitalRegistryBroker",
            "RevertOrbitalsToInterBus",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            rev.dict_get("ownerCorpID").and_then(|v| v.as_i64()),
            Some(1_000_148)
        );
    }
}
