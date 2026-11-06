//! Extra branch-coverage and mutation-killing unit tests for `fw_services`.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::fw_gameplay::TYPE_FW_IHUB;
use crate::services::fw_services::{
    add_victory_points, award_fw_kill_lp, award_fw_kill_lp_split, calculate_militia_rank,
    capture_warzone_system, complete_fw_dungeon_and_objective, compute_all_frontline_statuses,
    compute_system_frontline_status, donate_solar_system_lp, effective_char_militia_in_snap,
    handle, is_opposing_militia, process_pending_fw_captures_on_downtime,
    prune_and_fold_fw_stat_events_in_snap, record_fw_stat_event_in_snap,
    set_facwar_bunker_vulnerable, update_system_advantage, FACTION_AMARR, FACTION_CALDARI,
    FACTION_GALLENTE, FACTION_MINMATAR, STATUS_COMMAND, STATUS_FRONTLINE, STATUS_REARGUARD,
};
use eve_db::{FwSystemOccupationRecord, SystemDynamicState, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::StargateJump;

async fn call_fw_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let kw = HashMap::new();
    handle(ctx, session, service, method, args, &kw).await
}

#[test]
fn test_frontline_command_rearguard_graph_and_rank_boundaries() {
    for (standing, expected_rank) in [
        (-2.0, 0),
        (0.0, 0),
        (0.749, 0),
        (0.75, 1),
        (1.499, 1),
        (1.50, 2),
        (2.25, 3),
        (3.00, 4),
        (3.75, 5),
        (4.50, 6),
        (5.25, 7),
        (6.00, 8),
        (6.75, 9),
        (9.99, 9),
    ] {
        assert_eq!(calculate_militia_rank(standing), expected_rank);
    }

    assert!(is_opposing_militia(500010, FACTION_GALLENTE));
    assert!(is_opposing_militia(FACTION_CALDARI, 500010));
    assert!(is_opposing_militia(500011, FACTION_AMARR));
    assert!(is_opposing_militia(FACTION_MINMATAR, 500011));
    assert!(!is_opposing_militia(500010, FACTION_MINMATAR));
    assert!(!is_opposing_militia(500011, FACTION_CALDARI));

    // Construct a 4-system linear stargate graph:
    // sys_a (Gallente) <-> sys_b (Caldari) <-> sys_c (Caldari) <-> sys_d (Caldari)
    // plus sys_pirate (500010 - no opposing empire war pair in fw_opposing_faction)
    let mut ctx = test_ctx();
    let sys_a = 30_900_001_u32;
    let sys_b = 30_900_002_u32;
    let sys_c = 30_900_003_u32;
    let sys_d = 30_900_004_u32;
    let sys_pirate = 30_900_005_u32;

    let sde_mut = std::sync::Arc::make_mut(&mut ctx.sde);
    for (idx, (u, v)) in [(sys_a, sys_b), (sys_b, sys_a), (sys_b, sys_c), (sys_c, sys_b), (sys_c, sys_d), (sys_d, sys_c)].into_iter().enumerate() {
        let sg_id = 50_900_001_i64 + idx as i64;
        sde_mut.jumps.insert(
            sg_id,
            StargateJump {
                stargate_id: sg_id,
                from_solar_system_id: u,
                to_solar_system_id: v,
                destination_stargate_id: sg_id + 100,
                position: eve_destiny::DVec3::ZERO,
            },
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
        for (sid, fac) in [
            (sys_a, FACTION_GALLENTE),
            (sys_b, FACTION_CALDARI),
            (sys_c, FACTION_CALDARI),
            (sys_d, FACTION_CALDARI),
            (sys_pirate, 500_010),
        ] {
            snap.fw_system_occupations.insert(
                sid,
                FwSystemOccupationRecord {
                    solar_system_id: sid,
                    owner_faction_id: fac,
                    occupier_faction_id: fac,
                    victory_points: 0,
                    victory_points_threshold: 3_000,
                },
            );
        }
    });

    ctx.db.with_snapshot(|snap| {
        let all = compute_all_frontline_statuses(&ctx, snap);
        assert_eq!(all.get(&sys_a), Some(&STATUS_FRONTLINE));
        assert_eq!(all.get(&sys_b), Some(&STATUS_FRONTLINE));
        assert_eq!(all.get(&sys_c), Some(&STATUS_COMMAND));
        assert_eq!(all.get(&sys_d), Some(&STATUS_REARGUARD));
        assert_eq!(all.get(&sys_pirate), Some(&STATUS_REARGUARD));

        assert_eq!(compute_system_frontline_status(&ctx, snap, sys_a), STATUS_FRONTLINE);
        assert_eq!(compute_system_frontline_status(&ctx, snap, sys_b), STATUS_FRONTLINE);
        assert_eq!(compute_system_frontline_status(&ctx, snap, sys_c), STATUS_COMMAND);
        assert_eq!(compute_system_frontline_status(&ctx, snap, sys_d), STATUS_REARGUARD);
        assert_eq!(compute_system_frontline_status(&ctx, snap, sys_pirate), STATUS_REARGUARD);
        assert_eq!(compute_system_frontline_status(&ctx, snap, 30_000_142), STATUS_REARGUARD);
    });
}

