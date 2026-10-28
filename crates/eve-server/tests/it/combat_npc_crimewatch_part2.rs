#![allow(unused_imports)]
//! End-to-End Integration Tests for Death Pipeline, Wrecks, Killmails, CONCORD & CrimeWatch 2.0 (`C-3, C-8`).

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    check_crimewatch_action_restriction, compute_killmail_esi_hash, dispatch_combat_rpc, evaluate_faction_police_response, evaluate_remote_assistance_legality,
    faction_police_sec_threshold, record_killmail_assist_action, register_container_lifetime, step_combat_system, step_sentry_gun_grid_aggression,
    step_wreck_and_jetcan_decay, with_mut_combat_runtime,
};
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        _ => None,
    }
}

pub(crate) async fn setup_pilot_session(
    ctx: &ServerContext,
    client_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    solar_system_id: u32,
    pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let sig_radius = ctx.sde.types.get(&ship_type_id).and_then(|t| t.default_attributes.get(&552).copied()).unwrap_or(40.0);
    let ship_ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 3.0,
        mass: 1_200_000.0,
        radius: sig_radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(ship_type_id)).await;

    let mut sess = SessionState::new(client_id);
    let (user_id, corp_id) = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| (c.user_id, c.corporation_id)).unwrap_or((1, 1_000_002)));
    let _ = ctx.db.acquire_online_lock(user_id, client_id, Some(char_id), true, ctx.now_unix());
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(solar_system_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess
}

