//! Deep branch-coverage and mutation-killing unit tests for `pve_incursion_lifecycle`
//! and `pve_incursion_sites`.

use crate::router::ServerContext;
use crate::services::pve_incursion_lifecycle::{
    build_incursion_npc_combat_state, eligible_incursion_constellations,
};
use crate::services::pve_incursion_sites::{
    derive_incursion_site_position, get_sansha_encounter_composition,
    incursion_overstack_ratio, incursion_site_payout_band, process_incursion_site_completion_and_payout,
    seed_incursion_for_constellation,
};
use crate::services::pve_special_sites::with_pve_special_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::{IskCents, WorldRepository};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn build_ctx() -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_810_000_000)));
    Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db))
}

pub(super) fn prepare_site_ready_for_completion(
    ctx: &ServerContext,
    cid: u32,
    site_id: &str,
) -> (SolarSystemId, [f64; 3]) {
    with_pve_special_reg(ctx, |reg| {
        if let Some(wm) = reg.incursion_site_waves.get_mut(site_id) {
            wm.current_wave = wm.total_waves;
        }
        let inc = reg.incursions.get_mut(&cid).unwrap();
        let site = inc.sites.iter_mut().find(|s| s.site_id == site_id).unwrap();
        for e in &mut site.escort_spawns {
            e.destroyed = true;
        }
        site.boss_destroyed = true;
        site.killer_char_id = None;
        site.killer_fleet_id = None;
        (site.solar_system_id, site.position)
    })
}

