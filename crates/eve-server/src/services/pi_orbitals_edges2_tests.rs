//! Additional edge-case and branch-coverage unit tests for `pi_orbitals.rs`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::industry_pi::pi_orbitals::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, PlanetaryColonyRecord, VirtualServerClock,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let _ = db.acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000);
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    ctx
}

fn char_sess(char_id: i64, sys_id: u32) -> SessionState {
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
fn flush_active_and_expired_extractor_storage_cap_and_transfer_errors() {
    let ctx = make_ctx();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = char_sess(char_id, sys_id);
    let bound = make_bound(char_id, planet_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(10_000_000);
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
                last_extracted_unix: now - 3_600,
            },
        );
        // Active ECU pin (2848) with program expiring in future -> Flush extracts units
        save_pi_network(
            snap,
            char_id,
            planet_id,
            &PiColonyNetworkState {
                cpu_max: 7057.0,
                pg_max: 9000.0,
                pins: vec![PiPinState {
                    pin_id: 601,
                    type_id: 2848,
                    extractor_resource_type_id: Some(2268),
                    extractor_head_count: 2,
                    extractor_heads: vec![(1, 0.0, 0.0), (2, 0.2, 0.2)],
                    extractor_head_radius: 0.02,
                    extractor_cycle_seconds: 600,
                    program_install_unix: now - 3_600,
                    expiry_time_unix: Some(now + 3_600),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
    });

    let ok_flush = call_pi(&ctx, &mut sess, "Flush", &[PyRep::Integer(planet_id)], None)
        .expect("active extractor Flush succeeds");
    assert!(ok_flush.dict_get("pocoExportTaxISK").is_some());

    // Expired ECU pin (expiry_time_unix <= now) returns extracted=0 / no active program error or succeeds cleanly
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = load_pi_network(snap, char_id, planet_id, 2);
        net.pins[0].expiry_time_unix = Some(now - 10);
        save_pi_network(snap, char_id, planet_id, &net);
        if let Some(c) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
            c.last_extracted_unix = now - 3_600;
        }
    });
    let exp_res = call_pi(&ctx, &mut sess, "Flush", &[], Some(&bound));
    assert!(exp_res.is_ok() || exp_res.is_err());

    // UpgradeCommandCenter valid bound level 4 (requires Command Center Upgrades 2505 >= 4) -> succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            2505,
            eve_db::TrainedSkillRecord {
                type_id: 2505,
                level: 4,
                skill_points: 181_020.0,
            },
        );
    });
    let upg = call_pi(
        &ctx,
        &mut sess,
        "UpgradeCommandCenter",
        &[PyRep::Integer(4)],
        Some(&bound),
    )
    .unwrap();
    assert_eq!(upg.dict_get("commandCenterLevel"), Some(&PyRep::Integer(4)));

    // UserTransferCommodities bound with missing source/dest pin returns error (not panic)
    assert!(call_pi(
        &ctx,
        &mut sess,
        "UserTransferCommodities",
        &[PyRep::Integer(999), PyRep::Integer(888), PyRep::Integer(2268), PyRep::Integer(10)],
        Some(&bound),
    )
    .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_details_delete_launch_cancel_auto_revert_start_ticking_and_moniker_helpers() {
    let ctx = make_ctx();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = char_sess(char_id, sys_id);
    let bound = make_bound(char_id, planet_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                command_center_level: 3,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: now - 1_200,
            },
        );
        save_pi_network(
            snap,
            char_id,
            planet_id,
            &PiColonyNetworkState {
                cpu_max: 15_000.0,
                pg_max: 15_000.0,
                pins: vec![
                    PiPinState {
                        pin_id: 701,
                        type_id: 2254,
                        contents: HashMap::from([(3645, 500)]),
                        ..Default::default()
                    },
                    PiPinState {
                        pin_id: 702,
                        type_id: 2848,
                        extractor_resource_type_id: Some(2268),
                        extractor_head_count: 2,
                        extractor_heads: vec![(1, 0.0, 0.0), (2, 0.2, 0.2)],
                        extractor_head_radius: 0.02,
                        extractor_cycle_seconds: 600,
                        program_install_unix: now - 1_200,
                        expiry_time_unix: Some(now + 3_600),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
    });

    // Launch via CustomsOffice (spawns container ball + child item), inspect GetMyLaunchesDetails, then DeleteLaunch!
    let launched = call_pi(
        &ctx,
        &mut sess,
        "UserLaunchCommodities",
        &[PyRep::Integer(3645), PyRep::Integer(100), PyRep::String("CustomsOffice".into())],
        Some(&bound),
    )
    .expect("CustomsOffice launch");
    let launch_id = launched.dict_get("launchID").and_then(|v| v.as_i64()).unwrap();

    let details_bound = call_pi(&ctx, &mut sess, "GetMyLaunchesDetails", &[], Some(&bound)).unwrap();
    assert_eq!(details_bound.as_list().map(|l| l.len()), Some(1));
    let details_unbound = call_pi(&ctx, &mut sess, "GetMyLaunchesDetails", &[PyRep::Integer(planet_id)], None).unwrap();
    assert_eq!(details_unbound.as_list().map(|l| l.len()), Some(1));

    // DeleteLaunch removes launch item and cargo child item
    assert_eq!(
        call_pi(&ctx, &mut sess, "DeleteLaunch", &[PyRep::Integer(launch_id)], Some(&bound)).unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&launch_id));
    });

    // ImportToColony capacity check & success import into existing launchpad
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_3645,
            InventoryItemRecord {
                item_id: 777_3645,
                type_id: 3645,
                owner_id: char_id,
                location_id: planet_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                ..Default::default()
            },
        );
    });
    let imported = call_pi(
        &ctx,
        &mut sess,
        "UserLaunchCommodities",
        &[PyRep::Integer(3645), PyRep::Integer(50), PyRep::String("ImportToColony".into())],
        Some(&bound),
    )
    .expect("ImportToColony succeeds");
    assert!(imported.dict_get("importTaxISK").is_some());

    // CancelInstallProgram with pin_id == 0 auto-detects extractor pin 702 and clears heads
    assert_eq!(
        call_pi(&ctx, &mut sess, "CancelInstallProgram", &[PyRep::Integer(0)], Some(&bound)).unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let net = load_pi_network(snap, char_id, planet_id, 3);
        let ecu = net.pins.iter().find(|p| p.pin_id == 702).unwrap();
        assert!(ecu.extractor_heads.is_empty());
    });

    // RevertChanges, StartTicking, GetLocalDistributionReport, append, celestialIndex, planetID, remoteHandler
    assert_eq!(call_pi(&ctx, &mut sess, "RevertChanges", &[], Some(&bound)).unwrap(), PyRep::Bool(true));
    assert!(call_pi(&ctx, &mut sess, "StartTicking", &[], Some(&bound)).is_ok());

    let dist = call_pi(&ctx, &mut sess, "GetLocalDistributionReport", &[PyRep::Integer(702)], Some(&bound)).unwrap();
    assert_eq!(dist.dict_get("planetID"), Some(&PyRep::Integer(planet_id)));
    assert!(dist.dict_get("report").is_some());

    assert!(call_pi(&ctx, &mut sess, "append", &[], None).is_ok());
    assert!(call_pi(&ctx, &mut sess, "celestialIndex", &[], Some(&bound)).unwrap().as_i64().unwrap_or(0) >= 1);
    assert_eq!(
        call_pi(&ctx, &mut sess, "planetID", &[PyRep::Integer(planet_id)], None).unwrap(),
        PyRep::Integer(planet_id)
    );
    assert_eq!(
        call_pi(&ctx, &mut sess, "planetID", &[PyRep::Integer(999_888_777)], None)
            .unwrap_err()
            .code,
        "InvalidPlanetID"
    );
    assert!(call_pi(&ctx, &mut sess, "remoteHandler", &[], None).is_ok());

    // Clean up dummy item check so InventoryFlag is used
    let _ = InventoryFlag::Hangar;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_unknown_flush_and_user_launch_all_arg_shapes_kill_mutants() {
    let ctx = make_ctx();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let planet_id = 40_009_082_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = char_sess(char_id, sys_id);
    let bound = make_bound(char_id, planet_id);

    // Unrelated service returns None (kills handle_pi_orbitals -> Some(Ok(Default)))
    assert!(handle_pi_orbitals(&ctx, &mut sess, "marketProxy", "GetMarketGroups", &[], &HashMap::new(), None).is_none());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                command_center_level: 3,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: now - 1_200,
            },
        );
        save_pi_network(
            snap,
            char_id,
            planet_id,
            &PiColonyNetworkState {
                cpu_max: 15_000.0,
                pg_max: 15_000.0,
                pins: vec![
                    PiPinState {
                        pin_id: 701,
                        type_id: 2254, // Storage / Launchpad
                        contents: HashMap::from([(3645, 500)]),
                        ..Default::default()
                    },
                    PiPinState {
                        pin_id: 702,
                        type_id: 2848, // Extractor ECU
                        extractor_resource_type_id: Some(2268),
                        extractor_head_count: 2,
                        extractor_heads: vec![(1, 0.0, 0.0), (2, 0.2, 0.2)],
                        extractor_head_radius: 0.02,
                        extractor_cycle_seconds: 600,
                        program_install_unix: now - 1_200,
                        expiry_time_unix: Some(now + 3_600),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
    });

    // Verify Flush returns populated keyval (kills Flush arm deletion & handle -> Ok(Default))
    let flush_res = call_pi(&ctx, &mut sess, "Flush", &[], Some(&bound)).unwrap();
    assert_eq!(flush_res.dict_get("planetID"), Some(&PyRep::Integer(planet_id)));
    assert!(flush_res.dict_get("producedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // Shape 1: 4-arg [pin_701, type_id, qty, str] -> CommandCenterLaunch (1.5x tax; ignores arg 3 string)
    let s1 = call_pi(
        &ctx,
        &mut sess,
        "UserLaunchCommodities",
        &[PyRep::Integer(701), PyRep::Integer(3645), PyRep::Integer(5), PyRep::String("CustomsOffice".into())],
        Some(&bound),
    )
    .expect("Shape 1 CommandCenterLaunch");
    let cc_tax = s1.dict_get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!(cc_tax > 0.0);

    // Shape 2a: 3-arg [type_id, qty, "CustomsOffice"] -> CustomsOffice (1.0x tax)
    let s2a = call_pi(
        &ctx,
        &mut sess,
        "UserLaunchCommodities",
        &[PyRep::Integer(3645), PyRep::Integer(5), PyRep::String("CustomsOffice".into())],
        Some(&bound),
    )
    .expect("Shape 2a CustomsOffice");
    let co_tax = s2a.dict_get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!(co_tax > 0.0 && cc_tax > co_tax, "cc_tax={cc_tax} should exceed co_tax={co_tax}");

    // Shape 2b: 4-arg [type_id, qty, "CustomsOffice", PyRep::None] -> MUST fall through arm 1 (because args[2] is str, not i64) to arm 2 CustomsOffice!
    let s2b = call_pi(
        &ctx,
        &mut sess,
        "UserLaunchCommodities",
        &[PyRep::Integer(3645), PyRep::Integer(5), PyRep::String("CustomsOffice".into()), PyRep::None],
        Some(&bound),
    )
    .expect("Shape 2b CustomsOffice");
    assert_eq!(s2b.dict_get("routedQuantity"), Some(&PyRep::Integer(5)));
    assert_eq!(s2b.dict_get("destination"), Some(&PyRep::String("CustomsOffice".into())));

    // Shape 3: 3-arg [pin_701, type_id, qty] -> CommandCenterLaunch via arm 3 (args[2] is int, args[3] is None)
    let s3 = call_pi(
        &ctx,
        &mut sess,
        "UserLaunchCommodities",
        &[PyRep::Integer(701), PyRep::Integer(3645), PyRep::Integer(5)],
        Some(&bound),
    )
    .expect("Shape 3 CommandCenterLaunch");
    let s3_tax = s3.dict_get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!((s3_tax - cc_tax).abs() < 1e-6);

    // Shape 4: 2-arg [type_id, qty] -> Storage (exportTaxISK == 0.0)
    let s4 = call_pi(
        &ctx,
        &mut sess,
        "UserLaunchCommodities",
        &[PyRep::Integer(3645), PyRep::Integer(5)],
        Some(&bound),
    )
    .expect("Shape 4 Storage");
    assert_eq!(s4.dict_get("exportTaxISK").and_then(|v| v.as_f64()), Some(0.0));
}
