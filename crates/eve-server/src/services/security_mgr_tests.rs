use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{PersistedInsurgencyCampaign, SystemDynamicState};

#[test]
fn test_security_mgr_factions_war_hq_and_effective_security() {
    let (ctx, session, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    assert!(is_pirate_militia_faction(500_010));
    assert!(is_pirate_militia_faction(500_011));
    assert!(!is_pirate_militia_faction(500_001));

    assert!(is_empire_militia_faction(500_001));
    assert!(is_empire_militia_faction(500_004));
    assert!(!is_empire_militia_faction(500_010));

    assert!(is_empire_war_hq_system(30_002_974));
    assert!(is_empire_war_hq_system(30_045_324));
    assert!(!is_empire_war_hq_system(30_000_142));

    // Seed an insurgency system at corruption stage 5
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                insurgency_corruption_stage: 5,
                insurgency_suppression_stage: 5,
                ..Default::default()
            },
        );
    });

    // Stage 5 corruption forces effective security to 0.0
    let eff_sec = get_effective_system_security(&ctx, sys_id);
    assert_eq!(eff_sec, 0.0);

    // Stage 3 corruption leaves SDE security unmodified
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_corruption_stage = 3;
        }
    });
    assert!(get_effective_system_security(&ctx, sys_id) >= 0.0);

    // security_mgr RPC calls
    let mod_sys = handle_security_mgr_call(&ctx, &session, "get_modified_systems", &[])
        .unwrap()
        .unwrap();
    assert!(matches!(mod_sys, PyRep::Dict(_)));
    assert!(handle_security_mgr_call(&ctx, &session, "unknown", &[]).is_none());
}

#[tokio::test]
async fn test_security_mgr_insurgency_multipliers_sites_and_sentry_ewar() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();

    // Seed active campaign with corruption & suppression stage 4
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_010);
        }
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                insurgency_corruption_stage: 4,
                insurgency_suppression_stage: 4,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            1,
            PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now.saturating_sub(200),
                forecasting_end_unix: now.saturating_sub(100),
                active_end_unix: now + 1800,
                winddown_end_unix: now + 3600,
                corruption_points: [(sys_id, 700)].into_iter().collect(),
                suppression_points: [(sys_id, 700)].into_iter().collect(),
            },
        );
        sync_stage_gated_insurgency_sites_in_snap(&ctx.sde, snap, sys_id, now);
    });

    assert!((insurgency_pvp_loot_drop_threshold(&ctx, sys_id, char_id, 90_000_099, false) - 0.80).abs() < 1e-6);
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, char_id, sys_id), 2.0);
    assert!((insurgency_suppression_bounty_and_sec_multiplier(&ctx, sys_id) - 1.20).abs() < 1e-6);
    assert!((insurgency_suppression_empire_lp_multiplier(&ctx, sys_id, 500_001) - 1.15).abs() < 1e-6);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_001);
        }
    });
    assert!((insurgency_suppression_web_scram_range_multiplier(&ctx, char_id, sys_id) - 1.10).abs() < 1e-6);

    // Verify stage-gated insurgency sites were spawned in snap
    let site_count = ctx
        .db
        .with_snapshot(|snap| snap.insurgency_runtime.insurgency_instances.len());
    assert!(site_count >= 2);

    // sync_corruption_security_status_change
    sync_corruption_security_status_change(&ctx, sys_id, 2, 5);

    // apply_counter_insurgency_sentry_ewar on ballpark combat state
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut bp = sys_handle.ballpark.write().unwrap();
    bp.combat_states.insert(ship_id, eve_destiny::CombatState::default());
    apply_counter_insurgency_sentry_ewar(&ctx.sde, &mut bp, ship_id, 50.0, 10_000.0);
    let cs = bp.combat_states.get(&ship_id).unwrap();
    assert!(cs.signature_radius > 50.0);
}

