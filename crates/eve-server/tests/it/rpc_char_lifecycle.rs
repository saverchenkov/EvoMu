//! Integration tests for character lifecycle charMgr queries (BF-04 remediation).

use std::collections::HashMap;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

#[tokio::test]
async fn test_char_mgr_cohorts_homestation_and_corp_roles() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
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

    // 1. GetCohortsForCharacter: derived from character creation and corp
    let cohorts_rep = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetCohortsForCharacter", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetCohortsForCharacter derived");
    let PyRep::List(c_list) = cohorts_rep else { panic!("expected List") };
    assert!(!c_list.is_empty(), "Derived cohort list must not be empty");
    let cohort_strs: Vec<String> = c_list.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
    assert!(cohort_strs.iter().any(|c| c.starts_with("cohort_created_")), "Must have creation year cohort");
    assert!(cohort_strs.iter().any(|c| c.starts_with("cohort_corp_")), "Must have corp cohort");

    // 2. GetCohortsForCharacter: persisted in service_state_blobs
    let custom_cohorts = vec!["cohort_alpha_tester".to_string(), "cohort_winter_2026".to_string()];
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "char_cohorts:90000001".to_string(),
            serde_json::to_string(&custom_cohorts).unwrap(),
        );
    });
    let custom_rep = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetCohortsForCharacter", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetCohortsForCharacter persisted");
    let PyRep::List(custom_list) = custom_rep else { panic!("expected List") };
    let custom_strs: Vec<String> = custom_list.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
    assert_eq!(custom_strs, custom_cohorts);

    // 3. GetHomeStationRow: SDE station lookup
    let home_station = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetHomeStationRow", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetHomeStationRow station");
    let PyRep::KeyVal(station_kv) = home_station else { panic!("expected KeyVal") };
    assert_eq!(station_kv.get("stationID"), Some(&PyRep::Integer(60_003_760)));
    assert_eq!(station_kv.get("typeID"), Some(&PyRep::Integer(52_678)));
    assert_eq!(station_kv.get("solarSystemID"), Some(&PyRep::Integer(30_000_142)));

    // 4. GetHomeStationRow: Upwell Structure lookup
    let upwell_struct_id: i64 = 1_030_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            upwell_struct_id,
            eve_db::types::UpwellStructureRecord {
                structure_id: upwell_struct_id,
                type_id: 35832, // Astrahus
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_144,
                name: "Fort Citadel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000_000.0,
                armor_hp: 10_000_000.0,
                hull_hp: 10_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.home_station_id = upwell_struct_id;
        }
    });

    let home_struct = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetHomeStationRow", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetHomeStationRow structure");
    let PyRep::KeyVal(struct_kv) = home_struct else { panic!("expected KeyVal") };
    assert_eq!(struct_kv.get("stationID"), Some(&PyRep::Integer(upwell_struct_id)));
    assert_eq!(struct_kv.get("typeID"), Some(&PyRep::Integer(35832)));
    assert_eq!(struct_kv.get("solarSystemID"), Some(&PyRep::Integer(30_000_144)));

    // 5. GetPrivateInfoOnCorpChange: roles from member_roles and char_scoped_roles
    let corp_id = 98_000_001;
    let expected_director_mask = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_ACCOUNTANT;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = corp_id;
        }
        let mut corp = eve_db::CorporationRecord {
            corporation_id: corp_id,
            name: "Test Corp".into(),
            ticker: "TC".into(),
            ceo_id: 90_000_999,
            ..Default::default()
        };
        corp.member_roles.insert(90_000_001, expected_director_mask);
        snap.corporations.insert(corp_id, corp);
        snap.char_scoped_roles.insert(
            90_000_001,
            eve_db::snapshot_ext::CharacterScopedRolesRecord {
                roles: vec!["Personnel_Manager".into()],
                roles_at_hq: vec![],
                roles_at_base: vec![],
                roles_at_other: vec![],
            },
        );
    });

    let priv_info = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetPrivateInfoOnCorpChange", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetPrivateInfoOnCorpChange with roles");
    let PyRep::KeyVal(priv_kv) = priv_info else { panic!("expected KeyVal") };
    assert_eq!(priv_kv.get("characterID"), Some(&PyRep::Integer(90_000_001)));
    assert_eq!(priv_kv.get("corporationID"), Some(&PyRep::Integer(corp_id)));
    let corp_dt = priv_kv.get("corporationDateTime").and_then(|v| v.as_i64()).expect("corporationDateTime integer");
    assert!(corp_dt > 0, "corporationDateTime must be positive FileTime ticks");
    let roles_val = priv_kv.get("roles").and_then(|v| v.as_i64()).expect("roles integer");
    let combined_mask = (expected_director_mask | eve_db::corp_roles::ROLE_PERSONNEL_MANAGER) as i64;
    assert_eq!(roles_val & combined_mask, combined_mask, "Roles mask must include assigned corp roles and scoped roles");
}

