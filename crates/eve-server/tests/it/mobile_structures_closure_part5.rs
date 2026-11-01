//! End-to-end V24 RPC integration tests for Step 4B Mobile Deployable Structures (Part 5):
//! - `feat:structures.mobile-observatory` (`58904`)
//! - `feat:structures.mobile-observatory-decloak-ping` (`58904` 10m pings, 40% roll, 15m cloak stabilization, `OnCloakModuleForceDeactivated`)
//! - `feat:structures.mobile-scan-inhibitor` (`33589` 30 km d-scan & combat probe masking)
//! - `feat:structures.mobile-siphon` (`33477` on-grid moon/reaction material siphoning + 10% SDE waste)

#![forbid(unsafe_code)]

use crate::mobile_structures_closure_part4::{
    grant_skills, kv_get, rpc, rpc_bound, spawn_pilot_in_space, HIGHSEC_JITA, LOWSEC_AMAMAKE,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

const WH_SYSTEM: u32 = 31_000_007;
const POCHVEN_NALVULA: u32 = 30_000_192;
const ZARZAKH_SYS: u32 = 30_100_000;

#[tokio::test]
async fn test_mobile_observatory_restrictions_stabilization_and_decloak_pings() {
    // parity: feat:structures.mobile-observatory, feat:structures.mobile-observatory-decloak-ping
    eve_testkit::attest(&[
        "feat:structures.mobile-observatory",
        "feat:structures.mobile-observatory-decloak-ping",
    ]);

    let ctx = ServerContext::new().with_seeded_rng(42);
    crate::common::pin_clock(&ctx);
    let deep_pos = DVec3::new(75_000_000.0, 75_000_000.0, 75_000_000.0);

    let hunter_cid = 90_000_221;
    let stale_cloak_cid = 90_000_222;
    let fresh_cloak_cid = 90_000_223;

    let mut hunter_sess =
        spawn_pilot_in_space(&ctx, 721, hunter_cid, 98_002_221, 840_001, 626, LOWSEC_AMAMAKE, deep_pos).await;
    let mut stale_sess = spawn_pilot_in_space(
        &ctx,
        722,
        stale_cloak_cid,
        98_002_222,
        840_002,
        626,
        LOWSEC_AMAMAKE,
        deep_pos + DVec3::new(500_000.0, 0.0, 0.0),
    )
    .await;
    let mut fresh_sess = spawn_pilot_in_space(
        &ctx,
        723,
        fresh_cloak_cid,
        98_002_223,
        840_003,
        626,
        LOWSEC_AMAMAKE,
        deep_pos + DVec3::new(-500_000.0, 0.0, 0.0),
    )
    .await;

    grant_skills(&ctx, hunter_cid, &[(11584, 3)]);
    grant_skills(&ctx, stale_cloak_cid, &[(11579, 4)]);
    grant_skills(&ctx, fresh_cloak_cid, &[(11579, 4)]);

    ctx.db.with_mut_snapshot(|snap| {
        for tid_sys in [845_001i64, 845_002, 845_003, 845_004, 845_005] {
            snap.items.insert(
                tid_sys,
                InventoryItemRecord {
                    item_id: tid_sys,
                    type_id: 58904,
                    owner_id: hunter_cid,
                    location_id: 840_001,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for (mod_id, ship_id, owner) in [(846_002i64, 840_002i64, stale_cloak_cid), (846_003, 840_003, fresh_cloak_cid)]
        {
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 11370,
                    owner_id: owner,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mod_id, true);
        }
    });
    ctx.invalidate_ship_fitting(840_002);
    ctx.invalidate_ship_fitting(840_003);

    // 1. Verify space-class rejections: high-sec Jita, Wormhole, Pochven, Zarzakh
    for (bad_sys, item_id, expected_code) in [
        (HIGHSEC_JITA, 845_002i64, "CannotDeployInHighSec"),
        (WH_SYSTEM, 845_003i64, "CannotDeployInWormhole"),
        (POCHVEN_NALVULA, 845_004i64, "CannotDeployInPochven"),
        (ZARZAKH_SYS, 845_005i64, "AnchoringDisallowedInZarzakh"),
    ] {
        hunter_sess.solar_system_id = Some(bad_sys);
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&hunter_cid).unwrap().solar_system_id = bad_sys);
        let _ = ctx
            .systems
            .get_or_spawn(bad_sys)
            .spawn_ball(Ball {
                id: 840_001,
                type_id: 626,
                position: deep_pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 3.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        assert_eq!(
            rpc(&ctx, &mut hunter_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(item_id)])
                .await
                .unwrap_err()
                .code,
            expected_code
        );
    }
    hunter_sess.solar_system_id = Some(LOWSEC_AMAMAKE);
    ctx.db
        .with_mut_snapshot(|s| s.characters.get_mut(&hunter_cid).unwrap().solar_system_id = LOWSEC_AMAMAKE);

    // 2. Cloak `stale_cloak_cid` NOW (t = T0), then advance 400s before deploying Observatory!
    //    At ping #1 (`T0 + 400 + 600 + 600 = T0 + 1600`), `stale` has been cloaked 1,600s (>= 900s stabilization expired).
    rpc(&ctx, &mut stale_sess, "dogmaIM", "Activate", &[PyRep::Integer(846_002)])
        .await
        .expect("Activate cloak on stale ship");
    let sys_h = ctx.systems.get_or_spawn(LOWSEC_AMAMAKE);
    assert!(sys_h.get_combat_state(840_002).await.unwrap().unwrap().is_cloaked);

    ctx.advance_secs(400);
    let deploy_t = ctx.now_unix();
    let obs_rep = rpc(&ctx, &mut hunter_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(845_001)])
        .await
        .expect("Anchor Mobile Observatory in low-sec");
    assert_eq!(kv_get(&obs_rep, "anchoringDelaySeconds"), Some(&PyRep::Real(600.0)));
    assert_eq!(
        kv_get(&obs_rep, "despawnUnix"),
        Some(&PyRep::Integer((deploy_t + 7_800) as i64))
    );

    // 3. Advance 800s (to deploy_t + 800): Observatory finished 600s deploy at deploy_t + 600, ping #1 is at deploy_t + 1200.
    //    Cloak `fresh_cloak_cid` NOW (deploy_t + 800) -> at ping #1 (deploy_t + 1200), only 400s (< 900s) has elapsed -> IMMUNE!
    ctx.advance_secs(800);
    rpc(&ctx, &mut fresh_sess, "dogmaIM", "Activate", &[PyRep::Integer(846_003)])
        .await
        .expect("Activate cloak on fresh stabilized ship");
    assert!(sys_h.get_combat_state(840_003).await.unwrap().unwrap().is_cloaked);

    let hit_seed = (1u64..200)
        .find(|&sd| eve_server::rng::MechanicRng::seeded(sd).roll(0.40))
        .expect("seed with < 0.40 first draw");
    ctx.set_mechanic_rng_seed(Some(hit_seed));

    ctx.advance_secs(405);
    let _ = rpc(&ctx, &mut hunter_sess, "beyonce", "UpdateStateRequest", &[]).await;

    assert!(
        !sys_h.get_combat_state(840_002).await.unwrap().unwrap().is_cloaked,
        "Stale cloaked ship (> 15m) must be decloaked on ping #1 when 40% roll hits"
    );
    assert!(
        sys_h.get_combat_state(840_003).await.unwrap().unwrap().is_cloaked,
        "Freshly cloaked ship (< 15m Cloak Stabilization) MUST be immune to Observatory ping #1"
    );
    let stale_notifs = ctx.drain_notifications_for_char(stale_cloak_cid);
    let stale_decloak = stale_notifs.iter().find(|n| n.event == "OnCloakModuleForceDeactivated").expect("Decloaked pilot must receive OnCloakModuleForceDeactivated");
    assert!(stale_decloak.args.len() == 2 && matches!(stale_decloak.args[0], PyRep::String(_)) && matches!(stale_decloak.args[1], PyRep::Dict(_)), "expected [notifyMsg, notifyArgs]");

    // 4. Advance across remaining 11 pings to `deploy_t + 7_805` -> `fresh` stabilization expires at `deploy_t + 1700`,
    //    subsequent pings decloak `fresh`, and the Observatory expires & despawns after 12 pings (2h active)!
    ctx.advance_secs(6_600);
    let _ = rpc(&ctx, &mut hunter_sess, "beyonce", "UpdateStateRequest", &[]).await;

    assert!(
        !sys_h.get_combat_state(840_003).await.unwrap().unwrap().is_cloaked,
        "Once 15m Cloak Stabilization expires, subsequent Observatory pings decloak fresh ship"
    );
    let fresh_notifs = ctx.drain_notifications_for_char(fresh_cloak_cid);
    let fresh_decloak = fresh_notifs.iter().find(|n| n.event == "OnCloakModuleForceDeactivated").expect("Fresh ship pilot must receive OnCloakModuleForceDeactivated once stabilization expires");
    assert!(fresh_decloak.args.len() == 2 && matches!(fresh_decloak.args[0], PyRep::String(_)) && matches!(fresh_decloak.args[1], PyRep::Dict(_)));
    assert!(
        ctx.db.with_snapshot(|s| !s.items.contains_key(&845_001)),
        "Mobile Observatory must automatically expire and despawn after 10m deploy + 2h (12 pings)"
    );
}

#[tokio::test]
async fn test_mobile_scan_inhibitor_and_mobile_siphon_unit_end_to_end() {
    // parity: feat:structures.mobile-scan-inhibitor, feat:structures.mobile-siphon
    eve_testkit::attest(&[
        "feat:structures.mobile-scan-inhibitor",
        "feat:structures.mobile-siphon",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let grid_origin = DVec3::new(65_000_000.0, 65_000_000.0, 65_000_000.0);

    let owner_cid = 90_000_231;
    let hidden_cid = 90_000_232;
    let exposed_cid = 90_000_233;
    let scanner_cid = 90_000_234;

    let mut owner_sess =
        spawn_pilot_in_space(&ctx, 731, owner_cid, 98_002_231, 850_001, 626, LOWSEC_AMAMAKE, grid_origin).await;
    let mut hidden_sess = spawn_pilot_in_space(
        &ctx,
        732,
        hidden_cid,
        98_002_232,
        850_002,
        626,
        LOWSEC_AMAMAKE,
        grid_origin + DVec3::new(15_000.0, 0.0, 0.0),
    )
    .await;
    let _exposed_sess = spawn_pilot_in_space(
        &ctx,
        733,
        exposed_cid,
        98_002_233,
        850_003,
        626,
        LOWSEC_AMAMAKE,
        grid_origin + DVec3::new(45_000.0, 0.0, 0.0),
    )
    .await;
    let mut scanner_sess = spawn_pilot_in_space(
        &ctx,
        734,
        scanner_cid,
        98_002_234,
        850_004,
        626,
        LOWSEC_AMAMAKE,
        grid_origin + DVec3::new(80_000.0, 0.0, 0.0),
    )
    .await;

    let sys_h = ctx.systems.get_or_spawn(LOWSEC_AMAMAKE);
    for sid in [850_001i64, 850_002, 850_003, 850_004] {
        let _ = sys_h.set_movement_mode(sid, MovementMode::Stop).await;
    }

    grant_skills(&ctx, owner_cid, &[(11584, 3)]);
    grant_skills(&ctx, scanner_cid, &[(3412, 5), (25810, 5), (25811, 5)]);

    let inhib_id = 855_001i64;
    let inhib_dup_id = 855_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        for iid in [inhib_id, inhib_dup_id] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 33589,
                    owner_id: owner_cid,
                    location_id: 850_001,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            856_001,
            InventoryItemRecord {
                item_id: 856_001,
                type_id: 18639,
                owner_id: scanner_cid,
                location_id: 850_004,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            856_002,
            InventoryItemRecord {
                item_id: 856_002,
                type_id: 30028,
                owner_id: scanner_cid,
                location_id: 850_004,
                flag: InventoryFlag::HiSlot0,
                quantity: 8,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(850_004);

    // 1. Anchor Mobile Scan Inhibitor (33589); verify < 100 km same-type block and pre-spool d-scan visibility
    rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhib_id)])
        .await
        .expect("Anchor Mobile Scan Inhibitor");
    assert_eq!(
        rpc(&ctx, &mut owner_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhib_dup_id)])
            .await
            .unwrap_err()
            .code,
        "DeploymentTooCloseToSameType"
    );

    let pre_dscan = rpc(
        &ctx,
        &mut scanner_sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1_000_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
    )
    .await
    .unwrap();
    let PyRep::List(pre_rows) = pre_dscan else { panic!("expected list") };
    assert!(
        pre_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_002))),
        "Before 60s spool completes, ship at 15 km is still visible on ConeScan"
    );

    // 2. Complete 60s spool: hidden_ship (15 km) is masked on BOTH ConeScan AND Combat Probes,
    //    while exposed_ship (45 km) AND the Mobile Scan Inhibitor (33589) itself remain visible!
    ctx.advance_secs(61);
    let post_dscan = rpc(
        &ctx,
        &mut scanner_sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1_000_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
    )
    .await
    .unwrap();
    let PyRep::List(post_rows) = post_dscan else { panic!("expected list") };
    assert!(!post_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_002))), "Ship at 15 km must be hidden from ConeScan");
    assert!(post_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_003))), "Ship at 45 km must remain visible on ConeScan");
    assert!(post_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(inhib_id))), "Mobile Scan Inhibitor itself must remain visible on ConeScan");

    let blinded_dscan = rpc(
        &ctx,
        &mut hidden_sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1_000_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
    )
    .await
    .unwrap();
    assert_eq!(blinded_dscan, PyRep::List(Vec::new()));

    rpc(
        &ctx,
        &mut scanner_sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(856_001), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)],
    )
    .await
    .expect("Launch Combat Scanner Probes");
    let probe_ids: Vec<i64> = ctx
        .active_scanner_probes
        .read()
        .unwrap()
        .values()
        .filter(|p| p.owner_char_id == scanner_cid)
        .map(|p| p.probe_id)
        .collect();
    let probe_dict = PyRep::Dict(
        probe_ids
            .iter()
            .map(|&pid| {
                (
                    PyRep::Integer(pid),
                    PyRep::keyval([
                        ("pos", PyRep::Tuple(vec![PyRep::Real(grid_origin.x), PyRep::Real(grid_origin.y), PyRep::Real(grid_origin.z)])),
                        ("scanRange", PyRep::Real(150_000_000_000.0)),
                    ]),
                )
            })
            .collect(),
    );
    let scan_bound = rpc(&ctx, &mut scanner_sess, "scanMgr", "GetSystemScanMgr", &[])
        .await
        .unwrap()
        .dict_get("boundID")
        .and_then(|v| v.as_i64())
        .map(|v| v as u64);
    let PyRep::List(cmb_rows) = rpc_bound(&ctx, &mut scanner_sess, "scanMgr", scan_bound, "RequestScans", &[probe_dict])
        .await
        .unwrap()
    else {
        panic!("expected RequestScans list")
    };
    assert!(!cmb_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_002))), "Ship at 15 km must be hidden from Combat Probes");
    assert!(cmb_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(850_003))), "Ship at 45 km must be detected by Combat Probes");
    assert!(cmb_rows.iter().any(|r| kv_get(r, "itemID") == Some(&PyRep::Integer(inhib_id))), "Mobile Scan Inhibitor itself must be detected by Combat Probes");

    // 3. Mobile Siphon Unit (33477): off-grid rejection, on-grid Athanor siphoning + 10% SDE waste, public looting, empty unanchor & scoop!
    let siphon_pos = DVec3::new(-65_000_000.0, 65_000_000.0, 65_000_000.0);
    let mut siphon_sess =
        spawn_pilot_in_space(&ctx, 735, owner_cid, 98_002_231, 850_011, 626, LOWSEC_AMAMAKE, siphon_pos).await;
    let mut looter_sess = spawn_pilot_in_space(
        &ctx,
        736,
        scanner_cid,
        98_002_234,
        850_012,
        626,
        LOWSEC_AMAMAKE,
        siphon_pos + DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    for sid in [850_011i64, 850_012] {
        let _ = sys_h.set_movement_mode(sid, MovementMode::Stop).await;
    }
    let siphon_id = 857_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            siphon_id,
            InventoryItemRecord {
                item_id: siphon_id,
                type_id: 33477,
                owner_id: owner_cid,
                location_id: 850_011,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        rpc(&ctx, &mut siphon_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_id)])
            .await
            .unwrap_err()
            .code,
        "NoSiphonTargetInRange"
    );

    let athanor_id = 1_000_000_857_000i64;
    let athanor_pos = siphon_pos + DVec3::new(60_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            athanor_id,
            UpwellStructureRecord {
                structure_id: athanor_id,
                type_id: 35835,
                owner_corp_id: 98_002_999,
                solar_system_id: LOWSEC_AMAMAKE,
                position: athanor_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_200,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                name: "Moon Refinery".into(),
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.item_positions.insert(athanor_id, athanor_pos.to_array());
        snap.items.insert(
            858_010,
            InventoryItemRecord {
                item_id: 858_010,
                type_id: 16634,
                owner_id: owner_cid,
                location_id: athanor_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            858_011,
            InventoryItemRecord {
                item_id: 858_011,
                type_id: 16670,
                owner_id: 98_002_999,
                location_id: athanor_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        rpc(&ctx, &mut siphon_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_id)])
            .await
            .unwrap_err()
            .code,
        "NoSiphonTargetInRange",
        "Upwell Athanor with personal/corp hangar stock must not be a valid siphon target"
    );

    let silo_id = 1_000_000_857_100i64;
    let silo_pos = siphon_pos + DVec3::new(0.0, 60_000.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            silo_id,
            InventoryItemRecord {
                item_id: silo_id,
                type_id: 14343,
                owner_id: 98_002_999,
                location_id: LOWSEC_AMAMAKE as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(silo_id, silo_pos.to_array());
        snap.items.insert(
            858_001,
            InventoryItemRecord {
                item_id: 858_001,
                type_id: 16634,
                owner_id: 98_002_999,
                location_id: silo_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            858_002,
            InventoryItemRecord {
                item_id: 858_002,
                type_id: 16670,
                owner_id: 98_002_999,
                location_id: silo_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    rpc(&ctx, &mut siphon_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_id)])
        .await
        .expect("Anchor Mobile Siphon Unit within 150 km of Starbase Silo");

    ctx.advance_secs(7_265);
    let fill_ratio = rpc(&ctx, &mut looter_sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_id)])
        .await
        .expect("GetCapacityOfSiphon")
        .as_f64()
        .unwrap();
    assert!(fill_ratio > 0.0, "Siphon fill ratio must be > 0 after 2 cycles");

    let (rem_raw, rem_pro, athanor_personal, athanor_corp, stolen_items) = ctx.db.with_snapshot(|s| {
        let r = s.items[&858_001].quantity;
        let p = s.items[&858_002].quantity;
        let ap = s.items[&858_010].quantity;
        let ac = s.items[&858_011].quantity;
        let st: Vec<(i64, u32, i32)> = s
            .items
            .values()
            .filter(|i| i.location_id == siphon_id)
            .map(|i| (i.item_id, i.type_id, i.quantity))
            .collect();
        (r, p, ap, ac, st)
    });
    assert_eq!(rem_raw, 368, "Starbase Silo must lose 120 stolen + 12 (10%) waste = 132 raw moon material");
    assert_eq!(rem_pro, 434, "Starbase Silo must lose 60 stolen + 6 (10%) waste = 66 processed material");
    assert_eq!(athanor_personal, 500, "Athanor personal hangar stock must not be siphoned");
    assert_eq!(athanor_corp, 500, "Athanor corp hangar stock must not be siphoned");
    assert!(stolen_items.iter().any(|&(_, tid, q)| tid == 16634 && q == 120));
    assert!(stolen_items.iter().any(|&(_, tid, q)| tid == 16670 && q == 60));

    assert_eq!(
        rpc(&ctx, &mut looter_sess, "invbroker", "ValidateItemCanBeOpened", &[PyRep::Integer(siphon_id)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    let PyRep::Tuple(bind_parts) = rpc(
        &ctx,
        &mut looter_sess,
        "invbroker",
        "MachoBindObject",
        &[PyRep::Tuple(vec![PyRep::Integer(LOWSEC_AMAMAKE as i64), PyRep::Integer(5)]), PyRep::None],
    )
    .await
    .expect("Bind solar-system invbroker") else {
        panic!("expected MachoBindObject tuple");
    };
    let sys_inv_mgr = bind_parts[0].as_i64().unwrap() as u64;
    let ship_inv_bound = Some(
        rpc_bound(
            &ctx,
            &mut looter_sess,
            "invbroker",
            Some(sys_inv_mgr),
            "GetInventoryFromId",
            &[PyRep::Integer(850_012), PyRep::Integer(0)],
        )
        .await
        .expect("GetInventoryFromId looter ship")
        .as_i64()
        .unwrap() as u64,
    );

    rpc(&ctx, &mut siphon_sess, "entity", "CmdUnanchor", &[PyRep::Integer(siphon_id)])
        .await
        .expect("Unanchor Mobile Siphon Unit");
    assert_eq!(
        rpc(&ctx, &mut siphon_sess, "ship", "Scoop", &[PyRep::Integer(siphon_id)])
            .await
            .unwrap_err()
            .code,
        "CannotPutContainerInContainer"
    );

    for (st_id, _, _qty) in &stolen_items {
        rpc_bound(
            &ctx,
            &mut looter_sess,
            "invbroker",
            ship_inv_bound,
            "Add",
            &[PyRep::Integer(*st_id), PyRep::Integer(siphon_id), PyRep::Integer(5)],
        )
        .await
        .expect("Third-party pilot loots stolen material from Mobile Siphon Unit");
    }
    rpc(&ctx, &mut siphon_sess, "ship", "Scoop", &[PyRep::Integer(siphon_id)])
        .await
        .expect("Owner scoops empty unanchored Mobile Siphon Unit");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&siphon_id].location_id), 850_011);
}
