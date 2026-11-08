//! Deep branch-coverage and mutation-killing unit tests for Sovereignty ADM/vulnerability windows,
//! Entosis prechecks, Mercenary Den reinforcement, Faction Warfare occupancy/VP/LP, and Insurgencies.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_services::{
    add_victory_points, award_fw_kill_lp, award_fw_kill_lp_split,
    calculate_capture_speed_multiplier, calculate_fw_lp_reward, calculate_militia_rank,
    capture_warzone_system, compute_all_frontline_statuses, compute_fw_kill_base_lp,
    compute_system_frontline_status, fw_opposing_faction, is_opposing_militia,
    process_pending_fw_captures_on_downtime, update_system_advantage, FACTION_AMARR,
    FACTION_CALDARI, FACTION_GALLENTE, FACTION_MINMATAR, STATUS_COMMAND, STATUS_FRONTLINE,
    STATUS_REARGUARD,
};
use crate::services::insurgency_services::{
    add_system_corruption, add_system_suppression, end_insurgency_campaign,
    get_campaign_phase_and_timer, init_insurgency_campaign, insurgency_stage,
    insurgency_system_corruption_stage, is_stronghold_invulnerable_in_db, FACTION_ANGEL_CARTEL,
    FACTION_GURISTAS, PHASE_ACTIVE, PHASE_FORECASTING, PHASE_WINDDOWN,
    PIRATE_FOB_STRUCTURE_BASE_ID,
};
use crate::services::sov_gameplay::{
    apply_adm_scaled_vulnerability_window_in_snap, apply_mercenary_den_weapon_damage,
    compute_adm_vulnerability_seconds, is_claimable_nullsec_system,
    is_hub_within_vulnerability_window_in_snap, precheck_entosis_activation,
    register_entosis_capture_target, MERC_DEN_BASE_HP, MERC_DEN_REINFORCEMENT_SECS,
    MERC_DEN_VULNERABILITY_WINDOW_SECS, TYPE_MERCENARY_DEN,
};
use crate::services::sov_services::{
    calculate_adm, claim_system_sovereignty, is_hub_hacked, record_hub_hack,
    scale_entosis_capture_seconds, set_skyhook_hackable, set_skyhook_reagents,
    TYPE_SOVEREIGNTY_HUB,
};
use eve_db::{
    CorporationRecord, FwSystemOccupationRecord, SystemDynamicState, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn make_ctx() -> ServerContext {
    let sde = SHARED_SDE
        .get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone();
    ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[tokio::test]
async fn sov_adm_vulnerability_windows_entosis_and_mercden_reinforcement_kill_mutations() {
    assert_eq!(calculate_adm(0, 0, 0), 1.0);
    assert!((calculate_adm(3, 2, 1) - 4.3).abs() < 1e-9);
    assert_eq!(calculate_adm(5, 5, 5), 6.0);
    assert_eq!(compute_adm_vulnerability_seconds(1.0), 64_800);
    assert_eq!(compute_adm_vulnerability_seconds(2.0), 32_400);
    assert_eq!(compute_adm_vulnerability_seconds(3.0), 21_600);
    assert_eq!(compute_adm_vulnerability_seconds(6.0), 10_800);
    assert_eq!(scale_entosis_capture_seconds(240, 2.5), 600);

    let ctx = make_ctx();
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);
    assert!(!is_claimable_nullsec_system(&ctx.sde, 30_000_142));
    assert!(!is_claimable_nullsec_system(&ctx.sde, 31_000_005));
    assert!(!is_claimable_nullsec_system(&ctx.sde, 30_100_000));

    let hub_id = 1_080_100_001_i64;
    let den_id = 1_080_100_002_i64;
    let def_ally = 99_100_001_i64;
    let atk_ally = 99_100_002_i64;
    let atk_corp = 98_100_002_i64;
    let atk_char = 90_100_002_i64;
    let ship_id = 940_500_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            null_sys,
            SystemDynamicState {
                solar_system_id: null_sys,
                sov_alliance_id: Some(def_ally),
                military_index: 5,
                industrial_index: 5,
                strategic_index: 5,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_100_001,
                solar_system_id: null_sys,
                name: "Nullsec Sov Hub".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        // Non-wrapping window centered at 12:00 UTC (43_200s) with 3h duration -> [37_800, 48_600]
        let (s1, e1) = apply_adm_scaled_vulnerability_window_in_snap(snap, hub_id, 12).unwrap();
        assert_eq!((s1, e1), (37_800, 48_600));
        assert!(!is_hub_within_vulnerability_window_in_snap(snap, hub_id, 37_799));
        assert!(is_hub_within_vulnerability_window_in_snap(snap, hub_id, 37_800));
        assert!(is_hub_within_vulnerability_window_in_snap(snap, hub_id, 48_600));
        assert!(!is_hub_within_vulnerability_window_in_snap(snap, hub_id, 48_601));

        // Midnight-wrapping window centered at 23:00 UTC (82_800s) with 3h duration -> [77_400, 1_800]
        let (s2, e2) = apply_adm_scaled_vulnerability_window_in_snap(snap, hub_id, 23).unwrap();
        assert_eq!((s2, e2), (77_400, 1_800));
        assert!(!is_hub_within_vulnerability_window_in_snap(snap, hub_id, 77_399));
        assert!(is_hub_within_vulnerability_window_in_snap(snap, hub_id, 80_000));
        assert!(is_hub_within_vulnerability_window_in_snap(snap, hub_id, 1_800));
        assert!(!is_hub_within_vulnerability_window_in_snap(snap, hub_id, 1_801));

        snap.sov_runtime.adm_vulnerability_windows.insert(hub_id, (0, 0));
        snap.corporations.insert(
            atk_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = atk_corp;
                __corp.alliance_id = None;
                __corp.name = "Attacker Corp".into();
                __corp.ticker = "ATKC".into();
                __corp.ceo_id = atk_char;
                __corp },
        );
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = atk_char;
        ch.user_id = 202;
        ch.name = "Entosis Pilot".into();
        ch.corporation_id = atk_corp;
        ch.alliance_id = None;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = null_sys;
        ch.active_ship_id = ship_id;
        snap.characters.insert(atk_char, ch);
    });

    assert!(!precheck_entosis_activation(&ctx, 999_999, ship_id, atk_char).await.unwrap());
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, ship_id, atk_char)
            .await
            .unwrap_err()
            .code,
        "MustBeInAlliance"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&atk_char).unwrap().alliance_id = Some(def_ally);
    });
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, ship_id, atk_char)
            .await
            .unwrap_err()
            .code,
        "CannotEntosisFriendlyStructure"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&atk_char).unwrap().alliance_id = Some(atk_ally);
    });
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 593,
            position: DVec3::new(300_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        precheck_entosis_activation(&ctx, hub_id, ship_id, atk_char)
            .await
            .unwrap_err()
            .code,
        "TargetOutOfEntosisRange"
    );

    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 593,
            position: DVec3::new(50_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert!(precheck_entosis_activation(&ctx, hub_id, ship_id, atk_char).await.unwrap());
    register_entosis_capture_target(&ctx, hub_id, ship_id, atk_char, ctx.now_unix() + 300);
    assert!(ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress.contains_key(&hub_id)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: atk_corp,
                solar_system_id: null_sys,
                name: "Test Merc Den".into(),
                state: "ShieldVulnerable".into(),
                shield_hp: MERC_DEN_BASE_HP,
                armor_hp: 0.0,
                hull_hp: MERC_DEN_BASE_HP,
                position: [10_000.0, 0.0, 0.0],
                ..Default::default()
            },
        );
    });
    let hit1 = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP + 100.0)
        .await
        .unwrap();
    assert_eq!(hit1.dict_get("state").and_then(|v| v.as_str()), Some("HullReinforced"));
    let reinf_until = hit1.dict_get("reinforcedUntil").and_then(|v| v.as_i64()).unwrap() as u64;
    let vuln_end = hit1.dict_get("vulnerabilityEnds").and_then(|v| v.as_i64()).unwrap() as u64;
    assert_eq!(vuln_end - reinf_until, MERC_DEN_VULNERABILITY_WINDOW_SECS);
    assert!(reinf_until >= ctx.now_unix() + MERC_DEN_REINFORCEMENT_SECS - 5);

    assert_eq!(
        apply_mercenary_den_weapon_damage(&ctx, den_id, 500.0)
            .await
            .unwrap_err()
            .code,
        "MercenaryDenReinforced"
    );

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(den_id, (now - 10, now + 3_600, 0));
    });
    let hit2 = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP + 100.0)
        .await
        .unwrap();
    assert_eq!(hit2.dict_get("state").and_then(|v| v.as_str()), Some("Destroyed"));

    set_skyhook_hackable(&ctx, null_sys, 5501, true);
    set_skyhook_reagents(&ctx, 5501, 4_200, 800);
    record_hub_hack(&ctx, null_sys, hub_id, 600);
    assert!(is_hub_hacked(&ctx, hub_id));
    assert_eq!(
        claim_system_sovereignty(&ctx, null_sys, atk_ally, atk_corp, hub_id).unwrap_err().code,
        "SovereigntyHeldByAnotherAlliance"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = None;
    });
    claim_system_sovereignty(&ctx, null_sys, atk_ally, atk_corp, hub_id).unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id),
        Some(atk_ally)
    );
}

