//! Extra unit and mutation-killing tests for `services::industry_pi::pi_orbitals`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::industry_pi::pi_orbitals::*;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, PlanetaryColonyRecord,
    TrainedSkillRecord, VirtualServerClock, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx_with_sde<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let _ = db.acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000);
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    ctx
}

fn make_ctx() -> ServerContext {
    make_ctx_with_sde(|_| {})
}

fn char_session(char_id: i64, sys_id: u32) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.solar_system_id = Some(sys_id);
    s
}

fn make_bound(char_id: i64, planet_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "planetMgr".into(),
        object_id: planet_id,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    }
}

fn call_pi(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
    bound: Option<&BoundObjectEntry>,
) -> Result<PyRep, BridgeRpcError> {
    handle_pi_orbitals(ctx, sess, "planetMgr", method, args, &HashMap::new(), bound).unwrap()
}

#[test]
fn pi_commodity_tier_base_cost_covers_all_groups_attributes_and_fallbacks() {
    let ctx = make_ctx_with_sde(|sde| {
        let seed = sde.types.get(&2268).expect("seed type").clone();
        for (tid, gid) in [(990_001, 1032), (990_002, 1033), (990_003, 1035), (990_004, 426)] {
            sde.types.insert(tid, InvType { type_id: tid, group_id: gid, ..seed.clone() });
        }
        for (tid, aid, val, bp) in [
            (990_010, 1641, 1_500.0, 999.0),
            (990_011, 1640, 2_500.0, 999.0),
            (990_012, 1641, 0.0, 3_250.0),
            (990_013, 1641, 0.0, 0.0),
        ] {
            let mut t = InvType { type_id: tid, group_id: 9999, base_price: bp, ..seed.clone() };
            t.default_attributes.clear();
            t.default_attributes.insert(aid, val);
            sde.types.insert(tid, t);
        }
    });

    for tid in [990_001, 990_002, 990_003, 990_004] {
        assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, tid), 5.0);
    }
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 3645), 400.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2329), 7_200.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2344), 60_000.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2867), 1_200_000.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 990_010), 1_500.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 990_011), 2_500.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 990_012), 3_250.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 990_013), 400.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 888_888_888), 400.0);
}

