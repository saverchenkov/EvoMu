//! Edge-case unit tests for `security_mgr`:
//! auth guard on `securityMgr.get_modified_systems`, manual security overrides vs stage 5,
//! suppression stage window checks, `sync_corruption_security_status_change` sentry deactivation
//! & restore, Pochven/non-aligned/self-destruct PvP loot drop, pirate warp & empire web/scram
//! multipliers, live counter-insurgency sentries & out-of-range EWAR, SDE dungeon fallbacks,
//! Angel vs Guristas war HQs, site pruning, and `nearest_sentry_gun_origin`.

use super::*;
use crate::services::combat_crimewatch::{
    with_mut_combat_runtime, ExtendedNpcEntity, SentryAggressionState,
};
use crate::services::insurgency_services::FACTION_ANGEL_CARTEL;
use eve_db::{
    PersistedIceHeistRecord, PersistedInsurgencyCampaign, PersistedInsurgencySiteRecord,
    SystemDynamicState,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx_and_session() -> (ServerContext, SessionState, i64, u32) {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        1_776_200_000,
    )));
    let char_id = 90_000_001i64;
    let sys_id = 30_000_142u32;
    let mut sess = SessionState::default();
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    (ctx, sess, char_id, sys_id)
}

#[test]
fn rpc_auth_manual_override_and_suppression_window_edges() {
    let (ctx, sess, _char_id, sys_id) = make_ctx_and_session();

    // Unauthenticated call returns Err
    let unauth = SessionState::default();
    assert!(handle_security_mgr_call(&ctx, &unauth, "get_modified_systems", &[])
        .unwrap()
        .is_err());
    // Unknown method returns None
    assert!(handle_security_mgr_call(&ctx, &sess, "", &[]).is_none());

    // Manual override takes precedence over corruption stage 5 in compute_modified_systems
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                insurgency_corruption_stage: 5,
                insurgency_suppression_stage: 3,
                ..Default::default()
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(sys_id, -0.42);
    });
    let mods = compute_modified_systems(&ctx);
    assert_eq!(mods.get(&sys_id), Some(&-0.42));
    assert_eq!(get_effective_system_security(&ctx, sys_id), -0.42);

    // Unknown system fallback -> 0.0
    assert_eq!(get_effective_system_security(&ctx, 99_999_999), 0.0);

    // Without an active/winddown campaign covering sys_id, suppression stage is 0 even if system_state says 3
    assert_eq!(insurgency_system_suppression_stage(&ctx, sys_id), 0);
    assert_eq!(insurgency_suppression_bounty_and_sec_multiplier(&ctx, sys_id), 1.0);
    assert_eq!(
        insurgency_suppression_empire_lp_multiplier(&ctx, sys_id, 500_001),
        1.0
    );
    // Non-empire faction always 1.0
    assert_eq!(
        insurgency_suppression_empire_lp_multiplier(&ctx, sys_id, 500_010),
        1.0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_corruption_security_status_change_and_sentries_and_ewar() {
    let (ctx, _sess, _char_id, sys_id) = make_ctx_and_session();
    let other_sys = 30_002_187u32;

    // Same lawless state (0->4 and 5->5) is a no-op
    sync_corruption_security_status_change(&ctx, sys_id, 0, 4);
    sync_corruption_security_status_change(&ctx, sys_id, 5, 5);

    // Entering stage 5 deactivates sentry_gun_aggression only for sys_id
    with_mut_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression.insert(
            (sys_id, 100),
            SentryAggressionState {
                solar_system_id: sys_id,
                offender_char_id: 90_000_001,
                offender_ship_id: 1_000_000_001,
                sentry_origin: DVec3::ZERO,
                active_on_grid: true,
                total_sentry_damage_dealt: 0.0,
            },
        );
        rt.sentry_gun_aggression.insert(
            (other_sys, 200),
            SentryAggressionState {
                solar_system_id: other_sys,
                offender_char_id: 90_000_001,
                offender_ship_id: 1_000_000_001,
                sentry_origin: DVec3::ZERO,
                active_on_grid: true,
                total_sentry_damage_dealt: 0.0,
            },
        );
    });
    sync_corruption_security_status_change(&ctx, sys_id, 4, 5);
    with_mut_combat_runtime(&ctx, |rt| {
        assert!(!rt.sentry_gun_aggression[&(sys_id, 100)].active_on_grid);
        assert!(rt.sentry_gun_aggression[&(other_sys, 200)].active_on_grid);
        // Manual override prevents security update emission
        rt.modified_system_securities.insert(sys_id, 0.25);
    });
    sync_corruption_security_status_change(&ctx, sys_id, 5, 3);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.remove(&sys_id);
    });
    // Exiting stage 5 without override restores SDE security (and unknown sys restores 0.0)
    sync_corruption_security_status_change(&ctx, sys_id, 5, 2);
    sync_corruption_security_status_change(&ctx, 99_999_999, 5, 0);

    // has_active_counter_insurgency_sentries: false initially, false when not in ballpark,
    // false when dead (structure_hp == 0), true when alive
    assert!(!has_active_counter_insurgency_sentries(&ctx, sys_id));
    let sentry_bid = 9_100_001i64;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.insert(
            sentry_bid,
            ExtendedNpcEntity {
                ball_id: sentry_bid,
                type_id: 79_507,
                faction_id: 500_001,
                archetype: "CounterInsurgencySentry".into(),
                solar_system_id: sys_id,
                spawn_group_id: 0,
                target_id: None,
                aggression_threat: HashMap::new(),
                threat_table: HashMap::new(),
                bounty_isk: 0.0,
                optimal_range: 250_000.0,
                orbit_range: 0.0,
                damage_vector: DamageVector::default(),
                entropic_cycles: 0,
                drifter_overshield_hp: 0.0,
                drifter_doomsday_fired: false,
                web_speed_factor: 0.0,
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
            },
        );
    });
    assert!(!has_active_counter_insurgency_sentries(&ctx, sys_id));

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut dead_cs = CombatState::default();
    dead_cs.structure_hp = 0.0;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: sentry_bid,
                type_id: 79_507,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            dead_cs,
        )
        .await
        .unwrap();
    assert!(!has_active_counter_insurgency_sentries(&ctx, sys_id));

    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.combat_states.get_mut(&sentry_bid).unwrap().structure_hp = 5_000.0;
    }
    assert!(has_active_counter_insurgency_sentries(&ctx, sys_id));

    // apply_counter_insurgency_sentry_ewar: out of range, TP-only range, and sig==0 fallback
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        let target_id = 9_100_002i64;
        let mut tcs = CombatState::default();
        tcs.signature_radius = 0.0;
        bp.combat_states.insert(target_id, tcs);

        // Beyond 250km -> untouched
        apply_counter_insurgency_sentry_ewar(&ctx.sde, &mut bp, target_id, 20.0, 300_000.0);
        assert_eq!(bp.combat_states[&target_id].signature_radius, 0.0);

        // Between 75km and 250km -> TP applied with target_radius.max(35.0) = 35.0
        apply_counter_insurgency_sentry_ewar(&ctx.sde, &mut bp, target_id, 20.0, 100_000.0);
        assert!(bp.combat_states[&target_id].signature_radius > 35.0);
        assert!(bp.combat_states[&target_id].base_signature_radius == Some(35.0));
    }
}

