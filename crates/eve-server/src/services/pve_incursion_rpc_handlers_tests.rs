//! Semantic unit tests split from `pve_incursion_sites_tests`.

use super::*;
use crate::services::pve_special_sites::with_pve_special_reg;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn test_incursion_rpc_handlers() {
    let ctx = build_test_ctx();
    let mut unauth = SessionState::new(1);
    let err = handle_incursion_rpc(&ctx, &mut unauth, "incursion", "GetIncursionLPLog", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap_err();
    assert!(!err.code.is_empty());

    let char_id = 92_000_100;
    ctx.db
        .provision_gm_account("inc_rpc", "pw", char_id, "IncRpc");
    let uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let mut sess = SessionState::new(2);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);

    // Empty LP log -> empty list for GetIncursionLPLog, PyRep::None for UpdateIncursionLPLog
    let empty_get = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "GetIncursionLPLog",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(empty_get, PyRep::List(ref v) if v.is_empty()));

    let empty_upd = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "UpdateIncursionLPLog",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(empty_upd, PyRep::None);

    // Insert 2 LP log entries and query with filters
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_lp_logs.insert(
            char_id,
            vec![
                IncursionLPLogEntry {
                    log_id: 1,
                    character_id: char_id,
                    site_id: "INC-20000001-5165".into(),
                    tier: "Vanguard".into(),
                    constellation_id: 20_000_001,
                    solar_system_id: 30_000_001,
                    isk_earned: 10_395_000,
                    concord_lp_earned: 1_400,
                    fleet_size: 10,
                    fleet_scale_factor: 1.0,
                    timestamp_filetime: 0,
                },
                IncursionLPLogEntry {
                    log_id: 2,
                    character_id: char_id,
                    site_id: "INC-20000002-5168".into(),
                    tier: "Assault".into(),
                    constellation_id: 20_000_002,
                    solar_system_id: 30_000_002,
                    isk_earned: 18_200_000,
                    concord_lp_earned: 3_500,
                    fleet_size: 15,
                    fleet_scale_factor: 1.0,
                    timestamp_filetime: 133_000_000_000_000_000,
                },
            ],
        );
    });

    let all_logs = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "GetIncursionLPLog",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match all_logs {
        PyRep::List(rows) => assert_eq!(rows.len(), 2),
        other => panic!("expected list, got {other:?}"),
    }

    let by_site = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "UpdateIncursionLPLog",
        &[PyRep::String("INC-20000001-5165".into())],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match by_site {
        PyRep::List(rows) => {
            assert_eq!(rows.len(), 1);
            assert!(rows[0].dict_get("timestamp").and_then(|v| v.as_i64()).unwrap() > 0);
        }
        other => panic!("expected list, got {other:?}"),
    }

    let by_cid = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "UpdateIncursionLPLog",
        &[PyRep::Integer(20_000_002)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match by_cid {
        PyRep::List(rows) => assert_eq!(rows.len(), 1),
        other => panic!("expected list, got {other:?}"),
    }

    // taleMgr RPCs
    let global_tales = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "taleMgr",
        "GetGlobalWorldEventTales",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match global_tales {
        PyRep::List(rows) => assert!(!rows.is_empty()),
        other => panic!("expected list, got {other:?}"),
    }

    let tpl_tales = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "taleMgr",
        "get_active_tales_by_template",
        &[PyRep::Integer(INCURSION_TALE_TEMPLATE_ID as i64)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match tpl_tales {
        PyRep::List(rows) => assert_eq!(rows.len(), 3),
        other => panic!("expected 3 incursion tales, got {other:?}"),
    }

    assert!(
        handle_incursion_rpc(&ctx, &mut sess, "", "", &[], &HashMap::new())
            .await
            .is_none()
    );
}

#[tokio::test]
async fn test_incursion_rpc_auth_guards_and_template_filters() {
    let ctx = build_test_ctx();
    let mut unauth = SessionState::new(11);

    for (svc, method) in [
        ("incursion", "UpdateIncursionLPLog"),
        ("taleMgr", "GetGlobalWorldEventTales"),
        ("taleMgr", "get_active_tales_by_template"),
    ] {
        let err = handle_incursion_rpc(&ctx, &mut unauth, svc, method, &[], &HashMap::new())
            .await
            .expect("handled")
            .unwrap_err();
        assert!(!err.code.is_empty(), "{svc}.{method} must require auth");
    }

    let char_id = 92_000_200;
    ctx.db.provision_gm_account("inc_rpc_auth", "pw", char_id, "IncRpcAuth");
    let uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let mut sess = SessionState::new(12);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);

    // Mismatching / omitted template ID returns empty list
    let empty_tpl = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "taleMgr",
        "get_active_tales_by_template",
        &[PyRep::Integer(999_999)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(empty_tpl, PyRep::List(ref v) if v.is_empty()));

    let default_tpl = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "taleMgr",
        "get_active_tales_by_template",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(default_tpl, PyRep::List(ref v) if v.is_empty()));
}

