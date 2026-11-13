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

#[tokio::test]
async fn sig_radius_150m_threshold_shield_recharge_delta_and_engagement_timers() {
    let mut sde = (*crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session().0.sde).clone();
    let ship_149_tid = 99_149u32;
    let ship_150_tid = 99_150u32;
    let mwd_tid = 99_046u32;

    let mut t149 = make_fighter_type(ship_149_tid, 25, 6, 10_000.0, vec![(eve_sde::attr_ids::SIGNATURE_RADIUS, 149.0)]);
    t149.name = "Sig149".into();
    let mut t150 = make_fighter_type(ship_150_tid, 25, 6, 10_000.0, vec![(eve_sde::attr_ids::SIGNATURE_RADIUS, 150.0)]);
    t150.name = "Sig150".into();
    let mut tmwd = make_fighter_type(mwd_tid, 46, 7, 50.0, vec![(554, 400.0)]);
    tmwd.name = "CustomMWD".into();
    sde.types.insert(ship_149_tid, t149);
    sde.types.insert(ship_150_tid, t150);
    sde.types.insert(mwd_tid, tmwd);

    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = crate::router::ServerContext::with_sde_and_repository(std::sync::Arc::new(sde), db);
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));

    let sys_id = 30_000_142u32;
    let char_id = 9_910_001i64;
    let ship_149_id = 9_920_149i64;
    let ship_150_id = 9_920_150i64;
    let mwd_149_id = 9_921_149i64;
    let mwd_150_id = 9_921_150i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_149_id;
        ch.station_id = None;
        snap.characters.insert(char_id, ch);

        snap.items.insert(ship_149_id, make_fighter_item(ship_149_id, ship_149_tid, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(ship_150_id, make_fighter_item(ship_150_id, ship_150_tid, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(mwd_149_id, make_fighter_item(mwd_149_id, mwd_tid, char_id, ship_149_id, InventoryFlag::MedSlot0, 1));
        snap.items.insert(mwd_150_id, make_fighter_item(mwd_150_id, mwd_tid, char_id, ship_150_id, InventoryFlag::MedSlot0, 1));
        snap.module_active_states.insert(mwd_149_id);
        snap.module_active_states.insert(mwd_150_id);
    });

    // < 150.0m base sig blooms by active MWD (5x = 745.0), whereas >= 150.0m does not bloom (150.0).
    let eff_149 = evaluate_effective_signature_radius(&ctx, char_id, ship_149_id, sys_id).await;
    let eff_150 = evaluate_effective_signature_radius(&ctx, char_id, ship_150_id, sys_id).await;
    assert!((eff_149 - 745.0).abs() < 1e-3, "expected 149*5=745, got {eff_149}");
    assert!((eff_150 - 150.0).abs() < 1e-3, "expected 150 unbloomed, got {eff_150}");

    // Partial shield passive recharge uses exact EVE passive formula delta.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_149_id,
                type_id: ship_149_tid,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 250.0,
                max_shield_hp: 1_000.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let (after_sh, delta_sh) = step_passive_shield_recharge(&ctx, sys_id, Some(char_id), ship_149_id, 100.0, 1.0)
        .await
        .expect("partial shield recharge");
    assert!((delta_sh - 25.0).abs() < 1e-3, "expected delta=25, got {delta_sh}");
    assert!((after_sh - 275.0).abs() < 1e-3, "expected 250 + 25 = 275, got {after_sh}");

    // update_crimewatch_engagement_timers clears limited engagement and emits 3 engagement timeout/ended notifications.
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            snap.accounts.insert(
                format!("cw_user_{uid}"),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: format!("cw_user_{uid}"),
                    password_token: "pw".into(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(uid, 99_910, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);
    let now = ctx.now_unix();
    with_mut_combat_runtime(&ctx, |rt| rt.add_limited_engagement(char_id, 9_910_002, now));
    assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(char_id, 9_910_002, now)));
    update_crimewatch_engagement_timers(&ctx, char_id, 9_910_002);
    assert!(!with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(char_id, 9_910_002, now)));
    let eng_notifs: Vec<_> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| n.event)
        .collect();
    assert!(eng_notifs.contains(&"OnCrimewatchEngagementStartTimeout".to_string()));
    assert!(eng_notifs.contains(&"OnCrimewatchEngagementStopTimeout".to_string()));
    assert!(eng_notifs.contains(&"OnCrimewatchEngagementEnded".to_string()));
}

