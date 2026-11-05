//! Live integration tests for Homefront Operations, Drifter Hives, Triglavian Sites,
//! and NPC-NPC Standings table (`feat:pve.homefront-ops`, `feat:pve.drifter-hives`,
//! `feat:pve.triglavian-sites`, `feat:standings.npc-npc-standings`).

#![forbid(unsafe_code)]

use crate::common;
use crate::npc_spawns_pve::{
    bind_beyonce, rpc, setup_space_pilot, warp_lock_and_destroy_via_rpc,
};
use eve_destiny::{DVec3, DamageVector};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites::{
    ensure_special_sites_spawned, with_pve_special_reg,
};

#[tokio::test]
async fn test_homefront_operations_live() {

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    let jita = 30_000_142u32;
    let (mut sess, guns) = setup_space_pilot(
        &ctx,
        7601,
        "demo",
        "demo-secret",
        90_000_001,
        jita,
        910_401,
        54733,
        54754,
        54783,
        924_000,
        DVec3::ZERO,
    )
    .await;
    let jita_beyonce = bind_beyonce(&ctx, &mut sess, jita).await;

    // 1. Query dungeonInstanceCacheMgr.GetHomefrontSiteInstances for High-sec Jita
    let PyRep::Dict(jita_hf) = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(jita as i64)],
    )
    .await
    .expect("GetHomefrontSiteInstances Jita") else {
        panic!("Expected Dict from GetHomefrontSiteInstances");
    };

    let Some((_, PyRep::Dict(jita_dungeons))) = jita_hf.iter().find(|(k, _)| k.as_i64() == Some(jita as i64)) else {
        panic!("Dict must contain entry for queried solar system {jita}");
    };
    assert!(!jita_dungeons.is_empty(), "Jita must have seeded Homefront dungeons");

    let (did_rep, list_rep) = &jita_dungeons[0];
    let did = did_rep.as_i64().expect("dungeonID") as u32;
    let PyRep::List(inst_list) = list_rep else {
        panic!("Expected List of instance IDs");
    };
    assert!(!inst_list.is_empty(), "Dungeon must have active instance IDs");
    let target_instance_id = inst_list[0].as_i64().expect("instanceID");

    // SDE Archetype 70 verification
    let d_archetype = ctx.sde.dungeons.get(&did).and_then(|d| d.archetype_id);
    assert_eq!(d_archetype, Some(70), "Homefront dungeon must belong to SDE archetype 70");

    // 2. Query for null-sec / low-sec: must return empty
    let PyRep::Dict(null_hf) = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(30_001_269)], // null-sec
    )
    .await
    .expect("GetHomefrontSiteInstances null-sec") else {
        panic!("Expected Dict");
    };
    let PyRep::Dict(null_dngs) = &null_hf[0].1 else { panic!() };
    assert!(null_dngs.is_empty(), "Null-sec system must have 0 Homefront Operation instances");

    // 3. Locate boss ball and verify combat state in space
    let (boss_bid, boss_tid) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.homefront_sites[&jita]
            .iter()
            .find(|s| s.instance_id == target_instance_id)
            .unwrap();
        (s.boss_npc_ball_id, s.boss_type_id)
    });
    let jita_sys = ctx.systems.get_or_spawn(jita);
    let boss_snap = jita_sys
        .get_ball_snapshot(boss_bid)
        .await
        .expect("get_ball_snapshot")
        .expect("Homefront boss ball must be spawned");
    assert_eq!(boss_snap.1.type_id, boss_tid);

    let init_balance = ctx.db.get_character(90_000_001).unwrap().balance.0;
    let init_journal_len = ctx.db.with_snapshot(|s| s.wallet_journal.len());

    // 4. Destroy boss via live combat RPC dogmaIM.Activate
    warp_lock_and_destroy_via_rpc(
        &ctx,
        &mut sess,
        jita_beyonce,
        jita,
        910_401,
        &guns,
        boss_bid,
        None,
        25,
    )
    .await;

    // 5. Verify ISK payout and CorporateRewardPayout wallet entry
    let final_balance = ctx.db.get_character(90_000_001).unwrap().balance.0;
    assert!(
        final_balance > init_balance,
        "Pilot must receive ISK payout from Homefront completion"
    );

    let journal = ctx.db.with_snapshot(|s| s.wallet_journal.clone());
    assert!(
        journal.len() > init_journal_len
            && journal.iter().any(|j| {
                j.character_id == 90_000_001
                    && j.entry_type == "CorporateRewardPayout"
                    && j.reason.contains("Homefront Operation Payout")
            }),
        "Homefront completion must record CorporateRewardPayout wallet journal entry"
    );

    // 6. Verify completed instance is removed from active registry list
    let PyRep::Dict(hf_after) = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(jita as i64)],
    )
    .await
    .expect("GetHomefrontSiteInstances after completion") else {
        panic!("Expected Dict");
    };
    let Some((_, PyRep::Dict(jita_after_dngs))) = hf_after.iter().find(|(k, _)| k.as_i64() == Some(jita as i64)) else {
        panic!();
    };
    let remaining_instances: Vec<i64> = jita_after_dngs
        .iter()
        .flat_map(|(_, l)| match l {
            PyRep::List(ids) => ids.iter().filter_map(|i| i.as_i64()).collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    assert!(
        !remaining_instances.contains(&target_instance_id),
        "Completed instance {target_instance_id} must not appear in active registry instances"
    );
}

#[tokio::test]
async fn test_drifter_hives_exploration_and_loot_live() {

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    // Verify all 5 Jovian hive systems are seeded in registry
    with_pve_special_reg(&ctx, |reg| {
        for (sys_id, did, name) in [
            (31_000_001u32, 5648u32, "Barbican Hive"),
            (31_000_002, 5649, "Vidette Hive"),
            (31_000_003, 5650, "Conflux Hive"),
            (31_000_004, 5651, "Redoubt Hive"),
            (31_000_005, 5642, "Sentinel Hive"),
        ] {
            let hive = reg.drifter_hives.get(&sys_id).expect("Drifter hive must exist");
            assert_eq!(hive.dungeon_id, did);
            assert_eq!(hive.dungeon_name, name);
            assert_eq!(hive.total_rooms, 3);
            assert_eq!(hive.current_room, 1);
            assert!(!hive.cleared);
        }
    });

    let vidette = 31_000_002u32;
    let (mut sess, _guns) = setup_space_pilot(
        &ctx,
        7602,
        "demo",
        "demo-secret",
        90_000_001,
        vidette,
        910_402,
        54733,
        54754,
        54783,
        924_100,
        DVec3::ZERO,
    )
    .await;
    let _vidette_beyonce = bind_beyonce(&ctx, &mut sess, vidette).await;

    // Query GetSignatureInstances for Vidette Hive
    let PyRep::Dict(sig_rep) = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetSignatureInstances",
        &[PyRep::Integer(vidette as i64)],
    )
    .await
    .expect("GetSignatureInstances Vidette") else {
        panic!("Expected Dict");
    };
    let Some((_, PyRep::Dict(v_dngs))) = sig_rep.iter().find(|(k, _)| k.as_i64() == Some(vidette as i64)) else {
        panic!("Expected Vidette signature entry");
    };
    assert!(!v_dngs.is_empty(), "Vidette must have active hive dungeon signature");
    assert_eq!(v_dngs[0].0.as_i64(), Some(5649));

    // Room 1: Perimeter Approach -> 2 cruiser NPCs
    let (room1_bids, room2_bids, warden_bid) = with_pve_special_reg(&ctx, |reg| {
        let h = &reg.drifter_hives[&vidette];
        (h.rooms[0].npc_ball_ids.clone(), h.rooms[1].npc_ball_ids.clone(), h.warden_npc_ball_id)
    });

    let sys_handle = ctx.systems.get_or_spawn(vidette);
    for &bid in &room1_bids {
        assert!(sys_handle.get_ball_snapshot(bid).await.unwrap().is_some(), "Room 1 NPC ball must exist");
    }

    // Destroy Room 1 NPCs via live ballpark combat damage
    for &bid in &room1_bids {
        let kill_evt = sys_handle
            .apply_damage_with_attacker(
                bid,
                DamageVector {
                    em: 100_000.0,
                    thermal: 100_000.0,
                    kinetic: 100_000.0,
                    explosive: 100_000.0,
                },
                Some(90_000_001),
            )
            .await
            .expect("apply_damage_with_attacker")
            .expect("Room 1 NPC hit result");
        assert!(kill_evt.1, "Room 1 NPC must be destroyed");
    }

    // Hive advances to Room 2
    let cur_room = with_pve_special_reg(&ctx, |reg| reg.drifter_hives[&vidette].current_room);
    assert_eq!(cur_room, 2, "Hive must advance to Room 2 after Room 1 NPCs destroyed");

    // Room 2 NPCs must now be spawned in ballpark
    for &bid in &room2_bids {
        assert!(sys_handle.get_ball_snapshot(bid).await.unwrap().is_some(), "Room 2 NPC ball must be spawned");
    }

    // Destroy Room 2 NPCs via live ballpark combat damage
    for &bid in &room2_bids {
        let kill_evt = sys_handle
            .apply_damage_with_attacker(
                bid,
                DamageVector {
                    em: 250_000.0,
                    thermal: 250_000.0,
                    kinetic: 250_000.0,
                    explosive: 250_000.0,
                },
                Some(90_000_001),
            )
            .await
            .expect("apply_damage_with_attacker")
            .expect("Room 2 NPC hit result");
        assert!(kill_evt.1, "Room 2 NPC must be destroyed");
    }

    // Hive advances to Room 3: Hive Sanctuary with warden Artemis Tyrannos
    let cur_room3 = with_pve_special_reg(&ctx, |reg| reg.drifter_hives[&vidette].current_room);
    assert_eq!(cur_room3, 3, "Hive must advance to Room 3 (Hive Sanctuary)");

    assert!(sys_handle.get_ball_snapshot(warden_bid).await.unwrap().is_some(), "Warden ball must be spawned");

    // Destroy warden via live ballpark combat damage
    let kill_evt = sys_handle
        .apply_damage_with_attacker(
            warden_bid,
            DamageVector {
                em: 1_000_000.0,
                thermal: 1_000_000.0,
                kinetic: 1_000_000.0,
                explosive: 1_000_000.0,
            },
            Some(90_000_001),
        )
        .await
        .expect("apply_damage_with_attacker")
        .expect("Warden hit result");
    assert!(kill_evt.1, "Warden must be destroyed");

    // Verify hive cleared in registry
    let is_cleared = with_pve_special_reg(&ctx, |reg| reg.drifter_hives[&vidette].cleared);
    assert!(is_cleared, "Drifter hive must be marked cleared");

    // Verify loot drop: Vidette Element (34558) and Antikythera Elements (30745)
    let (has_elem, has_anti) = ctx.db.with_snapshot(|snap| {
        let has_elem = snap.items.values().any(|i| i.owner_id == 90_000_001 && i.type_id == 34558);
        let has_anti = snap.items.values().any(|i| i.owner_id == 90_000_001 && i.type_id == 30745);
        (has_elem, has_anti)
    });
    assert!(has_elem, "Pilot must receive Vidette Element (34558)");
    assert!(has_anti, "Pilot must receive Antikythera Element (30745)");
}

