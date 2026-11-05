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
