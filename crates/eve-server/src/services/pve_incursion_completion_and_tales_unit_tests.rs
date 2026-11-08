//! Semantic unit tests split from `pve_incursion_lifecycle_deep_extra_unit_tests`.

use super::pve_incursion_lifecycle_deep_extra_unit_tests::*;
use crate::services::pve_incursion_lifecycle::{
    eligible_incursion_constellations,
    sync_incursion_system_effect_beacons, INCURSION_EFFECT_BEACON_ASSAULT,
    INCURSION_EFFECT_BEACON_HQ, INCURSION_EFFECT_BEACON_VANGUARD,
};
use crate::services::pve_incursion_sites::{
    handle_incursion_rpc, mark_incursion_boss_destroyed,
    on_incursion_boss_destroyed, process_incursion_site_completion_and_payout,
    seed_incursion_for_constellation, INCURSION_TALE_TEMPLATE_ID,
};
use crate::services::pve_special_sites::with_pve_special_reg;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

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
                s.position,
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
