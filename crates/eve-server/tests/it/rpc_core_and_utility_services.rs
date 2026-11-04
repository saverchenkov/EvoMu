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
    // Caller's own corp/alliance and structureless corps must be excluded; war-eligible targets returned.
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
        snap.corporations.insert(
            98_000_077,
            eve_db::CorporationRecord {
                corporation_id: 98_000_077,
                name: "Federation War Target Corp".into(),
                ticker: "FWTC".into(),
                ceo_id: 90_000_077,
                alliance_id: Some(99_000_077),
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
        snap.structures.insert(
            1_030_000_077,
            eve_db::UpwellStructureRecord {
                structure_id: 1_030_000_077,
                type_id: 35832, // Astrahus
                owner_corp_id: 98_000_077,
                solar_system_id: 30_000_142,
                name: "Target Astrahus".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
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
        snap.alliances.insert(
            99_000_077,
            eve_db::AllianceRecord {
                alliance_id: 99_000_077,
                name: "Federation War Target Alliance".into(),
                short_name: "FWTA".into(),
                executor_corp_id: 98_000_077,
                member_corp_ids: [98_000_077].into_iter().collect(),
                ..Default::default()
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
    let has_corp = warable.iter().any(|w| w.dict_get("ownerID") == Some(&PyRep::Integer(98_000_077)) && w.dict_get("groupID") == Some(&PyRep::Integer(2)));
    let has_alliance = warable.iter().any(|w| w.dict_get("ownerID") == Some(&PyRep::Integer(99_000_077)) && w.dict_get("groupID") == Some(&PyRep::Integer(32)));
    assert!(!has_own_corp, "Caller's own corp must be excluded");
    assert!(!has_peaceful_corp, "Corp without Upwell structures must be excluded even if in an alliance");
    assert!(!has_peaceful_alliance, "Alliance with no structure-owning member corps must be excluded");
    assert!(has_target_corp, "Must find warable corp with Upwell structure");
    assert!(has_target_alliance, "Must find warable alliance");
    assert!(!has_corp, "Alliance-member corp must not be warable directly (declare war on its alliance instead)");
    assert!(has_alliance, "Must find warable target alliance with structure");
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

pub(crate) mod rpc_pvp_filament_mgr {
    // Parity test suite for core RPC services batch (Part 2: `pvpFilamentMgr` and `mailingListsMgr`).


    use std::collections::HashMap;

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

    fn as_dict(rep: &PyRep) -> &[(PyRep, PyRep)] {
        match rep {
            PyRep::Dict(ref d) => d.as_slice(),
            _ => panic!("expected Dict, got {rep:?}"),
        }
    }

    #[tokio::test]
    async fn test_rpc_pvp_filament_mgr_batch() {
        // parity: rpc:pvpFilamentMgr.AbyssalPVPEndGateActivation, rpc:pvpFilamentMgr.GetActiveEvents, rpc:pvpFilamentMgr.GetAllEvents, rpc:pvpFilamentMgr.GetCharacterStatistics, rpc:pvpFilamentMgr.GetLeaderboard, rpc:pvpFilamentMgr.GetMostRecentEvent, rpc:pvpFilamentMgr.GetNextEventDate, rpc:pvpFilamentMgr.JoinPVPQueue, rpc:pvpFilamentMgr.LeavePVPQueue
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let mut sess = SessionState::new(101);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();
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
        sess.station_id = None;
        sess.structure_id = None;
        sess.solar_system_id = Some(30_000_142);
        sess.ship_id = Some(100_000_001);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = 30_000_142;
            }
        });
        // 1. JoinPVPQueue (verify inventory check, consumption, and persistent state)
        let filament_item_id = 88_888_001;
        let hangar_filament_id = 88_888_002;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                filament_item_id,
                eve_db::InventoryItemRecord {
                    item_id: filament_item_id,
                    type_id: 53965,
                    owner_id: 90_000_001,
                    location_id: 100_000_001,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                hangar_filament_id,
                eve_db::InventoryItemRecord {
                    item_id: hangar_filament_id,
                    type_id: 53965,
                    owner_id: 90_000_001,
                    location_id: 100_000_001,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });

        // Non-positive item_id fails immediately
        let zero_item_res = ctx
            .dispatch_rpc(
                &mut sess,
                "pvpFilamentMgr",
                None,
                "JoinPVPQueue",
                &[PyRep::Integer(0), PyRep::Integer(0)],
                &kw,
                None,
            )
            .await;
        assert!(zero_item_res.is_err());

        // Filament in Hangar flag is rejected
        let hangar_res = ctx
            .dispatch_rpc(
                &mut sess,
                "pvpFilamentMgr",
                None,
                "JoinPVPQueue",
                &[PyRep::Integer(hangar_filament_id), PyRep::Integer(53965)],
                &kw,
                None,
            )
            .await;
        assert!(hangar_res.is_err());
        assert_eq!(hangar_res.unwrap_err().code, "ItemNotInCargo");

        // Unowned item fails
        let unowned_res = ctx
            .dispatch_rpc(
                &mut sess,
                "pvpFilamentMgr",
                None,
                "JoinPVPQueue",
                &[PyRep::Integer(99_999_999), PyRep::Integer(53965)],
                &kw,
                None,
            )
            .await;
        assert!(unowned_res.is_err());

        // Owned item succeeds and consumes filament
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "pvpFilamentMgr",
                None,
                "JoinPVPQueue",
                &[PyRep::Integer(filament_item_id), PyRep::Integer(53965)],
                &kw,
                None,
            )
            .await
            .expect("JoinPVPQueue");
        assert_eq!(res, PyRep::Integer(30));

        let item_consumed = ctx.db.with_snapshot(|s| !s.items.contains_key(&filament_item_id));
        assert!(item_consumed);

        let in_queue = ctx.db.with_snapshot(|snap| {
            let store = eve_server::services::pvp_filament_mgr::load_pvp_store(snap);
            store.queue.contains_key(&90_000_001)
        });
        assert!(in_queue, "Character must be present in queue state after JoinPVPQueue");

        // 2. LeavePVPQueue
        let res = ctx
            .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "LeavePVPQueue", &[], &kw, None)
            .await
            .expect("LeavePVPQueue");
        assert!(matches!(res, PyRep::None));

        let in_queue_after = ctx.db.with_snapshot(|snap| {
            let store = eve_server::services::pvp_filament_mgr::load_pvp_store(snap);
            store.queue.contains_key(&90_000_001)
        });
        assert!(
            !in_queue_after,
            "Character must be removed from queue state after LeavePVPQueue"
        );

        ctx.db.with_mut_snapshot(|snap| {
            let mut store = eve_server::services::pvp_filament_mgr::load_pvp_store(snap);
            store.stats.insert(90_000_001, (5, 1, 50, 6, 12000.0));
            eve_server::services::pvp_filament_mgr::save_pvp_store(snap, &store);
        });

        // 3. GetAllEvents (83 non-QA "ccp" region events from pvpfilamenteventdates.fsdbinary)
        let res = ctx
            .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "GetAllEvents", &[], &kw, None)
            .await
            .expect("GetAllEvents");
        let all_events = as_dict(&res);
        assert_eq!(all_events.len(), 83);
        let ev_170 = all_events
            .iter()
            .find_map(|(k, v)| (k.as_i64() == Some(170)).then_some(v))
            .expect("event 170 present in SDE schedule");
        assert_eq!(ev_170.dict_get("name").and_then(PyRep::as_str), Some("2v2 Battleships Proving Filament"));
        assert_eq!(ev_170.dict_get("matchTypeID").and_then(PyRep::as_i64), Some(128));

        // 4. GetActiveEvents (at 2023-01-07T12:00:00Z event 170 is active; after 2023-01-16 none are active)
        ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_673_092_800)));
        let res = ctx
            .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "GetActiveEvents", &[], &kw, None)
            .await
            .expect("GetActiveEvents");
        let act_events = as_dict(&res);
        assert_eq!(act_events.len(), 1);
        assert_eq!(act_events[0].1.dict_get("eventID").and_then(PyRep::as_i64), Some(170));
        assert_eq!(act_events[0].1.dict_get("matchTypeID").and_then(PyRep::as_i64), Some(128));

        // 5. GetMostRecentEvent (at 2023-01-07T12:00:00Z returns event 170)
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "pvpFilamentMgr",
                None,
                "GetMostRecentEvent",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetMostRecentEvent");
        let ev_id = res
            .dict_get("eventID")
            .and_then(PyRep::as_i64)
            .expect("eventID");
        assert_eq!(ev_id, 170);

        // 6. GetNextEventDate (at 2023-01-07T12:00:00Z returns 2023-01-14T00:00:00Z for event 171)
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "pvpFilamentMgr",
                None,
                "GetNextEventDate",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetNextEventDate");
        assert_eq!(res.as_i64(), Some(133_181_280_000_000_000));

        // 7. GetLeaderboard
        let res = ctx
            .dispatch_rpc(&mut sess, "pvpFilamentMgr", None, "GetLeaderboard", &[], &kw, None)
            .await
            .expect("GetLeaderboard");
        let lb = as_list(&res);
        assert!(!lb.is_empty());
        let score = lb[0].dict_get("score").and_then(PyRep::as_i64).expect("score");
        assert_eq!(score, 50);

        // 8. GetCharacterStatistics
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "pvpFilamentMgr",
                None,
                "GetCharacterStatistics",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetCharacterStatistics");
        let stats_cid = res
            .dict_get("characterID")
            .and_then(PyRep::as_i64)
            .expect("characterID");
        assert_eq!(stats_cid, 90_000_001);
        let wins = res.dict_get("wins").and_then(PyRep::as_i64).expect("wins");
        assert_eq!(wins, 5);
        let dmg = res
            .dict_get("damageDealt")
            .and_then(PyRep::as_f64)
            .expect("damageDealt");
        assert_eq!(dmg, 12000.0);

        // 9. AbyssalPVPEndGateActivation
        sess.solar_system_id = Some(32000001);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.solar_system_id = 32000001;
                ch.active_ship_id = 100_000_001;
            }
            let mut store = eve_server::services::pvp_filament_mgr::load_pvp_store(snap);
            let mut origin_systems = HashMap::new();
            origin_systems.insert(90_000_001, 30000142);
            store.active_matches.insert(999, eve_server::services::pvp_filament_mgr::PvpArenaMatchInstance {
                match_id: 999,
                event_id: 2,
                arena_solar_system_id: 32000001,
                team_a: vec![90_000_001],
                team_b: vec![90_000_002],
                origin_systems,
                started_unix: 1000,
                active: false,
                winner_char_id: Some(90_000_001),
                exited_pilots: std::collections::HashSet::new(),
                end_gate_id: 70001,
            });
            eve_server::services::pvp_filament_mgr::save_pvp_store(snap, &store);
        });
        let arena = ctx.systems.get_or_spawn(32000001);
        let _ = arena.spawn_ball(eve_destiny::Ball {
            id: 70001,
            type_id: 47459,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await;
        let _ = arena.spawn_ball(eve_destiny::Ball {
            id: 100_000_001,
            type_id: 621,
            position: glam::DVec3::new(1000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 0.5,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await;

        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "pvpFilamentMgr",
                None,
                "AbyssalPVPEndGateActivation",
                &[PyRep::Integer(70001)],
                &kw,
                None,
            )
            .await
            .expect("AbyssalPVPEndGateActivation");
        let gid = res.dict_get("gateID").and_then(PyRep::as_i64).expect("gateID");
        assert_eq!(gid, 70001);
        let activated = res
            .dict_get("activated")
            .and_then(PyRep::as_bool)
            .expect("activated");
        assert!(activated);
        let osys = res.dict_get("originSystemID").and_then(PyRep::as_i64).expect("originSystemID");
        assert_eq!(osys, 30000142);
        assert_eq!(sess.solar_system_id, Some(30000142));
    }

    #[tokio::test]
    async fn test_rpc_mailing_lists_mgr_batch() {
        // parity: rpc:mailingListsMgr.Delete, rpc:mailingListsMgr.GetInfo, rpc:mailingListsMgr.GetSettings, rpc:mailingListsMgr.Leave, rpc:mailingListsMgr.SendWelcomeMail, rpc:mailingListsMgr.SetDefaultAccess, rpc:mailingListsMgr.SetEntitiesAccess, rpc:mailingListsMgr.SetMembersClear
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let mut sess = SessionState::new(101);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();
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

        let create_res = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "Create",
                &[PyRep::String("Alliance Fleet Broadcast".into())],
                &kw,
                None,
            )
            .await
            .expect("Create");
        let list_id = create_res
            .as_i64()
            .or_else(|| create_res.dict_get("listID").and_then(PyRep::as_i64))
            .expect("created listID");
        assert!(list_id > 0);

        // 1. GetInfo
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "GetInfo",
                &[PyRep::Integer(list_id)],
                &kw,
                None,
            )
            .await
            .expect("GetInfo");
        let lid = res.dict_get("listID").and_then(PyRep::as_i64).expect("listID");
        assert_eq!(lid, list_id);
        let list_name = res
            .dict_get("name")
            .and_then(PyRep::as_str)
            .expect("list name");
        assert_eq!(list_name, "Alliance Fleet Broadcast");

        // 2. SetDefaultAccess
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "SetDefaultAccess",
                &[PyRep::Integer(list_id), PyRep::Integer(2)],
                &kw,
                None,
            )
            .await
            .expect("SetDefaultAccess");
        assert_eq!(res.dict_get("defaultAccess").and_then(PyRep::as_i64), Some(2));

        // 3. SetEntitiesAccess
        let entity_access_dict = vec![(PyRep::Integer(90_000_002), PyRep::Integer(1))];
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "SetEntitiesAccess",
                &[PyRep::Integer(list_id), PyRep::Dict(entity_access_dict)],
                &kw,
                None,
            )
            .await
            .expect("SetEntitiesAccess");
        assert_eq!(res.dict_get("listID").and_then(PyRep::as_i64), Some(list_id));

        // 4. GetSettings
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "GetSettings",
                &[PyRep::Integer(list_id)],
                &kw,
                None,
            )
            .await
            .expect("GetSettings");
        let def_access = res
            .dict_get("defaultAccess")
            .and_then(PyRep::as_i64)
            .expect("defaultAccess");
        assert_eq!(def_access, 2);

        // 5. SetMembersClear
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "SetMembersClear",
                &[
                    PyRep::Integer(list_id),
                    PyRep::List(vec![PyRep::Integer(90_000_001)]),
                ],
                &kw,
                None,
            )
            .await
            .expect("SetMembersClear");
        assert_eq!(res.as_bool(), Some(true));

        // 6. SendWelcomeMail
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "SendWelcomeMail",
                &[
                    PyRep::Integer(list_id),
                    PyRep::String("Welcome".into()),
                    PyRep::String("Rules".into()),
                ],
                &kw,
                None,
            )
            .await
            .expect("SendWelcomeMail");
        assert_eq!(res.as_bool(), Some(true));

        // 7. Leave
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "Leave",
                &[PyRep::Integer(list_id)],
                &kw,
                None,
            )
            .await
            .expect("Leave");
        assert_eq!(res.as_bool(), Some(true));

        // 8. Delete
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "Delete",
                &[PyRep::Integer(list_id)],
                &kw,
                None,
            )
            .await
            .expect("Delete");
        assert_eq!(res.as_bool(), Some(true));

        let get_after_del = ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "GetInfo",
                &[PyRep::Integer(list_id)],
                &kw,
                None,
            )
            .await;
        assert!(get_after_del.is_err(), "GetInfo must fail on deleted mailing list");
    }
}

pub(crate) mod rpc_map_svc {
    // Parity test suite for core RPC services batch (Part 3: `map`).


    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use crate::common;
    use eve_destiny::DVec3;
    use eve_machonet::SessionState;
    use eve_proto::{DBRowDescriptor, PyRep};
    use eve_server::router::ServerContext;

    fn as_list(rep: &PyRep) -> &[PyRep] {
        match rep {
            PyRep::List(ref l) => l.as_slice(),
            _ => panic!("expected List, got {rep:?}"),
        }
    }

    fn as_dict(rep: &PyRep) -> &[(PyRep, PyRep)] {
        match rep {
            PyRep::Dict(ref d) => d.as_slice(),
            _ => panic!("expected Dict, got {rep:?}"),
        }
    }