#[test]
fn deep_incursion_site_payout_bands_overstack_ratios_and_sansha_compositions() {
    let ctx = build_ctx();
    let hs_sys: SolarSystemId = 30_000_142; // Jita (high-sec >= 0.45)
    let ls_sys: SolarSystemId = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap();

    assert_eq!(
        incursion_site_payout_band(&ctx.sde, hs_sys, "Staging"),
        (1, 5, 3_500_000, 400)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, ls_sys, "Staging"),
        (1, 5, 3_500_000, 400)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, hs_sys, "Vanguard"),
        (5, 10, 10_395_000, 1_400)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, ls_sys, "Vanguard"),
        (5, 15, 15_000_000, 2_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, hs_sys, "Assault"),
        (1, 20, 18_200_000, 3_500)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, ls_sys, "Assault"),
        (1, 30, 26_000_000, 5_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, hs_sys, "Headquarters"),
        (20, 40, 31_500_000, 7_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, ls_sys, "Headquarters"),
        (20, 60, 45_000_000, 10_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, hs_sys, "Mothership"),
        (40, 80, 63_000_000, 14_000)
    );
    assert_eq!(
        incursion_site_payout_band(&ctx.sde, ls_sys, "Mothership"),
        (40, 120, 90_000_000, 20_000)
    );

    assert_eq!(incursion_overstack_ratio(0), 1.0);
    assert_eq!(incursion_overstack_ratio(1), 0.925);
    assert_eq!(incursion_overstack_ratio(2), 0.845);
    assert_eq!(incursion_overstack_ratio(3), 0.76);
    assert_eq!(incursion_overstack_ratio(4), 0.0);
    assert_eq!(incursion_overstack_ratio(99), 0.0);

    let (ms_boss, ms_esc) = get_sansha_encounter_composition(10_000, 5158, "Mothership");
    assert_eq!(ms_boss, 2192);
    assert_eq!(ms_esc.len(), 9);
    let (hq1_boss, hq1_esc) = get_sansha_encounter_composition(10_000, 5175, "Headquarters");
    assert_eq!(hq1_boss, 2845);
    assert_eq!(hq1_esc.len(), 6);
    let (hq2_boss, _) = get_sansha_encounter_composition(10_000, 5171, "Headquarters");
    assert_eq!(hq2_boss, 2935);
    let (as1_boss, as1_esc) = get_sansha_encounter_composition(10_000, 5168, "Assault");
    assert_eq!(as1_boss, 3492);
    assert_eq!(as1_esc.len(), 4);
    let (as2_boss, _) = get_sansha_encounter_composition(10_000, 5167, "Assault");
    assert_eq!(as2_boss, 2933);
    let (vg1_boss, vg1_esc) = get_sansha_encounter_composition(10_000, 5165, "Vanguard");
    assert_eq!(vg1_boss, 2933);
    assert_eq!(vg1_esc.len(), 3);
    let (vg2_boss, _) = get_sansha_encounter_composition(10_000, 5166, "Vanguard");
    assert_eq!(vg2_boss, 3492);
    let (stg_boss, stg_esc) = get_sansha_encounter_composition(10_000, 5164, "Staging");
    assert_eq!(stg_boss, 3492);
    assert_eq!(stg_esc.len(), 3);

    for idx in 0..4 {
        let p = derive_incursion_site_position(&ctx.sde, hs_sys, idx);
        assert!(p.length() > 1_000.0);
    }
    let fallback_pos = derive_incursion_site_position(&ctx.sde, 99_999_999, 0);
    assert_eq!(fallback_pos.to_array(), [100_000_000.0, 50_000_000.0, -80_000_000.0]);

    let ms_cs = build_incursion_npc_combat_state(&ctx.sde, 2192);
    let bs_cs = build_incursion_npc_combat_state(&ctx.sde, 2933);
    let cr_cs = build_incursion_npc_combat_state(&ctx.sde, 2936);
    let fg_cs = build_incursion_npc_combat_state(&ctx.sde, 2909);
    assert!(ms_cs.max_shield_hp >= bs_cs.max_shield_hp);
    assert!(bs_cs.max_shield_hp >= cr_cs.max_shield_hp);
    assert!(cr_cs.max_shield_hp >= fg_cs.max_shield_hp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_fleet_size_payout_scaling_undersized_sweet_spot_overstack_and_zero_cap() {
    let ctx = build_ctx();
    let cid = eligible_incursion_constellations(&ctx)
        .into_iter()
        .find(|&(_, avg_sec)| avg_sec >= 0.5)
        .unwrap()
        .0;
    let vg_site_id = with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.incursions[&cid]
            .sites
            .iter()
            .find(|s| s.tier == "Vanguard")
            .unwrap()
            .site_id
            .clone()
    });

    let (vg_sys, site_pos) = prepare_site_ready_for_completion(&ctx, cid, &vg_site_id);
    let fleet_id = 9101;
    let mut pilot_ids = Vec::new();
    for i in 0..14_i64 {
        let pid = 92_100_001 + i;
        let sid = 1_400_000_001 + i;
        pilot_ids.push(pid);
        ctx.db
            .provision_gm_account(&format!("inc_p_{i}"), "pw", pid, &format!("IncPilot{i}"));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&pid) {
                ch.solar_system_id = vg_sys;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.fleet_id = Some(fleet_id);
                ch.balance = IskCents::ZERO;
                ch.loyalty_points.clear();
            }
            snap.item_positions.insert(sid, site_pos);
        });
    }

    // 1. Under-sized fleet (2 of 5 min_pilots -> scale = 0.4)
    let res_under = process_incursion_site_completion_and_payout(
        &ctx,
        Some(pilot_ids[0]),
        Some(fleet_id),
        vg_sys,
        &vg_site_id,
        Some(&pilot_ids[..2]),
    )
    .await
    .expect("undersized Vanguard completion succeeds");
    assert_eq!(res_under.fleet_size, 2);
    assert!((res_under.fleet_scale_factor - 0.4).abs() < 1e-6);
    assert_eq!(res_under.isk_per_pilot, 4_158_000);
    assert_eq!(res_under.concord_lp_per_pilot, 560);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&pilot_ids[0]].balance,
            IskCents::from_isk(4_158_000)
        );
        assert_eq!(snap.characters[&pilot_ids[0]].loyalty_points[&1_000_125], 560);
    });

    // Clear respawned boss ball in space before next completion
    let respawned_boss = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&cid]
            .sites
            .iter()
            .find(|s| s.site_id == vg_site_id)
            .unwrap()
            .boss_npc_ball_id
    });
    let _ = ctx.systems.get_or_spawn(vg_sys).remove_ball(respawned_boss).await;

    // 2. Optimal sweet-spot fleet (5 pilots -> scale = 1.0)
    prepare_site_ready_for_completion(&ctx, cid, &vg_site_id);
    let res_opt = process_incursion_site_completion_and_payout(
        &ctx,
        Some(pilot_ids[0]),
        Some(fleet_id),
        vg_sys,
        &vg_site_id,
        Some(&pilot_ids[..5]),
    )
    .await
    .expect("optimal Vanguard completion succeeds");
    assert_eq!(res_opt.fleet_size, 5);
    assert!((res_opt.fleet_scale_factor - 1.0).abs() < 1e-6);
    assert_eq!(res_opt.isk_per_pilot, 10_395_000);
    assert_eq!(res_opt.concord_lp_per_pilot, 1_400);

    // 3. Overstacked fleet by 1 (11 pilots, max_opt = 10 -> scale = 0.925)
    prepare_site_ready_for_completion(&ctx, cid, &vg_site_id);
    let res_over1 = process_incursion_site_completion_and_payout(
        &ctx,
        Some(pilot_ids[0]),
        Some(fleet_id),
        vg_sys,
        &vg_site_id,
        Some(&pilot_ids[..11]),
    )
    .await
    .expect("overstack+1 Vanguard completion succeeds");
    assert_eq!(res_over1.fleet_size, 11);
    assert!((res_over1.fleet_scale_factor - 0.925).abs() < 1e-6);
    assert_eq!(res_over1.isk_per_pilot, 9_615_375);
    assert_eq!(res_over1.concord_lp_per_pilot, 1_295);

    // 4. Overstacked beyond hard cap (14 pilots, over = 4 -> scale = 0.0, zero payout)
    prepare_site_ready_for_completion(&ctx, cid, &vg_site_id);
    let res_over4 = process_incursion_site_completion_and_payout(
        &ctx,
        Some(pilot_ids[0]),
        Some(fleet_id),
        vg_sys,
        &vg_site_id,
        Some(&pilot_ids[..14]),
    )
    .await
    .expect("overstack+4 Vanguard completion succeeds with 0 payout");
    assert_eq!(res_over4.fleet_size, 14);
    assert_eq!(res_over4.fleet_scale_factor, 0.0);
    assert_eq!(res_over4.isk_per_pilot, 0);
    assert_eq!(res_over4.concord_lp_per_pilot, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incursion_wave_boss_threshold_offsets_effects_and_lifecycle_respawn_kill_mutants() {
    use crate::services::pve_incursion_lifecycle::{
        advance_incursion_lifecycles, advance_incursion_wave_on_escort_kill,
        compute_incursion_system_effects, compute_incursion_system_effects_in_snap,
        INCURSION_EFFECT_BEACON_ASSAULT, INCURSION_EFFECT_BEACON_HQ,
        INCURSION_EFFECT_BEACON_VANGUARD, INCURSION_RESPAWN_CADENCE_SEC,
        TARGET_ACTIVE_INCURSIONS,
    };
    use eve_db::{IncursionLifecycleMeta, IncursionSiteWaveMeta};
    use glam::DVec3;

    // 1. advance_incursion_wave_on_escort_kill: multi-wave threshold (>= total_waves vs <),
    //    exact position + offset vectors (+ vs - / *), and compute_incursion_system_effects_in_snap.
    let ctx = build_ctx();
    let eligible = eligible_incursion_constellations(&ctx);
    assert!(
        eligible.len() >= TARGET_ACTIVE_INCURSIONS + 1,
        "canonical SDE must have at least {} eligible incursion constellations",
        TARGET_ACTIVE_INCURSIONS + 1
    );
    let cid = eligible[0].0;
    let site_custom_pos = [120_000_000.0, -45_000_000.0, 75_000_000.0];

    let (vg_site_id, vg_sys, boss_ball_id, boss_type_id, hq_sys, assault_sys, staging_sys) =
        with_pve_special_reg(&ctx, |reg| {
            reg.initialized = true;
            seed_incursion_for_constellation(&ctx, reg, cid);
            let inc = reg.incursions.get_mut(&cid).unwrap();
            let hq = inc.hq_solar_system_id;
            let assault = inc.assault_solar_system_ids[0];
            let staging = inc.staging_solar_system_id;
            let site = inc
                .sites
                .iter_mut()
                .find(|s| s.tier == "Vanguard")
                .expect("Vanguard site");
            site.locked_until_low_influence = false;
            site.position = site_custom_pos;
            for e in &mut site.escort_spawns {
                e.destroyed = true;
            }
            let sid = site.site_id.clone();
            let sys = site.solar_system_id;
            let bid = site.boss_npc_ball_id;
            let btid = site.boss_type_id;
            reg.incursion_site_waves.insert(
                sid.clone(),
                IncursionSiteWaveMeta {
                    current_wave: 1,
                    total_waves: 3,
                },
            );
            (sid, sys, bid, btid, hq, assault, staging)
        });

    // Check compute_incursion_system_effects_in_snap returns Some (not None / not Default::default())
    // across HQ, Assault, and Vanguard/Staging systems, and None for unaffected Jita.
    ctx.db.with_snapshot(|snap| {
        let hq_eff = compute_incursion_system_effects_in_snap(&ctx.sde, snap, hq_sys)
            .expect("HQ system must have active effects");
        assert_eq!(hq_eff.constellation_id, cid);
        assert_eq!(hq_eff.effect_type_id, INCURSION_EFFECT_BEACON_HQ);
        assert!(hq_eff.influence > 0.0);
        assert!((hq_eff.shield_penalty - 0.50 * hq_eff.influence).abs() < 1e-9);
        assert!((hq_eff.armor_penalty - 0.50 * hq_eff.influence).abs() < 1e-9);
        assert!(hq_eff.cyno_jammed);

        let as_eff = compute_incursion_system_effects_in_snap(&ctx.sde, snap, assault_sys)
            .expect("Assault system must have active effects");
        assert_eq!(as_eff.effect_type_id, INCURSION_EFFECT_BEACON_ASSAULT);
        assert!((as_eff.damage_penalty - 0.25 * as_eff.influence).abs() < 1e-9);

        let stg_eff = compute_incursion_system_effects_in_snap(&ctx.sde, snap, staging_sys)
            .expect("Staging system must have active effects");
        assert_eq!(stg_eff.effect_type_id, INCURSION_EFFECT_BEACON_VANGUARD);
        assert!((stg_eff.lp_penalty - 0.10 * stg_eff.influence).abs() < 1e-9);

        assert!(
            compute_incursion_system_effects_in_snap(&ctx.sde, snap, 30_000_142).is_none(),
            "Jita must not have incursion effects"
        );
    });
    assert!(compute_incursion_system_effects(&ctx, hq_sys).is_some());

    // Advance Wave 1 -> Wave 2 (2 < total_waves=3): reinforcements spawn at site_custom_pos + offset,
    // and boss MUST NOT spawn yet (kills `>= -> <` on Wave 2 and `-> ()`).
    advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, vg_sys, &vg_site_id).await;

    let wave2_escorts = with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursion_site_waves[&vg_site_id].current_wave, 2);
        let site = reg.incursions[&cid]
            .sites
            .iter()
            .find(|s| s.site_id == vg_site_id)
            .unwrap();
        site.escort_spawns
            .iter()
            .filter(|e| !e.destroyed)
            .map(|e| (e.npc_ball_id, e.offset))
            .collect::<Vec<_>>()
    });
    assert!(!wave2_escorts.is_empty(), "wave 2 reinforcements must exist");

    let sys_h = ctx.systems.get_or_spawn(vg_sys);
    assert!(
        sys_h.get_ball_snapshot(boss_ball_id).await.unwrap().is_none(),
        "boss must NOT spawn on non-final wave 2"
    );
    let base_vec = DVec3::from_array(site_custom_pos);
    for (ebid, off) in &wave2_escorts {
        let ball = sys_h
            .get_ball_snapshot(*ebid)
            .await
            .unwrap()
            .expect("wave 2 reinforcement ball must be spawned");
        let expected_pos = base_vec + DVec3::from_array(*off);
        assert!(
            (ball.1.position - expected_pos).length() < 1e-3,
            "wave 2 escort {ebid} position {:?} must equal site_pos + offset {:?}",
            ball.1.position,
            expected_pos
        );
    }

    // Mark wave 2 escorts destroyed and advance Wave 2 -> Wave 3 (final wave 3 >= 3):
    // Boss MUST now spawn at exact site_custom_pos (also kills `>= -> <` on final wave).
    with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&cid).unwrap();
        let site = inc.sites.iter_mut().find(|s| s.site_id == vg_site_id).unwrap();
        for e in &mut site.escort_spawns {
            e.destroyed = true;
        }
    });
    advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, vg_sys, &vg_site_id).await;
    let boss_ball = sys_h
        .get_ball_snapshot(boss_ball_id)
        .await
        .unwrap()
        .expect("boss MUST spawn on final wave 3");
    assert_eq!(boss_ball.1.type_id, boss_type_id);
    assert!((boss_ball.1.position - base_vec).length() < 1e-3);

    // 2. advance_incursion_lifecycles respawn branch (`!esc.destroyed` & `esc_pos = s.pos + esc.offset`):
    let ctx2 = build_ctx();
    let ended_cid = eligible[0].0;
    with_pve_special_reg(&ctx2, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx2, reg, ended_cid);
        let now_sec = crate::services::pve_special_sites::spec_now_sec(reg);
        let old = now_sec.saturating_sub(INCURSION_RESPAWN_CADENCE_SEC + 10_000);
        reg.incursions.get_mut(&ended_cid).unwrap().state = "Ended".into();
        reg.incursion_lifecycle.insert(
            ended_cid,
            IncursionLifecycleMeta {
                spawned_at_sec: old.saturating_sub(50_000),
                last_site_completed_sec: old.saturating_sub(20_000),
                last_regen_sec: old.saturating_sub(20_000),
                withdraw_until_sec: Some(old),
                ended_at_sec: Some(old),
            },
        );
    });

    advance_incursion_lifecycles(&ctx2);
    // Allow tokio::spawn task inside advance_incursion_lifecycles to spawn balls
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }

    let expected_escorts = with_pve_special_reg(&ctx2, |reg| {
        let mut out = Vec::new();
        for (&resp_cid, inc) in &reg.incursions {
            if resp_cid == ended_cid || inc.state == "Ended" {
                continue;
            }
            for s in &inc.sites {
                if !s.locked_until_low_influence {
                    for esc in &s.escort_spawns {
                        assert!(!esc.destroyed);
                        out.push((s.solar_system_id, esc.npc_ball_id, s.position, esc.offset));
                    }
                }
            }
        }
        out
    });
    assert!(
        !expected_escorts.is_empty(),
        "advance_incursion_lifecycles must respawn active incursions with unlocked escorts"
    );
    for (sys_id, ebid, spos, eoff) in expected_escorts.into_iter().take(6) {
        let ball = ctx2
            .systems
            .get_or_spawn(sys_id)
            .get_ball_snapshot(ebid)
            .await
            .unwrap()
            .expect("undestroyed escort of respawned incursion must be spawned in space");
        let expected_pos = DVec3::from_array(spos) + DVec3::from_array(eoff);
        assert!(
            (ball.1.position - expected_pos).length() < 1e-3,
            "respawned escort {ebid} position {:?} must equal site_pos + offset {:?}",
            ball.1.position,
            expected_pos
        );
    }
}