#[tokio::test]
async fn bounty_proxy_full_rpc_suite_add_search_leaderboards_clear_and_gm_tools() {
    let (ctx, mut session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = std::collections::HashMap::new();
    let target_pilot = 9_930_001i64;
    let corp_id = 98_930_020i64;
    let ally_id = 99_930_030i64;
    let list_len = |rep: &PyRep| -> usize {
        match rep {
            PyRep::List(v) => v.len(),
            other => panic!("expected list, got {other:?}"),
        }
    };

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = eve_db::IskCents(5_000_000_00);
        }
        let mut t = snap.characters[&char_id].clone();
        t.character_id = target_pilot;
        t.user_id = target_pilot as u64;
        t.name = "Wanted Pilot Alpha".into();
        t.corporation_id = corp_id;
        t.alliance_id = Some(ally_id);
        t.solar_system_id = sys_id;
        snap.characters.insert(target_pilot, t);

        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "BountyTargetCorp".into();
        corp.ticker = "BTC".into();
        corp.alliance_id = Some(ally_id);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
        snap.alliances.insert(
            ally_id,
            eve_db::AllianceRecord {
                alliance_id: ally_id,
                name: "BountyTargetAlly".into(),
                short_name: "BTA".into(),
                executor_corp_id: corp_id,
                ..Default::default()
            },
        );
    });

    // AddToBounty validation and error checks.
    for bad_args in [
        vec![PyRep::Integer(0), PyRep::Real(200_000.0)],
        vec![PyRep::Integer(target_pilot), PyRep::Real(f64::NAN)],
        vec![PyRep::Integer(target_pilot), PyRep::Real(99_999.0)],
    ] {
        let err = dispatch_combat_rpc(&ctx, &mut session, "bountyProxy", "AddToBounty", &bad_args, &kw)
            .await
            .unwrap_err();
        assert_eq!(err.code, "BountyAmountTooLow");
    }
    let missing_err = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(888_777_666), PyRep::Real(200_000.0)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(missing_err.code, "TargetNotFound");

    let broke_err = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(target_pilot), PyRep::Real(100_000_000.0)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(broke_err.code, "InsufficientFunds");

    // Place bounties on pilot, corp, and alliance.
    let add_pilot = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(target_pilot), PyRep::Real(200_000.0)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(add_pilot.dict_get("addedISK"), Some(&PyRep::Real(200_000.0)));
    assert_eq!(add_pilot.dict_get("totalBountyISK"), Some(&PyRep::Real(200_000.0)));

    dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(corp_id), PyRep::Real(300_000.0)],
        &kw,
    )
    .await
    .unwrap();
    dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(ally_id), PyRep::Real(400_000.0)],
        &kw,
    )
    .await
    .unwrap();
    let bal_after_place = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_place - 4_100_000.0).abs() < 1e-3);

    // GetBounty, GetBounties, GetMyBounties.
    let single = dispatch_combat_rpc(&ctx, &mut session, "bountyProxy", "GetBounty", &[PyRep::Integer(target_pilot)], &kw)
        .await
        .unwrap();
    assert_eq!(single.dict_get("targetID"), Some(&PyRep::Integer(target_pilot)));
    assert_eq!(single.dict_get("bountyISK"), Some(&PyRep::Real(200_000.0)));

    let multi = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "GetBounties",
        &[PyRep::List(vec![PyRep::Integer(target_pilot), PyRep::Integer(corp_id)])],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(list_len(&multi), 2);

    let mine = dispatch_combat_rpc(&ctx, &mut session, "bountyProxy", "GetMyBounties", &[], &kw)
        .await
        .unwrap();
    assert_eq!(list_len(&mine), 3);

    // SearchBounties / SearchCharBounties / SearchCorpBounties / SearchAllianceBounties + clear_memoized.
    let s_char = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SearchCharBounties",
        &[PyRep::String("wanted pilot".into())],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(list_len(&s_char), 1);
    let s_corp = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SearchCorpBounties",
        &[PyRep::String("bountytargetcorp".into())],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(list_len(&s_corp), 1);
    let s_ally = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SearchAllianceBounties",
        &[PyRep::String("bountytargetally".into())],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(list_len(&s_ally), 1);
    let s_id = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SearchBounties",
        &[PyRep::Integer(target_pilot)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(list_len(&s_id), 1);

    let cleared_memo = dispatch_combat_rpc(&ctx, &mut session, "bountyProxy", "clear_memoized", &[], &kw)
        .await
        .unwrap();
    assert_eq!(cleared_memo, PyRep::None);

    // Seed hunter claims & check all 7 Top leaderboard RPCs.
    with_mut_combat_runtime(&ctx, |rt| {
        rt.bounty_hunter_claimed.insert(char_id, 250_000.0);
        rt.bounty_hunter_claimed.insert(corp_id, 500_000.0);
        rt.bounty_hunter_claimed.insert(ally_id, 750_000.0);
    });
    for (top_m, is_hunters) in [
        ("GetTopPilotBounties", false),
        ("GetTopCorpBounties", false),
        ("GetTopAllianceBounties", false),
        ("GetTopPilotBountyHunters", true),
        ("GetTopCorporationBountyHunters", true),
        ("GetTopAllianceBountyHunters", true),
        ("GetTopBountyHunters", true),
    ] {
        let top_rep = dispatch_combat_rpc(&ctx, &mut session, "bountyProxy", top_m, &[PyRep::Integer(1)], &kw)
            .await
            .unwrap();
        if is_hunters {
            let PyRep::Tuple(parts) = top_rep else {
                panic!("{top_m} should return (rows, ticks) tuple, got {top_rep:?}");
            };
            assert!(list_len(&parts[0]) > 0, "{top_m} should return non-empty hunter list");
            assert!(matches!(parts[1], PyRep::Integer(t) if t > 0));
        } else {
            assert!(list_len(&top_rep) > 0, "{top_m} should return non-empty list");
        }
    }

    // GMClearBountyCache requires GM role; ClearBounty and GMReimburseBounties operate on pools.
    session.role = 0;
    let non_gm_err = dispatch_combat_rpc(&ctx, &mut session, "bountyProxy", "GMClearBountyCache", &[], &kw)
        .await
        .unwrap_err();
    assert_eq!(non_gm_err.code, "CustomError");

    session.role = u64::MAX;
    assert_eq!(
        dispatch_combat_rpc(&ctx, &mut session, "bountyProxy", "GMClearBountyCache", &[], &kw).await.unwrap(),
        PyRep::None
    );

    let clear_rep = dispatch_combat_rpc(&ctx, &mut session, "bountyProxy", "ClearBounty", &[PyRep::Integer(corp_id)], &kw)
        .await
        .unwrap();
    assert_eq!(clear_rep.dict_get("action"), Some(&PyRep::String("clear".into())));
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.player_bounty_pool.get(&corp_id).copied().unwrap_or(0.0)), 0.0);

    let reimb_rep = dispatch_combat_rpc(&ctx, &mut session, "bountyProxy", "GMReimburseBounties", &[PyRep::Integer(ally_id)], &kw)
        .await
        .unwrap();
    assert_eq!(reimb_rep.dict_get("action"), Some(&PyRep::String("reimburse".into())));
    let bal_after_reimb = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_reimb - 4_500_000.0).abs() < 1e-3);
}