#[test]
fn insurgency_loot_warp_web_scram_and_site_sync_pruning_edges() {
    let (ctx, _sess, char_id, sys_id) = make_ctx_and_session();
    let now = ctx.now_unix();
    let pochven_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .unwrap_or(sys_id);

    // Boundary checks when char_id <= 0, self-destruct, victim <= 0, same char, stage 0
    assert_eq!(
        insurgency_pvp_loot_drop_threshold(&ctx, pochven_sys, char_id, 90_000_002, false),
        0.60
    );
    assert_eq!(
        insurgency_pvp_loot_drop_threshold(&ctx, sys_id, char_id, char_id, false),
        0.50
    );
    assert_eq!(
        insurgency_pvp_loot_drop_threshold(&ctx, sys_id, 0, 90_000_002, false),
        0.50
    );
    assert_eq!(
        insurgency_pvp_loot_drop_threshold(&ctx, sys_id, char_id, 90_000_002, true),
        0.50
    );
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, 0, sys_id), 0.0);
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, char_id, sys_id), 0.0);
    assert_eq!(
        insurgency_suppression_web_scram_range_multiplier(&ctx, 0, sys_id),
        1.0
    );
    assert_eq!(
        insurgency_suppression_web_scram_range_multiplier(&ctx, char_id, sys_id),
        1.0
    );

    // Active Angel Cartel campaign (FACTION_ANGEL_CARTEL = 500_011) at corruption stage 3 & suppression stage 1
    // Char enlisted in Guristas (500_010) -> NOT aligned with Angel campaign in sys_id!
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_010);
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                insurgency_corruption_stage: 3,
                insurgency_suppression_stage: 1,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            99,
            PersistedInsurgencyCampaign {
                campaign_id: 99,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_002,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now - 200,
                forecasting_end_unix: now - 100,
                active_end_unix: now + 1800,
                winddown_end_unix: now + 3600,
                corruption_points: [(sys_id, 500)].into_iter().collect(),
                suppression_points: [(sys_id, 100)].into_iter().collect(),
            },
        );
        sync_stage_gated_insurgency_sites_in_snap(&ctx.sde, snap, sys_id, now);
        // Also test a system with no celestials (synthetic ID)
        snap.insurgency_runtime.campaigns.insert(
            100,
            PersistedInsurgencyCampaign {
                campaign_id: 100,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_002,
                fob_solar_system_id: 30_999_991,
                systems: vec![30_999_991],
                created_at_unix: now - 200,
                forecasting_end_unix: now - 100,
                active_end_unix: now + 1800,
                winddown_end_unix: now + 3600,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
        sync_stage_gated_insurgency_sites_in_snap(&ctx.sde, snap, 30_999_991, now);
    });

    assert!(!is_pilot_aligned_with_insurgency_pirate(&ctx, sys_id, char_id));
    assert_eq!(
        insurgency_pvp_loot_drop_threshold(&ctx, sys_id, char_id, 90_000_002, false),
        0.50
    );
    // Stage 1 suppression gives 1.0x empire LP (requires stage >= 2)
    assert_eq!(
        insurgency_suppression_empire_lp_multiplier(&ctx, sys_id, 500_001),
        1.0
    );

    // Stage 4 corruption but pilot is not aligned -> warp bonus 0.0; stage 4 suppression but pilot is pirate -> web/scram 1.0
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.get_mut(&sys_id).unwrap();
        st.insurgency_corruption_stage = 4;
        st.insurgency_suppression_stage = 4;
        snap.insurgency_runtime
            .campaigns
            .get_mut(&99)
            .unwrap()
            .corruption_points
            .insert(sys_id, 750);
    });
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, char_id, sys_id), 0.0);
    assert_eq!(
        insurgency_suppression_web_scram_range_multiplier(&ctx, char_id, sys_id),
        1.0
    );

    // Prune ineligible sites when corruption drops to 0: MiningOpAmbush & IceRefineryHeist & ice_heist_instances pruned;
    // PirateStronghold & CorporateOutpostRaid & CounterInsurgencyOutpost(at war HQ) kept
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_id).unwrap().insurgency_corruption_stage = 0;
        snap.insurgency_runtime
            .insurgency_instances
            .push(PersistedInsurgencySiteRecord {
                instance_id: 960_999,
                solar_system_id: sys_id,
                site_type: "PirateStronghold".into(),
                name: "Stronghold".into(),
                corruption_reward: 500.0,
                completed: false,
            });
        snap.insurgency_runtime
            .ice_heist_instances
            .push(PersistedIceHeistRecord {
                instance_id: 860_999,
                solar_system_id: sys_id,
                pirate_tankers_saved: 0,
                empire_tankers_saved: 0,
                completed: false,
            });
        prune_ineligible_insurgency_sites_in_snap(snap, sys_id);
        assert!(snap
            .insurgency_runtime
            .insurgency_instances
            .iter()
            .any(|s| s.solar_system_id == sys_id && s.site_type == "PirateStronghold"));
        assert!(!snap
            .insurgency_runtime
            .insurgency_instances
            .iter()
            .any(|s| s.solar_system_id == sys_id && s.site_type == "IceRefineryHeist"));
        assert!(!snap
            .insurgency_runtime
            .ice_heist_instances
            .iter()
            .any(|h| h.solar_system_id == sys_id && h.instance_id == 860_999));
    });

    // sde_insurgency_dungeon_name fallback when no archetype matches
    assert_eq!(
        sde_insurgency_dungeon_name(&ctx.sde, &[999_999], 500_010, "Fallback Name"),
        "Fallback Name"
    );

    // nearest_sentry_gun_origin: hits near a gate/station in Jita, None far away
    if let Some(gate) = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
    {
        let found = nearest_sentry_gun_origin(&ctx, sys_id, gate.position + DVec3::new(1000.0, 0.0, 0.0));
        assert_eq!(found, Some(gate.position));
    }
    assert!(nearest_sentry_gun_origin(&ctx, sys_id, DVec3::new(1e15, 1e15, 1e15)).is_none());
}

