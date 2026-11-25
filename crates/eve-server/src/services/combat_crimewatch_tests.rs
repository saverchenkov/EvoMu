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
    let mut snap = eve_db::WorldStateSnapshot::default();
    let now = 1_700_000_000_u64;

    assert!(!has_limited_engagement_in_snap(&snap, 101, 202, now));
    assert!(!is_in_any_limited_engagement_in_snap(&snap, 101, now));
    add_limited_engagement_in_snap(&mut snap, 101, 202, now);
    assert!(has_limited_engagement_in_snap(&snap, 101, 202, now));
    assert!(has_limited_engagement_in_snap(&snap, 101, 202, now + 299));
    assert!(!has_limited_engagement_in_snap(&snap, 101, 202, now + 300));
    assert!(is_in_any_limited_engagement_in_snap(&snap, 101, now + 299));
    assert!(!is_in_any_limited_engagement_in_snap(&snap, 101, now + 300));

    assert!(!state.is_npc_timer_active(101, now));
    state.trigger_npc_timer(101, now);
    assert!(state.is_npc_timer_active(101, now + 299));
    assert!(!state.is_npc_timer_active(101, now + 300));

    state.drone_settings.insert(
        1,
        DroneBehaviorSettings {
            aggressive: true,
            focus_fire: false,
        },
    );
    let mut current_db = PersistedCombatCrimewatchState::default();
    current_db.safety_levels.insert(1, 0);
    current_db.safety_levels.insert(2, 1);
    current_db.safety_levels.insert(3, 2);
    current_db
        .limited_engagements
        .insert(101, HashMap::from([(202, now + 300)]));
    current_db.duel_challenges.insert((101, 202), now + 60);
    current_db.player_bounty_pool.insert(202, 5_000_000.0);
    current_db
        .bounty_contributions
        .insert(202, vec![(101, 5_000_000.0)]);
    current_db.bounty_hunter_claimed.insert(101, 1_250_000.0);
    current_db.kill_right_expires_unix.insert(77, now + 3600);
    current_db.kill_right_restricted_to.insert(77, 303);

    let persisted: PersistedCombatCrimewatchState = state.to_persisted_preserving(&current_db);
    assert_eq!(persisted.safety_levels.get(&1), Some(&0));
    assert_eq!(persisted.safety_levels.get(&2), Some(&1));
    assert_eq!(persisted.safety_levels.get(&3), Some(&2));
    assert_eq!(SafetyLevel::from_persisted_i32(0), SafetyLevel::Red);
    assert_eq!(SafetyLevel::from_persisted_i32(1), SafetyLevel::Yellow);
    assert_eq!(SafetyLevel::from_persisted_i32(2), SafetyLevel::Green);
    assert_eq!(SafetyLevel::from_persisted_i32(99), SafetyLevel::Green);
    assert_eq!(persisted.drone_settings.get(&1), Some(&(true, false)));
    assert_eq!(
        persisted
            .limited_engagements
            .get(&101)
            .and_then(|m| m.get(&202)),
        Some(&(now + 300))
    );
    assert_eq!(persisted.duel_challenges.get(&(101, 202)), Some(&(now + 60)));
    assert_eq!(persisted.player_bounty_pool.get(&202), Some(&5_000_000.0));
    assert_eq!(persisted.kill_right_restricted_to.get(&77), Some(&303));

    let mut restored = CombatRuntimeState::default();
    restored.sync_from_persisted(&persisted);
    let ds = restored.drone_settings.get(&1).expect("drone settings");
    assert!(ds.aggressive);
    assert!(!ds.focus_fire);
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
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .as_inner_mut()
            .bounty_contributions
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
        Some(sys_id),
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

