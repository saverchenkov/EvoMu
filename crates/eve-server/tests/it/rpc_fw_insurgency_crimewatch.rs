//! Parity integration tests for Faction Warfare (`fwVictoryPointMgr`), Pirate Insurgencies (`insurgencySolarsystem`),
//! Crimewatch Kill Rights (`killRightMgr`), Security Status (`securityMgr`), Hacking (`hackingMgr`),
//! and Solar System Interference (`solarsystemInterferenceMgr`).

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
use eve_server::services::fw_services::with_fw_runtime;
use eve_server::services::insurgency_services::{with_insurgency_runtime, InsurgencyCampaign};
use eve_server::services::pve_exploration::{with_pve_exploration_reg, HackingGameSession, HackingUtilityKind};

use crate::common;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

#[tokio::test]
// parity: rpc:fwVictoryPointMgr.GetAllVictoryPointStates
// parity: rpc:fwVictoryPointMgr.GetLocalVictoryPointState
// parity: rpc:insurgencySolarsystem.GetAllVisibleCampaigns
// parity: rpc:insurgencySolarsystem.GetLocalCampaignClientSnapshot
// parity: rpc:securityMgr.get_modified_systems
async fn test_fw_insurgency_and_security_mgr() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(28_801);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let sys_id = 30_002_057u32; // In FW warzone

    // 1. Setup FW runtime state for victory points
    with_fw_runtime(&ctx, |rt| {
        rt.system_victory_points.insert(sys_id, 1_500);
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = sys_id;
        }
        let st = snap.system_states.entry(sys_id).or_default();
        st.fw_contested_pct = 50.0;
    });

    // 1a. fwVictoryPointMgr.GetLocalVictoryPointState
    let local_vp = ctx
        .dispatch_rpc(&mut sess, "fwVictoryPointMgr", None, "GetLocalVictoryPointState", &[], &kw, None)
        .await
        .expect("GetLocalVictoryPointState");
    assert!(is_keyval(&local_vp));
    assert_eq!(local_vp.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_id as i64)));
    assert_eq!(local_vp.dict_get("victoryPoints"), Some(&PyRep::Integer(1_500)));
    assert_eq!(local_vp.dict_get("threshold"), Some(&PyRep::Integer(3_000)));

    // 1b. fwVictoryPointMgr.GetAllVictoryPointStates
    let all_vp = ctx
        .dispatch_rpc(&mut sess, "fwVictoryPointMgr", None, "GetAllVictoryPointStates", &[], &kw, None)
        .await
        .expect("GetAllVictoryPointStates");
    let PyRep::Dict(vp_map) = &all_vp else { panic!("Expected Dict") };
    let sys_entry = vp_map.iter().find(|(k, _)| k == &PyRep::Integer(sys_id as i64));
    assert!(sys_entry.is_some(), "System present in all victory points");
    let (_, sys_data) = sys_entry.unwrap();
    assert_eq!(sys_data.dict_get("victoryPoints"), Some(&PyRep::Integer(1_500)));

    // Negative verification: session without solar system returns NoSolarSystem
    sess.character_id = None;
    sess.solar_system_id = None;
    let no_sys_err = ctx
        .dispatch_rpc(&mut sess, "fwVictoryPointMgr", None, "GetLocalVictoryPointState", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(no_sys_err.code, "NoSolarSystem");
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(sys_id);

    // 2. Setup Insurgency campaign in runtime
    let now_unix = ctx.now_unix();
    with_insurgency_runtime(&ctx, |rt| {
        let mut corruption_points = HashMap::new();
        corruption_points.insert(sys_id, 450); // Stage 3
        let mut suppression_points = HashMap::new();
        suppression_points.insert(sys_id, 150); // Stage 1
        rt.campaigns.insert(
            101,
            InsurgencyCampaign {
                campaign_id: 101,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now_unix - 3600,
                forecasting_end_unix: now_unix - 1800,
                active_end_unix: now_unix + 86400,
                winddown_end_unix: now_unix + 86400 * 2,
                corruption_points,
                suppression_points,
            },
        );
    });

    // 2a. insurgencySolarsystem.GetAllVisibleCampaigns
    let all_camps = ctx
        .dispatch_rpc(&mut sess, "insurgencySolarsystem", None, "GetAllVisibleCampaigns", &[], &kw, None)
        .await
        .expect("GetAllVisibleCampaigns");
    let PyRep::List(camp_list) = &all_camps else { panic!("Expected List") };
    assert!(!camp_list.is_empty(), "Campaign list not empty");
    assert_eq!(camp_list[0].dict_get("campaignID"), Some(&PyRep::Integer(101)));
    assert_eq!(camp_list[0].dict_get("pirateFactionID"), Some(&PyRep::Integer(500_010)));
    assert_eq!(camp_list[0].dict_get("isFobVulnerable"), Some(&PyRep::Bool(true)));

    // 2b. insurgencySolarsystem.GetLocalCampaignClientSnapshot
    let local_camp = ctx
        .dispatch_rpc(&mut sess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None)
        .await
        .expect("GetLocalCampaignClientSnapshot");
    let PyRep::Tuple(camp_tuple) = &local_camp else { panic!("Expected Tuple") };
    assert_eq!(camp_tuple[0], PyRep::Integer(sys_id as i64));
    assert!(is_keyval(&camp_tuple[1]));
    assert_eq!(camp_tuple[1].dict_get("campaignID"), Some(&PyRep::Integer(101)));
    assert_eq!(camp_tuple[1].dict_get("isFobSystem"), Some(&PyRep::Bool(true)));
    assert_eq!(camp_tuple[1].dict_get("corruptionStage"), Some(&PyRep::Integer(3)));
    assert_eq!(camp_tuple[1].dict_get("suppressionStage"), Some(&PyRep::Integer(1)));

    // 3. securityMgr.get_modified_systems
    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(sys_id, 0.25);
    });
    let mod_sec = ctx
        .dispatch_rpc(&mut sess, "securityMgr", None, "get_modified_systems", &[], &kw, None)
        .await
        .expect("get_modified_systems");
    let PyRep::Dict(sec_entries) = &mod_sec else { panic!("Expected Dict") };
    let found_sec = sec_entries.iter().find(|(k, _)| k == &PyRep::Integer(sys_id as i64));
    assert!(found_sec.is_some(), "Modified system present in securityMgr response");
    assert_eq!(found_sec.unwrap().1, PyRep::Real(0.25));
}