#[tokio::test]
async fn counter_insurgency_ewar_sig_branches_multiplier_and_stage_site_sync() {
    let (ctx, _sess, _char_id, sys_id) = make_ctx_and_session();
    let now = ctx.now_unix();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let tp_sig_pct = ctx
        .sde
        .types
        .get(&79_507)
        .and_then(|t| t.default_attributes.get(&554).copied())
        .unwrap_or(50.0);
    let expected_mult = 1.0 + (tp_sig_pct * 0.01);

    {
        let mut bp = sys_handle.ballpark.write().unwrap();

        // Case A: signature_radius == 0.0 -> falls back to target_radius.max(35.0) = 50.0 (kills `> -> >=` and `> -> ==`)
        let target_zero = 9_101_001_i64;
        let mut cs_zero = CombatState::default();
        cs_zero.signature_radius = 0.0;
        bp.combat_states.insert(target_zero, cs_zero);
        apply_counter_insurgency_sentry_ewar(&ctx.sde, &mut bp, target_zero, 50.0, 100_000.0);
        assert_eq!(bp.combat_states[&target_zero].base_signature_radius, Some(50.0));
        let mult_zero = bp.combat_states[&target_zero].tp_sig_multiplier.unwrap();
        assert!((mult_zero - expected_mult).abs() < 1e-6);

        // Case B: signature_radius == 42.0 > 0.0 -> preserves 42.0 instead of target_radius 90.0 (kills `> -> <` and `> -> ==`)
        let target_pos = 9_101_002_i64;
        let mut cs_pos = CombatState::default();
        cs_pos.signature_radius = 42.0;
        bp.combat_states.insert(target_pos, cs_pos);
        apply_counter_insurgency_sentry_ewar(&ctx.sde, &mut bp, target_pos, 90.0, 10_000.0);
        assert_eq!(bp.combat_states[&target_pos].base_signature_radius, Some(42.0));
        let mult_pos = bp.combat_states[&target_pos].tp_sig_multiplier.unwrap();
        assert!((mult_pos - expected_mult).abs() < 1e-6);
        assert!(bp.combat_states[&target_pos].ecm_jammed_remaining_s > 0.0);
    }

    // `sync_stage_gated_insurgency_sites_in_snap` actually populates stage-gated sites for active campaign
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                insurgency_corruption_stage: 3,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            707,
            PersistedInsurgencyCampaign {
                campaign_id: 707,
                pirate_faction_id: FACTION_ANGEL_CARTEL,
                target_faction_id: 500_002,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now - 200,
                forecasting_end_unix: now - 100,
                active_end_unix: now + 1800,
                winddown_end_unix: now + 3600,
                corruption_points: [(sys_id, 500)].into_iter().collect(),
                suppression_points: HashMap::new(),
            },
        );
        sync_stage_gated_insurgency_sites_in_snap(&ctx.sde, snap, sys_id, now);
        assert!(
            snap.insurgency_runtime
                .insurgency_instances
                .iter()
                .any(|s| s.solar_system_id == sys_id && !s.completed),
            "sync_stage_gated_insurgency_sites_in_snap must spawn corruption stage-3 sites"
        );
    });
}