    fn crowset_header_has_column(rep: &PyRep, col_name: &str) -> bool {
        if let PyRep::ObjectEx {
            is_type_2: true,
            header,
            ..
        } = rep
        {
            if let PyRep::Tuple(items) = header.as_ref() {
                if let Some(PyRep::Dict(dict)) = items.get(1) {
                    for (k, v) in dict {
                        if k.as_str() == Some("header") {
                            if let Some(desc) = DBRowDescriptor::from_pyrep(v) {
                                return desc.column_names().iter().any(|c| c == col_name);
                            }
                        }
                    }
                }
            }
        }
        false
    }

    #[tokio::test]
    async fn test_rpc_map_svc_part1_batch() {
        // parity: rpc:map.GetBeaconCount, rpc:map.GetCurrentSovData, rpc:map.GetDeadspaceComplexMap, rpc:map.GetFacWarZoneInfo, rpc:map.GetIncursionGlobalReport
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        ctx.db.with_mut_snapshot(|snap| {
            // Seed Pharolux Cyno Beacon structure (type 35840) in system 30000142
            snap.structures.insert(
                77_000_001,
                eve_db::UpwellStructureRecord {
                    structure_id: 77_000_001,
                    type_id: 35840,
                    solar_system_id: 30_000_142,
                    owner_corp_id: 98_000_001,
                    name: "Jita Pharolux Cyno Beacon".into(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 1000,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0, self_destruct_at_unix: None,
                },
            );
            // Seed Ansiblex Jump Gate (type 35841) in system 30000144 — must NOT be counted as a cyno beacon
            snap.structures.insert(
                77_000_002,
                eve_db::UpwellStructureRecord {
                    structure_id: 77_000_002,
                    type_id: 35841,
                    solar_system_id: 30_000_144,
                    owner_corp_id: 98_000_001,
                    name: "Maurasi Ansiblex Jump Gate".into(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 1000,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0, self_destruct_at_unix: None,
                },
            );
            // Seed Mobile Cynosural Beacon (type 56099) deployed in space in system 30000142
            snap.items.insert(
                77_000_003,
                eve_db::InventoryItemRecord {
                    item_id: 77_000_003,
                    type_id: 56099,
                    owner_id: 90_000_001,
                    location_id: 30_000_142,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            // Seed sov system state with alliance
            snap.alliances.insert(
                99_000_001,
                eve_db::AllianceRecord {
                    alliance_id: 99_000_001,
                    name: "Northern Coalition".into(),
                    short_name: "NC".into(),
                    executor_corp_id: 98_000_001,
                    member_corp_ids: HashSet::from([98_000_001]),
                },
            );
            snap.system_states.insert(
                30_000_142,
                eve_db::SystemDynamicState {
                    solar_system_id: 30_000_142,
                    sov_alliance_id: Some(99_000_001),
                    ..Default::default()
                },
            );
        });
        let mut sess = SessionState::new(101);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();

        // 1. GetBeaconCount — returns per-system (module_count, structure_count) 2-tuple:
        // Mobile Cyno Beacon (56099) + Pharolux Cyno Beacon (35840) in 30000142 => (1, 1),
        // and excludes Ansiblex Jump Gate (35841) in 30000144.
        let res = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetBeaconCount", &[], &kw, None)
            .await
            .expect("GetBeaconCount");
        let beacons = as_dict(&res);
        let jita_tuple = beacons
            .iter()
            .find(|(k, _)| k.as_i64() == Some(30_000_142))
            .map(|(_, v)| v)
            .expect("beacon tuple for 30000142");
        assert_eq!(
            jita_tuple,
            &PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1)])
        );
        let has_maurasi = beacons.iter().any(|(k, _)| k.as_i64() == Some(30_000_144));
        assert!(
            !has_maurasi,
            "Ansiblex Jump Gate (35841) must not be counted by map.GetBeaconCount"
        );

        // 2. GetConstellationLPData
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetConstellationLPData",
                &[PyRep::Integer(20_000_020)],
                &kw,
                None,
            )
            .await
            .expect("GetConstellationLPData");
        assert!(res.is_crowset());
        assert!(crowset_header_has_column(&res, "solarSystemID"));

        // 3. GetCurrentSovData
        let res = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetCurrentSovData", &[], &kw, None)
            .await
            .expect("GetCurrentSovData");
        let sov_rows = as_list(&res);
        assert!(!sov_rows.is_empty());
        let row = &sov_rows[0];
        assert_eq!(
            row.dict_get("solarSystemID").and_then(PyRep::as_i64),
            Some(30_000_142)
        );
        assert_eq!(
            row.dict_get("allianceID").and_then(PyRep::as_i64),
            Some(99_000_001)
        );

        // 4. GetDeadspaceAgentsMap — 4-column (solarSystemID, dungeonID, difficulty, dungeonName)
        let now_unix = ctx.now_unix();
        let cid = *sess.character_id.get_or_insert(90_000_001);
        eve_server::services::pve_exploration::with_pve_exploration_reg_db(&ctx.db, now_unix, |reg| {
            reg.escalations_by_char.entry(cid).or_default().push(eve_server::services::pve_exploration::EscalatingPathEntry {
                instance_id: 910_001, character_id: cid, source_dungeon_id: 1001, expedition_dungeon_id: 2001,
                expedition_name: "Test Escalation".into(), destination_solar_system_id: 30_000_142, step_index: 1, total_steps: 4,
                journal_text: "Test Escalation".into(), created_at_sec: now_unix, expires_at_sec: now_unix + 86_400, dungeons_entered: vec![1001], journal_entry_template_id: 280_001,
            });
        });
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetDeadspaceAgentsMap",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetDeadspaceAgentsMap");
        assert!(res.is_crowset());
        for col in ["solarSystemID", "dungeonID", "difficulty", "dungeonName"] {
            assert!(crowset_header_has_column(&res, col));
        }
        let agent_rows = res.crowset_rows().expect("agent rows");
        assert!(!agent_rows.is_empty());

        // 5. GetDeadspaceComplexMap — 4-column (solarSystemID, dungeonID, difficulty, dungeonName)
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetDeadspaceComplexMap",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetDeadspaceComplexMap");
        assert!(res.is_crowset());
        for col in ["solarSystemID", "dungeonID", "difficulty", "dungeonName"] {
            assert!(crowset_header_has_column(&res, col));
        }

        // 6. GetEdencomFortressSystems
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetEdencomFortressSystems",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetEdencomFortressSystems");
        let PyRep::List(f_list) = res else { panic!("Expected List") };
        assert_eq!(f_list.len(), 53);
        assert!(f_list.contains(&PyRep::Integer(30_002_253)));

        // 7. GetEdencomMinorVictorySystems
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetEdencomMinorVictorySystems",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetEdencomMinorVictorySystems");
        let PyRep::List(e_list) = res else { panic!("Expected List") };
        assert_eq!(e_list.len(), 84);
        assert!(e_list.contains(&PyRep::Integer(30_002_048)) && e_list.contains(&PyRep::Integer(30_003_074)));

        // 8. GetFacWarZoneInfo
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetFacWarZoneInfo",
                &[PyRep::Integer(500_001)],
                &kw,
                None,
            )
            .await
            .expect("GetFacWarZoneInfo");
        let max_pts = res
            .dict_get("maxWarZonePoints")
            .and_then(PyRep::as_i64)
            .expect("maxWarZonePoints");
        assert_eq!(max_pts, 100_000);

        // 9. GetHistory
        let res = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetHistory", &[], &kw, None)
            .await
            .expect("GetHistory");
        assert!(res.is_crowset());
        assert!(crowset_header_has_column(&res, "solarSystemID"));

        // 10. GetIncursionGlobalReport
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetIncursionGlobalReport",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetIncursionGlobalReport");
        assert!(matches!(&res, PyRep::List(_)));
    }

    #[tokio::test]
    async fn test_rpc_map_svc_part2_batch() {
        // parity: rpc:map.GetIndustryJobsOverLast24Hours, rpc:map.GetMyExtraMapInfo, rpc:map.GetMyExtraMapInfoAgents, rpc:map.GetRecentSovActivity, rpc:map.GetSecurityModifiedSystems, rpc:map.GetSolarSystemVisits, rpc:map.GetStationCount, rpc:map.GetStationInfo, rpc:map.GetSystemsInIncursions
        let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
        // Seed 300 stations across two regions (10000002 The Forge and 10000043 Domain) so we verify
        // map.GetStationInfo returns the full cluster station rowset without region filtering or 256-row truncation.
        for idx in 0..300i64 {
            let station_id = 60_900_000 + idx;
            let region_id = if idx < 150 { 10_000_002 } else { 10_000_043 };
            let solar_system_id = if idx < 150 { 30_000_142 } else { 30_002_187 };
            sde.stations.insert(
                station_id,
                eve_sde::StationInfo {
                    station_id,
                    solar_system_id,
                    constellation_id: 20_000_020,
                    region_id,
                    station_type_id: 1529,
                    name: format!("Cluster Test Station {idx}"),
                    position: DVec3::ZERO,
                    owner_corporation_id: 1_000_035,
                    owner_faction_id: None,
                    operation_id: 26,
                    reprocessing_efficiency: 0.5,
                    reprocessing_stations_take: 0.05,
                    is_conquerable: false,
                    orbit_id: None,
                    has_clone_bay: true,
                },
            );
        }
        let expected_total_stations = sde.stations.len();
        assert!(expected_total_stations > 300);

        let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
        ctx.seed_demo_world_fixtures();
        let now = ctx.now_unix_secs();
        ctx.db.with_mut_snapshot(|snap| {
            snap.industry_jobs.insert(
                77_001,
                eve_db::IndustryJobRecord {
                    job_id: 77_001,
                    installer_id: 90_000_001,
                    facility_id: 60_003_760,
                    blueprint_item_id: 100_000_001,
                    blueprint_type_id: 587,
                    activity_id: 1,
                    runs: 5,
                    product_type_id: 587,
                    status: "Active".into(),
                    duration_seconds: 3600,
                    start_unix: now,
                },
            );
            snap.industry_jobs.insert(
                77_002,
                eve_db::IndustryJobRecord {
                    job_id: 77_002,
                    installer_id: 90_000_001,
                    facility_id: 60_003_760,
                    blueprint_item_id: 100_000_002,
                    blueprint_type_id: 587,
                    activity_id: 1,
                    runs: 10,
                    product_type_id: 587,
                    status: "Active".into(),
                    duration_seconds: 3600,
                    start_unix: now.saturating_sub(100_000),
                },
            );
        });

        let mut sess = SessionState::new(101);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();
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

        // 1. GetIndustryJobsOverLast24Hours
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetIndustryJobsOverLast24Hours",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetIndustryJobsOverLast24Hours");
        if let PyRep::Rowset { columns, rows } = &res {
            assert!(columns.iter().any(|c| c == "solarSystemID"));
            assert!(columns.iter().any(|c| c == "noOfJobs"));
            assert_eq!(rows.len(), 1, "only jobs within last 24 hours should be counted");
            assert_eq!(rows[0][0].as_i64(), Some(30_000_142));
            assert_eq!(rows[0][1].as_i64(), Some(1));
        } else {
            panic!("expected Rowset for GetIndustryJobsOverLast24Hours");
        }

        // 2. GetMyExtraMapInfo
        let res = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetMyExtraMapInfo", &[], &kw, None)
            .await
            .expect("GetMyExtraMapInfo");
        let extra_map = as_list(&res);
        assert!(!extra_map.is_empty());
        let my_cid = extra_map[0]
            .dict_get("characterID")
            .and_then(PyRep::as_i64)
            .expect("characterID");
        assert_eq!(my_cid, 90_000_001);

        // 3. GetMyExtraMapInfoAgents
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.standings.insert(3011394, 5.0);
            }
        });

        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetMyExtraMapInfoAgents",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetMyExtraMapInfoAgents");
        assert!(res.is_crowset());
        assert!(crowset_header_has_column(&res, "agentID"));
        let agent_rows = res.crowset_rows().expect("agents rows");
        assert_eq!(agent_rows.len(), 1);
        assert_eq!(
            agent_rows[0].dbrow_get("agentID").and_then(PyRep::as_i64),
            Some(3011394)
        );

        // 4. GetRecentSovActivity
        ctx.db.with_mut_snapshot(|snap| {
            snap.system_states.entry(30_000_142).or_default().sov_alliance_id = Some(99_000_001);
        });

        let res = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetRecentSovActivity", &[], &kw, None)
            .await
            .expect("GetRecentSovActivity");
        let sov_act = as_list(&res);
        assert!(!sov_act.is_empty());
        let owner_id = sov_act[0]
            .dict_get("ownerID")
            .and_then(PyRep::as_i64)
            .expect("ownerID");
        assert_eq!(owner_id, 99_000_001);

        // 5. GetSecurityModifiedSystems
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetSecurityModifiedSystems",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetSecurityModifiedSystems");
        assert!(res.is_crowset());
        assert!(crowset_header_has_column(&res, "solarSystemID"));
        assert!(crowset_header_has_column(&res, "modifiedSecurity"));

        // 6. GetSolarSystemVisits
        let res = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
            .await
            .expect("GetSolarSystemVisits");
        let PyRep::List(visits) = &res else { panic!("expected List of visits, got {res:?}") };
        assert!(!visits.is_empty());
        let visit_count = visits[0].dict_get("visits").and_then(|v| v.as_i64()).expect("visit count int");
        assert_eq!(visit_count, 1);
        let last_dt = visits[0].dict_get("lastDateTime").and_then(|v| v.as_i64()).expect("lastDateTime");
        assert!(last_dt > 0);
        let sys_id = visits[0].dict_get("solarSystemID").and_then(|v| v.as_i64()).expect("solarSystemID");
        assert_eq!(sys_id, 30_000_142);

        // 7. GetStationCount
        let res = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetStationCount", &[], &kw, None)
            .await
            .expect("GetStationCount");
        let stations = as_dict(&res);
        assert!(!stations.is_empty());
        let st_count = stations[0].1.as_i64().expect("station count int");
        assert!(st_count > 0);

        // 8. GetStationInfo — must return all stations across all regions without 256-row truncation
        let res = ctx
            .dispatch_rpc(&mut sess, "map", None, "GetStationInfo", &[], &kw, None)
            .await
            .expect("GetStationInfo");
        if let PyRep::Rowset { columns, rows } = &res {
            assert!(columns.iter().any(|c| c == "stationID"));
            assert_eq!(
                rows.len(),
                expected_total_stations,
                "map.GetStationInfo must return all cluster stations without 256-row cap or region filter"
            );
            let region_idx = columns.iter().position(|c| c == "regionID").expect("regionID col");
            let regions: HashSet<i64> = rows.iter().filter_map(|r| r[region_idx].as_i64()).collect();
            assert!(regions.contains(&10_000_002));
            assert!(regions.contains(&10_000_043));

            let single_st = ctx
                .dispatch_rpc(&mut sess, "map", None, "GetStationInfo", &[PyRep::Integer(60_900_000)], &kw, None)
                .await
                .expect("GetStationInfo single");
            assert_eq!(single_st.dict_get("stationID"), Some(&PyRep::Integer(60_900_000)));
            assert_eq!(single_st.dict_get("solarSystemID"), Some(&PyRep::Integer(30_000_142)));
        } else {
            panic!("expected Rowset for GetStationInfo");
        }

        // 9. GetSystemsInIncursions
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetSystemsInIncursions",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetSystemsInIncursions");
        assert!(matches!(&res, PyRep::List(_)));

        // 10. GetTriglavianMinorVictorySystems
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetTriglavianMinorVictorySystems",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetTriglavianMinorVictorySystems");
        let PyRep::List(t_list) = res else { panic!("Expected List") };
        assert_eq!(t_list.len(), 28);
        assert!(t_list.contains(&PyRep::Integer(30_003_076)));
    }

    #[tokio::test]
    async fn test_map_visited_systems_system_entry_tracking() {
        // parity: feat:ui.map-visited-systems
        eve_testkit::attest(&["feat:ui.map-visited-systems"]);

        let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
        let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(102);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();

        let t0 = ctx.now_blueos_filetime_ticks();

        // 1. Initial logon via SelectCharacterID records initial system visit (Jita 30000142)
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
            .await
            .expect("SelectCharacterID");

        let res = ctx.dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
            .await
            .expect("GetSolarSystemVisits");
        let PyRep::List(visits) = res else { panic!("expected List") };
        assert_eq!(visits.len(), 1);
        let v0 = &visits[0];
        assert_eq!(v0.dict_get("solarSystemID"), Some(&PyRep::Integer(30_000_142)));
        assert_eq!(v0.dict_get("visits"), Some(&PyRep::Integer(1)));
        let dt0 = v0.dict_get("lastDateTime").and_then(|v| v.as_i64()).expect("lastDateTime");
        assert!(dt0 >= t0);

        // 2. Undock into space and jump stargate 50001248 (Jita -> Perimeter 30000144) via real CmdStargateJump RPC
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
        let ship_id = sess.ship_id.expect("ship_id");
        let gate_id = 50001248;
        let gate = &ctx.sde.jumps[&gate_id];
        let to_sys = gate.to_solar_system_id;
        let dest_gate_id = gate.destination_stargate_id;
        let bid_jita = ctx.bind_object(&mut sess, "beyonce", 30_000_142).expect("bind beyonce");
        let h_jita = ctx.systems.get_or_spawn(30_000_142);
        let gate_rad = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
        h_jita.set_ball_position(ship_id, gate.position + DVec3::new(gate_rad + 500.0, 0.0, 0.0)).await;
        ctx.advance_secs(15);

        ctx.dispatch_rpc(&mut sess, "beyonce", Some(bid_jita), "CmdStargateJump", &[PyRep::Integer(gate_id)], &kw, None)
            .await
            .expect("CmdStargateJump to Perimeter");

        let res = ctx.dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
            .await
            .expect("GetSolarSystemVisits after gate jump");
        let PyRep::List(visits) = res else { panic!("expected List") };
        assert_eq!(visits.len(), 2);
        let perim = visits.iter().find(|v| v.dict_get("solarSystemID") == Some(&PyRep::Integer(to_sys as i64))).expect("Perimeter visit");
        assert_eq!(perim.dict_get("visits"), Some(&PyRep::Integer(1)));
        let dt1 = perim.dict_get("lastDateTime").and_then(|v| v.as_i64()).expect("Perimeter lastDateTime");
        assert!(dt1 > dt0);

        // 3. Return jump into Jita (30000142) via real CmdStargateJump RPC increments visit count to 2
        let bid_dest = ctx.bind_object(&mut sess, "beyonce", to_sys as i64).expect("bind dest beyonce");
        let h_dest = ctx.systems.get_or_spawn(to_sys);
        let dest_gate = &ctx.sde.jumps[&dest_gate_id];
        h_dest.set_ball_position(ship_id, dest_gate.position + DVec3::new(gate_rad + 500.0, 0.0, 0.0)).await;
        ctx.advance_secs(15);

        ctx.dispatch_rpc(&mut sess, "beyonce", Some(bid_dest), "CmdStargateJump", &[PyRep::Integer(dest_gate_id)], &kw, None)
            .await
            .expect("CmdStargateJump return to Jita");

        let res = ctx.dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &kw, None)
            .await
            .expect("GetSolarSystemVisits after return jump");
        let PyRep::List(visits) = res else { panic!("expected List") };
        let jita = visits.iter().find(|v| v.dict_get("solarSystemID") == Some(&PyRep::Integer(30_000_142))).expect("Jita visit");
        assert_eq!(jita.dict_get("visits"), Some(&PyRep::Integer(2)));
        let dt2 = jita.dict_get("lastDateTime").and_then(|v| v.as_i64()).expect("Jita lastDateTime updated");
        assert!(dt2 > dt1);

        // 4. Persistence across service blobs and fresh session reload
        assert!(ctx.db.with_snapshot(|s| s.service_state_blobs.contains_key("char_system_visits:90000001")));
        let mut fresh_sess = SessionState::new(103);
        common::login_pilot(&ctx, &mut fresh_sess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut fresh_sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
        let res = ctx.dispatch_rpc(&mut fresh_sess, "map", None, "GetSolarSystemVisits", &[], &kw, None).await.unwrap();
        let PyRep::List(visits) = res else { panic!("expected List") };
        assert_eq!(visits.len(), 2);
    }
}