#[tokio::test]
async fn bounty_20pct_cap_aggression_mgr_kill_right_docked_and_grid_8000km_boundary() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = std::collections::HashMap::new();
    let vic_id = 9_940_001i64;
    let vic_ship_id = 9_940_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut v = snap.characters[&char_id].clone();
        v.character_id = vic_id;
        v.user_id = vic_id as u64;
        v.corporation_id += 100;
        v.solar_system_id = sys_id;
        v.active_ship_id = vic_ship_id;
        v.station_id = None;
        snap.characters.insert(vic_id, v);
    });

    // Pool of 1,000,000 ISK vs Rifter (587) loss cap leaves a non-zero remainder in pool.
    with_mut_combat_runtime(&ctx, |rt| {
        rt.player_bounty_pool.insert(vic_id, 1_000_000.0);
    });
    let expected_cap = 0.20 * crate::formulas::compute_ship_insurance_reference_value(&ctx.sde, 587).max(250_000.0);
    let claimed = claim_player_bounty_on_kill(&ctx, char_id, vic_id, 587);
    assert!((claimed - expected_cap).abs() < 1e-2);
    let remaining_pool = with_combat_runtime(&ctx, |rt| rt.player_bounty_pool[&vic_id]);
    assert!((remaining_pool - (1_000_000.0 - claimed)).abs() < 1e-2);

    // Create kill right; test aggressionMgr RPCs & live precondition boundaries.
    let kr_id = 910_901i64;
    create_kill_right(&ctx, kr_id, char_id, vic_id, 0.0);
    let crim_ts = dispatch_combat_rpc(&ctx, &mut session, "aggressionMgr", "GetCriminalTimeStamps", &[], &kw) // rpc-lint: allow test fixture
        .await
        .unwrap();
    assert!(matches!(&crim_ts, PyRep::List(v) if v.contains(&PyRep::Integer(kr_id))));
    let loot_exc = dispatch_combat_rpc(&ctx, &mut session, "aggressionMgr", "CheckLootRightExceptions", &[], &kw) // rpc-lint: allow test fixture
        .await
        .unwrap();
    assert!(matches!(&loot_exc, PyRep::List(v) if v.contains(&PyRep::Integer(kr_id))));

    // Caller docked -> KillRightCannotActivateDocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, kr_id, char_id, vic_id).unwrap_err().code,
            "KillRightCannotActivateDocked"
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        snap.characters.get_mut(&vic_id).unwrap().station_id = Some(60_003_760);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, kr_id, char_id, vic_id).unwrap_err().code,
            "KillRightTargetNotInSystem"
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().station_id = None;
    });

    // Exact 8,000,000.0m passes, 8,000,001.0m rejects with KillRightTargetNotOnGrid.
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 23_915,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    sys_handle
        .spawn_ball(Ball {
            id: vic_ship_id,
            type_id: 587,
            position: glam::DVec3::new(8_000_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(validate_kill_right_live_preconditions(&ctx, snap, kr_id, char_id, vic_id).is_ok());
    });

    sys_handle
        .spawn_ball(Ball {
            id: vic_ship_id,
            type_id: 587,
            position: glam::DVec3::new(8_000_001.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_kill_right_live_preconditions(&ctx, snap, kr_id, char_id, vic_id).unwrap_err().code,
            "KillRightTargetNotOnGrid"
        );
    });
}

