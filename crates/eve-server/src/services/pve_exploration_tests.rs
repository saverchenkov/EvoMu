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

    // 2. Unseeded remote-system query emits empty list read-only without mutating DB/reg/systems (PVE-081).
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
        remote_items.is_empty(),
        "PVE-081: unseeded remote system must return empty anomaly list without predicting fake sites"
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

    // 4. keeper.ActivateAccelerationGate uses canonical site_id and rejects stringified dungeon_id (#1495).
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
    let alias_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String("424242".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("stringified dungeon_id must not alias canonical site_id");
    assert_eq!(alias_err.code, "DungeonNotFound");
    let res = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String("DED-30000142-ALIAS".into())],
        &kw,
    )
    .await
    .expect("handled")
    .expect("ActivateAccelerationGate by canonical site_id must advance room");
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

    // 2. After seeding null_sys, dynamically allocated DED gate ball ID resolves cleanly to its site (PVE-213).
    sess.solar_system_id = Some(null_sys);
    crate::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, null_sys).await;
    let (ded_site_id, ded_pos, valid_null_gate) = read_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&null_sys)
            .and_then(|sites| {
                sites.iter().find_map(|s| {
                    s.site_id
                        .starts_with(&format!("DED-{null_sys}-"))
                        .then(|| s.gate_ball_id.map(|gid| (s.site_id.clone(), s.position, gid)))
                        .flatten()
                })
            })
            .expect("seeded null-sec DED site with dynamic gate_ball_id")
    });
    assert!(valid_null_gate > 0);
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
    .expect_err("seeded null-sec DED gate resolves room NPC guard check");
    assert_eq!(err.code, "WaveNpcsRemaining");
    assert!(
        err.message.contains(&ded_site_id),
        "expected resolved null-sec DED site ID in error: {}",
        err.message
    );
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

    // 2. Unseeded systems reject shattered/C13 site entries read-purely (PVE-081); once seeded,
    //    shattered WH anomalies (SHD-/SHI-) and C13 signatures (DAT-*-1) enter/resolve cleanly.
    sess.solar_system_id = Some(shattered_sys);
    let shat_pre_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "scanSvc",
        "EnterCombatAnomaly",
        &[PyRep::String(format!("SHD-{shattered_sys}"))],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("unseeded SHD anomaly must fail AnomalyNotFound without auto-seeding");
    assert_eq!(shat_pre_err.code, "AnomalyNotFound");
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    crate::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, shattered_sys).await;
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
        .expect("seeded shattered WH anomaly must enter cleanly");
        assert_eq!(rep.dict_get("siteID"), Some(&PyRep::String(shat_anom)));
    }

    crate::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, c13_sys).await;
    let c13_dat_id = format!("DAT-{c13_sys}-1");
    let dat1_pos = read_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&c13_sys]
            .iter()
            .find(|s| s.site_id == c13_dat_id)
            .map(|s| s.position)
            .expect("seeded C13 DAT-*-1 site")
    });
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
    .expect_err("seeded C13 DAT-*-1 signature gate resolves seeded container check");
    assert_eq!(c13_err.code, "SiteContainersUnhacked");
    assert!(
        c13_err.message.contains(&c13_dat_id),
        "expected resolved C13 site ID in error: {}",
        c13_err.message
    );
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

    // 1. PVE-081: Unseeded keeper.GetDungeonShipRestrictions / CanEnterDungeonInCurrentShip reject unseeded site strings
    //    read-only without fabricating unseeded predictor sites.
    for missing in ["DED-BOGUS-5", "NO-SUCH-SITE", "99999999", &format!("PVE-ANOM-{sys_id}-1")] {
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
            .expect_err("unseeded string siteID must reject with DungeonNotFound");
            assert_eq!(err.code, "DungeonNotFound", "{meth}({missing})");
        }
    }
    assert!(read_pve_exploration_reg(&ctx, |reg| {
        !reg.seeded_systems.contains_key(&sys_id) && !reg.sites_by_system.contains_key(&sys_id)
    }));
    assert_eq!(ctx.db.state_epoch(), e0);
    assert_eq!(ctx.db.tx_commit_count(), c0);

    // 2. #1495: Seed sys_id; restrictions & gate activation accept canonical site_id but strictly reject numeric dungeon_id strings.
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
    crate::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    let ((anom_site_id, anom_did), (sig_site_id, sig_did)) = read_pve_exploration_reg(&ctx, |reg| {
        let sites = &reg.sites_by_system[&sys_id];
        let a = sites
            .iter()
            .find(|s| !s.probe_required && !s.cleared)
            .map(|s| (s.site_id.clone(), s.dungeon_id))
            .expect("seeded combat anomaly");
        let sig = sites
            .iter()
            .find(|s| {
                s.probe_required
                    && !s.cleared
                    && s.ded_rating.is_none()
                    && !s.site_id.starts_with("SLC-")
                    && !s.site_id.starts_with("DED-")
            })
            .map(|s| (s.site_id.clone(), s.dungeon_id))
            .expect("seeded non-DED/non-SLC signature");
        (a, sig)
    });

    for (site_id, did) in [(&anom_site_id, anom_did), (&sig_site_id, sig_did)] {
        for meth in ["GetDungeonShipRestrictions", "CanEnterDungeonInCurrentShip"] {
            let ok_rep = handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                meth,
                &[PyRep::String(site_id.clone())],
                &kw,
            )
            .await
            .expect("handled")
            .unwrap_or_else(|e| panic!("seeded {meth} for {site_id} failed: {e:?}"));
            assert_eq!(
                ok_rep.dict_get("dungeonID").and_then(|v| v.as_i64()),
                Some(i64::from(did))
            );
            let alias_err = handle_pve_exploration(
                &ctx,
                &mut sess,
                "keeper",
                meth,
                &[PyRep::String(did.to_string())],
                &kw,
            )
            .await
            .expect("handled")
            .expect_err("numeric dungeon_id string must not alias canonical site_id");
            assert_eq!(alias_err.code, "DungeonNotFound", "{meth}({did})");
        }
    }

    let alias_gate_err = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(anom_did.to_string())],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("ActivateAccelerationGate must reject stringified dungeon_id");
    assert_eq!(alias_gate_err.code, "DungeonNotFound");

    let gate_res = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::String(anom_site_id)],
        &kw,
    )
    .await
    .expect("handled");
    if let Err(ref e) = gate_res {
        assert_ne!(
            e.code, "DungeonNotFound",
            "canonical site_id must resolve past DungeonNotFound"
        );
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

    // 2. Warp-scrambled ship rejects gate/wave advancement BEFORE mutating DB, while unrecognized
    // integer gate ball IDs return None without mutating DB (allowing mission gates downstream).
    for (svc, meth, args, expected_code) in [
        (
            "scanSvc",
            "EnterCombatAnomaly",
            vec![PyRep::String("PVE-ANOM-30000142-1".into()), PyRep::Bool(true)],
            "WarpScrambled",
        ),
        (
            "keeper",
            "ActivateAccelerationGate",
            vec![PyRep::String("DED-30000142-3".into())],
            "DungeonNotFound",
        ),
        (
            "keeper",
            "EnterNextDungeonRoom",
            vec![PyRep::String("DED-30000142-3".into())],
            "DungeonNotFound",
        ),
        (
            "keeper",
            "EnterNextDungeonRoom",
            vec![PyRep::Integer(999_888_777)],
            "DungeonNotFound",
        ),
    ] {
        let err = handle_pve_exploration(&ctx, &mut sess, svc, meth, &args, &kw)
            .await
            .expect("handled")
            .expect_err("warp-scrambled/unmatched movement precheck must reject");
        assert_eq!(err.code, expected_code, "{svc}.{meth}");
    }
    assert!(
        handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "ActivateAccelerationGate",
            &[PyRep::Integer(999_888_777)],
            &kw,
        )
        .await
        .is_none(),
        "unknown integer gate ball should fall through to downstream gate services without seeding"
    );
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