pub(crate) mod rpc_utility_services {
    // Parity integration test suite for 11 utility and management RPCs.
    //
    // Validates live-server return shapes and state mutations for:
    // - `calendarProxy.GetEventList` & `calendarProxy.GetEventDetails`
    // - `characterEnergyMgr.GetMyEnergyState`
    // - `characterMiningLedger.GetCharacterLogs`
    // - `jumpTimers.GetTimers`
    // - `onlineStatus.GetInitialState` & `onlineStatus.GetOnlineStatus`
    // - `overviewPresetMgr.StoreLinkAndGetID` & `overviewPresetMgr.GetStoredPreset`
    // - `sharedSettingsMgr.StoreSettingLinkAndGetID` & `sharedSettingsMgr.GetStoredSharedSetting`


    use crate::common;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const PILOT1: i64 = 90_000_001;
    const PILOT2: i64 = 90_000_002;

    fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
        match rep {
            PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()),
            PyRep::Dict(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .and_then(|(_, v)| v.as_i64()),
            _ => None,
        }
    }

    fn get_str<'a>(rep: &'a PyRep, key: &str) -> Option<&'a str> {
        match rep {
            PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_str()),
            PyRep::Dict(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .and_then(|(_, v)| v.as_str()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn test_rpc_utility_services_batch_complete() {
        // parity: rpc:calendarProxy.GetEventList, rpc:calendarProxy.GetEventDetails
        // parity: rpc:characterEnergyMgr.GetMyEnergyState, rpc:characterMiningLedger.GetCharacterLogs
        // parity: rpc:jumpTimers.GetTimers, rpc:onlineStatus.GetInitialState, rpc:onlineStatus.GetOnlineStatus
        // parity: rpc:overviewPresetMgr.StoreLinkAndGetID, rpc:overviewPresetMgr.GetStoredPreset
        // parity: rpc:sharedSettingsMgr.StoreSettingLinkAndGetID, rpc:sharedSettingsMgr.GetStoredSharedSetting
        eve_testkit::attest(&[
            "rpc:calendarProxy.GetEventList",
            "rpc:calendarProxy.GetEventDetails",
            "rpc:characterEnergyMgr.GetMyEnergyState",
            "rpc:characterMiningLedger.GetCharacterLogs",
            "rpc:jumpTimers.GetTimers",
            "rpc:onlineStatus.GetInitialState",
            "rpc:onlineStatus.GetOnlineStatus",
            "rpc:overviewPresetMgr.StoreLinkAndGetID",
            "rpc:overviewPresetMgr.GetStoredPreset",
            "rpc:sharedSettingsMgr.StoreSettingLinkAndGetID",
            "rpc:sharedSettingsMgr.GetStoredSharedSetting",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();

        let mut sess1 = SessionState::new(1001);
        common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        ctx.dispatch_rpc(
            &mut sess1,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT1)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID sess1");

        let mut sess2 = SessionState::new(1002);
        common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        ctx.dispatch_rpc(
            &mut sess2,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT2)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID sess2");

        // 1. Overview preset manager: store YAML preset, retrieve by ID and hash
        let overview_yaml = "presets:\n  - tab: Default\n    bracket: true\n";
        let store_preset_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "overviewPresetMgr",
                None,
                "StoreLinkAndGetID",
                &[PyRep::String(overview_yaml.into())],
                &kw,
                None,
            )
            .await
            .expect("StoreLinkAndGetID overview");
        let PyRep::KeyVal(preset_kv) = &store_preset_res else {
            panic!("StoreLinkAndGetID must return KeyVal");
        };
        let sq_id = preset_kv
            .get("sqID")
            .and_then(|v| v.as_i64())
            .expect("sqID in preset KeyVal");
        let hashvalue = preset_kv
            .get("hashvalue")
            .and_then(|v| v.as_str())
            .expect("hashvalue in preset KeyVal");
        assert!(!hashvalue.is_empty(), "hashvalue must be non-empty");

        // Round-trip retrieval: get preset by integer sqID
        let got_by_sqid = ctx
            .dispatch_rpc(
                &mut sess2,
                "overviewPresetMgr",
                None,
                "GetStoredPreset",
                &[PyRep::Integer(sq_id)],
                &kw,
                None,
            )
            .await
            .expect("GetStoredPreset by sqID");
        assert_eq!(got_by_sqid, PyRep::String(overview_yaml.into()));

        // Round-trip retrieval: get preset by string hashvalue
        let got_by_hash = ctx
            .dispatch_rpc(
                &mut sess2,
                "overviewPresetMgr",
                None,
                "GetStoredPreset",
                &[PyRep::String(hashvalue.into())],
                &kw,
                None,
            )
            .await
            .expect("GetStoredPreset by hashvalue");
        assert_eq!(got_by_hash, PyRep::String(overview_yaml.into()));

        // 2. Shared settings manager: store setting link, retrieve by linkID and key
        let setting_type = "inventory_filter";
        let setting_yaml = "filters:\n  - ore\n  - ice\n";
        let store_setting_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "sharedSettingsMgr",
                None,
                "StoreSettingLinkAndGetID",
                &[
                    PyRep::String(setting_type.into()),
                    PyRep::String(setting_yaml.into()),
                ],
                &kw,
                None,
            )
            .await
            .expect("StoreSettingLinkAndGetID");
        let PyRep::KeyVal(setting_kv) = &store_setting_res else {
            panic!("StoreSettingLinkAndGetID must return KeyVal");
        };
        let link_id = setting_kv
            .get("linkID")
            .and_then(|v| v.as_i64())
            .expect("linkID");
        let setting_key = setting_kv
            .get("key")
            .and_then(|v| v.as_str())
            .expect("key");
        assert_eq!(
            setting_kv.get("settingType"),
            Some(&PyRep::String(setting_type.into()))
        );

        // Round-trip retrieval: get shared setting by linkID
        let got_setting_by_id = ctx
            .dispatch_rpc(
                &mut sess2,
                "sharedSettingsMgr",
                None,
                "GetStoredSharedSetting",
                &[PyRep::Integer(link_id)],
                &kw,
                None,
            )
            .await
            .expect("GetStoredSharedSetting by linkID");
        assert_eq!(got_setting_by_id, PyRep::String(setting_yaml.into()));

        // Round-trip retrieval: get shared setting by string key
        let got_setting_by_key = ctx
            .dispatch_rpc(
                &mut sess2,
                "sharedSettingsMgr",
                None,
                "GetStoredSharedSetting",
                &[PyRep::String(setting_key.into())],
                &kw,
                None,
            )
            .await
            .expect("GetStoredSharedSetting by key");
        assert_eq!(got_setting_by_key, PyRep::String(setting_yaml.into()));

        // Also verify 1-argument signature and payload bound validation
        let single_yaml = "mode: test_single\n";
        let store_1arg_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "sharedSettingsMgr",
                None,
                "StoreSettingLinkAndGetID",
                &[PyRep::String(single_yaml.into())],
                &kw,
                None,
            )
            .await
            .expect("StoreSettingLinkAndGetID with 1 argument");
        let PyRep::KeyVal(single_kv) = &store_1arg_res else {
            panic!("StoreSettingLinkAndGetID 1-arg must return KeyVal");
        };
        let single_link_id = single_kv.get("linkID").and_then(|v| v.as_i64()).unwrap();
        let got_single = ctx
            .dispatch_rpc(
                &mut sess2,
                "sharedSettingsMgr",
                None,
                "GetStoredSharedSetting",
                &[PyRep::Integer(single_link_id)],
                &kw,
                None,
            )
            .await
            .expect("GetStoredSharedSetting single");
        assert_eq!(got_single, PyRep::String(single_yaml.into()));

        let oversized_payload = "a".repeat(65_537);
        let oversize_err = ctx
            .dispatch_rpc(
                &mut sess1,
                "sharedSettingsMgr",
                None,
                "StoreSettingLinkAndGetID",
                &[PyRep::String(oversized_payload)],
                &kw,
                None,
            )
            .await;
        assert!(oversize_err.is_err(), "Payload > 64 KiB must be rejected");

        // 3. Jump timers: query for active character
        let init_jump_timers = ctx
            .dispatch_rpc(
                &mut sess1,
                "jumpTimers",
                None,
                "GetTimers",
                &[PyRep::Integer(PILOT1)],
                &kw,
                None,
            )
            .await
            .expect("jumpTimers.GetTimers");
        let PyRep::KeyVal(jt_kv) = &init_jump_timers else {
            panic!("jumpTimers.GetTimers must return KeyVal");
        };
        assert!(jt_kv.contains_key("jumpActivation"));
        assert!(jt_kv.contains_key("jumpFatigue"));
        assert_eq!(jt_kv.get("jumpActivation"), Some(&PyRep::Integer(0)));
        assert_eq!(jt_kv.get("jumpFatigue"), Some(&PyRep::Integer(0)));

        // State mutation: advance character jump fatigue in DB and verify updated timers
        let future_unix = ctx.now_unix() + 1800;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT1) {
                c.jump_cooldown_until_unix = future_unix;
                c.jump_fatigue_until_unix = future_unix + 3600;
            }
        });
        let updated_jump_timers = ctx
            .dispatch_rpc(
                &mut sess1,
                "jumpTimers",
                None,
                "GetTimers",
                &[PyRep::Integer(PILOT1)],
                &kw,
                None,
            )
            .await
            .expect("jumpTimers.GetTimers updated");
        let act_ft = get_i64(&updated_jump_timers, "jumpActivation").unwrap();
        let fat_ft = get_i64(&updated_jump_timers, "jumpFatigue").unwrap();
        assert!(act_ft > 0, "jumpActivation must be positive FILETIME");
        assert!(fat_ft > act_ft, "jumpFatigue must exceed activation");

        // 4. Character energy state: GetMyEnergyState
        let init_energy = ctx
            .dispatch_rpc(
                &mut sess1,
                "characterEnergyMgr",
                None,
                "GetMyEnergyState",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetMyEnergyState");
        assert_eq!(get_i64(&init_energy, "currentEnergy"), Some(100));
        assert_eq!(get_i64(&init_energy, "maxEnergy"), Some(100));
        assert!(get_i64(&init_energy, "lastUpdateTime").unwrap_or(0) > 0);

        // State mutation: mutate energy state via set_character_energy and verify updated state
        let _ = ctx.drain_notifications_for_char(PILOT1);
        let mutated_state = eve_server::services::character_energy::set_character_energy(&ctx, PILOT1, 40);
        assert_eq!(mutated_state.current_energy, 40);
        let energy_notifs = ctx.drain_notifications_for_char(PILOT1);
        assert!(
            energy_notifs
                .iter()
                .any(|n| n.event == "OnCharacterEnergyChanged"),
            "Mutating energy must emit OnCharacterEnergyChanged"
        );

        // Query again via canonical GetMyEnergyState to verify persistent state
        let after_consume = ctx
            .dispatch_rpc(
                &mut sess1,
                "characterEnergyMgr",
                None,
                "GetMyEnergyState",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetMyEnergyState after consume");
        assert_eq!(get_i64(&after_consume, "currentEnergy"), Some(40));

        // 5. Mining ledger: record entry and query GetCharacterLogs
        let date_str = "2027-01-15";
        let mined_type = 1230u32; // Veldspar
        let mined_qty = 5000i64;
        let sys_id = 30000142u32;
        let _ = ctx.db.record_mining_ledger_entry(
            PILOT1,
            sys_id,
            mined_type,
            mined_qty,
            ctx.now_unix(),
        );

        let logs_rep = ctx
            .dispatch_rpc(
                &mut sess1,
                "characterMiningLedger",
                None,
                "GetCharacterLogs",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetCharacterLogs");
        let PyRep::List(logs) = &logs_rep else {
            panic!("GetCharacterLogs must return PyRep::List");
        };
        assert!(!logs.is_empty(), "Mining logs must contain entry");
        let entry = logs
            .iter()
            .find(|item| {
                get_str(item, "date") == Some(date_str)
                    && get_i64(item, "typeID") == Some(mined_type as i64)
            })
            .expect("Must find recorded mining log entry");
        assert_eq!(get_i64(entry, "quantity"), Some(mined_qty));
        assert_eq!(get_i64(entry, "solarSystemID"), Some(sys_id as i64));

        // 6. Online status: GetInitialState and GetOnlineStatus
        // Add Pilot 2 as watched contact of Pilot 1 in ESI contacts
        ctx.esi_state
            .char_contacts
            .write()
            .unwrap()
            .entry(PILOT1)
            .or_default()
            .push(eve_server::esi::runtime_state::EsiContactEntry {
                contact_id: PILOT2,
                contact_type: "character".into(),
                standing: 5.0,
                is_watched: true,
                is_blocked: false,
                label_ids: vec![],
            });

        let init_online = ctx
            .dispatch_rpc(
                &mut sess1,
                "onlineStatus",
                None,
                "GetInitialState",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetInitialState");
        let PyRep::Rowset { rows, columns, .. } = init_online else {
            panic!("GetInitialState must return Rowset");
        };
        assert_eq!(columns, vec!["contactID", "online"]);
        assert!(
            rows.iter()
                .any(|r| r[0] == PyRep::Integer(PILOT2) && r[1] == PyRep::Bool(true)),
            "Pilot 2 must be listed as online in initial state"
        );

        // Query GetOnlineStatus with list of character IDs -> returns Dict mapping charID -> bool
        let status_dict = ctx
            .dispatch_rpc(
                &mut sess1,
                "onlineStatus",
                None,
                "GetOnlineStatus",
                &[PyRep::List(vec![
                    PyRep::Integer(PILOT2),
                    PyRep::Integer(99_999_999),
                ])],
                &kw,
                None,
            )
            .await
            .expect("GetOnlineStatus with ID list");
        let PyRep::Dict(status_map) = &status_dict else {
            panic!("GetOnlineStatus with list must return Dict");
        };
        let p2_online = status_map
            .iter()
            .find(|(k, _)| k == &PyRep::Integer(PILOT2))
            .map(|(_, v)| v);
        assert_eq!(p2_online, Some(&PyRep::Bool(true)));
        let offline_check = status_map
            .iter()
            .find(|(k, _)| k == &PyRep::Integer(99_999_999))
            .map(|(_, v)| v);
        assert_eq!(offline_check, Some(&PyRep::Bool(false)));

        // Also verify single integer argument returns Bool for backward compatibility
        let single_p2 = ctx
            .dispatch_rpc(
                &mut sess1,
                "onlineStatus",
                None,
                "GetOnlineStatus",
                &[PyRep::Integer(PILOT2)],
                &kw,
                None,
            )
            .await
            .expect("GetOnlineStatus single");
        assert_eq!(single_p2, PyRep::Bool(true));

        // 7. Calendar proxy: create event, query GetEventList (year, month & month, year), GetEventDetails
        let future_time = ctx.now_blueos_filetime_ticks() + 86_400 * 10_000_000;
        let create_ev = ctx
            .dispatch_rpc(
                &mut sess1,
                "calendarMgr",
                None,
                "CreatePersonalEvent",
                &[
                    PyRep::Integer(future_time),
                    PyRep::Integer(60),
                    PyRep::String("Utility Fleet Op".into()),
                    PyRep::String("Deep Space Expedition".into()),
                    PyRep::Integer(2),
                ],
                &kw,
                None,
            )
            .await
            .expect("CreatePersonalEvent");
        let ev_id = get_i64(&create_ev, "eventID").expect("eventID");

        // Query GetEventList with (year, month) = (2027, 1)
        let ev_list_ym = ctx
            .dispatch_rpc(
                &mut sess1,
                "calendarProxy",
                None,
                "GetEventList",
                &[PyRep::Integer(2027), PyRep::Integer(1)],
                &kw,
                None,
            )
            .await
            .expect("calendarProxy.GetEventList (year, month)");
        let PyRep::List(rowsets_ym) = &ev_list_ym else {
            panic!("GetEventList must return List of rowsets");
        };
        assert!(!rowsets_ym.is_empty(), "Must return event rowset");
        let PyRep::Rowset { rows: ev_rows, .. } = &rowsets_ym[0] else {
            panic!("Inner item must be Rowset");
        };
        assert!(
            ev_rows.iter().any(|r| r[0] == PyRep::Integer(ev_id)),
            "Event must be found in calendarProxy.GetEventList(2027, 1)"
        );

        // Query GetEventList with (month, year) = (1, 2027)
        let ev_list_my = ctx
            .dispatch_rpc(
                &mut sess1,
                "calendarProxy",
                None,
                "GetEventList",
                &[PyRep::Integer(1), PyRep::Integer(2027)],
                &kw,
                None,
            )
            .await
            .expect("calendarProxy.GetEventList (month, year)");
        let PyRep::List(rowsets_my) = &ev_list_my else {
            panic!("GetEventList must return List");
        };
        let PyRep::Rowset { rows: my_rows, .. } = &rowsets_my[0] else {
            panic!("Inner item must be Rowset");
        };
        assert!(
            my_rows.iter().any(|r| r[0] == PyRep::Integer(ev_id)),
            "Event must be found in calendarProxy.GetEventList(1, 2027)"
        );

        // Query calendarProxy.GetEventDetails
        let ev_details = ctx
            .dispatch_rpc(
                &mut sess1,
                "calendarProxy",
                None,
                "GetEventDetails",
                &[PyRep::Integer(ev_id), PyRep::Integer(PILOT1)],
                &kw,
                None,
            )
            .await
            .expect("calendarProxy.GetEventDetails");
        assert_eq!(get_i64(&ev_details, "eventID"), Some(ev_id));
        assert_eq!(get_i64(&ev_details, "ownerID"), Some(PILOT1));
        assert_eq!(get_str(&ev_details, "eventTitle"), Some("Utility Fleet Op"));
        assert_eq!(get_i64(&ev_details, "eventDateTime"), Some(future_time));
        assert_eq!(get_i64(&ev_details, "duration"), Some(60));
        assert_eq!(get_i64(&ev_details, "importance"), Some(2));
        assert_eq!(get_i64(&ev_details, "response"), Some(4));

        // 8. Server context reload persistence: verify character energy state survives reloaded context
        let ctx_reloaded = ServerContext::with_sde_and_repository(
            std::sync::Arc::clone(&ctx.sde),
            std::sync::Arc::clone(&ctx.db),
        );
        common::pin_clock(&ctx_reloaded);
        let mut sess_reloaded = SessionState::new(1003);
        common::login_pilot(&ctx_reloaded, &mut sess_reloaded, "demo", "demo-secret").await;
        ctx_reloaded
            .dispatch_rpc(
                &mut sess_reloaded,
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(PILOT1)],
                &kw,
                None,
            )
            .await
            .unwrap();
        let reloaded_energy = ctx_reloaded
            .dispatch_rpc(
                &mut sess_reloaded,
                "characterEnergyMgr",
                None,
                "GetMyEnergyState",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetMyEnergyState on reloaded context");
        assert_eq!(
            get_i64(&reloaded_energy, "currentEnergy"),
            Some(40),
            "Character energy state must persist across server context reload"
        );
    }
}

pub(crate) mod dict_get_call_rpc {
    // State-backed integration tests for 15 newly implemented V24 RPCs across
    // `contractProxy`, `repairSvc`, `structure`, `search`, `i2`, `jumpCloneSvc`,
    // `warpVectorMgr`, and `devIndexManager`.

    use eve_db::{
        CharacterRecord, ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
        IskCents, SystemDynamicState, UpwellStructureRecord,
    };
    use eve_destiny::Ball;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_sde::attr_ids;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
        match rep {
            PyRep::Dict(entries) => entries
                .iter()
                .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
                .map(|(_, v)| v),
            PyRep::KeyVal(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }

    async fn call_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
    }

    async fn call_rpc_kwargs(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
        kwargs: HashMap<String, PyRep>,
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &kwargs, None).await
    }

    fn make_item(
        item_id: i64,
        type_id: u32,
        owner_id: i64,
        location_id: i64,
        flag: InventoryFlag,
        quantity: i32,
        singleton: bool,
        is_bpc: bool,
    ) -> InventoryItemRecord {
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: is_bpc,
            blueprint_runs_remaining: if is_bpc { Some(10) } else { None },
        }
    }

    fn make_char(char_id: i64, corp_id: i64, station_id: Option<i64>, structure_id: Option<i64>, ship_id: i64) -> CharacterRecord {
        CharacterRecord {
            character_id: char_id, user_id: 700, name: "Valkyrie SearchPilot".into(),
            corporation_id: corp_id, alliance_id: Some(99_000_777), war_faction_id: None, fleet_id: None, autopilot_waypoints: Vec::new(),
            station_id, structure_id,
            solar_system_id: 30_000_142, constellation_id: 20_000_020, region_id: 10_000_002,
            active_ship_id: ship_id, balance: IskCents::from_f64_isk(100_000_000.0),
            plex_balance: 0, loyalty_points: HashMap::new(), standings: HashMap::new(),
            security_status: 1.5, clone_grade: 2, home_station_id: 60_003_760,
            race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1,
            neural: eve_db::NeuralAttributes::default(), skill_points: 1_000_000.0,
            activity_status: None,
            free_skill_points: 0.0, last_clone_jump_unix: 1_700_000_000, jump_fatigue_minutes: 0.0,
            jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
            description: String::new(), title: String::new(), gender: 1,
            create_date_unix: 1_700_000_000, version: 1, system_epoch: 1,
        }
    }

    fn make_contract(
        contract_id: i64,
        issuer_id: i64,
        contract_type: &str,
        status: &str,
        station_id: i64,
        price: f64,
        reward: f64,
        collateral: f64,
        assignee_id: i64,
        corp_id: i64,
    ) -> ContractRecord {
        ContractRecord {
            contract_id, issuer_id, acceptor_id: None, contract_type: contract_type.into(),
            status: status.into(), start_station_id: station_id, end_station_id: station_id,
            price: IskCents::from_f64_isk(price), reward: IskCents::from_f64_isk(reward), collateral: IskCents::from_f64_isk(collateral),
            item_ids: Vec::new(), expiration_unix: 2_000_000_000, issuer_corp_id: corp_id, assignee_id,
            availability: if assignee_id > 0 { "Personal".into() } else { "Public".into() },
            buyout: IskCents(0), current_bid: IskCents(0), current_bidder_id: None,
            requested_items: Vec::new(), bid_history: Vec::new(), date_issued_unix: 1_700_000_000,
            title: format!("Contract {contract_id}"), for_corp: false, num_days: 14,
        }
    }

    fn seed_char_and_corp(ctx: &ServerContext, char_id: i64, corp_id: i64, station_id: Option<i64>, structure_id: Option<i64>, ship_id: i64) {
        ctx.db.with_mut_snapshot(|snap| {
            let mut roles = HashMap::new();
            roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER | eve_db::corp_roles::ROLE_STATION_MANAGER);
            snap.accounts.insert("pilot_demo".into(), eve_db::AccountRecord {
                user_id: 700, username: "pilot_demo".into(), password_token: "secret".into(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT, banned_until_unix: None, ban_reason: None,
            });
            snap.corporations.insert(corp_id, CorporationRecord {
                corporation_id: corp_id,
                name: "Apex Vanguard Logistics".into(),
                ticker: "APXVL".into(),
                ceo_id: char_id,
                alliance_id: Some(99_000_777),
                tax_rate: 0.05,
                master_wallet: IskCents::from_f64_isk(50_000_000.0),
                members: [char_id].into_iter().collect(),
                member_roles: roles,
                ..Default::default()
            });
            snap.characters.insert(char_id, make_char(char_id, corp_id, station_id, structure_id, ship_id));
            let loc = station_id.or(structure_id).unwrap_or(30_000_142);
            snap.items.insert(ship_id, make_item(ship_id, 587, char_id, loc, InventoryFlag::Hangar, 1, true, false));
        });
    }

    #[tokio::test]
    async fn contract_proxy_notifications_bulk_delete_and_dockable_items() {
        // parity: rpc:contractProxy.DeleteContractNotification, rpc:contractProxy.DeleteNotification
        // parity: rpc:contractProxy.DeleteMultipleContracts, rpc:contractProxy.GetItemsInDockableLocation
        eve_testkit::attest(&[
            "rpc:contractProxy.DeleteContractNotification",
            "rpc:contractProxy.DeleteNotification",
            "rpc:contractProxy.DeleteMultipleContracts",
            "rpc:contractProxy.GetItemsInDockableLocation",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let char_id = 95_100_001i64;
        let other_id = 95_100_002i64;
        let corp_id = 98_100_001i64;
        let station_id = 60_003_760i64;
        let ship_id = 950_100_001i64;
        seed_char_and_corp(&ctx, char_id, corp_id, Some(station_id), None, ship_id);

        let mut sess = SessionState::new(81_001);
        sess.user_id = Some(700);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.station_id = Some(station_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(30_000_142);
        let _ = ctx.db.acquire_online_lock(700, sess.session_id, Some(char_id), true, ctx.now_unix());

        let item_ore = 950_100_010i64;
        let item_bpc = 950_100_011i64;
        let item_escrow = 950_100_012i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(item_ore, make_item(item_ore, 34, char_id, station_id, InventoryFlag::Hangar, 2500, false, false));
            snap.items.insert(item_bpc, make_item(item_bpc, 688, char_id, station_id, InventoryFlag::Hangar, 1, true, true));
            snap.items.insert(item_escrow, make_item(item_escrow, 35, 0, station_id, InventoryFlag::Locked, 500, false, false));

            let mut c1 = make_contract(7001, other_id, "Auction", "Outstanding", station_id, 1_000_000.0, 0.0, 0.0, 0, corp_id);
            c1.current_bid = IskCents::from_f64_isk(2_000_000.0);
            c1.current_bidder_id = Some(other_id);
            c1.buyout = IskCents::from_f64_isk(5_000_000.0);
            c1.bid_history = vec![(char_id, IskCents::from_f64_isk(1_500_000.0), 1_700_000_010)];
            snap.contracts.insert(7001, c1);

            let c2 = make_contract(7002, other_id, "ItemExchange", "Outstanding", station_id, 500_000.0, 0.0, 0.0, char_id, corp_id);
            snap.contracts.insert(7002, c2);

            let mut c3 = make_contract(7003, char_id, "Courier", "Outstanding", station_id, 0.0, 250_000.0, 1_000_000.0, 0, corp_id);
            c3.item_ids = vec![item_escrow];
            snap.contracts.insert(7003, c3);
        });

        // 1. GetItemsInDockableLocation returns unlocked station hangar items
        let dock_items = call_rpc(&ctx, &mut sess, "contractProxy", "GetItemsInDockableLocation", &[PyRep::Integer(station_id), PyRep::Integer(0)])
            .await.expect("GetItemsInDockableLocation");
        let PyRep::List(items_list) = dock_items else { panic!("expected list") };
        assert_eq!(items_list.len(), 2, "must include Tritanium + BPC, excluding active ship and locked escrow item");
        assert_eq!(dict_get(&items_list[0], "itemID"), Some(&PyRep::Integer(item_ore)));
        assert_eq!(dict_get(&items_list[0], "stacksize"), Some(&PyRep::Integer(2500)));
        assert_eq!(dict_get(&items_list[0], "categoryID"), Some(&PyRep::Integer(4)));
        assert_eq!(dict_get(&items_list[1], "itemID"), Some(&PyRep::Integer(item_bpc)));
        assert_eq!(dict_get(&items_list[1], "singleton"), Some(&PyRep::Integer(2)));

        // 2. DeleteContractNotification removes outbid auction 7001 from GetMyBids
        let bids_before = call_rpc(&ctx, &mut sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false)]).await.expect("GetMyBids before");
        assert!(matches!(&bids_before, PyRep::List(l) if l.len() == 1));
        let del_c_notif = call_rpc(&ctx, &mut sess, "contractProxy", "DeleteContractNotification", &[PyRep::Integer(7001), PyRep::Integer(0)])
            .await.expect("DeleteContractNotification");
        assert_eq!(del_c_notif, PyRep::Bool(true));
        let bids_after = call_rpc(&ctx, &mut sess, "contractProxy", "GetMyBids", &[PyRep::Bool(false)]).await.expect("GetMyBids after");
        assert!(matches!(&bids_after, PyRep::List(l) if l.is_empty()));
        assert_eq!(ctx.db.with_snapshot(|s| s.contracts[&7001].bid_history.len()), 1, "bid_history must not be mutated");

        // 3. DeleteNotification dismisses assigned contract 7002 from GetLoginInfo().assignedToMe
        let login_before = call_rpc(&ctx, &mut sess, "contractProxy", "GetLoginInfo", &[]).await.expect("GetLoginInfo before");
        assert!(matches!(dict_get(&login_before, "assignedToMe"), Some(PyRep::List(l)) if l.len() == 1));
        let del_notif = call_rpc(&ctx, &mut sess, "contractProxy", "DeleteNotification", &[PyRep::Integer(7002), PyRep::Integer(0)])
            .await.expect("DeleteNotification");
        assert_eq!(del_notif, PyRep::Bool(true));
        let login_after = call_rpc(&ctx, &mut sess, "contractProxy", "GetLoginInfo", &[]).await.expect("GetLoginInfo after");
        assert!(matches!(dict_get(&login_after, "assignedToMe"), Some(PyRep::List(l)) if l.is_empty()));
        assert_eq!(ctx.db.with_snapshot(|s| s.contracts[&7002].assignee_id), char_id, "assignee_id must remain intact");

        // 4. DeleteMultipleContracts returns (deletedIDs, failedIDs), refunds reward, and unlocks item_escrow
        let bal_before = ctx.db.get_character(char_id).unwrap().balance.0;
        let multi_res = call_rpc(&ctx, &mut sess, "contractProxy", "DeleteMultipleContracts", &[PyRep::List(vec![PyRep::Integer(7003), PyRep::Integer(7001), PyRep::Integer(999999)])])
            .await.expect("DeleteMultipleContracts");
        assert_eq!(multi_res, PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(7003)]), PyRep::List(vec![PyRep::Integer(7001), PyRep::Integer(999999)])]));
        let bal_after = ctx.db.get_character(char_id).unwrap().balance.0;
        assert_eq!(bal_after - bal_before, IskCents::from_f64_isk(250_000.0).0);
        let restored_item = ctx.db.get_item(item_escrow).unwrap();
        assert_eq!(restored_item.owner_id, char_id);
        assert_eq!(restored_item.flag, InventoryFlag::Hangar);
    }

    #[tokio::test]
    async fn repair_svc_quotes_station_structure_and_fuel_jettison() {
        // parity: rpc:repairSvc.GetRepairQuotes, rpc:repairSvc.RepairItemsInStation
        // parity: rpc:repairSvc.RepairItemsInStructure, rpc:structure.JettisonStructureFuel
        // parity: feat:structures.fuel-jettison
        eve_testkit::attest(&[
            "rpc:repairSvc.GetRepairQuotes",
            "rpc:repairSvc.RepairItemsInStation",
            "rpc:repairSvc.RepairItemsInStructure",
            "rpc:structure.JettisonStructureFuel",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let char_id = 95_200_001i64;
        let corp_id = 98_200_001i64;
        let station_id = 60_003_760i64;
        let structure_id = 1_045_000_000_001i64;
        let ship_id = 950_200_001i64;
        let mod_id = 950_200_002i64;
        let struct_item_id = 950_200_003i64;
        seed_char_and_corp(&ctx, char_id, corp_id, Some(station_id), None, ship_id);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.dynamic_attributes.insert(attr_ids::DAMAGE, 40.0);
                ship.dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 20.0);
            }
            let mut mod_rec = make_item(mod_id, 440, char_id, ship_id, InventoryFlag::MedSlot0, 1, true, false);
            mod_rec.dynamic_attributes.insert(attr_ids::DAMAGE, 15.0);
            snap.items.insert(mod_id, mod_rec);

            let mut st_rec = make_item(struct_item_id, 587, char_id, structure_id, InventoryFlag::Hangar, 1, true, false);
            st_rec.dynamic_attributes.insert(attr_ids::DAMAGE, 25.0);
            snap.items.insert(struct_item_id, st_rec);

            snap.structures.insert(structure_id, UpwellStructureRecord {
                structure_id, type_id: 35832, owner_corp_id: corp_id, solar_system_id: 30_000_142,
                name: "Jita Perimeter Astrahus".into(), position: [150_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 500, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.items.insert(950_200_099, make_item(950_200_099, 35895, corp_id, structure_id, InventoryFlag::ServiceSlot0, 1, true, false));
            snap.module_online_states.insert(950_200_099, true);
        });

        let mut sess = SessionState::new(81_002);
        sess.user_id = Some(700);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.station_id = Some(station_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(30_000_142);
        let _ = ctx.db.acquire_online_lock(700, sess.session_id, Some(char_id), true, ctx.now_unix());

        // 1. GetRepairQuotes returns Dict[itemID -> List[KeyVal]]
        let quotes = call_rpc(&ctx, &mut sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![PyRep::Integer(ship_id)])])
            .await.expect("GetRepairQuotes");
        let PyRep::Dict(q_map) = quotes else { panic!("expected dict") };
        let ship_quotes = &q_map.iter().find(|(k, _)| k == &PyRep::Integer(ship_id)).unwrap().1;
        let PyRep::List(q_rows) = ship_quotes else { panic!("expected quote list") };
        assert_eq!(q_rows.len(), 2, "ship + fitted module damage quotes");
        assert_eq!(dict_get(&q_rows[0], "itemID"), Some(&PyRep::Integer(ship_id)));
        assert_eq!(dict_get(&q_rows[0], "damage"), Some(&PyRep::Real(60.0)));
        assert!(matches!(dict_get(&q_rows[0], "costToRepairOneUnitOfDamage"), Some(PyRep::Real(c)) if *c > 0.0));
        assert!(matches!(dict_get(&q_rows[0], "maxHealth"), Some(PyRep::Integer(h)) if *h > 0));

        // 2. RepairItemsInStation deducts ISK, clears damage on ship + module, and emits OnModuleRepaired
        let bal0 = ctx.db.get_character(char_id).unwrap().balance.0;
        let _ = call_rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStation", &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Real(5_000_000.0)])
            .await.expect("RepairItemsInStation");
        let bal1 = ctx.db.get_character(char_id).unwrap().balance.0;
        assert!(bal1 < bal0, "RepairItemsInStation must debit repair bill from character wallet");
        assert!(!ctx.db.get_item(ship_id).unwrap().dynamic_attributes.contains_key(&attr_ids::DAMAGE));
        assert!(!ctx.db.get_item(mod_id).unwrap().dynamic_attributes.contains_key(&attr_ids::DAMAGE));
        let notifs = ctx.drain_notifications_for_char(char_id);
        assert!(notifs.iter().any(|n| n.event == "OnModuleRepaired"));

        // 3. Dock in structure and test RepairItemsInStructure + structure.JettisonStructureFuel
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.structure_id = Some(structure_id);
            }
        });
        sess.station_id = None;
        sess.structure_id = Some(structure_id);

        let _ = call_rpc(&ctx, &mut sess, "repairSvc", "RepairItemsInStructure", &[PyRep::List(vec![PyRep::Integer(struct_item_id)])])
            .await.expect("RepairItemsInStructure");
        assert!(!ctx.db.get_item(struct_item_id).unwrap().dynamic_attributes.contains_key(&attr_ids::DAMAGE));

        // Verify unauthorized member without Station Manager role is rejected
        let member_char_id = 95_200_002i64;
        let member_ship_id = 950_200_004i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(
                member_char_id,
                make_char(member_char_id, corp_id, None, Some(structure_id), member_ship_id),
            );
            if let Some(c) = snap.corporations.get_mut(&corp_id) {
                c.members.insert(member_char_id);
                c.member_roles.insert(member_char_id, 0);
            }
        });

        let mut member_sess = SessionState::new(81_003);
        member_sess.user_id = Some(701);
        member_sess.character_id = Some(member_char_id);
        member_sess.corporation_id = Some(corp_id);
        member_sess.structure_id = Some(structure_id);
        member_sess.solar_system_id = Some(30_000_142);
        member_sess.corp_role = 0;

        let unauth_err = call_rpc(
            &ctx,
            &mut member_sess,
            "structure",
            "JettisonStructureFuel",
            &[PyRep::Integer(structure_id), PyRep::Integer(4247), PyRep::Integer(120)],
        )
        .await
        .expect_err("JettisonStructureFuel without Station Manager role must fail");
        // rpc-lint: allow error code assertion
        assert_eq!(unauth_err.code, "InsufficientRoles");

        // Grant Station Manager role (1 << 11) to member
        const ROLE_STATION_MANAGER: u64 = 1 << 11;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.corporations.get_mut(&corp_id) {
                c.member_roles.insert(member_char_id, ROLE_STATION_MANAGER);
            }
        });
        member_sess.corp_role = ROLE_STATION_MANAGER;

        let jet_res = call_rpc(
            &ctx,
            &mut member_sess,
            "structure",
            "JettisonStructureFuel",
            &[PyRep::Integer(structure_id), PyRep::Integer(4247), PyRep::Integer(120)],
        )
        .await
        .expect("JettisonStructureFuel with Station Manager role");
        let Some(PyRep::Integer(cont_id)) = dict_get(&jet_res, "containerID") else { panic!("containerID") };
        assert_eq!(dict_get(&jet_res, "remainingFuelBlocks"), Some(&PyRep::Integer(380)));
        let PyRep::List(jettisoned_ids) = dict_get(&jet_res, "jettisonedItemIDs").unwrap() else { panic!("jettisonedItemIDs") };
        assert_eq!(jettisoned_ids.len(), 1);
        let Some(PyRep::Integer(spawned_fuel_id)) = jettisoned_ids.first() else { panic!("fuel_id") };
        let moved_fuel = ctx.db.get_item(*spawned_fuel_id).unwrap();
        assert_eq!(moved_fuel.location_id, *cont_id);
        assert_eq!(moved_fuel.flag, InventoryFlag::Cargo);
        assert_eq!(moved_fuel.quantity, 120);

        let bad_fuel = call_rpc(
            &ctx,
            &mut member_sess,
            "structure",
            "JettisonStructureFuel",
            &[PyRep::Integer(structure_id), PyRep::Integer(34), PyRep::Integer(50)],
        )
        .await;
        assert!(matches!(bad_fuel, Err(e) if e.code == "ItemNotFuel"));
    }

    #[tokio::test]
    async fn search_i2_jump_clone_reset_warp_vector_and_dev_indices() {
        // parity: rpc:search.Query, rpc:search.QuickQuery, rpc:i2.GetItem, rpc:i2.IsPrimed
        // parity: rpc:jumpCloneSvc.ResetLastCloneJumpTime, rpc:warpVectorMgr.UseVector
        // parity: rpc:devIndexManager.GetDevelopmentIndicesForSystem
        // parity: feat:navigation.warp-vector
        eve_testkit::attest(&[
            "rpc:search.Query",
            "rpc:search.QuickQuery",
            "rpc:i2.GetItem",
            "rpc:i2.IsPrimed",
            "rpc:jumpCloneSvc.ResetLastCloneJumpTime",
            "rpc:warpVectorMgr.UseVector",
            "rpc:devIndexManager.GetDevelopmentIndicesForSystem",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let char_id = 95_300_001i64;
        let corp_id = 98_300_001i64;
        let ship_id = 950_300_001i64;
        let vector_id = 950_300_099i64;
        seed_char_and_corp(&ctx, char_id, corp_id, None, None, ship_id);

        ctx.db.with_mut_snapshot(|snap| {
            snap.system_states.insert(30_000_142, SystemDynamicState {
                solar_system_id: 30_000_142, sov_alliance_id: Some(99_000_777), military_index: 4,
                industrial_index: 3, strategic_index: 5, ..Default::default()
            });
        });

        let sys_handle = ctx.systems.get_or_spawn(30_000_142);
        let _ = sys_handle.spawn_ball(Ball {
            id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0,
            speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        }).await;
        let _ = sys_handle.spawn_ball(Ball {
            id: vector_id, type_id: 23, position: DVec3::new(10_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 1_000.0, radius: 50.0,
            speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        }).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(vector_id, eve_db::InventoryItemRecord {
                item_id: vector_id, type_id: 77_825, owner_id: char_id, location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1,
                dynamic_attributes: std::collections::HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            });
        });

        let mut sess = SessionState::new(81_003);
        sess.user_id = Some(700);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(30_000_142);
        let _ = ctx.db.acquire_online_lock(700, sess.session_id, Some(char_id), true, ctx.now_unix());

        // 1. search.Query & search.QuickQuery
        let q_res = call_rpc(&ctx, &mut sess, "search", "Query", &[PyRep::String("Jita".into()), PyRep::List(vec![PyRep::Integer(7), PyRep::Integer(9)])])
            .await.expect("search.Query");
        let PyRep::Dict(q_dict) = q_res else { panic!("expected dict") };
        let sys_matches = &q_dict.iter().find(|(k, _)| k == &PyRep::Integer(7)).unwrap().1;
        assert!(matches!(sys_matches, PyRep::List(l) if l.contains(&PyRep::Integer(30_000_142))));

        let mut kw = HashMap::new();
        kw.insert("exact".into(), PyRep::Integer(1));
        let qq_res = call_rpc_kwargs(&ctx, &mut sess, "search", "QuickQuery", &[PyRep::String("Valkyrie SearchPilot".into()), PyRep::List(vec![PyRep::Integer(2)])], kw)
            .await.expect("search.QuickQuery");
        assert_eq!(qq_res, PyRep::List(vec![PyRep::Integer(char_id)]));

        // 2. i2.IsPrimed & i2.GetItem require GM role
        // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
        let err_prime = call_rpc(&ctx, &mut sess, "i2", "IsPrimed", &[PyRep::Integer(ship_id)]).await;
        assert!(matches!(err_prime, Err(e) if e.code == "GMRoleRequired"));
        // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
        let err_item = call_rpc(&ctx, &mut sess, "i2", "GetItem", &[PyRep::Integer(ship_id)]).await;
        assert!(matches!(err_item, Err(e) if e.code == "GMRoleRequired"));

        sess.role = eve_db::service_roles::ROLE_ADMIN;
        // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
        let primed_yes = call_rpc(&ctx, &mut sess, "i2", "IsPrimed", &[PyRep::Integer(ship_id)])
            .await.expect("i2.IsPrimed(ship_id)");
        assert_eq!(primed_yes, PyRep::Bool(true));
        // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
        let primed_no = call_rpc(&ctx, &mut sess, "i2", "IsPrimed", &[PyRep::Integer(999_999_999)])
            .await.expect("i2.IsPrimed(missing)");
        assert_eq!(primed_no, PyRep::Bool(false));

        // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
        let item_rep = call_rpc(&ctx, &mut sess, "i2", "GetItem", &[PyRep::Integer(ship_id)])
            .await.expect("i2.GetItem(ship_id)");
        assert_eq!(dict_get(&item_rep, "typeID"), Some(&PyRep::Integer(587)));
        assert_eq!(dict_get(&item_rep, "itemID"), Some(&PyRep::Integer(ship_id)));

        // 3. jumpCloneSvc.ResetLastCloneJumpTime requires GM role
        sess.role = eve_db::service_roles::ROLE_PLAYER_DEFAULT;
        let reset_denied = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "ResetLastCloneJumpTime", &[]).await;
        assert!(matches!(reset_denied, Err(e) if e.code == "GMRoleRequired"));

        sess.role = eve_db::service_roles::ROLE_ADMIN;
        assert_eq!(ctx.db.get_character(char_id).unwrap().last_clone_jump_unix, 1_700_000_000);
        let _ = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "ResetLastCloneJumpTime", &[]).await.expect("ResetLastCloneJumpTime");
        assert_eq!(ctx.db.get_character(char_id).unwrap().last_clone_jump_unix, 0);
        let jc_state = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[]).await.expect("GetCloneState");
        assert_eq!(dict_get(&jc_state, "timeLastJump"), Some(&PyRep::None));

        // 4. warpVectorMgr.UseVector enforces scram check and moves ship ball along vector
        sys_handle.try_add_mobile_warp_disruptor(888_001, DVec3::ZERO, 20_000.0);
        let scram_err = call_rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(vector_id)]).await;
        assert!(matches!(scram_err, Err(e) if e.code == "WarpScrambled"));
        sys_handle.try_remove_warp_bubble(888_001);

        let missing_vec = call_rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(888_888_888)]).await;
        assert!(matches!(missing_vec, Err(e) if e.code == "InvalidVectorItem"));

        let vec_res = call_rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(vector_id)]).await.expect("warpVectorMgr.UseVector");
        assert_eq!(dict_get(&vec_res, "shipID"), Some(&PyRep::Integer(ship_id)));
        let (_, after_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        match after_ball.mode {
            eve_destiny::MovementMode::Warp { destination, .. } => {
                assert!(destination.x > 50_000.0, "ship ball must warp along vector (+X)");
            }
            ref other => panic!("ship ball must enter warp along vector (+X), got {other:?}"),
        }

        // 5. devIndexManager.GetDevelopmentIndicesForSystem returns 1583, 1584, 1615
        let dev_res = call_rpc(&ctx, &mut sess, "devIndexManager", "GetDevelopmentIndicesForSystem", &[PyRep::Integer(30_000_142)])
            .await.expect("GetDevelopmentIndicesForSystem");
        let PyRep::Dict(dev_map) = dev_res else { panic!("expected dict") };
        let mil = &dev_map.iter().find(|(k, _)| k == &PyRep::Integer(1583)).unwrap().1;
        assert_eq!(dict_get(mil, "level"), Some(&PyRep::Integer(4)));
        assert_eq!(dict_get(mil, "increasing"), Some(&PyRep::Integer(1)));
        let strat = &dev_map.iter().find(|(k, _)| k == &PyRep::Integer(1615)).unwrap().1;
        assert_eq!(dict_get(strat, "level"), Some(&PyRep::Integer(5)));
    }
}