#[tokio::test]
async fn has_extended_runtime_state_all_32_fields_epoch_sync_and_sig_radius_mutants() {
    use eve_destiny::DamageVector;
    use eve_sde::attr_ids;

    // 1. `has_extended_runtime_state` (via `to_persisted().extended_runtime_json`) for ALL 32 fields individually + empty baseline
    let mut rt = CombatRuntimeState::default();
    assert!(rt.to_persisted().extended_runtime_json.is_empty());

    macro_rules! check_ext_field {
        ($insert:expr, $clear:expr) => {{
            $insert(&mut rt);
            let p = rt.to_persisted();
            assert!(!p.extended_runtime_json.is_empty());
            $clear(&mut rt);
            assert!(rt.to_persisted().extended_runtime_json.is_empty());
        }};
    }

    let dmg = DamageVector::default();
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.entropic_spools.insert(
                (1, 2),
                EntropicSpoolState {
                    target_id: 2,
                    solar_system_id: 30_000_142,
                    attacker_char_id: 1,
                    attacker_ship_id: 10,
                    module_id: 11,
                    module_type_id: 47269,
                    charge_item_id: None,
                    base_damage: dmg,
                    cap_need: 10.0,
                    duration_s: 4.0,
                    cycles: 1,
                    elapsed_in_cycle: 0.0,
                    per_cycle_bonus: 0.1,
                    max_bonus: 1.5,
                    current_multiplier: 1.1,
                },
            );
        },
        |r: &mut CombatRuntimeState| r.entropic_spools.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.in_flight_missiles.insert(
                1,
                InFlightMissileState {
                    missile_ball_id: 1,
                    solar_system_id: 30_000_142,
                    attacker_char_id: 1,
                    attacker_ship_id: 10,
                    launcher_module_id: 11,
                    launcher_type_id: 2404,
                    missile_type_id: 209,
                    target_ball_id: 2,
                    max_velocity: 1000.0,
                    flight_time_s: 5.0,
                    remaining_flight_s: 4.0,
                    hp: 50.0,
                    max_hp: 50.0,
                    base_damage: dmg,
                    explosion_radius: 40.0,
                    explosion_velocity: 100.0,
                    drf: 0.6,
                    is_fof: false,
                    is_breacher_pod: false,
                    breacher_dot_duration_s: 0.0,
                    breacher_flat_cap: 0.0,
                    breacher_pct_cap: 0.0,
                    destroyed: false,
                    expired: false,
                    impacted: false,
                    applied_damage: 0.0,
                },
            );
        },
        |r: &mut CombatRuntimeState| r.in_flight_missiles.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.active_breacher_dots.insert(
                1,
                ActiveBreacherDot {
                    target_ball_id: 1,
                    source_char_id: 2,
                    source_ship_id: 3,
                    weapon_type_id: 4,
                    charge_type_id: 5,
                    flat_cap_dps: 100.0,
                    pct_cap_fraction: 0.01,
                    remaining_s: 10.0,
                    tick_damage: 10.0,
                },
            );
        },
        |r: &mut CombatRuntimeState| r.active_breacher_dots.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.aggression_assist_log.insert(
                1,
                vec![AssistLogEntry {
                    attacker_char_id: 2,
                    corporation_id: 3,
                    alliance_id: None,
                    ship_type_id: 587,
                    weapon_type_id: 482,
                    damage_done: 50.0,
                    is_ewar: false,
                    last_action_unix: 100,
                }],
            );
        },
        |r: &mut CombatRuntimeState| r.aggression_assist_log.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.container_lifetimes.insert(
                1,
                ContainerLifetimeEntry {
                    item_id: 1,
                    solar_system_id: 30_000_142,
                    type_id: 23,
                    owner_id: 2,
                    created_unix: 10,
                    lifetime_secs: 3600,
                    is_abandoned: false,
                },
            );
        },
        |r: &mut CombatRuntimeState| r.container_lifetimes.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.self_destruct_countdowns.insert(
                1,
                SelfDestructCountdown {
                    ship_id: 1,
                    char_id: 2,
                    solar_system_id: 30_000_142,
                    started_unix: 10,
                    detonate_at_unix: 130,
                },
            );
        },
        |r: &mut CombatRuntimeState| r.self_destruct_countdowns.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.modified_system_securities.insert(30_000_142, 0.4);
        },
        |r: &mut CombatRuntimeState| r.modified_system_securities.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.sentry_gun_aggression.insert(
                (30_000_142, 1),
                SentryAggressionState {
                    solar_system_id: 30_000_142,
                    offender_char_id: 1,
                    offender_ship_id: 2,
                    sentry_origin: DVec3::ZERO,
                    active_on_grid: true,
                    total_sentry_damage_dealt: 10.0,
                },
            );
        },
        |r: &mut CombatRuntimeState| r.sentry_gun_aggression.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.crimewatch_timer_states.insert(1, (10, 0, 0, 0, 0));
        },
        |r: &mut CombatRuntimeState| r.crimewatch_timer_states.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.crimewatch_timer_triggered_at.insert(1, [10, 0, 0, 0, 0]);
        },
        |r: &mut CombatRuntimeState| r.crimewatch_timer_triggered_at.clear()
    );
    let drone_st = DroneSpaceState {
        ball_id: 1,
        drone_item_id: 2,
        type_id: 2454,
        group_id: 100,
        owner_char_id: 3,
        original_owner_char_id: 3,
        parent_ship_id: 4,
        solar_system_id: 30_000_142,
        target_id: None,
        activity_state: 0,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry: false,
        bandwidth_used: 5.0,
        max_velocity: 100.0,
        optimal_range: 1000.0,
        damage_vector: dmg,
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 1.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    };
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.active_drones_in_space.insert(1, drone_st.clone());
        },
        |r: &mut CombatRuntimeState| r.active_drones_in_space.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.drone_next_ready_unix.insert(1, 500);
        },
        |r: &mut CombatRuntimeState| r.drone_next_ready_unix.clear()
    );
    let npc_st = ExtendedNpcEntity {
        ball_id: 1,
        type_id: 10_000,
        faction_id: 500_001,
        archetype: "rat".into(),
        solar_system_id: 30_000_142,
        spawn_group_id: 1,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 1_000.0,
        optimal_range: 10_000.0,
        orbit_range: 5_000.0,
        damage_vector: dmg,
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: 1.0,
        web_range: 0.0,
        scram_strength: 0.0,
        scram_range: 0.0,
        neut_amount: 0.0,
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: DVec3::ZERO,
        sleeper_wh_class: 0,
        custom_cargo_loot: None,
    };
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.npc_entities.insert(1, npc_st.clone());
        },
        |r: &mut CombatRuntimeState| r.npc_entities.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.pending_bounty_ticks.insert((30_000_142, 1), 250.0);
        },
        |r: &mut CombatRuntimeState| r.pending_bounty_ticks.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.bounty_tick_next_flush_unix.insert((30_000_142, 1), 1_000);
        },
        |r: &mut CombatRuntimeState| r.bounty_tick_next_flush_unix.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.pending_bounty_tick_started_unix.insert((30_000_142, 1), 900);
        },
        |r: &mut CombatRuntimeState| r.pending_bounty_tick_started_unix.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.encounter_waves.insert(
                1,
                EncounterWaveDirector {
                    encounter_id: 1,
                    solar_system_id: 30_000_142,
                    current_wave: 1,
                    max_waves: 2,
                    trigger_ball_id: 10,
                    npc_type_id: 10_000,
                    active_wave_ball_ids: vec![],
                    completed: false,
                    wave_npc_types: vec![],
                    origin_position: DVec3::ZERO,
                    next_wave_pending: false,
                },
            );
        },
        |r: &mut CombatRuntimeState| r.encounter_waves.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.insurance_contracts.insert(
                1,
                InsuranceContractState {
                    ship_id: 1,
                    ship_type_id: 587,
                    owner_id: 2,
                    level_name: "Platinum".into(),
                    level_fraction: 1.0,
                    cost_isk: 100.0,
                    payout_isk: 500.0,
                    start_unix: 10,
                    end_unix: 1000,
                },
            );
        },
        |r: &mut CombatRuntimeState| r.insurance_contracts.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.fighter_unit_damage_accum.insert(1, 5.0);
        },
        |r: &mut CombatRuntimeState| r.fighter_unit_damage_accum.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.fighter_tube_rearm_until.insert((1, 0), 99);
        },
        |r: &mut CombatRuntimeState| r.fighter_tube_rearm_until.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.behavior_debugging_active.insert(1);
        },
        |r: &mut CombatRuntimeState| r.behavior_debugging_active.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.authorized_shipping_storage.insert((1, 2));
        },
        |r: &mut CombatRuntimeState| r.authorized_shipping_storage.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.active_drone_miners.insert(
                1,
                DroneMiningState {
                    drone_id: 1,
                    asteroid_id: 2,
                    ship_id: 3,
                    owner_char_id: 4,
                    solar_system_id: 30_000_142,
                    yield_units: 10,
                    duration_secs: 60,
                    next_cycle_unix: 100,
                    waste_prob: 0.0,
                    ore_type_id: 1230,
                    destination_flag: InventoryFlag::Cargo,
                },
            );
        },
        |r: &mut CombatRuntimeState| r.active_drone_miners.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.ess_main_bank_thefts.insert(30_000_142, vec![(1, 100.0, 50)]);
        },
        |r: &mut CombatRuntimeState| r.ess_main_bank_thefts.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.ess_reserve_bank_thefts.insert(30_000_142, vec![(1, 100.0, 50)]);
        },
        |r: &mut CombatRuntimeState| r.ess_reserve_bank_thefts.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.ess_reserve_unlocked_until.insert(30_000_142, 500);
        },
        |r: &mut CombatRuntimeState| r.ess_reserve_unlocked_until.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.clone_soldier_cooldown_until.insert(30_000_142, 500);
        },
        |r: &mut CombatRuntimeState| r.clone_soldier_cooldown_until.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.bounty_credited_balls.insert(1);
        },
        |r: &mut CombatRuntimeState| r.bounty_credited_balls.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.destroyed_npc_cache.insert(1, npc_st.clone());
        },
        |r: &mut CombatRuntimeState| r.destroyed_npc_cache.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.ci_sentry_deployed_systems.insert(30_000_142);
        },
        |r: &mut CombatRuntimeState| r.ci_sentry_deployed_systems.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.npc_scrambled_ships.entry(30_000_142).or_default().insert(1);
        },
        |r: &mut CombatRuntimeState| r.npc_scrambled_ships.clear()
    );
    check_ext_field!(
        |r: &mut CombatRuntimeState| {
            r.npc_sec_gain_intervals.insert((1, 2), (100, 0.5));
        },
        |r: &mut CombatRuntimeState| r.npc_sec_gain_intervals.clear()
    );

    // 2. `ext_runtime_hash`, `rehydrate_from_persisted_epochs`, and `sync_from_persisted` bounty tick precedence
    let mut rt_ext = CombatRuntimeState::default();
    rt_ext.pending_bounty_ticks.insert((30_000_142, 11), 777.0);
    rt_ext.bounty_tick_next_flush_unix.insert((30_000_142, 11), 888);
    rt_ext.pending_bounty_tick_started_unix.insert((30_000_142, 11), 999);
    let p_ext = rt_ext.to_persisted();
    let p_empty = CombatRuntimeState::default().to_persisted();
    let h1 = CombatRuntimeState::ext_runtime_hash(&p_ext);
    let h2 = CombatRuntimeState::ext_runtime_hash(&p_empty);
    assert!(h1 != 0 && h1 != 1 && h2 != 1 && h1 != h2);

    // `rehydrate_from_persisted_epochs`: triggers ext sync when synced_db_epoch==0 or ext_hash changes;
    // skips ext sync when both match.
    let mut rt_rehyd = CombatRuntimeState::default();
    rt_rehyd.modified_system_securities.insert(99, 0.1);
    rt_rehyd.synced_db_epoch = 0;
    rt_rehyd.synced_ext_hash = h2;
    rt_rehyd.rehydrate_from_persisted_epochs(&p_empty, 3);
    assert!(rt_rehyd.modified_system_securities.is_empty());

    // When db_epoch > 0 and ext_hash matches -> ext NOT overwritten; when ext_hash differs -> ext synced
    rt_rehyd.modified_system_securities.insert(99, 0.1);
    rt_rehyd.rehydrate_from_persisted_epochs(&p_empty, 4);
    assert!(rt_rehyd.modified_system_securities.contains_key(&99));
    rt_rehyd.rehydrate_from_persisted_epochs(&p_ext, 5);
    assert!(rt_rehyd.modified_system_securities.is_empty());
    assert_eq!(rt_rehyd.pending_bounty_ticks.get(&(30_000_142, 11)), Some(&777.0));

    // sync_from_persisted bounty tick precedence:
    // Case A: p has different core value AND ext is non-empty -> ext wins (`||` not `&&`)
    let mut p_both = p_ext.clone();
    p_both.pending_bounty_ticks.insert((30_000_142, 11), 111.0);
    p_both.bounty_tick_next_flush_unix.insert((30_000_142, 11), 222);
    p_both.pending_bounty_tick_started_unix.insert((30_000_142, 11), 333);
    let mut rt_sync = CombatRuntimeState::default();
    rt_sync.sync_from_persisted(&p_both);
    assert_eq!(rt_sync.pending_bounty_ticks.get(&(30_000_142, 11)), Some(&777.0));
    assert_eq!(rt_sync.bounty_tick_next_flush_unix.get(&(30_000_142, 11)), Some(&888));
    assert_eq!(rt_sync.pending_bounty_tick_started_unix.get(&(30_000_142, 11)), Some(&999));

    // Case B: p non-empty AND ext empty (with another ext field present) -> p preserved (`!` kept)
    let mut rt_other_ext = CombatRuntimeState::default();
    rt_other_ext.modified_system_securities.insert(30_000_142, 0.9);
    let mut p_core_only = rt_other_ext.to_persisted();
    p_core_only.pending_bounty_ticks.insert((30_000_142, 22), 111.0);
    p_core_only.bounty_tick_next_flush_unix.insert((30_000_142, 22), 222);
    p_core_only.pending_bounty_tick_started_unix.insert((30_000_142, 22), 333);
    rt_sync.sync_from_persisted(&p_core_only);
    assert_eq!(rt_sync.pending_bounty_ticks.get(&(30_000_142, 22)), Some(&111.0));
    assert_eq!(rt_sync.bounty_tick_next_flush_unix.get(&(30_000_142, 22)), Some(&222));
    assert_eq!(rt_sync.pending_bounty_tick_started_unix.get(&(30_000_142, 22)), Some(&333));

    // 3. `sync_combat_runtime_from_db`, `flush_combat_runtime_to_db`, `with_mut_combat_runtime`, and `dispatch_combat_rpc`
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    with_mut_combat_runtime(&ctx, |r| {
        r.modified_system_securities.insert(30_000_144, 0.25);
    });
    assert_eq!(
        with_combat_runtime(&ctx, |r| r.modified_system_securities.get(&30_000_144).copied()),
        Some(0.25)
    );
    // Write-locked guards + epoch sync
    ctx.db.with_mut_snapshot(|snap| {
        flush_combat_runtime_to_db(&ctx);
        sync_combat_runtime_from_db(&ctx);
        with_mut_combat_runtime(&ctx, |r| {
            r.modified_system_securities.insert(30_000_145, 0.55);
        });
        snap.combat_crimewatch_state.npc_timers.insert(char_id, 44_444);
    });
    assert_eq!(with_combat_runtime(&ctx, |r| r.npc_timers.get(&char_id).copied()), Some(44_444));
    flush_combat_runtime_to_db(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.combat_crimewatch_state.npc_timers.get(&char_id), Some(&44_444));
    });

    // dispatch_combat_rpc notification forwarding: pre_seq filter (`seq_id > pre_seq`) & `!live_notify_listener`
    let _ = ctx.db.acquire_online_lock(
        session.user_id.unwrap_or(char_id as u64),
        session.session_id,
        Some(char_id),
        true,
        ctx.now_unix(),
    );
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![PyRep::Integer(1)],
    );
    session.live_notify_listener = false;
    let kw = HashMap::new();
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap();
    let pushed_nonlive = session.drain_notifications();
    assert!(!pushed_nonlive.iter().any(|(ev, _)| ev == "OnSessionChanged"));
    assert!(pushed_nonlive.iter().any(|(ev, _)| ev == "OnSafetyLevelChanged"));

    session.live_notify_listener = true;
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(2)],
        &kw,
    )
    .await
    .unwrap();
    assert!(session.drain_notifications().is_empty());

    // QA role passes `gm_admin_bounty_action`
    session.role = eve_machonet::service_roles::ROLE_QA;
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "set", char_id, 42_000.0).unwrap(), 42_000.0);
    assert_eq!(gm_admin_bounty_action(&ctx, &session, "clear", char_id, 0.0).unwrap(), 42_000.0);
    // `claim_player_bounty_on_kill` non-0/1/-1 payout + invalid guard
    assert_eq!(claim_player_bounty_on_kill(&ctx, char_id, char_id, 587), 0.0);

    // 4. `evaluate_effective_signature_radius`: exact 150.0 vs 149.0 MWD bloom boundary, TP projection, zero sig fallback, and cs_mismatch fields
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(998_301, make_fighter_type(998_301, 25, 6, 15_000.0, vec![(attr_ids::SIGNATURE_RADIUS, 149.0)]));
    sde.types.insert(998_302, make_fighter_type(998_302, 25, 6, 15_000.0, vec![(attr_ids::SIGNATURE_RADIUS, 150.0)]));
    sde.types.insert(998_303, make_fighter_type(998_303, 25, 6, 15_000.0, vec![(attr_ids::SIGNATURE_RADIUS, 0.0)]));
    sde.types.insert(998_310, make_fighter_type(998_310, 46, 7, 10.0, vec![(554, 100.0)])); // +100% MWD bloom -> mult=2.0
    sde.types.insert(998_311, make_fighter_type(998_311, 379, 7, 10.0, vec![(554, 50.0), (attr_ids::MAX_RANGE, 30_000.0), (158, 30_000.0)]));
    ctx.sde = std::sync::Arc::new(sde);

    let s149 = 790_301_i64;
    let s150 = 790_302_i64;
    let s0 = 790_303_i64;
    let mwd149 = 790_311_i64;
    let mwd150 = 790_312_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(s149, make_fighter_item(s149, 998_301, char_id, sys_id as i64, InventoryFlag::Hangar, 1));
        snap.items.insert(mwd149, make_fighter_item(mwd149, 998_310, char_id, s149, InventoryFlag::MedSlot0, 1));
        snap.module_active_states.insert(mwd149);
        snap.items.insert(s150, make_fighter_item(s150, 998_302, char_id, sys_id as i64, InventoryFlag::Hangar, 1));
        snap.items.insert(mwd150, make_fighter_item(mwd150, 998_310, char_id, s150, InventoryFlag::MedSlot0, 1));
        snap.module_active_states.insert(mwd150);
        snap.items.insert(s0, make_fighter_item(s0, 998_303, char_id, sys_id as i64, InventoryFlag::Hangar, 1));
    });

    // 149.0 < 150.0 blooms * 2.0 -> 298.0; exact 150.0 does NOT bloom -> 150.0; 0.0 sig falls back to max(35.0) -> 35.0
    let sig_149 = evaluate_effective_signature_radius(&ctx, char_id, s149, sys_id).await;
    assert!((sig_149 - 298.0).abs() < 1e-6, "sig_149={sig_149}");
    let sig_150 = evaluate_effective_signature_radius(&ctx, char_id, s150, sys_id).await;
    assert!((sig_150 - 150.0).abs() < 1e-6, "sig_150={sig_150}");
    let sig_0 = evaluate_effective_signature_radius(&ctx, char_id, s0, sys_id).await;
    assert!((sig_0 - 35.0).abs() < 1e-6, "sig_0={sig_0}");

    // Apply Target Painter (50% bonus -> tp_mult = 1.5) onto s149
    let tp_eff_mod = eve_dogma::DogmaItemState::from_sde(790_399, 998_311, &ctx.sde);
    ctx.projected_ewar.write().unwrap().insert(
        s149,
        vec![eve_dogma::ProjectedEwarEffect {
            source_module_id: 790_399,
            source_module_type_id: 998_311,
            charge_type_id: None,
            distance: 0.0,
            overloaded: false,
            effective_module: tp_eff_mod,
        }],
    );
    let tp_m = ctx.effective_target_painter_multiplier(s149);
    assert!(tp_m > 1.2, "tp_m={tp_m}");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let expected_unbloomed = 149.0;
    let expected_eff = expected_unbloomed * 2.0 * tp_m;
    let spawn_s149 = |radius: f64, cs: CombatState| {
        let sh = sys_handle.clone();
        async move {
            sh.spawn_ball_with_combat(
                Ball {
                    id: s149,
                    type_id: 998_301,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await
            .unwrap();
        }
    };

    // Case 1: within tolerance -> sentinel signature_radius (9999.0) MUST NOT be overwritten
    spawn_s149(
        expected_eff + 1e-4,
        CombatState {
            base_signature_radius: Some(expected_unbloomed + 1e-4),
            tp_sig_multiplier: Some(tp_m),
            external_sig_multiplier: Some(2.0),
            signature_radius: 9_999.0,
            ..Default::default()
        },
    )
    .await;
    let res1 = evaluate_effective_signature_radius(&ctx, char_id, s149, sys_id).await;
    assert!((res1 - expected_eff).abs() < 1e-6);
    assert!((sys_handle.get_combat_state(s149).await.unwrap().unwrap().signature_radius - 9_999.0).abs() < 1e-6);

    // Case 2: ONLY tp_sig_multiplier mismatched -> updates cs
    spawn_s149(
        expected_eff,
        CombatState {
            base_signature_radius: Some(expected_unbloomed),
            tp_sig_multiplier: Some(9.9),
            external_sig_multiplier: Some(2.0),
            signature_radius: 9_999.0,
            ..Default::default()
        },
    )
    .await;
    let _ = evaluate_effective_signature_radius(&ctx, char_id, s149, sys_id).await;
    let cs2 = sys_handle.get_combat_state(s149).await.unwrap().unwrap();
    assert!((cs2.tp_sig_multiplier.unwrap() - tp_m).abs() < 1e-6);
    assert!((cs2.signature_radius - expected_eff).abs() < 1e-6);

    // Case 3: ONLY external_sig_multiplier mismatched -> updates cs
    spawn_s149(
        expected_eff,
        CombatState {
            base_signature_radius: Some(expected_unbloomed),
            tp_sig_multiplier: Some(tp_m),
            external_sig_multiplier: Some(9.9),
            signature_radius: 9_999.0,
            ..Default::default()
        },
    )
    .await;
    let _ = evaluate_effective_signature_radius(&ctx, char_id, s149, sys_id).await;
    let cs3 = sys_handle.get_combat_state(s149).await.unwrap().unwrap();
    assert!((cs3.external_sig_multiplier.unwrap() - 2.0).abs() < 1e-6);

    // Case 4: ONLY base_signature_radius mismatched -> updates cs
    spawn_s149(
        expected_eff,
        CombatState {
            base_signature_radius: Some(expected_unbloomed + 5.0),
            tp_sig_multiplier: Some(tp_m),
            external_sig_multiplier: Some(2.0),
            signature_radius: 9_999.0,
            ..Default::default()
        },
    )
    .await;
    let _ = evaluate_effective_signature_radius(&ctx, char_id, s149, sys_id).await;
    let cs4 = sys_handle.get_combat_state(s149).await.unwrap().unwrap();
    assert!((cs4.base_signature_radius.unwrap() - expected_unbloomed).abs() < 1e-6);

    // Case 5: ONLY ball.radius mismatched -> updates cs
    spawn_s149(
        expected_eff + 5.0,
        CombatState {
            base_signature_radius: Some(expected_unbloomed),
            tp_sig_multiplier: Some(tp_m),
            external_sig_multiplier: Some(2.0),
            signature_radius: 9_999.0,
            ..Default::default()
        },
    )
    .await;
    let _ = evaluate_effective_signature_radius(&ctx, char_id, s149, sys_id).await;
    let cs5 = sys_handle.get_combat_state(s149).await.unwrap().unwrap();
    assert!((cs5.signature_radius - expected_eff).abs() < 1e-6);
    let _ = ship_id;
}