#[test]
fn test_security_mgr_rpc_auth_and_manual_override_precedence() {
    let (ctx, _session, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // Unauthenticated session fails get_modified_systems
    let anon = SessionState::default();
    assert!(handle_security_mgr_call(&ctx, &anon, "securityMgr_bad", &[]).is_none());
    let auth_err = handle_security_mgr_call(&ctx, &anon, "get_modified_systems", &[])
        .unwrap()
        .unwrap_err();
    assert!(!auth_err.code.is_empty());

    // Manual override in modified_system_securities takes precedence over Stage 5 lawless (0.0)
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                insurgency_corruption_stage: 5,
                ..Default::default()
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(sys_id, 0.42);
    });
    let mod_map = compute_modified_systems(&ctx);
    assert_eq!(mod_map.get(&sys_id).copied(), Some(0.42));
    assert!((get_effective_system_security(&ctx, sys_id) - 0.42).abs() < 1e-9);

    // Unknown system ID falls back to 0.0
    assert_eq!(get_effective_system_security(&ctx, 99_999_999), 0.0);

    // Authenticated get_modified_systems returns the overridden value
    let mut authed = SessionState::new(1);
    authed.user_id = Some(1);
    authed.character_id = Some(90_000_001);
    let rep = handle_security_mgr_call(&ctx, &authed, "get_modified_systems", &[])
        .unwrap()
        .unwrap();
    let PyRep::Dict(entries) = rep else {
        panic!("expected dict from get_modified_systems");
    };
    let sys_sec = entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(sys_id as i64))
        .and_then(|(_, v)| v.as_f64());
    assert_eq!(sys_sec, Some(0.42));
}

#[test]
fn test_insurgency_suppression_stage_and_lp_web_scram_boundaries() {
    let (ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();

    // Outside active/winddown window -> suppression stage is 0 even if system_states has stage 5
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                insurgency_suppression_stage: 5,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            10,
            PersistedInsurgencyCampaign {
                campaign_id: 10,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now,
                forecasting_end_unix: now + 100, // still forecasting
                active_end_unix: now + 500,
                winddown_end_unix: now + 1000,
                corruption_points: HashMap::new(),
                suppression_points: [(sys_id, 1000)].into_iter().collect(),
            },
        );
    });
    assert_eq!(insurgency_system_suppression_stage(&ctx, sys_id), 0);
    assert!((insurgency_suppression_bounty_and_sec_multiplier(&ctx, sys_id) - 1.0).abs() < 1e-9);
    assert!((insurgency_suppression_empire_lp_multiplier(&ctx, sys_id, 500_001) - 1.0).abs() < 1e-9);

    // Move campaign into active window with stage 1 suppression (points=200 -> stage 1)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_suppression_stage = 1;
        }
        if let Some(c) = snap.insurgency_runtime.campaigns.get_mut(&10) {
            c.forecasting_end_unix = now.saturating_sub(10);
            c.suppression_points.insert(sys_id, 200);
        }
    });
    assert_eq!(insurgency_system_suppression_stage(&ctx, sys_id), 1);
    assert!((insurgency_suppression_bounty_and_sec_multiplier(&ctx, sys_id) - 1.05).abs() < 1e-9);
    // Stage 1 is below stage 2 threshold for Empire LP bonus
    assert!((insurgency_suppression_empire_lp_multiplier(&ctx, sys_id, 500_001) - 1.0).abs() < 1e-9);
    // Pirate militia faction never gets Empire LP bonus even at stage 5
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.insurgency_suppression_stage = 5;
        }
    });
    assert!((insurgency_suppression_empire_lp_multiplier(&ctx, sys_id, 500_010) - 1.0).abs() < 1e-9);
    assert!((insurgency_suppression_empire_lp_multiplier(&ctx, sys_id, 500_001) - 1.20).abs() < 1e-9);
    assert!((insurgency_suppression_bounty_and_sec_multiplier(&ctx, sys_id) - 1.25).abs() < 1e-9);

    // Web/scram range multiplier guards: char_id <= 0, non-empire, stage < 4
    assert_eq!(insurgency_suppression_web_scram_range_multiplier(&ctx, 0, sys_id), 1.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_010);
        }
    });
    assert_eq!(insurgency_suppression_web_scram_range_multiplier(&ctx, char_id, sys_id), 1.0);

    // Campaign past winddown_end_unix -> stage drops back to 0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.insurgency_runtime.campaigns.get_mut(&10) {
            c.winddown_end_unix = now;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_001);
        }
    });
    assert_eq!(insurgency_system_suppression_stage(&ctx, sys_id), 0);
    assert_eq!(insurgency_suppression_web_scram_range_multiplier(&ctx, char_id, sys_id), 1.0);
}