pub(crate) mod rpc_missing {
    // Integration test suite for final 20 missing RPC rows.
    //
    // Validates real V24 argument layouts, persistent state mutations,
    // and live-server return shapes for all 20 endpoints.


    use crate::common;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const PILOT1: i64 = 90_000_001;
    const PILOT2: i64 = 90_000_002;

    #[tokio::test]
    async fn test_rpc_missing_batch3_part1() {
        // parity: rpc:alert.LogModeChanged, rpc:alert.SendProxyStackTraceAlert
        // parity: rpc:clientStatLogger.LogString, rpc:externalQueueMgr.PublishProxyEvent
        // parity: rpc:machoNet.ReloadClientCodeHash, rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID
        eve_testkit::attest(&[
            "rpc:alert.LogModeChanged",
            "rpc:alert.SendProxyStackTraceAlert",
            "rpc:clientStatLogger.LogString",
            "rpc:externalQueueMgr.PublishProxyEvent",
            "rpc:machoNet.ReloadClientCodeHash",
            "rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID",
            ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();

        let mut sess1 = SessionState::new(1001);
        common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        sess1.role |= eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
        ctx.dispatch_rpc(
            &mut sess1,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT1)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID sess1");

        // 1. rpc:alert.LogModeChanged
        let log_mode_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "alert",
                None,
                "LogModeChanged",
                &[PyRep::Integer(201), PyRep::String("I".into())],
                &kw,
                None,
            )
            .await
            .expect("alert.LogModeChanged");
        assert_eq!(log_mode_res, PyRep::None);

        let get_mode_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "alert",
                None,
                "GetLogModeForError",
                &[PyRep::Integer(201)],
                &kw,
                None,
            )
            .await
            .expect("alert.GetLogModeForError");
        match get_mode_res {
            PyRep::Tuple(items) => {
                assert_eq!(items[0], PyRep::Integer(201));
                assert_eq!(items[1], PyRep::ascii_bytes("I"));
            }
            other => panic!("Unexpected GetLogModeForError shape: {other:?}"),
        }
        let persisted_modes = ctx.db.with_snapshot(|s| {
            s.service_state_blobs.get("alert_log_modes").cloned()
        });
        assert!(persisted_modes.is_some(), "alert_log_modes must be persisted");
        assert!(persisted_modes.unwrap().contains("201"));

        // 2. rpc:alert.SendProxyStackTraceAlert
        let proxy_alert_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "alert",
                None,
                "SendProxyStackTraceAlert",
                &[
                    PyRep::Tuple(vec![PyRep::Integer(777_888), PyRep::String("proxy_key".into())]),
                    PyRep::String("Traceback:\n  File foo.py, in bar\nError".into()),
                    PyRep::String("B".into()),
                ],
                &kw,
                None,
            )
            .await
            .expect("alert.SendProxyStackTraceAlert");
        match proxy_alert_res {
            PyRep::Tuple(items) => {
                assert!(items[0].as_i64().unwrap_or(0) > 0);
                assert_eq!(items[1], PyRep::ascii_bytes("B"));
            }
            other => panic!("Unexpected SendProxyStackTraceAlert shape: {other:?}"),
        }
        let proxy_traces = ctx.db.with_snapshot(|s| {
            s.service_state_blobs.get("alert_proxy_traces").cloned()
        });
        assert!(proxy_traces.is_some(), "alert_proxy_traces must be persisted");
        assert!(proxy_traces.unwrap().contains("777888"));

