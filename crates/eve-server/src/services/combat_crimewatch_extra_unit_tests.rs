//! Extra unit and mutation-killing tests for `combat_crimewatch.rs`.

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::*;
use eve_db::{InventoryFlag, WarRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn defaults_ephemeral_accessors_and_signature_radius_shield_recharge_edges() {
    assert_eq!(SafetyLevel::default(), SafetyLevel::Green);
    let ds = DroneBehaviorSettings::default();
    assert!(ds.aggressive && ds.focus_fire);

    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();

    with_mut_combat_ephemeral(&ctx, |rt| {
        rt.npc_timers.insert(char_id, 123_456);
    });
    assert_eq!(
        with_combat_ephemeral(&ctx, |rt| rt.npc_timers.get(&char_id).copied()),
        Some(123_456)
    );

    // 1. evaluate_effective_signature_radius fallback when ship has no fitting (unregistered char 999_999)
    let fallback_sig = evaluate_effective_signature_radius(&ctx, 999_999, 888_888, sys_id).await;
    assert!((fallback_sig - 35.0).abs() < 1e-6);

    // 2. MWD bloom when base_sig >= 150.0 (no extra * mwd_bloom_mult in fallback) vs < 150.0
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        24_690,
        make_fighter_type(24_690, 27, 6, 50_000.0, vec![(552, 300.0)]),
    );
    sde.types.insert(
        5_973,
        make_fighter_type(5_973, 46, 7, 10.0, vec![(554, 500.0)]),
    );
    ctx.sde = std::sync::Arc::new(sde);

    let bs_id = 710_001_i64;
    let mwd_id = 710_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bs_id,
            make_fighter_item(bs_id, 24_690, char_id, sys_id as i64, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            mwd_id,
            make_fighter_item(mwd_id, 5_973, char_id, bs_id, InventoryFlag::MedSlot0, 1),
        );
        snap.module_active_states.insert(mwd_id);
    });
    let bs_sig = evaluate_effective_signature_radius(&ctx, 999_999, bs_id, sys_id).await;
    assert!((bs_sig - 300.0).abs() < 1e-6);

    // 3. step_passive_shield_recharge edge cases: missing ball, max_shield <= 0, full shield, and None char_id
    assert_eq!(
        step_passive_shield_recharge(&ctx, sys_id, None, 999_888_777, 100.0, 1.0).await,
        None
    );
    assert_eq!(
        passive_shield_recharge_rate_hps(50.0, 0.0, 100.0),
        0.0
    );
    assert_eq!(
        passive_shield_recharge_rate_hps(50.0, 100.0, 0.0),
        0.0
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let zero_shield_id = 710_010_i64;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: zero_shield_id,
                type_id: 24_690,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 0.0,
                max_shield_hp: 0.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        step_passive_shield_recharge(&ctx, sys_id, None, zero_shield_id, 100.0, 1.0).await,
        None
    );

    // Full shield -> delta == 0.0
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 24_690,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 1_000.0,
                max_shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    let (full_hp, full_delta) =
        step_passive_shield_recharge(&ctx, sys_id, Some(char_id), ship_id, 100.0, 2.0)
            .await
            .unwrap();
    assert!((full_hp - 1_000.0).abs() < 1e-6 && full_delta == 0.0);
}