#[test]
fn test_sync_corruption_security_status_change_transitions_and_guards() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let other_sys = 30_000_144_u32;

    // Seed sentry_gun_aggression in both sys_id and other_sys
    with_mut_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression.insert(
            (sys_id, 1),
            crate::services::combat_crimewatch::SentryAggressionState {
                solar_system_id: sys_id,
                offender_char_id: char_id,
                offender_ship_id: ship_id,
                sentry_origin: DVec3::ZERO,
                active_on_grid: true,
                total_sentry_damage_dealt: 0.0,
            },
        );
        rt.sentry_gun_aggression.insert(
            (other_sys, 2),
            crate::services::combat_crimewatch::SentryAggressionState {
                solar_system_id: other_sys,
                offender_char_id: char_id,
                offender_ship_id: ship_id,
                sentry_origin: DVec3::ZERO,
                active_on_grid: true,
                total_sentry_damage_dealt: 0.0,
            },
        );
    });

    let _ = ctx.db.acquire_online_lock(1, 1, Some(char_id), true, ctx.now_unix());

    // No transition (2 -> 4 or 5 -> 5) -> no-op, leaves sentries active and emits no notification
    let _ = ctx.drain_notifications_for_char(char_id);
    sync_corruption_security_status_change(&ctx, sys_id, 2, 4);
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.sentry_gun_aggression[&(sys_id, 1)].active_on_grid);
    });
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Enter Stage 5 lawless -> disables sentries only in sys_id and emits OnSecurityModified(0.0)
    sync_corruption_security_status_change(&ctx, sys_id, 4, 5);
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.sentry_gun_aggression[&(sys_id, 1)].active_on_grid);
        assert!(rt.sentry_gun_aggression[&(other_sys, 2)].active_on_grid);
    });
    let enter_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(enter_notifs.iter().any(|n| n.event == "OnSecurityModified"));

    // Exit Stage 5 lawless -> restores SDE security and emits OnSecurityModified
    sync_corruption_security_status_change(&ctx, sys_id, 5, 3);
    let exit_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(exit_notifs.iter().any(|n| n.event == "OnSecurityModified"));

    // Manual override suppresses OnSecurityModified emission
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(sys_id, 0.75);
    });
    sync_corruption_security_status_change(&ctx, sys_id, 3, 5);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
}