        // 3. localizationServer.UpdateLocalizationQAWrap (DERIVED role-gated boolean toggle + GetAllTextChanges wrapping)
        let mut unpriv_loc_sess = SessionState::new(902_099);
        common::login_pilot(&ctx, &mut unpriv_loc_sess, "other", "other-secret").await;
        unpriv_loc_sess.role = 0;
        let unpriv_qa_err = ctx
            .dispatch_rpc(
                &mut unpriv_loc_sess,
                "localizationServer",
                None,
                "UpdateLocalizationQAWrap",
                &[PyRep::Bool(true)],
                &kw,
                None,
            )
            .await
            .expect_err("unprivileged UpdateLocalizationQAWrap must fail with RoleRequired");
        assert_eq!(unpriv_qa_err.code, "RoleRequired");

        ctx.db.with_mut_snapshot(|snap| {
            let mut overrides = std::collections::BTreeMap::new();
            overrides.insert(99991_i64, "Overridden QA String".to_string());
            snap.owner_notes.insert(
                -8_960_000_000_000,
                eve_db::OwnerNoteRecord {
                    note_id: -8_960_000_000_000,
                    character_id: 0,
                    item_id: 0,
                    label: "localization_overrides".into(),
                    note: serde_json::to_string(&overrides).unwrap(),
                    timestamp_unix: 0,
                },
            );
        });

