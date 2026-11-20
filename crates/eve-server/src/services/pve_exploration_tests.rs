//! Unit and integration tests for `pve_exploration`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock, WorldRepository};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unseeded_dungeon_restrictions_dungeon_id_gate_alias_and_ended_incursion_guards() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_512;
    let ship_id: i64 = 1_050_401_512;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_r5", "pw", char_id, "ExpR5");
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
                type_id: 587, // Rifter (Frigate)
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

    let mut sess = SessionState::new(512);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    // 1. F-01: Unseeded keeper.GetDungeonShipRestrictions / CanEnterDungeonInCurrentShip resolve
    //    real unseeded PvE signatures & anomalies read-only and reject missing strings with DungeonNotFound.
    let sig_specs = crate::services::pve_dungeon_cache::unseeded_pve_signature_specs(&ctx, sys_id);
    assert!(!sig_specs.is_empty());
    let anom_specs = crate::services::pve_dungeon_cache::unseeded_combat_anomaly_specs(&ctx, sys_id);
    assert!(!anom_specs.is_empty());

    let mut pre_sig_replies: Vec<(u32, String, PyRep)> = Vec::new();
    for (sig_did, sig_id) in &sig_specs {
        for meth in ["GetDungeonShipRestrictions", "CanEnterDungeonInCurrentShip"] {
            let rep = handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                meth,
                &[PyRep::String(sig_id.clone())],
                &kw,
            )
            .await
            .expect("handled")
            .unwrap_or_else(|e| panic!("{meth} for {sig_id} failed: {e:?}"));
            if !sig_id.starts_with("DED-") {
                assert_eq!(
                    rep.dict_get("dungeonID").and_then(|v| v.as_i64()),
                    Some(i64::from(*sig_did)),
                    "{meth} must preserve unseeded signature dungeonID for {sig_id}"
                );
            }
            if meth == "GetDungeonShipRestrictions" {
                pre_sig_replies.push((*sig_did, sig_id.clone(), rep));
            }
        }
    }

    let first_anom = &anom_specs[0];
    let mut pre_anom_rep: Option<PyRep> = None;
    for query in [first_anom.site_id.clone(), first_anom.dungeon_id.to_string()] {
        for meth in ["GetDungeonShipRestrictions", "CanEnterDungeonInCurrentShip"] {
            let rep = handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                meth,
                &[PyRep::String(query.clone())],
                &kw,
            )
            .await
            .expect("handled")
            .unwrap_or_else(|e| panic!("{meth} for anomaly {query} failed: {e:?}"));
            assert_eq!(
                rep.dict_get("dungeonID").and_then(|v| v.as_i64()),
                Some(i64::from(first_anom.dungeon_id))
            );
            if meth == "GetDungeonShipRestrictions" && pre_anom_rep.is_none() {
                pre_anom_rep = Some(rep);
            }
        }
    }

    for missing in ["DED-BOGUS-5", "NO-SUCH-SITE", "99999999"] {
        for meth in ["GetDungeonShipRestrictions", "CanEnterDungeonInCurrentShip"] {
            let err = handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                meth,
                &[PyRep::String(missing.into())],
                &kw,
            )
            .await
            .expect("handled")
            .expect_err("unknown string siteID must reject with DungeonNotFound");
            assert_eq!(err.code, "DungeonNotFound", "{meth}({missing})");
        }
    }

    assert!(read_pve_exploration_reg(&ctx, |reg| {
        !reg.seeded_systems.contains_key(&sys_id) && !reg.sites_by_system.contains_key(&sys_id)
    }));
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // 2. P2 #1: is_naturally_seedable_pve_site_id and unseeded ActivateAccelerationGate accept stringified dungeon_id.
    let (sig0_did, _) = &sig_specs[0];
    assert!(super::is_naturally_seedable_pve_site_id(
        &ctx,
        sys_id,
        &first_anom.dungeon_id.to_string(),
    ));
    assert!(super::is_naturally_seedable_pve_site_id(
        &ctx,
        sys_id,
        &sig0_did.to_string(),
    ));
    assert!(!super::is_naturally_seedable_pve_site_id(&ctx, sys_id, "99999999"));

    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
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
    let gate_res = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(first_anom.dungeon_id.to_string())],
        &kw,
    )
    .await
    .expect("handled");
    if let Err(ref e) = gate_res {
        assert_ne!(
            e.code, "DungeonNotFound",
            "stringified natural dungeon_id must resolve past DungeonNotFound"
        );
    }
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.get(&sys_id).copied() == Some(true)
    }));

    // Once seeded, missing dungeon strings still reject with DungeonNotFound in GetDungeonShipRestrictions,
    // and non-SleeperCache seeded anomalies/signatures preserve pre-seeding dungeonID/dungeonName/dedRating (F-02).
    let seeded_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "GetDungeonShipRestrictions",
        &[PyRep::String("DED-BOGUS-5".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("missing string in seeded system must return DungeonNotFound");
    assert_eq!(seeded_err.code, "DungeonNotFound");

    let pre_anom = pre_anom_rep.expect("captured unseeded anomaly restriction reply");
    for query in [first_anom.site_id.clone(), first_anom.dungeon_id.to_string()] {
        for meth in ["GetDungeonShipRestrictions", "CanEnterDungeonInCurrentShip"] {
            let post = handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                meth,
                &[PyRep::String(query.clone())],
                &kw,
            )
            .await
            .expect("handled")
            .unwrap_or_else(|e| panic!("seeded {meth} for anomaly {query} failed: {e:?}"));
            assert_eq!(post.dict_get("dungeonID"), pre_anom.dict_get("dungeonID"), "{meth}({query})");
            assert_eq!(post.dict_get("dungeonName"), pre_anom.dict_get("dungeonName"), "{meth}({query})");
            assert_eq!(post.dict_get("dedRating"), pre_anom.dict_get("dedRating"), "{meth}({query})");
        }
    }
    for (sig_did, sig_id, pre_sig) in &pre_sig_replies {
        if sig_id.starts_with("SLC-") {
            continue;
        }
        for meth in ["GetDungeonShipRestrictions", "CanEnterDungeonInCurrentShip"] {
            let post = handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                meth,
                &[PyRep::String(sig_id.clone())],
                &kw,
            )
            .await
            .expect("handled")
            .unwrap_or_else(|e| panic!("seeded {meth} for sig {sig_id} failed: {e:?}"));
            if !sig_id.starts_with("DED-") {
                assert_eq!(
                    post.dict_get("dungeonID").and_then(|v| v.as_i64()),
                    Some(i64::from(*sig_did)),
                    "seeded {meth} for {sig_id}"
                );
            }
            assert_eq!(post.dict_get("dungeonID"), pre_sig.dict_get("dungeonID"), "{meth}({sig_id})");
            assert_eq!(post.dict_get("dungeonName"), pre_sig.dict_get("dungeonName"), "{meth}({sig_id})");
            assert_eq!(post.dict_get("dedRating"), pre_sig.dict_get("dedRating"), "{meth}({sig_id})");
        }
    }

    // 3. P2 #3: Ended / Withdrawing incursion tales must not satisfy INC-* ActivateAccelerationGate probes.
    let (const_id, _) = crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(&ctx)
        .into_iter()
        .next()
        .expect("eligible incursion constellation");
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        crate::services::pve_incursion_sites::seed_incursion_for_constellation(&ctx, reg, const_id);
    });
    let (inc_sys, inc_site_id, inc_did) =
        crate::services::pve_special_sites::read_pve_special_reg(&ctx, |reg| {
            let inc = reg.incursions.get(&const_id).expect("seeded incursion");
            let s = inc.sites.first().expect("staging incursion site");
            (s.solar_system_id, s.site_id.clone(), s.dungeon_id)
        });
    sess.solar_system_id = Some(inc_sys);
    let _ = ctx
        .systems
        .get_or_spawn(inc_sys)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
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
    for state in ["Ended", "Withdrawing"] {
        crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            reg.incursions.get_mut(&const_id).unwrap().state = state.into();
        });
        for inc_arg in [inc_site_id.clone(), format!("INC-{inc_did}")] {
            let err = handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                "ActivateAccelerationGate",
                &[PyRep::String(inc_arg.clone())],
                &kw,
            )
            .await
            .expect("handled")
            .expect_err("Ended/Withdrawing incursion must reject INC-* gate");
            assert_eq!(err.code, "DungeonNotFound", "state={state}, arg={inc_arg}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_acceleration_gate_read_pure_on_invalid_calls_in_seeded_system() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_513;
    let ship_id: i64 = 1_050_401_513;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_r6_f02", "pw", char_id, "ExpR6F02");
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
                type_id: 587, // Rifter (Frigate)
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
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_820_100)));
    let kw = HashMap::new();

    // Seed the system so it contains an active DSG-{sys_id} DungeonRuntimeInstance.
    ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    let (dsg_inst_id, dsg_entry_gate_ball_id, dsg_r0_gate_ball_id) = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .instances
            .values()
            .find(|i| !i.completed && !i.despawned && i.solar_system_id == sys_id)
            .map(|i| {
                (
                    i.instance_id,
                    i.entry_gate_ball_id
                        .expect("seeded DSG instance has materialized entry_gate_ball_id"),
                    i.rooms
                        .first()
                        .and_then(|r| r.gate_ball_id)
                        .expect("seeded 2-room DSG instance has materialized room 0 gate_ball_id"),
                )
            })
            .expect("seeded system must contain an active DSG DungeonRuntimeInstance")
    });

    // Spawn the pilot's ship ball at origin (well outside 2,500m of DSG-{sys_id} at [68M, -34M, 26M]).
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
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

    let mut sess = SessionState::new(513);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let e_seeded = ctx.db.state_epoch();
    let c_seeded = ctx.db.tx_commit_count();

    // 1. Non-matching integer gate ID falls through as None (no EveDb write).
    assert!(
        handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .is_none()
    );

    // 2. Non-matching string gate IDs and empty args return DungeonNotFound (no EveDb write).
    for bad_args in [
        vec![PyRep::String("NO-SUCH-GATE".into())],
        vec![PyRep::String("DNG-INST-999999".into())],
        Vec::new(),
    ] {
        let err = handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &bad_args,
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("invalid gate arg must return DungeonNotFound");
        assert_eq!(err.code, "DungeonNotFound");
    }

    // 3. EnterNextDungeonRoom with non-matching integer returns DungeonNotFound (no EveDb write).
    let next_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "EnterNextDungeonRoom",
        &[PyRep::Integer(999_999_999)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("invalid EnterNextDungeonRoom integer must return DungeonNotFound");
    assert_eq!(next_err.code, "DungeonNotFound");

    // 4. Shipless session with non-matching integer gate ID must not falsely match has_runtime_gate.
    let mut shipless = sess.clone();
    shipless.ship_id = None;
    assert!(
        handle_pve_exploration(
            &ctx,
            &mut shipless,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .is_none()
    );

    // 5. Direct try_activate_dungeon_runtime_gate misses stay read-pure (with_snapshot).
    assert!(
        crate::services::pve_dungeon_runtime_rpc::try_activate_dungeon_runtime_gate(
            &ctx,
            &mut sess,
            sys_id,
            &[PyRep::Integer(999_999_999)],
        )
        .await
        .is_none()
    );
    assert!(
        crate::services::pve_dungeon_runtime_rpc::try_activate_dungeon_runtime_gate(
            &ctx,
            &mut sess,
            sys_id,
            &[PyRep::String("DNG-INST-999999".into())],
        )
        .await
        .is_none()
    );

    // 6. Matching DSG-{sys_id} runtime gate out-of-range error paths (DNG-INST-*, entry gate, room gate) stay read-pure.
    let dng_tag_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(format!("DNG-INST-{dsg_inst_id}"))],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("out-of-range DNG-INST-* gate must reject");
    assert_eq!(dng_tag_err.code, "OutsideGateActivationRange");

    for matched_gate_id in [dsg_entry_gate_ball_id, dsg_r0_gate_ball_id] {
        let gate_err = handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(matched_gate_id)],
            &kw,
        )
        .await
        .expect("handled")
        .expect_err("out-of-range runtime gate ball must reject");
        assert_eq!(gate_err.code, "OutsideGateActivationRange");
    }

    assert_eq!(ctx.db.state_epoch(), e_seeded);
    assert_eq!(ctx.db.tx_commit_count(), c_seeded);

    // 7. Regression (#1435): Valid active INC-* gate call when incursion metadata is lazily
    //    initialized (balls not yet spawned) must spawn the incursion NPC ball into the ballpark,
    //    while unknown INC-* IDs stay read-pure and return DungeonNotFound.
    let (const_id, _) = crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(&ctx)
        .into_iter()
        .next()
        .expect("eligible incursion constellation");
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        crate::services::pve_incursion_sites::seed_incursion_for_constellation(&ctx, reg, const_id);
    });
    let (inc_sys, inc_site_id, inc_npc_ball_id) =
        crate::services::pve_special_sites::read_pve_special_reg(&ctx, |reg| {
            let inc = reg.incursions.get(&const_id).expect("seeded incursion");
            let s = inc.sites.first().expect("incursion site");
            let first_npc_ball = s
                .escort_spawns
                .first()
                .map(|e| e.npc_ball_id)
                .unwrap_or(s.boss_npc_ball_id);
            (s.solar_system_id, s.site_id.clone(), first_npc_ball)
        });
    let inc_handle = ctx.systems.get_or_spawn(inc_sys);
    assert!(
        inc_handle.get_ball_snapshot(inc_npc_ball_id).await.ok().flatten().is_none(),
        "lazy incursion metadata must not have spawned NPC ball prior to ActivateAccelerationGate"
    );
    sess.solar_system_id = Some(inc_sys);
    let _ = inc_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
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

    let e_inc = ctx.db.state_epoch();
    let c_inc = ctx.db.tx_commit_count();
    let bogus_inc_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String("INC-BOGUS-999999".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unknown INC-* site must reject with DungeonNotFound");
    assert_eq!(bogus_inc_err.code, "DungeonNotFound");
    assert_eq!(ctx.db.state_epoch(), e_inc);
    assert_eq!(ctx.db.tx_commit_count(), c_inc);
    assert!(
        inc_handle.get_ball_snapshot(inc_npc_ball_id).await.ok().flatten().is_none(),
        "bogus INC-* gate call must not spawn incursion NPC balls"
    );

    let inc_gate_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(inc_site_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("active INC-* site with live NPCs rejects with IncursionSiteNpcsRemaining");
    assert_eq!(inc_gate_err.code, "IncursionSiteNpcsRemaining");
    assert!(
        inc_handle.get_ball_snapshot(inc_npc_ball_id).await.ok().flatten().is_some(),
        "valid active INC-* gate call must spawn incursion NPC ball into system ballpark (#1435)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleared_preceding_site_does_not_shadow_active_wave2_writeback_or_gate_lookup() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_051_462;
    let ship_id: i64 = 1_051_462_001;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hf1462_shadow", "pw", char_id, "Hf1462Shadow");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
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
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_000_000));
    ctx.set_clock(clock.clone());
    let now = 1_776_000_000_u64;

    let mut session = SessionState::new(1462);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    let shared_dungeon_id = 1420_u32;
    let site_pos = [15_000_000.0, 0.0, 0.0];
    let shared_gate_ball_id = 9_146_201_i64;
    let cleared_first_id = "EXP-CLEARED-FIRST-1";
    let expired_second_id = "EXP-EXPIRED-SECOND-1";
    let active_third_id = "EXP-ACTIVE-THIRD-1";
    let active_canonical_id = "EXP-ACTIVE-CANONICAL-1";

    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        let sites = reg.sites_by_system.entry(sys_id).or_default();
        // Cleared site sharing dungeon_id and gate_ball_id placed first.
        sites.push(PveSiteInstance {
            site_id: cleared_first_id.into(),
            dungeon_id: shared_dungeon_id,
            dungeon_name: "Guristas Scout Outpost [Cleared]".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 2,
            total_rooms: 2,
            current_wave: 2,
            total_waves: 2,
            active_npc_ball_ids: Vec::new(),
            cleared: true,
            created_unix: now.saturating_sub(1_000),
            lifetime_seconds: 86_400,
            gate_ball_id: Some(shared_gate_ball_id),
            ..Default::default()
        });
        // Uncleared expired site sharing dungeon_id placed second.
        sites.push(PveSiteInstance {
            site_id: expired_second_id.into(),
            dungeon_id: shared_dungeon_id,
            dungeon_name: "Guristas Scout Outpost [Expired]".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: Vec::new(),
            cleared: false,
            created_unix: now.saturating_sub(90_000),
            lifetime_seconds: 86_400,
            gate_ball_id: Some(shared_gate_ball_id),
            ..Default::default()
        });
        // Active site sharing dungeon_id and gate_ball_id placed third.
        sites.push(PveSiteInstance {
            site_id: active_third_id.into(),
            dungeon_id: shared_dungeon_id,
            dungeon_name: "Guristas Scout Outpost [Active]".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: Vec::new(),
            cleared: false,
            created_unix: now,
            lifetime_seconds: 86_400,
            gate_ball_id: Some(shared_gate_ball_id),
            ..Default::default()
        });
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicAnomalyRecord {
                anomaly_id: active_third_id.into(),
                solar_system_id: sys_id,
                name: "Guristas Scout Outpost [Active]".into(),
                anomaly_type: "Combat".into(),
                position: site_pos,
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 0.0,
                created_unix: now,
                lifetime_seconds: 86_400,
                cleared: false,
            });
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicSignatureRecord {
                signature_id: active_third_id.into(),
                solar_system_id: sys_id,
                type_name: "Combat".into(),
                sig_group: "Combat".into(),
                position: site_pos,
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
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

    // 1. Advancing via shared numeric dungeon_id must target active_third_id and write Wave-2 NPCs strictly onto it.
    let wave2_rep = advance_site_wave_or_complete(&ctx, &mut session, sys_id, "1420")
        .await
        .expect("numeric dungeon advance should target active site");
    assert_eq!(
        wave2_rep.dict_get("siteID"),
        Some(&PyRep::String(active_third_id.into()))
    );
    assert_eq!(wave2_rep.dict_get("currentWave"), Some(&PyRep::Integer(2)));
    assert_eq!(
        wave2_rep.dict_get("completed"),
        Some(&PyRep::Bool(false))
    );

    let wave2_balls = read_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&sys_id];
        let cleared_site = sites.iter().find(|s| s.site_id == cleared_first_id).unwrap();
        let expired_site = sites.iter().find(|s| s.site_id == expired_second_id).unwrap();
        let active_site = sites.iter().find(|s| s.site_id == active_third_id).unwrap();
        assert!(
            cleared_site.active_npc_ball_ids.is_empty(),
            "cleared preceding site must never absorb Wave-2 NPC ball IDs"
        );
        assert!(
            expired_site.active_npc_ball_ids.is_empty(),
            "expired preceding site must never absorb Wave-2 NPC ball IDs"
        );
        assert!(
            !active_site.active_npc_ball_ids.is_empty(),
            "active site must retain spawned Wave-2 NPC ball IDs"
        );
        assert_eq!(active_site.current_wave, 2);
        active_site.active_npc_ball_ids.clone()
    });

    // 2. Immediate repeat gate advance while Wave-2 NPCs are alive must fail with WaveNpcsRemaining (no wave skip).
    let skip_err = advance_site_wave_or_complete(&ctx, &mut session, sys_id, "1420")
        .await
        .expect_err("WaveNpcsRemaining must block immediate second advance");
    assert_eq!(skip_err.code, "WaveNpcsRemaining");

    // 3. Killing Wave-2 NPCs clears active_third_id + its canonical anomaly and signature.
    for (idx, bid) in wave2_balls.iter().enumerate() {
        let _ = sys_handle.remove_ball(*bid).await;
        let (cleared_opt, _, _) = handle_pve_site_npc_destruction_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.mechanic_rng,
            &*ctx.combat_ephemeral,
            sys_id,
            *bid,
            char_id,
            false,
        )
        .await;
        if idx + 1 == wave2_balls.len() {
            assert_eq!(cleared_opt.as_deref(), Some(active_third_id));
        } else {
            assert!(cleared_opt.is_none());
        }
    }
    read_pve_exploration_reg(&ctx, |reg| {
        let active_site = reg.sites_by_system[&sys_id]
            .iter()
            .find(|s| s.site_id == active_third_id)
            .unwrap();
        assert!(active_site.cleared);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies[&sys_id]
            .iter()
            .find(|a| a.anomaly_id == active_third_id)
            .unwrap()
            .cleared);
        assert!(snap.cosmic_signatures[&sys_id]
            .iter()
            .find(|s| s.signature_id == active_third_id)
            .unwrap()
            .collapsed);
    });

    // 4. Seed a second active site sharing dungeon_id & gate_ball_id and advance using canonical site_id directly
    // (previously line 925 `s.site_id == site_id || s.dungeon_id == dungeon_id` still matched cleared_first_id!).
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .entry(sys_id)
            .or_default()
            .push(PveSiteInstance {
                site_id: active_canonical_id.into(),
                dungeon_id: shared_dungeon_id,
                dungeon_name: "Guristas Scout Outpost [Canonical]".into(),
                solar_system_id: sys_id,
                site_category: "CombatAnomaly".into(),
                position: site_pos,
                signal_strength: 1.0,
                current_room: 1,
                total_rooms: 2,
                current_wave: 1,
                total_waves: 2,
                active_npc_ball_ids: Vec::new(),
                cleared: false,
                created_unix: now,
                lifetime_seconds: 86_400,
                gate_ball_id: Some(shared_gate_ball_id),
                ..Default::default()
            });
    });

    // Gate lookup by shared integer gate_ball_id must pick active_canonical_id over cleared/expired sites.
    let kw = HashMap::new();
    let gate_res = handle_pve_exploration(
        &ctx,
        &mut session,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(shared_gate_ball_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("ActivateAccelerationGate by gate_ball_id must advance active site");
    assert_eq!(
        gate_res.dict_get("siteID"),
        Some(&PyRep::String(active_canonical_id.into()))
    );
    assert_eq!(gate_res.dict_get("currentWave"), Some(&PyRep::Integer(2)));

    read_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&sys_id];
        let cleared_site = sites.iter().find(|s| s.site_id == cleared_first_id).unwrap();
        let canonical_site = sites
            .iter()
            .find(|s| s.site_id == active_canonical_id)
            .unwrap();
        assert!(cleared_site.active_npc_ball_ids.is_empty());
        assert!(!canonical_site.active_npc_ball_ids.is_empty());
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_unpruned_sites_reject_and_prune_on_npc_kill_and_gate_advance() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_052_462;
    let ship_id: i64 = 1_052_462_001;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("hf1462_expiry", "pw", char_id, "Hf1462Expiry");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
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
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_100_000));
    ctx.set_clock(clock.clone());
    let base_now = 1_776_100_000_u64;

    let mut session = SessionState::new(1463);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    let site_pos = [20_000_000.0, 0.0, 0.0];
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let spawn_npc = |bid: i64| {
        let h = sys_handle.clone();
        let cs = crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(
            &ctx.sde, 23_333,
        );
        async move {
            let _ = h
                .spawn_ball_with_combat(
                    eve_destiny::Ball {
                        id: bid,
                        type_id: 23_333,
                        position: glam::DVec3::from_array(site_pos),
                        velocity: glam::DVec3::ZERO,
                        max_velocity: 200.0,
                        inertia: 1.0,
                        mass: 500_000.0,
                        radius: 40.0,
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    cs,
                )
                .await;
        }
    };

    let npc_kill_wave1 = 9_146_301_i64;
    let npc_kill_final = 9_146_302_i64;
    let npc_gate_expired = 9_146_303_i64;
    let npc_linked_esc_expired = 9_146_304_i64;
    for bid in [
        npc_kill_wave1,
        npc_kill_final,
        npc_gate_expired,
        npc_linked_esc_expired,
    ] {
        spawn_npc(bid).await;
    }

    let linked_inst_id = 888_462_i64;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        let sites = reg.sites_by_system.entry(sys_id).or_default();
        // 1. Multi-wave combat anomaly whose site lifetime has expired.
        sites.push(PveSiteInstance {
            site_id: "ANOM-EXP-W1".into(),
            dungeon_id: 1420,
            dungeon_name: "Expired Outpost W1".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: vec![npc_kill_wave1],
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 3_600,
            ..Default::default()
        });
        // 2. Final-wave combat anomaly whose site lifetime has expired.
        sites.push(PveSiteInstance {
            site_id: "ANOM-EXP-FINAL".into(),
            dungeon_id: 1420,
            dungeon_name: "Expired Outpost Final".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![npc_kill_final],
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 3_600,
            ..Default::default()
        });
        // 3. Expired site tested via advance_site_wave_or_complete (no remaining balls in space or unpruned NPC).
        sites.push(PveSiteInstance {
            site_id: "SITE-EXP-GATE".into(),
            dungeon_id: 1430,
            dungeon_name: "Expired Gate Site".into(),
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: vec![npc_gate_expired],
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 3_600,
            ..Default::default()
        });
        // 4. Site with lifetime_seconds == 0 whose linked EscalatingPathEntry has expired.
        sites.push(PveSiteInstance {
            site_id: format!("EXP-{linked_inst_id}-1"),
            dungeon_id: 1420,
            dungeon_name: "Linked Expired Escalation".into(),
            solar_system_id: sys_id,
            site_category: "Expedition".into(),
            position: site_pos,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![npc_linked_esc_expired],
            cleared: false,
            created_unix: base_now,
            lifetime_seconds: 0,
            expedition_instance_id: Some(linked_inst_id),
            ..Default::default()
        });
        reg.escalations_by_char
            .entry(char_id)
            .or_default()
            .push(EscalatingPathEntry {
                instance_id: linked_inst_id,
                character_id: char_id,
                source_dungeon_id: 1420,
                expedition_dungeon_id: 1420,
                expedition_name: "Linked Expired Escalation".into(),
                destination_solar_system_id: sys_id,
                step_index: 1,
                total_steps: 2,
                journal_text: "Expired".into(),
                created_at_sec: base_now,
                expires_at_sec: base_now + 3_600,
                dungeons_entered: vec![1420],
                journal_entry_template_id: 0,
            });
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicAnomalyRecord {
                anomaly_id: "ANOM-EXP-W1".into(),
                solar_system_id: sys_id,
                name: "Expired Outpost W1".into(),
                anomaly_type: "Combat".into(),
                position: site_pos,
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 0.0,
                created_unix: base_now,
                lifetime_seconds: 3_600,
                cleared: false,
            });
        snap.bookmarks.insert(
            77_1462,
            eve_db::BookmarkRecord {
                bookmark_id: 77_1462,
                owner_id: char_id,
                label: "Expired Linked Esc BM".into(),
                solar_system_id: sys_id,
                x: site_pos[0],
                y: site_pos[1],
                z: site_pos[2],
                item_id: Some(linked_inst_id),
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    let items_before = ctx.db.with_snapshot(|snap| snap.items.len());
    clock.advance_secs(4_000);

    // Case 1 & 2: Killing Wave-1 or final-wave NPC in expired site must return None, spawn no Wave 2, drop no loot, and prune site/balls.
    let res1 = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &*ctx.combat_ephemeral,
        sys_id,
        npc_kill_wave1,
        char_id,
        false,
    )
    .await;
    assert!(res1.0.is_none() && res1.1.is_none() && res1.2.is_none());

    let res2 = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &*ctx.combat_ephemeral,
        sys_id,
        npc_kill_final,
        char_id,
        false,
    )
    .await;
    assert!(res2.0.is_none() && res2.1.is_none() && res2.2.is_none());

    // Case 3: Gate / wave advance on expired site must reject with SiteExpired and prune site + live NPC ball.
    let gate_err = advance_site_wave_or_complete(&ctx, &mut session, sys_id, "SITE-EXP-GATE")
        .await
        .expect_err("expired site must reject with SiteExpired");
    assert_eq!(gate_err.code, "SiteExpired");

    // Case 4: Active-lifetime_seconds==0 site linked to an expired EscalatingPathEntry must reject on NPC kill and prune escalation + bookmark + ball.
    let res4 = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &*ctx.combat_ephemeral,
        sys_id,
        npc_linked_esc_expired,
        char_id,
        false,
    )
    .await;
    assert!(res4.0.is_none() && res4.1.is_none() && res4.2.is_none());

    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items.len()),
        items_before,
        "expired unpruned site kills/advances must drop zero loot items"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_anomalies[&sys_id]
            .iter()
            .find(|a| a.anomaly_id == "ANOM-EXP-W1")
            .unwrap()
            .cleared);
        assert!(!snap.bookmarks.contains_key(&77_1462));
    });
    read_pve_exploration_reg(&ctx, |reg| {
        for sid in [
            "ANOM-EXP-W1",
            "ANOM-EXP-FINAL",
            "SITE-EXP-GATE",
            &format!("EXP-{linked_inst_id}-1"),
        ] {
            let site = reg.sites_by_system[&sys_id]
                .iter()
                .find(|s| s.site_id == sid)
                .unwrap();
            assert!(site.cleared, "{sid} must be marked cleared after expiry prune");
            assert_eq!(site.current_wave, 1, "{sid} must never advance to Wave 2");
            assert!(site.active_npc_ball_ids.is_empty());
        }
        assert!(
            reg.escalations_by_char
                .get(&char_id)
                .is_none_or(|l| l.is_empty()),
            "expired linked escalation must be pruned"
        );
    });
    for bid in [
        npc_kill_wave1,
        npc_kill_final,
        npc_gate_expired,
        npc_linked_esc_expired,
    ] {
        assert_eq!(
            sys_handle.get_ball_snapshot(bid).await.unwrap(),
            None,
            "expired NPC ball {bid} must be removed from ballpark"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exploration_arg_and_movement_prechecks_prevent_unseeded_mutations() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_050_599;
    let ship_id: i64 = 1_050_405_599;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("exp_track_d", "pw", char_id, "ExpTrackD");
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
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_825_000)));
    let kw = HashMap::new();

    let mut combat = ctx.build_ship_combat_state(char_id, ship_id, 587);
    combat.warp_scramble_strength = 5;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            combat,
        )
        .await;

    let mut sess = SessionState::new(599);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();

    // 1. Invalid args on restrictions, anomaly queries, and scanSvc escalation history must fail read-purely.
    for (svc, meth, bad_args) in [
        ("keeper", "GetDungeonShipRestrictions", vec![]),
        ("keeper", "GetDungeonShipRestrictions", vec![PyRep::Bool(true)]),
        ("keeper", "GetDungeonShipRestrictions", vec![PyRep::Integer(0)]),
        ("keeper", "GetDungeonShipRestrictions", vec![PyRep::Integer(-1)]),
        (
            "keeper",
            "CanEnterDungeonInCurrentShip",
            vec![PyRep::Integer(2), PyRep::Integer(3)],
        ),
        ("scanSvc", "GetCombatAnomalyInstances", vec![PyRep::Bool(true)]),
        (
            "scanSvc",
            "GetCombatAnomalyInstances",
            vec![PyRep::Integer(sys_id as i64), PyRep::Integer(1)],
        ),
        (
            "scanSvc",
            "EnterCombatAnomaly",
            vec![PyRep::String("PVE-ANOM-30000142-1".into()), PyRep::Integer(1)],
        ),
        ("scanSvc", "GetEscalatingPathDungeonsEntered", vec![PyRep::Integer(90_000_002)]),
        ("dungeonExplorationMgr", "GetEscalatingPathDungeonsEntered", vec![PyRep::Integer(0)]),
    ] {
        let err = handle_pve_exploration(&ctx, &mut sess, svc, meth, &bad_args, &kw)
            .await
            .expect("handled")
            .expect_err("invalid argument probe must fail");
        assert_eq!(err.code, "InvalidArguments", "{svc}.{meth}");
    }

    // 2. Warp-scrambled ship must reject natural unseeded gate/wave advancement BEFORE seeding DB.
    let ded_gate_ball_id = 1_864_000_000 + i64::from(sys_id % 100_000) * 10 + 3;
    let first_anom = crate::services::pve_dungeon_cache::unseeded_combat_anomaly_specs(&ctx, sys_id)
        .into_iter()
        .next()
        .expect("unseeded anomaly spec");
    for (svc, meth, args) in [
        (
            "scanSvc",
            "EnterCombatAnomaly",
            vec![PyRep::String(first_anom.site_id), PyRep::Bool(true)],
        ),
        ("keeper", "ActivateAccelerationGate", vec![PyRep::Integer(ded_gate_ball_id)]),
        ("keeper", "EnterNextDungeonRoom", vec![PyRep::String("DED-30000142-3".into())]),
    ] {
        let err = handle_pve_exploration(&ctx, &mut sess, svc, meth, &args, &kw)
            .await
            .expect("handled")
            .expect_err("warp-scrambled movement precheck must reject");
        assert!(!err.code.is_empty(), "{svc}.{meth}");
    }
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        !reg.seeded_systems.contains_key(&sys_id) && !reg.sites_by_system.contains_key(&sys_id)
    }));
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // Clear warp scramble -> valid dungeonExplorationMgr escalation query and ship restrictions succeed cleanly.
    let mut clear_combat = ctx.build_ship_combat_state(char_id, ship_id, 587);
    clear_combat.warp_scramble_strength = 0;
    let _ = sys_h.set_combat_state(ship_id, clear_combat).await;

    let entered = handle_pve_exploration(
        &ctx,
        &mut sess,
        "dungeonExplorationMgr",
        "GetEscalatingPathDungeonsEntered",
        &[PyRep::Integer(1)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("valid instance_id query succeeds");
    assert_eq!(entered, PyRep::List(Vec::new()));

    // Locked Mothership INC-* gate probe rejects with MothershipSiteLocked before spawning incursion NPC balls.
    let (const_id, _) = crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(&ctx)
        .into_iter()
        .next()
        .expect("eligible incursion constellation");
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        crate::services::pve_incursion_sites::seed_incursion_for_constellation(&ctx, reg, const_id);
    });
    let (hq_sys, hq_site_id, hq_boss_ball) = crate::services::pve_special_sites::read_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get(&const_id).expect("seeded incursion");
        let hq = inc
            .sites
            .iter()
            .find(|s| s.locked_until_low_influence)
            .expect("locked Mothership site");
        (hq.solar_system_id, hq.site_id.clone(), hq.boss_npc_ball_id)
    });
    sess.solar_system_id = Some(hq_sys);
    let hq_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(hq_site_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("locked Mothership site must reject before spawning balls");
    assert_eq!(hq_err.code, "MothershipSiteLocked");
    assert_eq!(
        ctx.systems.get_or_spawn(hq_sys).get_ball_snapshot(hq_boss_ball).await.unwrap(),
        None,
        "locked Mothership probe must not spawn incursion balls"
    );
}

