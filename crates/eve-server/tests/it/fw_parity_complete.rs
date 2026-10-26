//! `fw_parity_complete` — Integration tests for Faction Warfare Warzones, Victory Points,
//! System Capture, Advantage, Militia Ranks, and War Targets.

use crate::common::login_pilot;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::AggressionEvaluation;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::is_legal_war_or_suspect_target;
use eve_server::services::fw_services::{
    self, calculate_capture_speed_multiplier, calculate_militia_rank, capture_warzone_system,
    update_system_advantage, FACTION_AMARR, FACTION_CALDARI, FACTION_GALLENTE, FACTION_MINMATAR,
    HQ_SYSTEM_AMO, STATUS_COMMAND, STATUS_FRONTLINE, STATUS_REARGUARD,
};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_fw_warzones_victory_points_and_system_capture() {
    // parity: feat:fw.warzones, feat:fw.victory-points, feat:fw.system-capture
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(10_001);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let kw = HashMap::new();

    // Select character and place session in FW solar system (Amo: 30002055, Minmatar HQ)
    let sys_id = HQ_SYSTEM_AMO;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
        }
    });

    // ------------------------------------------------------------------------
    // 1. feat:fw.warzones
    // ------------------------------------------------------------------------
    // GetAllWarzones returns Caldari-Gallente and Amarr-Minmatar warzones
    let wz_rep = ctx
        .dispatch_rpc(&mut sess, "fwWarzoneSolarsystem", None, "GetAllWarzones", &[], &kw, None)
        .await
        .expect("GetAllWarzones");
    match wz_rep {
        PyRep::List(wzs) => {
            assert_eq!(wzs.len(), 2, "Must return 2 warzones");
            let wz1 = &wzs[0];
            assert_eq!(wz1.dict_get("warzoneID"), Some(&PyRep::Integer(1)));
            assert_eq!(wz1.dict_get("factionOneID"), Some(&PyRep::Integer(FACTION_CALDARI)));
            assert_eq!(wz1.dict_get("factionTwoID"), Some(&PyRep::Integer(FACTION_GALLENTE)));
            let wz2 = &wzs[1];
            assert_eq!(wz2.dict_get("warzoneID"), Some(&PyRep::Integer(2)));
            assert_eq!(wz2.dict_get("factionOneID"), Some(&PyRep::Integer(FACTION_AMARR)));
            assert_eq!(wz2.dict_get("factionTwoID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
        }
        other => panic!("Expected List for GetAllWarzones, got {other:?}"),
    }

    // GetAllWarzonesOccupationStates returns owner, occupier, frontline status, and contested %
    let all_states = ctx
        .dispatch_rpc(&mut sess, "fwWarzoneSolarsystem", None, "GetAllWarzonesOccupationStates", &[], &kw, None)
        .await
        .expect("GetAllWarzonesOccupationStates");
    match all_states {
        PyRep::List(states) => {
            assert!(!states.is_empty(), "Must contain warzone systems");
            let amo = states
                .iter()
                .find(|s| s.dict_get("solarSystemID") == Some(&PyRep::Integer(sys_id as i64)))
                .expect("Amo must be in occupation states");
            assert_eq!(amo.dict_get("ownerFactionID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
            assert_eq!(amo.dict_get("occupierFactionID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
            assert_eq!(amo.dict_get("frontlineStatus"), Some(&PyRep::Integer(STATUS_REARGUARD)));
            assert!(matches!(amo.dict_get("contestedPercentage"), Some(&PyRep::Real(_))));

            // Frontline system 30002087 and Command system 30002086
            let fl = states
                .iter()
                .find(|s| s.dict_get("solarSystemID") == Some(&PyRep::Integer(30002087)))
                .expect("30002087 must be present");
            assert_eq!(fl.dict_get("frontlineStatus"), Some(&PyRep::Integer(STATUS_FRONTLINE)));
            let cmd = states
                .iter()
                .find(|s| s.dict_get("solarSystemID") == Some(&PyRep::Integer(30002086)))
                .expect("30002086 must be present");
            assert_eq!(cmd.dict_get("frontlineStatus"), Some(&PyRep::Integer(STATUS_COMMAND)));
        }
        other => panic!("Expected List for GetAllWarzonesOccupationStates, got {other:?}"),
    }

    // GetLocalOccupationState returns tuple (sys_id, state_keyval)
    let local_occ = ctx
        .dispatch_rpc(&mut sess, "fwWarzoneSolarsystem", None, "GetLocalOccupationState", &[], &kw, None)
        .await
        .expect("GetLocalOccupationState");
    match local_occ {
        PyRep::Tuple(t) => {
            assert_eq!(t.len(), 2);
            assert_eq!(t[0], PyRep::Integer(sys_id as i64));
            let state = &t[1];
            assert_eq!(state.dict_get("ownerFactionID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
            assert_eq!(state.dict_get("occupierFactionID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
            assert_eq!(state.dict_get("frontlineStatus"), Some(&PyRep::Integer(STATUS_REARGUARD)));
        }
        other => panic!("Expected Tuple for GetLocalOccupationState, got {other:?}"),
    }

    // ------------------------------------------------------------------------
    // 2. feat:fw.victory-points
    // ------------------------------------------------------------------------
    // Check initial local victory point state
    let vp_initial = ctx
        .dispatch_rpc(&mut sess, "fwVictoryPointMgr", None, "GetLocalVictoryPointState", &[], &kw, None)
        .await
        .expect("GetLocalVictoryPointState");
    assert_eq!(vp_initial.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_id as i64)));
    assert_eq!(vp_initial.dict_get("victoryPoints"), Some(&PyRep::Integer(0)));
    assert_eq!(vp_initial.dict_get("contestedPercentage"), Some(&PyRep::Real(0.0)));
    let threshold = match vp_initial.dict_get("threshold") {
        Some(PyRep::Integer(v)) => *v,
        other => panic!("Expected Integer threshold, got {other:?}"),
    };
    assert!(threshold > 0, "Threshold must be positive");

    // Killing an enemy awards LP and adds 20 VP to the system
    let kill_lp = fw_services::award_fw_kill_lp(&ctx, char_id, sys_id, 100_000_000.0);
    assert!(kill_lp > 0, "Kill in warzone awards LP");

    let vp_after_kill = ctx
        .dispatch_rpc(&mut sess, "fwVictoryPointMgr", None, "GetLocalVictoryPointState", &[], &kw, None)
        .await
        .expect("GetLocalVictoryPointState after kill");
    assert_eq!(vp_after_kill.dict_get("victoryPoints"), Some(&PyRep::Integer(20)));
    let c_kill = match vp_after_kill.dict_get("contestedPercentage") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("Expected Real contested %, got {other:?}"),
    };
    assert!((c_kill - (20.0 / threshold as f64 * 100.0)).abs() < 1e-4);

    // Adding VP up to threshold (100.0% contested)
    let (new_vp, new_c) = fw_services::add_victory_points(&ctx, sys_id, threshold - 20);
    assert_eq!(new_vp, threshold);
    assert_eq!(new_c, 100.0);

    let vp_full = ctx
        .dispatch_rpc(&mut sess, "fwVictoryPointMgr", None, "GetLocalVictoryPointState", &[], &kw, None)
        .await
        .expect("GetLocalVictoryPointState at 100%");
    assert_eq!(vp_full.dict_get("victoryPoints"), Some(&PyRep::Integer(threshold)));
    assert_eq!(vp_full.dict_get("contestedPercentage"), Some(&PyRep::Real(100.0)));

    // ------------------------------------------------------------------------
    // 3. feat:fw.system-capture
    // ------------------------------------------------------------------------
    // At 100% contested, system is vulnerable and capturing changes occupation to Amarr
    let capture_res = capture_warzone_system(&ctx, sys_id, FACTION_AMARR)
        .expect("capture_warzone_system at 100% contested");
    assert_eq!(capture_res.0, FACTION_MINMATAR, "Previous occupier");
    assert_eq!(capture_res.1, FACTION_AMARR, "New occupier");
    assert_eq!(capture_res.2, FACTION_MINMATAR, "Owner faction unchanged");

    // After capture, contested percentage and VP reset to 0
    let vp_reset = ctx
        .dispatch_rpc(&mut sess, "fwVictoryPointMgr", None, "GetLocalVictoryPointState", &[], &kw, None)
        .await
        .expect("GetLocalVictoryPointState after capture");
    assert_eq!(vp_reset.dict_get("victoryPoints"), Some(&PyRep::Integer(0)));
    assert_eq!(vp_reset.dict_get("contestedPercentage"), Some(&PyRep::Real(0.0)));

    // Verify occupation state reflects the new occupier faction
    let occ_after_capture = ctx
        .dispatch_rpc(&mut sess, "fwWarzoneSolarsystem", None, "GetLocalOccupationState", &[], &kw, None)
        .await
        .expect("GetLocalOccupationState after capture");
    if let PyRep::Tuple(t) = occ_after_capture {
        let state = &t[1];
        assert_eq!(state.dict_get("occupierFactionID"), Some(&PyRep::Integer(FACTION_AMARR)));
        assert_eq!(state.dict_get("ownerFactionID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
        assert_eq!(state.dict_get("contestedPercentage"), Some(&PyRep::Real(0.0)));
    } else {
        panic!("Expected Tuple");
    }

    // Capture when not 100% contested must fail with SystemNotVulnerable
    let err = capture_warzone_system(&ctx, sys_id, FACTION_MINMATAR).unwrap_err();
    assert_eq!(err.code, "SystemNotVulnerable");

    // Verify system recapture via internal helper when 100% contested
    fw_services::add_victory_points(&ctx, sys_id, threshold);
    let cap_back = capture_warzone_system(&ctx, sys_id, FACTION_MINMATAR)
        .expect("capture_warzone_system back to Minmatar");
    assert_eq!(cap_back.0, FACTION_AMARR);
    assert_eq!(cap_back.1, FACTION_MINMATAR);
    assert_eq!(cap_back.2, FACTION_MINMATAR);

    let occ_recap = ctx
        .dispatch_rpc(&mut sess, "fwWarzoneSolarsystem", None, "GetLocalOccupationState", &[], &kw, None)
        .await
        .expect("GetLocalOccupationState after recapture");
    if let PyRep::Tuple(t) = occ_recap {
        let state = &t[1];
        assert_eq!(state.dict_get("occupierFactionID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
        assert_eq!(state.dict_get("ownerFactionID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
        assert_eq!(state.dict_get("contestedPercentage"), Some(&PyRep::Real(0.0)));
    } else {
        panic!("Expected Tuple");
    }
}

#[tokio::test]
async fn test_fw_advantage_ranks_and_war_targets() {
    // parity: feat:fw.advantage, feat:fw.fw-ranks, feat:fw.fw-war-targets
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(10_002);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let kw = HashMap::new();
    let sys_id = HQ_SYSTEM_AMO;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
        }
    });

    // ------------------------------------------------------------------------
    // 4. feat:fw.advantage
    // ------------------------------------------------------------------------
    // Update advantage: emits OnSolarsystemAdvantageStateUpdated_Local
    let (friendly, enemy) = update_system_advantage(&ctx, sys_id, 40.0, 10.0);
    assert_eq!(friendly, 40.0);
    assert_eq!(enemy, 10.0);

    // Speed multiplier: (1.0 + (40 - 10) * 0.005) = 1.15
    let mult = calculate_capture_speed_multiplier(friendly, enemy);
    assert!((mult - 1.15).abs() < 1e-4);

    // Negative advantage delta lowers speed: friendly 10, enemy 50 => (1.0 + (10 - 50) * 0.005) = 0.80
    let disadv_mult = calculate_capture_speed_multiplier(10.0, 50.0);
    assert!((disadv_mult - 0.80).abs() < 1e-4);

    // Query advantage from authentic runtime state
    let (rt_friendly, rt_enemy) = fw_services::read_fw_runtime(&ctx, |rt| {
        rt.system_advantage.get(&sys_id).copied().unwrap_or((0.0, 0.0))
    });
    assert_eq!(rt_friendly, 40.0);
    assert_eq!(rt_enemy, 10.0);

    // ------------------------------------------------------------------------
    // 5. feat:fw.fw-ranks
    // ------------------------------------------------------------------------
    // Join Minmatar militia
    ctx.dispatch_rpc(
        &mut sess,
        "facWarMgr",
        None,
        "JoinFactionAsCharacter",
        &[PyRep::Integer(FACTION_MINMATAR)],
        &kw,
        None,
    )
    .await
    .expect("JoinFactionAsCharacter");

    // Rank derivation checks from standing
    assert_eq!(calculate_militia_rank(0.0), 0);
    assert_eq!(calculate_militia_rank(0.75), 1);
    assert_eq!(calculate_militia_rank(2.25), 3);
    assert_eq!(calculate_militia_rank(6.75), 9);
    assert_eq!(calculate_militia_rank(9.50), 9);

    // Set character standing towards Minmatar faction to 3.50 (Rank 4)
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.insert(FACTION_MINMATAR, 3.50);
    });

    // Verify show-info (charMgr.GetPublicInfo) displays warFactionID and militiaRank
    let pub_info = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetPublicInfo", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("GetPublicInfo");
    assert_eq!(pub_info.dict_get("warFactionID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
    assert_eq!(pub_info.dict_get("militiaRank"), Some(&PyRep::Integer(4)));
    assert_eq!(pub_info.dict_get("rank"), Some(&PyRep::Integer(4)));

    // Verify show-info (charMgr.GetPublicInfo3)
    let pub_info3 = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetPublicInfo3", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("GetPublicInfo3");
    assert_eq!(pub_info3.dict_get("warFactionID"), Some(&PyRep::Integer(FACTION_MINMATAR)));
    assert_eq!(pub_info3.dict_get("militiaRank"), Some(&PyRep::Integer(4)));

    // ------------------------------------------------------------------------
    // 6. feat:fw.fw-war-targets
    // ------------------------------------------------------------------------
    let enemy_char_id = 90_000_002i64;
    let neutral_char_id = 90_000_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        // Enemy militia member (Amarr, opposing Minmatar)
        let mut enemy_char = snap.characters.get(&char_id).unwrap().clone();
        enemy_char.character_id = enemy_char_id;
        enemy_char.name = "Amarr Militia Pilot".into();
        enemy_char.war_faction_id = Some(FACTION_AMARR);
        enemy_char.security_status = 5.0;
        snap.characters.insert(enemy_char_id, enemy_char);

        // Neutral pilot (no militia)
        let mut neutral_char = snap.characters.get(&char_id).unwrap().clone();
        neutral_char.character_id = neutral_char_id;
        neutral_char.name = "Neutral Trader".into();
        neutral_char.war_faction_id = None;
        neutral_char.security_status = 5.0;
        snap.characters.insert(neutral_char_id, neutral_char);
    });

    let now_unix = ctx.now_unix();
    // In low-sec (sys_sec = 0.35), check target legality for opposing militia member
    let is_enemy_legal = ctx.db.with_snapshot(|snap| {
        is_legal_war_or_suspect_target(snap, char_id, enemy_char_id, now_unix)
    });
    assert!(is_enemy_legal, "Opposing militia member must be legal war target in low-sec");

    let is_neutral_legal = ctx.db.with_snapshot(|snap| {
        is_legal_war_or_suspect_target(snap, char_id, neutral_char_id, now_unix)
    });
    assert!(!is_neutral_legal, "Neutral pilot is NOT a legal target");

    // Execute combat aggression against opposing militia member in low-sec
    let mut cw = ctx.crimewatch.write().unwrap();
    let enemy_eval = cw.handle_combat_aggression(
        char_id,
        enemy_char_id,
        1001,
        sys_id,
        0.35,
        is_enemy_legal,
        now_unix,
    );
    assert_eq!(enemy_eval, AggressionEvaluation::LegalEngagement);
    assert!(!cw.is_suspect(char_id, now_unix), "Attacking militia enemy must NOT trigger Suspect flag");
    assert!(!cw.get_or_create(char_id).is_criminal(now_unix), "Attacking militia enemy must NOT trigger Criminal flag");
    assert!(cw.get_or_create(char_id).pending_concord_arrival.is_none(), "No CONCORD for militia war target");

    // Contrast: attacking neutral pilot triggers Suspect flag in low-sec
    let neutral_eval = cw.handle_combat_aggression(
        char_id,
        neutral_char_id,
        1001,
        sys_id,
        0.35,
        is_neutral_legal,
        now_unix,
    );
    assert_eq!(neutral_eval, AggressionEvaluation::SuspectFlagTriggered);
    assert!(cw.is_suspect(char_id, now_unix), "Attacking neutral pilot in low-sec must trigger Suspect flag");
}