        // Enable QA wrap for sess1 and verify GetAllTextChanges wraps localized strings
        assert_eq!(
            ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "UpdateLocalizationQAWrap", &[PyRep::Bool(true)], &kw, None).await.expect("UpdateLocalizationQAWrap(true)"),
            PyRep::Bool(true)
        );
        let wrapped_changes = ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "GetAllTextChanges", &[], &kw, None).await.expect("GetAllTextChanges wrapped");
        assert_eq!(
            match &wrapped_changes {
                PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_i64() == Some(99991)).map(|(_, v)| v),
                _ => None,
            },
            Some(&PyRep::String("[QA:99991] Overridden QA String".into()))
        );
        assert_eq!(
            ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "UpdateLocalizationQAWrap", &[PyRep::Bool(false)], &kw, None).await.expect("UpdateLocalizationQAWrap(false)"),
            PyRep::Bool(false)
        );
        let unwrapped_changes = ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "GetAllTextChanges", &[], &kw, None).await.expect("GetAllTextChanges unwrapped");
        assert_eq!(
            match &unwrapped_changes {
                PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_i64() == Some(99991)).map(|(_, v)| v),
                _ => None,
            },
            Some(&PyRep::String("Overridden QA String".into()))
        );

        // 4. rpc:localizationServer.ReloadFSDPickle
        let reload_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "localizationServer",
                None,
                "ReloadFSDPickle",
                &[PyRep::String("fsd_english.pickle".into())],
                &kw,
                None,
            )
            .await;
        assert!(reload_res.is_err(), "ReloadFSDPickle must return an error until implemented");
        if let Err(e) = reload_res {
            assert_eq!(e.code, "UnimplementedRPC");
        }

        // 5. rpc:clientStatLogger.LogString
        let log_str_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "clientStatLogger",
                None,
                "LogString",
                &[PyRep::String("jump_monitor:jump_done:30000142".into())],
                &kw,
                None,
            )
            .await
            .expect("clientStatLogger.LogString");
        assert_eq!(log_str_res, PyRep::None);
        let logged_strings = ctx.db.with_snapshot(|s| {
            s.service_state_blobs.get("client_stat_logger:strings").cloned()
        });
        assert!(logged_strings.is_some());
        assert!(logged_strings.unwrap().contains("jump_monitor:jump_done:30000142"));

        // 6. rpc:externalQueueMgr.PublishProxyEvent (7 args)
        let queue_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "externalQueueMgr",
                None,
                "PublishProxyEvent",
                &[
                    PyRep::String("proxy_session_init".into()),
                    PyRep::String("session_token_123".into()),
                    PyRep::Integer(1),
                    PyRep::Integer(2),
                    PyRep::None,
                    PyRep::None,
                    PyRep::Dict(vec![]),
                ],
                &kw,
                None,
            )
            .await
            .expect("externalQueueMgr.PublishProxyEvent");
        assert_eq!(queue_res, PyRep::None);
        let queue_blob = ctx.db.with_snapshot(|s| {
            s.service_state_blobs.get("external_queue_events").cloned()
        });
        assert!(queue_blob.is_some());
        assert!(queue_blob.unwrap().contains("proxy_session_init"));

        // 7. rpc:machoNet.ReloadClientCodeHash (0 args)
        let hash_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "machoNet",
                None,
                "ReloadClientCodeHash",
                &[],
                &kw,
                None,
            )
            .await
            .expect("machoNet.ReloadClientCodeHash");
        match hash_res {
            PyRep::String(s) => assert!(!s.is_empty()),
            other => panic!("Unexpected ReloadClientCodeHash shape: {other:?}"),
        }
        let hash_blob = ctx.db.with_snapshot(|s| {
            s.service_state_blobs.get("macho_client_code_hash").cloned()
        });
        assert!(hash_blob.is_some());

        // 8. rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID (0 args -> (default_addr, node_map))
        ctx.db.with_mut_snapshot(|s| {
            let mut table = HashMap::new();
            table.insert("42".to_string(), "10.0.0.42:28042".to_string());
            s.service_state_blobs.insert("cluster_node_table".into(), serde_json::to_string(&table).unwrap());
        });
        let tunnel_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "tcpRawProxyService",
                None,
                "GetESPTunnelingAddressByNodeID",
                &[],
                &kw,
                None,
            )
            .await
            .expect("tcpRawProxyService.GetESPTunnelingAddressByNodeID");
        match tunnel_res {
            PyRep::Tuple(items) => {
                assert_eq!(items.len(), 2);
                assert!(items[0].as_str().is_some());
                match &items[1] {
                    PyRep::Dict(map) => {
                        let entry = map.iter().find(|(k, _)| k.as_i64() == Some(42));
                        assert_eq!(entry.map(|(_, v)| v), Some(&PyRep::String("10.0.0.42:28042".into())));
                    }
                    other => panic!("Unexpected node map shape: {other:?}"),
                }
            }
            other => panic!("Unexpected GetESPTunnelingAddressByNodeID shape: {other:?}"),
        }

        // 9. rpc:loadService.StartLoad (1 arg: loadSpec)
        let target_sys = sess1.solar_system_id.unwrap_or(30_000_142);
        let elapsed_before = ctx.systems.get_system_sim_elapsed(target_sys);
        let start_load_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "loadService",
                None,
                "StartLoad",
                &[PyRep::Dict(vec![
                    (PyRep::String("profile".into()), PyRep::String("load_profile_std".into())),
                    (PyRep::String("ticks".into()), PyRep::Integer(4)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("loadService.StartLoad");
        assert_eq!(start_load_res, PyRep::None);
        let elapsed_after = ctx.systems.get_system_sim_elapsed(target_sys);
        assert!(
            elapsed_after >= elapsed_before + 4.0,
            "StartLoad must step target system (before={elapsed_before}, after={elapsed_after})"
        );
        let load_blob = ctx.db.with_snapshot(|s| {
            s.service_state_blobs.get("load_service_state").cloned()
        });
        assert!(load_blob.is_some());
        let load_blob_str = load_blob.unwrap();
        assert!(load_blob_str.contains("\"active\":true"));
        assert!(load_blob_str.contains("\"ticks_executed\":4"));

        // 10. rpc:loadService.StopLoad
        let stop_load_res = ctx
            .dispatch_rpc(&mut sess1, "loadService", None, "StopLoad", &[], &kw, None)
            .await
            .expect("loadService.StopLoad");
        assert_eq!(stop_load_res, PyRep::None);
        let load_blob2 = ctx.db.with_snapshot(|s| {
            s.service_state_blobs.get("load_service_state").cloned()
        });
        assert!(load_blob2.unwrap().contains("\"active\":false"));
    }

    #[tokio::test]
    async fn test_rpc_missing_batch3_part2() {
        // parity: rpc:cache.Rowset, rpc:cache.Setting
        // parity: rpc:sessionMgr.EndAllGameSessions, rpc:sessionMgr.RemoveSessionsFromServer
        // parity: rpc:userSvc.ProcessEVESessionCreated, rpc:raffleMgr.QA_SeedRaffles, rpc:DB2.CallProc
        // DATA GAP: Triaged NA in docs/parity/triage.json (dev/QA/cluster RPCs): rpc:debug.Exec
        eve_testkit::attest(&[
            "rpc:cache.Rowset",
            "rpc:cache.Setting",
            "rpc:sessionMgr.EndAllGameSessions",
            "rpc:sessionMgr.RemoveSessionsFromServer",
            "rpc:userSvc.ProcessEVESessionCreated",
            "rpc:raffleMgr.QA_SeedRaffles",
            "rpc:DB2.CallProc",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let kw = HashMap::new();

        let mut sess1 = SessionState::new(1001);
        common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        sess1.role |= eve_db::service_roles::ROLE_ADMIN;
        ctx.dispatch_rpc(
            &mut sess1,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT1)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID sess1");

        let mut sess2 = SessionState::new(1002);
        common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        ctx.dispatch_rpc(
            &mut sess2,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT2)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID sess2");

        // 11. rpc:loadService.SetConfig
        let set_config_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "loadService",
                None,
                "SetConfig",
                &[PyRep::String("concurrency".into()), PyRep::Integer(16)],
                &kw,
                None,
            )
            .await
            .expect("loadService.SetConfig");
        assert_eq!(set_config_res, PyRep::None);

        // 12. rpc:loadService.GetTotalStats
        let stats_res = ctx
            .dispatch_rpc(&mut sess1, "loadService", None, "GetTotalStats", &[], &kw, None)
            .await
            .expect("loadService.GetTotalStats");
        assert_eq!(stats_res.dict_get("active"), Some(&PyRep::Bool(false)));
        assert!(stats_res.dict_get("sde_types_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        assert!(stats_res.dict_get("sde_solar_systems_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        let cfg = stats_res.dict_get("config").expect("config dict");
        assert_eq!(cfg.dict_get("concurrency"), Some(&PyRep::Integer(16)));

        // 13. rpc:sessionMgr.EndAllGameSessions
        let (tx_victim, mut rx_victim) = tokio::sync::mpsc::unbounded_channel();
        ctx.register_session_closer(2002, tx_victim);
        assert!(ctx.session_closers.read().unwrap().contains_key(&2002));
        let end_sessions_res = ctx
            .dispatch_rpc(&mut sess1, "sessionMgr", None, "EndAllGameSessions", &[], &kw, None)
            .await
            .expect("sessionMgr.EndAllGameSessions");
        assert_eq!(end_sessions_res, PyRep::None);
        assert_eq!(
            rx_victim.try_recv().expect("victim session must receive EndAllGameSessions"),
            ("EndAllGameSessions".into(), "All game sessions ended by administrator".into())
        );
        assert!(!ctx.session_closers.read().unwrap().contains_key(&2002));
        let active_blob = ctx.db.with_snapshot(|s| s.service_state_blobs.get("active_sessions").cloned());
        assert_eq!(active_blob.as_deref(), Some("[]"));

        // 14. rpc:sessionMgr.RemoveSessionsFromServer
        let unpriv_rm_err = ctx
            .dispatch_rpc(&mut sess2, "sessionMgr", None, "RemoveSessionsFromServer", &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(1001)])], &kw, None)
            .await;
        assert!(unpriv_rm_err.is_err(), "Unprivileged session must be rejected for RemoveSessionsFromServer");

        let (tx_1002, mut rx_1002) = tokio::sync::mpsc::unbounded_channel();
        let (tx_1003, mut rx_1003) = tokio::sync::mpsc::unbounded_channel();
        ctx.register_session_closer(1002, tx_1002);
        ctx.register_session_closer(1003, tx_1003);
        ctx.db.with_mut_snapshot(|s| {
            s.service_state_blobs.insert("active_sessions".into(), "[1001, 1002, 1003]".into());
        });
        let rm_sessions_res = ctx
            .dispatch_rpc(&mut sess1, "sessionMgr", None, "RemoveSessionsFromServer", &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(1002), PyRep::Integer(1003)])], &kw, None)
            .await
            .expect("sessionMgr.RemoveSessionsFromServer");
        assert_eq!(rm_sessions_res, PyRep::None);
        assert_eq!(rx_1002.try_recv().expect("1002 closed").0, "RemoveSession");
        assert_eq!(rx_1003.try_recv().expect("1003 closed").0, "RemoveSession");
        assert!(!ctx.session_closers.read().unwrap().contains_key(&1002));
        assert!(!ctx.session_closers.read().unwrap().contains_key(&1003));
        let remaining_sessions = ctx.db.with_snapshot(|s| s.service_state_blobs.get("active_sessions").cloned().unwrap());
        assert!(remaining_sessions.contains("1001"));
        assert!(!remaining_sessions.contains("1002"));

        // 15. rpc:userSvc.ProcessEVESessionCreated
        let wrong_char_err = ctx
            .dispatch_rpc(&mut sess2, "userSvc", None, "ProcessEVESessionCreated", &[PyRep::Integer(PILOT1)], &kw, None)
            .await
            .expect_err("foreign character_id must be rejected even when session.character_id is set");
        assert_eq!(wrong_char_err.message, "CharacterDoesNotBelongToUser");

        let process_sess_res = ctx
            .dispatch_rpc(&mut sess1, "userSvc", None, "ProcessEVESessionCreated", &[PyRep::Integer(PILOT1), PyRep::Integer(1001)], &kw, None)
            .await
            .expect("userSvc.ProcessEVESessionCreated");
        assert_eq!(process_sess_res, PyRep::None);
        let user_sess_blob = ctx.db.with_snapshot(|s| s.service_state_blobs.get("user_sessions").cloned().unwrap());
        let entries: Vec<serde_json::Value> = serde_json::from_str(&user_sess_blob).expect("valid user_sessions json");
        assert!(entries.iter().any(|e| {
            e["user_id"].as_u64() == sess1.user_id
                && e["session_id"].as_u64() == Some(1001)
                && e["character_id"].as_i64() == Some(PILOT1)
        }));

        // 16. rpc:cache.Rowset
        let cache_rowset_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "cache",
                None,
                "Rowset",
                &[PyRep::String("roles".into())],
                &kw,
                None,
            )
            .await
            .expect("cache.Rowset");
        match cache_rowset_res {
            PyRep::Rowset { columns, rows } => {
                assert!(columns.contains(&"roleID".to_string()));
                assert!(columns.contains(&"roleName".to_string()));
                assert!(!rows.is_empty());
            }
            other => panic!("Unexpected cache.Rowset shape: {other:?}"),
        }

        // 17. rpc:cache.Setting
        let cache_setting_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "cache",
                None,
                "Setting",
                &[PyRep::String("zsystem".into()), PyRep::String("SP-Allowed-Extensions".into())],
                &kw,
                None,
            )
            .await
            .expect("cache.Setting");
        match cache_setting_res {
            PyRep::String(exts) => {
                assert!(exts.contains("txt"));
                assert!(exts.contains("csv"));
            }
            other => panic!("Unexpected cache.Setting shape: {other:?}"),
        }
        let custom_set_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "cache",
                None,
                "Setting",
                &[
                    PyRep::String("zsystem".into()),
                    PyRep::String("CustomTestKey".into()),
                    PyRep::String("CustomTestVal".into()),
                ],
                &kw,
                None,
            )
            .await
            .expect("cache.Setting update");
        assert_eq!(custom_set_res, PyRep::String("CustomTestVal".into()));

        // 18. rpc:raffleMgr.QA_SeedRaffles
        // Verify unprivileged rejection
        let unpriv_seed_err = ctx
            .dispatch_rpc(&mut sess2, "raffleMgr", None, "QA_SeedRaffles", &[PyRep::Integer(1)], &kw, None)
            .await;
        assert!(unpriv_seed_err.is_err(), "Unprivileged session must be rejected for QA_SeedRaffles");

        let raffle_seed_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "raffleMgr",
                None,
                "QA_SeedRaffles",
                &[PyRep::Integer(2)],
                &kw,
                None,
            )
            .await
            .expect("raffleMgr.QA_SeedRaffles");
        match raffle_seed_res {
            PyRep::List(items) => {
                assert_eq!(items.len(), 2);
            }
            other => panic!("Unexpected QA_SeedRaffles shape: {other:?}"),
        }
        let (seeded_notes, item_is_escrowed) = ctx.db.with_snapshot(|s| {
            let count = s.owner_notes.values().filter(|n| n.label.starts_with("raffle:")).count();
            let escrowed = s.items.values().any(|it| {
                it.owner_id == -52_568 && it.location_id == -52_568 && it.flag == eve_db::InventoryFlag::Locked
            });
            (count, escrowed)
        });
        assert!(seeded_notes >= 2);
        assert!(item_is_escrowed, "Seeded raffle item must be escrowed at -52_568 with Locked flag");

        // 19. rpc:DB2.CallProc (2 args: proc_name, role_mask)
        let mut unauth = SessionState::new(9998);
        let attack_args = [
            PyRep::String("zcluster.Attacks_Insert".into()),
            PyRep::String("attack_data".into()),
            PyRep::String("test".into()),
        ];
        let unauth_attacks_err = ctx
            .dispatch_rpc(&mut unauth, "DB2", None, "CallProc", &attack_args, &kw, None)
            .await;
        assert!(unauth_attacks_err.is_err(), "Unauthenticated session must be rejected for DB2.CallProc zcluster.Attacks_Insert");

        let mut unpriv = SessionState::new(9999);
        unpriv.user_id = Some(8888);
        unpriv.role = eve_db::service_roles::ROLE_PLAYER_DEFAULT;
        let auth_err = ctx
            .dispatch_rpc(&mut unpriv, "DB2", None, "CallProc", &[PyRep::String("zcluster.Nodes_PolarisID".into())], &kw, None)
            .await;
        assert!(auth_err.is_err(), "Non-admin must be rejected for DB2.CallProc zcluster.Nodes_PolarisID");

        let attacks_ok = ctx
            .dispatch_rpc(&mut unpriv, "DB2", None, "CallProc", &attack_args, &kw, None)
            .await
            .expect("Authenticated non-admin must be allowed to call DB2.CallProc zcluster.Attacks_Insert");
        assert_eq!(attacks_ok, PyRep::None);

        let polaris_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "DB2",
                None,
                "CallProc",
                &[PyRep::String("zcluster.Nodes_PolarisID".into())],
                &kw,
                None,
            )
            .await
            .expect("DB2.CallProc polaris");
        assert_eq!(polaris_res, PyRep::Integer(1));

        let users_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "DB2",
                None,
                "CallProc",
                &[
                    PyRep::String("zuser.Users_SelectByRole".into()),
                    PyRep::Integer(eve_db::service_roles::ROLE_PLAYER_DEFAULT as i64),
                ],
                &kw,
                None,
            )
            .await
            .expect("DB2.CallProc select users");
        match users_res {
            PyRep::Rowset { columns, rows } => {
                assert!(columns.contains(&"userID".to_string()));
                assert!(columns.contains(&"userName".to_string()));
                assert!(!rows.is_empty());
            }
            other => panic!("Unexpected Users_SelectByRole shape: {other:?}"),
        }

        // 20. rpc:debug.Exec (5 args + kwargs)
        let debug_auth_err = ctx
            .dispatch_rpc(&mut unpriv, "debug", None, "Exec", &[PyRep::String("print('hello')".into())], &kw, None)
            .await;
        assert!(debug_auth_err.is_err(), "Non-admin/programmer must be rejected for debug.Exec");

        let debug_err = ctx
            .dispatch_rpc(
                &mut sess1,
                "debug",
                None,
                "Exec",
                &[
                    PyRep::String("self.stats()".into()),
                    PyRep::Integer(1),
                    PyRep::None,
                    PyRep::String("extra".into()),
                    PyRep::Bool(true),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("debug.Exec must fail closed with UnimplementedRPC");
        assert_eq!(debug_err.code, "UnimplementedRPC");
    }
}

