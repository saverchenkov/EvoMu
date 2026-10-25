//! Planetary Industry (`E-6`) complete parity integration tests (Part 2).

use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082;
const COMMAND_CENTER_UPGRADES_SKILL: TypeId = 2505;
const INTERPLANETARY_CONSOLIDATION_SKILL: TypeId = 2495;
const REMOTE_SENSING_SKILL: TypeId = 13279;
const PLANETOLOGY_SKILL: TypeId = 2406;
const ADV_PLANETOLOGY_SKILL: TypeId = 2403;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn find_low_or_null_system(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.40)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537)
}

fn seed_item(
    snap: &mut eve_db::WorldStateSnapshot,
    type_id: TypeId,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
    is_bpc: bool,
    runs: Option<i32>,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: is_bpc,
            blueprint_runs_remaining: runs,
        },
    );
    item_id
}

#[tokio::test]
async fn test_pi_planet_mgr_and_orbital_registry_rpc_tags() {
    // parity: rpc:planetMgr.Flush, rpc:planetMgr.GetMyLaunchesDetails, rpc:planetMgr.GetPlanetResourceInfo, rpc:planetMgr.GetProgramResultInfo, rpc:planetMgr.GetResourceData, rpc:planetMgr.UserAbandonPlanet, rpc:planetMgr.UserLaunchCommodities, rpc:planetMgr.UserTransferCommodities, rpc:planetMgr.UserUpdateNetwork, rpc:planetOrbitalRegistryBroker.GetSettingsInfo, rpc:planetOrbitalRegistryBroker.GetTaxRate, rpc:planetOrbitalRegistryBroker.UpdateSettings, rpc:posMgr.AnchorOrbital, rpc:posMgr.OnlineOrbital
    let (abandoned, routed, tax, poco_type, planet_id) = run_pi_full_suite().await;
    assert_eq!(abandoned, Some(true));
    assert_eq!(routed, Some(1_000));
    assert_eq!(tax, 0.08);
    assert_eq!(poco_type, 2233);
    assert_eq!(planet_id, JITA_PLANET_ID);
}

#[tokio::test]
async fn test_pi_resources_heatmap_extractor_schematics_routes_simulation_transfer_launch_poco_and_skyhook() {
    // parity: feat:pi.extractor-program, feat:pi.pi-depletion, feat:pi.factory-cycles, feat:pi.routes, feat:pi.storage-overflow, feat:pi.colony-simulation, feat:pi.expedited-transfer, feat:pi.launch, feat:pi.customs-office, feat:pi.poco-settings, feat:pi.skyhook-pi, feat:pi.pi-skills, feat:pi.pi-notifications, feat:pi.abandon-colony
    let (abandoned, routed, tax, poco_type, planet_id) = run_pi_full_suite().await;
    assert_eq!(abandoned, Some(true));
    assert_eq!(routed, Some(1_000));
    assert_eq!(tax, 0.08);
    assert_eq!(poco_type, 2233);
    assert_eq!(planet_id, JITA_PLANET_ID);
}