#[tokio::test]
async fn test_triglavian_edencom_sites_and_standings_live() {

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    // 1. Triglavian Minor Victory system: Gammel (30003076)
    let gammel = 30_003_076u32;
    let (mut trig_sess, trig_guns) = setup_space_pilot(
        &ctx,
        7603,
        "demo",
        "demo-secret",
        90_000_001,
        gammel,
        910_403,
        54733,
        54754,
        54783,
        924_200,
        DVec3::ZERO,
    )
    .await;
    let trig_beyonce = bind_beyonce(&ctx, &mut trig_sess, gammel).await;

    // Query GetTriglavianSiteInstances for Gammel
    let PyRep::Dict(g_rep) = rpc(
        &ctx,
        &mut trig_sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(gammel as i64)],
    )
    .await
    .expect("GetTriglavianSiteInstances Gammel") else {
        panic!("Expected Dict");
    };
    let PyRep::Dict(g_dngs) = &g_rep[0].1 else { panic!() };
    // Major Conduit (8802) and Minor Conduit (8742)
    assert_eq!(g_dngs.len(), 2, "Gammel must contain 2 Triglavian conduits");

    // 2. EDENCOM Minor Victory system: e.g. 30004231
    let edencom_sys = 30_004_231u32;
    let PyRep::Dict(ed_rep) = rpc(
        &ctx,
        &mut trig_sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(edencom_sys as i64)],
    )
    .await
    .expect("GetTriglavianSiteInstances EDENCOM") else {
        panic!("Expected Dict");
    };
    let PyRep::Dict(ed_dngs) = &ed_rep[0].1 else { panic!() };
    // EDENCOM Field Base (8808) and Forward Post (8792)
    assert_eq!(ed_dngs.len(), 2, "EDENCOM system must contain 2 EDENCOM sites");

    // 3. Combat completion on Triglavian Conduit boss (Striking Leshak 48122)
    let trig_boss_bid = with_pve_special_reg(&ctx, |reg| reg.triglavian_sites[&gammel][0].boss_npc_ball_id);
    let init_bal = ctx.db.get_character(90_000_001).unwrap().balance.0;

    warp_lock_and_destroy_via_rpc(
        &ctx,
        &mut trig_sess,
        trig_beyonce,
        gammel,
        910_403,
        &trig_guns,
        trig_boss_bid,
        None,
        35,
    )
    .await;

    // Verify ISK reward payout
    let final_bal = ctx.db.get_character(90_000_001).unwrap().balance.0;
    assert!(final_bal > init_bal, "Pilot must receive ISK reward for completing Triglavian site");

    // Verify Survey Database (48121) loot drop
    let has_survey_db = ctx.db.with_snapshot(|s| {
        s.items.values().any(|i| i.owner_id == 90_000_001 && i.type_id == 48_121)
    });
    assert!(has_survey_db, "Triglavian boss kill must award Survey Database (48121)");

    // Verify Standings modification: +0.15 EDENCOM (500027) and -0.15 Triglavian Collective (500026)
    let ch = ctx.db.get_character(90_000_001).unwrap();
    let edencom_standing = ch.standings.get(&500_027).copied().unwrap_or(0.0);
    let triglavian_standing = ch.standings.get(&500_026).copied().unwrap_or(0.0);
    assert!(
        edencom_standing > 0.0,
        "Killing Triglavian boss must award positive EDENCOM standing (+0.15), got {edencom_standing}"
    );
    assert!(
        triglavian_standing < 0.0,
        "Killing Triglavian boss must decrease Triglavian standing (-0.15), got {triglavian_standing}"
    );
}

