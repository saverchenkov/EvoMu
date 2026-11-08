//! Unit and mutation-killing tests for `combat_crimewatch`.

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::{
    InventoryFlag, IskCents, PersistedCombatCrimewatchState, PersistedWarMeta,
    PersistedWarNegotiation, WarRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn combat_runtime_state_timers_and_persistence_roundtrip() {
    let mut state = CombatRuntimeState::default();
    let now = 1_700_000_000_u64;

    assert!(!state.has_limited_engagement_with(101, 202, now));
    assert!(!state.is_in_any_limited_engagement(101, now));
    state.add_limited_engagement(101, 202, now);
    assert!(state.has_limited_engagement_with(101, 202, now));
    assert!(state.has_limited_engagement_with(101, 202, now + 299));
    assert!(!state.has_limited_engagement_with(101, 202, now + 300));
    assert!(state.is_in_any_limited_engagement(101, now + 299));
    assert!(!state.is_in_any_limited_engagement(101, now + 300));

    assert!(!state.is_npc_timer_active(101, now));
    state.trigger_npc_timer(101, now);
    assert!(state.is_npc_timer_active(101, now + 299));
    assert!(!state.is_npc_timer_active(101, now + 300));

    state.safety_levels.insert(1, SafetyLevel::Red);
    state.safety_levels.insert(2, SafetyLevel::Yellow);
    state.safety_levels.insert(3, SafetyLevel::Green);
    state.drone_settings.insert(
        1,
        DroneBehaviorSettings {
            aggressive: true,
            focus_fire: false,
        },
    );
    state.player_bounty_pool.insert(202, 5_000_000.0);
    state
        .bounty_contributions
        .insert(202, vec![(101, 5_000_000.0)]);
    state.bounty_hunter_claimed.insert(101, 1_250_000.0);
    state.kill_right_expires_unix.insert(77, now + 3600);
    state.kill_right_restricted_to.insert(77, 303);

    let persisted: PersistedCombatCrimewatchState = state.to_persisted();
    assert_eq!(persisted.safety_levels.get(&1), Some(&0));
    assert_eq!(persisted.safety_levels.get(&2), Some(&1));
    assert_eq!(persisted.safety_levels.get(&3), Some(&2));
    assert_eq!(persisted.drone_settings.get(&1), Some(&(true, false)));

    let mut restored = CombatRuntimeState::default();
    restored.sync_from_persisted(&persisted);
    assert_eq!(restored.safety_levels.get(&1), Some(&SafetyLevel::Red));
    assert_eq!(restored.safety_levels.get(&2), Some(&SafetyLevel::Yellow));
    assert_eq!(restored.safety_levels.get(&3), Some(&SafetyLevel::Green));
    let ds = restored.drone_settings.get(&1).expect("drone settings");
    assert!(ds.aggressive);
    assert!(!ds.focus_fire);
    assert_eq!(restored.player_bounty_pool.get(&202), Some(&5_000_000.0));
    assert_eq!(restored.kill_right_restricted_to.get(&77), Some(&303));
}

#[test]
fn probe_scan_signal_strength_and_extract_id_collection_variants() {
    let weak = compute_probe_scan_signal_strength(10.0, 20.0, 8.0);
    let mid = compute_probe_scan_signal_strength(40.0, 40.0, 1.0);
    let strong = compute_probe_scan_signal_strength(80.0, 200.0, 0.25);
    assert!(weak > 0.0 && weak < mid);
    assert!((mid - 0.25).abs() < 1e-6);
    assert!((strong - 1.0).abs() < 1e-6);

    assert!(extract_id_collection(None).is_empty());
    assert_eq!(
        extract_id_collection(Some(&PyRep::List(vec![
            PyRep::Integer(11),
            PyRep::String("skip".into()),
            PyRep::Integer(22),
        ]))),
        vec![11, 22]
    );
    let obj_ex = PyRep::ObjectEx {
        header: Box::new(PyRep::Tuple(vec![
            PyRep::String("builtin.set".into()),
            PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(33), PyRep::Integer(44)])]),
        ])),
        list: vec![PyRep::Integer(55)],
        dict: Vec::new(),
        is_type_2: false,
    };
    assert_eq!(extract_id_collection(Some(&obj_ex)), vec![55, 33, 44]);
}