#[test]
fn flush_validation_errors_cooldown_poco_tax_and_bound_moniker_routing() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;
    let poco_corp_id = 98_500_001_i64;
    let kw = HashMap::new();

    let mut unauth = SessionState::default();
    for m in [
        "Flush", "UpgradeCommandCenter", "UserTransferCommodities", "UserLaunchCommodities",
        "GetMyLaunchesDetails", "DeleteLaunch", "CancelInstallProgram", "RevertChanges",
        "StartTicking", "GetProgramResultInfo", "GetLocalDistributionReport", "append",
        "colonies", "id", "planetID", "remoteHandler",
    ] {
        assert!(call_pi(&ctx, &mut unauth, m, &[], None).is_err());
    }
    assert!(handle_pi_orbitals(&ctx, &mut unauth, "config", "Flush", &[], &kw, None).is_none());
    assert!(handle_pi_orbitals(&ctx, &mut unauth, "planetMgr", "", &[], &kw, None).is_none());

    let mut sess = char_session(char_id, sys_id);
    assert_eq!(
        call_pi(&ctx, &mut sess, "Flush", &[PyRep::Integer(planet_id), PyRep::Integer(2268), PyRep::Integer(1)], None).unwrap_err().code,
        "ClientSchematicOverrideForbidden"
    );
    assert_eq!(call_pi(&ctx, &mut sess, "Flush", &[], None).unwrap_err().code, "InvalidPlanetID");
    assert_eq!(
        call_pi(&ctx, &mut sess, "Flush", &[PyRep::Integer(planet_id), PyRep::Integer(34)], None).unwrap_err().code,
        "ClientSchematicOverrideForbidden"
    );
    assert_eq!(
        call_pi(&ctx, &mut sess, "Flush", &[PyRep::Integer(planet_id)], None).unwrap_err().code,
        "ColonyNotFound"
    );

    // Seed colony with NO pins so Flush takes the direct orbital export tax path (`ext_idx.is_none()`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id, owner_id: char_id, solar_system_id: sys_id,
                command_center_level: 2, pins_count: 0,
                extracted_commodities: HashMap::new(), last_extracted_unix: 0,
            },
        );
        save_pi_network(snap, char_id, planet_id, &PiColonyNetworkState {
            cpu_max: 7057.0,
            pg_max: 9000.0,
            pins: vec![PiPinState { pin_id: 701, type_id: 2469, ..Default::default() }],
            ..Default::default()
        });
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = poco_corp_id;
            __corp.name = "POCO Owners".into();
            __corp.ticker = "POCO".into();
            __corp.ceo_id = char_id;
            __corp.set_division_balance(1, IskCents::from_isk(1_000));
            __corp };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(poco_corp_id, corp);
        save_orbital_settings(snap, planet_id, &OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: 2233,
            owner_corp_id: poco_corp_id,
            corporation_tax_rate: 0.05,
            alliance_tax_rate: 0.05,
            standing_high_tax_rate: 0.05,
            standing_good_tax_rate: 0.05,
            standing_neutral_tax_rate: 0.08,
            standing_bad_tax_rate: 0.10,
            standing_horrible_tax_rate: 0.15,
            allow_alliance: true,
            allow_standings: true,
            minimum_standing: -10.0,
        });
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents(0);
        }
    });

    sess.solar_system_id = Some(30_000_144);
    assert_eq!(
        call_pi(&ctx, &mut sess, "Flush", &[PyRep::Integer(planet_id)], None).unwrap_err().code,
        "ColonyNotInCurrentSystem"
    );
    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        call_pi(&ctx, &mut sess, "Flush", &[PyRep::Integer(planet_id)], None).unwrap_err().code,
        "InsufficientFunds"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(100_000);
        }
        snap.skills.entry(char_id).or_default().insert(
            33467,
            TrainedSkillRecord { type_id: 33467, level: 3, skill_points: 40_000.0 },
        );
    });
    let bound = make_bound(char_id, planet_id);
    let flush_ok = call_pi(&ctx, &mut sess, "Flush", &[PyRep::Integer(2268)], Some(&bound)).expect("bound Flush");
    assert!(flush_ok.dict_get("pocoExportTaxISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert_eq!(flush_ok.dict_get("deliveredItemID").and_then(|v| v.as_i64()), Some(planet_id));
    assert!(ctx.db.with_snapshot(|s| s.corporations[&poco_corp_id].master_wallet_balance()) > IskCents::from_isk(1_000));
    assert_eq!(call_pi(&ctx, &mut sess, "Flush", &[], Some(&bound)).unwrap_err().code, "ExtractorCycleNotReady");
}

#[test]
fn upgrade_transfer_launch_import_and_delete_launch_edge_cases() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let planet_id = 40_009_082_i64;
    let other_planet = 40_009_083_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = char_session(char_id, sys_id);
    let bound = make_bound(char_id, planet_id);

    assert_eq!(call_pi(&ctx, &mut sess, "UpgradeCommandCenter", &[], None).unwrap_err().code, "InvalidPlanetID");
    assert_eq!(
        call_pi(&ctx, &mut sess, "UpgradeCommandCenter", &[PyRep::Integer(planet_id)], None).unwrap_err().code,
        "InvalidCommandCenterLevel"
    );
    for bad_lvl in [0, 6] {
        assert_eq!(
            call_pi(&ctx, &mut sess, "UpgradeCommandCenter", &[PyRep::Integer(bad_lvl)], Some(&bound)).unwrap_err().code,
            "InvalidCommandCenterLevel"
        );
    }
    assert_eq!(call_pi(&ctx, &mut sess, "UserTransferCommodities", &[], None).unwrap_err().code, "InvalidPlanetID");
    assert_eq!(call_pi(&ctx, &mut sess, "UserLaunchCommodities", &[], None).unwrap_err().code, "InvalidPlanetID");
    assert_eq!(
        call_pi(&ctx, &mut sess, "UserLaunchCommodities", &[PyRep::Integer(0), PyRep::Integer(10), PyRep::String("Hangar".into())], Some(&bound)).unwrap_err().code,
        "InvalidRoutingParameters"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(10_000_000);
        }
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id, owner_id: char_id, solar_system_id: sys_id,
                command_center_level: 3, pins_count: 2,
                extracted_commodities: HashMap::new(), last_extracted_unix: 0,
            },
        );
        save_pi_network(snap, char_id, planet_id, &PiColonyNetworkState {
            cpu_max: 15_000.0, pg_max: 15_000.0,
            pins: vec![
                PiPinState {
                    pin_id: 401, type_id: 2254, contents: HashMap::from([(3645, 500)]), ..Default::default()
                },
                PiPinState {
                    pin_id: 402, type_id: 2848, extractor_resource_type_id: Some(2268),
                    extractor_head_count: 2, extractor_heads: vec![(1, 0.1, 0.2), (2, 0.12, 0.22)],
                    extractor_head_radius: 0.03, extractor_cycle_seconds: 600, program_install_unix: 100,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
    });

    assert_eq!(
        call_pi(&ctx, &mut sess, "UserLaunchCommodities", &[PyRep::Integer(999), PyRep::Integer(3645), PyRep::Integer(10)], Some(&bound)).unwrap_err().code,
        "PinNotFound"
    );
    assert_eq!(
        call_pi(&ctx, &mut sess, "UserLaunchCommodities", &[PyRep::Integer(402), PyRep::Integer(3645), PyRep::Integer(10)], Some(&bound)).unwrap_err().code,
        "InvalidPinTypeForLaunch"
    );
    assert_eq!(
        call_pi(&ctx, &mut sess, "UserLaunchCommodities", &[PyRep::Integer(401), PyRep::Integer(3645), PyRep::Integer(5_000)], Some(&bound)).unwrap_err().code,
        "CommandCenterLaunchCapacityExceeded"
    );
    assert_eq!(
        call_pi(&ctx, &mut sess, "UserLaunchCommodities", &[PyRep::Integer(3645), PyRep::Integer(100_000), PyRep::String("CustomsOffice".into())], Some(&bound)).unwrap_err().code,
        "LaunchpadCapacityExceeded"
    );
    assert_eq!(
        call_pi(&ctx, &mut sess, "UserLaunchCommodities", &[PyRep::Integer(3645), PyRep::Integer(600), PyRep::String("Hangar".into())], Some(&bound)).unwrap_err().code,
        "InsufficientCommoditiesForRouting"
    );

    let hangar_launch = call_pi(
        &ctx, &mut sess, "UserLaunchCommodities",
        &[PyRep::Integer(3645), PyRep::Integer(100), PyRep::String("Hangar".into())],
        Some(&bound),
    ).expect("Hangar export");
    assert!(hangar_launch.dict_get("launchID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    assert_eq!(
        call_pi(&ctx, &mut sess, "UserLaunchCommodities", &[PyRep::Integer(3645), PyRep::Integer(5_000), PyRep::String("ImportToColony".into())], Some(&bound)).unwrap_err().code,
        "LaunchpadCapacityExceeded"
    );

    // DeleteLaunch error cases: invalid ID, not found, wrong owner, non-planetary item, wrong bound planet
    assert_eq!(call_pi(&ctx, &mut sess, "DeleteLaunch", &[PyRep::Integer(0)], None).unwrap_err().code, "InvalidLaunchID");
    assert_eq!(call_pi(&ctx, &mut sess, "DeleteLaunch", &[PyRep::Integer(999_999_999)], None).unwrap_err().code, "LaunchNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, owner, loc) in [
            (888_101, 27_912, other_char, planet_id),
            (888_102, 34, char_id, 60_003_760),
            (888_103, 27_912, char_id, other_planet),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: owner, location_id: loc,
                flag: InventoryFlag::Hangar, quantity: 1, ..Default::default()
            });
        }
    });
    assert_eq!(call_pi(&ctx, &mut sess, "DeleteLaunch", &[PyRep::Integer(888_101)], None).unwrap_err().code, "LaunchAccessDenied");
    assert_eq!(call_pi(&ctx, &mut sess, "DeleteLaunch", &[PyRep::Integer(888_102)], None).unwrap_err().code, "InvalidLaunchID");
    assert_eq!(call_pi(&ctx, &mut sess, "DeleteLaunch", &[PyRep::Integer(888_103)], Some(&bound)).unwrap_err().code, "LaunchNotFound");
}