#[test]
fn test_pilot_alignment_loot_drop_and_warp_bonus_edge_cases() {
    let (ctx, _session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();

    // Non-positive char_id & low corruption stage return 0.0 warp bonus
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, 0, sys_id), 0.0);
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, char_id, sys_id), 0.0);

    // Self-destruct / invalid IDs / same attacker & victim return base threshold
    assert!((insurgency_pvp_loot_drop_threshold(&ctx, sys_id, char_id, 90_000_099, true) - 0.50).abs() < 1e-9);
    assert!((insurgency_pvp_loot_drop_threshold(&ctx, sys_id, 0, 90_000_099, false) - 0.50).abs() < 1e-9);
    assert!((insurgency_pvp_loot_drop_threshold(&ctx, sys_id, char_id, 0, false) - 0.50).abs() < 1e-9);
    assert!((insurgency_pvp_loot_drop_threshold(&ctx, sys_id, char_id, char_id, false) - 0.50).abs() < 1e-9);

    // Pochven system (region 10_000_070) has 0.60 base loot drop threshold
    if let Some(pochven_sys) = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
    {
        assert!((insurgency_pvp_loot_drop_threshold(&ctx, pochven_sys, char_id, 90_000_099, false) - 0.60).abs() < 1e-9);
    }

    // Seed Guristas campaign (500_010) at stage 4
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                insurgency_corruption_stage: 4,
                ..Default::default()
            },
        );
        snap.insurgency_runtime.campaigns.insert(
            20,
            PersistedInsurgencyCampaign {
                campaign_id: 20,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now.saturating_sub(50),
                forecasting_end_unix: now.saturating_sub(10),
                active_end_unix: now + 500,
                winddown_end_unix: now + 1000,
                corruption_points: [(sys_id, 700)].into_iter().collect(),
                suppression_points: HashMap::new(),
            },
        );
        // Character enlisted in Angel Cartel (500_011) is NOT aligned with Guristas campaign
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_011);
        }
    });
    assert!(!is_pilot_aligned_with_insurgency_pirate(&ctx, sys_id, char_id));
    assert_eq!(insurgency_pirate_warp_bonus_au_s(&ctx, char_id, sys_id), 0.0);
    assert!((insurgency_pvp_loot_drop_threshold(&ctx, sys_id, char_id, 90_000_099, false) - 0.50).abs() < 1e-9);

    // Character with war_faction_id = None or Empire militia is also false
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = None;
        }
    });
    assert!(!is_pilot_aligned_with_insurgency_pirate(&ctx, sys_id, char_id));
}

#[tokio::test]
async fn test_active_ci_sentries_ewar_boundaries_and_nearest_sentry_origin() {
    let (ctx, _session, _char_id, ship_id, sys_id) = make_fighter_ctx_and_session();

    // No CI sentries initially
    assert!(!has_active_counter_insurgency_sentries(&ctx, sys_id));

    let sentry_ball_id = 9_795_070_i64;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.insert(
            sentry_ball_id,
            crate::services::combat_crimewatch::ExtendedNpcEntity {
                ball_id: sentry_ball_id,
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
                damage_vector: Default::default(),
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
            },
        );
    });
    // Not in ballpark yet -> false
    assert!(!has_active_counter_insurgency_sentries(&ctx, sys_id));

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.balls.insert(
            sentry_ball_id,
            eve_destiny::Ball {
                id: sentry_ball_id,
                type_id: 79_507,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
        );
        bp.combat_states.insert(
            sentry_ball_id,
            eve_destiny::CombatState {
                structure_hp: 5000.0,
                max_structure_hp: 5000.0,
                ..Default::default()
            },
        );
    }
    assert!(has_active_counter_insurgency_sentries(&ctx, sys_id));

    // Destroyed sentry (structure_hp <= 0) -> false
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.combat_states.get_mut(&sentry_ball_id).unwrap().structure_hp = 0.0;
    }
    assert!(!has_active_counter_insurgency_sentries(&ctx, sys_id));

    // EWAR range & base_signature_radius boundaries
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.combat_states.insert(
            ship_id,
            eve_destiny::CombatState {
                signature_radius: 0.0,
                base_signature_radius: None,
                ..Default::default()
            },
        );
        // Outside 250 km -> no TP and no ECM
        apply_counter_insurgency_sentry_ewar(&ctx.sde, &mut bp, ship_id, 20.0, 300_000.0);
        assert_eq!(bp.combat_states[&ship_id].base_signature_radius, None);

        // Between 75 km and 250 km -> TP applied (clamped min base 35.0 -> 52.5), no ECM
        apply_counter_insurgency_sentry_ewar(&ctx.sde, &mut bp, ship_id, 20.0, 100_000.0);
        assert_eq!(bp.combat_states[&ship_id].base_signature_radius, Some(35.0));
        let painted_sig = bp.combat_states[&ship_id].signature_radius;
        assert!(painted_sig > 35.0);

        // Second application does not compound base_signature_radius
        apply_counter_insurgency_sentry_ewar(&ctx.sde, &mut bp, ship_id, 20.0, 100_000.0);
        assert!((bp.combat_states[&ship_id].signature_radius - painted_sig).abs() < 1e-9);
    }

    // nearest_sentry_gun_origin near an SDE stargate/station vs far away
    if let Some(gate_pos) = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .map(|j| j.position)
        .or_else(|| {
            ctx.sde
                .stations
                .values()
                .find(|s| s.solar_system_id == sys_id)
                .map(|s| s.position)
        })
    {
        assert_eq!(
            nearest_sentry_gun_origin(&ctx, sys_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)),
            Some(gate_pos)
        );
    }
    assert_eq!(
        nearest_sentry_gun_origin(&ctx, sys_id, DVec3::new(1.0e15, 1.0e15, 1.0e15)),
        None
    );
}