#[test]
fn legal_war_or_suspect_target_honors_ally_expiry_and_alliance_allies() {
    use eve_db::{AllianceRecord, CorporationRecord, PersistedNegKind};

    let (ctx, _session, att_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_ally_char = 90_000_202_i64;
    let alliance_ally_char = 90_000_203_i64;
    let agg_corp = 98_200_001_i64;
    let def_corp = 98_200_002_i64;
    let corp_ally = 98_200_003_i64;
    let ext_alliance = 99_200_004_i64;
    let ext_member_corp = 98_200_004_i64;
    let war_id = 700_200_001_i64;
    let now = 1_750_000_000_u64;
    let eff = now + 100;
    let exp = now + 500;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&att_id).cloned().expect("base char");
        if let Some(att) = snap.characters.get_mut(&att_id) {
            att.corporation_id = agg_corp;
            att.alliance_id = None;
            att.security_status = 0.0;
            att.war_faction_id = None;
        }
        for (cid, corp_id) in [
            (corp_ally_char, corp_ally),
            (alliance_ally_char, ext_member_corp),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
            ch.security_status = 0.0;
            ch.war_faction_id = None;
            snap.characters.insert(cid, ch);
        }
        for (cid, alliance_opt) in [
            (agg_corp, None),
            (def_corp, None),
            (corp_ally, None),
            (ext_member_corp, Some(ext_alliance)),
        ] {
            let mut corp = CorporationRecord::default();
            corp.corporation_id = cid;
            corp.alliance_id = alliance_opt;
            snap.corporations.insert(cid, corp);
        }
        snap.alliances.insert(
            ext_alliance,
            AllianceRecord {
                alliance_id: ext_alliance,
                name: "Ally Alliance".into(),
                short_name: "ALYA".into(),
                executor_corp_id: ext_member_corp,
                member_corp_ids: [ext_member_corp].into_iter().collect(),
            },
        );
        snap.wars.insert(
            war_id,
            WarRecord {
                war_id,
                aggressor_corp_id: agg_corp,
                defender_corp_id: def_corp,
                mutual: true,
                active: true,
                started_unix: now,
            },
        );
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 11,
            war_id,
            kind: PersistedNegKind::Ally,
            from_corp: corp_ally,
            isk: IskCents::ZERO,
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(eff),
        });
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 12,
            war_id,
            kind: PersistedNegKind::Ally,
            from_corp: ext_alliance,
            isk: IskCents::ZERO,
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(eff),
        });
        snap.war_state.ally_expires_unix.insert((war_id, corp_ally), exp);
        snap.war_state
            .ally_expires_unix
            .insert((war_id, ext_alliance), exp);
    });

    ctx.db.with_snapshot(|snap| {
        assert!(!is_legal_war_or_suspect_target(snap, att_id, corp_ally_char, eff - 1));
        assert!(!is_legal_war_or_suspect_target(snap, corp_ally_char, att_id, eff - 1));
        assert!(!is_legal_war_or_suspect_target(snap, att_id, alliance_ally_char, eff - 1));
        assert!(!is_legal_war_or_suspect_target(snap, alliance_ally_char, att_id, eff - 1));

        for ts in [eff, exp - 1] {
            assert!(is_legal_war_or_suspect_target(snap, att_id, corp_ally_char, ts));
            assert!(is_legal_war_or_suspect_target(snap, corp_ally_char, att_id, ts));
            assert!(is_legal_war_or_suspect_target(snap, att_id, alliance_ally_char, ts));
            assert!(is_legal_war_or_suspect_target(snap, alliance_ally_char, att_id, ts));
        }

        for ts in [exp, exp + 60] {
            assert!(!is_legal_war_or_suspect_target(snap, att_id, corp_ally_char, ts));
            assert!(!is_legal_war_or_suspect_target(snap, corp_ally_char, att_id, ts));
            assert!(!is_legal_war_or_suspect_target(snap, att_id, alliance_ally_char, ts));
            assert!(!is_legal_war_or_suspect_target(snap, alliance_ally_char, att_id, ts));
        }
    });
}