#[test]
fn fw_frontline_tiers_advantage_victory_points_capture_and_lp_split_kill_mutations() {
    assert_eq!(fw_opposing_faction(FACTION_CALDARI), Some(FACTION_GALLENTE));
    assert_eq!(fw_opposing_faction(FACTION_GALLENTE), Some(FACTION_CALDARI));
    assert_eq!(fw_opposing_faction(FACTION_AMARR), Some(FACTION_MINMATAR));
    assert_eq!(fw_opposing_faction(FACTION_MINMATAR), Some(FACTION_AMARR));
    assert!(is_opposing_militia(FACTION_GURISTAS, FACTION_GALLENTE));
    assert!(is_opposing_militia(FACTION_ANGEL_CARTEL, FACTION_MINMATAR));
    assert!(!is_opposing_militia(FACTION_CALDARI, FACTION_AMARR));

    assert_eq!(calculate_militia_rank(-1.0), 0);
    assert_eq!(calculate_militia_rank(0.74), 0);
    assert_eq!(calculate_militia_rank(0.75), 1);
    assert_eq!(calculate_militia_rank(1.50), 2);
    assert_eq!(calculate_militia_rank(6.75), 9);
    assert_eq!(calculate_militia_rank(10.0), 9);

    assert_eq!(calculate_fw_lp_reward(1_000, STATUS_FRONTLINE), 1_500);
    assert_eq!(calculate_fw_lp_reward(1_000, STATUS_COMMAND), 1_000);
    assert_eq!(calculate_fw_lp_reward(1_000, STATUS_REARGUARD), 10);

    assert!((calculate_capture_speed_multiplier(60.0, 10.0) - 1.25).abs() < 1e-6);
    assert!((calculate_capture_speed_multiplier(0.0, 100.0) - 0.50).abs() < 1e-6);

    let ctx = make_ctx();
    let sys_id = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations.keys().next().copied())
        .unwrap_or(30_045_324);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: FACTION_CALDARI,
                occupier_faction_id: FACTION_CALDARI,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        let all = compute_all_frontline_statuses(&ctx, snap);
        assert!(all.contains_key(&sys_id));
        let st = compute_system_frontline_status(&ctx, snap, sys_id);
        assert!(matches!(st, STATUS_FRONTLINE | STATUS_COMMAND | STATUS_REARGUARD));
    });

    assert_eq!(update_system_advantage(&ctx, sys_id, 50.0, 10.0), (50.0, 10.0));
    assert_eq!(update_system_advantage(&ctx, sys_id, 90.0, -50.0), (100.0, 0.0));
    assert_eq!(update_system_advantage(&ctx, sys_id, -60.0, 25.0), (40.0, 25.0));

    let (vp1, pct1) = add_victory_points(&ctx, sys_id, 1_500);
    assert_eq!((vp1, pct1), (1_500, 50.0));
    assert_eq!(
        capture_warzone_system(&ctx, sys_id, FACTION_GALLENTE)
            .unwrap_err()
            .code,
        "SystemNotVulnerable"
    );

    let (vp2, pct2) = add_victory_points(&ctx, sys_id, 2_000);
    assert_eq!((vp2, pct2), (3_500, 100.0));
    let (old_occ, new_occ, owner_fac) = capture_warzone_system(&ctx, sys_id, FACTION_GALLENTE).unwrap();
    assert_eq!((old_occ, new_occ, owner_fac), (FACTION_CALDARI, FACTION_GALLENTE, FACTION_CALDARI));
    let flipped = ctx.db.with_snapshot(|s| s.fw_system_occupations[&sys_id].clone());
    assert_eq!(flipped.occupier_faction_id, FACTION_GALLENTE);
    assert_eq!(flipped.victory_points, 0);

    // Downtime pending capture processing
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.pending_downtime_captures.insert(sys_id, FACTION_CALDARI);
    });
    assert_eq!(process_pending_fw_captures_on_downtime(&ctx), 1);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.fw_system_occupations[&sys_id].occupier_faction_id),
        FACTION_CALDARI
    );

    // Kill LP base & split across opposing-militia attackers
    assert_eq!(compute_fw_kill_base_lp(0.0), 0);
    assert_eq!(compute_fw_kill_base_lp(100_000.0), 33);

    let (atk1, atk2, victim) = (90_300_001_i64, 90_300_002_i64, 90_300_003_i64);
    let atk_corp = 98_300_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().unwrap().clone();
        for &(cid, fac) in &[
            (atk1, FACTION_CALDARI),
            (atk2, FACTION_CALDARI),
            (victim, FACTION_GALLENTE),
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.name = format!("FW {cid}");
            ch.corporation_id = atk_corp;
            ch.solar_system_id = sys_id;
            ch.war_faction_id = Some(fac);
            snap.characters.insert(cid, ch);
            snap.fw_runtime.direct_enlisted_chars.insert(cid, fac);
        }
    });
    assert_eq!(award_fw_kill_lp_split(&ctx, &[atk1], victim, sys_id, 670, 50_000_000.0), 0);
    let single_lp = award_fw_kill_lp(&ctx, atk1, sys_id, 10_000_000.0);
    assert!(single_lp > 0);
    let split_lp = award_fw_kill_lp_split(&ctx, &[atk1, atk2], victim, sys_id, 587, 10_000_000.0);
    assert!(split_lp > 0);
}

