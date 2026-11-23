use super::*;
use crate::router::ServerContext;
use eve_db::{PlanetaryColonyRecord, ServerClock, WorldRepository};
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

    // Legacy `col.extracted_commodities` must NOT drive depletion when `colony_extraction_pressure` is empty;
    // unified `colony_extraction_pressure >= 100k` + local spots enforce:
    // zero-radius ignored, out-of-radius ignored, other-resource ignored, max local cap 0.50, total depletion cap 0.85, floor 0.02
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
        let (legacy_colony_depl, _, _) =
            global_colony_and_gm_depletion_at(&ctx.sde, snap, planet_id, 2268, 1000);
        assert_eq!(
            legacy_colony_depl, 0.0,
            "col.extracted_commodities must never act as a fallback depletion source"
        );
        let state = PlanetResourceDepletionState {
            planet_id,
            resource_depletion: HashMap::from([(2268, 0.20)]),
            last_updated_unix: 0,
            nugget_layers: Default::default(),
            colony_extraction_pressure: HashMap::from([(2268, 500_000.0)]),
            colony_pressure_last_unix: 0,
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
            ..Default::default()
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

    // Verify spherical harmonic binary coefficients across planet types and depletion
    assert!((MAX_DISPLAY_QUALITY - 1.21 * 255.0 * 0.5).abs() < 1e-9);
    assert_eq!(
        PLANET_RESOURCE_PROXIMITY_LIMITS,
        [(2, 6), (4, 10), (6, 15), (10, 20), (15, 30)]
    );
    let (sh_before, sh_after_depl) = ctx.db.with_snapshot(|snap| {
        let b = compute_planet_resource_sh_coefficients(&ctx, snap, planet_id, 2268, 6);
        let q = compute_planet_resource_quality(&ctx, snap, planet_id, 2268);
        assert_eq!(b.len(), 36);
        assert!(b.iter().all(|v| v.is_finite()));
        assert!(b[0] > 0.0 && (q - f64::from(b[0])).abs() < 1e-6);
        assert!(b[1..9].iter().any(|c| c.abs() > 1e-5));
        (b, compute_planet_complete_resource_sh_bytes(&ctx, snap, custom_planet_id, 2268))
    });
    assert_eq!(sh_after_depl.len(), 36);
    assert_ne!(
        sh_after_depl[0..4],
        sh_before[0].to_le_bytes(),
        "depleted custom planet SH monopole must differ from fresh planet"
    );

    // Time-based depletion regeneration (`last_updated_unix` + colony extraction decay)
    let mut reg_state = PlanetResourceDepletionState {
        planet_id: custom_planet_id,
        resource_depletion: HashMap::from([(2268, 0.80)]),
        local_depletion: vec![PiDepletionSpot {
            latitude: 0.1,
            longitude: 0.2,
            radius: 0.2,
            resource_type_id: 2268,
            amount_depleted: 0.40,
        }],
        gm_depletion_rate: 0.40,
        last_updated_unix: 1_700_000_000,
        ..Default::default()
    };
    regenerate_planet_depletion(&mut reg_state, 1_700_000_000 + 43_200);
    assert!((reg_state.resource_depletion[&2268] - 0.40).abs() < 1e-6);
    assert!((reg_state.gm_depletion_rate - 0.20).abs() < 1e-6);
    assert!((reg_state.local_depletion[0].amount_depleted - 0.20).abs() < 1e-6);

    // Legacy `col.extracted_commodities` never triggers depletion on its own, while unified
    // `colony_extraction_pressure` decays by half across one 12h half-life (`43_200s`).
    let mut reg_pressure_state = PlanetResourceDepletionState {
        planet_id: custom_planet_id,
        colony_extraction_pressure: HashMap::from([(2268, 80_000.0)]),
        colony_pressure_last_unix: 1_700_000_000,
        ..Default::default()
    };
    regenerate_planet_depletion(&mut reg_pressure_state, 1_700_000_000 + 43_200);
    assert!((reg_pressure_state.colony_extraction_pressure[&2268] - 40_000.0).abs() < 1e-6);

    // Suppress unused mut warning if ctx wasn't mutated directly
    let _ = &mut ctx;
}