#[tokio::test]
async fn test_incursion_completion_errors_solo_offgrid_boss_first_and_lockout() {
    let ctx = build_test_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let inc = with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.incursions[&cid].clone()
    });
    let stg_site = inc.sites.iter().find(|s| s.tier == "Staging").unwrap().clone();
    let ms_site = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap().clone();

    // 1. Whitespace site_id fails cleanly
    let empty_err = process_incursion_site_completion_and_payout(
        &ctx,
        None,
        None,
        stg_site.solar_system_id,
        "   \t",
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(empty_err.code, "IncursionSiteNotFound");

    // 2. Withdrawing / Ended incursion sites are skipped cleanly
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.get_mut(&cid).unwrap().state = "Withdrawing".into();
    });
    let wd_err = process_incursion_site_completion_and_payout(
        &ctx,
        None,
        None,
        stg_site.solar_system_id,
        &stg_site.site_id,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(wd_err.code, "IncursionSiteNotFound");
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.get_mut(&cid).unwrap().state = "Establishing".into();
    });

    // 3. mark_incursion_boss_destroyed false branches: locked mothership, pending waves, live escorts; then solar_system_id=0 wildcard
    assert!(!mark_incursion_boss_destroyed(
        &ctx,
        ms_site.solar_system_id,
        ms_site.boss_npc_ball_id
    ));
    assert!(!mark_incursion_boss_destroyed(
        &ctx,
        stg_site.solar_system_id,
        stg_site.boss_npc_ball_id
    ));

    // Advance staging wave to total_waves=2, keep escorts alive -> escort remaining error
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves
            .get_mut(&stg_site.site_id)
            .unwrap()
            .current_wave = 2;
    });
    assert!(!mark_incursion_boss_destroyed(
        &ctx,
        stg_site.solar_system_id,
        stg_site.boss_npc_ball_id
    ));
    let esc_err = process_incursion_site_completion_and_payout(
        &ctx,
        None,
        None,
        stg_site.solar_system_id,
        &stg_site.site_id,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(esc_err.code, "IncursionSiteNpcsRemaining");

    // Clear escorts, leave boss_destroyed == false -> boss not destroyed error
    with_pve_special_reg(&ctx, |reg| {
        let s = reg
            .incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == stg_site.site_id)
            .unwrap();
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
    });
    let boss_not_dead = process_incursion_site_completion_and_payout(
        &ctx,
        None,
        None,
        stg_site.solar_system_id,
        &stg_site.site_id,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(boss_not_dead.code, "IncursionSiteNpcsRemaining");

    // Wildcard solar_system_id == 0 marks boss destroyed
    assert!(mark_incursion_boss_destroyed(&ctx, 0, stg_site.boss_npc_ball_id));

    // If boss ball is STILL in space ballpark -> still alive error!
    let sys_handle = ctx.systems.get_or_spawn(stg_site.solar_system_id);
    sys_handle
        .spawn_ball(make_site_npc_ball(
            stg_site.boss_npc_ball_id,
            stg_site.boss_type_id,
            DVec3::from_array(stg_site.position),
        ))
        .await
        .unwrap();
    let ball_alive_err = process_incursion_site_completion_and_payout(
        &ctx,
        None,
        None,
        stg_site.solar_system_id,
        &stg_site.site_id,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(ball_alive_err.code, "IncursionSiteNpcsRemaining");
    sys_handle.remove_ball(stg_site.boss_npc_ball_id).await.unwrap();

    // 4. Solo non-fleet killer at exact 500km boundary via item_positions + third-party bystander excluded + dungeon_id string suffix match
    let solo_killer = 92_100_001i64;
    let bystander = 92_100_002i64;
    let site_pos = DVec3::from_array(stg_site.position);
    insert_in_space_pilot(
        &ctx,
        solo_killer,
        stg_site.solar_system_id,
        None,
        site_pos + DVec3::new(500_000.0, 0.0, 0.0),
        0.0,
    );
    insert_in_space_pilot(
        &ctx,
        bystander,
        stg_site.solar_system_id,
        None,
        site_pos,
        0.0,
    );
    let did_str = stg_site.dungeon_id.to_string();
    let solo_res = process_incursion_site_completion_and_payout(
        &ctx,
        Some(solo_killer),
        None,
        stg_site.solar_system_id,
        &did_str,
        Some(&[]), // empty slice uses caller_char_id
    )
    .await
    .unwrap();
    assert_eq!(solo_res.fleet_size, 1);
    assert_eq!(solo_res.isk_per_pilot, stg_site.base_isk);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&solo_killer].balance,
            IskCents::from_isk(1_000_000 + stg_site.base_isk)
        );
        assert_eq!(
            snap.characters[&bystander].balance,
            IskCents::from_isk(1_000_000)
        );
    });

    // 5. Off-grid pilot at 500_001m -> NF-INC-OFFGRID-KILL-01 advances site cleanly with 0 payout!
    let next_boss_bid = with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves
            .get_mut(&stg_site.site_id)
            .unwrap()
            .current_wave = 2;
        let s = reg
            .incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == stg_site.site_id)
            .unwrap();
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
        s.boss_destroyed = true;
        s.boss_npc_ball_id
    });
    let _ = sys_handle.remove_ball(next_boss_bid).await;
    let offgrid_killer = 92_100_003i64;
    insert_in_space_pilot(
        &ctx,
        offgrid_killer,
        stg_site.solar_system_id,
        None,
        site_pos + DVec3::new(500_001.0, 0.0, 0.0),
        0.0,
    );
    let offgrid_res = process_incursion_site_completion_and_payout(
        &ctx,
        Some(offgrid_killer),
        None,
        stg_site.solar_system_id,
        &stg_site.site_id,
        None,
    )
    .await
    .unwrap();
    assert_eq!(offgrid_res.fleet_size, 0);
    assert_eq!(offgrid_res.isk_per_pilot, 0);
    assert_eq!(offgrid_res.concord_lp_per_pilot, 0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&offgrid_killer].balance,
            IskCents::from_isk(1_000_000)
        );
    });

    // 6. Boss killed BEFORE escorts with security_status already >= 5.0 (no sec txn added),
    //    then remaining escorts killed -> final escort triggers completion via stored site.killer_char_id!
    let capped_killer = 92_100_010i64;
    insert_in_space_pilot(
        &ctx,
        capped_killer,
        stg_site.solar_system_id,
        None,
        site_pos,
        5.0,
    );
    let (boss_bid_3, escorts_3): (i64, Vec<i64>) = with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves
            .get_mut(&stg_site.site_id)
            .unwrap()
            .current_wave = 2;
        let s = reg
            .incursions
            .get(&cid)
            .unwrap()
            .sites
            .iter()
            .find(|s| s.site_id == stg_site.site_id)
            .unwrap();
        (
            s.boss_npc_ball_id,
            s.escort_spawns.iter().map(|e| e.npc_ball_id).collect(),
        )
    });
    assert!(!escorts_3.is_empty());
    let _ = sys_handle.remove_ball(boss_bid_3).await;
    let boss_early = on_incursion_boss_destroyed(
        &ctx,
        stg_site.solar_system_id,
        boss_bid_3,
        Some(capped_killer),
    )
    .await
    .expect("boss recognized");
    assert_eq!(boss_early.unwrap_err().code, "IncursionSiteNpcsRemaining");
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap
                .combat_crimewatch_state
                .sec_status_transactions
                .contains_key(&capped_killer),
            "5.0 sec status pilot must not gain extra sec_status_transactions"
        );
    });
    let mut escort_completed = None;
    for esc_bid in escorts_3 {
        let _ = sys_handle.remove_ball(esc_bid).await;
        if let Some(res) = on_incursion_boss_destroyed(
            &ctx,
            stg_site.solar_system_id,
            esc_bid,
            None, // verifies stored site.killer_char_id is credited!
        )
        .await
        {
            escort_completed = Some(res.unwrap());
            break;
        }
    }
    let esc_fin = escort_completed.expect("killing last escort after boss triggers completion");
    assert_eq!(esc_fin.fleet_size, 1);

    // 7. Mothership locked while influence > 0.10 via on_incursion_boss_destroyed, AND lockout expiry -> Withdrawing
    let locked_boss = on_incursion_boss_destroyed(
        &ctx,
        ms_site.solar_system_id,
        ms_site.boss_npc_ball_id,
        Some(solo_killer),
    )
    .await
    .unwrap();
    assert_eq!(locked_boss.unwrap_err().code, "MothershipSiteLocked");

    let now_unix = ctx.db.clock().now_unix_secs();
    let now_ft = ((now_unix + 11_644_473_600) * 10_000_000) as i64;
    with_pve_special_reg(&ctx, |reg| {
        let inc_m = reg.incursions.get_mut(&cid).unwrap();
        inc_m.mothership_spawned = true;
        inc_m.mothership_defeated = false;
        inc_m.influence = 0.0;
        inc_m.lockout_until_filetime = Some(now_ft - 1);
    });
    let expired_err = process_incursion_site_completion_and_payout(
        &ctx,
        Some(solo_killer),
        None,
        ms_site.solar_system_id,
        &ms_site.site_id,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(expired_err.code, "MothershipSiteLocked");
    with_pve_special_reg(&ctx, |reg| {
        assert_eq!(reg.incursions[&cid].state, "Withdrawing");
    });
}
