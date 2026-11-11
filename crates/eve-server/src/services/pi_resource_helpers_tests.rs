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

#[test]
fn test_pi_resource_edge_branches_caps_fallbacks_and_truncation() {
    let mut ctx = make_ctx();
    let planet_id = 40_009_082;
    let nid = pi_depletion_note_id(planet_id);
    assert_eq!(nid, pi_depletion_note_id(-planet_id));

    // Malformed owner_note JSON falls back to default state
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            nid,
            OwnerNoteRecord {
                note_id: nid,
                character_id: -1,
                item_id: planet_id,
                label: "bad".into(),
                note: "{not-valid-json".into(),
                timestamp_unix: 0,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        let def = load_planet_depletion(snap, planet_id);
        assert_eq!(def.planet_id, planet_id);
        assert!(def.resource_depletion.is_empty());
    });

    // Colony extraction >= 100k + local spots (zero-radius ignored, out-of-radius ignored,
    // other-resource ignored, max local cap 0.50, total depletion cap 0.85, floor 0.02)
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (90_000_001, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: 90_000_001,
                solar_system_id: 30_000_142,
                command_center_level: 3,
                pins_count: 1,
                extracted_commodities: HashMap::from([(2268, 500_000)]),
                last_extracted_unix: 1000,
            },
        );
        let state = PlanetResourceDepletionState {
            planet_id,
            resource_depletion: HashMap::from([(2268, 0.20)]),
            nugget_layers: Default::default(),
            local_depletion: vec![
                PiDepletionSpot {
                    latitude: 0.2,
                    longitude: 0.4,
                    radius: 0.0, // ignored because radius <= 0.0
                    resource_type_id: 2268,
                    amount_depleted: 0.9,
                },
                PiDepletionSpot {
                    latitude: 2.0,
                    longitude: 2.0,
                    radius: 0.1, // ignored because dist >= radius
                    resource_type_id: 2268,
                    amount_depleted: 0.9,
                },
                PiDepletionSpot {
                    latitude: 0.2,
                    longitude: 0.4,
                    radius: 0.5,
                    resource_type_id: 2267, // different resource
                    amount_depleted: 0.9,
                },
                PiDepletionSpot {
                    latitude: 0.2,
                    longitude: 0.4,
                    radius: 0.5,
                    resource_type_id: 2268,
                    amount_depleted: 0.8, // capped at 0.50
                },
            ],
            gm_depletion_rate: 0.35, // overrides resource_depletion 0.20 via max()
            gm_depletion_curve: vec![],
            gm_last_sim_unix: 0,
        };
        save_planet_depletion(snap, &state);
    });

    let capped_density = ctx.db.with_snapshot(|snap| {
        compute_planet_resource_density(&ctx, snap, planet_id, 2268, 0.2, 0.4)
    });
    // colony_depl(0.40) + gm_depl(0.35) + local_depl(0.50) -> clamped to 0.85 total_depletion -> 0.15 * base
    assert!(capped_density >= 0.02 && capped_density <= 0.95 * 0.15 + 1e-6);

    // compute_pin_resource_distribution with empty extractor_heads and custom unknown planet_id
    let custom_planet_id = 40_999_888_i64;
    let empty_head_pin = PiPinState {
        pin_id: 2001,
        type_id: 3060,
        latitude: 0.1,
        longitude: -0.2,
        extractor_resource_type_id: None,
        extractor_head_count: 0,
        extractor_head_radius: 0.01,
        extractor_heads: vec![],
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (90_000_001, custom_planet_id),
            PlanetaryColonyRecord {
                planet_id: custom_planet_id,
                owner_id: 90_000_001,
                solar_system_id: 30_000_142,
                command_center_level: 1,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 1000,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        // Known SDE planet with empty extractor_heads falls back to pin lat/lon
        let dist_known = compute_pin_resource_distribution(&ctx, snap, planet_id, &empty_head_pin);
        assert!(!dist_known.is_empty());
        // Unknown planet not in SDE returns empty distribution (exercises fallback sys_id paths cleanly)
        let dist_custom =
            compute_pin_resource_distribution(&ctx, snap, custom_planet_id, &empty_head_pin);
        assert!(dist_custom.is_empty());
        let dist_uncolonized =
            compute_pin_resource_distribution(&ctx, snap, 40_999_777, &empty_head_pin);
        assert!(dist_uncolonized.is_empty());
    });

    // simulate_planet_depletion: duration clamping (0 -> 1, 500 -> 168), empty extractor pins
    let empty_net = PiColonyNetworkState::default();
    let (empty_rate, min_curve) = ctx.db.with_mut_snapshot(|snap| {
        simulate_planet_depletion(snap, custom_planet_id, 90_000_001, &empty_net, 0)
    });
    assert!((empty_rate - 0.015).abs() < 1e-9);
    assert_eq!(min_curve.len(), 1);

    // Type 3060 with empty extractor_heads + >64 local depletion spots truncation + resource_depletion 0.90 cap
    let many_heads: Vec<(i64, f64, f64)> = (0..70).map(|i| (i + 1, 0.01 * (i as f64), 0.1)).collect();
    let big_net = PiColonyNetworkState {
        pins: vec![
            empty_head_pin.clone(),
            PiPinState {
                pin_id: 2002,
                type_id: 2848,
                latitude: 0.0,
                longitude: 0.0,
                extractor_resource_type_id: Some(2268),
                extractor_head_count: 70,
                extractor_head_radius: 0.05,
                extractor_heads: many_heads,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let (big_rate, max_curve) = ctx.db.with_mut_snapshot(|snap| {
        simulate_planet_depletion(snap, custom_planet_id, 90_000_001, &big_net, 500)
    });
    assert!((big_rate - 0.015 * 71.0).abs() < 1e-9);
    assert_eq!(max_curve.len(), 168);
    ctx.db.with_snapshot(|snap| {
        let st = load_planet_depletion(snap, custom_planet_id);
        assert_eq!(st.local_depletion.len(), 64);
        assert!((st.resource_depletion[&2268] - 0.90).abs() < 1e-9);
    });

    // parse_theta_phi List, 1-element tuple/list, and non-numeric variants
    assert_eq!(
        parse_theta_phi(Some(&PyRep::List(vec![PyRep::Real(1.1), PyRep::Real(-2.2)])), None),
        (1.1, -2.2)
    );
    assert_eq!(
        parse_theta_phi(
            Some(&PyRep::Tuple(vec![PyRep::Real(0.5)])),
            Some(&PyRep::Real(0.9))
        ),
        (0.0, 0.9)
    );
    assert_eq!(
        parse_theta_phi(Some(&PyRep::String("bad".into())), Some(&PyRep::None)),
        (0.0, 0.0)
    );

    // Suppress unused mut warning if ctx wasn't mutated directly
    let _ = &mut ctx;
}