#[test]
fn legacy_colony_decayed_past_16_half_lives_stays_migrated_after_get_colony_and_ticks() {
    use crate::services::account_services::account_services_tests::auth_session;
    use crate::services::industry_pi::handle;
    use crate::services::industry_pi::pi_helpers::tick_all_colonies_background;

    let initial_unix = 1_700_000_000_u64;
    let idle_secs = (20.0 * super::PI_DEPLETION_RECOVERY_HALF_LIFE_SECS) as u64;
    let now_unix = initial_unix + idle_secs;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let res_tid = 2268_u32;

    let ctx = make_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(now_unix),
    ));
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            13279,
            eve_db::TrainedSkillRecord {
                type_id: 13279,
                level: 4,
                skill_points: 256_000.0,
            },
        );
    });
    let mut sess = auth_session(1, char_id, 1_000_170, sys_id);
    let kw = HashMap::new();

    let query_quality_and_sh0 = |sess: &mut eve_machonet::SessionState| -> (f64, f32) {
        let info = handle(
            &ctx,
            sess,
            "planetMgr",
            "GetPlanetResourceInfo",
            &[eve_proto::PyRep::Integer(planet_id)],
            &kw,
            None,
        )
        .expect("GetPlanetResourceInfo handled")
        .expect("GetPlanetResourceInfo ok");
        let eve_proto::PyRep::Dict(entries) = info else {
            panic!("GetPlanetResourceInfo must return Dict");
        };
        let quality = entries
            .iter()
            .find_map(|(k, v)| (k.as_i64() == Some(i64::from(res_tid))).then(|| v.as_f64()).flatten())
            .expect("planet resource info must include Aqueous Liquids (2268)");

        let data_rep = handle(
            &ctx,
            sess,
            "planetMgr",
            "GetResourceData",
            &[eve_proto::PyRep::keyval([
                ("planetID", eve_proto::PyRep::Integer(planet_id)),
                ("resourceTypeID", eve_proto::PyRep::Integer(i64::from(res_tid))),
            ])],
            &kw,
            None,
        )
        .expect("GetResourceData handled")
        .expect("GetResourceData ok");
        let Some(eve_proto::PyRep::Bytes(sh_bytes)) = data_rep.dict_get("data") else {
            panic!("GetResourceData must include SH bytes");
        };
        assert!(sh_bytes.len() >= 4);
        let sh0 = f32::from_le_bytes([sh_bytes[0], sh_bytes[1], sh_bytes[2], sh_bytes[3]]);
        (quality, sh0)
    };

    let (clean_qual, clean_sh0) = query_quality_and_sh0(&mut sess);
    assert!(clean_qual > 0.0 && clean_sh0 > 0.0);

    // Unmigrated legacy `col.extracted_commodities` must NEVER create depletion or lower quality/yield.
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_id,
                planet_id,
                solar_system_id: sys_id,
                command_center_level: 1,
                pins_count: 1,
                extracted_commodities: HashMap::from([(res_tid, 80_000)]),
                last_extracted_unix: initial_unix,
            },
        );
    });

    let (eff_legacy, _, _) = ctx.db.with_snapshot(|snap| {
        super::global_colony_and_gm_depletion_at(&ctx.sde, snap, planet_id, res_tid, now_unix)
    });
    assert_eq!(eff_legacy, 0.0);

    let (pre_qual, pre_sh0) = query_quality_and_sh0(&mut sess);
    assert!((pre_qual - clean_qual).abs() < 1e-9);
    assert!((pre_sh0 - clean_sh0).abs() < 1e-6);

    // Seed decayed `colony_extraction_pressure` (> 16 half-lives old, < 1.0 effective).
    ctx.db.with_mut_snapshot(|snap| {
        let mut depl = super::load_planet_depletion(snap, planet_id);
        depl.colony_extraction_pressure
            .insert(res_tid, 80_000.0);
        depl.colony_pressure_last_unix = initial_unix;
        super::save_planet_depletion(snap, &depl);
    });

    let (eff_before, _, _) = ctx.db.with_snapshot(|snap| {
        super::global_colony_and_gm_depletion_at(&ctx.sde, snap, planet_id, res_tid, now_unix)
    });
    assert_eq!(eff_before, 0.0);

    // Calling GetColony prunes decayed pressure cleanly while preserving physical `extracted_commodities`.
    handle(
        &ctx,
        &mut sess,
        "planetMgr",
        "GetColony",
        &[eve_proto::PyRep::Integer(planet_id)],
        &kw,
        None,
    )
    .expect("GetColony handled")
    .expect("GetColony ok");

    ctx.db.with_snapshot(|snap| {
        let depl = super::load_planet_depletion(snap, planet_id);
        assert_eq!(depl.colony_pressure_last_unix, now_unix);
        assert!(depl.colony_extraction_pressure.is_empty());
        let col = &snap.planetary_colonies[&(char_id, planet_id)];
        assert_eq!(col.last_extracted_unix, now_unix);
        assert_eq!(col.extracted_commodities.get(&res_tid).copied(), Some(80_000));
        let (eff_after, _, _) =
            super::global_colony_and_gm_depletion_at(&ctx.sde, snap, planet_id, res_tid, now_unix);
        assert_eq!(eff_after, 0.0);
    });

    let (after_qual, after_sh0) = query_quality_and_sh0(&mut sess);
    assert!(
        (after_qual - clean_qual).abs() < 1e-6,
        "decayed planet must keep ~0% depletion after GetColony: clean={clean_qual}, after={after_qual}"
    );
    assert!(
        (after_sh0 - clean_sh0).abs() < 1e-4,
        "SH survey data must not resurrect decayed depletion: clean={clean_sh0}, after={after_sh0}"
    );

    // Also verify `always_persist = false` background tick path prunes decayed pressure cleanly.
    let ctx_bg = make_ctx();
    ctx_bg.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(now_unix),
    ));
    ctx_bg.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_id,
                planet_id,
                solar_system_id: sys_id,
                command_center_level: 1,
                pins_count: 1,
                extracted_commodities: HashMap::from([(res_tid, 80_000)]),
                last_extracted_unix: initial_unix,
            },
        );
        let mut depl = super::load_planet_depletion(snap, planet_id);
        depl.colony_extraction_pressure
            .insert(res_tid, 80_000.0);
        depl.colony_pressure_last_unix = initial_unix;
        super::save_planet_depletion(snap, &depl);
    });
    tick_all_colonies_background(&ctx_bg, now_unix);
    ctx_bg.db.with_snapshot(|snap| {
        let depl = super::load_planet_depletion(snap, planet_id);
        assert_eq!(depl.colony_pressure_last_unix, now_unix);
        assert!(depl.colony_extraction_pressure.is_empty());
        let (eff_bg, _, _) =
            super::global_colony_and_gm_depletion_at(&ctx_bg.sde, snap, planet_id, res_tid, now_unix);
        assert_eq!(eff_bg, 0.0);
    });
}

#[cfg(test)]
fn build_test_globaltypes_fixture() -> HashMap<TypeId, HashMap<TypeId, f64>> {
    let mut cfsd = std::collections::BTreeMap::new();
    cfsd.insert(
        "11".to_string(),
        serde_json::json!({
            "resourceDistribution": {
                "2305": 1.0,
                "2268": 0.9,
                "2073": 0.01
            }
        }),
    );
    parse_globaltypes_resource_distributions(&cfsd)
}