#[test]
fn program_result_cancel_revert_ticking_and_moniker_properties() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = char_session(char_id, sys_id);
    let bound = make_bound(char_id, planet_id);

    for m in [
        "CancelInstallProgram", "RevertChanges", "StartTicking", "GetProgramResultInfo",
        "GetLocalDistributionReport", "celestialIndex", "id",
    ] {
        assert_eq!(call_pi(&ctx, &mut sess, m, &[], None).unwrap_err().code, "InvalidPlanetID");
    }
    for m in ["CancelInstallProgram", "RevertChanges", "StartTicking"] {
        assert_eq!(call_pi(&ctx, &mut sess, m, &[], Some(&bound)).unwrap_err().code, "ColonyNotFound");
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id, owner_id: char_id, solar_system_id: sys_id,
                command_center_level: 2, pins_count: 1,
                extracted_commodities: HashMap::new(), last_extracted_unix: 1_775_399_000,
            },
        );
        save_pi_network(snap, char_id, planet_id, &PiColonyNetworkState {
            cpu_max: 7_057.0, pg_max: 9_000.0,
            pins: vec![PiPinState {
                pin_id: 502, type_id: 2848, extractor_resource_type_id: Some(2268),
                extractor_head_count: 2, extractor_heads: vec![(1, 0.0, 0.0), (2, 0.2, 0.2)],
                extractor_head_radius: 0.02, extractor_cycle_seconds: 600,
                program_install_unix: 1_775_399_000, ..Default::default()
            }],
            ..Default::default()
        });
    });

    let prog = call_pi(
        &ctx, &mut sess, "GetProgramResultInfo",
        &[
            PyRep::Integer(502), PyRep::Integer(2268),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0)]),
                PyRep::Tuple(vec![PyRep::Real(0.5), PyRep::Real(0.5)]),
            ]),
        ],
        None,
    ).expect("3-arg GetProgramResultInfo");
    if let PyRep::Tuple(vals) = prog {
        assert_eq!(vals[0].as_i64(), Some(2000));
        assert_eq!(vals[1].as_i64(), Some(600 * 10_000_000));
        assert_eq!(vals[2].as_i64(), Some(10));
    } else {
        panic!("expected Tuple");
    }

    assert_eq!(
        call_pi(&ctx, &mut sess, "CancelInstallProgram", &[PyRep::Integer(999)], Some(&bound)).unwrap_err().code,
        "PinNotFound"
    );
    assert_eq!(
        call_pi(&ctx, &mut sess, "CancelInstallProgram", &[PyRep::Integer(502)], Some(&bound)).unwrap(),
        PyRep::Bool(true)
    );
    assert!(matches!(call_pi(&ctx, &mut sess, "colonies", &[], Some(&bound)).unwrap(), PyRep::Dict(_)));
    assert_eq!(call_pi(&ctx, &mut sess, "id", &[], Some(&bound)).unwrap(), PyRep::Integer(planet_id));
}