#[tokio::test]
async fn signature_radius_mwd_bloom_and_passive_shield_recharge_mechanics() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = (*ctx.sde).clone();
    // Ship with base signature radius 100.0 (attr 552)
    sde.types.insert(
        23_915,
        make_fighter_type(23_915, 26, 6, 10_000.0, vec![(552, 100.0)]),
    );
    // MWD module (group 46, attr 554 = 500% signature radius bloom)
    sde.types.insert(
        5_973,
        make_fighter_type(5_973, 46, 7, 10.0, vec![(554, 500.0)]),
    );
    ctx.sde = std::sync::Arc::new(sde);

    let mwd_id = 700_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mwd_id,
            make_fighter_item(mwd_id, 5_973, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 0.0,
                max_shield_hp: 1_000.0,
                armor_hp: 800.0,
                max_armor_hp: 1_000.0,
                structure_hp: 900.0,
                max_structure_hp: 1_000.0,
                signature_radius: 40.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let sig_inactive = evaluate_effective_signature_radius(&ctx, char_id, ship_id, sys_id).await;
    assert!((sig_inactive - 100.0).abs() < 1e-6);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mwd_id);
    });
    let sig_active = evaluate_effective_signature_radius(&ctx, char_id, ship_id, sys_id).await;
    assert!((sig_active - 600.0).abs() < 1e-6);
    let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs.signature_radius - 600.0).abs() < 1e-6);

    let rate_25 = passive_shield_recharge_rate_hps(250.0, 1_000.0, 100.0);
    let rate_90 = passive_shield_recharge_rate_hps(900.0, 1_000.0, 100.0);
    assert!(rate_25 > rate_90 && rate_25 > 0.0);

    let (after_zero, delta_zero) =
        step_passive_shield_recharge(&ctx, sys_id, Some(char_id), ship_id, 100.0, 5.0)
            .await
            .expect("recharge from 0 shield");
    assert!(after_zero > 0.0 && delta_zero > 0.0);
}

#[test]
fn legal_war_or_suspect_target_outlaw_fw_mutual_and_ally_rules() {
    let (ctx, _session, att_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let vic_id = 90_000_002_i64;
    let now = 1_750_000_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters.get(&att_id).cloned().unwrap();
        vic.character_id = vic_id;
        vic.corporation_id = 2_000_001;
        vic.alliance_id = None;
        vic.security_status = -5.0;
        snap.characters.insert(vic_id, vic);
        if let Some(att) = snap.characters.get_mut(&att_id) {
            att.corporation_id = 1_000_001;
            att.alliance_id = None;
            att.security_status = 0.0;
        }
    });

    // 1. Outlaw security status <= -4.95 is legal
    ctx.db.with_snapshot(|snap| {
        assert!(is_legal_war_or_suspect_target(snap, att_id, vic_id, now));
    });

    // 2. Non-outlaw without war or FW is illegal
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&vic_id).unwrap().security_status = 0.0;
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!is_legal_war_or_suspect_target(snap, att_id, vic_id, now));
    });

    // 3. Opposing FW militia is legal
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&att_id).unwrap().war_faction_id = Some(500_001);
        snap.characters.get_mut(&vic_id).unwrap().war_faction_id = Some(500_004);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(is_legal_war_or_suspect_target(snap, att_id, vic_id, now));
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&att_id).unwrap().war_faction_id = None;
        snap.characters.get_mut(&vic_id).unwrap().war_faction_id = None;
    });

    // 4. Non-mutual war during 24h warmup is not legal yet; after 86400s is legal
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            10,
            WarRecord {
                war_id: 10,
                aggressor_corp_id: 1_000_001,
                defender_corp_id: 2_000_001,
                mutual: false,
                active: true,
                started_unix: now,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!is_legal_war_or_suspect_target(
            snap,
            att_id,
            vic_id,
            now + 86_399
        ));
        assert!(is_legal_war_or_suspect_target(
            snap,
            att_id,
            vic_id,
            now + 86_400
        ));
    });

    // 5. War finished in war_state makes it no longer legal; Accepted ally is legal
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.get_mut(&10).unwrap().mutual = true;
        snap.wars.get_mut(&10).unwrap().defender_corp_id = 3_000_001;
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 1,
            war_id: 10,
            kind: eve_db::PersistedNegKind::Ally,
            from_corp: 2_000_001,
            isk: IskCents::ZERO,
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(now + 100),
        });
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!is_legal_war_or_suspect_target(snap, att_id, vic_id, now + 50));
        assert!(is_legal_war_or_suspect_target(snap, att_id, vic_id, now + 100));
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.war_state.wars.insert(
            10,
            PersistedWarMeta {
                finish_at_unix: Some(now + 200),
                ..PersistedWarMeta::default()
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!is_legal_war_or_suspect_target(snap, att_id, vic_id, now + 200));
    });
}

