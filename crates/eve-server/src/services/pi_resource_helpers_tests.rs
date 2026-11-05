use super::*;
use crate::router::ServerContext;
use eve_db::{PlanetaryColonyRecord, WorldRepository};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

fn make_ctx() -> ServerContext {
    let sde = SdeRegistry::with_canonical_seed();
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_spherical_harmonics_and_parse_theta_phi() {
    let b0 = evaluate_spherical_harmonic_basis(0.0, 0.0);
    assert!((b0[0] - 1.0).abs() < 1e-9);
    assert!((b0[1] - 0.0).abs() < 1e-9);
    assert!((b0[2] - 1.0).abs() < 1e-9);
    assert!((b0[3] - 0.0).abs() < 1e-9);
    assert!((b0[4] - (-1.0)).abs() < 1e-9);
    assert!((b0[7] - 1.0).abs() < 1e-9);

    let bp = evaluate_spherical_harmonic_basis(std::f64::consts::FRAC_PI_2, 0.0);
    assert!((bp[0] - 1.0).abs() < 1e-9);
    assert!((bp[1] - 1.0).abs() < 1e-9);
    assert!((bp[4] - 2.0).abs() < 1e-9);

    assert_eq!(
        parse_theta_phi(Some(&PyRep::Tuple(vec![PyRep::Real(0.25), PyRep::Real(0.75)])), None),
        (0.25, 0.75)
    );
    assert_eq!(
        parse_theta_phi(Some(&PyRep::Real(0.5)), Some(&PyRep::Real(-0.5))),
        (0.5, -0.5)
    );
    assert_eq!(parse_theta_phi(Some(&PyRep::Real(0.3)), None), (0.3, 0.0));
    assert_eq!(parse_theta_phi(None, None), (0.0, 0.0));
}

#[test]
fn test_depletion_persistence_density_distribution_and_simulation() {
    let ctx = make_ctx();
    let planet_id = 40_009_082;
    let nid = pi_depletion_note_id(planet_id);
    assert!(nid < -8_500_000_000_000);

    let initial_density = ctx.db.with_snapshot(|snap| {
        let def = load_planet_depletion(snap, planet_id);
        assert_eq!(def.planet_id, planet_id);
        assert!(def.resource_depletion.is_empty());
        compute_planet_resource_density(&ctx, snap, planet_id, 2268, 0.2, 0.4)
    });
    assert!(initial_density >= 0.15 && initial_density <= 0.95);

    // Simulate depletion and colony extraction and verify density decreases
    let net = PiColonyNetworkState {
        pins: vec![PiPinState {
            pin_id: 1001,
            type_id: 2848,
            latitude: 0.2,
            longitude: 0.4,
            extractor_resource_type_id: Some(2268),
            extractor_head_count: 4,
            extractor_head_radius: 0.1,
            extractor_heads: vec![(1, 0.2, 0.4), (2, 0.21, 0.41)],
            ..Default::default()
        }],
        ..Default::default()
    };

    let (rate, curve) = ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (90_000_001, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: 90_000_001,
                solar_system_id: 30_000_142,
                command_center_level: 2,
                pins_count: 2,
                extracted_commodities: HashMap::from([(2268, 50_000)]),
                last_extracted_unix: 1000,
            },
        );
        simulate_planet_depletion(snap, planet_id, 90_000_001, &net, 12)
    });
    assert!((rate - 0.06).abs() < 1e-9);
    assert_eq!(curve.len(), 12);
    assert!((curve[0] - 1.0).abs() < 1e-9);
    assert!(curve[11] < curve[0]);

    ctx.db.with_snapshot(|snap| {
        let loaded = load_planet_depletion(snap, planet_id);
        assert_eq!(loaded.local_depletion.len(), 2);
        let depleted_density =
            compute_planet_resource_density(&ctx, snap, planet_id, 2268, 0.2, 0.4);
        assert!(depleted_density < initial_density);
        assert!(depleted_density >= 0.02);

        let dist = compute_pin_resource_distribution(&ctx, snap, planet_id, &net.pins[0]);
        assert!(!dist.is_empty());
        assert!(dist.values().all(|&v| (0.02..=1.0).contains(&v)));
    });
}