pub(crate) mod rpc_sweep_group1a_bookmarks {
    // Integration tests for 13 rock-solid `rpc:*` parity rows across `beyonce` and `contractProxy`.

    use crate::common;
    use eve_db::{
        BookmarkFolderRecord, ContractRecord, CorporationRecord, CosmicSignatureRecord, InventoryFlag,
        InventoryItemRecord, IskCents, UpwellStructureRecord, WorldRepository,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use std::sync::Arc;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const JITA_44: i64 = 60_003_760;
    const JITA_SYS: u32 = 30_000_142;

    async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, cid: i64) -> SessionState {
        let mut s = SessionState::new(sid);
        let empty = HashMap::new();
        ctx.dispatch_rpc(&mut s, "authentication", None, "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())], &empty, None).await.expect("Login");
        ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(cid)], &empty, None).await.expect("SelectCharacterID");
        s
    }

    async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
        ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
    }

    async fn call_err(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> String {
        ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await.expect_err(&format!("{svc}.{method} should have failed")).code
    }

    fn mk_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
        InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
    }

    fn mk_structure(structure_id: i64, type_id: u32, owner_corp_id: i64) -> UpwellStructureRecord {
        UpwellStructureRecord {
            structure_id, type_id, owner_corp_id, solar_system_id: JITA_SYS, name: "Test Structure".into(),
            state: "ShieldVulnerable".into(), position: [100_000.0, 0.0, 0.0], fuel_blocks: 100,
            quantum_core_installed: true, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        }
    }

    // parity: rpc:beyonce.BookmarkLocation, rpc:beyonce.BookmarkScanResult
    #[tokio::test]
    async fn test_rpc_sweep_group1a_bookmarks() {
        eve_testkit::attest(&["rpc:beyonce.BookmarkLocation", "rpc:beyonce.BookmarkScanResult"]);
        let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
        common::pin_clock(&ctx);
        let mut s1 = login_select(&ctx, 8101, "demo", "demo-secret", P1).await;

        let p1_folder_id = 501i64;
        let foreign_folder_id = 999i64;
        let bmk_item_id = 1_910_000_001i64;

        ctx.db.with_mut_snapshot(|snap| {
            snap.bookmark_folders.insert(
                p1_folder_id,
                BookmarkFolderRecord {
                    folder_id: p1_folder_id,
                    owner_id: P1,
                    folder_name: "Fleet Pins".into(),
                    is_shared: false,
                    corporation_id: None,
                    ..Default::default()
                },
            );
            snap.items.insert(
                bmk_item_id,
                mk_item(bmk_item_id, 34, P1, JITA_44, InventoryFlag::Hangar, 100, false),
            );
            snap.cosmic_signatures.entry(JITA_SYS).or_default().push(CosmicSignatureRecord {
                signature_id: "BMK-101".into(),
                solar_system_id: JITA_SYS,
                sig_group: "Data".into(),
                type_name: "Central Data Core".into(),
                position: [12_000.0, 24_000.0, 36_000.0],
                signal_strength: 1.0,
                wormhole_type_id: Some(9988),
                destination_system_id: None,
                created_unix: 1000,
                lifetime_seconds: 0,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
        });

        // Exploit regression tests for BookmarkLocation:
        // 1. Specifying an inaccessible / unowned folder must fail with FolderNotFound
        assert_eq!(
            call_err(
                &ctx,
                &mut s1,
                "beyonce",
                "BookmarkLocation",
                &[
                    PyRep::Integer(bmk_item_id),
                    PyRep::Integer(foreign_folder_id),
                    PyRep::String("Pin".into()),
                    PyRep::String("Note".into()),
                    PyRep::Integer(1),
                    PyRep::Integer(JITA_SYS as i64)
                ]
            )
            .await,
            "FolderNotFound"
        );

        // 2. Legitimate BookmarkLocation with owned folder derives typeID (34) and locationID (JITA_44) from item:
        let bm_loc = call(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkLocation",
            &[
                PyRep::Integer(bmk_item_id),
                PyRep::Integer(p1_folder_id),
                PyRep::String("Jita Trit".into()),
                PyRep::String("Pin".into()),
                PyRep::Integer(1),
                PyRep::Integer(JITA_SYS as i64),
            ],
        )
        .await;
        let PyRep::Tuple(bm_tuple) = bm_loc else { panic!("expected 8-tuple from beyonce.BookmarkLocation") };
        assert_eq!(bm_tuple.len(), 8);
        let bm_id = bm_tuple[0].as_i64().unwrap();
        assert!(bm_id > 0);
        assert_eq!(bm_tuple[1], PyRep::Integer(bmk_item_id));
        assert_eq!(bm_tuple[2], PyRep::Integer(34)); // derived from item type
        assert_eq!(bm_tuple[6], PyRep::Integer(JITA_44)); // derived from item location
        assert_eq!(bm_tuple[7], PyRep::Integer(p1_folder_id));

        // Exploit regression tests for BookmarkScanResult:
        // 1. Caller docked in station must fail with NotInSpace
        assert_eq!(
            call_err(
                &ctx,
                &mut s1,
                "beyonce",
                "BookmarkScanResult",
                &[
                    PyRep::Integer(JITA_SYS as i64),
                    PyRep::String("Scanned Core".into()),
                    PyRep::String("Note".into()),
                    PyRep::String("BMK-101".into()),
                    PyRep::Integer(p1_folder_id),
                ]
            )
            .await,
            "NotInSpace"
        );

        // Undock into space
        call(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;

        // 2. Specifying a different solar system must fail with NotInSameSystem
        assert_eq!(
            call_err(
                &ctx,
                &mut s1,
                "beyonce",
                "BookmarkScanResult",
                &[
                    PyRep::Integer(30_000_144),
                    PyRep::String("Scanned Core".into()),
                    PyRep::String("Note".into()),
                    PyRep::String("BMK-101".into()),
                    PyRep::Integer(p1_folder_id),
                ]
            )
            .await,
            "NotInSameSystem"
        );

        // 3. Specifying an invalid / foreign folder must fail with FolderNotFound
        assert_eq!(
            call_err(
                &ctx,
                &mut s1,
                "beyonce",
                "BookmarkScanResult",
                &[
                    PyRep::Integer(JITA_SYS as i64),
                    PyRep::String("Scanned Core".into()),
                    PyRep::String("Note".into()),
                    PyRep::String("BMK-101".into()),
                    PyRep::Integer(foreign_folder_id),
                ]
            )
            .await,
            "FolderNotFound"
        );

        // 4. Legitimate BookmarkScanResult in space derives signature typeID (9988) on server:
        let bm_scan = call(
            &ctx,
            &mut s1,
            "beyonce",
            "BookmarkScanResult",
            &[
                PyRep::Integer(JITA_SYS as i64),
                PyRep::String("Scanned Data".into()),
                PyRep::String("Note".into()),
                PyRep::String("BMK-101".into()),
                PyRep::Integer(p1_folder_id),
            ],
        )
        .await;
        let PyRep::Tuple(scan_tuple) = bm_scan else { panic!("expected 8-tuple from beyonce.BookmarkScanResult") };
        assert_eq!(scan_tuple.len(), 8);
        assert_eq!(scan_tuple[1], PyRep::Integer(JITA_SYS as i64));
        assert_eq!(scan_tuple[2], PyRep::Integer(9988)); // server-derived signature typeID
        assert_eq!(scan_tuple[3], PyRep::Real(12_000.0));
        assert_eq!(scan_tuple[6], PyRep::Integer(JITA_SYS as i64));
        assert_eq!(scan_tuple[7], PyRep::Integer(p1_folder_id));
    }

    // parity: rpc:contractProxy.SplitStack
    #[tokio::test]
    async fn test_rpc_sweep_group1b_stack() {
        eve_testkit::attest(&["rpc:contractProxy.SplitStack"]);
        let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
        common::pin_clock(&ctx);
        let mut s1 = login_select(&ctx, 8111, "demo", "demo-secret", P1).await;
        let mut s2 = login_select(&ctx, 8112, "other", "other-secret", P2).await;

        let stack_id = 1_910_000_010i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                stack_id,
                InventoryItemRecord {
                    item_id: stack_id,
                    type_id: 34,
                    owner_id: P1,
                    location_id: JITA_44,
                    flag: InventoryFlag::Hangar,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        // Exploit regression tests for SplitStack:
        // 1. Splitting with quantity <= 0 must fail
        assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(0), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");
        // 2. Splitting with quantity >= stack size must fail
        assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(100), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");
        assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(150), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");
        // 3. Unauthorized pilot (P2) splitting P1's item must fail
        assert_eq!(call_err(&ctx, &mut s2, "contractProxy", "SplitStack", &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(40), PyRep::Bool(false), PyRep::Integer(4)]).await, "SplitFailed");

        // Legitimate split succeeds
        let split = call(
            &ctx,
            &mut s1,
            "contractProxy",
            "SplitStack",
            &[PyRep::Integer(JITA_44), PyRep::Integer(stack_id), PyRep::Integer(40), PyRep::Bool(false), PyRep::Integer(4)],
        )
        .await;
        assert_eq!(split.dict_get("originalQuantity"), Some(&PyRep::Integer(60)));
    }

    // parity: rpc:beyonce.CmdAbandonLoot, rpc:beyonce.CmdTransferCorpAssetOwnership, rpc:beyonce.CmdTurboDock
    #[tokio::test]
    async fn test_rpc_sweep_group2_beyonce_commands() {
        eve_testkit::attest(&[
            "rpc:beyonce.CmdAbandonLoot",
            "rpc:beyonce.CmdTransferCorpAssetOwnership",
            "rpc:beyonce.CmdTurboDock",
        ]);

        let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
        common::pin_clock(&ctx);
        let mut s1 = login_select(&ctx, 8102, "demo", "demo-secret", P1).await;
        let mut s2 = login_select(&ctx, 8122, "other", "other-secret", P2).await;

        let loot_id = 1_920_000_001i64;
        let loot_hangar_id = 1_920_000_005i64;
        let st_id = 1_920_000_002i64;
        let corp_id = s1.corporation_id.unwrap_or(1_000_170);
        let target_player_corp = 98_000_001i64;

        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(loot_id, mk_item(loot_id, 23, P1, JITA_SYS as i64, InventoryFlag::None, 1, true));
            snap.items.insert(loot_hangar_id, mk_item(loot_hangar_id, 23, P1, JITA_44, InventoryFlag::Hangar, 1, true));
            snap.corporations.insert(
                target_player_corp,
                CorporationRecord {
                    corporation_id: target_player_corp,
                    name: "Allied Fleet Corp".into(),
                    ticker: "AFC".into(),
                    ceo_id: P2,
                    tax_rate: 0.05,
                    hq_station_id: Some(JITA_44),
                    ..Default::default()
                },
            );
            snap.structures.insert(st_id, mk_structure(st_id, 35832, corp_id));
        });

        // Exploit regression tests for CmdAbandonLoot:
        // 1. Item in hangar (not in space) must fail
        assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(loot_hangar_id)]).await, "ItemNotInSpace");
        // 2. Item not owned by caller must fail
        assert_eq!(call_err(&ctx, &mut s2, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(loot_id)]).await, "AccessDenied");
        // 3. Legitimate abandonment succeeds and clears owner
        assert_eq!(call(&ctx, &mut s1, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(loot_id)]).await.dict_get("abandoned"), Some(&PyRep::Bool(true)));
        ctx.db.with_snapshot(|snap| assert_eq!(snap.items[&loot_id].owner_id, 0));

        // Exploit regression tests for CmdTransferCorpAssetOwnership:
        // 1. Caller docked in station must fail with NotInSpace (P2 #97)
        s1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
        assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(target_player_corp)]).await, "NotInSpace");

        // Undock into space
        call(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;

        // 2. Unprivileged member (non-director) must fail with CrpAccessDenied
        s1.corp_role = 0;
        assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(target_player_corp)]).await, "CrpAccessDenied");
        // 3. Transfer to NPC corporation (e.g. 1_000_045) must fail with InvalidTargetCorporation
        s1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
        assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(1_000_045)]).await, "InvalidTargetCorporation");
        // 4. Legitimate transfer by Director in space to player corp succeeds
        assert_eq!(
            call(&ctx, &mut s1, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(target_player_corp)]).await.dict_get("ownerID"),
            Some(&PyRep::Integer(target_player_corp))
        );

        // Exploit regression tests for CmdTurboDock:
        // 1. Non-GM pilot must fail
        assert_eq!(call_err(&ctx, &mut s1, "beyonce", "CmdTurboDock", &[PyRep::Integer(JITA_44)]).await, "GmRoleRequired");
        // 2. GM pilot succeeds
        s1.role = eve_db::service_roles::ROLE_GM_DEFAULT;
        assert_eq!(call(&ctx, &mut s1, "beyonce", "CmdTurboDock", &[PyRep::Integer(JITA_44)]).await.dict_get("docked"), Some(&PyRep::Bool(true)));
        assert_eq!(s1.station_id, Some(JITA_44));
        assert_eq!(call(&ctx, &mut s1, "beyonce", "CmdTurboDock", &[PyRep::Integer(st_id)]).await.dict_get("docked"), Some(&PyRep::Bool(true)));
        assert_eq!(s1.structure_id, Some(st_id));
        assert_eq!(s1.station_id, None);
    }

    // parity: rpc:contractProxy.CollectMyPageInfo, rpc:contractProxy.FinishAuction, rpc:contractProxy.GetCourierContractFromItemID
    // parity: rpc:contractProxy.GetItemsInContainer, rpc:contractProxy.GetMyContractEscrow, rpc:contractProxy.GetNumItemsInContainers, rpc:contractProxy.NumOutstandingContracts
    #[tokio::test]
    async fn test_rpc_sweep_group3_contract_proxy_methods() {
        eve_testkit::attest(&[
            "rpc:contractProxy.CollectMyPageInfo",
            "rpc:contractProxy.FinishAuction",
            "rpc:contractProxy.GetCourierContractFromItemID",
            "rpc:contractProxy.GetItemsInContainer",
            "rpc:contractProxy.GetMyContractEscrow",
            "rpc:contractProxy.GetNumItemsInContainers",
            "rpc:contractProxy.NumOutstandingContracts",
        ]);

        let repo = Arc::new(WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_repository(repo);
        common::pin_clock(&ctx);
        let mut s1 = login_select(&ctx, 8103, "demo", "demo-secret", P1).await;

        let cid_del = 1_930_000_001i64;
        let cid_auc = 1_930_000_002i64;
        let cid_unexpired = 1_930_000_006i64;
        let cid_unauth = 1_930_000_007i64;
        let item_auc = 1_930_000_003i64;
        let box_id = 1_930_000_004i64;
        let box_child = 1_930_000_005i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                item_auc,
                InventoryItemRecord {
                    item_id: item_auc,
                    type_id: 34,
                    owner_id: P1,
                    location_id: JITA_44,
                    flag: InventoryFlag::Locked,
                    quantity: 500,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                box_id,
                InventoryItemRecord {
                    item_id: box_id,
                    type_id: 3293,
                    owner_id: P1,
                    location_id: JITA_44,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                box_child,
                InventoryItemRecord {
                    item_id: box_child,
                    type_id: 34,
                    owner_id: P1,
                    location_id: box_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 25,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.contracts.insert(
                cid_del,
                ContractRecord {
                    contract_id: cid_del,
                    issuer_id: P1,
                    assignee_id: 0,
                    acceptor_id: None,
                    contract_type: "Courier".into(),
                    availability: "Public".into(),
                    status: "Outstanding".into(),
                    start_station_id: JITA_44,
                    end_station_id: JITA_44,
                    price: IskCents(0),
                    reward: IskCents::from_f64_isk(50_000.0),
                    collateral: IskCents(0),
                    item_ids: vec![box_id],
                    requested_items: vec![],
                    current_bid: IskCents(0),
                    current_bidder_id: None,
                    title: "Courier".into(),
                    date_issued_unix: 1000,
                    expiration_unix: 2_000_000_000,
                    ..Default::default()
                },
            );
            snap.contracts.insert(
                cid_auc,
                ContractRecord {
                    contract_id: cid_auc,
                    issuer_id: P1,
                    assignee_id: 0,
                    acceptor_id: None,
                    contract_type: "Auction".into(),
                    availability: "Public".into(),
                    status: "Outstanding".into(),
                    start_station_id: JITA_44,
                    end_station_id: JITA_44,
                    price: IskCents::from_f64_isk(100_000.0),
                    reward: IskCents(0),
                    collateral: IskCents(0),
                    item_ids: vec![item_auc],
                    requested_items: vec![],
                    current_bid: IskCents::from_f64_isk(150_000.0),
                    current_bidder_id: Some(P2),
                    title: "Auction".into(),
                    date_issued_unix: 100,
                    expiration_unix: 500,
                    ..Default::default()
                },
            );
            snap.contracts.insert(
                cid_unexpired,
                ContractRecord {
                    contract_id: cid_unexpired,
                    issuer_id: P1,
                    assignee_id: 0,
                    acceptor_id: None,
                    contract_type: "Auction".into(),
                    availability: "Public".into(),
                    status: "Outstanding".into(),
                    start_station_id: JITA_44,
                    end_station_id: JITA_44,
                    price: IskCents::from_f64_isk(100_000.0),
                    reward: IskCents(0),
                    collateral: IskCents(0),
                    item_ids: vec![],
                    requested_items: vec![],
                    current_bid: IskCents(0),
                    current_bidder_id: None,
                    title: "Unexpired Auction".into(),
                    date_issued_unix: 100,
                    expiration_unix: 2_000_000_000,
                    ..Default::default()
                },
            );
            snap.contracts.insert(
                cid_unauth,
                ContractRecord {
                    contract_id: cid_unauth,
                    issuer_id: 999_999,
                    assignee_id: 0,
                    acceptor_id: None,
                    contract_type: "Auction".into(),
                    availability: "Public".into(),
                    status: "Outstanding".into(),
                    start_station_id: JITA_44,
                    end_station_id: JITA_44,
                    price: IskCents::from_f64_isk(100_000.0),
                    reward: IskCents(0),
                    collateral: IskCents(0),
                    item_ids: vec![],
                    requested_items: vec![],
                    current_bid: IskCents(0),
                    current_bidder_id: None,
                    title: "Other Auction".into(),
                    date_issued_unix: 100,
                    expiration_unix: 500,
                    ..Default::default()
                },
            );
        });

        let page = call(&ctx, &mut s1, "contractProxy", "CollectMyPageInfo", &[]).await;
        assert_eq!(page.dict_get("numOutstandingContractsNonCorp"), Some(&PyRep::Integer(3)));
        assert_eq!(call(&ctx, &mut s1, "contractProxy", "NumOutstandingContracts", &[]).await, PyRep::Integer(3));
        let escrow = call(&ctx, &mut s1, "contractProxy", "GetMyContractEscrow", &[]).await;
        assert_eq!(escrow.dict_get("iskEscrow"), Some(&PyRep::Real(50_000.0)));
        assert_eq!(escrow.dict_get("itemsEscrow"), Some(&PyRep::Integer(2)));

        assert_eq!(call(&ctx, &mut s1, "contractProxy", "GetCourierContractFromItemID", &[PyRep::Integer(box_id)]).await.dict_get("contractID"), Some(&PyRep::Integer(cid_del)));
        assert_eq!(call(&ctx, &mut s1, "contractProxy", "GetCourierContractFromItemID", &[PyRep::Integer(item_auc)]).await, PyRep::None);

        let in_box = call(&ctx, &mut s1, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(JITA_44), PyRep::Integer(box_id), PyRep::Bool(false), PyRep::Integer(4)]).await;
        assert!(matches!(in_box, PyRep::List(ref l) if l.len() == 1));
        let counts = call(&ctx, &mut s1, "contractProxy", "GetNumItemsInContainers", &[PyRep::Integer(JITA_44), PyRep::List(vec![PyRep::Integer(box_id)]), PyRep::Bool(false), PyRep::Integer(4)]).await;
        assert!(matches!(counts, PyRep::Dict(ref d) if d.contains(&(PyRep::Integer(box_id), PyRep::Integer(1)))));

        // Exploit regression tests for FinishAuction:
        // 1. Finishing unexpired auction must fail with AuctionNotExpired
        assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_unexpired), PyRep::Bool(true)]).await, "AuctionNotExpired");
        // 2. Finishing non-auction contract (e.g. Courier) must fail with InvalidContractType
        assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_del), PyRep::Bool(true)]).await, "InvalidContractType");
        // 3. Unauthorized caller (P1 who is neither issuer nor bidder on cid_unauth) must fail with ContractOwnershipViolation
        assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_unauth), PyRep::Bool(true)]).await, "ContractOwnershipViolation");
        // 4. Legitimate finish by issuer or winner after expiration succeeds
        assert_eq!(call(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_auc), PyRep::Bool(true)]).await, PyRep::Bool(true));
        // 5. Finishing already finished auction fails with InvalidContractState
        assert_eq!(call_err(&ctx, &mut s1, "contractProxy", "FinishAuction", &[PyRep::Integer(cid_auc), PyRep::Bool(true)]).await, "InvalidContractState");
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.contracts[&cid_auc].status, "Finished");
            assert_eq!(snap.items[&item_auc].owner_id, P2);
        });
    }
}

