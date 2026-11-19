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

    let dng_entered = handle_pve_exploration(
        &ctx,
        &mut docked,
        "dungeonExplorationMgr",
        "GetEscalatingPathDungeonsEntered",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("read-only dungeonExplorationMgr.GetEscalatingPathDungeonsEntered ok");
    assert_eq!(dng_entered, PyRep::List(Vec::new()));

    for (svc, meth, args) in [
        ("keeper", "GetDungeonShipRestrictions", vec![PyRep::Integer(2)]),
        ("keeper", "CanEnterDungeonInCurrentShip", vec![PyRep::Integer(2)]),
        ("keeper", "GetDungeonInfo", vec![PyRep::String("DED-30000142-3".into())]),
        ("keeper", "GetDungeonObjectivesData", vec![PyRep::String("DED-30000142-3".into())]),
        ("keeper", "ActivateAccelerationGate", vec![PyRep::String("DED-30000142-3".into())]),
        ("keeper", "EnterNextDungeonRoom", vec![PyRep::String("DED-30000142-3".into())]),
        ("beyonce", "ActivateAccelerationGate", vec![PyRep::String("DED-30000142-3".into())]),
        ("scanSvc", "EnterCombatAnomaly", vec![PyRep::String("PVE-ANOM-30000142-1".into())]),
        ("scanSvc", "GetEscalatingPathDungeonsEntered", vec![]),
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
        ("keeper", "CanEnterDungeonInCurrentShip", vec![PyRep::Integer(2)]),
        ("scanSvc", "EnterCombatAnomaly", vec![PyRep::String("PVE-ANOM-30000142-1".into())]),
        ("scanSvc", "GetEscalatingPathDungeonsEntered", vec![]),
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
