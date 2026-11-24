//! Edge-case behavioral tests for `combat_crimewatch`.

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_fighters::fighter_test_helpers;
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
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .modified_system_securities
            .insert(30_002_187, 0.6);
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
    emit_combat_log_and_damage_state(&ctx, 90_000_001, None, 200_000_001, 200_000_001, 488, 0.0, false, 1.0, 1.0, 1.0, Some(sys_id));
    emit_combat_log_and_damage_state(&ctx, 90_000_001, Some(90_000_002), 200_000_001, 700_002, 488, 250.0, true, 0.7, 1.0, 1.0, Some(sys_id));
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
    rt.rehydrate_from_persisted_epochs(&p, 10, false);
    assert_eq!(rt.synced_db_epoch, 10);
    assert_eq!(rt.synced_ext_hash, h_non_empty);
    assert_eq!(SafetyLevel::from_persisted_i32(p.safety_levels[&90_000_001]), SafetyLevel::Red);
    assert_eq!(SafetyLevel::from_persisted_i32(p.safety_levels[&90_000_002]), SafetyLevel::Yellow);
    assert_eq!(SafetyLevel::from_persisted_i32(p.safety_levels[&90_000_003]), SafetyLevel::Green);
    assert_eq!(rt.to_persisted_preserving(&p).safety_levels, p.safety_levels);
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

    // 2. When synced_db_epoch != 0 and ext_hash matches, only core is synced
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
    p.drone_settings.insert(90_000_001, (false, true));
    rt.rehydrate_from_persisted_epochs(&p, 11, false);
    assert_eq!(rt.synced_db_epoch, 11);
    let ds2 = rt
        .drone_settings
        .get(&90_000_001)
        .expect("updated drone settings present");
    assert!(!ds2.aggressive);
    assert!(ds2.focus_fire);
    assert_eq!(
        rt.container_lifetimes.get(&202).map(|e| e.lifetime_secs),
        Some(999),
        "core-only sync must keep in-memory extended state when ext_hash matches"
    );

    // 3. Passing force_full=true triggers full sync_from_persisted (overwriting transient 202)
    rt.rehydrate_from_persisted_epochs(&p, 12, true);
    assert_eq!(rt.synced_db_epoch, 12);
    assert!(!rt.container_lifetimes.contains_key(&202));

    // 4. Changing extended_runtime_json to empty clears extended state via full sync
    p.extended_runtime_json.clear();
    rt.rehydrate_from_persisted_epochs(&p, 13, false);
    assert!(rt.container_lifetimes.is_empty());
    assert!(!rt.has_extended_runtime_state());
}