#[test]
fn test_pve118_no_client_globaltypes_fallback_in_production_and_test_fixture_helper() {
    // 1. Test-only synthetic fixture supplies per-planet weights under `#[cfg(test)]`.
    let fixture = build_test_globaltypes_fixture();
    assert_eq!(lookup_planet_resource_distribution_weight(&fixture, 11, 2305), Some(1.0));
    assert_eq!(lookup_planet_resource_distribution_weight(&fixture, 11, 2268), Some(0.9));
    assert_eq!(lookup_planet_resource_distribution_weight(&fixture, 11, 2073), Some(0.05));

    // 2. Build an SDE with canonical planet/type metadata but zero runtime CFSD resources.
    let canonical_sde = SdeRegistry::with_canonical_seed();
    let mut no_cfsd_sde = canonical_sde.clone();
    no_cfsd_sde.resources.clear();

    // Even if runtime SDE loads and populates the static cache first, non-CFSD SDE must not read it.
    if let Ok(real_sde) = SdeRegistry::open(&SdeRegistry::default_db_path()) {
        let loaded = load_globaltypes_resource_distributions(&real_sde);
        if real_sde
            .resources
            .contains_key("res:/staticdata/globaltypes.fsdbinary")
        {
            let dist = loaded.expect(
                "real SDE containing res:/staticdata/globaltypes.fsdbinary must parse globaltypes distributions",
            );
            assert!(!dist.is_empty());
            let runtime_weight =
                resolve_planet_resource_distribution_weight(&real_sde, 40_009_082, 2305);
            assert!(runtime_weight >= 0.05 && runtime_weight <= 1.0);
        }
    }

    assert!(load_globaltypes_resource_distributions(&no_cfsd_sde).is_none());
    // Without runtime CFSD data, production returns uniform 0.75 across all resources (no client fallback table).
    let w_2305 = resolve_planet_resource_distribution_weight(&no_cfsd_sde, 40_009_082, 2305);
    let w_2268 = resolve_planet_resource_distribution_weight(&no_cfsd_sde, 40_009_082, 2268);
    let w_unknown = resolve_planet_resource_distribution_weight(&no_cfsd_sde, 40_009_082, 999_999);
    assert!((w_2305 - 0.75).abs() < 1e-9);
    assert!((w_2268 - 0.75).abs() < 1e-9);
    assert!((w_unknown - 0.75).abs() < 1e-9);
}

