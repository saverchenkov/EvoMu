#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use std::collections::HashMap;

#[tokio::test]
async fn insurgency_services_stages_campaigns_and_rpc_mutations() {
    assert_eq!(insurgency_stage(-10), 0);
    assert_eq!(insurgency_stage(0), 0);
    assert_eq!(insurgency_stage(1), 1);
    assert_eq!(insurgency_stage(200), 1);
    assert_eq!(insurgency_stage(201), 2);
    assert_eq!(insurgency_stage(400), 2);
    assert_eq!(insurgency_stage(401), 3);
    assert_eq!(insurgency_stage(800), 4);
    assert_eq!(insurgency_stage(801), 5);
    assert_eq!(insurgency_stage(2000), 5);

    assert!(are_fw_factions_allied(500_001, 500_003));
    assert!(are_fw_factions_allied(500_002, 500_004));
    assert!(!are_fw_factions_allied(500_001, 500_002));

    assert_eq!(fw_militia_side_key(FACTION_GURISTAS, Some((1, FACTION_GURISTAS))), 10);
    assert_eq!(fw_militia_side_key(500_001, Some((1, FACTION_GURISTAS))), 20);
    assert_eq!(fw_militia_side_key(500_001, None), 1);
    assert_eq!(fw_militia_side_key(500_002, None), 2);

    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let unknown_svc = "other";
    let unknown_method = "Unknown";
    assert!(handle(&ctx, &mut sess, unknown_svc, "GetAllVisibleCampaigns", &[], &kw)
        .await
        .is_none());
    assert!(handle(&ctx, &mut sess, "insurgencySolarsystem", unknown_method, &[], &kw)
        .await
        .is_none());

    let mut unauth = SessionState::default();
    assert!(handle(
        &ctx,
        &mut unauth,
        "insurgencySolarsystem",
        "GetAllVisibleCampaigns",
        &[],
        &kw
    )
    .await
    .unwrap()
    .is_err());

    let camp_id = 88_001_i64;
    let fob_sys = 30045324_u32;
    init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, 500_001, fob_sys, vec![fob_sys]);

    let sh_id = ensure_stronghold_in_space(&ctx, camp_id).await.unwrap();
    assert_eq!(sh_id, PIRATE_FOB_STRUCTURE_BASE_ID + camp_id);
    // In Forecasting phase (and before 5 Stage-5 suppressed systems), FOB/stronghold is invulnerable
    assert!(is_insurgency_stronghold_invulnerable(&ctx, fob_sys, sh_id));

    // Switch campaign into Active phase and reach 5 Stage-5 suppressed systems while leaving fob_sys at 0
    let now = ctx.now_unix();
    with_insurgency_runtime(&ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&camp_id) {
            c.forecasting_end_unix = now.saturating_sub(100);
            c.active_end_unix = now + 86_400;
            c.winddown_end_unix = now + 2 * 86_400;
            for offset in 1..=5_u32 {
                c.suppression_points.insert(fob_sys + offset, 1_000);
            }
        }
    });
    assert!(!is_insurgency_stronghold_invulnerable(&ctx, fob_sys, sh_id));

    let (c_pts, c_stg) = add_system_corruption(&ctx, camp_id, fob_sys, 250);
    assert_eq!(c_pts, 250);
    assert_eq!(c_stg, 2);
    assert_eq!(insurgency_system_corruption_stage(&ctx, fob_sys), 2);

    let (s_pts, s_stg) = add_system_suppression(&ctx, camp_id, fob_sys, 450);
    assert_eq!(s_pts, 450);
    assert_eq!(s_stg, 3);

    let visible = handle(
        &ctx,
        &mut sess,
        "insurgencySolarsystem",
        "GetAllVisibleCampaigns",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!visible.as_list().unwrap().is_empty());

    sess.solar_system_id = Some(fob_sys);
    let local_snap = handle(
        &ctx,
        &mut sess,
        "insurgencySolarsystem",
        "GetLocalCampaignClientSnapshot",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(local_tup) = local_snap else {
        panic!("expected tuple");
    };
    assert_eq!(local_tup[0], PyRep::Integer(i64::from(fob_sys)));
    assert_eq!(local_tup[1].dict_get("campaignID"), Some(&PyRep::Integer(camp_id)));

    let insts = handle(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyInstances",
        &[PyRep::Integer(i64::from(fob_sys))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!insts.as_list().unwrap().is_empty());

    let ih_id = spawn_ice_heist(&ctx, fob_sys);
    assert!(ih_id >= 800_001);
    let ih_insts = handle(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyIceHeistInstances",
        &[PyRep::Integer(i64::from(fob_sys))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!ih_insts.as_list().unwrap().is_empty());

    // Destroy stronghold in Active phase
    let d_info = on_insurgency_stronghold_destroyed_core(&ctx.systems, &ctx.db, fob_sys, sh_id, Some(90_000_001))
        .await
        .unwrap();
    assert_eq!(d_info.campaign_id, camp_id);
    assert_eq!(d_info.new_suppression, 950);
    notify_stronghold_destruction(&ctx.db, Some(&ctx.quasar_gateway), None, None, &d_info);

    end_insurgency_campaign(&ctx, camp_id);
    assert_eq!(insurgency_system_corruption_stage(&ctx, fob_sys), 0);
}
