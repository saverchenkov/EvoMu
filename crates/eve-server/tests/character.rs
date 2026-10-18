//! Character management, standings, clone bays, and clone jump integration tests.

#![forbid(unsafe_code)]

mod common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s4_split_shared_handlers_and_jump_clones() {
    // parity: plan:S-4
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(904);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // 1. charUnboundMgr: GetCharactersToSelect vs GetCharacterInfo(char_id)
    let chars_list = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharactersToSelect");
    assert!(matches!(chars_list, PyRep::Rowset { .. }));
    let char_info_missing_arg = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        char_info_missing_arg.unwrap_err().code,
        "CharacterNotSelected"
    );
    let char_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterInfo(90_000_001)");
    assert!(matches!(char_info, PyRep::KeyVal(_)));

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

    // 2. standingMgr: GetCharStandings vs GetStandingTransactions
    let standings_rs = ctx
        .dispatch_rpc(
            &mut sess,
            "standingMgr",
            None,
            "GetCharStandings",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharStandings");
    let PyRep::Rowset {
        columns: st_hdr, ..
    } = standings_rs
    else {
        panic!("Expected Rowset from GetCharStandings");
    };
    assert_eq!(st_hdr, vec!["fromID", "standing"]);

    let st_tx_rs = ctx
        .dispatch_rpc(
            &mut sess,
            "standingMgr",
            None,
            "GetStandingTransactions",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetStandingTransactions");
    let PyRep::Rowset {
        columns: tx_hdr, ..
    } = st_tx_rs
    else {
        panic!("Expected Rowset from GetStandingTransactions");
    };
    assert_eq!(
        tx_hdr,
        vec!["eventID", "fromID", "toID", "modification", "eventTypeID"]
    );

    // 3. machoNet: GetInitVals vs GetServiceInfo
    let init_vals = ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetInitVals", &[], &kw, None)
        .await
        .expect("GetInitVals");
    assert!(matches!(init_vals, PyRep::Tuple(_)));
    let svc_info = ctx
        .dispatch_rpc(
            &mut sess,
            "machoNet",
            None,
            "GetServiceInfo",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetServiceInfo");
    assert!(matches!(svc_info, PyRep::Dict(_)));

    // 4. subscriptionMgr / loginRewardFacilities: GetCloneGrade vs get_my_clone_grade
    let clone_grade_kv = ctx
        .dispatch_rpc(
            &mut sess,
            "subscriptionMgr",
            None,
            "GetCloneGrade",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCloneGrade");
    assert!(matches!(clone_grade_kv, PyRep::KeyVal(_)));
    let clone_grade_int = ctx
        .dispatch_rpc(
            &mut sess,
            "loginRewardFacilities",
            None,
            "get_my_clone_grade",
            &[],
            &kw,
            None,
        )
        .await
        .expect("get_my_clone_grade");
    assert_eq!(clone_grade_int, PyRep::Integer(2));

    // 5. jumpCloneSvc: GetPriceForClone, InstallCloneInStation (station clone bay check),
    // InstallCloneInStructure, GetCloneState vs GetStationCloneState, CloneJump (implant swap + departure bay check + cooldown)
    let price_resp = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "GetPriceForClone",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetPriceForClone");
    assert_eq!(price_resp, PyRep::Real(900_000.0));

    let struct_not_docked = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStructure",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(struct_not_docked.unwrap_err().code, "MustBeDocked");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_000_361); // Jita IV - Moon 6 - Ytiri Storage (operation_id 26, no clone bay)
        }
    });
    let no_bay_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(no_bay_err.unwrap_err().code, "StationHasNoCloneBay");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
        }
        snap.skills.entry(90_000_001).or_default().insert(
            24242,
            eve_db::TrainedSkillRecord {
                type_id: 24242,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    let jc_install = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await
        .expect("InstallCloneInStation at 60003760");
    let PyRep::KeyVal(jc_kv) = jc_install else {
        panic!("Expected KeyVal from InstallCloneInStation");
    };
    let jc_id = jc_kv.get("jumpCloneID").unwrap().as_i64().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.insert(90_000_001, vec![9899]);
        if let Some(clones) = snap.jump_clones.get_mut(&90_000_001) {
            if let Some(c) = clones.iter_mut().find(|c| c.jump_clone_id == jc_id) {
                c.location_id = 60_008_494;
                c.implants = vec![9941];
            }
        }
    });

    // Attempting CloneJump while docked at no-bay station 60000361 must reject so no clone is left at a bay-less station
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_000_361);
        }
    });
    let jump_from_no_bay = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(jump_from_no_bay.unwrap_err().code, "StationHasNoCloneBay");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
        }
    });
    let state_all = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "GetCloneState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCloneState");
    let PyRep::Rowset { rows: all_rows, .. } = state_all else {
        panic!("Expected Rowset from GetCloneState");
    };
    assert_eq!(all_rows.len(), 1);
    let state_station = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "GetStationCloneState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetStationCloneState");
    let PyRep::Rowset {
        rows: station_rows, ..
    } = state_station
    else {
        panic!("Expected Rowset from GetStationCloneState");
    };
    assert_eq!(station_rows.len(), 0);

    let jump_swap = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await
        .expect("CloneJump implant swap");
    let PyRep::KeyVal(js_kv) = jump_swap else {
        panic!("Expected KeyVal from CloneJump");
    };
    assert_eq!(
        js_kv.get("activeImplants"),
        Some(&PyRep::List(vec![PyRep::Integer(9941)]))
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_implants.get(&90_000_001), Some(&vec![9941]));
        let left_behind = snap.jump_clones[&90_000_001]
            .iter()
            .find(|c| c.jump_clone_id == jc_id)
            .unwrap();
        assert_eq!(left_behind.location_id, 60_003_760);
        assert_eq!(left_behind.implants, vec![9899]);
        let ch = &snap.characters[&90_000_001];
        assert_eq!(ch.station_id, Some(60_008_494));
        assert_eq!(ch.solar_system_id, 30_002_187);
    });

    let cooldown_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(cooldown_err.unwrap_err().code, "CloneJumpCooldownActive");
}