pub(crate) mod rpc_solid_charmgr_settings_persistence {
    // Integration tests for verified solid RPC parity rows:
    // `charMgr.SaveCharacterSetting` and `charMgr.DeleteCharacterSetting`.


    use crate::common;
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    async fn setup_pilot(ctx: &ServerContext, sid: u64, char_id: i64) -> SessionState {
        let kw = HashMap::new();
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID");
        sess
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
            .await
    }

    #[tokio::test]
    // parity: feat:ui.server-settings-storage
    // parity: rpc:charMgr.DeleteCharacterSetting
    // parity: rpc:charMgr.SaveCharacterSetting
    async fn test_rpc_solid_charmgr_settings_persistence() {
        eve_testkit::attest(&[
            "feat:ui.server-settings-storage",
            "rpc:charMgr.DeleteCharacterSetting",
            "rpc:charMgr.SaveCharacterSetting",
        ]);

        let ctx = ServerContext::new();
        let char_id = 90_000_001i64;
        let mut sess = setup_pilot(&ctx, 41_101, char_id).await;

        // 1. Initial settings empty
        let init_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
            .await
            .expect("GetCharacterSettings initial");
        assert_eq!(init_settings, PyRep::Dict(vec![]));

        // 2. Save settings
        let saved1 = rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::String("overview_tab".into()), PyRep::Integer(3)],
        )
        .await
        .expect("SaveCharacterSetting 1");
        assert_eq!(saved1, PyRep::Bool(true));

        let saved2 = rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::String("audio_mute".into()), PyRep::Bool(false)],
        )
        .await
        .expect("SaveCharacterSetting 2");
        assert_eq!(saved2, PyRep::Bool(true));

        // 3. Verify retrieved settings contain both
        let cur_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
            .await
            .expect("GetCharacterSettings after save");
        assert_eq!(cur_settings.dict_get("overview_tab"), Some(&PyRep::Integer(3)));
        assert_eq!(cur_settings.dict_get("audio_mute"), Some(&PyRep::Bool(false)));

        // 4. Persistence round-trip across new context state load
        let loaded = eve_server::services::cluster_macho_misc_services::ClusterMiscRuntimeState::from_db(&ctx.db);
        assert_eq!(
            loaded.char_settings.get(&char_id).and_then(|m| m.get("overview_tab")),
            Some(&PyRep::Integer(3))
        );

        // 5. Delete one setting
        let deleted = rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "DeleteCharacterSetting",
            &[PyRep::String("overview_tab".into())],
        )
        .await
        .expect("DeleteCharacterSetting");
        assert_eq!(deleted, PyRep::Bool(true));

        // 6. Verify setting deleted and other setting preserved
        let updated_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", &[])
            .await
            .expect("GetCharacterSettings after delete");
        assert_eq!(updated_settings.dict_get("overview_tab"), None);
        assert_eq!(updated_settings.dict_get("audio_mute"), Some(&PyRep::Bool(false)));

        // 7. Error validation paths
        let err_empty_save = rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::String("".into()), PyRep::Integer(1)],
        )
        .await
        .expect_err("Empty key save should fail");
        assert_eq!(err_empty_save.code, "CustomError");

        let err_empty_del = rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "DeleteCharacterSetting",
            &[PyRep::String("".into())],
        )
        .await
        .expect_err("Empty key delete should fail");
        assert_eq!(err_empty_del.code, "CustomError");

        let long_key = "k".repeat(200);
        let err_long_key = rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::String(long_key), PyRep::Integer(1)],
        )
        .await
        .expect_err("Overly long key should fail");
        assert_eq!(err_long_key.code, "CustomError");

        let huge_val = "v".repeat(5_000);
        let err_huge_val = rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::String("valid_key".into()), PyRep::String(huge_val)],
        )
        .await
        .expect_err("Overly large value should fail");
        assert_eq!(err_huge_val.code, "CustomError");

        // 8. Unauthenticated call rejects cleanly
        let mut unauth_sess = SessionState::new(41_102);
        let err_unauth = rpc(
            &ctx,
            &mut unauth_sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::String("k".into()), PyRep::Integer(1)],
        )
        .await
        .expect_err("Unauthenticated session should fail");
        assert_eq!(err_unauth.code, "Unauthorized");

        // 9. Verify settings restored upon new session login
        let mut sess2 = setup_pilot(&ctx, 41_103, char_id).await;
        let login_settings = rpc(&ctx, &mut sess2, "charMgr", "GetCharacterSettings", &[])
            .await
            .expect("GetCharacterSettings restored on login");
        assert_eq!(login_settings.dict_get("audio_mute"), Some(&PyRep::Bool(false)));
        assert_eq!(login_settings.dict_get("overview_tab"), None);
    }
}