#[test]
fn transfer_launch_import_export_and_sim_remaining_branches() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let planet_id = 40_009_082_i64;
    let missing_planet = 40_999_999_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = char_session(char_id, sys_id);
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    sess.ship_id = Some(ship_id);
    let bound = make_bound(char_id, planet_id);

    // 1. UserTransferCommodities unbound success (`L457-460`) + Launch ColonyNotFound (`L529-534`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                command_center_level: 2,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        save_pi_network(
            snap,
            char_id,
            planet_id,
            &PiColonyNetworkState {
                cpu_max: 7_057.0,
                pg_max: 9_000.0,
                last_simulated_unix: 1_775_399_000,
                pins: vec![
                    PiPinState {
                        pin_id: 701,
                        type_id: 2254, // Launchpad
                        contents: HashMap::from([(2268, 50)]),
                        ..Default::default()
                    },
                    PiPinState {
                        pin_id: 702,
                        type_id: 2541, // Storage
                        ..Default::default()
                    },
                ],
                links: vec![PiLinkState {
                    source_pin_id: 701,
                    dest_pin_id: 702,
                    level: 1,
                    bandwidth: 2500.0,
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
    });
    assert!(call_pi(
        &ctx,
        &mut sess,
        "UserTransferCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![PyRep::Integer(701), PyRep::Integer(702)]),
            PyRep::Dict(vec![(PyRep::Integer(2268), PyRep::Integer(5))]),
        ],
        None,
    )
    .is_ok());

    // ColonyNotFound on UserLaunchCommodities + Import PinNotFound + Launch PinNotFound
    assert_eq!(
        call_pi(
            &ctx,
            &mut sess,
            "UserLaunchCommodities",
            &[PyRep::Integer(missing_planet), PyRep::Integer(2268), PyRep::Integer(5), PyRep::String("Storage".into())],
            None,
        )
        .unwrap_err()
        .code,
        "ColonyNotFound"
    );

    // ColonyNotInCurrentSystem on bound UserLaunchCommodities with explicit planetID prefix (`L473`, `L537-543`)
    sess.solar_system_id = Some(30_000_144);
    assert_eq!(
        call_pi(
            &ctx,
            &mut sess,
            "UserLaunchCommodities",
            &[PyRep::Integer(planet_id), PyRep::Integer(701), PyRep::Integer(2268), PyRep::Integer(2), PyRep::String("CommandCenter".into())],
            Some(&bound),
        )
        .unwrap_err()
        .code,
        "ColonyNotInCurrentSystem"
    );
    sess.solar_system_id = Some(sys_id);

    // Short 2-arg form (`L505-508`) & Import InsufficientFunds (`L596-598`) + Export InsufficientFunds (`L690-692`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents(0);
        }
    });
    assert_eq!(
        call_pi(
            &ctx,
            &mut sess,
            "UserLaunchCommodities",
            &[PyRep::Integer(9999), PyRep::Integer(2268), PyRep::Integer(5)],
            Some(&bound),
        )
        .unwrap_err()
        .code,
        "PinNotFound"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            77_700_001,
            eve_db::InventoryItemRecord {
                item_id: 77_700_001,
                type_id: 2268,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_pi(
            &ctx,
            &mut sess,
            "UserLaunchCommodities",
            &[PyRep::Integer(2268), PyRep::Integer(10), PyRep::String("ImportToColony".into())],
            Some(&bound),
        )
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    assert_eq!(
        call_pi(
            &ctx,
            &mut sess,
            "UserLaunchCommodities",
            &[PyRep::Integer(2268), PyRep::Integer(10), PyRep::String("CustomsOffice".into())],
            Some(&bound),
        )
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );

    // Empty network: PinNotFound for Import and Export + dummy pin in GetLocalDistributionReport (`L563, 649, 1243`)
    let empty_planet = 40_009_084_i64;
    let empty_bound = make_bound(char_id, empty_planet);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(1_000_000);
        }
        snap.planetary_colonies.insert(
            (char_id, empty_planet),
            PlanetaryColonyRecord {
                planet_id: empty_planet,
                owner_id: char_id,
                solar_system_id: sys_id,
                command_center_level: 0,
                pins_count: 0,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        save_pi_network(snap, char_id, empty_planet, &PiColonyNetworkState {
            pins: vec![PiPinState {
                pin_id: 99,
                type_id: 2469, // Basic Industry (not a Launchpad/Storage/CommandCenter)
                ..Default::default()
            }],
            last_simulated_unix: 1_775_399_000,
            ..Default::default()
        });
    });
    assert_eq!(
        call_pi(
            &ctx,
            &mut sess,
            "UserLaunchCommodities",
            &[PyRep::Integer(2268), PyRep::Integer(1), PyRep::String("ImportToColony".into())],
            Some(&empty_bound),
        )
        .unwrap_err()
        .code,
        "PinNotFound"
    );
    assert_eq!(
        call_pi(
            &ctx,
            &mut sess,
            "UserLaunchCommodities",
            &[PyRep::Integer(2268), PyRep::Integer(1), PyRep::String("ExportFromColony".into())],
            Some(&empty_bound),
        )
        .unwrap_err()
        .code,
        "PinNotFound"
    );
    assert!(call_pi(&ctx, &mut sess, "StartTicking", &[], Some(&empty_bound)).is_ok());
    assert!(call_pi(&ctx, &mut sess, "GetLocalDistributionReport", &[PyRep::Integer(999)], Some(&empty_bound)).is_ok());
    // Bound GetProgramResultInfo with & without planet_id prefix (`L1129-1145`)
    assert!(call_pi(
        &ctx,
        &mut sess,
        "GetProgramResultInfo",
        &[PyRep::Integer(empty_planet), PyRep::Integer(1), PyRep::Integer(2268), PyRep::List(vec![]), PyRep::Real(0.02)],
        Some(&empty_bound),
    ).is_ok());
    assert!(call_pi(
        &ctx,
        &mut sess,
        "GetProgramResultInfo",
        &[PyRep::Integer(1), PyRep::Integer(2268)],
        Some(&empty_bound),
    ).is_ok());
}