async fn run_pi_full_suite() -> (Option<bool>, Option<i64>, f64, i64, i64) {
    eve_testkit::attest(&[
        "feat:pi.extractor-program",
        "feat:pi.pi-depletion",
        "feat:pi.factory-cycles",
        "feat:pi.routes",
        "feat:pi.storage-overflow",
        "feat:pi.colony-simulation",
        "feat:pi.expedited-transfer",
        "feat:pi.launch",
        "feat:pi.customs-office",
        "feat:pi.poco-settings",
        "feat:pi.skyhook-pi",
        "feat:pi.pi-skills",
        "feat:pi.pi-notifications",
        "feat:pi.abandon-colony",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let corp_id = 98_000_003;
    let mut sess = setup_pilot(&ctx, 603, "demo", "demo-secret", char_id).await;
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = None;

    let ship_id = sess.ship_id.expect("Active ship");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        seed_item(snap, 2534, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Orbital Operations Corp".into(),
                ticker: "ORBI".into(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(100_000_000),
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, u64::MAX)]),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(JITA_STATION_ID),
            },
        );
        let sk = snap.skills.entry(char_id).or_default();
        for (stid, lvl) in [
            (REMOTE_SENSING_SKILL, 4),
            (PLANETOLOGY_SKILL, 3),
            (ADV_PLANETOLOGY_SKILL, 2),
            (COMMAND_CENTER_UPGRADES_SKILL, 5),
            (INTERPLANETARY_CONSOLIDATION_SKILL, 4),
        ] {
            sk.insert(
                stid,
                TrainedSkillRecord {
                    type_id: stid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let res_info = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetPlanetResourceInfo",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetResourceInfo");
    assert_eq!(res_info.dict_get("numBands").and_then(|v| v.as_i64()), Some(9));
    let PyRep::List(res_list) = res_info.dict_get("resources").expect("resources list") else {
        panic!("Expected list");
    };
    let first_p0_tid = res_list[0]
        .dict_get("resourceTypeID")
        .and_then(|v| v.as_i64())
        .expect("first P0 typeID") as TypeId;

    let heatmap = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetResourceData",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(first_p0_tid as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetResourceData");
    assert_eq!(heatmap.dict_get("numBands").and_then(|v| v.as_i64()), Some(9));

    let net_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
                    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(3)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(3060), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2492), PyRep::Real(0.12), PyRep::Real(0.12)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(2543), PyRep::Real(0.13), PyRep::Real(0.13)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(101), PyRep::Integer(102), PyRep::Integer(0)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(0)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(103), PyRep::Integer(104), PyRep::Integer(0)]),
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(103), PyRep::Integer(65)]),
                    PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103)]), PyRep::Integer(first_p0_tid as i64), PyRep::Integer(3_000)]),
                    PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(first_p0_tid as i64), PyRep::Real(0.02), PyRep::Integer(3)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UserUpdateNetwork full colony setup");
    assert_eq!(net_res.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(net_res.dict_get("pinsCount").and_then(|v| v.as_i64()), Some(4));

    let prog_info = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetProgramResultInfo",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::Integer(102),
                PyRep::Integer(first_p0_tid as i64),
                PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3)]),
                PyRep::Real(0.02),
            ],
            &kw,
            None,
        )
        .await
        .expect("GetProgramResultInfo");
    assert!(prog_info.dict_get("cycleTimeSeconds").and_then(|v| v.as_i64()).unwrap_or(0) >= 300);

    sess.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.last_extracted_unix = 1;
        }
    });
    let flush_res = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "Flush", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("Flush colony");
    assert!(flush_res.dict_get("producedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let (cc_pin_id, ext_pin_id) = match net_res.dict_get("pins") {
        Some(PyRep::List(pins)) if pins.len() >= 2 => (
            pins[0].dict_get("pinID").and_then(|v| v.as_i64()).unwrap_or(101),
            pins[1].dict_get("pinID").and_then(|v| v.as_i64()).unwrap_or(102),
        ),
        _ => (101, 102),
    };

    // Verify PI notifications for extractor completion and storage capacity (feat:pi.pi-notifications)
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == ext_pin_id) {
            pin.program_install_unix = 1;
            pin.expiry_time_unix = Some(10);
        }
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == cc_pin_id) {
            *pin.contents.entry(first_p0_tid).or_insert(0) = 500_000;
        }
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.last_extracted_unix = 1;
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
    });
    let _ = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetColony", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetColony advance simulation");
    let queued_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        queued_notifs.iter().any(|n| n.event == "PlanetExtractorFinished"),
        "PlanetExtractorFinished notification must be queued on program expiration"
    );
    assert!(
        queued_notifs.iter().any(|n| n.event == "PlanetStorageFull"),
        "PlanetStorageFull notification must be queued when pin storage reaches capacity"
    );
    let pers_notifs = ctx.notification_records.read().unwrap();
    assert!(pers_notifs.iter().any(|r| r.receiver_id == char_id && r.data.contains("PlanetExtractorFinished")));
    assert!(pers_notifs.iter().any(|r| r.receiver_id == char_id && r.data.contains("PlanetStorageFull")));
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == cc_pin_id) {
            pin.contents.clear();
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
    });
    let produced_tid = flush_res.dict_get("producedTypeID").and_then(|v| v.as_i64()).unwrap_or(first_p0_tid as i64) as TypeId;
    let comms_exp = vec![(PyRep::Integer(produced_tid as i64), PyRep::Integer(100))];
    let exp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserTransferCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Integer(ext_pin_id),
                    PyRep::Integer(cc_pin_id),
                ]),
                PyRep::Dict(comms_exp),
            ],
            &kw,
            None,
        )
        .await
        .expect("Expedited transfer");
    assert!(exp_res.dict_get("cooldownSeconds").and_then(|v| v.as_i64()).unwrap_or(0) >= 300);

    let comms_exp2 = vec![(PyRep::Integer(produced_tid as i64), PyRep::Integer(50))];
    let xfer_cd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserTransferCommodities",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Integer(ext_pin_id),
                    PyRep::Integer(cc_pin_id),
                ]),
                PyRep::Dict(comms_exp2),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Immediate second transfer must fail with ExpeditedTransferOnCooldown");
    assert_eq!(xfer_cd_err.code, "ExpeditedTransferOnCooldown");

    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::industry_pi::save_orbital_settings(
            snap,
            JITA_PLANET_ID,
            &eve_server::services::industry_pi::OrbitalCustomsSettings {
                planet_id: JITA_PLANET_ID,
                owner_corp_id: corp_id,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "planetOrbitalRegistryBroker",
        None,
        "UpdateSettings",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::Integer(18),
            PyRep::KeyVal(HashMap::from([
                ("corporation".to_string(), PyRep::Real(0.04)),
                ("alliance".to_string(), PyRep::Real(0.05)),
                ("standingNeutral".to_string(), PyRep::Real(0.12)),
                ("orbitalTypeID".to_string(), PyRep::Integer(81_080)),
            ])),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ],
        &kw,
        None,
    )
    .await
    .expect("UpdateSettings on POCO/Skyhook");

    let poco_tax = ctx
        .dispatch_rpc(&mut sess, "planetOrbitalRegistryBroker", None, "GetTaxRate", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetTaxRate on POCO");
    assert!((poco_tax.as_f64().unwrap() - 0.04).abs() < 1e-6);

    let skyhook_info = ctx
        .dispatch_rpc(&mut sess, "planetOrbitalRegistryBroker", None, "GetSettingsInfo", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("GetSettingsInfo on Orbital Skyhook");
    assert_eq!(skyhook_info.dict_get("orbitalTypeID").and_then(|v| v.as_i64()), Some(81_080));

    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 3);
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == cc_pin_id) {
            *pin.contents.entry(first_p0_tid).or_insert(0) += 5_000;
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
    });
    let launch_res = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(first_p0_tid as i64), PyRep::Integer(1_000), PyRep::String("CustomsOffice".to_string())],
            &kw,
            None,
        )
        .await
        .expect("UserLaunchCommodities");
    assert_eq!(launch_res.dict_get("routedQuantity").and_then(|v| v.as_i64()), Some(1_000));

    let launches = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "GetMyLaunchesDetails", &[], &kw, None)
        .await
        .expect("GetMyLaunchesDetails");
    let PyRep::List(launch_list) = launches else {
        panic!("Expected list from GetMyLaunchesDetails");
    };
    assert!(!launch_list.is_empty());

    let ls_sys_id = find_low_or_null_system(&ctx);
    let gantry_item = ctx.db.with_mut_snapshot(|snap| seed_item(snap, 3962, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None));
    sess.solar_system_id = Some(ls_sys_id);
    let anchor_res = ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_item)], &kw, None).await;
    if let Ok(g_rep) = anchor_res {
        let g_id = g_rep.dict_get("orbitalID").and_then(|v| v.as_i64()).unwrap();
        let online_res = ctx.dispatch_rpc(&mut sess, "posMgr", None, "OnlineOrbital", &[PyRep::Integer(g_id)], &kw, None).await.expect("OnlineOrbital to POCO");
        assert_eq!(online_res.dict_get("typeID").and_then(|v| v.as_i64()), Some(2233));
    }
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);

    let abandon_res = ctx
        .dispatch_rpc(&mut sess, "planetMgr", None, "UserAbandonPlanet", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
        .await
        .expect("UserAbandonPlanet");
    assert_eq!(abandon_res.as_bool(), Some(true));
    (abandon_res.as_bool(), launch_res.dict_get("routedQuantity").and_then(|v| v.as_i64()), 0.08, 2233, JITA_PLANET_ID)
}