#[tokio::test]
async fn test_npc_npc_standings_table_live() {

    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();

    // 1. Unauthenticated call must fail
    let mut unauth = SessionState::new(7604);
    let err = rpc(&ctx, &mut unauth, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect_err("Unauthenticated GetNPCNPCStandings must fail");
    assert!(
        err.code == "Unauthorized" || err.code == "SessionNotFound" || err.code == "AuthRequired",
        "Expected auth error, got: {err:?}"
    );

    // 2. Authenticated call returns Rowset
    let mut sess = SessionState::new(7605);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);

    let rep = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");

    let PyRep::Rowset { columns, rows } = rep else {
        panic!("Expected PyRep::Rowset from GetNPCNPCStandings");
    };

    assert!(
        columns.contains(&"fromID".to_string())
            && columns.contains(&"toID".to_string())
            && columns.contains(&"standing".to_string()),
        "Rowset columns must contain fromID, toID, standing: {columns:?}"
    );

    assert!(
        rows.len() >= 40,
        "Expected at least 40 static NPC faction standing pairs, got {}",
        rows.len()
    );

    // Verify key faction standing relationships
    let find_standing = |from: i64, to: i64| -> Option<f64> {
        for r in &rows {
            if let (Some(f), Some(t), Some(s)) = (
                r.first().and_then(|v| v.as_i64()),
                r.get(1).and_then(|v| v.as_i64()),
                r.get(2).and_then(|v| v.as_f64()),
            ) {
                if f == from && t == to {
                    return Some(s);
                }
            }
        }
        None
    };

    // Amarr (500001) -> Caldari (500003) is +5.0
    let amarr_to_caldari = find_standing(500_001, 500_003).expect("Amarr -> Caldari standing");
    assert!(
        (amarr_to_caldari - 5.0).abs() < 1e-6,
        "Amarr -> Caldari should be +5.0, got {amarr_to_caldari}"
    );

    // Caldari (500003) -> Gallente (500004) is -3.0
    let caldari_to_gallente = find_standing(500_003, 500_004).expect("Caldari -> Gallente standing");
    assert!(
        (caldari_to_gallente - (-3.0)).abs() < 1e-6,
        "Caldari -> Gallente should be -3.0, got {caldari_to_gallente}"
    );

    // Caldari (500003) -> Minmatar (500002) is -5.0
    let caldari_to_minmatar = find_standing(500_003, 500_002).expect("Caldari -> Minmatar standing");
    assert!(
        (caldari_to_minmatar - (-5.0)).abs() < 1e-6,
        "Caldari -> Minmatar should be -5.0, got {caldari_to_minmatar}"
    );

    // Triglavian Collective (500026) -> EDENCOM (500027) is -10.0
    let trig_to_edencom = find_standing(500_026, 500_027).expect("Triglavian -> EDENCOM standing");
    assert!(
        (trig_to_edencom - (-10.0)).abs() < 1e-6,
        "Triglavian -> EDENCOM should be -10.0, got {trig_to_edencom}"
    );
}