#[test]
fn test_pve120_unified_colony_pressure_drives_yield_preview_and_overlay_with_recovery() {
    use crate::services::account_services::account_services_tests::auth_session;
    use crate::services::industry_pi::handle;
    use crate::services::industry_pi::pi_helpers::{
        advance_colony_and_update_depletion, save_pi_network, PiColonyNetworkState, PiLinkState,
        PiPinState, PiRouteState,
    };

    let t0 = 1_700_000_000_u64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let res_tid = 2268_u32;

    let ctx = make_ctx();
    let clock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0));
    ctx.set_clock(clock.clone());
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            13279,
            eve_db::TrainedSkillRecord {
                type_id: 13279,
                level: 4,
                skill_points: 256_000.0,
            },
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_id,
                planet_id,
                solar_system_id: sys_id,
                command_center_level: 3,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: t0,
            },
        );
    });
    let mut sess = auth_session(1, char_id, 1_000_170, sys_id);
    let kw = HashMap::new();

    let preview_heads = eve_proto::PyRep::List(vec![
        eve_proto::PyRep::Tuple(vec![
            eve_proto::PyRep::Integer(1),
            eve_proto::PyRep::Real(0.20),
            eve_proto::PyRep::Real(0.40),
        ]),
        eve_proto::PyRep::Tuple(vec![
            eve_proto::PyRep::Integer(2),
            eve_proto::PyRep::Real(0.40),
            eve_proto::PyRep::Real(0.60),
        ]),
    ]);

    let query_preview_and_overlay = |sess: &mut eve_machonet::SessionState| -> (i64, f64, f64) {
        let prog = handle(
            &ctx,
            sess,
            "planetMgr",
            "GetProgramResultInfo",
            &[
                eve_proto::PyRep::Integer(planet_id),
                eve_proto::PyRep::Integer(5001),
                eve_proto::PyRep::Integer(i64::from(res_tid)),
                preview_heads.clone(),
                eve_proto::PyRep::Real(0.02),
            ],
            &kw,
            None,
        )
        .expect("GetProgramResultInfo handled")
        .expect("GetProgramResultInfo ok");
        let eve_proto::PyRep::Tuple(parts) = prog else {
            panic!("GetProgramResultInfo tuple expected");
        };
        let qty_preview = parts[0].as_i64().unwrap();
        let dens = ctx.db.with_snapshot(|snap| {
            compute_planet_resource_density(&ctx, snap, planet_id, res_tid, 0.20, 0.40)
        });
        let yield_scale = ctx.db.with_snapshot(|snap| {
            planet_resource_yield_scale(&ctx.sde, snap, planet_id, res_tid, ctx.now_unix())
        });
        (qty_preview, dens, yield_scale)
    };

    let (preview_fresh, dens_fresh, scale_fresh) = query_preview_and_overlay(&mut sess);
    assert_eq!(preview_fresh, 2_000);
    assert!((scale_fresh - 1.0).abs() < 1e-9);

    // Record Flush-equivalent extraction pressure via unified `record_planet_colony_extraction`.
    ctx.db.with_mut_snapshot(|snap| {
        record_planet_colony_extraction(&ctx.sde, snap, planet_id, &HashMap::from([(res_tid, 30_000)]), t0);
    });
    let (preview_depl, dens_depl, scale_depl) = query_preview_and_overlay(&mut sess);
    assert!((scale_depl - 0.70).abs() < 1e-9);
    assert_eq!(preview_depl, 1_400);
    assert!((dens_depl / dens_fresh - 0.70).abs() < 1e-6);

    // Run one colony cycle under that exact unified depletion pressure.
    let extracted_depleted = ctx.db.with_mut_snapshot(|snap| {
        let mut col = snap.planetary_colonies.get(&(char_id, planet_id)).cloned().unwrap();
        let mut net = PiColonyNetworkState {
            pins: vec![
                PiPinState {
                    pin_id: 5001,
                    type_id: 2848,
                    latitude: 0.20,
                    longitude: 0.40,
                    extractor_resource_type_id: Some(res_tid),
                    extractor_cycle_seconds: 1_800,
                    extractor_head_count: 2,
                    extractor_head_radius: 0.02,
                    extractor_heads: vec![(1, 0.20, 0.40), (2, 0.40, 0.60)],
                    program_install_unix: t0,
                    expiry_time_unix: Some(t0 + 1_800),
                    ..Default::default()
                },
                PiPinState {
                    pin_id: 5002,
                    type_id: 2541,
                    latitude: 0.21,
                    longitude: 0.41,
                    ..Default::default()
                },
            ],
            links: vec![PiLinkState {
                source_pin_id: 5001,
                dest_pin_id: 5002,
                level: 1,
                ..Default::default()
            }],
            routes: vec![PiRouteState {
                route_id: 1,
                path: vec![5001, 5002],
                commodity_type_id: res_tid,
                quantity: 5_000,
            }],
            last_simulated_unix: t0,
            ..Default::default()
        };
        clock.advance_secs(1_800);
        let (ext, _) = advance_colony_and_update_depletion(
            &ctx,
            snap,
            planet_id,
            &mut col,
            &mut net,
            t0,
            1_800,
            t0 + 1_800,
            true,
        );
        snap.planetary_colonies.insert((char_id, planet_id), col);
        save_pi_network(snap, char_id, planet_id, &net);
        ext
    });
    assert_eq!(extracted_depleted, 1_400);

    // After 12h idle recovery, GetColony decays unified pressure and restores preview, overlay, and extracted yield together.
    clock.advance_secs(43_200);
    handle(
        &ctx,
        &mut sess,
        "planetMgr",
        "GetColony",
        &[eve_proto::PyRep::Integer(planet_id)],
        &kw,
        None,
    )
    .expect("GetColony idle handled")
    .expect("GetColony idle ok");

    let (preview_rec, dens_rec, scale_rec) = query_preview_and_overlay(&mut sess);
    assert!(scale_rec > scale_depl && preview_rec > preview_depl && dens_rec > dens_depl);

    let extracted_recovered = ctx.db.with_mut_snapshot(|snap| {
        let now_u = ctx.now_unix();
        let mut col = snap.planetary_colonies.get(&(char_id, planet_id)).cloned().unwrap();
        let mut net = crate::services::industry_pi::pi_helpers::load_pi_network(snap, char_id, planet_id, col.command_center_level);
        net.pins[0].program_install_unix = now_u;
        net.pins[0].expiry_time_unix = Some(now_u + 1_800);
        let (ext, _) = advance_colony_and_update_depletion(
            &ctx,
            snap,
            planet_id,
            &mut col,
            &mut net,
            now_u,
            1_800,
            now_u + 1_800,
            true,
        );
        ext
    });
    assert_eq!(extracted_recovered as i64, preview_rec);
}

