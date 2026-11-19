//! Unit and integration tests for `pve_exploration`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn exploration_clock_progression_and_wormhole_system_classification() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_800_000)));

    let t0 = with_pve_exploration_reg(&ctx, |reg| now_sec(reg));
    advance_pve_exploration_clock(&ctx, 120);
    let t1 = with_pve_exploration_reg(&ctx, |reg| now_sec(reg));
    assert_eq!(t1, t0 + 120);

    // Known K-space has no wormhole class; wormhole systems return Some(class)
    assert_eq!(wormhole_class_for_system(30_000_142), None);
    let wh_sys = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| wormhole_class_for_system(sid).is_some())
        .expect("SDE contains wormhole system");
    assert!(wormhole_class_for_system(wh_sys).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exploration_rpc_anomalies_ship_restrictions_gates_info_and_escalations() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_401;
    let ship_id: i64 = 1_050_400_001;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_rpc", "pw", char_id, "ExpRpc");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_810_000)));
    let mut session = SessionState::new(504);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    let empty_kw = HashMap::new();

    // Unrelated service/method returns None
    assert!(handle_pve_exploration(&ctx, &mut session, "", "", &[], &empty_kw)
        .await
        .is_none());

    // GetCombatAnomalyInstances seeds and lists non-probe anomalies
    let anomalies = handle_pve_exploration(
        &ctx,
        &mut session,
        "scanSvc",
        "GetCombatAnomalyInstances",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("GetCombatAnomalyInstances succeeds");
    assert!(matches!(anomalies, PyRep::List(_)));

    let first_site_id = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&sys_id)
            .and_then(|l| l.iter().find(|s| !s.probe_required))
            .map(|s| s.site_id.clone())
            .expect("seeded anomaly")
    });

    // EnterCombatAnomaly & GetDungeonInfo
    let enter = handle_pve_exploration(
        &ctx,
        &mut session,
        "scanSvc",
        "EnterCombatAnomaly",
        &[PyRep::String(first_site_id.clone())],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("EnterCombatAnomaly succeeds");
    assert!(!matches!(enter, PyRep::None));

    let info = handle_pve_exploration(
        &ctx,
        &mut session,
        "keeper",
        "GetDungeonInfo",
        &[PyRep::String(first_site_id.clone())],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("GetDungeonInfo succeeds");
    assert!(!matches!(info, PyRep::None));

    // GetDungeonShipRestrictions for DED rating 2 (frigate-allowed -> canEnter true) & Battleship in DED 2 (false)
    let ded2 = handle_pve_exploration(
        &ctx,
        &mut session,
        "keeper",
        "GetDungeonShipRestrictions",
        &[PyRep::Integer(2)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("DED 2 restriction");
    assert_eq!(ded2.dict_get("canEnter"), Some(&PyRep::Bool(true)));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = 638; // Raven Battleship (group 27, > 5M kg mass)
        }
    });
    let ded2_bs = handle_pve_exploration(
        &ctx,
        &mut session,
        "keeper",
        "CanEnterDungeonInCurrentShip",
        &[PyRep::Integer(2)],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("DED 2 battleship restriction");
    assert_eq!(ded2_bs.dict_get("canEnter"), Some(&PyRep::Bool(false)));

    // Escalating path journal queries
    let esc_details = handle_pve_exploration(
        &ctx,
        &mut session,
        "dungeonExplorationMgr",
        "GetMyEscalatingPathDetails",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("GetMyEscalatingPathDetails");
    assert!(!matches!(esc_details, PyRep::None));

    let esc_entered = handle_pve_exploration(
        &ctx,
        &mut session,
        "dungeonExplorationMgr",
        "GetEscalatingPathDungeonsEntered",
        &[],
        &empty_kw,
    )
    .await
    .expect("handled")
    .expect("GetEscalatingPathDungeonsEntered");
    assert!(matches!(esc_entered, PyRep::List(_)));

    // Kill last active NPC and advance wave/completion
    let last_npc_opt = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get_mut(&sys_id)
            .and_then(|l| l.iter_mut().find(|s| s.site_id == first_site_id))
            .and_then(|s| {
                let first = s.active_npc_ball_ids.first().copied();
                if first.is_some() {
                    s.active_npc_ball_ids.truncate(1);
                }
                first
            })
    });
    if let Some(npc_id) = last_npc_opt {
        handle_pve_site_npc_destruction(&ctx, sys_id, npc_id, char_id).await;
    }
    let _ = advance_site_wave_or_complete(&ctx, &mut session, sys_id, &first_site_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exploration_read_purity_and_undocked_guards_do_not_mutate_db() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_499;
    let ship_id: i64 = 1_050_400_999;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_purity", "pw", char_id, "ExpPurity");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(60_003_760);
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_815_000)));
    let kw = HashMap::new();

    let mut docked = SessionState::new(599);
    docked.user_id = Some(1);
    docked.character_id = Some(char_id);
    docked.solar_system_id = Some(sys_id);
    docked.station_id = Some(60_003_760);
    docked.ship_id = Some(ship_id);

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    for svc in ["dungeonExplorationMgr", "scanSvc"] {
        let dng_entered = handle_pve_exploration(
            &ctx,
            &mut docked,
            svc,
            "GetEscalatingPathDungeonsEntered",
            &[],
            &kw,
        )
        .await
        .expect("handled")
        .expect("read-only GetEscalatingPathDungeonsEntered ok");
        assert_eq!(dng_entered, PyRep::List(Vec::new()), "{svc}");
    }

    for (svc, meth, args) in [
        ("scanSvc", "GetCombatAnomalyInstances", vec![]),
        ("keeper", "GetDungeonShipRestrictions", vec![PyRep::Integer(2)]),
        ("keeper", "CanEnterDungeonInCurrentShip", vec![PyRep::Integer(2)]),
        ("keeper", "GetDungeonInfo", vec![PyRep::String("DED-30000142-3".into())]),
        ("keeper", "GetDungeonObjectivesData", vec![PyRep::String("DED-30000142-3".into())]),
        ("keeper", "ActivateAccelerationGate", vec![PyRep::String("DED-30000142-3".into())]),
        ("keeper", "EnterNextDungeonRoom", vec![PyRep::String("DED-30000142-3".into())]),
        ("beyonce", "ActivateAccelerationGate", vec![PyRep::String("DED-30000142-3".into())]),
        ("scanSvc", "EnterCombatAnomaly", vec![PyRep::String("PVE-ANOM-30000142-1".into())]),
    ] {
        let err = handle_pve_exploration(&ctx, &mut docked, svc, meth, &args, &kw)
            .await
            .expect("handled")
            .expect_err("docked session must be rejected");
        assert_eq!(err.code, "NotInSpace", "{svc}.{meth} while docked");
    }
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Undocked without ship -> NoActiveShip where active ship is required
    let mut no_ship = SessionState::new(600);
    no_ship.user_id = Some(1);
    no_ship.character_id = Some(char_id);
    no_ship.solar_system_id = Some(sys_id);
    for (svc, meth, args) in [
        ("scanSvc", "GetCombatAnomalyInstances", vec![]),
        ("keeper", "CanEnterDungeonInCurrentShip", vec![PyRep::Integer(2)]),
        ("scanSvc", "EnterCombatAnomaly", vec![PyRep::String("PVE-ANOM-30000142-1".into())]),
        ("keeper", "EnterNextDungeonRoom", vec![PyRep::Integer(999_999)]),
    ] {
        let err = handle_pve_exploration(&ctx, &mut no_ship, svc, meth, &args, &kw)
            .await
            .expect("handled")
            .expect_err("missing active ship must fail");
        assert_eq!(err.code, "NoActiveShip", "{svc}.{meth} without ship");
    }
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Undocked with ship -> reads & invalid-target errors must not seed or mutate DB
    let mut space = no_ship;
    space.ship_id = Some(ship_id);

    let rest_ok = handle_pve_exploration(
        &ctx,
        &mut space,
        "keeper",
        "GetDungeonShipRestrictions",
        &[PyRep::Integer(2)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("GetDungeonShipRestrictions DED 2 ok");
    assert_eq!(rest_ok.dict_get("dedRating"), Some(&PyRep::Integer(2)));

    assert_eq!(
        handle_pve_exploration(
            &ctx,
            &mut space,
            "keeper",
            "GetDungeonShipRestrictions",
            &[PyRep::Integer(999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DungeonNotFound"
    );
    assert_eq!(
        handle_pve_exploration(
            &ctx,
            &mut space,
            "keeper",
            "GetDungeonInfo",
            &[PyRep::String("DED-30000142-3".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DungeonNotFound"
    );
    assert_eq!(
        handle_pve_exploration(
            &ctx,
            &mut space,
            "scanSvc",
            "EnterCombatAnomaly",
            &[PyRep::String("FAKE-ANOM-999".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AnomalyNotFound"
    );
    assert!(
        handle_pve_exploration(
            &ctx,
            &mut space,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(999_999)],
            &kw,
        )
        .await
        .is_none(),
        "unknown integer gate must fall through cleanly as None"
    );
    assert_eq!(
        handle_pve_exploration(
            &ctx,
            &mut space,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::String("FAKE-SITE-999".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DungeonNotFound"
    );
    assert_eq!(
        handle_pve_exploration(
            &ctx,
            &mut space,
            "keeper",
            "EnterNextDungeonRoom",
            &[PyRep::Integer(999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DungeonNotFound"
    );
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Repeat GetCombatAnomalyInstances after initial seed must be read-pure
    let _ = handle_pve_exploration(&ctx, &mut space, "scanSvc", "GetCombatAnomalyInstances", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let e1 = ctx.db.state_epoch();
    let c1 = ctx.db.tx_commit_count();
    let _ = handle_pve_exploration(&ctx, &mut space, "scanSvc", "GetCombatAnomalyInstances", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ctx.db.state_epoch(), e1);
    assert_eq!(ctx.db.tx_commit_count(), c1);
}

#[tokio::test]
async fn escalation_queries_and_special_gate_guards_are_read_pure() {
    let err_no_active_ship = "NoActiveShip";
    let err_not_in_space = "NotInSpace";
    let svc_exp_mgr = "dungeonExplorationMgr";
    let svc_scan_svc = "scanSvc";
    let svc_keeper = "keeper";
    let m_get_path = "GetMyEscalatingPathDetails";
    let m_get_esc = "GetEscalations";
    let m_activate_gate = "ActivateAccelerationGate";

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_800_000)));
    let kw = HashMap::new();
    let sys_id = 30_000_142;

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(90_000_088),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };

    // Empty-repo escalation reads must not create or touch DB mutations.
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();
    for (svc, method) in [
        (svc_exp_mgr, m_get_path),
        (svc_scan_svc, m_get_path),
        (svc_exp_mgr, m_get_esc),
    ] {
        let res = handle_pve_exploration(&ctx, &mut sess, svc, method, &[], &kw)
            .await
            .unwrap();
        assert!(res.is_ok(), "{svc}.{method} failed: {res:?}");
    }
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Seed a special Homefront site without touching DB again after setup; undocked shipless gate activation -> NoActiveShip (and read-only special-site lookup).
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.homefront_sites.entry(sys_id).or_default().push(
            crate::services::pve_special_sites::HomefrontSiteRecord {
                instance_id: 710_701,
                dungeon_id: 900_001,
                dungeon_name: "Test Homefront Raid".to_string(),
                solar_system_id: sys_id,
                optimal_fleet_size: 5,
                base_pilot_isk: 1_000_000,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 9_888_777_667,
                boss_type_id: 0,
            },
        );
    });
    let e1 = ctx.db.state_epoch();
    let c1 = ctx.db.tx_commit_count();

    let hf_str_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        svc_keeper,
        m_activate_gate,
        &[PyRep::String(format!("HF-{sys_id}-0"))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(hf_str_err.code, err_no_active_ship);

    let hf_int_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        svc_keeper,
        m_activate_gate,
        &[PyRep::Integer(710_701)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(hf_int_err.code, err_no_active_ship);

    // Docked non-observatory acceleration gate activation -> NotInSpace and zero DB mutation.
    let mut docked = sess.clone();
    docked.station_id = Some(60_003_760);
    docked.ship_id = Some(500_000_088);
    let docked_err = handle_pve_exploration(
        &ctx,
        &mut docked,
        svc_keeper,
        m_activate_gate,
        &[PyRep::Integer(710_701)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(docked_err.code, err_not_in_space);
    assert_eq!(ctx.db.state_epoch(), e1);
    assert_eq!(ctx.db.tx_commit_count(), c1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anomaly_query_incursion_seeding_and_dungeon_id_gate_alias() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_511;
    let ship_id: i64 = 1_050_401_511;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_r2", "pw", char_id, "ExpR2");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_820_000)));
    let kw = HashMap::new();

    let mut sess = SessionState::new(511);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    // 1. Invalid, negative, and unknown solarSystemID args must fail cleanly before seeding.
    for (arg, expected_code) in [
        (PyRep::Integer(-1), "InvalidArguments"),
        (PyRep::String("bad".into()), "InvalidArguments"),
        (PyRep::Integer(39_999_999), "UnknownSolarSystem"),
    ] {
        let err = handle_pve_exploration(&ctx, &mut sess, "scanSvc", "GetCombatAnomalyInstances", &[arg], &kw)
            .await
            .expect("handled")
            .expect_err("invalid solarSystemID must fail");
        assert_eq!(err.code, expected_code);
    }

    // 2. Unseeded remote-system query serves anomalies read-only without mutating DB/reg/systems.
    let remote_sys: SolarSystemId = 30_002_187;
    let remote_rep = handle_pve_exploration(
        &ctx,
        &mut sess,
        "scanSvc",
        "GetCombatAnomalyInstances",
        &[PyRep::Integer(i64::from(remote_sys))],
        &kw,
    )
    .await
    .expect("handled")
    .expect("remote unseeded GetCombatAnomalyInstances succeeds read-only");
    let PyRep::List(remote_items) = remote_rep else {
        panic!("expected list from remote GetCombatAnomalyInstances");
    };
    assert!(
        remote_items
            .iter()
            .filter_map(|x| x.dict_get("dungeonName").and_then(|v| v.as_str()))
            .any(|n| n.contains("Blood")),
        "remote Blood Raiders system must report Blood anomalies read-only"
    );
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        !reg.seeded_systems.contains_key(&remote_sys)
            && !reg.sites_by_system.contains_key(&remote_sys)
    }));
    assert!(ctx.systems.get_handle(remote_sys).is_none());

    // 3. Non-natural INC-* acceleration gate probes stay read-only without initializing incursions.
    let (eligible_const, eligible_inc_sys) =
        crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(&ctx)
            .into_iter()
            .next()
            .map(|(cid, _)| {
                let mut sys_list: Vec<SolarSystemId> = ctx
                    .sde
                    .solar_systems
                    .values()
                    .filter(|s| s.constellation_id == cid)
                    .map(|s| s.solar_system_id)
                    .collect();
                sys_list.sort_unstable();
                let (stg, _, _, _) =
                    crate::services::pve_incursion_lifecycle::assign_constellation_system_roles(
                        &ctx.sde,
                        cid,
                        &sys_list,
                    );
                (cid, stg)
            })
            .unwrap_or((0, sys_id));
    for probe_sys in [sys_id, eligible_inc_sys] {
        sess.solar_system_id = Some(probe_sys);
        for fake_inc in ["INC-99999-2134", "INC-99999-5165", "INC-bogus-5164", "INC-bogus-5165"] {
            let err = handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                "ActivateAccelerationGate",
                &[PyRep::String(fake_inc.into())],
                &kw,
            )
            .await
            .expect("handled")
            .expect_err("fabricated INC-* site must fail with DungeonNotFound");
            assert_eq!(err.code, "DungeonNotFound", "{fake_inc} in {probe_sys}");
        }
    }
    if eligible_const != 0 {
        assert!(!is_naturally_seedable_incursion_site(&ctx, eligible_inc_sys, "INC-bogus-5164"));
        assert!(is_naturally_seedable_incursion_site(&ctx, eligible_inc_sys, "INC-5164"));
        assert!(is_naturally_seedable_incursion_site(
            &ctx,
            eligible_inc_sys,
            &format!("INC-{eligible_const}-5164"),
        ));
    }
    assert!(crate::services::pve_special_sites::read_pve_special_reg(&ctx, |r| !r.initialized));
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // 4. keeper.ActivateAccelerationGate accepts numeric dungeon_id strings for multi-room DED gates.
    sess.solar_system_id = Some(sys_id);
    let site_pos = [35_000_000_000.0, 12_000_000_000.0, -8_000_000_000.0];
    with_pve_exploration_reg(&ctx, |reg| {
        let now = now_sec(reg);
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "DED-30000142-ALIAS".into(),
            dungeon_id: 424_242,
            dungeon_name: "Test Multi-Room DED".into(),
            solar_system_id: sys_id,
            site_category: "DEDComplex".into(),
            ded_rating: Some(3),
            position: site_pos,
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 1,
            created_unix: now,
            lifetime_seconds: 86_400,
            ..Default::default()
        });
    });
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::from_array(site_pos),
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;
    let res = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String("424242".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect("ActivateAccelerationGate by str(dungeon_id) must advance room");
    assert_eq!(
        res.dict_get("siteID"),
        Some(&PyRep::String("DED-30000142-ALIAS".into()))
    );
    assert_eq!(res.dict_get("dungeonID"), Some(&PyRep::Integer(424_242)));
    assert_eq!(
        read_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&sys_id)
                .and_then(|l| l.iter().find(|s| s.site_id == "DED-30000142-ALIAS"))
                .map(|s| s.cleared)
        }),
        Some(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nullsec_ded_gate_seeding_and_invalid_gate_id_read_purity() {
    use crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde;

    let sde = shared_sde();
    let null_sys = sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id < 30_100_000
                && s.region_id != 10_000_070
                && s.security_status <= 0.0
        })
        .map(|s| s.solar_system_id)
        .expect("SDE contains non-Pochven K-space nullsec system");
    let pochven_sys = sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE contains Pochven system");
    let shattered_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            let (c, _, shat, _) = system_wormhole_profile_sde(&sde, sid);
            (1..=6).contains(&c) && shat
        })
        .expect("SDE contains shattered C1-C6 WH");
    let drifter_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| (14..=18).contains(&system_wormhole_profile_sde(&sde, sid).0))
        .expect("SDE contains Drifter WH");

    let char_id: i64 = 90_050_512;
    let ship_id: i64 = 1_050_401_512;
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_null_ded", "pw", char_id, "ExpNullDed");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: null_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_825_000)));
    let kw = HashMap::new();

    let mut sess = SessionState::new(512);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    // 1. Bogus DED gate remainder offsets in nullsec and DED gate IDs in Pochven/shattered/Drifter stay read-pure.
    let null_base = 1_864_000_000 + i64::from(null_sys % 100_000) * 10;
    for bogus_offset in [1_i64, 4, 9] {
        let bogus_gate = null_base + bogus_offset;
        assert!(
            handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                "ActivateAccelerationGate",
                &[PyRep::Integer(bogus_gate)],
                &kw,
            )
            .await
            .is_none(),
            "bogus nullsec DED offset +{bogus_offset} must return None"
        );
        let err = handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "EnterNextDungeonRoom",
            &[PyRep::Integer(bogus_gate)],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("bogus nullsec DED gate must fail EnterNextDungeonRoom");
        assert_eq!(err.code, "DungeonNotFound");
    }
    for non_kspace_sys in [pochven_sys, shattered_sys, drifter_sys] {
        sess.solar_system_id = Some(non_kspace_sys);
        let candidate_gate = 1_864_000_000 + i64::from(non_kspace_sys % 100_000) * 10 + 10;
        assert!(
            handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                "ActivateAccelerationGate",
                &[PyRep::Integer(candidate_gate)],
                &kw,
            )
            .await
            .is_none(),
            "non-DED system {non_kspace_sys} must not seed on DED gate ID"
        );
    }
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        [null_sys, pochven_sys, shattered_sys, drifter_sys]
            .into_iter()
            .all(|sid| !reg.seeded_systems.contains_key(&sid))
    }));

    // 2. Valid unseeded null-sec DED 10/10 gate ball ID (`+10`) seeds null_sys and advances cleanly.
    sess.solar_system_id = Some(null_sys);
    let ded_pos = [45_000_000.0, -20_000_000.0, 15_000_000.0];
    let _ = ctx
        .systems
        .get_or_spawn(null_sys)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::from_array(ded_pos),
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;
    let valid_null_gate = null_base + 10;
    let err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(valid_null_gate)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unseeded null-sec DED 10/10 gate resolves seeded room NPC guard check");
    assert_eq!(err.code, "WaveNpcsRemaining");
    assert!(
        err.message.contains(&format!("DED-{null_sys}-10")),
        "expected resolved null-sec DED site ID in error: {}",
        err.message
    );
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.get(&null_sys).copied().unwrap_or(false)
            && reg.sites_by_system.get(&null_sys).is_some_and(|sites| {
                sites.iter().any(|s| {
                    s.site_id == format!("DED-{null_sys}-10")
                        && s.gate_ball_id == Some(valid_null_gate)
                })
            })
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unseeded_shattered_wh_site_activation_and_bogus_site_id_read_purity() {
    use crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde;

    let sde = shared_sde();
    let jita_sys: SolarSystemId = 30_000_142;
    let c13_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| system_wormhole_profile_sde(&sde, sid).0 == 13)
        .expect("SDE contains C13 WH");
    let shattered_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| {
            let (c, _, shat, _) = system_wormhole_profile_sde(&sde, sid);
            (1..=6).contains(&c) && shat
        })
        .expect("SDE contains shattered C1-C6 WH");
    let drifter_sys = sde
        .solar_systems
        .keys()
        .copied()
        .find(|&sid| (14..=18).contains(&system_wormhole_profile_sde(&sde, sid).0))
        .expect("SDE contains Drifter WH");

    let char_id: i64 = 90_050_513;
    let ship_id: i64 = 1_050_401_513;
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_shat", "pw", char_id, "ExpShat");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: jita_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_830_000)));
    let kw = HashMap::new();

    let mut sess = SessionState::new(513);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(jita_sys);
    sess.ship_id = Some(ship_id);

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    // 1. Fabricated / wrong-system PVE-ANOM / DED / SHD IDs never seed DB on error paths.
    for (probe_sys, fake_site) in [
        (jita_sys, format!("PVE-ANOM-{jita_sys}-999")),
        (jita_sys, format!("DED-{jita_sys}-bogus")),
        (jita_sys, format!("DED-{jita_sys}-5")),
        (jita_sys, format!("SHD-{jita_sys}")),
        (drifter_sys, format!("PVE-ANOM-{drifter_sys}-1")),
        (shattered_sys, format!("DED-{shattered_sys}-10")),
    ] {
        sess.solar_system_id = Some(probe_sys);
        let anom_err = handle_pve_exploration(
            &ctx,
            &mut sess,
            "scanSvc",
            "EnterCombatAnomaly",
            &[PyRep::String(fake_site.clone())],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("fabricated site must fail EnterCombatAnomaly");
        assert_eq!(anom_err.code, "AnomalyNotFound", "{fake_site}");

        let gate_err = handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::String(fake_site.clone())],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("fabricated site must fail ActivateAccelerationGate");
        assert_eq!(gate_err.code, "DungeonNotFound", "{fake_site}");
    }
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        [jita_sys, drifter_sys, shattered_sys, c13_sys]
            .into_iter()
            .all(|sid| !reg.seeded_systems.contains_key(&sid))
    }));

    // 2. Real unseeded shattered WH anomalies (SHD-/SHI-) and shattered/C13 signatures (DAT-*-1) work.
    sess.solar_system_id = Some(shattered_sys);
    for shat_anom in [format!("SHD-{shattered_sys}"), format!("SHI-{shattered_sys}")] {
        let rep = handle_pve_exploration(
            &ctx,
            &mut sess,
            "scanSvc",
            "EnterCombatAnomaly",
            &[PyRep::String(shat_anom.clone())],
            &kw,
        )
        .await
        .expect("handled")
        .expect("unseeded shattered WH anomaly must enter cleanly");
        assert_eq!(rep.dict_get("siteID"), Some(&PyRep::String(shat_anom)));
    }

    let dat1_pos = [-32_000_000.0, 28_000_000.0, -10_000_000.0];
    let _ = ctx
        .systems
        .get_or_spawn(c13_sys)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::from_array(dat1_pos),
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;
    sess.solar_system_id = Some(c13_sys);
    let c13_dat_id = format!("DAT-{c13_sys}-1");
    let c13_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(c13_dat_id.clone())],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unseeded C13 DAT-*-1 signature gate resolves seeded container check");
    assert_eq!(c13_err.code, "SiteContainersUnhacked");
    assert!(
        c13_err.message.contains(&c13_dat_id),
        "expected resolved C13 site ID in error: {}",
        c13_err.message
    );
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.get(&c13_sys).copied().unwrap_or(false)
            && reg
                .sites_by_system
                .get(&c13_sys)
                .is_some_and(|sites| sites.iter().any(|s| s.site_id == c13_dat_id))
    }));
}