#[test]
fn test_system_capture_downtime_ihub_transfer_and_notifications() {
    let ctx = test_ctx();
    let wz_sys_id = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .keys()
            .next()
            .copied()
            .unwrap_or(30_045_324)
    });

    // Non-warzone add_victory_points returns (0, 0.0) and capture_warzone_system returns Err
    assert_eq!(add_victory_points(&ctx, 30_000_142, 500), (0, 0.0));
    assert!(capture_warzone_system(&ctx, 30_000_142, FACTION_GALLENTE).is_err());

    // Advantage clamping [0.0, 100.0]
    assert_eq!(update_system_advantage(&ctx, wz_sys_id, 250.0, -50.0), (100.0, 0.0));
    assert_eq!(update_system_advantage(&ctx, wz_sys_id, -200.0, 180.0), (0.0, 100.0));

    // Reset warzone system to 0 VP -> capture fails with SystemNotVulnerable (< 100%)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(occ) = snap.fw_system_occupations.get_mut(&wz_sys_id) {
            occ.occupier_faction_id = FACTION_CALDARI;
            occ.victory_points = 0;
            occ.victory_points_threshold = 3_000;
        }
        snap.fw_runtime.system_victory_points.insert(wz_sys_id, 0);
        snap.system_states.insert(
            wz_sys_id,
            SystemDynamicState {
                solar_system_id: wz_sys_id,
                fw_contested_pct: 0.0,
                ..Default::default()
            },
        );
    });
    assert!(capture_warzone_system(&ctx, wz_sys_id, FACTION_GALLENTE).is_err());

    // Add 3,000 VP -> contested reaches 100%, bunker becomes vulnerable
    let (new_vp, contested) = add_victory_points(&ctx, wz_sys_id, 3_000);
    assert_eq!(new_vp, 3_000);
    assert!((contested - 100.0).abs() < 1e-6);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fw_runtime.ihub_vulnerable.get(&wz_sys_id), Some(&true));
        assert!(snap.fw_runtime.ihub_vulnerable_since_unix.contains_key(&wz_sys_id));
    });

    // Capture warzone system -> flips occupier to Gallente, resets VP/contested/advantage, marks bunker non-vulnerable
    let (old_occ, new_occ, _owner) =
        capture_warzone_system(&ctx, wz_sys_id, FACTION_GALLENTE).expect("capture at 100% succeeds");
    assert_eq!(old_occ, FACTION_CALDARI);
    assert_eq!(new_occ, FACTION_GALLENTE);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fw_system_occupations[&wz_sys_id].occupier_faction_id, FACTION_GALLENTE);
        assert_eq!(snap.fw_system_occupations[&wz_sys_id].victory_points, 0);
        assert_eq!(snap.fw_runtime.ihub_vulnerable.get(&wz_sys_id), Some(&false));
        assert!(!snap.fw_runtime.ihub_vulnerable_since_unix.contains_key(&wz_sys_id));
    });

    // Queue a pending downtime capture + seed a TYPE_FW_IHUB structure in wz_sys_id
    let ihub_id = 1_099_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime
            .pending_downtime_captures
            .insert(wz_sys_id, FACTION_CALDARI);
        snap.structures.insert(
            ihub_id,
            UpwellStructureRecord {
                structure_id: ihub_id,
                type_id: TYPE_FW_IHUB,
                owner_corp_id: 1_000_180,
                solar_system_id: wz_sys_id,
                name: "FW IHub".into(),
                state: "HullReinforced".into(),
                position: [0.0; 3],
                shield_hp: 0.0,
                armor_hp: 0.0,
                hull_hp: 100.0,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(process_pending_fw_captures_on_downtime(&ctx), 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fw_system_occupations[&wz_sys_id].occupier_faction_id, FACTION_CALDARI);
        let st = snap.structures.get(&ihub_id).unwrap();
        assert_eq!(st.state, "ShieldVulnerable");
        assert_eq!(st.shield_hp, 5_000.0);
        assert_eq!(st.hull_hp, 8_000.0);
    });

    // Cover complete_fw_dungeon_and_objective, donate_solar_system_lp, and set_facwar_bunker_vulnerable
    set_facwar_bunker_vulnerable(&ctx, wz_sys_id, 90_000_001, true);
    set_facwar_bunker_vulnerable(&ctx, wz_sys_id, 0, false);
    complete_fw_dungeon_and_objective(&ctx, wz_sys_id, FACTION_CALDARI, true, 1, 800_001);
    donate_solar_system_lp(&ctx, wz_sys_id, 1_000, 5_000);
}