#[tokio::test]
async fn emit_combat_log_and_damage_state_blocks_docked_other_system_and_abandoned_drone_victims() {
    let (ctx, _session, att_id, att_ship, sys_a) = make_fighter_ctx_and_session();
    let att_uid = _session.user_id.expect("seeded attacker user_id");
    ctx.db
        .acquire_online_lock(att_uid, 9_087, Some(att_id), true, 1_750_000_000)
        .unwrap();
    let sys_b = 30_000_144_u32;
    let vic_id = 90_000_002_i64;
    let vic_ship = 720_088_i64;
    let stale_ship = 720_089_i64;
    let abandoned_drone = 750_838_839_i64;
    let untracked_drone = 750_846_013_i64;
    let active_drone = 750_846_999_i64;
    let active_fighter_item = 750_847_001_i64;
    let active_fighter_ball = 750_847_002_i64;
    let npc_att_id = 880_000_001_i64;
    let npc_ship_id = 880_000_002_i64;

    let vic_uid = ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters[&vic_id].clone();
        vic.active_ship_id = vic_ship;
        vic.solar_system_id = sys_a;
        vic.station_id = None;
        vic.structure_id = None;
        let uid = vic.user_id;
        snap.characters.insert(vic_id, vic);
        for (iid, tid) in [
            (vic_ship, 587_u32),
            (stale_ship, 587_u32),
            (abandoned_drone, 2454_u32),
            (untracked_drone, 2454_u32),
            (active_drone, 2454_u32),
            (active_fighter_item, 23_057_u32),
        ] {
            snap.items.insert(
                iid,
                make_fighter_item(iid, tid, vic_id, sys_a as i64, InventoryFlag::None, 1),
            );
        }
        uid
    });
    ctx.db
        .acquire_online_lock(vic_uid, 9_088, Some(vic_id), true, 1_750_000_000)
        .unwrap();

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            abandoned_drone,
            DroneSpaceState {
                ball_id: abandoned_drone,
                drone_item_id: abandoned_drone,
                type_id: 2454,
                group_id: 100,
                owner_char_id: 0,
                original_owner_char_id: vic_id,
                parent_ship_id: 0,
                solar_system_id: sys_a,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: true,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 800.0,
                optimal_range: 5_000.0,
                damage_vector: eve_destiny::DamageVector::new(0.0, 20.0, 0.0, 0.0),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
        rt.active_drones_in_space.insert(
            active_drone,
            DroneSpaceState {
                owner_char_id: vic_id,
                parent_ship_id: vic_ship,
                is_abandoned: false,
                ball_id: active_drone,
                drone_item_id: active_drone,
                ..rt.active_drones_in_space[&abandoned_drone].clone()
            },
        );
    });
    fighter_test_helpers::insert_active_fighter(
        &ctx,
        active_fighter_ball,
        crate::router::context::ActiveFighterSquadron {
            ball_id: active_fighter_ball,
            item_id: active_fighter_item,
            type_id: 23_057,
            owner_char_id: vic_id,
            parent_ship_id: vic_ship,
            tube_index: 0,
            squadron_size: 5,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_a);
    for (bid, tid) in [
        (vic_ship, 587_u32),
        (active_drone, 2454_u32),
        (active_fighter_ball, 23_057_u32),
    ] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: tid,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(tid),
        );
    }

    let drain_vic_wire_combat_events = || {
        let mut sess = SessionState::new(9_088);
        sess.user_id = Some(vic_uid);
        sess.character_id = Some(vic_id);
        ctx.sync_session(&mut sess);
        let mut wire = crate::router::client_wire::ClientWireSession::from_session(&ctx, sess);
        let (frames, _) = wire.drain_pending_frames();
        let events = frames
            .into_iter()
            .filter_map(|f| match f {
                crate::outbound_sequencer::OutFrame::Notification { event, .. }
                | crate::outbound_sequencer::OutFrame::ServiceCall { method: event, .. }
                    if matches!(
                        event.as_str(),
                        "OnDamageMessage" | "OnDamageMessages" | "OnCombatMessage" | "OnDamageStateChange"
                    ) =>
                {
                    Some(event)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        (events, wire.wire_state.scene_drop_count())
    };
    let raw_victim_combat_events = || {
        ctx.drain_notifications_for_char(vic_id)
            .into_iter()
            .filter(|n| {
                matches!(
                    n.event.as_str(),
                    "OnDamageMessage" | "OnDamageMessages" | "OnCombatMessage" | "OnDamageStateChange"
                )
            })
            .map(|n| n.event)
            .collect::<Vec<_>>()
    };

    // 1. Positive control: undocked in sys_a hitting active ship, active drone, and active fighter delivers victim events
    let _ = ctx.drain_notifications_for_char(vic_id);
    emit_combat_log_and_damage_state(&ctx, att_id, None, att_ship, vic_ship, 2_905, 50.0, false, 0.9, 1.0, 1.0, Some(sys_a));
    let (ev_ship, _) = drain_vic_wire_combat_events();
    assert!(ev_ship.contains(&"OnDamageMessage".to_string()));
    assert!(ev_ship.contains(&"OnDamageMessages".to_string()));

    emit_combat_log_and_damage_state(&ctx, att_id, None, att_ship, active_drone, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    let (ev_drone, _) = drain_vic_wire_combat_events();
    assert!(ev_drone.contains(&"OnDamageMessage".to_string()));
    assert!(ev_drone.contains(&"OnDamageMessages".to_string()));

    emit_combat_log_and_damage_state(&ctx, npc_att_id, None, npc_ship_id, active_fighter_ball, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    let raw_fighter_notifs = ctx.drain_notifications_for_char(vic_id);
    assert!(raw_fighter_notifs.iter().any(|n| {
        n.event == "OnDamageStateChange"
            && n.scene_scope
                .is_some_and(|sc| sc.requires_in_space && sc.scene_sys_id == Some(sys_a))
    }));
    emit_combat_log_and_damage_state(&ctx, npc_att_id, None, npc_ship_id, active_fighter_ball, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    let (ev_fighter, _) = drain_vic_wire_combat_events();
    assert!(ev_fighter.contains(&"OnDamageMessage".to_string()));
    assert!(ev_fighter.contains(&"OnDamageMessages".to_string()));

    // 2. Abandoned drone, untracked drone, and stale unpiloted ship item -> zero notifications even with Some(vic_id)
    for target in [abandoned_drone, untracked_drone, stale_ship] {
        for explicit in [None, Some(vic_id)] {
            emit_combat_log_and_damage_state(&ctx, att_id, explicit, att_ship, target, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
            assert!(
                raw_victim_combat_events().is_empty(),
                "must suppress victim notifications for target={target} explicit={explicit:?}"
            );
        }
    }

    // 3. Docked in station or structure at emit time -> zero pending_notifications enqueued (#1565);
    //    enqueued while undocked in sys_a then docked before drain -> dropped on wire by OutboundSceneScope (#1190).
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().station_id = Some(60_003_760);
    });
    for explicit in [None, Some(vic_id)] {
        emit_combat_log_and_damage_state(&ctx, att_id, explicit, att_ship, vic_ship, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
        assert!(
            raw_victim_combat_events().is_empty(),
            "already-docked victim must not enqueue space combat notifications (explicit={explicit:?})"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        let v = snap.characters.get_mut(&vic_id).unwrap();
        v.station_id = None;
        v.structure_id = Some(1_000_000_000_123);
    });
    emit_combat_log_and_damage_state(&ctx, att_id, Some(vic_id), att_ship, vic_ship, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    assert!(
        raw_victim_combat_events().is_empty(),
        "already-structure-docked victim must not enqueue space combat notifications"
    );

    // Post-enqueue dock transition: queued while undocked in sys_a, then docks before wire drain -> dropped on wire.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().structure_id = None;
    });
    emit_combat_log_and_damage_state(&ctx, att_id, Some(vic_id), att_ship, vic_ship, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().station_id = Some(60_003_760);
    });
    let (ev_post_dock, drops_post_dock) = drain_vic_wire_combat_events();
    assert!(
        ev_post_dock.is_empty() && drops_post_dock > 0,
        "post-enqueue dock transition must drop scene-scoped notifications on wire"
    );

    // 4. Undocked in different solar system (`sys_b` vs combat in `sys_a`):
    //    Already in `sys_b` -> zero pending_notifications enqueued AND attacker scope never falls back to `sys_b`
    //    even for NPC-attacked active fighter (`npc_att_id`).
    ctx.db.with_mut_snapshot(|snap| {
        let v = snap.characters.get_mut(&vic_id).unwrap();
        v.station_id = None;
        v.structure_id = None;
        v.solar_system_id = sys_b;
        snap.items.get_mut(&vic_ship).unwrap().location_id = sys_b as i64;
    });
    for target in [vic_ship, active_drone] {
        for explicit in [None, Some(vic_id)] {
            emit_combat_log_and_damage_state(&ctx, att_id, explicit, att_ship, target, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
            assert!(
                raw_victim_combat_events().is_empty(),
                "out-of-system victim must not enqueue combat logs for target={target} (explicit={explicit:?})"
            );
        }
    }
    for explicit in [None, Some(vic_id)] {
        let _ = ctx.drain_notifications_for_char(npc_att_id);
        emit_combat_log_and_damage_state(&ctx, npc_att_id, explicit, npc_ship_id, active_fighter_ball, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
        assert!(
            raw_victim_combat_events().is_empty(),
            "moved-victim NPC/fighter stale sys_a hit must not enqueue victim notifications (explicit={explicit:?})"
        );
        let _ = ctx.drain_notifications_for_char(att_id);
        for combat_sys in [Some(sys_a), None] {
            emit_combat_log_and_damage_state(&ctx, att_id, explicit, npc_ship_id, active_fighter_ball, 2_905, 25.0, false, 0.8, 1.0, 1.0, combat_sys);
            assert!(
                raw_victim_combat_events().is_empty(),
                "moved-victim stale sys_a hit must not enqueue victim notifications (combat_sys={combat_sys:?}, explicit={explicit:?})"
            );
            let atk_notifs = ctx.drain_notifications_for_char(att_id);
            assert!(
                !atk_notifs.is_empty()
                    && atk_notifs.iter().all(|n| n.scene_scope.is_some_and(|sc| sc.scene_sys_id == Some(sys_a))),
                "attacker_scope with absent attacker ship item must remain sys_a and never fall back to victim's sys_b (combat_sys={combat_sys:?}): {atk_notifs:?}"
            );
        }
    }

    // Post-enqueue jump to `sys_b`: queued while in `sys_a`, jumped to `sys_b` before drain -> dropped on wire.
    ctx.db.with_mut_snapshot(|snap| {
        let v = snap.characters.get_mut(&vic_id).unwrap();
        v.solar_system_id = sys_a;
        snap.items.get_mut(&vic_ship).unwrap().location_id = sys_a as i64;
    });
    emit_combat_log_and_damage_state(&ctx, att_id, Some(vic_id), att_ship, vic_ship, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    ctx.db.with_mut_snapshot(|snap| {
        let v = snap.characters.get_mut(&vic_id).unwrap();
        v.solar_system_id = sys_b;
        snap.items.get_mut(&vic_ship).unwrap().location_id = sys_b as i64;
    });
    let (ev_post_jump, drops_post_jump) = drain_vic_wire_combat_events();
    assert!(
        ev_post_jump.is_empty() && drops_post_jump > 0,
        "post-enqueue jump to sys_b must drop queued sys_a combat notifications on wire"
    );

    // 5. Same system (`sys_a`) stale `scene_epoch` transition drops queued epoch-N combat notifications on wire (#1190)
    ctx.db.with_mut_snapshot(|snap| {
        let v = snap.characters.get_mut(&vic_id).unwrap();
        v.solar_system_id = sys_a;
        v.station_id = None;
        v.structure_id = None;
        snap.items.get_mut(&vic_ship).unwrap().location_id = sys_a as i64;
    });
    emit_combat_log_and_damage_state(&ctx, att_id, Some(vic_id), att_ship, vic_ship, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    let _ = ctx.record_atomic_session_transition(
        crate::router::binding_transition_policy::AtomicSceneTransitionRecord {
            session_id: 9_088,
            user_id: Some(vic_uid),
            char_id: vic_id,
            kind_label: "scene_transition",
            reason: "scene_epoch_bump",
            is_logout: false,
            is_login: false,
            prev_sys: Some(sys_a),
            prev_dock: None,
            next_sys: Some(sys_a),
            next_dock: None,
            can_record_departed: true,
            removed_bindings: Vec::new(),
        },
    );
    let (ev_stale_ep, drops_stale_ep) = drain_vic_wire_combat_events();
    assert!(
        ev_stale_ep.is_empty() && drops_stale_ep > 0,
        "stale scene_epoch combat notifications must drop after atomic session transition"
    );
}

#[tokio::test]
async fn emit_combat_log_and_damage_state_allows_active_structure_gunner_and_blocks_structure_docked() {
    let (ctx, _session, att_id, att_ship, sys_a) = make_fighter_ctx_and_session();
    let sys_b = 30_000_144_u32;
    let vic_id = 90_000_002_i64;
    let vic_ship = 720_090_i64;
    let sid = 1_000_000_000_123_i64;
    let standup_fighter_item = 750_847_101_i64;
    let standup_fighter_ball = 750_847_102_i64;
    let npc_att_id = 880_000_001_i64;
    let npc_ship_id = 880_000_002_i64;

    let vic_uid = ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters[&vic_id].clone();
        vic.active_ship_id = vic_ship;
        vic.solar_system_id = sys_a;
        vic.station_id = None;
        vic.structure_id = Some(sid);
        let uid = vic.user_id;
        snap.characters.insert(vic_id, vic);
        snap.structures.insert(
            sid,
            eve_db::UpwellStructureRecord {
                structure_id: sid,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_a,
                name: "Gunner Citadel".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            vic_ship,
            make_fighter_item(vic_ship, 587, vic_id, sid, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            standup_fighter_item,
            make_fighter_item(standup_fighter_item, 40_556, vic_id, sys_a as i64, InventoryFlag::None, 1),
        );
        uid
    });
    ctx.db
        .acquire_online_lock(vic_uid, 9_089, Some(vic_id), true, 1_750_000_000)
        .unwrap();
    fighter_test_helpers::insert_active_fighter(
        &ctx,
        standup_fighter_ball,
        crate::router::context::ActiveFighterSquadron {
            ball_id: standup_fighter_ball,
            item_id: standup_fighter_item,
            type_id: 40_556,
            owner_char_id: vic_id,
            parent_ship_id: sid,
            tube_index: 0,
            squadron_size: 3,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    let sys_handle = ctx.systems.get_or_spawn(sys_a);
    for (bid, tid) in [
        (sid, 35_832_u32),
        (vic_ship, 587_u32),
        (standup_fighter_ball, 40_556_u32),
    ] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: tid,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(tid),
        );
    }

    let drain_vic_wire_combat_events = || {
        let mut sess = SessionState::new(9_089);
        sess.user_id = Some(vic_uid);
        sess.character_id = Some(vic_id);
        ctx.sync_session(&mut sess);
        let mut wire = crate::router::client_wire::ClientWireSession::from_session(&ctx, sess);
        let (frames, _) = wire.drain_pending_frames();
        let events = frames
            .into_iter()
            .filter_map(|f| match f {
                crate::outbound_sequencer::OutFrame::Notification { event, .. }
                | crate::outbound_sequencer::OutFrame::ServiceCall { method: event, .. }
                    if matches!(
                        event.as_str(),
                        "OnDamageMessage" | "OnDamageMessages" | "OnCombatMessage" | "OnDamageStateChange"
                    ) =>
                {
                    Some(event)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        (events, wire.wire_state.scene_drop_count())
    };

    // 1. Normally docked in structure (`structure_id == Some(sid)`, `active_ship_id != sid`) -> zero enqueued
    let _ = ctx.drain_notifications_for_char(vic_id);
    for (target, explicit) in [
        (sid, Some(vic_id)),
        (vic_ship, None),
        (vic_ship, Some(vic_id)),
        (standup_fighter_ball, Some(vic_id)),
    ] {
        emit_combat_log_and_damage_state(&ctx, att_id, explicit, att_ship, target, 2_905, 25.0, false, 0.9, 1.0, 1.0, Some(sys_a));
        assert!(
            ctx.drain_notifications_for_char(vic_id).is_empty(),
            "docked pilot must get zero pending notifications for target={target}"
        );
    }

    // 2. Active Upwell structure gunner (`structure_id == Some(sid)`, `active_ship_id == sid`):
    //    - Even before View Outside ballpark is bootstrapped, scene-scoped combat logs still deliver on wire.
    //    - With View Outside ballpark attached, wire combat notifications continue to deliver cleanly.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().active_ship_id = sid;
    });
    emit_combat_log_and_damage_state(&ctx, att_id, None, att_ship, sid, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    let (ev_no_bp, _) = drain_vic_wire_combat_events();
    assert!(
        ev_no_bp.contains(&"OnDamageMessage".to_string()),
        "active structure gunner without View Outside must still receive OnDamageMessage: {ev_no_bp:?}"
    );
    assert!(
        ev_no_bp.contains(&"OnDamageMessages".to_string()),
        "active structure gunner without View Outside must still receive OnDamageMessages: {ev_no_bp:?}"
    );

    ctx.mark_space_ballpark_bootstrapped(9_089, vic_id, sys_a);
    for (aid, aship, target, label) in [
        (att_id, att_ship, sid, "player attacking manned structure"),
        (npc_att_id, npc_ship_id, sid, "NPC attacking manned structure"),
        (npc_att_id, npc_ship_id, standup_fighter_ball, "NPC attacking Standup fighters"),
    ] {
        emit_combat_log_and_damage_state(&ctx, aid, None, aship, target, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
        let (ev, _) = drain_vic_wire_combat_events();
        assert!(ev.contains(&"OnDamageMessage".to_string()), "{label}: missing OnDamageMessage");
        assert!(ev.contains(&"OnDamageMessages".to_string()), "{label}: missing OnDamageMessages");
    }

    // 3. Out-of-system guard: already in `sys_b` -> zero enqueued; enqueued in `sys_a` then moved -> dropped on wire.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().solar_system_id = sys_b;
    });
    emit_combat_log_and_damage_state(&ctx, npc_att_id, Some(vic_id), npc_ship_id, sid, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    assert!(
        ctx.drain_notifications_for_char(vic_id).is_empty(),
        "already out-of-system gunner must get zero pending notifications"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().solar_system_id = sys_a;
    });
    emit_combat_log_and_damage_state(&ctx, npc_att_id, Some(vic_id), npc_ship_id, sid, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().solar_system_id = sys_b;
    });
    let (ev_oos, drops_oos) = drain_vic_wire_combat_events();
    assert!(
        ev_oos.is_empty() && drops_oos > 0,
        "post-enqueue out-of-system gunner must drop scene-scoped notifications on wire"
    );
}

#[tokio::test]
async fn emit_combat_log_and_passive_recharge_isolate_drone_fighter_and_docked_watchlist_1565() {
    let (ctx, _session, att_id, att_ship, sys_a) = make_fighter_ctx_and_session();
    let sys_b = 30_000_144_u32;
    let vic_id = 90_000_002_i64;
    let vic_ship = 720_110_i64;
    let watcher_id = 90_000_003_i64;
    let watcher_ship = 720_111_i64;
    let active_drone = 810_110_i64;
    let active_fighter_item = 810_111_i64;
    let active_fighter_ball = 810_112_i64;
    let fleet_id = 950_110_i64;

    let (vic_uid, watcher_uid) = ctx.db.with_mut_snapshot(|snap| {
        let v_uid = {
            let v = snap.characters.get_mut(&vic_id).unwrap();
            v.active_ship_id = vic_ship;
            v.solar_system_id = sys_a;
            v.station_id = None;
            v.structure_id = None;
            v.user_id
        };
        let w_uid = {
            let mut w = snap.characters[&att_id].clone();
            w.character_id = watcher_id;
            w.user_id = 9_903;
            w.active_ship_id = watcher_ship;
            w.solar_system_id = sys_a;
            w.station_id = None;
            w.structure_id = None;
            snap.characters.insert(watcher_id, w);
            let mut acct = snap.accounts.values().next().unwrap().clone();
            acct.user_id = 9_903;
            acct.username = "watcher_1565".into();
            snap.accounts.insert(acct.username.clone(), acct);
            9_903_u64
        };
        snap.items.insert(
            vic_ship,
            make_fighter_item(vic_ship, 587, vic_id, sys_a as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            watcher_ship,
            make_fighter_item(watcher_ship, 587, watcher_id, sys_a as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            active_fighter_item,
            make_fighter_item(
                active_fighter_item,
                40_556,
                vic_id,
                sys_a as i64,
                InventoryFlag::None,
                1,
            ),
        );
        let mut fleet = eve_db::FleetRecord::default();
        fleet.fleet_id = fleet_id;
        fleet.boss_id = vic_id;
        fleet.members.push(vic_id);
        fleet.members.push(watcher_id);
        snap.fleets.insert(fleet_id, fleet);
        (v_uid, w_uid)
    });
    ctx.db
        .acquire_online_lock(vic_uid, 9_190, Some(vic_id), true, 1_750_000_000)
        .unwrap();
    ctx.db
        .acquire_online_lock(watcher_uid, 9_191, Some(watcher_id), true, 1_750_000_000)
        .unwrap();
    ctx.mark_space_ballpark_bootstrapped(9_191, watcher_id, sys_a);

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            active_drone,
            DroneSpaceState {
                ball_id: active_drone,
                drone_item_id: active_drone,
                type_id: 2_454,
                group_id: 100,
                owner_char_id: vic_id,
                original_owner_char_id: vic_id,
                parent_ship_id: vic_ship,
                solar_system_id: sys_a,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 800.0,
                optimal_range: 5_000.0,
                damage_vector: eve_destiny::DamageVector::new(0.0, 20.0, 0.0, 0.0),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });
    fighter_test_helpers::insert_active_fighter(
        &ctx,
        active_fighter_ball,
        crate::router::context::ActiveFighterSquadron {
            ball_id: active_fighter_ball,
            item_id: active_fighter_item,
            type_id: 23_057,
            owner_char_id: vic_id,
            parent_ship_id: vic_ship,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    ctx.fleet_extras
        .lock()
        .unwrap()
        .watchlist
        .entry(watcher_id)
        .or_default()
        .insert(vic_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_a);
    for (bid, tid, cur_sh) in [
        (vic_ship, 587_u32, 400.0),
        (active_drone, 2_454_u32, 400.0),
    ] {
        sys_handle.try_spawn_ball_with_combat(
            make_ball(bid, tid, DVec3::ZERO),
            CombatState {
                shield_hp: cur_sh,
                max_shield_hp: 1_000.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..Default::default()
            },
        );
    }

    // 1. Hitting piloted ship delivers scoped watchlist notifications to watcher
    let _ = ctx.drain_notifications_for_char(watcher_id);
    emit_combat_log_and_damage_state(&ctx, att_id, None, att_ship, vic_ship, 2_905, 50.0, false, 0.7, 1.0, 1.0, Some(sys_a));
    let w_ship_hits = ctx.drain_notifications_for_char(watcher_id);
    assert!(w_ship_hits.iter().any(|n| {
        n.event == "OnDamageStateChange"
            && n.scene_scope.is_some_and(|s| s.requires_in_space && s.scene_sys_id == Some(sys_a))
    }));
    assert!(w_ship_hits.iter().any(|n| {
        n.event == "OnFleetStateChange"
            && n.scene_scope.is_some_and(|s| s.requires_in_space && s.scene_sys_id == Some(sys_a))
    }));

    // 2. Hitting active drone or active fighter must NOT emit fleet watchlist damage to watcher
    emit_combat_log_and_damage_state(&ctx, att_id, None, att_ship, active_drone, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    emit_combat_log_and_damage_state(&ctx, 90_999_001, None, 800_999_001, active_fighter_ball, 2_905, 25.0, false, 0.8, 1.0, 1.0, Some(sys_a));
    assert!(
        ctx.drain_notifications_for_char(watcher_id).is_empty(),
        "drone/fighter hits must never emit fleet watchlist updates"
    );

    // 3. step_passive_shield_recharge emits watchlist only for undocked piloted ship, never drone or docked pilot
    assert!(step_passive_shield_recharge(&ctx, sys_a, Some(vic_id), vic_ship, 100.0, 5.0).await.is_some());
    assert!(ctx
        .drain_notifications_for_char(watcher_id)
        .iter()
        .any(|n| n.event == "OnFleetStateChange"));
    assert!(step_passive_shield_recharge(&ctx, sys_a, Some(vic_id), active_drone, 100.0, 5.0).await.is_some());
    assert!(ctx.drain_notifications_for_char(watcher_id).is_empty());

    let _ = ctx.drain_notifications_for_char(vic_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().station_id = Some(60_003_760);
    });
    assert!(step_passive_shield_recharge(&ctx, sys_a, Some(vic_id), vic_ship, 100.0, 5.0).await.is_some());
    assert!(ctx.drain_notifications_for_char(vic_id).is_empty());
    assert!(ctx.drain_notifications_for_char(watcher_id).is_empty());

    // 4. Docked or out-of-system watcher drops queued watchlist notifications on outbound wire
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().station_id = None;
    });
    emit_combat_log_and_damage_state(&ctx, att_id, None, att_ship, vic_ship, 2_905, 50.0, false, 0.6, 1.0, 1.0, Some(sys_a));
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&watcher_id).unwrap().solar_system_id = sys_b;
    });
    let mut w_sess = SessionState::new(9_191);
    w_sess.user_id = Some(watcher_uid);
    w_sess.character_id = Some(watcher_id);
    ctx.sync_session(&mut w_sess);
    let mut w_wire = crate::router::client_wire::ClientWireSession::from_session(&ctx, w_sess);
    let (w_frames, _) = w_wire.drain_pending_frames();
    assert!(!w_frames.iter().any(|f| matches!(
        f,
        crate::outbound_sequencer::OutFrame::Notification { event, .. }
            if event == "OnFleetStateChange" || event == "OnDamageStateChange"
    )));
    assert!(w_wire.wire_state.scene_drop_count() > 0);
}

#[tokio::test]
async fn emit_combat_log_and_damage_state_drops_wire_for_docked_attacker_1190() {
    let (ctx, _session, att_id, att_ship, sys_a) = make_fighter_ctx_and_session();
    let vic_id = 90_000_002_i64;
    let vic_ship = 720_220_i64;
    let (att_uid, vic_uid) = ctx.db.with_mut_snapshot(|snap| {
        let a = snap.characters.get_mut(&att_id).unwrap();
        a.station_id = Some(60_003_760);
        let a_uid = a.user_id;
        let v = snap.characters.get_mut(&vic_id).unwrap();
        v.active_ship_id = vic_ship;
        v.solar_system_id = sys_a;
        v.station_id = None;
        v.structure_id = None;
        let v_uid = v.user_id;
        (a_uid, v_uid)
    });
    ctx.db
        .acquire_online_lock(att_uid, 9_289, Some(att_id), true, ctx.now_unix())
        .unwrap();
    ctx.db
        .acquire_online_lock(vic_uid, 9_290, Some(vic_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(att_id);
    let _ = ctx.drain_notifications_for_char(vic_id);

    emit_combat_log_and_damage_state(
        &ctx,
        att_id,
        Some(vic_id),
        att_ship,
        vic_ship,
        2_905,
        25.0,
        false,
        0.75,
        1.0,
        1.0,
        Some(sys_a),
    );

    let mut att_sess = SessionState::new(9_289);
    att_sess.user_id = Some(att_uid);
    att_sess.character_id = Some(att_id);
    ctx.sync_session(&mut att_sess);
    let mut att_wire = crate::router::client_wire::ClientWireSession::from_session(&ctx, att_sess);
    let (att_frames, _) = att_wire.drain_pending_frames();
    assert!(
        !att_frames.iter().any(|f| matches!(
            f,
            crate::outbound_sequencer::OutFrame::Notification { event, .. }
                if matches!(
                    event.as_str(),
                    "OnDamageMessage" | "OnDamageMessages" | "OnDamageStateChange" | "OnCombatMessage"
                )
        )),
        "docked attacker wire must drop scene-scoped combat events: {att_frames:?}"
    );
    assert!(att_wire.wire_state.scene_drop_count() >= 1);

    let vic_notifs = ctx.drain_notifications_for_char(vic_id);
    assert!(
        vic_notifs.iter().any(|n| n.event == "OnDamageMessage"),
        "in-space victim must receive OnDamageMessage: {vic_notifs:?}"
    );
    assert!(
        vic_notifs.iter().any(|n| n.event == "OnDamageMessages"),
        "in-space victim must receive OnDamageMessages: {vic_notifs:?}"
    );
}

#[test]
fn combat_runtime_flush_and_preserving_do_not_clobber_db_owned_sys_security_or_ess_ledger() {
    let (ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let other_sys = sys_id + 1;

    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .modified_system_securities
            .insert(sys_id, 0.35);
        snap.combat_crimewatch_state
            .ess_reserve_unlocked_until
            .insert(sys_id, 1_700_009_000);
        snap.combat_crimewatch_state
            .ess_reserve_bank_thefts
            .insert(sys_id, vec![(char_id, 1_250_000.0, 1_700_001_000)]);
        snap.combat_crimewatch_state
            .ess_main_bank_thefts
            .insert(other_sys, vec![(char_id + 1, 750_000.0, 1_700_002_000)]);
    });

    // Runtime mutations via `with_mut_combat_runtime`, `with_mut_combat_runtime_in_snap`,
    // and `flush_combat_runtime_to_db` must preserve all 4 DB-owned B2 fields and keep them out of `extended_runtime_json`.
    with_mut_combat_runtime(&ctx, |rt| {
        rt.clone_soldier_cooldown_until.insert(sys_id, 1_700_003_000);
    });
    ctx.db.with_mut_snapshot(|snap| {
        with_mut_combat_runtime_in_snap(snap, |rt| {
            rt.clone_soldier_cooldown_until
                .insert(other_sys, 1_700_004_000);
        });
    });
    flush_combat_runtime_to_db(&ctx);

    ctx.db.with_snapshot(|snap| {
        let cw = &snap.combat_crimewatch_state;
        assert_eq!(cw.modified_system_securities.get(&sys_id), Some(&0.35));
        assert_eq!(
            cw.ess_reserve_unlocked_until.get(&sys_id),
            Some(&1_700_009_000)
        );
        assert_eq!(
            cw.ess_reserve_bank_thefts.get(&sys_id),
            Some(&vec![(char_id, 1_250_000.0, 1_700_001_000)])
        );
        assert_eq!(
            cw.ess_main_bank_thefts.get(&other_sys),
            Some(&vec![(char_id + 1, 750_000.0, 1_700_002_000)])
        );
        assert!(!cw.extended_runtime_json.contains("modified_system_securities"));
        assert!(!cw.extended_runtime_json.contains("ess_reserve_unlocked_until"));
        assert!(!cw.extended_runtime_json.contains("ess_reserve_bank_thefts"));
        assert!(!cw.extended_runtime_json.contains("ess_main_bank_thefts"));
    });
}