#[tokio::test]
async fn test_char_mgr_character_profile_info_and_lifecycle() {
    // parity: rpc:charMgr.GetCharacterCreationDate, rpc:charMgr.GetHomeStationRow, rpc:charMgr.GetOrganizationInfoForCharacters, rpc:charMgr.GetPrivateInfoOnCorpChange, rpc:charMgr.GetPublicInfo, rpc:charMgr.GetRecentShipKillsAndLosses, rpc:charMgr.SetActivityStatus, rpc:charMgr.GetCohortsForCharacter, rpc:charMgr.GetPaperdollState, rpc:charMgr.SetNote, rpc:charMgr.GetNote
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(1001);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
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

    // 1. GetCharacterCreationDate
    let cdate = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetCharacterCreationDate", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetCharacterCreationDate");
    let cdate_ft = cdate.as_i64().expect("creation filetime");
    assert!(cdate_ft > 116_444_736_000_000_000);

    // 2. GetHomeStationRow
    let home = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetHomeStationRow", &[], &kw, None)
        .await
        .expect("GetHomeStationRow");
    let PyRep::KeyVal(home_kv) = home else { panic!("expected KeyVal") };
    assert_eq!(home_kv.get("stationID"), Some(&PyRep::Integer(60_003_760)));
    assert_eq!(home_kv.get("typeID"), Some(&PyRep::Integer(52_678)));

    // 4. GetOrganizationInfoForCharacters
    let orgs = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetOrganizationInfoForCharacters",
            &[PyRep::List(vec![PyRep::Integer(90_000_001)])],
            &kw,
            None,
        )
        .await
        .expect("GetOrganizationInfoForCharacters");
    let PyRep::Dict(org_map) = orgs else { panic!("expected Dict") };
    assert_eq!(org_map.len(), 1);

    // 5. GetPrivateInfoOnCorpChange
    let priv_info = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetPrivateInfoOnCorpChange", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetPrivateInfoOnCorpChange");
    let PyRep::KeyVal(priv_kv) = priv_info else { panic!("expected KeyVal") };
    assert_eq!(priv_kv.get("characterID"), Some(&PyRep::Integer(90_000_001)));

    // 6. GetPublicInfo
    let pub_info = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetPublicInfo", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetPublicInfo");
    let PyRep::KeyVal(pub_kv) = pub_info else { panic!("expected KeyVal") };
    assert_eq!(pub_kv.get("characterID"), Some(&PyRep::Integer(90_000_001)));
    assert_eq!(pub_kv.get("characterName"), Some(&PyRep::String("Aura Capsuleer".into())));

    // 7. GetRecentShipKillsAndLosses (queries real killmails)
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(88001, eve_db::KillmailRecord {
            kill_id: 88001,
            victim_character_id: 90_000_001,
            attacker_character_id: 90_000_002,
            solar_system_id: 30000142,
            ship_type_id: 587,
            total_value_isk: 1_200_000.0,
            killed_unix: 1700000000,
            victim_corp_id: 98_000_001,
            victim_alliance_id: None,
            attacker_corp_id: 98_000_002,
            attacker_alliance_id: None,
        });
    });
    let kills = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetRecentShipKillsAndLosses", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetRecentShipKillsAndLosses");
    let PyRep::Rowset { columns, rows } = kills else { panic!("expected Rowset") };
    assert_eq!(columns, vec!["killID", "victimID", "solarSystemID", "killTime"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], PyRep::Integer(88001));
    assert_eq!(rows[0][1], PyRep::Integer(90_000_001));

    // 8. SetActivityStatus (persists to snapshot and across SQLite reload)
    let mut kw_act = HashMap::new();
    kw_act.insert("status".to_string(), PyRep::String("online".into()));
    let act = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "SetActivityStatus", &[], &kw_act, None)
        .await
        .expect("SetActivityStatus");
    assert_eq!(act, PyRep::None);
    let status_val = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&90_000_001).unwrap().activity_status.clone()
    });
    assert_eq!(status_val, Some("online".into()));
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "char_activity_status_reload");
    let reloaded_status = ctx_reloaded.db.with_snapshot(|snap| {
        snap.characters.get(&90_000_001).unwrap().activity_status.clone()
    });
    assert_eq!(reloaded_status, Some("online".into()));

    // 9. GetCohortsForCharacter
    let cohorts = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetCohortsForCharacter", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("GetCohortsForCharacter");
    let PyRep::List(c_list) = cohorts else { panic!("expected List") };
    assert!(!c_list.is_empty(), "Cohorts must be derived from character data");

    // 10. SetSessionCheck & clear_memoized
    let sc = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "SetSessionCheck", &[], &kw, None)
        .await
        .expect("SetSessionCheck");
    assert_eq!(sc, PyRep::Bool(true));

    let cm = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "clear_memoized", &[], &kw, None)
        .await
        .expect("clear_memoized");
    assert_eq!(cm, PyRep::Bool(true));

    // 11. GetPaperdollState
    let pd = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetPaperdollState", &[], &kw, None)
        .await
        .expect("GetPaperdollState");
    assert_eq!(pd, PyRep::Integer(0));

    // 12. SetNote & GetNote
    let sn = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "SetNote", &[PyRep::Integer(90_000_002), PyRep::String("Ally scout".into())], &kw, None)
        .await
        .expect("SetNote");
    assert_eq!(sn, PyRep::Bool(true));

    let gn = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetNote", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("GetNote");
    assert_eq!(gn, PyRep::String("Ally scout".into()));
}