#[tokio::test]
async fn test_kill_lp_award_split_rolling_stats_and_uncached_rpcs() {
    let ctx = test_ctx();
    let wz_sys_id = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .keys()
            .next()
            .copied()
            .unwrap_or(30_045_324)
    });
    let killer1 = 90_000_001_i64;
    let killer2 = 90_000_002_i64;
    let victim = 90_300_003_i64;
    let corp_id = 98_300_001_i64;
    let ally_id = 99_300_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // killer1 enlisted via direct_enlisted_chars (Caldari)
        snap.fw_runtime
            .direct_enlisted_chars
            .insert(killer1, FACTION_CALDARI);
        if let Some(c1) = snap.characters.get_mut(&killer1) {
            c1.corporation_id = corp_id;
            c1.alliance_id = Some(ally_id);
        }
        // killer2 enlisted via corp_war_factions (Caldari)
        if let Some(c2) = snap.characters.get_mut(&killer2) {
            c2.corporation_id = corp_id;
            c2.war_faction_id = None;
        }
        snap.fw_runtime
            .corp_war_factions
            .insert(corp_id, FACTION_CALDARI);

        // victim enlisted via character war_faction_id (Gallente)
        let mut v_ch = snap.characters.get(&killer1).unwrap().clone();
        v_ch.character_id = victim;
        v_ch.war_faction_id = Some(FACTION_GALLENTE);
        snap.characters.insert(victim, v_ch);
    });

    ctx.db.with_snapshot(|snap| {
        assert_eq!(effective_char_militia_in_snap(snap, killer1), Some(FACTION_CALDARI));
        assert_eq!(effective_char_militia_in_snap(snap, killer2), Some(FACTION_CALDARI));
        assert_eq!(effective_char_militia_in_snap(snap, victim), Some(FACTION_GALLENTE));
    });

    // Solo kill LP award
    let solo_lp = award_fw_kill_lp(&ctx, killer1, wz_sys_id, 50_000_000.0);
    assert!(solo_lp > 0);

    // Split kill LP award across killer1 and killer2 (with duplicate killer1 and victim in attacker list)
    let split_lp = award_fw_kill_lp_split(
        &ctx,
        &[killer1, killer1, killer2, victim, 0],
        victim,
        wz_sys_id,
        587, // Rifter
        100_000_000.0,
    );
    assert!(split_lp > 0);

    // Non-warzone system returns 0 LP in award_fw_kill_lp_split
    assert_eq!(
        award_fw_kill_lp_split(&ctx, &[killer1], victim, 30_000_142, 587, 100_000_000.0),
        0
    );

    // Rolling stat windows: > 7d folded into historical (including alliance), 2d in last_week, 1h in yesterday
    let base_t = 10_000_000_u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.stat_events.clear();
        snap.fw_runtime.historical_char_totals.clear();
        snap.fw_runtime.historical_corp_totals.clear();
        snap.fw_runtime.historical_fac_totals.clear();
        snap.fw_runtime.historical_ally_totals.clear();

        // Event 1: at base_t (will be > 7 days old at base_t + 8 days)
        record_fw_stat_event_in_snap(snap, base_t, killer1, Some(FACTION_CALDARI), 3, 100);
        // Event 2: at base_t + 6 days (2 days old at base_t + 8 days -> in last_week, not yesterday)
        record_fw_stat_event_in_snap(
            snap,
            base_t + 6 * 86_400,
            killer1,
            Some(FACTION_CALDARI),
            4,
            200,
        );
        // Event 3: at base_t + 8 days - 3600s (1h old -> in yesterday, last_week, and total)
        let now_8d = base_t + 8 * 86_400;
        record_fw_stat_event_in_snap(snap, now_8d - 3_600, killer1, Some(FACTION_CALDARI), 5, 300);
        prune_and_fold_fw_stat_events_in_snap(snap, now_8d);

        assert_eq!(snap.fw_runtime.historical_ally_totals.get(&ally_id), Some(&(3, 100)));
        let stats = snap.fw_character_stats.get(&killer1).unwrap();
        assert_eq!(stats.kills.yesterday, 5);
        assert_eq!(stats.kills.last_week, 9); // 4 + 5
        assert_eq!(stats.kills.total, 12); // 3 + 4 + 5
        assert_eq!(stats.victory_points.yesterday, 300);
        assert_eq!(stats.victory_points.last_week, 500);
        assert_eq!(stats.victory_points.total, 600);
    });

    // Test GetAllWarzonesOccupationStatesUncached backfill when fw_system_occupations is cleared,
    // and GetLocalOccupationState / GetLocalVictoryPointState in a non-warzone solar system (Jita)
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
    });
    let mut sess = auth_session(1, killer1, corp_id, 30_000_142);
    let uncached = call_fw_rpc(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        "GetAllWarzonesOccupationStatesUncached",
        &[],
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(uncached, PyRep::Dict(_)));

    let local_non_wz = call_fw_rpc(
        &ctx,
        &mut sess,
        "fwWarzoneSolarsystem",
        "GetLocalOccupationState",
        &[],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        local_non_wz,
        PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::None])
    );

    let vp_non_wz = call_fw_rpc(
        &ctx,
        &mut sess,
        "fwVictoryPointMgr",
        "GetLocalVictoryPointState",
        &[],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(vp_non_wz, PyRep::None);
}