#[test]
fn insurgency_stages_suppression_dampening_and_stronghold_vulnerability_kill_mutations() {
    assert_eq!(insurgency_stage(0), 0);
    assert_eq!(insurgency_stage(1), 1);
    assert_eq!(insurgency_stage(200), 1);
    assert_eq!(insurgency_stage(201), 2);
    assert_eq!(insurgency_stage(400), 2);
    assert_eq!(insurgency_stage(600), 3);
    assert_eq!(insurgency_stage(800), 4);
    assert_eq!(insurgency_stage(801), 5);
    assert_eq!(insurgency_stage(1_000), 5);

    let ctx = make_ctx();
    let sys_id = 30_002_813_u32;
    let camp_id = 501_i64;
    let stronghold_id = PIRATE_FOB_STRUCTURE_BASE_ID + camp_id;
    let now = ctx.now_unix();
    init_insurgency_campaign(
        &ctx,
        camp_id,
        FACTION_GURISTAS,
        FACTION_CALDARI,
        sys_id,
        vec![sys_id, sys_id + 1, sys_id + 2, sys_id + 3, sys_id + 4],
    );
    ctx.db.with_snapshot(|snap| {
        let camp = &snap.insurgency_runtime.campaigns[&camp_id];
        assert_eq!(get_campaign_phase_and_timer(camp, now + 100).0, PHASE_FORECASTING);
        assert_eq!(
            get_campaign_phase_and_timer(camp, camp.forecasting_end_unix + 10).0,
            PHASE_ACTIVE
        );
        assert_eq!(
            get_campaign_phase_and_timer(camp, camp.active_end_unix + 10).0,
            PHASE_WINDDOWN
        );
    });

    // Raise suppression to Stage 3 (600 pts) -> positive corruption delta (+100) is dampened by 30% to +70
    let (sup_pts, sup_stg) = add_system_suppression(&ctx, camp_id, sys_id, 600);
    assert_eq!((sup_pts, sup_stg), (600, 3));
    let (cor_pts, cor_stg) = add_system_corruption(&ctx, camp_id, sys_id, 100);
    assert_eq!((cor_pts, cor_stg), (70, 1));

    // Raise corruption past Stage 3 (600 pts): +800 * 0.70 = +560 -> 70 + 560 = 630 (Stage 4) -> spawns Ice Heist
    let (cor_pts2, cor_stg2) = add_system_corruption(&ctx, camp_id, sys_id, 800);
    assert_eq!((cor_pts2, cor_stg2), (630, 4));
    assert!(ctx.db.with_snapshot(|s| {
        s.insurgency_runtime
            .ice_heist_instances
            .iter()
            .any(|h| h.solar_system_id == sys_id)
    }));

    // Push suppression to Stage 5 (1000 pts) before corruption reaches Stage 5:
    // Stronghold is invulnerable during Forecasting & before 5 Stage-5 systems, then vulnerable once Active with 5 Stage-5 systems,
    // and corruption is capped at 800 (Stage 4)
    let (sup_pts2, sup_stg2) = add_system_suppression(&ctx, camp_id, sys_id, 400);
    assert_eq!((sup_pts2, sup_stg2), (1_000, 5));
    assert!(is_stronghold_invulnerable_in_db(&ctx.db, sys_id, stronghold_id));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(camp) = snap.insurgency_runtime.campaigns.get_mut(&camp_id) {
            camp.forecasting_end_unix = now;
        }
    });
    // Still invulnerable with only 1 Stage-5 suppressed system
    assert!(is_stronghold_invulnerable_in_db(&ctx.db, sys_id, stronghold_id));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(camp) = snap.insurgency_runtime.campaigns.get_mut(&camp_id) {
            for off in 1..=4_u32 {
                camp.suppression_points.insert(sys_id + off, 1_000);
            }
        }
    });
    assert!(!is_stronghold_invulnerable_in_db(&ctx.db, sys_id, stronghold_id));
    let (cor_capped, cor_stg_capped) = add_system_corruption(&ctx, camp_id, sys_id, 1_000);
    assert_eq!((cor_capped, cor_stg_capped), (800, 4));
    assert_eq!(insurgency_system_corruption_stage(&ctx, sys_id), 4);

    end_insurgency_campaign(&ctx, camp_id);
    assert!(!ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns.contains_key(&camp_id)));
}