#[test]
fn test_sync_and_prune_stage_gated_insurgency_sites_and_dungeon_names() {
    let (ctx, _session, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();

    // Unknown archetype falls back to supplied string
    assert_eq!(
        sde_insurgency_dungeon_name(&ctx.sde, &[999_999], 500_010, "Fallback Dungeon"),
        "Fallback Dungeon"
    );

    // Seed Angel Cartel campaign covering sys_id (stage 3) -> spawns MiningOpAmbush, IceRefineryHeist, ice_heist_instances, and Empire War HQ outposts
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
            30,
            PersistedInsurgencyCampaign {
                campaign_id: 30,
                pirate_faction_id: 500_011,
                target_faction_id: 500_002,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now.saturating_sub(100),
                forecasting_end_unix: now.saturating_sub(50),
                active_end_unix: now + 500,
                winddown_end_unix: now + 1000,
                corruption_points: [(sys_id, 500)].into_iter().collect(),
                suppression_points: HashMap::new(),
            },
        );
        snap.insurgency_runtime
            .insurgency_instances
            .push(eve_db::PersistedInsurgencySiteRecord {
                instance_id: 960_999,
                solar_system_id: sys_id,
                site_type: "PirateStronghold".into(),
                name: "FOB".into(),
                corruption_reward: 0.0,
                completed: false,
            });
        sync_stage_gated_insurgency_sites_in_snap(&ctx.sde, snap, sys_id, now);
        assert!(snap
            .insurgency_runtime
            .insurgency_instances
            .iter()
            .any(|s| s.solar_system_id == sys_id && s.site_type == "IceRefineryHeist"));
        assert!(snap
            .insurgency_runtime
            .insurgency_instances
            .iter()
            .any(|s| s.solar_system_id == sys_id && s.site_type == "PirateLaunderingFacility"));
        assert!(snap
            .insurgency_runtime
            .insurgency_instances
            .iter()
            .any(|s| s.solar_system_id == 30_002_974 && s.site_type == "CounterInsurgencyOutpost"));
        assert!(!snap.insurgency_runtime.ice_heist_instances.is_empty());

        // Drop corruption stage to 0 and move FOB elsewhere -> prunes MiningOpAmbush, IceRefineryHeist, PirateLaunderingFacility, and ice_heist_instances >= 860_001, but keeps CorporateOutpostRaid & PirateStronghold
        snap.system_states.get_mut(&sys_id).unwrap().insurgency_corruption_stage = 0;
        snap.insurgency_runtime.campaigns.get_mut(&30).unwrap().fob_solar_system_id = 30_000_144;
        prune_ineligible_insurgency_sites_in_snap(snap, sys_id);
        assert!(snap
            .insurgency_runtime
            .insurgency_instances
            .iter()
            .any(|s| s.solar_system_id == sys_id && s.site_type == "PirateStronghold"));
        assert!(snap
            .insurgency_runtime
            .insurgency_instances
            .iter()
            .any(|s| s.solar_system_id == sys_id && s.site_type == "CorporateOutpostRaid"));
        assert!(!snap
            .insurgency_runtime
            .insurgency_instances
            .iter()
            .any(|s| s.solar_system_id == sys_id && (s.site_type == "MiningOpAmbush" || s.site_type == "IceRefineryHeist" || s.site_type == "PirateLaunderingFacility")));
        assert!(snap
            .insurgency_runtime
            .ice_heist_instances
            .iter()
            .all(|h| h.solar_system_id != sys_id || h.instance_id < 860_001));
    });
}
