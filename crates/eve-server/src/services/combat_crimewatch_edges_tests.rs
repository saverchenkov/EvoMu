//! Edge-case behavioral tests for `combat_crimewatch`.

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::{
    InventoryFlag, IskCents, VirtualServerClock, WarRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{SolarSystem, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        sde.solar_systems.entry(30_002_187).or_insert_with(|| SolarSystem {
            solar_system_id: 30_002_187,
            constellation_id: 20_000_320,
            region_id: 10_000_022,
            name: "Tama".into(),
            security_status: 0.3,
            radius: 1e13,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        });
        sde.solar_systems.entry(30_004_759).or_insert_with(|| SolarSystem {
            solar_system_id: 30_004_759,
            constellation_id: 20_000_694,
            region_id: 10_000_060,
            name: "1DQ1-A".into(),
            security_status: -0.5,
            radius: 1e13,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        });
        Arc::new(sde)
    }))
}

fn edge_ctx(base_unix: u64) -> ServerContext {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

fn make_ball(id: i64, type_id: u32, position: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concord_scheduling_modified_sec_idempotence_and_mwd_bloom_cap() {
    let base = 1_750_000_000;
    let ctx = edge_ctx(base);
    let char_id = 90_000_001;
    let ship_id = 200_000_701;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_fighter_item(ship_id, 587, char_id, 30_000_142, InventoryFlag::None, 1),
        );
    });

    // Non-criminal never schedules CONCORD even in 1.0 highsec
    schedule_highsec_criminal_concord_if_needed(&ctx, char_id, ship_id, 30_000_142, base);
    assert!(ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&char_id)
        .and_then(|s| s.pending_concord_arrival)
        .is_none());

    // Flag criminal: Jita (30_000_142, ~0.95) schedules arrival once; repeating in same sys+ship preserves first arrival timestamp
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).criminal_flag_until_unix = base + 900;
    }
    schedule_highsec_criminal_concord_if_needed(&ctx, char_id, ship_id, 30_000_142, base);
    let first_arrival = ctx.crimewatch.read().unwrap().states[&char_id]
        .pending_concord_arrival
        .unwrap();
    schedule_highsec_criminal_concord_if_needed(&ctx, char_id, ship_id, 30_000_142, base + 4);
    assert_eq!(
        ctx.crimewatch.read().unwrap().states[&char_id].pending_concord_arrival,
        Some(first_arrival)
    );

    // Changing ship re-schedules CONCORD
    let ship2 = 200_000_702;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship2,
            make_fighter_item(ship2, 587, char_id, 30_000_142, InventoryFlag::None, 1),
        );
    });
    schedule_highsec_criminal_concord_if_needed(&ctx, char_id, ship2, 30_000_142, base + 4);
    let rescheduled = ctx.crimewatch.read().unwrap().states[&char_id]
        .pending_concord_arrival
        .unwrap();
    assert_eq!(rescheduled.2, ship2);
    assert!(rescheduled.1 >= first_arrival.1);

    // Capsule never schedules CONCORD even when criminal in highsec
    let char2 = 90_000_002;
    let pod_id = 200_000_703;
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char2).criminal_flag_until_unix = base + 900;
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            make_fighter_item(pod_id, 670, char2, 30_000_142, InventoryFlag::None, 1),
        );
    });
    schedule_highsec_criminal_concord_if_needed(&ctx, char2, pod_id, 30_000_142, base);
    assert_eq!(
        ctx.crimewatch.read().unwrap().states[&char2].pending_concord_arrival,
        None
    );

    // Modified system security can bump lowsec -> 0.6 and trigger CONCORD
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(30_002_187, 0.6);
    });
    let char3 = 90_000_003;
    let ship3 = 200_000_704;
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char3).criminal_flag_until_unix = base + 900;
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship3,
            make_fighter_item(ship3, 587, char3, 30_002_187, InventoryFlag::None, 1),
        );
    });
    schedule_highsec_criminal_concord_if_needed(&ctx, char3, ship3, 30_002_187, base);
    assert!(ctx.crimewatch.read().unwrap().states[&char3]
        .pending_concord_arrival
        .is_some());

    // MWD bloom skipped for base_sig >= 150.0, applied for base_sig < 150.0
    let (mut sctx, _sess, ..) = make_fighter_ctx_and_session();
    let mwd_type = make_fighter_type(95_001, 46, 7, 50.0, vec![(554, 500.0)]);
    let large_hull = make_fighter_type(
        95_002,
        27,
        6,
        100_000.0,
        vec![(eve_sde::attr_ids::SIGNATURE_RADIUS, 300.0)],
    );
    let small_hull = make_fighter_type(
        95_003,
        25,
        6,
        20_000.0,
        vec![(eve_sde::attr_ids::SIGNATURE_RADIUS, 40.0)],
    );
    let mut sde_mut = (*sctx.sde).clone();
    sde_mut.types.insert(95_001, mwd_type);
    sde_mut.types.insert(95_002, large_hull);
    sde_mut.types.insert(95_003, small_hull);
    sctx.sde = Arc::new(sde_mut);

    let big_ship = 200_000_801;
    let small_ship = 200_000_802;
    sctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(big_ship, make_fighter_item(big_ship, 95_002, char_id, 30_000_142, InventoryFlag::None, 1));
        snap.items.insert(200_000_803, make_fighter_item(200_000_803, 95_001, char_id, big_ship, InventoryFlag::MedSlot0, 1));
        snap.module_active_states.insert(200_000_803);

        snap.items.insert(small_ship, make_fighter_item(small_ship, 95_003, char_id, 30_000_142, InventoryFlag::None, 1));
        snap.items.insert(200_000_804, make_fighter_item(200_000_804, 95_001, char_id, small_ship, InventoryFlag::MedSlot0, 1));
        snap.module_active_states.insert(200_000_804);
    });
    let big_sig = evaluate_effective_signature_radius(&sctx, char_id, big_ship, 30_000_142).await;
    let small_sig = evaluate_effective_signature_radius(&sctx, char_id, small_ship, 30_000_142).await;
    assert!((big_sig - 300.0).abs() < 1e-6, "base_sig >= 150 must not bloom: {big_sig}");
    assert!((small_sig - 240.0).abs() < 1e-6, "base_sig < 150 must bloom 6x: {small_sig}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shield_recharge_damage_log_qualities_and_war_legality_edges() {
    let ctx = edge_ctx(1_750_000_000);
    let sys_id = 30_000_142;
    let sys = ctx.systems.get_or_spawn(sys_id);

    // Missing ball -> None; max_shield_hp <= 0 -> None
    assert!(step_passive_shield_recharge(&ctx, sys_id, None, 999_999, 100.0, 1.0)
        .await
        .is_none());

    let ball_zero_shield = 700_001;
    let ball_normal = 700_002;
    for (bid, max_sh, cur_sh) in [(ball_zero_shield, 0.0, 0.0), (ball_normal, 1000.0, 999.0)] {
        sys.try_spawn_ball_with_combat(
            make_ball(bid, 587, DVec3::ZERO),
            CombatState {
                shield_hp: cur_sh,
                max_shield_hp: max_sh,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..Default::default()
            },
        );
    }
    assert!(step_passive_shield_recharge(&ctx, sys_id, None, ball_zero_shield, 100.0, 1.0)
        .await
        .is_none());

    // Recharge clamps at max_shield_hp and dt == 0 yields delta == 0 without notification
    let (after0, delta0) = step_passive_shield_recharge(&ctx, sys_id, Some(90_000_001), ball_normal, 100.0, 0.0)
        .await
        .unwrap();
    assert_eq!(after0, 999.0);
    assert_eq!(delta0, 0.0);

    let (after, delta) = step_passive_shield_recharge(&ctx, sys_id, Some(90_000_001), ball_normal, 100.0, 50.0)
        .await
        .unwrap();
    assert_eq!(after, 1000.0);
    assert!((delta - 1.0).abs() < 1e-6);

    // emit_combat_log_and_damage_state: miss vs wrecking + self-hit avoids duplicate notifications
    let att_uid = ctx.db.with_snapshot(|s| s.characters[&90_000_001].user_id);
    ctx.db
        .acquire_online_lock(att_uid, 9_002, Some(90_000_001), true, 1_750_000_000)
        .unwrap();
    ctx.pending_notifications.write().unwrap().clear();
    emit_combat_log_and_damage_state(&ctx, 90_000_001, None, 200_000_001, 200_000_001, 488, 0.0, false, 1.0, 1.0, 1.0);
    emit_combat_log_and_damage_state(&ctx, 90_000_001, Some(90_000_002), 200_000_001, 700_002, 488, 250.0, true, 0.7, 1.0, 1.0);
    let att_notifs = ctx.pending_notifications.read().unwrap()[&90_000_001].clone();
    let qualities: Vec<String> = att_notifs
        .iter()
        .filter(|n| n.event == "OnDamageMessage")
        .filter_map(|n| {
            let d = n.args.first()?;
            let dmg = d.dict_get("damage").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let wreck = d.dict_get("isWrecking").and_then(|v| v.as_bool()).unwrap_or(false);
            assert_eq!(d.dict_get("isBanked"), Some(&PyRep::Bool(false)));
            Some(if dmg <= 0.0 {
                "miss".to_string()
            } else if wreck {
                "wrecking".to_string()
            } else {
                "hit".to_string()
            })
        })
        .collect();
    assert_eq!(qualities, vec!["miss".to_string(), "wrecking".to_string()]);
    assert!(!att_notifs.iter().any(|n| n.event == "AttackedBy"));

    // is_legal_war_or_suspect_target: unknown chars -> false; inactive war -> false; alliance war -> true; same-faction FW -> false
    ctx.db.with_snapshot(|snap| {
        assert!(!is_legal_war_or_suspect_target(snap, 999_991, 999_992, 1_750_000_000));
    });
    ctx.db.with_mut_snapshot(|snap| {
        let c1 = 90_000_001;
        let c2 = 90_000_010;
        let mut ch2 = snap.characters[&c1].clone();
        ch2.character_id = c2;
        ch2.corporation_id = 98_000_110;
        ch2.alliance_id = Some(99_000_110);
        ch2.war_faction_id = Some(500_001);
        snap.characters.insert(c2, ch2);
        snap.characters.get_mut(&c1).unwrap().alliance_id = Some(99_000_100);
        snap.characters.get_mut(&c1).unwrap().war_faction_id = Some(500_001);
        snap.wars.insert(
            7001,
            WarRecord {
                war_id: 7001,
                aggressor_corp_id: 99_000_100,
                defender_corp_id: 99_000_110,
                started_unix: 1,
                active: false,
                mutual: false,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!is_legal_war_or_suspect_target(snap, 90_000_001, 90_000_010, 1_750_000_000));
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.get_mut(&7001).unwrap().active = true;
    });
    ctx.db.with_snapshot(|snap| {
        assert!(is_legal_war_or_suspect_target(snap, 90_000_001, 90_000_010, 1_750_000_000));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounties_kill_rights_gm_tools_and_dbs_output_edges() {
    let base = 1_750_000_000;
    let ctx = edge_ctx(base);
    let char_id = 90_000_001;
    let target_id = 90_000_020;
    let corp_id = 98_000_020;
    let all_id = 99_000_020;

    ctx.db.with_mut_snapshot(|snap| {
        let mut t = snap.characters[&char_id].clone();
        t.character_id = target_id;
        t.corporation_id = corp_id;
        t.alliance_id = Some(all_id);
        t.balance = IskCents::from_isk(1_000);
        snap.characters.insert(target_id, t);
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(1_000);
    });

    // claim_player_bounty_on_kill invalid / self-kill returns 0.0
    assert_eq!(claim_player_bounty_on_kill(&ctx, 0, target_id, 587), 0.0);
    assert_eq!(claim_player_bounty_on_kill(&ctx, target_id, target_id, 587), 0.0);

    // GM tools: non-GM rejected, unknown action rejected, NaN rejected, reimburse pro-rata
    let player_sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        role: service_roles::ROLE_PLAYER,
        ..Default::default()
    };
    let gm_sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        role: service_roles::ROLE_PLAYER | service_roles::ROLE_GML,
        ..Default::default()
    };
    assert_eq!(
        gm_admin_bounty_action(&ctx, &player_sess, "set", target_id, 100.0)
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    assert_eq!(
        gm_admin_bounty_action(&ctx, &gm_sess, "bogus", target_id, 100.0)
            .unwrap_err()
            .code,
        "InvalidGmBountyAction"
    );
    assert_eq!(
        gm_admin_bounty_action(&ctx, &gm_sess, "set", target_id, f64::NAN)
            .unwrap_err()
            .code,
        "InvalidBountyAmount"
    );

    // Pro-rata reimburse when remaining pool is 50% of original contributions
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.player_bounty_pool.insert(target_id, 1_000.0);
        snap.combat_crimewatch_state
            .bounty_contributions
            .insert(target_id, vec![(char_id, 2_000.0)]);
    });
    let refunded = gm_admin_bounty_action(&ctx, &gm_sess, "reimburse", target_id, 0.0).unwrap();
    assert!((refunded - 1_000.0).abs() < 1e-6);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.combat_crimewatch_state.player_bounty_pool.contains_key(&target_id));
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(2_000));
    });

    // Cascade claim across personal -> corp -> alliance pools up to 20% cap & remove drained pools
    let max_claim = 0.20 * compute_ship_insurance_reference_value(&ctx.sde, 587).max(250_000.0);
    let p_part = max_claim * 0.20;
    let c_part = max_claim * 0.40;
    let a_part = max_claim;
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.player_bounty_pool.insert(target_id, p_part);
        snap.combat_crimewatch_state.player_bounty_pool.insert(corp_id, c_part);
        snap.combat_crimewatch_state.player_bounty_pool.insert(all_id, a_part);
    });
    let claimed = claim_player_bounty_on_kill(&ctx, char_id, target_id, 587);
    assert!((claimed - max_claim).abs() < 1e-3, "claimed={claimed}, max_claim={max_claim}");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.combat_crimewatch_state.player_bounty_pool.contains_key(&target_id));
        assert!(!snap.combat_crimewatch_state.player_bounty_pool.contains_key(&corp_id));
        let rem_all = snap.combat_crimewatch_state.player_bounty_pool[&all_id];
        assert!(
            (rem_all - max_claim * 0.60).abs() < 1e-3,
            "rem_all={rem_all}, expected={}",
            max_claim * 0.60
        );
    });

    // Kill right preconditions: docked caller, off-grid (> 8_000_000m), expired
    create_kill_right(&ctx, 777, char_id, target_id, 0.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, 777, char_id, target_id)
                .unwrap_err()
                .code,
            "KillRightCannotActivateDocked"
        );
    });
    let c_ship = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let t_ship = 200_000_777;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.solar_system_id = 30_000_142;
        let tch = snap.characters.get_mut(&target_id).unwrap();
        tch.station_id = None;
        tch.solar_system_id = 30_000_142;
        tch.active_ship_id = t_ship;
    });
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.try_spawn_ball_with_combat(make_ball(c_ship, 587, DVec3::ZERO), CombatState::default());
    sys.try_spawn_ball_with_combat(
        make_ball(t_ship, 587, DVec3::new(9_000_000.0, 0.0, 0.0)),
        CombatState::default(),
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, 777, char_id, target_id)
                .unwrap_err()
                .code,
            "KillRightTargetNotOnGrid"
        );
    });
    ctx.advance_secs(31 * 86_400);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, 777, char_id, target_id)
                .unwrap_err()
                .code,
            "KillRightExpired"
        );
    });

    // dynamicBountyMgr.GetOutputForClientSolarSystem: unauth, no system, highsec vs nullsec
    let kw = HashMap::new();
    let mut unauth = SessionState::default();
    assert!(
        handle(&ctx, &mut unauth, "dynamicBountyMgr", "GetOutputForClientSolarSystem", &[], &kw)
            .await
            .unwrap()
            .is_err()
    );

    let mut no_sys = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: None,
        ..Default::default()
    };
    assert_eq!(
        handle(&ctx, &mut no_sys, "dynamicBountyMgr", "GetOutputForClientSolarSystem", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.remove(&30_000_142);
    });
    let mut hs_sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };
    let hs_out = handle(&ctx, &mut hs_sess, "dynamicBountyMgr", "GetOutputForClientSolarSystem", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hs_out, PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Bool(false)]));

    let mut ns_sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(30_004_759),
        ..Default::default()
    };
    let ns_out = handle(&ctx, &mut ns_sess, "dynamicBountyMgr", "GetOutputForClientSolarSystem", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ns_out, PyRep::Tuple(vec![PyRep::Real(1.35), PyRep::Bool(true)]));

    // GetBountiesAndKillRights: expired kill right excluded, empty bounty_ids returns remaining pool
    let mut bk_sess = hs_sess;
    let bk_res = handle(
        &ctx,
        &mut bk_sess,
        "bountyProxy",
        "GetBountiesAndKillRights",
        &[PyRep::List(vec![]), PyRep::List(vec![])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    match bk_res {
        PyRep::Tuple(parts) => {
            assert_eq!(parts.len(), 2);
            match (&parts[0], &parts[1]) {
                (PyRep::List(bounties), PyRep::List(krs)) => {
                    assert_eq!(bounties.len(), 1);
                    assert!(krs.is_empty(), "expired kill right should be excluded");
                }
                other => panic!("unexpected tuple elements {other:?}"),
            }
        }
        other => panic!("expected 2-tuple, got {other:?}"),
    }
}

#[test]
fn combat_runtime_rehydrate_epochs_hash_safety_levels_and_extended_state() {
    let mut p = eve_db::PersistedCombatCrimewatchState::default();
    let h_empty = CombatRuntimeState::ext_runtime_hash(&p);
    assert_ne!(h_empty, 0);
    p.extended_runtime_json = serde_json::json!({
        "container_lifetimes": {
            "101": ContainerLifetimeEntry {
                item_id: 101,
                solar_system_id: 30_000_142,
                type_id: 23,
                owner_id: 90_000_001,
                created_unix: 100,
                lifetime_secs: 500,
                is_abandoned: false,
            }
        }
    })
    .to_string();
    let h_non_empty = CombatRuntimeState::ext_runtime_hash(&p);
    assert_ne!(h_non_empty, 0);
    assert_ne!(h_empty, h_non_empty);

    p.safety_levels.insert(90_000_001, 0);
    p.safety_levels.insert(90_000_002, 1);
    p.safety_levels.insert(90_000_003, 2);
    p.drone_settings.insert(90_000_001, (true, false));

    let mut rt = CombatRuntimeState::default();
    assert!(!rt.has_extended_runtime_state());
    assert!(rt.to_persisted().extended_runtime_json.is_empty());

    // 1. Initial rehydrate (synced_db_epoch == 0) performs full sync_from_persisted
    rt.rehydrate_from_persisted_epochs(&p, 10, 2);
    assert_eq!(rt.synced_db_epoch, 10);
    assert_eq!(rt.synced_rollback_epoch, 2);
    assert_eq!(rt.synced_ext_hash, h_non_empty);
    assert_eq!(rt.safety_levels.get(&90_000_001), Some(&SafetyLevel::Red));
    assert_eq!(rt.safety_levels.get(&90_000_002), Some(&SafetyLevel::Yellow));
    assert_eq!(rt.safety_levels.get(&90_000_003), Some(&SafetyLevel::Green));
    let ds = rt
        .drone_settings
        .get(&90_000_001)
        .expect("drone settings present");
    assert!(ds.aggressive);
    assert!(!ds.focus_fire);
    assert_eq!(
        rt.container_lifetimes.get(&101).map(|e| e.lifetime_secs),
        Some(500)
    );
    assert!(rt.has_extended_runtime_state());
    assert!(!rt.to_persisted().extended_runtime_json.is_empty());

    // 2. When synced_db_epoch != 0, rb_epoch matches, and ext_hash matches, only core is synced
    rt.container_lifetimes.insert(
        202,
        ContainerLifetimeEntry {
            item_id: 202,
            solar_system_id: 30_000_142,
            type_id: 23,
            owner_id: 90_000_001,
            created_unix: 200,
            lifetime_secs: 999,
            is_abandoned: false,
        },
    );
    p.safety_levels.insert(90_000_001, 2);
    rt.rehydrate_from_persisted_epochs(&p, 11, 2);
    assert_eq!(rt.synced_db_epoch, 11);
    assert_eq!(rt.safety_levels.get(&90_000_001), Some(&SafetyLevel::Green));
    assert_eq!(
        rt.container_lifetimes.get(&202).map(|e| e.lifetime_secs),
        Some(999),
        "core-only sync must keep in-memory extended state when rb_epoch and ext_hash match"
    );

    // 3. Changing rb_epoch triggers full sync_from_persisted (overwriting transient 202)
    rt.rehydrate_from_persisted_epochs(&p, 12, 3);
    assert_eq!(rt.synced_rollback_epoch, 3);
    assert!(!rt.container_lifetimes.contains_key(&202));

    // 4. Changing extended_runtime_json to empty clears extended state via full sync
    p.extended_runtime_json.clear();
    rt.rehydrate_from_persisted_epochs(&p, 13, 3);
    assert!(rt.container_lifetimes.is_empty());
    assert!(!rt.has_extended_runtime_state());
}