#[test]
fn test_pve120_shared_planet_idle_colony_and_overflow_preserve_active_pressure() {
    use crate::services::account_services::account_services_tests::auth_session;
    use crate::services::industry_pi::handle;
    use crate::services::industry_pi::pi_helpers::{
        advance_colony_and_update_depletion, save_pi_network, PiLinkState, PiRouteState,
    };

    let t0 = 1_700_000_000_u64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let res_tid = 2268_u32;

    let ctx = make_ctx();
    let clock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0));
    ctx.set_clock(clock.clone());

    ctx.db.with_mut_snapshot(|snap| {
        for cid in [char_a, char_b] {
            snap.skills.entry(cid).or_default().insert(
                13279,
                eve_db::TrainedSkillRecord {
                    type_id: 13279,
                    level: 4,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.planetary_colonies.insert(
            (char_a, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_a,
                planet_id,
                solar_system_id: sys_id,
                command_center_level: 3,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: t0,
            },
        );
        save_pi_network(
            snap,
            char_a,
            planet_id,
            &PiColonyNetworkState {
                pins: vec![
                    PiPinState {
                        pin_id: 5001,
                        type_id: 2848,
                        latitude: 0.20,
                        longitude: 0.40,
                        extractor_resource_type_id: Some(res_tid),
                        extractor_cycle_seconds: 1_800,
                        extractor_head_count: 2,
                        extractor_head_radius: 0.02,
                        extractor_heads: vec![(1, 0.20, 0.40), (2, 0.40, 0.60)],
                        program_install_unix: t0,
                        expiry_time_unix: Some(t0 + 7_200),
                        ..Default::default()
                    },
                    PiPinState {
                        pin_id: 5002,
                        type_id: 2541,
                        latitude: 0.21,
                        longitude: 0.41,
                        ..Default::default()
                    },
                ],
                links: vec![PiLinkState {
                    source_pin_id: 5001,
                    dest_pin_id: 5002,
                    level: 1,
                    ..Default::default()
                }],
                routes: vec![PiRouteState {
                    route_id: 1,
                    path: vec![5001, 5002],
                    commodity_type_id: res_tid,
                    quantity: 5_000,
                }],
                last_simulated_unix: t0,
                ..Default::default()
            },
        );

        // Idle second colony on the same planet (no active ECU).
        snap.planetary_colonies.insert(
            (char_b, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_b,
                planet_id,
                solar_system_id: sys_id,
                command_center_level: 1,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: t0,
            },
        );
        save_pi_network(
            snap,
            char_b,
            planet_id,
            &PiColonyNetworkState {
                pins: vec![PiPinState {
                    pin_id: 6001,
                    type_id: 2534,
                    latitude: -0.30,
                    longitude: 0.50,
                    ..Default::default()
                }],
                last_simulated_unix: t0,
                ..Default::default()
            },
        );

        let mut depl = load_planet_depletion(snap, planet_id);
        depl.planet_id = planet_id;
        depl.colony_extraction_pressure.insert(res_tid, 30_000.0);
        depl.colony_pressure_last_unix = t0;
        save_planet_depletion(snap, &depl);
    });

    let mut sess_b = auth_session(2, char_b, 1_000_170, sys_id);
    let kw = HashMap::new();

    // 1. Mid-cycle (t0 + 900): colony A is active on [t0, t0 + 7_200].
    // Neither read-only scale/density nor idle colony B ticking via GetColony may decay planet pressure.
    clock.advance_secs(900);
    let mid_scale_ro = ctx.db.with_snapshot(|snap| {
        planet_resource_yield_scale(&ctx.sde, snap, planet_id, res_tid, ctx.now_unix())
    });
    assert!((mid_scale_ro - 0.70).abs() < 1e-9);

    handle(
        &ctx,
        &mut sess_b,
        "planetMgr",
        "GetColony",
        &[eve_proto::PyRep::Integer(planet_id)],
        &kw,
        None,
    )
    .expect("GetColony handled")
    .expect("GetColony ok");

    let (mid_pressure, mid_scale_after_b) = ctx.db.with_snapshot(|snap| {
        let depl = load_planet_depletion(snap, planet_id);
        (
            depl.colony_extraction_pressure.get(&res_tid).copied().unwrap_or(0.0),
            planet_resource_yield_scale(&ctx.sde, snap, planet_id, res_tid, ctx.now_unix()),
        )
    });
    assert!(
        (mid_pressure - 30_000.0).abs() < 1e-6,
        "idle second colony ticking must not decay active planet pressure mid-cycle, got {mid_pressure}"
    );
    assert!((mid_scale_after_b - 0.70).abs() < 1e-9);

    // 2. Full-storage overflow on colony A across cycles [t0, t0 + 3_600] (actual_deposit == 0):
    // active cycles still advance colony_pressure_last_unix to t0 + 3_600 without decaying working_depl.
    clock.advance_secs(2_700);
    let now_overflow = ctx.now_unix();
    let (ext_overflow, depl_overflow, scale_overflow) = ctx.db.with_mut_snapshot(|snap| {
        let mut col_a = snap.planetary_colonies.get(&(char_a, planet_id)).cloned().unwrap();
        let mut net_a = crate::services::industry_pi::pi_helpers::load_pi_network(
            snap,
            char_a,
            planet_id,
            col_a.command_center_level,
        );
        net_a.pins[1].contents.insert(res_tid, 10_000_000);
        let (ext, _) = advance_colony_and_update_depletion(
            &ctx,
            snap,
            planet_id,
            &mut col_a,
            &mut net_a,
            t0,
            3_600,
            now_overflow,
            true,
        );
        snap.planetary_colonies.insert((char_a, planet_id), col_a);
        save_pi_network(snap, char_a, planet_id, &net_a);
        let depl = load_planet_depletion(snap, planet_id);
        let scale = planet_resource_yield_scale(&ctx.sde, snap, planet_id, res_tid, now_overflow);
        (ext, depl, scale)
    });
    assert_eq!(ext_overflow, 0);
    assert!(
        (depl_overflow.colony_extraction_pressure.get(&res_tid).copied().unwrap_or(0.0) - 30_000.0)
            .abs()
            < 1e-6,
        "full-storage overflow cycles must not decay colony_extraction_pressure mid-cycle"
    );
    assert_eq!(depl_overflow.colony_pressure_last_unix, t0 + 3_600);
    assert!((scale_overflow - 0.70).abs() < 1e-9);

    // 3. Advance 12h past program expiry (t0 + 7_200 + 43_200):
    // [t0 + 3_600, t0 + 7_200] stays protected (active program), while [t0 + 7_200, +43_200]
    // recovers exact 12h half-life (30_000 -> 15_000 -> yield_scale 0.85).
    clock.advance_secs(46_800);
    handle(
        &ctx,
        &mut sess_b,
        "planetMgr",
        "GetColony",
        &[eve_proto::PyRep::Integer(planet_id)],
        &kw,
        None,
    )
    .expect("GetColony post-expiry handled")
    .expect("GetColony post-expiry ok");

    let (post_pressure, post_scale) = ctx.db.with_snapshot(|snap| {
        let depl = load_planet_depletion(snap, planet_id);
        (
            depl.colony_extraction_pressure.get(&res_tid).copied().unwrap_or(0.0),
            planet_resource_yield_scale(&ctx.sde, snap, planet_id, res_tid, ctx.now_unix()),
        )
    });
    assert!(
        (post_pressure - 15_000.0).abs() < 1e-3,
        "exact post-expiry 12h idle interval must halve 30000 -> 15000, got {post_pressure}"
    );
    assert!((post_scale - 0.85).abs() < 1e-6);
}

#[test]
fn test_bf01_gm_and_local_depletion_monotonic_across_two_colonies_and_staggered_ecus() {
    use crate::services::industry_pi::list_sde_planet_p0_resources;
    use crate::services::industry_pi::pi_helpers::{
        advance_colony_and_update_depletion, save_pi_network, PiColonyNetworkState, PiLinkState,
        PiPinState, PiRouteState,
    };

    let t0 = 1_700_000_000_u64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;

    // Part (a): Two active colonies on the same planet settling the same 4h window must
    // decay GM depletion, gm_depletion_rate, and local_depletion strictly once (no rewind).
    let ctx_a = make_ctx();
    let clock_a = Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0));
    ctx_a.set_clock(clock_a.clone());
    let p0_res = list_sde_planet_p0_resources(&ctx_a, sys_id, planet_id)
        .first()
        .copied()
        .unwrap_or(2268);
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;

    let make_single_ecu_network = |base_pin: i64, cyc_secs: u64, res_tid: u32| PiColonyNetworkState {
        pins: vec![
            PiPinState {
                pin_id: base_pin,
                type_id: 2848,
                latitude: 0.20,
                longitude: 0.40,
                extractor_resource_type_id: Some(res_tid),
                extractor_cycle_seconds: cyc_secs,
                extractor_head_count: 2,
                extractor_head_radius: 0.05,
                program_install_unix: t0,
                expiry_time_unix: Some(t0 + 86_400),
                ..Default::default()
            },
            PiPinState {
                pin_id: base_pin + 1,
                type_id: 2256,
                latitude: 0.25,
                longitude: 0.45,
                ..Default::default()
            },
        ],
        links: vec![PiLinkState {
            source_pin_id: base_pin,
            dest_pin_id: base_pin + 1,
            level: 1,
            length_km: 10.0,
            ..Default::default()
        }],
        routes: vec![PiRouteState {
            route_id: base_pin + 10,
            path: vec![base_pin, base_pin + 1],
            commodity_type_id: res_tid,
            quantity: 1_000,
        }],
        last_simulated_unix: t0,
        ..Default::default()
    };

    let mut net_a = make_single_ecu_network(710_001, 1_800, p0_res);
    let mut net_b = make_single_ecu_network(720_001, 1_800, p0_res);
    let mut col_a = PlanetaryColonyRecord {
        planet_id,
        owner_id: char_a,
        solar_system_id: sys_id,
        command_center_level: 3,
        pins_count: 2,
        extracted_commodities: HashMap::new(),
        last_extracted_unix: t0,
    };
    let mut col_b = PlanetaryColonyRecord {
        planet_id,
        owner_id: char_b,
        solar_system_id: sys_id,
        command_center_level: 3,
        pins_count: 2,
        extracted_commodities: HashMap::new(),
        last_extracted_unix: t0,
    };

    ctx_a.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert((char_a, planet_id), col_a.clone());
        snap.planetary_colonies.insert((char_b, planet_id), col_b.clone());
        save_pi_network(snap, char_a, planet_id, &net_a);
        save_pi_network(snap, char_b, planet_id, &net_b);
        let mut depl = PlanetResourceDepletionState {
            planet_id,
            gm_depletion_rate: 0.40,
            last_updated_unix: t0,
            ..Default::default()
        };
        depl.resource_depletion.insert(p0_res, 0.40);
        depl.local_depletion.push(PiDepletionSpot {
            latitude: -0.9,
            longitude: -0.9,
            radius: 0.05,
            resource_type_id: p0_res,
            amount_depleted: 0.40,
        });
        save_planet_depletion(snap, &depl);
    });

    clock_a.advance_secs(14_400);
    let expected_4h = 0.40 * 0.5_f64.powf(14_400.0 / PI_DEPLETION_RECOVERY_HALF_LIFE_SECS);

    let after_a = ctx_a.db.with_mut_snapshot(|snap| {
        advance_colony_and_update_depletion(
            &ctx_a,
            snap,
            planet_id,
            &mut col_a,
            &mut net_a,
            t0,
            14_400,
            t0 + 14_400,
            true,
        );
        snap.planetary_colonies.insert((char_a, planet_id), col_a.clone());
        save_pi_network(snap, char_a, planet_id, &net_a);
        load_planet_depletion(snap, planet_id)
    });
    assert_eq!(after_a.last_updated_unix, t0 + 14_400);
    assert!(
        (after_a.resource_depletion.get(&p0_res).copied().unwrap_or(0.0) - expected_4h).abs()
            < 1e-6
    );
    assert!((after_a.gm_depletion_rate - expected_4h).abs() < 1e-6);
    assert!((after_a.local_depletion[0].amount_depleted - expected_4h).abs() < 1e-6);

    let after_b = ctx_a.db.with_mut_snapshot(|snap| {
        advance_colony_and_update_depletion(
            &ctx_a,
            snap,
            planet_id,
            &mut col_b,
            &mut net_b,
            t0,
            14_400,
            t0 + 14_400,
            true,
        );
        snap.planetary_colonies.insert((char_b, planet_id), col_b.clone());
        save_pi_network(snap, char_b, planet_id, &net_b);
        load_planet_depletion(snap, planet_id)
    });
    assert_eq!(after_b.last_updated_unix, t0 + 14_400);
    assert!(
        (after_b.resource_depletion.get(&p0_res).copied().unwrap_or(0.0) - expected_4h).abs()
            < 1e-6,
        "second colony must not rewind last_updated_unix or double-decay GM resource_depletion"
    );
    assert!(
        (after_b.gm_depletion_rate - expected_4h).abs() < 1e-6,
        "second colony must not double-decay gm_depletion_rate"
    );
    assert!(
        (after_b.local_depletion[0].amount_depleted - expected_4h).abs() < 1e-6,
        "second colony must not double-decay local_depletion"
    );

    // Part (b): Single colony with two staggered ECUs (900s + 3600s cycles) sharing cycle_end = t0 + 3600
    // where ECU2 has cycle_start = t0 earlier than ECU1's 4th cycle_start = t0 + 2700.
    let ctx_b = make_ctx();
    let clock_b = Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0));
    ctx_b.set_clock(clock_b.clone());
    let char_stagger = 90_000_001_i64;
    let mut col_stagger = PlanetaryColonyRecord {
        planet_id,
        owner_id: char_stagger,
        solar_system_id: sys_id,
        command_center_level: 3,
        pins_count: 3,
        extracted_commodities: HashMap::new(),
        last_extracted_unix: t0,
    };
    let mut net_stagger = PiColonyNetworkState {
        pins: vec![
            PiPinState {
                pin_id: 730_001,
                type_id: 2848,
                latitude: 0.15,
                longitude: 0.35,
                extractor_resource_type_id: Some(p0_res),
                extractor_cycle_seconds: 900,
                extractor_head_count: 2,
                extractor_head_radius: 0.05,
                program_install_unix: t0,
                expiry_time_unix: Some(t0 + 86_400),
                ..Default::default()
            },
            PiPinState {
                pin_id: 730_002,
                type_id: 2848,
                latitude: 0.25,
                longitude: 0.45,
                extractor_resource_type_id: Some(p0_res),
                extractor_cycle_seconds: 3_600,
                extractor_head_count: 2,
                extractor_head_radius: 0.05,
                program_install_unix: t0,
                expiry_time_unix: Some(t0 + 86_400),
                ..Default::default()
            },
            PiPinState {
                pin_id: 730_003,
                type_id: 2256,
                latitude: 0.20,
                longitude: 0.40,
                ..Default::default()
            },
        ],
        links: vec![
            PiLinkState {
                source_pin_id: 730_001,
                dest_pin_id: 730_003,
                level: 1,
                length_km: 10.0,
                ..Default::default()
            },
            PiLinkState {
                source_pin_id: 730_002,
                dest_pin_id: 730_003,
                level: 1,
                length_km: 10.0,
                ..Default::default()
            },
        ],
        routes: vec![
            PiRouteState {
                route_id: 730_011,
                path: vec![730_001, 730_003],
                commodity_type_id: p0_res,
                quantity: 1_000,
            },
            PiRouteState {
                route_id: 730_012,
                path: vec![730_002, 730_003],
                commodity_type_id: p0_res,
                quantity: 1_000,
            },
        ],
        last_simulated_unix: t0,
        ..Default::default()
    };

    ctx_b.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies
            .insert((char_stagger, planet_id), col_stagger.clone());
        save_pi_network(snap, char_stagger, planet_id, &net_stagger);
        let mut depl = PlanetResourceDepletionState {
            planet_id,
            gm_depletion_rate: 0.40,
            last_updated_unix: t0,
            ..Default::default()
        };
        depl.resource_depletion.insert(p0_res, 0.40);
        depl.local_depletion.push(PiDepletionSpot {
            latitude: -0.9,
            longitude: -0.9,
            radius: 0.05,
            resource_type_id: p0_res,
            amount_depleted: 0.40,
        });
        save_planet_depletion(snap, &depl);
    });

    clock_b.advance_secs(3_600);
    let expected_1h = 0.40 * 0.5_f64.powf(3_600.0 / PI_DEPLETION_RECOVERY_HALF_LIFE_SECS);
    let after_stagger = ctx_b.db.with_mut_snapshot(|snap| {
        advance_colony_and_update_depletion(
            &ctx_b,
            snap,
            planet_id,
            &mut col_stagger,
            &mut net_stagger,
            t0,
            3_600,
            t0 + 3_600,
            true,
        );
        load_planet_depletion(snap, planet_id)
    });
    assert_eq!(after_stagger.last_updated_unix, t0 + 3_600);
    assert!(
        (after_stagger.resource_depletion.get(&p0_res).copied().unwrap_or(0.0) - expected_1h).abs()
            < 1e-6,
        "staggered ECU cycles must decay GM resource_depletion strictly once for 1h"
    );
    assert!(
        (after_stagger.gm_depletion_rate - expected_1h).abs() < 1e-6,
        "staggered ECU cycles must decay gm_depletion_rate strictly once for 1h"
    );
    assert!(
        (after_stagger.local_depletion[0].amount_depleted - expected_1h).abs() < 1e-6,
        "staggered ECU cycles must decay local_depletion strictly once for 1h"
    );
}