#[tokio::test]
async fn bounties_kill_rights_gm_actions_and_dynamic_bounty_rpc() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let victim_id = 90_000_002_i64;
    let victim_ship_id = 200_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters.get(&char_id).cloned().unwrap();
        vic.character_id = victim_id;
        vic.corporation_id = 2_000_002;
        vic.active_ship_id = victim_ship_id;
        vic.solar_system_id = sys_id;
        vic.station_id = None;
        vic.structure_id = None;
        snap.characters.insert(victim_id, vic);
        if let Some(att) = snap.characters.get_mut(&char_id) {
            att.station_id = None;
            att.structure_id = None;
        }
    });

    // Non-GM rejected from gm_admin_bounty_action
    assert!(gm_admin_bounty_action(&ctx, &session, "set", victim_id, 1_000_000.0).is_err());

    session.role = eve_machonet::service_roles::ROLE_GMH;
    assert!(gm_admin_bounty_action(&ctx, &session, "set", victim_id, -1.0).is_err());
    let set_amt = gm_admin_bounty_action(&ctx, &session, "set", victim_id, 500_000.0).unwrap();
    assert!((set_amt - 500_000.0).abs() < 1e-6);

    // Create kill right and query handle_get_bounties_and_kill_rights
    create_kill_right(&ctx, 501, char_id, victim_id, 250_000.0);
    let bk = handle_get_bounties_and_kill_rights(
        &ctx,
        &session,
        &[
            PyRep::List(vec![PyRep::Integer(victim_id)]),
            PyRep::List(vec![PyRep::Integer(victim_id)]),
        ],
    )
    .unwrap();
    if let PyRep::Tuple(parts) = bk {
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], PyRep::List(v) if v.len() == 1));
        assert!(matches!(&parts[1], PyRep::List(v) if v.len() == 1));
    } else {
        panic!("expected tuple");
    }

    // Claim player bounty on kill (caps at 20% of ship insurance reference value)
    assert_eq!(claim_player_bounty_on_kill(&ctx, char_id, char_id, 23_915), 0.0);
    let claimed = claim_player_bounty_on_kill(&ctx, char_id, victim_id, 23_915);
    assert!((claimed - 100_000.0).abs() < 1e-2);

    // Reimburse remaining 400k across contributor(s) with pro-rata scaling
    with_mut_combat_runtime(&ctx, |rt| {
        rt.bounty_contributions
            .insert(victim_id, vec![(char_id, 500_000.0)]);
    });
    let reimbursed = gm_admin_bounty_action(&ctx, &session, "reimburse", victim_id, 0.0).unwrap();
    assert!((reimbursed - 400_000.0).abs() < 1e-2);
    assert_eq!(
        gm_admin_bounty_action(&ctx, &session, "clear", 0, 0.0).unwrap(),
        0.0
    );

    // Live kill right preconditions: on-grid vs off-grid vs docked
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, pos) in [(ship_id, DVec3::ZERO), (victim_ship_id, DVec3::new(1_000.0, 0.0, 0.0))] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: 23_915,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
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
        assert!(validate_kill_right_live_preconditions(&ctx, snap, 501, char_id, victim_id).is_ok());
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(validate_kill_right_live_preconditions(&ctx, snap, 501, char_id, victim_id).is_err());
    });

    // emit_combat_log_and_damage_state & update_crimewatch_engagement_timers & dynamicBountyMgr RPC
    emit_combat_log_and_damage_state(
        &ctx,
        char_id,
        Some(victim_id),
        ship_id,
        victim_ship_id,
        2_905,
        150.0,
        true,
        0.8,
        1.0,
        1.0,
    );
    update_crimewatch_engagement_timers(&ctx, char_id, victim_id);

    let dbs_res = dispatch_combat_rpc(
        &ctx,
        &mut session,
        "dynamicBountyMgr",
        "GetOutputForClientSolarSystem",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(matches!(dbs_res, PyRep::Tuple(ref t) if t.len() == 2));
}

#[tokio::test]
async fn schedule_highsec_criminal_concord_if_needed_covers_exemptions_and_system_switch() {
    let ctx = ServerContext::new();
    let now_unix = ctx.now_unix();
    let highsec_sys = 30_000_142_u32; // Jita (1.0 -> 6s delay)
    let second_highsec_sys = 30_000_144_u32; // Perimeter
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .expect("lowsec system");

    let char_id = 975_001_i64;
    let frigate_id = 975_002_i64;
    let pod_id = 975_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_id;
        ch.active_ship_id = frigate_id;
        snap.characters.insert(char_id, ch);
        for (iid, tid) in [(frigate_id, 587_u32), (pod_id, 670_u32)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: highsec_sys as i64,
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
        }
    });

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).criminal_flag_until_unix = now_unix + 900;
    }

    // 1. Low-sec does not schedule CONCORD
    schedule_highsec_criminal_concord_if_needed(&ctx, char_id, frigate_id, lowsec_sys, now_unix);
    assert!(ctx.crimewatch.read().unwrap().states[&char_id].pending_concord_arrival.is_none());

    // 2. Capsule (type 670) does not schedule CONCORD even in high-sec
    schedule_highsec_criminal_concord_if_needed(&ctx, char_id, pod_id, highsec_sys, now_unix);
    assert!(ctx.crimewatch.read().unwrap().states[&char_id].pending_concord_arrival.is_none());

    // 3. Frigate in high-sec schedules CONCORD for highsec_sys
    schedule_highsec_criminal_concord_if_needed(&ctx, char_id, frigate_id, highsec_sys, now_unix);
    let sched = ctx.crimewatch.read().unwrap().states[&char_id]
        .pending_concord_arrival
        .expect("CONCORD scheduled in high-sec");
    assert_eq!(sched.0, highsec_sys);
    assert_eq!(sched.2, frigate_id);

    // 4. Transitioning to another high-sec system updates pending_concord_arrival to the new system
    schedule_highsec_criminal_concord_if_needed(&ctx, char_id, frigate_id, second_highsec_sys, now_unix);
    let sched2 = ctx.crimewatch.read().unwrap().states[&char_id]
        .pending_concord_arrival
        .expect("CONCORD rescheduled in second high-sec");
    assert_eq!(sched2.0, second_highsec_sys);
}