#[test]
fn typed_site_identity_lookup_and_unified_expiry_rule_reject_aliases_and_expired_siblings() {
    let mut reg = PveExplorationRegistry::default();
    let now: u64 = 1_780_000_000;
    let exp_inst = EscalationInstanceId::new(42);

    // Linked escalation is expired (`expires_at_sec <= now`) even though the site's own lifetime is still fresh!
    reg.escalations_by_char.insert(
        90_000_001,
        vec![EscalatingPathEntry {
            instance_id: exp_inst.as_i64(),
            character_id: 90_000_001,
            source_dungeon_id: 2134,
            expedition_dungeon_id: 400_001,
            expedition_name: "Expired Esc".into(),
            destination_solar_system_id: 30_000_142,
            step_index: 1,
            total_steps: 2,
            journal_text: String::new(),
            created_at_sec: now - 1_000,
            expires_at_sec: now - 1,
            dungeons_entered: vec![],
            journal_entry_template_id: 0,
        }],
    );

    let expired_linked = PveSiteInstance {
        site_id: "EXP-42-1".into(),
        dungeon_id: 400_001,
        solar_system_id: 30_000_142,
        site_category: "Expedition".into(),
        created_unix: now - 10,
        lifetime_seconds: 86_400,
        expedition_instance_id: Some(exp_inst.as_i64()),
        ..Default::default()
    };
    let active_same_template = PveSiteInstance {
        site_id: "EXP-99-1".into(),
        dungeon_id: 400_001,
        solar_system_id: 30_000_142,
        site_category: "Expedition".into(),
        created_unix: now - 10,
        lifetime_seconds: 86_400,
        expedition_instance_id: None,
        ..Default::default()
    };
    reg.assign_site_instance_id("EXP-42-1", ExplorationSiteId::new(10_000_501));
    reg.assign_site_instance_id("EXP-99-1", ExplorationSiteId::new(10_000_502));
    let sites = vec![expired_linked.clone(), active_same_template.clone()];

    // Unified expiry rule: catches escalation-expiry #1464 and finite lifetime expiry consistently.
    assert!(is_site_expired_in_reg(&reg, &expired_linked, now));
    assert!(!is_site_expired_in_reg(&reg, &active_same_template, now));
    assert!(is_lifetime_expired(now - 100, 100, now));
    assert!(!is_lifetime_expired(now - 99, 100, now));
    assert!(!is_lifetime_expired(now - 100, 0, now));
    assert!(is_lifetime_expired(0, 100, now));

    // Canonical string lookup rejects stringified dungeon_id aliases.
    assert!(
        find_site_by_canonical_key_in_reg(&reg, &sites, ExplorationSiteKeyRef("400001"), now)
            .is_none()
    );
    assert_eq!(
        find_site_by_canonical_key_in_reg(&reg, &sites, ExplorationSiteKeyRef("EXP-99-1"), now)
            .map(|s| s.site_id.as_str()),
        Some("EXP-99-1")
    );

    // Typed ExplorationSiteId vs DungeonTemplateId lookups never conflate namespaces.
    assert!(
        find_active_site_by_id_in_reg(&reg, &sites, ExplorationSiteId::new(400_001), now).is_none()
    );
    assert!(
        find_active_site_by_id_in_reg(&reg, &sites, ExplorationSiteId::new(10_000_501), now)
            .is_none(),
        "linked expired escalation must not be returned as active"
    );
    assert_eq!(
        find_active_site_by_id_in_reg(&reg, &sites, ExplorationSiteId::new(10_000_502), now)
            .map(|s| s.site_id.as_str()),
        Some("EXP-99-1")
    );
    assert_eq!(
        find_active_site_by_dungeon_template_in_reg(
            &reg,
            &sites,
            DungeonTemplateId::new(400_001),
            now,
        )
        .map(|s| s.site_id.as_str()),
        Some("EXP-99-1"),
        "template lookup must skip expired sibling and find active instance"
    );
    assert!(find_active_site_by_dungeon_template_in_reg(
        &reg,
        &sites,
        DungeonTemplateId::new(10_000_502),
        now,
    )
    .is_none());
}