#[tokio::test]
// parity: rpc:killRightMgr.ActivateKillRight
// parity: rpc:killRightMgr.BuyKillRight
// parity: rpc:hackingMgr.QuitHackingAttempt
async fn test_killrights_hacking_and_interference() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess1 = SessionState::new(28_901);
    let mut sess2 = SessionState::new(28_902);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess2");

    let p3_uid = ctx.db.provision_player_account("buyer_pilot3", "bp3-secret");
    let p3_char = ctx.db.create_character(p3_uid, "Buyer Pilot Three", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
    let mut sess3 = SessionState::new(28_903);
    common::login_pilot(&ctx, &mut sess3, "buyer_pilot3", "bp3-secret").await;
    ctx.dispatch_rpc(
        &mut sess3,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(p3_char)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess3");

    let p1_char = sess1.character_id.expect("p1 char");
    let p2_char = sess2.character_id.expect("p2 char");

    // 1. Setup Kill Rights
    let kr1_id = 70_001i64;
    let kr2_id = 70_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.insert(
            kr1_id,
            eve_db::KillRightRecord {
                kill_right_id: kr1_id,
                owner_id: p1_char,
                target_id: p2_char,
                activation_fee: eve_db::IskCents(0),
                is_active_suspect: false,
            },
        );
        snap.kill_rights.insert(
            kr2_id,
            eve_db::KillRightRecord {
                kill_right_id: kr2_id,
                owner_id: p1_char,
                target_id: p2_char,
                activation_fee: eve_db::IskCents(50_000_000_00), // 50M ISK
                is_active_suspect: false,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&p3_char) {
            ch.balance = eve_db::IskCents(100_000_000_00);
        }
    });

    // 1a. Non-owner cannot directly ActivateKillRight without buying
    let unauth_act = ctx
        .dispatch_rpc(&mut sess2, "killRightMgr", None, "ActivateKillRight", &[PyRep::Integer(kr1_id)], &kw, None)
        .await;
    assert!(unauth_act.is_err());
    assert_eq!(unauth_act.unwrap_err().code, "NotKillRightOwner");

    // Owner activates kr1
    let act_rep = ctx
        .dispatch_rpc(&mut sess1, "killRightMgr", None, "ActivateKillRight", &[PyRep::Integer(kr1_id)], &kw, None)
        .await
        .expect("ActivateKillRight");
    assert!(is_keyval(&act_rep));
    assert_eq!(act_rep.dict_get("killRightID"), Some(&PyRep::Integer(kr1_id)));
    assert_eq!(act_rep.dict_get("suspectFlagActive"), Some(&PyRep::Bool(true)));
    let kr1_target_suspect = ctx.crimewatch.read().unwrap().states.get(&p2_char).map(|c| c.suspect_flag_until_unix).unwrap_or(0);
    assert!(kr1_target_suspect > ctx.now_unix(), "Target of activated kill right must have active suspect timer");

    // 1b. Target pilot (p2_char) cannot buy kill right against self
    let self_buy_err = ctx
        .dispatch_rpc(&mut sess2, "killRightMgr", None, "BuyKillRight", &[PyRep::Integer(kr2_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(self_buy_err.code, "CannotBuyKillRightAgainstSelf");

    // Owner (p1_char) cannot buy own kill right
    let owner_buy_err = ctx
        .dispatch_rpc(&mut sess1, "killRightMgr", None, "BuyKillRight", &[PyRep::Integer(kr2_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(owner_buy_err.code, "AlreadyKillRightOwner");

    // Third-party pilot (p3_char) buys public kr2
    let p1_bal_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&p1_char).map(|c| c.balance.0).unwrap_or(0)
    });
    let buy_rep = ctx
        .dispatch_rpc(&mut sess3, "killRightMgr", None, "BuyKillRight", &[PyRep::Integer(kr2_id)], &kw, None)
        .await
        .expect("BuyKillRight");
    assert!(is_keyval(&buy_rep));
    assert_eq!(buy_rep.dict_get("killRightID"), Some(&PyRep::Integer(kr2_id)));
    assert_eq!(buy_rep.dict_get("suspectFlagActive"), Some(&PyRep::Bool(true)));

    let (p1_bal_after, p3_bal_after) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters.get(&p1_char).map(|c| c.balance.0).unwrap_or(0),
            snap.characters.get(&p3_char).map(|c| c.balance.0).unwrap_or(0),
        )
    });
    assert_eq!(p3_bal_after, 50_000_000_00, "Buyer ISK balance should decrease by 50M ISK");
    assert_eq!(p1_bal_after, p1_bal_before + 50_000_000_00, "Owner ISK balance should increase by 50M ISK");
    let target_suspect = ctx.crimewatch.read().unwrap().states.get(&p2_char).map(|c| c.suspect_flag_until_unix).unwrap_or(0);
    assert!(target_suspect > ctx.now_unix(), "Target of purchased kill right must have active suspect timer");

    // Buying already activated kill right fails
    let rebuy_err = ctx
        .dispatch_rpc(&mut sess3, "killRightMgr", None, "BuyKillRight", &[PyRep::Integer(kr2_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(rebuy_err.code, "KillRightAlreadyActivated");

    // 2. Setup Hacking Minigame
    let container_id = 1_870_000_123i64;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games.insert(
            (p1_char, container_id),
            HackingGameSession {
                game_id: 1,
                character_id: p1_char,
                solar_system_id: 30_000_142,
                container_id,
                site_id: "site_test_1".into(),
                is_relic: false,
                is_ghost_site: false,
                is_sleeper_cache_puzzle: false,
                virus_coherence: 60,
                max_virus_coherence: 100,
                base_virus_strength: 30,
                polymorphic_shield_charges: 0,
                secondary_vector_target: None,
                utilities: vec![Some(HackingUtilityKind::SelfRepair)],
                tiles: vec![],
                status: "active".into(),
            },
        );
    });

    // 2a. hackingMgr.UsedUtilityElementOnVirus returns UnimplementedRPC
    let util_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "hackingMgr",
            None,
            "UsedUtilityElementOnVirus",
            &[PyRep::Integer(container_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(util_err.code, "UnimplementedRPC");

    // 2b. hackingMgr.QuitHackingAttempt requires explicit container_id argument
    let missing_arg_err = ctx
        .dispatch_rpc(&mut sess1, "hackingMgr", None, "QuitHackingAttempt", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(missing_arg_err.code, "InvalidArguments");

    let quit_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "hackingMgr",
            None,
            "QuitHackingAttempt",
            &[PyRep::Integer(container_id)],
            &kw,
            None,
        )
        .await
        .expect("QuitHackingAttempt");
    assert_eq!(quit_rep, PyRep::Bool(true));
    with_pve_exploration_reg(&ctx, |reg| {
        assert!(!reg.hacking_games.contains_key(&(p1_char, container_id)), "Hacking game terminated");
    });

    // Calling QuitHackingAttempt again fails with NoActiveHackingGame
    let re_quit_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "hackingMgr",
            None,
            "QuitHackingAttempt",
            &[PyRep::Integer(container_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(re_quit_err.code, "NoActiveHackingGame");

    // 3. solarsystemInterferenceMgr.GetAllInterferenceBands returns UnimplementedRPC
    let sys_id = 30_000_142u32;
    sess1.solar_system_id = Some(sys_id);
    let band_err = ctx
        .dispatch_rpc(&mut sess1, "solarsystemInterferenceMgr", None, "GetAllInterferenceBands", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(band_err.code, "UnimplementedRPC");
}
