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
            CorporationRecord {
                corporation_id: corp_id,
                name: "POCO Owners".into(),
                ticker: "POCO".into(),
                ceo_id: char_id,
                tax_rate: 0.05,
                ..Default::default()
            },
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