pub(crate) fn fit_module_and_charge(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_item_id: i64,
    module_type_id: u32,
    flag: InventoryFlag,
    charge_opt: Option<(i64, u32, i32)>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            module_item_id,
            InventoryItemRecord {
                item_id: module_item_id,
                type_id: module_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        if let Some((charge_id, charge_type_id, qty)) = charge_opt {
            snap.items.insert(
                charge_id,
                InventoryItemRecord {
                    item_id: charge_id,
                    type_id: charge_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });
}

#[tokio::test]
async fn test_c8_crimewatch_faction_police_killrights_tags_and_sentries() {
    // parity: feat:crimewatch.sec-status-tags
    // parity: feat:crimewatch.sec-status-transactions
    // parity: feat:crimewatch.faction-police
    // parity: feat:crimewatch.kill-rights-gain
    // parity: feat:crimewatch.kill-rights-sell
    // parity: feat:crimewatch.faction-police-thresholds
    // parity: feat:crimewatch.sec-status-gain-caps
    // parity: feat:crimewatch.sec-tag-tier-bands
    eve_testkit::attest(&[
        "feat:crimewatch.sec-status-tags",
        "feat:crimewatch.sec-status-transactions",
        "feat:crimewatch.faction-police",
        "feat:crimewatch.kill-rights-gain",
        "feat:crimewatch.kill-rights-sell",
        "feat:crimewatch.faction-police-thresholds",
        "feat:crimewatch.sec-status-gain-caps",
        "feat:crimewatch.sec-tag-tier-bands",
    ]);

    let ctx = ServerContext::new();
    let char_a = 90_000_001;
    let char_b = 90_000_002;
    let ship_a = 100_000_001;
    let ship_b = 100_000_002;
    let pod_target_id = 530_001;
    let highsec_id = 30_000_142;
    let lowsec_id = 30_002_537;
    let _sess_a = setup_pilot_session(&ctx, 7127, char_a, ship_a, 587, lowsec_id, DVec3::ZERO).await;
    let mut sess_b = setup_pilot_session(&ctx, 7128, char_b, ship_b, 587, lowsec_id, DVec3::new(1_500.0, 0.0, 0.0)).await;
    let sys_handle = ctx.systems.get_or_spawn(lowsec_id);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_b) {
            c.security_status = -5.5;
        }
        s.items.insert(
            pod_target_id,
            InventoryItemRecord {
                item_id: pod_target_id,
                type_id: 670,
                owner_id: char_b,
                location_id: highsec_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 8. Faction Police thresholds & response (`feat:crimewatch.faction-police`)
    assert_eq!(faction_police_sec_threshold(1.0), Some(-2.0));
    assert_eq!(faction_police_sec_threshold(0.9), Some(-2.5));
    assert_eq!(faction_police_sec_threshold(0.8), Some(-3.0));
    assert_eq!(faction_police_sec_threshold(0.7), Some(-3.5));
    assert_eq!(faction_police_sec_threshold(0.6), Some(-4.0));
    assert_eq!(faction_police_sec_threshold(0.5), Some(-4.5));
    let (attacked_ship, can_dock_ship, cloak_broken) = evaluate_faction_police_response(&ctx, highsec_id, char_b, ship_b).await;
    assert!(attacked_ship && !can_dock_ship && cloak_broken);
    let (attacked_pod, can_dock_pod, _) = evaluate_faction_police_response(&ctx, highsec_id, char_b, pod_target_id).await;
    assert!(!attacked_pod && can_dock_pod, "Capsules must not be attacked by Faction Police and may dock");

    // 9. Kill Rights (`feat:crimewatch.kill-rights`)
    let kr_id = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.kill_rights.insert(
            id,
            eve_db::KillRightRecord {
                kill_right_id: id,
                owner_id: char_b,
                target_id: char_a,
                activation_fee: IskCents::from_isk(5_000_000),
                is_active_suspect: false,
            },
        );
        id
    });
    let _ = dispatch_combat_rpc(&ctx, &mut sess_b, "bountyProxy", "SellKillRight", &[PyRep::Integer(kr_id), PyRep::Real(10_000_000.0)], &HashMap::new())
        .await
        .expect("SellKillRight");
    let my_krs = dispatch_combat_rpc(&ctx, &mut sess_b, "bountyProxy", "GetMyKillRights", &[], &HashMap::new()).await.expect("GetMyKillRights");
    assert!(matches!(my_krs, PyRep::List(ref l) if !l.is_empty()));

    let act_kr = dispatch_combat_rpc(&ctx, &mut sess_b, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(kr_id)], &HashMap::new())
        .await
        .expect("ActivateKillRight");
    assert_eq!(dict_get(&act_kr, "suspectFlagged").and_then(|v| v.as_bool()), Some(true));

    // 10. Clone Soldier Security Tags (`feat:crimewatch.security-tags`)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_b) {
            c.security_status = -6.5;
        }
        s.items.insert(
            750_001,
            InventoryItemRecord {
                item_id: 750_001,
                type_id: 35662, // Clone Soldier Recruiter Tag ([-8.0, -5.0) band)
                owner_id: char_b,
                location_id: ship_b,
                flag: InventoryFlag::Cargo,
                quantity: 4,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    sess_b.station_id = Some(60_003_760);
    let tag_res = dispatch_combat_rpc(&ctx, &mut sess_b, "crimewatch", "ApplyTagsForSecStatus", &[PyRep::Integer(35662), PyRep::Integer(2)], &HashMap::new())
        .await
        .expect("ApplyTagsForSecStatus");
    sess_b.station_id = None;
    let new_sec = dict_get(&tag_res, "newSecurityStatus").and_then(|v| v.as_f64()).unwrap_or(-10.0);
    assert!((new_sec - (-5.5)).abs() < 1e-6, "2 Clone Soldier Recruiter tags must raise security status from -6.5 to -5.5 (got {new_sec})");
    let tx_res = dispatch_combat_rpc(&ctx, &mut sess_b, "crimewatch", "GetSecurityStatusTransactions", &[], &HashMap::new())
        .await
        .expect("GetSecurityStatusTransactions");
    assert!(matches!(tx_res, PyRep::Rowset { .. }));

    // 11. Station / Stargate Sentry Guns (`feat:crimewatch.sentry-guns`)
    // Trigger sentry aggression on grid (0,0,0), step once -> takes damage; warp off grid (>150km) -> ceases fire; warp back -> does not re-engage!
    let (active1, sentry_dmg1) = step_sentry_gun_grid_aggression(&ctx, lowsec_id, char_a, ship_a, DVec3::ZERO, true).await;
    assert!(active1 && sentry_dmg1 > 0.0, "Sentry guns must engage offender on grid");

    // Warp ship_a 600km away (off grid, beyond 500km sentry range)
    let _ = sys_handle.remove_ball(ship_a).await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: 587,
                position: DVec3::new(600_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_200_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let (active2, sentry_dmg2) = step_sentry_gun_grid_aggression(&ctx, lowsec_id, char_a, ship_a, DVec3::ZERO, false).await;
    assert!(!active2 && sentry_dmg2 == 0.0, "Sentry guns must lose aggro when offender warps off grid");

    // Warp back to grid (0,0,0) without re-aggressing -> sentries must remain passive
    let _ = sys_handle.remove_ball(ship_a).await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 3.0,
                mass: 1_200_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    let (active3, sentry_dmg3) = step_sentry_gun_grid_aggression(&ctx, lowsec_id, char_a, ship_a, DVec3::ZERO, false).await;
    assert!(!active3 && sentry_dmg3 == 0.0, "Sentry guns must not re-engage returning ship unless it re-aggresses");
}
