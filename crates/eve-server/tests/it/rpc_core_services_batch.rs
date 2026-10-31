//! Parity test suite for core RPC services batch (Part 1: `lookupSvc` and `charUnboundMgr`).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

fn as_list(rep: &PyRep) -> &[PyRep] {
    match rep {
        PyRep::List(ref l) => l.as_slice(),
        _ => panic!("expected List, got {rep:?}"),
    }
}

#[tokio::test]
async fn test_rpc_lookup_svc_batch() {
    // parity: rpc:lookupSvc.LookupCharacters, rpc:lookupSvc.LookupCorporations, rpc:lookupSvc.LookupEvePlayerCharacters, rpc:lookupSvc.LookupFactions, rpc:lookupSvc.LookupKnownLocationsByGroup, rpc:lookupSvc.LookupLocationsByGroup, rpc:lookupSvc.LookupNoneNPCAccountOwners, rpc:lookupSvc.LookupOwners, rpc:lookupSvc.LookupPCOwners, rpc:lookupSvc.LookupWarableCorporationsOrAlliances
    // parity: feat:ui.lookup
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_000_001,
            eve_db::CorporationRecord {
                corporation_id: 98_000_001,
                name: "Federation Defense Corp".into(),
                ticker: "FED".into(),
                ceo_id: 90_000_001,
                alliance_id: Some(99_000_001),
                tax_rate: 0.05,
                master_wallet: eve_db::IskCents::from_isk(1_000_000),
                members: HashSet::from([90_000_001]),
                member_roles: HashMap::new(),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(60_003_760),
            },
        );
        snap.alliances.insert(
            99_000_001,
            eve_db::AllianceRecord {
                alliance_id: 99_000_001,
                name: "Federation Allied Forces".into(),
                short_name: "FEDAF".into(),
                executor_corp_id: 98_000_001,
                member_corp_ids: HashSet::from([98_000_001]),
            },
        );
    });

    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();

    // 1. LookupCharacters
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupCharacters",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupCharacters");
    let chars = as_list(&res);
    assert!(!chars.is_empty());
    let char_name = chars[0]
        .dict_get("characterName")
        .and_then(PyRep::as_str)
        .expect("characterName");
    assert!(char_name.to_ascii_lowercase().contains("aura"));

    // 2. LookupEvePlayerCharacters
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupEvePlayerCharacters",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupEvePlayerCharacters");
    let pc_chars = as_list(&res);
    assert!(!pc_chars.is_empty());
    let cid = pc_chars[0]
        .dict_get("characterID")
        .and_then(PyRep::as_i64)
        .expect("characterID");
    assert!(cid >= 90_000_000);

    // 3. LookupCorporations
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupCorporations",
            &[PyRep::String("Federation".into())],
            &kw,
            None,
        )
        .await
        .expect("LookupCorporations");
    let corps = as_list(&res);
    assert!(!corps.is_empty());
    let corp_type = corps[0]
        .dict_get("corporationType")
        .and_then(PyRep::as_i64)
        .expect("corporationType");
    assert!(corp_type == 1 || corp_type == 2);

    // 4. LookupFactions
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupFactions",
            &[PyRep::String("Caldari".into())],
            &kw,
            None,
        )
        .await
        .expect("LookupFactions");
    let factions = as_list(&res);
    assert!(!factions.is_empty());
    let faction_name = factions[0]
        .dict_get("factionName")
        .and_then(PyRep::as_str)
        .expect("factionName");
    assert!(faction_name.contains("Caldari"));

    // 5. LookupKnownLocationsByGroup
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupKnownLocationsByGroup",
            &[
                PyRep::Integer(5),
                PyRep::String("Jita".into()),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("LookupKnownLocationsByGroup");
    let locs = as_list(&res);
    assert!(!locs.is_empty());
    let gid = locs[0]
        .dict_get("groupID")
        .and_then(PyRep::as_i64)
        .expect("groupID");
    assert_eq!(gid, 5);
    let item_name = locs[0]
        .dict_get("itemName")
        .and_then(PyRep::as_str)
        .expect("itemName");
    assert_eq!(item_name, "Jita");

    // 6. LookupLocationsByGroup (Solar Systems group 5, Stations group 15)
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupLocationsByGroup",
            &[
                PyRep::Integer(5),
                PyRep::String("Jita".into()),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("LookupLocationsByGroup solar system");
    let locs2 = as_list(&res);
    assert!(!locs2.is_empty());
    assert_eq!(locs2[0].dict_get("groupID"), Some(&PyRep::Integer(5)));
    assert_eq!(locs2[0].dict_get("itemID"), Some(&PyRep::Integer(30_000_142)));
    assert_eq!(locs2[0].dict_get("itemName"), Some(&PyRep::String("Jita".into())));

    let res_st = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupLocationsByGroup",
            &[
                PyRep::Integer(15),
                PyRep::String("Caldari Navy Assembly".into()),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("LookupLocationsByGroup station");
    let locs_st = as_list(&res_st);
    assert!(!locs_st.is_empty());
    assert_eq!(locs_st[0].dict_get("groupID"), Some(&PyRep::Integer(15)));
    assert!(locs_st.iter().any(|st| st.dict_get("itemID") == Some(&PyRep::Integer(60_003_760))));

    // 7. LookupNoneNPCAccountOwners
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupNoneNPCAccountOwners",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupNoneNPCAccountOwners");
    let non_npc = as_list(&res);
    assert!(!non_npc.is_empty());
    let gid_non_npc = non_npc[0]
        .dict_get("groupID")
        .and_then(PyRep::as_i64)
        .expect("groupID");
    assert!(gid_non_npc == 1 || gid_non_npc == 2);

    // 8. LookupOwners (Characters group 1, Corporations group 2, Alliances group 32)
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupOwners",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupOwners character");
    let owners = as_list(&res);
    assert!(!owners.is_empty());
    assert_eq!(owners[0].dict_get("groupID"), Some(&PyRep::Integer(1)));
    let owner_name = owners[0]
        .dict_get("ownerName")
        .and_then(PyRep::as_str)
        .expect("ownerName");
    assert!(owner_name.to_ascii_lowercase().contains("aura"));

    let res_corp_owner = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupOwners",
            &[PyRep::String("Federation Defense".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupOwners corp");
    let corp_owners = as_list(&res_corp_owner);
    assert!(!corp_owners.is_empty());
    assert_eq!(corp_owners[0].dict_get("ownerID"), Some(&PyRep::Integer(98_000_001)));
    assert_eq!(corp_owners[0].dict_get("groupID"), Some(&PyRep::Integer(2)));

    // 9. LookupPCOwners
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupPCOwners",
            &[PyRep::String("Aura".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupPCOwners");
    let pc_owners = as_list(&res);
    assert!(!pc_owners.is_empty());
    let oid = pc_owners[0]
        .dict_get("ownerID")
        .and_then(PyRep::as_i64)
        .expect("ownerID");
    assert!(oid >= 90_000_000);

    // 10. LookupWarableCorporationsOrAlliances (Player corps group 2, Alliances group 32)
    sess.corporation_id = Some(98_000_001);
    sess.alliance_id = Some(99_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_000_050,
            eve_db::CorporationRecord {
                corporation_id: 98_000_050,
                name: "Target Federation Corp".into(),
                ticker: "TFC".into(),
                ceo_id: 90_000_050,
                ..Default::default()
            },
        );
        snap.corporations.insert(
            98_000_051,
            eve_db::CorporationRecord {
                corporation_id: 98_000_051,
                name: "Peaceful Federation Corp".into(),
                ticker: "PFC".into(),
                ceo_id: 90_000_051,
                alliance_id: Some(99_000_051),
                ..Default::default()
            },
        );
        snap.structures.insert(
            1_000_000_050_001,
            eve_db::UpwellStructureRecord {
                structure_id: 1_000_000_050_001,
                type_id: 35832,
                owner_corp_id: 98_000_050,
                solar_system_id: 30_000_142,
                name: "TFC Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.alliances.insert(
            99_000_050,
            eve_db::AllianceRecord {
                alliance_id: 99_000_050,
                name: "Target Federation Alliance".into(),
                short_name: "TFA".into(),
                executor_corp_id: 98_000_050,
                member_corp_ids: [98_000_050].into_iter().collect(),
            },
        );
        snap.alliances.insert(
            99_000_051,
            eve_db::AllianceRecord {
                alliance_id: 99_000_051,
                name: "Peaceful Federation Alliance".into(),
                short_name: "PFA".into(),
                executor_corp_id: 98_000_051,
                member_corp_ids: [98_000_051].into_iter().collect(),
            },
        );
    });
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupWarableCorporationsOrAlliances",
            &[PyRep::String("Federation".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupWarableCorporationsOrAlliances");
    let warable = as_list(&res);
    let own_corp_id = sess.corporation_id.unwrap_or(98_000_001);
    let has_own_corp = warable.iter().any(|w| w.dict_get("ownerID") == Some(&PyRep::Integer(own_corp_id)));
    let has_peaceful_corp = warable.iter().any(|w| w.dict_get("ownerID") == Some(&PyRep::Integer(98_000_051)));
    let has_peaceful_alliance = warable.iter().any(|w| w.dict_get("ownerID") == Some(&PyRep::Integer(99_000_051)));
    let has_target_corp = warable.iter().any(|w| w.dict_get("ownerID") == Some(&PyRep::Integer(98_000_050)) && w.dict_get("groupID") == Some(&PyRep::Integer(2)));
    let has_target_alliance = warable.iter().any(|w| w.dict_get("ownerID") == Some(&PyRep::Integer(99_000_050)) && w.dict_get("groupID") == Some(&PyRep::Integer(32)));
    assert!(!has_own_corp, "Caller's own corp must be excluded");
    assert!(!has_peaceful_corp, "Corp without Upwell structures must be excluded even if in an alliance");
    assert!(!has_peaceful_alliance, "Alliance with no structure-owning member corps must be excluded");
    assert!(has_target_corp, "Must find warable corp with Upwell structure");
    assert!(has_target_alliance, "Must find warable alliance");
}

#[tokio::test]
async fn test_rpc_char_unbound_mgr_batch() {
    // parity: rpc:charUnboundMgr.CancelCharacterDeletePrepare, rpc:charUnboundMgr.DeleteCharacter, rpc:charUnboundMgr.GetCharOmegaDowngradeStatus, rpc:charUnboundMgr.GetCharacterLockType, rpc:charUnboundMgr.GetCohortsForUser, rpc:charUnboundMgr.GetNumCharacters, rpc:charUnboundMgr.GetQAStarterSystemIDs, rpc:charUnboundMgr.PrepareCharacterForDelete, rpc:charUnboundMgr.ToggleValidation, rpc:charUnboundMgr.UpdateCharacterBloodline, rpc:charUnboundMgr.UpdateCharacterGender
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut sess = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();

    // 1. GetNumCharacters
    let res = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetNumCharacters", &[], &kw, None)
        .await
        .expect("GetNumCharacters");
    let num_chars = match &res {
        PyRep::Tuple(v) => v.first().and_then(PyRep::as_i64).expect("num characters int"),
        other => other.as_i64().expect("num characters int"),
    };
    assert!(num_chars >= 1);

    // 2. GetCharOmegaDowngradeStatus
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharOmegaDowngradeStatus",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharOmegaDowngradeStatus");
    assert_eq!(res.as_bool(), Some(false));

    // 3. GetCharacterLockType
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterLockType",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterLockType");
    assert!(matches!(res, PyRep::None));

    // 4. GetCohortsForUser
    let res = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCohortsForUser", &[], &kw, None)
        .await
        .expect("GetCohortsForUser");
    let cohorts = as_list(&res);
    assert!(!cohorts.is_empty());

    // 5. GetQAStarterSystemIDs
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetQAStarterSystemIDs",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetQAStarterSystemIDs");
    let starter_ids = as_list(&res);
    assert_eq!(starter_ids.len(), 4);
    assert_eq!(starter_ids[0].as_i64(), Some(30_000_142));

    // 6. ToggleValidation
    let res = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "ToggleValidation", &[], &kw, None)
        .await
        .expect("ToggleValidation");
    assert_eq!(res.as_bool(), Some(true));

    // 7. UpdateCharacterBloodline (unauthorized fails, owned succeeds)
    let unauth_res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "UpdateCharacterBloodline",
            &[PyRep::Integer(99_999_999), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await;
    assert!(unauth_res.is_err());

    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.insert("resculpt_pending:90000001".into(), "true".into());
    });
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "UpdateCharacterBloodline",
            &[PyRep::Integer(90_000_001), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("UpdateCharacterBloodline");
    assert_eq!(res.as_bool(), Some(true));
    let bloodline = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().bloodline_id);
    assert_eq!(bloodline, 2);

    // 8. UpdateCharacterGender (unauthorized fails, owned succeeds)
    let unauth_res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "UpdateCharacterGender",
            &[PyRep::Integer(99_999_999), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await;
    assert!(unauth_res.is_err());

    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.insert("resculpt_pending:90000001".into(), "true".into());
    });
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "UpdateCharacterGender",
            &[PyRep::Integer(90_000_001), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("UpdateCharacterGender");
    assert_eq!(res.as_bool(), Some(true));
    let gender = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().gender);
    assert_eq!(gender, 1);

    // 9. PrepareCharacterForDelete
    let temp_char_id = 90_999_999;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get(&90_000_001).cloned() {
            let mut temp = c;
            temp.character_id = temp_char_id;
            temp.name = "Temp Delete Candidate".into();
            snap.characters.insert(temp_char_id, temp);
        }
    });

    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "PrepareCharacterForDelete",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await
        .expect("PrepareCharacterForDelete");
    let del_date = res.as_i64().expect("deleteDate");
    assert!(del_date > 0);

    // 10. CancelCharacterDeletePrepare
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CancelCharacterDeletePrepare",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await
        .expect("CancelCharacterDeletePrepare");
    assert_eq!(res.as_bool(), Some(true));

    // 11. DeleteCharacter
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "PrepareCharacterForDelete",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await
        .expect("PrepareCharacterForDelete 2");

    let premature_del = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "DeleteCharacter",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await;
    assert!(premature_del.is_err());

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = eve_server::services::character_lifecycle::load_store(snap);
        store
            .delete_prepared_unix
            .insert(temp_char_id, ctx.now_unix().saturating_sub(36000 + 10));
        eve_server::services::character_lifecycle::save_store(snap, &store);
    });

    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "DeleteCharacter",
            &[PyRep::Integer(temp_char_id)],
            &kw,
            None,
        )
        .await
        .expect("DeleteCharacter");
    assert!(matches!(res, PyRep::KeyVal(_) | PyRep::Bool(true)));
    let is_biomassed = ctx
        .db
        .with_snapshot(|s| s.characters.get(&temp_char_id).unwrap().user_id == 0);
    assert!(is_biomassed);
}