#[test]
fn with_mut_combat_runtime_preserves_db_owned_migrated_fields_no_clobber() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let other_id = char_id + 1;

    // Establish initial synced baseline before mutating DB-owned fields directly.
    with_combat_runtime(&ctx, |_| {});

    let sample_dkm = eve_db::DetailedKillmail {
        kill_id: 88_801,
        kill_time_unix: 1_700_000_000,
        solar_system_id: sys_id,
        victim_character_id: char_id,
        victim_corporation_id: 1_000_009,
        victim_alliance_id: None,
        victim_ship_type_id: 587,
        victim_damage_taken: 500.0,
        victim_position: [0.0, 0.0, 0.0],
        attackers: vec![],
        dropped_items: vec![],
        destroyed_items: vec![],
        total_value_isk: 123_456.0,
        killmail_hash: "no-clobber-hash".into(),
    };

    ctx.db.with_mut_snapshot(|snap| {
        set_char_safety_level_in_snap(snap, char_id, SafetyLevel::Red);
        set_char_safety_level_in_snap(snap, other_id, SafetyLevel::Yellow);
        let cw = snap.combat_crimewatch_state.as_inner_mut();
        cw.limited_engagements
            .insert(char_id, HashMap::from([(other_id, 1_900_000_300)]));
        cw.duel_challenges
            .insert((char_id, other_id), 1_900_000_060);
        cw.player_bounty_pool.insert(char_id, 4_200_000.0);
        cw.bounty_contributions
            .insert(char_id, vec![(other_id, 4_200_000.0)]);
        cw.bounty_hunter_claimed.insert(other_id, 850_000.0);
        cw.kill_right_expires_unix.insert(77_701, 1_900_000_000);
        cw.kill_right_restricted_to.insert(77_701, 98_000_001);
        cw.detailed_killmails.insert(88_801, sample_dkm.clone());
        cw.modified_system_securities.insert(sys_id, 0.45);
        cw.ess_reserve_unlocked_until.insert(sys_id, 1_700_009_000);
        cw.ess_reserve_bank_thefts
            .insert(sys_id, vec![(char_id, 250_000.0, 1_700_008_000)]);
        cw.ess_main_bank_thefts
            .insert(sys_id, vec![(other_id, 500_000.0, 1_700_008_500)]);
    });

    // Mutate remaining runtime fields via `with_mut_combat_runtime`, `with_mut_combat_runtime_in_snap`,
    // and an explicit runtime flush; none may clobber DB-owned fields.
    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_timers.insert(char_id, 1_800_000_000);
    });
    crate::services::drone_test_helpers::seed_active_drone(
        &ctx,
        DroneSpaceState {
            ball_id: 77_001,
            drone_item_id: 77_001,
            type_id: 2454,
            group_id: 100,
            owner_char_id: char_id,
            original_owner_char_id: char_id,
            parent_ship_id: ship_id,
            solar_system_id: sys_id,
            target_id: None,
            activity_state: 0,
            assist_char_id: None,
            guard_char_id: None,
            is_abandoned: false,
            is_sentry: false,
            bandwidth_used: 5.0,
            max_velocity: 1_000.0,
            optimal_range: 5_000.0,
            damage_vector: DamageVector::default(),
            shield_repair_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            web_speed_factor: 0.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        },
    );
    ctx.db.with_mut_snapshot(|snap| {
        with_mut_combat_runtime_in_snap(snap, |rt| {
            rt.npc_timers.insert(other_id, 1_800_000_100);
        });
    });
    flush_combat_runtime_to_db(&ctx);

    ctx.db.with_snapshot(|snap| {
        let cw = &snap.combat_crimewatch_state;
        assert_eq!(
            cw.limited_engagements
                .get(&char_id)
                .and_then(|m| m.get(&other_id)),
            Some(&1_900_000_300)
        );
        assert_eq!(
            cw.duel_challenges.get(&(char_id, other_id)),
            Some(&1_900_000_060)
        );
        assert_eq!(cw.player_bounty_pool.get(&char_id), Some(&4_200_000.0));
        assert_eq!(
            cw.bounty_contributions.get(&char_id),
            Some(&vec![(other_id, 4_200_000.0)])
        );
        assert_eq!(cw.bounty_hunter_claimed.get(&other_id), Some(&850_000.0));
        assert_eq!(cw.kill_right_expires_unix.get(&77_701), Some(&1_900_000_000));
        assert_eq!(cw.kill_right_restricted_to.get(&77_701), Some(&98_000_001));
        assert_eq!(cw.detailed_killmails.get(&88_801), Some(&sample_dkm));
        assert_eq!(cw.modified_system_securities.get(&sys_id), Some(&0.45));
        assert_eq!(
            cw.ess_reserve_unlocked_until.get(&sys_id),
            Some(&1_700_009_000)
        );
        assert_eq!(
            cw.ess_reserve_bank_thefts.get(&sys_id),
            Some(&vec![(char_id, 250_000.0, 1_700_008_000)])
        );
        assert_eq!(
            cw.ess_main_bank_thefts.get(&sys_id),
            Some(&vec![(other_id, 500_000.0, 1_700_008_500)])
        );

        assert_eq!(cw.safety_levels.get(&char_id), Some(&(SafetyLevel::Red as i32)));
        assert_eq!(char_safety_level_in_snap(snap, char_id), Some(SafetyLevel::Red));
        assert_eq!(char_safety_level_in_snap(snap, other_id), Some(SafetyLevel::Yellow));
        assert_eq!(cw.npc_timers.get(&char_id), Some(&1_800_000_000));
        assert_eq!(cw.npc_timers.get(&other_id), Some(&1_800_000_100));
    });
    assert!(crate::services::drone_test_helpers::has_active_drone(&ctx, 77_001));
}

