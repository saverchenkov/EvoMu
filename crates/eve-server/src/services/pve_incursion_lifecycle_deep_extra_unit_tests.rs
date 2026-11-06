//! Deep branch-coverage and mutation-killing unit tests for `pve_incursion_lifecycle`
//! and `pve_incursion_sites`.

use crate::router::ServerContext;
use crate::services::pve_incursion_lifecycle::{
    build_incursion_npc_combat_state, eligible_incursion_constellations,
    sync_incursion_system_effect_beacons, INCURSION_EFFECT_BEACON_ASSAULT,
    INCURSION_EFFECT_BEACON_HQ, INCURSION_EFFECT_BEACON_VANGUARD,
};
use crate::services::pve_incursion_sites::{
    derive_incursion_site_position, get_sansha_encounter_composition, handle_incursion_rpc,
    incursion_overstack_ratio, incursion_site_payout_band, mark_incursion_boss_destroyed,
    on_incursion_boss_destroyed, process_incursion_site_completion_and_payout,
    seed_incursion_for_constellation, INCURSION_TALE_TEMPLATE_ID,
};
use crate::services::pve_special_sites::with_pve_special_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::{IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn build_ctx() -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_810_000_000)));
    Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db))
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

fn prepare_site_ready_for_completion(
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
        (site.solar_system_id, site.position.to_array())
    })
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
async fn deep_site_completion_error_gates_killer_attribution_and_mothership_finale() {
    let ctx = build_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let (hq_site_id, ms_site_id, hq_sys) = with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        let inc = &reg.incursions[&cid];
        let hq_id = inc
            .sites
            .iter()
            .find(|s| s.tier == "Headquarters")
            .unwrap()
            .site_id
            .clone();
        let ms_id = inc
            .sites
            .iter()
            .find(|s| s.tier == "Mothership")
            .unwrap()
            .site_id
            .clone();
        (hq_id, ms_id, inc.hq_solar_system_id)
    });
    sync_incursion_system_effect_beacons(&ctx);

    // 1. Unknown site ID -> IncursionSiteNotFound
    assert_eq!(
        process_incursion_site_completion_and_payout(
            &ctx,
            None,
            None,
            hq_sys,
            "INC-UNKNOWN-9999",
            None,
        )
        .await
        .unwrap_err()
        .code,
        "IncursionSiteNotFound"
    );

    // 2. Mothership site locked while influence > 0.0 -> MothershipSiteLocked
    assert_eq!(
        process_incursion_site_completion_and_payout(
            &ctx,
            None,
            None,
            hq_sys,
            &ms_site_id,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "MothershipSiteLocked"
    );

    // 3. HQ site with waves remaining -> IncursionSiteNpcsRemaining
    assert_eq!(
        process_incursion_site_completion_and_payout(
            &ctx,
            None,
            None,
            hq_sys,
            &hq_site_id,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "IncursionSiteNpcsRemaining"
    );

    // 4. Killer attribution mismatch -> KillerAttributionMismatch
    prepare_site_ready_for_completion(&ctx, cid, &hq_site_id);
    with_pve_special_reg(&ctx, |reg| {
        let s = reg
            .incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == hq_site_id)
            .unwrap();
        s.killer_char_id = Some(99_999_001);
        s.killer_fleet_id = Some(5555);
    });
    assert_eq!(
        process_incursion_site_completion_and_payout(
            &ctx,
            Some(99_999_002),
            Some(6666),
            hq_sys,
            &hq_site_id,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "KillerAttributionMismatch"
    );

    // 5. Drive influence to 0.0 via HQ completion -> unlocks Mothership and transitions to Mobilizing
    with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&cid).unwrap();
        inc.influence = 0.25;
        let s = inc.sites.iter_mut().find(|s| s.site_id == hq_site_id).unwrap();
        s.killer_char_id = None;
        s.killer_fleet_id = None;
    });
    let hq_res = process_incursion_site_completion_and_payout(
        &ctx,
        None,
        None,
        hq_sys,
        &hq_site_id,
        None,
    )
    .await
    .expect("HQ site completion unlocks Mothership");
    assert_eq!(hq_res.influence, 0.0);
    assert!(hq_res.mothership_spawned);
    assert_eq!(hq_res.state, "Mobilizing");

    // 6. Complete Mothership encounter -> incursion_ended = true, state = Withdrawing, beacons removed
    let ms_boss_bid = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&cid]
            .sites
            .iter()
            .find(|s| s.site_id == ms_site_id)
            .unwrap()
            .boss_npc_ball_id
    });
    let _ = ctx.systems.get_or_spawn(hq_sys).remove_ball(ms_boss_bid).await;
    prepare_site_ready_for_completion(&ctx, cid, &ms_site_id);
    let ms_res = process_incursion_site_completion_and_payout(
        &ctx,
        None,
        None,
        hq_sys,
        &ms_site_id,
        None,
    )
    .await
    .expect("Mothership completion succeeds");
    assert!(ms_res.incursion_ended);
    assert_eq!(ms_res.state, "Withdrawing");
    let remaining_beacons = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                matches!(
                    i.type_id,
                    INCURSION_EFFECT_BEACON_VANGUARD
                        | INCURSION_EFFECT_BEACON_ASSAULT
                        | INCURSION_EFFECT_BEACON_HQ
                )
            })
            .count()
    });
    assert_eq!(remaining_beacons, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_on_incursion_boss_destroyed_sec_status_gain_and_incursion_tale_rpcs() {
    let ctx = build_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let (stg_site_id, stg_sys, stg_boss_bid, first_esc_bid, stg_pos) =
        with_pve_special_reg(&ctx, |reg| {
            reg.initialized = true;
            seed_incursion_for_constellation(&ctx, reg, cid);
            let s = reg.incursions[&cid]
                .sites
                .iter()
                .find(|s| s.tier == "Staging")
                .unwrap();
            (
                s.site_id.clone(),
                s.solar_system_id,
                s.boss_npc_ball_id,
                s.escort_spawns[0].npc_ball_id,
                s.position.to_array(),
            )
        });

    let char_id: i64 = 92_500_001;
    let ship_id: i64 = 1_500_000_001;
    ctx.db
        .provision_gm_account("sec_pilot", "pw", char_id, "SecPilot");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = stg_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
            ch.security_status = 1.0;
        }
        snap.item_positions.insert(ship_id, stg_pos);
    });

    // Cannot mark boss destroyed while waves/escorts are still pending
    assert!(!mark_incursion_boss_destroyed(&ctx, stg_sys, stg_boss_bid));

    // Escort kill awards security status gain to killer (< 5.0 cap)
    let esc_out = on_incursion_boss_destroyed(&ctx, stg_sys, first_esc_bid, Some(char_id)).await;
    assert!(esc_out.is_none());
    let new_sec = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(new_sec > 1.0);

    // Clear all waves & escorts, then mark_incursion_boss_destroyed succeeds and boss kill completes site
    prepare_site_ready_for_completion(&ctx, cid, &stg_site_id);
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == stg_site_id)
            .unwrap()
            .boss_destroyed = false;
    });
    assert!(mark_incursion_boss_destroyed(&ctx, stg_sys, stg_boss_bid));
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == stg_site_id)
            .unwrap()
            .boss_destroyed = false;
    });
    let boss_out = on_incursion_boss_destroyed(&ctx, stg_sys, stg_boss_bid, Some(char_id))
        .await
        .expect("boss kill triggers completion")
        .expect("completion succeeds");
    assert_eq!(boss_out.isk_per_pilot, 3_500_000);
    assert_eq!(boss_out.concord_lp_per_pilot, 400);

    // Verify incursion & taleMgr RPCs
    let mut sess = SessionState::new(88);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    let kwargs = HashMap::new();

    let PyRep::List(lp_rows) =
        handle_incursion_rpc(&ctx, &mut sess, "incursion", "GetIncursionLPLog", &[], &kwargs)
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected list");
    };
    assert_eq!(lp_rows.len(), 1);

    let PyRep::List(synced_by_site) = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "UpdateIncursionLPLog",
        &[PyRep::String(stg_site_id.clone())],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    assert_eq!(synced_by_site.len(), 1);

    let PyRep::List(synced_by_cid) = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "UpdateIncursionLPLog",
        &[PyRep::Integer(cid as i64)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    assert_eq!(synced_by_cid.len(), 1);

    let synced_miss = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "UpdateIncursionLPLog",
        &[PyRep::String("NON-EXISTENT-SITE".into())],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(synced_miss, PyRep::None));

    let PyRep::List(global_tales) =
        handle_incursion_rpc(&ctx, &mut sess, "taleMgr", "GetGlobalWorldEventTales", &[], &kwargs)
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected list");
    };
    assert!(!global_tales.is_empty());

    let PyRep::List(filtered_tales) = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "taleMgr",
        "get_active_tales_by_template",
        &[PyRep::Integer(INCURSION_TALE_TEMPLATE_ID as i64)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    assert!(!filtered_tales.is_empty());
}