#[test]
fn emit_combat_log_variants_and_legal_war_alliance_and_fw_wars() {
    let (ctx, _session, att_id, ship_id, _sys_id) = make_fighter_ctx_and_session();
    let vic_id = 90_000_077_i64;
    let vic_ship_id = 720_001_i64;
    let now = 1_750_000_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters.get(&att_id).cloned().unwrap();
        vic.character_id = vic_id;
        vic.corporation_id = 2_000_077;
        vic.alliance_id = Some(99_000_002);
        vic.active_ship_id = vic_ship_id;
        vic.security_status = 0.0;
        snap.characters.insert(vic_id, vic);
        if let Some(att) = snap.characters.get_mut(&att_id) {
            att.corporation_id = 1_000_077;
            att.alliance_id = Some(99_000_001);
            att.security_status = 0.0;
        }
    });

    // 1. emit_combat_log_and_damage_state with victim_char_id = None resolves victim via active_ship_id,
    //    and tests both "miss" (0.0 dmg) and "hit" (!is_wrecking)
    emit_combat_log_and_damage_state(
        &ctx,
        att_id,
        None,
        ship_id,
        vic_ship_id,
        2_905,
        0.0,
        false,
        1.0,
        1.0,
        1.0,
    );
    emit_combat_log_and_damage_state(
        &ctx,
        att_id,
        None,
        ship_id,
        vic_ship_id,
        2_905,
        75.0,
        false,
        0.9,
        1.0,
        1.0,
    );

    // 2. Missing character in is_legal_war_or_suspect_target -> false
    ctx.db.with_snapshot(|snap| {
        assert!(!is_legal_war_or_suspect_target(snap, 999_999, vic_id, now));
        assert!(!is_legal_war_or_suspect_target(snap, att_id, 999_999, now));
    });

    // 3. Custom fw_wars entry in snap.fw_wars makes opposing factions legal targets
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&att_id).unwrap().war_faction_id = Some(500_010);
        snap.characters.get_mut(&vic_id).unwrap().war_faction_id = Some(500_001);
        snap.fw_wars.push((500_010, 500_001));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(is_legal_war_or_suspect_target(snap, att_id, vic_id, now));
    });

    // 4. Alliance-level mutual war (aggressor = 99_000_001, defender = 99_000_002) and inactive war guard
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&att_id).unwrap().war_faction_id = None;
        snap.characters.get_mut(&vic_id).unwrap().war_faction_id = None;
        snap.wars.insert(
            77,
            WarRecord {
                war_id: 77,
                aggressor_corp_id: 99_000_001,
                defender_corp_id: 99_000_002,
                mutual: true,
                active: false,
                started_unix: now - 100,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!is_legal_war_or_suspect_target(snap, att_id, vic_id, now));
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.get_mut(&77).unwrap().active = true;
    });
    ctx.db.with_snapshot(|snap| {
        assert!(is_legal_war_or_suspect_target(snap, att_id, vic_id, now));
    });
}

#[tokio::test]
async fn dynamic_bounty_mgr_security_branches_and_bounty_kill_right_filters() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();

    // 1. Unhandled service returns None from handle()
    assert!(handle(&ctx, &mut session, "corpRegistry", "GetCorporation", &[], &kw).await.is_none());

    // 2. dynamicBountyMgr without solar_system_id -> NoSolarSystem
    let saved_sys = session.solar_system_id;
    session.solar_system_id = None;
    assert_eq!(
        dispatch_combat_rpc(
            &ctx,
            &mut session,
            "dynamicBountyMgr",
            "GetOutputForClientSolarSystem",
            &[],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "NoSolarSystem"
    );

    // 3. Wormhole system (>= 31_000_000) -> (1.35, false)
    session.solar_system_id = Some(31_000_005);
    let wh_res = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "dynamicBountyMgr",
        "GetOutputForClientSolarSystem",
        &[],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(
        wh_res,
        PyRep::Tuple(vec![PyRep::Real(1.35), PyRep::Bool(false)])
    );

    // 4. Nullsec system (< 0.0 security) default (1.35, true) and custom dbs_multiplier (1.75, true)
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);
    session.solar_system_id = Some(null_sys);
    let null_default = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "dynamicBountyMgr",
        "GetOutputForClientSolarSystem",
        &[],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(
        null_default,
        PyRep::Tuple(vec![PyRep::Real(1.35), PyRep::Bool(true)])
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().dbs_multiplier = 1.75;
    });
    let null_custom = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "dynamicBountyMgr",
        "GetOutputForClientSolarSystem",
        &[],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(
        null_custom,
        PyRep::Tuple(vec![PyRep::Real(1.75), PyRep::Bool(true)])
    );
    session.solar_system_id = saved_sys;

    // 5. handle_get_bounties_and_kill_rights with empty filter args, expired kill right, and restrictedTo
    let now = ctx.now_unix();
    let vic_id = 90_000_088_i64;
    create_kill_right(&ctx, 801, char_id, vic_id, 100_000.0);
    create_kill_right(&ctx, 802, char_id, vic_id, 200_000.0);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.player_bounty_pool.insert(vic_id, 750_000.0);
        rt.kill_right_expires_unix.insert(801, now - 1); // expired -> filtered out
        rt.kill_right_restricted_to.insert(802, 98_000_001);
    });

    let all_bk = handle_get_bounties_and_kill_rights(&ctx, &session, &[]).unwrap();
    if let PyRep::Tuple(parts) = all_bk {
        let PyRep::List(b_list) = &parts[0] else { panic!("expected list") };
        let PyRep::List(kr_list) = &parts[1] else { panic!("expected list") };
        assert!(!b_list.is_empty());
        assert_eq!(kr_list.len(), 1);
        assert_eq!(
            kr_list[0].dict_get("restrictedTo"),
            Some(&PyRep::Integer(98_000_001))
        );
    } else {
        panic!("expected tuple");
    }
}