#[tokio::test]
async fn incursion_mothership_5158_does_not_seed_and_dungeons_entered_filters_by_instance_id() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_800_000)));
    let kw = HashMap::new();

    // 1. F-03: On an uninitialized special-sites registry, INC-<const_id>-5158 returns MothershipSiteLocked read-purely.
    let eligible = crate::services::pve_incursion_lifecycle::eligible_incursion_constellations(&ctx);
    let (const_id, _) = eligible.first().copied().expect("eligible incursion constellation");
    let mut sys_list: Vec<SolarSystemId> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.constellation_id == const_id)
        .map(|s| s.solar_system_id)
        .collect();
    sys_list.sort_unstable();
    let (_, _, _, hq_sys) = crate::services::pve_incursion_lifecycle::assign_constellation_system_roles(
        &ctx.sde,
        const_id,
        &sys_list,
    );
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = hq_sys;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = hq_sys as i64;
        }
    });
    let hq_handle = ctx.systems.get_or_spawn(hq_sys);
    let _ = hq_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(hq_sys),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    assert!(!is_naturally_seedable_incursion_site(&ctx, hq_sys, "INC-5158"));
    let e0 = ctx.db.state_epoch();
    let c0 = ctx.db.tx_commit_count();
    let err_mothership = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(format!("INC-{const_id}-5158"))],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unseeded INC-*-5158 must return MothershipSiteLocked");
    assert_eq!(err_mothership.code, "MothershipSiteLocked");
    assert!(crate::services::pve_special_sites::read_pve_special_reg(&ctx, |r| !r.initialized));
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // 2. F-04: GetEscalatingPathDungeonsEntered(instance_id) filters strictly by instance_id read-purely.
    let now = with_pve_exploration_reg(&ctx, |reg| now_sec(reg));
    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.insert(
            char_id,
            vec![
                crate::services::pve_expeditions::EscalatingPathEntry {
                    instance_id: 1_001,
                    character_id: char_id,
                    source_dungeon_id: 1420,
                    expedition_dungeon_id: 1421,
                    expedition_name: "Escalation Alpha".into(),
                    destination_solar_system_id: hq_sys,
                    step_index: 2,
                    total_steps: 3,
                    journal_text: "Alpha".into(),
                    created_at_sec: now,
                    expires_at_sec: now + 3_600,
                    dungeons_entered: vec![1420, 1421],
                    journal_entry_template_id: 0,
                },
                crate::services::pve_expeditions::EscalatingPathEntry {
                    instance_id: 1_002,
                    character_id: char_id,
                    source_dungeon_id: 2420,
                    expedition_dungeon_id: 2421,
                    expedition_name: "Escalation Beta".into(),
                    destination_solar_system_id: hq_sys,
                    step_index: 1,
                    total_steps: 2,
                    journal_text: "Beta".into(),
                    created_at_sec: now,
                    expires_at_sec: now + 3_600,
                    dungeons_entered: vec![2420],
                    journal_entry_template_id: 0,
                },
            ],
        );
    });
    let e_esc = ctx.db.state_epoch();
    let c_esc = ctx.db.tx_commit_count();
    let only_1001 = handle_pve_exploration(
        &ctx,
        &mut sess,
        "dungeonExplorationMgr",
        "GetEscalatingPathDungeonsEntered",
        &[PyRep::Integer(1_001)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(only_1001, PyRep::List(vec![PyRep::Integer(1420), PyRep::Integer(1421)]));

    let only_1002 = handle_pve_exploration(
        &ctx,
        &mut sess,
        "dungeonExplorationMgr",
        "GetEscalatingPathDungeonsEntered",
        &[PyRep::Integer(1_002)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(only_1002, PyRep::List(vec![PyRep::Integer(2420)]));

    let missing_inst = handle_pve_exploration(
        &ctx,
        &mut sess,
        "dungeonExplorationMgr",
        "GetEscalatingPathDungeonsEntered",
        &[PyRep::Integer(9_999)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(missing_inst, PyRep::List(Vec::new()));

    let all_entered = handle_pve_exploration(
        &ctx,
        &mut sess,
        "dungeonExplorationMgr",
        "GetEscalatingPathDungeonsEntered",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        all_entered,
        PyRep::List(vec![PyRep::Integer(1420), PyRep::Integer(1421), PyRep::Integer(2420)])
    );
    assert_eq!(ctx.db.state_epoch(), e_esc);
    assert_eq!(ctx.db.tx_commit_count(), c_esc);
}
