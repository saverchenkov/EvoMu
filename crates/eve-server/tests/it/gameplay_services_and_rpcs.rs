#![forbid(unsafe_code)]


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_core_services_ext_a {
    use super::*;

// Parity test suite for core RPC services batch (Part 1: `lookupSvc` and `charUnboundMgr`).


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

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_core_services_ext_b {
    use super::*;

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


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_core_services_ext_c {
    use super::*;

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
                start_unix: now, ..Default::default() },
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
                start_unix: now.saturating_sub(100_000), ..Default::default() },
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


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_utility_services_ext {
    use super::*;

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