#[tokio::test]
async fn claim_bounty_corp_alliance_pools_gm_actions_and_kill_right_preconditions() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let vic_id = 90_000_099_i64;
    let vic_ship_id = 730_001_i64;
    let vic_corp = 98_300_001_i64;
    let vic_all = 99_300_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters.get(&char_id).cloned().unwrap();
        vic.character_id = vic_id;
        vic.corporation_id = vic_corp;
        vic.alliance_id = Some(vic_all);
        vic.active_ship_id = vic_ship_id;
        vic.solar_system_id = sys_id;
        vic.station_id = None;
        vic.structure_id = None;
        snap.characters.insert(vic_id, vic);
        if let Some(att) = snap.characters.get_mut(&char_id) {
            att.alliance_id = Some(99_300_002);
            att.station_id = None;
            att.structure_id = None;
        }
    });

    // 1. Claim bounty draining char (10k) + corp (15k) + alliance (25k) pools = 50k total
    with_mut_combat_runtime(&ctx, |rt| {
        rt.player_bounty_pool.insert(vic_id, 10_000.0);
        rt.player_bounty_pool.insert(vic_corp, 15_000.0);
        rt.player_bounty_pool.insert(vic_all, 25_000.0);
    });
    assert_eq!(claim_player_bounty_on_kill(&ctx, 0, vic_id, 23_915), 0.0);
    let claimed = claim_player_bounty_on_kill(&ctx, char_id, vic_id, 23_915);
    assert!((claimed - 50_000.0).abs() < 1e-2);
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.player_bounty_pool.contains_key(&vic_id));
        assert!(!rt.player_bounty_pool.contains_key(&vic_corp));
        assert!(!rt.player_bounty_pool.contains_key(&vic_all));
        assert_eq!(rt.bounty_hunter_claimed.get(&99_300_002), Some(&50_000.0));
    });

    // 2. GM bounty actions with ROLE_QA: set 0.0, clear specific, reimburse all (target_id = 0), invalid action
    session.role = eve_machonet::service_roles::ROLE_QA;
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "set", vic_id, 200_000.0).unwrap(), 200_000.0);
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "set", vic_id, 0.0).unwrap(), 0.0);
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "set", vic_id, 300_000.0).unwrap(), 300_000.0);
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "clear", vic_id, 0.0).unwrap(), 300_000.0);

    with_mut_combat_runtime(&ctx, |rt| {
        rt.player_bounty_pool.insert(vic_id, 100_000.0);
        rt.bounty_contributions.insert(vic_id, vec![(char_id, 100_000.0)]);
    });
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "reimburse", 0, 0.0).unwrap(), 100_000.0);
    assert_eq!(
        gm_admin_bounty_action(&ctx, &session, "bogus", vic_id, 0.0)
            .unwrap_err()
            .code,
        "InvalidGmBountyAction"
    );

    // 3. validate_kill_right_live_preconditions all error branches
    let now = ctx.now_unix();
    create_kill_right(&ctx, 901, char_id, vic_id, 50_000.0);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.kill_right_expires_unix.insert(901, now - 1);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, 901, char_id, vic_id)
                .unwrap_err()
                .code,
            "KillRightExpired"
        );
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.kill_right_expires_unix.insert(901, now + 3_600);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, 901, 999_888, vic_id)
                .unwrap_err()
                .code,
            "CharacterNotFound"
        );
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, 901, char_id, 999_888)
                .unwrap_err()
                .code,
            "TargetNotFound"
        );
    });

    // Target in different system -> KillRightTargetNotInSystem
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().solar_system_id = sys_id + 1;
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, 901, char_id, vic_id)
                .unwrap_err()
                .code,
            "KillRightTargetNotInSystem"
        );
    });

    // Target in same system but > 8,000 km away -> KillRightTargetNotOnGrid
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().solar_system_id = sys_id;
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, pos) in [
        (ship_id, DVec3::ZERO),
        (vic_ship_id, DVec3::new(9_000_000.0, 0.0, 0.0)),
    ] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: 23_915,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            )
            .await
            .unwrap();
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, 901, char_id, vic_id)
                .unwrap_err()
                .code,
            "KillRightTargetNotOnGrid"
        );
    });
}