#[test]
fn test_bf02_active_resource_b_does_not_suppress_resource_a_idle_recovery() {
    use crate::services::industry_pi::list_sde_planet_p0_resources;
    use crate::services::industry_pi::pi_helpers::{
        advance_colony_and_update_depletion, save_pi_network, PiColonyNetworkState, PiLinkState,
        PiPinState, PiRouteState,
    };

    let t0 = 1_700_000_000_u64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;
    let ctx = make_ctx();
    let clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0));
    ctx.set_clock(clock.clone());

    let p0_types = list_sde_planet_p0_resources(&ctx, sys_id, planet_id);
    let p0_a = p0_types.first().copied().unwrap_or(2268);
    let p0_b = p0_types
        .iter()
        .copied()
        .find(|&t| t != p0_a)
        .unwrap_or(2310);
    assert_ne!(p0_a, p0_b);

    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;

    // Colony A holds only a zero-install legacy pin on p0_a (must NOT create a [0, t0+86400) window).
    let net_a = PiColonyNetworkState {
        pins: vec![PiPinState {
            pin_id: 740_001,
            type_id: 2848,
            extractor_resource_type_id: Some(p0_a),
            extractor_cycle_seconds: 1_800,
            extractor_head_count: 1,
            program_install_unix: 0,
            expiry_time_unix: Some(t0 + 86_400),
            ..Default::default()
        }],
        last_simulated_unix: t0,
        ..Default::default()
    };
    let mut col_b = PlanetaryColonyRecord {
        planet_id,
        owner_id: char_b,
        solar_system_id: sys_id,
        command_center_level: 3,
        pins_count: 2,
        extracted_commodities: HashMap::new(),
        last_extracted_unix: t0,
    };
    let mut net_b = PiColonyNetworkState {
        pins: vec![
            PiPinState {
                pin_id: 750_001,
                type_id: 2848,
                latitude: 0.20,
                longitude: 0.40,
                extractor_resource_type_id: Some(p0_b),
                extractor_cycle_seconds: 1_800,
                extractor_head_count: 2,
                extractor_head_radius: 0.05,
                program_install_unix: t0,
                expiry_time_unix: Some(t0 + 86_400),
                ..Default::default()
            },
            PiPinState {
                pin_id: 750_002,
                type_id: 2256,
                latitude: 0.25,
                longitude: 0.45,
                ..Default::default()
            },
        ],
        links: vec![PiLinkState {
            source_pin_id: 750_001,
            dest_pin_id: 750_002,
            level: 1,
            length_km: 10.0,
            ..Default::default()
        }],
        routes: vec![PiRouteState {
            route_id: 750_010,
            path: vec![750_001, 750_002],
            commodity_type_id: p0_b,
            quantity: 1_000,
        }],
        last_simulated_unix: t0,
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_a, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_a,
                solar_system_id: sys_id,
                command_center_level: 1,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: t0,
            },
        );
        snap.planetary_colonies.insert((char_b, planet_id), col_b.clone());
        save_pi_network(snap, char_a, planet_id, &net_a);
        save_pi_network(snap, char_b, planet_id, &net_b);

        // Seed both p0_a and p0_b via legacy scalar timestamp `colony_pressure_last_unix = t0`.
        let mut depl = PlanetResourceDepletionState {
            planet_id,
            colony_pressure_last_unix: t0,
            ..Default::default()
        };
        depl.colony_extraction_pressure.insert(p0_a, 30_000.0);
        depl.colony_extraction_pressure.insert(p0_b, 30_000.0);
        save_planet_depletion(snap, &depl);
    });

    // Advance 6h (21,600s) and settle Colony B (active only on p0_b):
    // p0_a must recover 6h while p0_b stays un-decayed and gains pressure from extraction.
    clock.advance_secs(21_600);
    let expected_6h_a = 30_000.0 * 0.5_f64.powf(0.5);
    let depl_6h = ctx.db.with_mut_snapshot(|snap| {
        advance_colony_and_update_depletion(
            &ctx,
            snap,
            planet_id,
            &mut col_b,
            &mut net_b,
            t0,
            21_600,
            t0 + 21_600,
            true,
        );
        snap.planetary_colonies.insert((char_b, planet_id), col_b.clone());
        save_pi_network(snap, char_b, planet_id, &net_b);
        load_planet_depletion(snap, planet_id)
    });
    let pa_6h = depl_6h.colony_extraction_pressure.get(&p0_a).copied().unwrap_or(0.0);
    let pb_6h = depl_6h.colony_extraction_pressure.get(&p0_b).copied().unwrap_or(0.0);
    assert!(
        (pa_6h - expected_6h_a).abs() < 1e-3,
        "idle p0_a must recover 6h despite active p0_b ECU (expected {expected_6h_a}, got {pa_6h})"
    );
    assert!(
        pb_6h > 30_000.0,
        "active p0_b must not decay mid-cycle before extraction (got {pb_6h})"
    );

    // Advance another 6h (total 12h = 43,200s) and settle Colony B again:
    // p0_a must hit exact 12h half-life (15,000.0, yield_scale = 0.85).
    clock.advance_secs(21_600);
    let (depl_12h, scale_12h) = ctx.db.with_mut_snapshot(|snap| {
        advance_colony_and_update_depletion(
            &ctx,
            snap,
            planet_id,
            &mut col_b,
            &mut net_b,
            t0 + 21_600,
            21_600,
            t0 + 43_200,
            true,
        );
        snap.planetary_colonies.insert((char_b, planet_id), col_b.clone());
        save_pi_network(snap, char_b, planet_id, &net_b);
        let d = load_planet_depletion(snap, planet_id);
        let s = planet_resource_yield_scale(&ctx.sde, snap, planet_id, p0_a, ctx.now_unix());
        (d, s)
    });
    let pa_12h = depl_12h.colony_extraction_pressure.get(&p0_a).copied().unwrap_or(0.0);
    assert!(
        (pa_12h - 15_000.0).abs() < 1e-3,
        "idle p0_a must reach exact 12h half-life 15000, got {pa_12h}"
    );
    assert!((scale_12h - 0.85).abs() < 1e-6);
    assert_eq!(
        depl_12h.colony_pressure_last_unix_by_type.get(&p0_a).copied(),
        Some(t0 + 43_200)
    );

    // Advance another 12h (to 24h idle for p0_a) without touching Colony B:
    // read-only yield_scale for p0_a must reflect full 24h recovery (30000 -> 7500 -> 0.925).
    clock.advance_secs(43_200);
    let scale_24h = ctx.db.with_snapshot(|snap| {
        planet_resource_yield_scale(&ctx.sde, snap, planet_id, p0_a, ctx.now_unix())
    });
    assert!(
        (scale_24h - 0.925).abs() < 1e-6,
        "read-only yield_scale for p0_a after 24h must be 0.925 despite continuous p0_b activity, got {scale_24h}"
    );
}