#[test]
fn combat_runtime_state_drops_limited_engagements_and_duel_challenges_and_helpers_work() {
    let src = include_str!("combat_crimewatch.rs");
    let struct_body = src
        .split("pub struct CombatRuntimeState {")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .expect("CombatRuntimeState struct body");
    let impl_body = src
        .split("impl CombatRuntimeState {")
        .nth(1)
        .and_then(|s| s.split("\nfn merge_map_3way").next())
        .expect("impl CombatRuntimeState body");
    for forbidden in [
        "limited_engagements",
        "duel_challenges",
        "has_limited_engagement_with",
        "add_limited_engagement",
        "is_in_any_limited_engagement",
    ] {
        assert!(
            !struct_body.contains(forbidden),
            "CombatRuntimeState must not contain `{forbidden}` (#1541)"
        );
        assert!(
            !impl_body.contains(forbidden),
            "impl CombatRuntimeState must not contain `{forbidden}` (#1541)"
        );
    }

    let (ctx, _session, char_a, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let char_b = char_a + 1;
    let char_c = char_a + 2;
    let now = 1_750_000_000_u64;

    assert!(!has_limited_engagement_with(&ctx, char_a, char_b, now));
    assert!(!is_in_any_limited_engagement(&ctx, char_a, now));
    assert!(!ensure_mutual_limited_engagement_if(&ctx, char_a, char_b, false, now));

    add_limited_engagement(&ctx, char_a, char_b, now);
    assert!(has_limited_engagement_with(&ctx, char_a, char_b, now + 299));
    assert!(!has_limited_engagement_with(&ctx, char_b, char_a, now + 299));
    assert!(is_in_any_limited_engagement(&ctx, char_a, now + 299));
    assert!(!is_in_any_limited_engagement(&ctx, char_a, now + 300));

    assert!(ensure_mutual_limited_engagement_if(
        &ctx,
        char_a,
        char_b,
        false,
        now + 50
    ));
    assert!(has_limited_engagement_with(&ctx, char_a, char_b, now + 349));
    assert!(has_limited_engagement_with(&ctx, char_b, char_a, now + 349));

    // Unshared engagement inspection between char_c (helper) and char_b (beneficiary)
    ctx.db.with_snapshot(|snap| {
        assert!(has_unshared_limited_engagement_in_snap(
            snap,
            char_c,
            char_b,
            now + 50
        ));
        assert_eq!(
            unshared_limited_engagement_partners_in_snap(snap, char_c, char_b, now + 50),
            vec![char_a]
        );
    });
    add_limited_engagement(&ctx, char_c, char_a, now + 50);
    ctx.db.with_snapshot(|snap| {
        assert!(!has_unshared_limited_engagement_in_snap(
            snap,
            char_c,
            char_b,
            now + 50
        ));
        assert!(unshared_limited_engagement_partners_in_snap(snap, char_c, char_b, now + 50).is_empty());
    });
}
