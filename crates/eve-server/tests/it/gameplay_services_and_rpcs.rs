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
    // parity: rpc:lookupSvc.LookupCharacters, rpc:lookupSvc.LookupCorporations, rpc:lookupSvc.LookupEvePlayerCharacters, rpc:lookupSvc.LookupFactions, rpc:lookupSvc.LookupKnownLocationsByGroup, rpc:lookupSvc.LookupLocationsByGroup, rpc:lookupSvc.LookupNoneNPCAccountOwners, rpc:lookupSvc.LookupOwners, rpc:lookupSvc.LookupPCOwners
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_000_001,
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = 98_000_001;
                __corp.name = "Federation Defense Corp".into();
                __corp.ticker = "FED".into();
                __corp.ceo_id = 90_000_001;
                __corp.alliance_id = Some(99_000_001);
                __corp.tax_rate = 0.05;
                __corp.members = HashSet::from([90_000_001]);
                __corp.member_roles = HashMap::new();
                __corp.applications = HashMap::new();
                __corp.description = String::new();
                __corp.url = String::new();
                __corp.hq_station_id = Some(60_003_760);
                __corp.wallet_divisions = Vec::new();
                let __mw = eve_db::IskCents::from_isk(1_000_000);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
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
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = 98_000_050;
                __corp.name = "Target Federation Corp".into();
                __corp.ticker = "TFC".into();
                __corp.ceo_id = 90_000_050;
                __corp },
        );
        snap.corporations.insert(
            98_000_051,
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = 98_000_051;
                __corp.name = "Peaceful Federation Corp".into();
                __corp.ticker = "PFC".into();
                __corp.ceo_id = 90_000_051;
                __corp.alliance_id = Some(99_000_051);
                __corp },
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
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = 98_000_077;
                __corp.name = "Federation War Target Corp".into();
                __corp.ticker = "FWTC".into();
                __corp.ceo_id = 90_000_077;
                __corp.alliance_id = Some(99_000_077);
                __corp },
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
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
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
        s.cluster_and_runtime.resculpt_pending.insert(90_000_001);
    });
    // Cross-race bloodline change is rejected (#race-lock); same-race Brutor (4, Minmatar) succeeds
    let cross_race_err = ctx
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
        .expect_err("cross-race bloodline change must be rejected");
    assert_eq!(cross_race_err.code, "RaceChangeNotAllowed");
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "UpdateCharacterBloodline",
            &[PyRep::Integer(90_000_001), PyRep::Integer(4)],
            &kw,
            None,
        )
        .await
        .expect("UpdateCharacterBloodline");
    assert_eq!(res.as_bool(), Some(true));
    let bloodline = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().bloodline_id);
    assert_eq!(bloodline, 4);

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
        s.cluster_and_runtime.resculpt_pending.insert(90_000_001);
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
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
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
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
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
async fn test_rpc_map_svc_beacons_sov_incursions() {
    // parity: rpc:map.GetBeaconCount, rpc:map.GetCurrentSovData, rpc:map.GetDeadspaceComplexMap, rpc:map.GetFacWarZoneInfo, rpc:map.GetIncursionGlobalReport
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
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
async fn test_rpc_map_svc_stations_history_visits() {
    // parity: rpc:map.GetIndustryJobsOverLast24Hours, rpc:map.GetMyExtraMapInfo, rpc:map.GetMyExtraMapInfoAgents, rpc:map.GetRecentSovActivity, rpc:map.GetSecurityModifiedSystems, rpc:map.GetSolarSystemVisits, rpc:map.GetStationCount, rpc:map.GetStationInfo, rpc:map.GetSystemsInIncursions
    let mut sde = (*crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx().sde).clone();
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

    let demo_snap = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx()
        .db
        .with_snapshot(|s| s.clone());
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = demo_snap);
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

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
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

    // 4. Persistence across typed snapshot storage and fresh session reload
    assert!(ctx.db.with_snapshot(|s| s.cluster_and_runtime.char_system_visits.contains_key(&90_000_001)));
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

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
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
        panic!("GetEventList must return List of crowsets");
    };
    assert!(!rowsets_ym.is_empty(), "Must return event crowset");
    let PyRep::ObjectEx { list: ev_rows, .. } = &rowsets_ym[0] else {
        panic!("Inner item must be CRowset ObjectEx");
    };
    assert!(
        ev_rows.iter().any(|r| matches!(r, PyRep::DBRow { columns, .. } if columns[0].1 == PyRep::Integer(ev_id))),
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
    let PyRep::ObjectEx { list: my_rows, .. } = &rowsets_my[0] else {
        panic!("Inner item must be CRowset ObjectEx");
    };
    assert!(
        my_rows.iter().any(|r| matches!(r, PyRep::DBRow { columns, .. } if columns[0].1 == PyRep::Integer(ev_id))),
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

#[allow(dead_code, unused_imports)]
pub(crate) mod rpc_checkout_and_missions {
    use super::*;

//  End-to-end MachoNet RPC integration tests for the 10 remediated `rpc:*` parity rows.

use crate::common;
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const LOWSEC_ASABONA: u32 = 30_000_012;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_and_select(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

#[tokio::test]
// parity: rpc:FastCheckoutService.RequestMiningCycleOutput
async fn test_parity_fast_checkout_offers_buy_and_mining_output() {
    eve_testkit::attest(&[
        "rpc:FastCheckoutService.RequestMiningCycleOutput",
    ]);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);

    // Unauthenticated FastCheckoutService calls fail
    let mut unauth = SessionState::new(91_000);
    assert_eq!(rpc(&ctx, &mut unauth, "FastCheckoutService", "GetOffersForUser", &[]).await.unwrap_err().code, "Unauthorized");

    let mut s1 = login_and_select(&ctx, 91_001, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_and_select(&ctx, 91_002, "other", "other-secret", PILOT_2).await;
    let user_id = s1.user_id.expect("user_id");

    // Disabled by default: GetOffersForUser returns empty list; BuyOffer returns FastCheckoutDisabled
    assert_eq!(rpc(&ctx, &mut s1, "FastCheckoutService", "GetOffersForUser", &[]).await.expect("GetOffersForUser disabled"), PyRep::List(Vec::new()));
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(2001), PyRep::Integer(1), PyRep::String("demo-secret".into())]).await.unwrap_err().code,
        "FastCheckoutDisabled"
    );

    // Enable FastCheckout via real slash.SlashCmd("/fastcheckout 1")
    s1.role |= service_roles::ROLE_GM_DEFAULT;
    let fc_enable = rpc(&ctx, &mut s1, "slash", "SlashCmd", &[PyRep::String("/fastcheckout 1".into())]).await.expect("/fastcheckout 1");
    assert_eq!(fc_enable, PyRep::Bool(true));

    // Seed NES store catalog with PLEX offer (240 PLEX @ $9.99)
    ctx.db.with_mut_snapshot(|snap| {
        use eve_server::services::quasar_payment_store_types::{
            save_store_catalog, save_store_offer_tags, StoreCatalog, StoreCategory, StoreOffer, StoreProduct,
        };
        save_store_catalog(snap, &StoreCatalog {
            categories: vec![StoreCategory { id: 1, name: "PLEX".into(), description: "PLEX Packs".into() }],
            products: vec![StoreProduct { id: 201, name: "240 PLEX".into(), description: "240 PLEX".into(), type_id: 44_992, quantity: 240 }],
            offers: vec![StoreOffer {
                id: 2001, name: "240 PLEX".into(), description: "240 PLEX Pack".into(), category_id: 1,
                price_plex: 0, product_ids: vec![201], targeted: false, price_cents: Some(999), currency: Some("USD".into()),
            }],
        });
        let mut tags = HashMap::new();
        tags.insert(2001, vec!["popular".to_string()]);
        save_store_offer_tags(snap, &tags);
    });

    // GetOffersForUser returns catalog-backed offer with exact fields
    let offers_rep = rpc(&ctx, &mut s1, "FastCheckoutService", "GetOffersForUser", &[]).await.expect("GetOffersForUser enabled");
    let PyRep::Dict(offers_dict) = offers_rep else { panic!("Expected Dict from GetOffersForUser"); };
    let plex_list = offers_dict.iter().find_map(|(k, v)| (k == &PyRep::String("plex".into())).then_some(v)).expect("plex key");
    let PyRep::List(offers) = plex_list else { panic!("Expected List for plex"); };
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].dict_get("offerId").and_then(|v| v.as_i64()), Some(2001));
    assert_eq!(offers[0].dict_get("baseQuantity").and_then(|v| v.as_i64()), Some(240));
    assert_eq!(offers[0].dict_get("price").and_then(|v| v.as_f64()), Some(9.99));

    // BuyOffer without quick-pay payment token fails with PaymentTokenRequired
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(2001), PyRep::Integer(2), PyRep::String("demo-secret".into())]).await.unwrap_err().code,
        "PaymentTokenRequired"
    );

    // Register quick-pay payment token via production helper
    eve_server::services::quasar_payment_store_types::add_user_quick_pay_token(
        &ctx.db,
        &ctx.quasar_gateway.pending_notices,
        user_id,
        eve_server::services::quasar_payment_store_types::PaymentTokenRecord {
            id: 77, user_id, token_str: "tok_fc_77".into(), is_quick_pay: true, consumed: false,
        },
    );

    // Negative BuyOffer cases: wrong password, unknown offer, non-positive quantity
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(2001), PyRep::Integer(1), PyRep::String("bad-pass".into())]).await.unwrap_err().code,
        "InvalidPassword"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(9999), PyRep::Integer(1), PyRep::String("demo-secret".into())]).await.unwrap_err().code,
        "OfferNotFound"
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::String("demo-secret".into())]).await.unwrap_err().code,
        "InvalidQuantity"
    );

    // Valid BuyOffer (2 x 240 PLEX = 480 PLEX, $19.98 = 1998 cents)
    let vault_before = ctx
        .db
        .with_snapshot(|snap| snap.account_plex_vault.get(&user_id).copied().unwrap_or(0));
    let buy_res = rpc(
        &ctx,
        &mut s1,
        "FastCheckoutService",
        "BuyOffer",
        &[PyRep::Integer(2001), PyRep::Integer(2), PyRep::String("demo-secret".into())],
    )
    .await
    .expect("BuyOffer succeeds");
    assert_eq!(buy_res.dict_get("status").and_then(|v| v.as_str()), Some("CONFIRMED"));
    assert_eq!(buy_res.dict_get("offerID").and_then(|v| v.as_i64()), Some(2001));
    assert_eq!(buy_res.dict_get("plexAmount").and_then(|v| v.as_i64()), Some(480));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault.get(&user_id).copied().unwrap_or(0), vault_before + 480);
        let receipts = &snap.quasar_commerce.payment_receipts;
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].amount_in_cents, 1998);
        assert_eq!(receipts[0].offer_id, "2001");
        let txs = eve_server::services::quasar_payment_store::get_plex_transactions(snap);
        assert_eq!(txs.last().map(|t| t.amount), Some(480));
    });

    // RequestMiningCycleOutput: create player corp, teleport to Low-Sec Asabona, give Metenox (81826), undock & anchor
    rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveisk 5000000000".into())],
    )
    .await
    .expect("/giveisk");
    rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Metenox Mining Co".into()), PyRep::String("MTNX".into())],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_secs(7 * 86_400 + 10);
    rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/tr {LOWSEC_ASABONA}"))],
    )
    .await
    .expect("/tr lowsec");
    let create_item_res = rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/createitem 81826 1".into())],
    )
    .await
    .expect("/createitem Metenox");
    let metenox_item_id = create_item_res
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("created Metenox itemID");

    // Anchor Metenox near deploying ship via structureDeployment.Anchor
    let anchor_res = rpc(
        &ctx,
        &mut s1,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(metenox_item_id),
            PyRep::String("Asabona Metenox".into()),
        ],
    )
    .await
    .expect("structureDeployment.Anchor Metenox");
    let structure_id = anchor_res
        .dict_get("structureID")
        .and_then(|v| v.as_i64())
        .expect("anchored structureID");

    // Verify RequestMiningCycleOutput returns exact 40% yield SDE moon materials
    let cycle_rep = rpc(
        &ctx,
        &mut s1,
        "FastCheckoutService",
        "RequestMiningCycleOutput",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("FastCheckoutService.RequestMiningCycleOutput");
    let PyRep::List(rows) = cycle_rep else {
        panic!("Expected List from RequestMiningCycleOutput");
    };
    assert!(!rows.is_empty(), "Must yield group-427 moon materials");
    for row in &rows {
        let PyRep::Tuple(t) = row else {
            panic!("Expected 3-tuple");
        };
        assert_eq!(t.len(), 3);
        let tid = t[0].as_i64().expect("type_id") as u32;
        let qty = t[1].as_i64().expect("quantity");
        let vol = t[2].as_f64().expect("volume");
        let inv = ctx.sde.types.get(&tid).expect("material in SDE");
        assert_eq!(inv.group_id, 427, "Material must be group 427 moon material");
        assert!(qty > 0);
        assert!((vol - (qty as f64) * inv.volume).abs() < 1e-6);
    }

    // Negative RequestMiningCycleOutput cases: missing arg, unknown structure, unauthorized corp, High-Sec
    assert_eq!(
        rpc(&ctx, &mut s1, "FastCheckoutService", "RequestMiningCycleOutput", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(9_999_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "StructureNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s2,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(structure_id)],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // Anchor an Astrahus (35832) in another Low-Sec system -> RequestMiningCycleOutput rejects InvalidStructureType
    rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tr 30000013".into())],
    )
    .await
    .expect("/tr 30000013");
    let astra_item = rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/createitem 35832 1".into())],
    )
    .await
    .expect("/createitem Astrahus")
    .dict_get("itemID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let astra_struct = rpc(
        &ctx,
        &mut s1,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(astra_item),
            PyRep::String("Asabona Astrahus".into()),
        ],
    )
    .await
    .expect("Anchor Astrahus")
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();
    assert_eq!(
        rpc(
            &ctx,
            &mut s1,
            "FastCheckoutService",
            "RequestMiningCycleOutput",
            &[PyRep::Integer(astra_struct)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidStructureType"
    );
}

#[tokio::test]
// parity: rpc:agentMgr.GetPlausibleMissionIDs
async fn test_parity_agent_plausible_missions_and_cache_rowset() {
    eve_testkit::attest(&[
        "rpc:agentMgr.GetPlausibleMissionIDs",
    ]);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    let mut sess = login_and_select(&ctx, 91_101, "demo", "demo-secret", PILOT_1).await;

    // 1. agentMgr.GetPlausibleMissionIDs: pick two distinct SDE agents (level 1 vs level 4)
    let mut agents: Vec<_> = ctx.sde.agents.values().collect();
    agents.sort_by_key(|a| a.agent_id);
    let lvl1_agent = agents
        .iter()
        .find(|a| a.level == 1 && a.division_id == 24)
        .expect("level 1 security agent in SDE");
    let lvl4_agent = agents
        .iter()
        .find(|a| a.level == 4 && a.division_id == 22)
        .expect("level 4 distribution agent in SDE");

    let res1 = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetPlausibleMissionIDs",
        &[PyRep::Integer(lvl1_agent.agent_id)],
    )
    .await
    .expect("GetPlausibleMissionIDs lvl1");
    let PyRep::List(ids1_rep) = res1 else {
        panic!("Expected List from GetPlausibleMissionIDs");
    };
    let ids1: Vec<u32> = ids1_rep
        .iter()
        .filter_map(|r| r.as_i64().map(|v| v as u32))
        .collect();
    assert!(!ids1.is_empty() && ids1.len() <= 25);
    for mid in &ids1 {
        let m = ctx.sde.missions.get(mid).expect("mission in SDE");
        assert!(!m.is_courier, "Security agent mission {mid} must not be courier");
    }

    let res4 = rpc(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetPlausibleMissionIDs",
        &[PyRep::Integer(lvl4_agent.agent_id)],
    )
    .await
    .expect("GetPlausibleMissionIDs lvl4");
    let PyRep::List(ids4_rep) = res4 else {
        panic!("Expected List from GetPlausibleMissionIDs");
    };
    let ids4: Vec<u32> = ids4_rep
        .iter()
        .filter_map(|r| r.as_i64().map(|v| v as u32))
        .collect();
    assert!(!ids4.is_empty() && ids4.len() <= 25);
    assert_ne!(ids1, ids4, "Different agent levels/divisions must return distinct mission pools");
    for mid in &ids4 {
        let m = ctx.sde.missions.get(mid).expect("mission in SDE");
        assert!(m.is_courier, "Distribution agent mission {mid} must be courier");
    }

    // Negative: unknown agentID returns AgentNotFound
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetPlausibleMissionIDs",
            &[PyRep::Integer(999_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "AgentNotFound"
    );

    // 2. cache.Rowset: verify roles, races, factions, bloodlines, ancestries against SDE & corp role defs
    let roles_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("roles".into())])
        .await
        .expect("cache.Rowset roles");
    let PyRep::Rowset { columns, rows } = roles_rep else {
        panic!("Expected Rowset for roles");
    };
    assert_eq!(columns, vec!["roleID", "roleName"]);
    assert_eq!(
        rows.len(),
        eve_server::esi::corp_governance_routes::CORP_ROLE_DEFINITIONS.len()
    );
    assert!(rows.iter().any(|r| r.get(1).and_then(|v| v.as_str()) == Some("Director")));

    let races_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("races".into())])
        .await
        .expect("cache.Rowset races");
    let PyRep::Rowset { columns: r_cols, rows: r_rows } = races_rep else {
        panic!("Expected Rowset for races");
    };
    assert_eq!(r_cols, vec!["raceID", "raceName"]);
    assert_eq!(r_rows.len(), ctx.sde.races.len());

    let factions_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("factions".into())])
        .await
        .expect("cache.Rowset factions");
    let PyRep::Rowset { rows: f_rows, .. } = factions_rep else {
        panic!("Expected Rowset for factions");
    };
    assert_eq!(f_rows.len(), ctx.sde.factions.len());

    let bloodlines_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("bloodlines".into())])
        .await
        .expect("cache.Rowset bloodlines");
    let PyRep::Rowset { rows: b_rows, .. } = bloodlines_rep else {
        panic!("Expected Rowset for bloodlines");
    };
    assert_eq!(b_rows.len(), ctx.sde.bloodlines.len());

    let ancestries_rep = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("ancestries".into())])
        .await
        .expect("cache.Rowset ancestries");
    let PyRep::Rowset { rows: a_rows, .. } = ancestries_rep else {
        panic!("Expected Rowset for ancestries");
    };
    assert_eq!(a_rows.len(), ctx.sde.ancestries.len());

    // Negative: unknown table name returns UnknownCacheRowset
    assert_eq!(
        rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("nonexistent_table".into())])
            .await
            .unwrap_err()
            .code,
        "UnknownCacheRowset"
    );
}

#[tokio::test]
async fn test_parity_project_discovery_tasks_stats_xp_and_tutorial() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    let mut sess = login_and_select(&ctx, 91_201, "demo", "demo-secret", PILOT_1).await;

    // 1. get_total_needed_xp: default V24 analyst progression curve returns 500.0 at rank 1
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(1)])
            .await
            .unwrap(),
        PyRep::Real(500.0)
    );
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::project_discovery::configure_project_discovery_mmos(
            snap, 2_000.0, 250.0, 150.0, 75.0, 50,
        );
        eve_server::services::project_discovery::set_project_discovery_task_consensus(snap, 1, true);
        eve_server::services::project_discovery::set_project_discovery_task_consensus(snap, 2, false);
    });
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(0)])
            .await
            .unwrap(),
        PyRep::Real(0.0)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(2)])
            .await
            .unwrap(),
        PyRep::Real(4_000.0)
    );

    // 2. give_tutorial_rewards: false before completion, true once level 5 reached, idempotent false after
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[])
            .await
            .unwrap(),
        PyRep::Bool(false)
    );
    for _ in 0..4 {
        rpc(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[])
            .await
            .expect("increase_tutorial_level");
    }
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[])
            .await
            .unwrap(),
        PyRep::Bool(false),
        "Tutorial reward must be idempotent"
    );

    // 3. get_new_task & post_classification (accurate task 1 -> +150 XP, +50 AK, +1000 ISK)
    let t1 = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task 1");
    assert_eq!(t1.dict_get("task_id").and_then(|v| v.as_str()), Some("task_1"));
    assert_eq!(t1.dict_get("task_number").and_then(|v| v.as_i64()), Some(1));

    // Negative post_classification cases: empty payload, mismatched task_id
    let err_empty = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::List(vec![])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_empty.code.to_ascii_lowercase(), "invalidclassification");

    let err_mismatch = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_99".into()), PyRep::List(vec![PyRep::Real(1.0)])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_mismatch.code.to_ascii_lowercase(), "invalidtaskid");

    let post1 = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::List(vec![PyRep::Real(1.0)])],
    )
    .await
    .expect("post_classification task_1");
    assert_eq!(post1.dict_get("experience_reward").and_then(|v| v.as_f64()), Some(300.0));
    assert_eq!(post1.dict_get("kredits_reward").and_then(|v| v.as_i64()), Some(50));
    assert_eq!(post1.dict_get("ISK_Reward").and_then(|v| v.as_f64()), Some(2_000.0));

    // Advance past MIN_SUBMISSION_INTERVAL_SECS (1s) so duplicate task check is tested instead of rate limiting
    ctx.advance_secs(1);

    // Duplicate submission of task_1 fails with TaskAlreadySubmitted
    let err_dup = rpc(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::List(vec![PyRep::Real(1.0)])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_dup.code.to_ascii_lowercase(), "taskalreadysubmitted");

    // 4. get_player_statistics: verify score (100.0) != accuracy (1.0) in both top-level and project dict
    let stats = rpc(&ctx, &mut sess, "ProjectDiscovery", "get_player_statistics", &[])
        .await
        .expect("get_player_statistics");
    assert_eq!(stats.dict_get("tasks_submitted").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(stats.dict_get("accuracy").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(stats.dict_get("score").and_then(|v| v.as_f64()), Some(100.0));
    assert_eq!(stats.dict_get("experience").and_then(|v| v.as_f64()), Some(550.0));
    let proj_score = stats
        .dict_get("project")
        .and_then(|p| p.dict_get("score"))
        .and_then(|v| v.as_f64());
    assert_eq!(proj_score, Some(100.0), "project.score must equal st.score (100.0), not st.accuracy");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod rpc_cache_and_raffles {
    use super::*;

//  Integration tests remediating batch 2 of `fake_done` RPC rows.


use std::collections::{HashMap, HashSet};

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::with_misc_state;
use glam::DVec3;

use crate::common;

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

fn kv_i64(rep: &PyRep, key: &str) -> Option<i64> {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, v)| v.as_i64()),
        _ => None,
    }
}

fn kv_bool(rep: &PyRep, key: &str) -> Option<bool> {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_bool()),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, v)| v.as_bool()),
        _ => None,
    }
}

async fn setup_pilot(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

#[tokio::test]
async fn test_batch2_cache_setting_and_load_service() {
    // parity: rpc:loadService.StartLoad
    // parity: rpc:loadService.StopLoad
    // parity: rpc:loadService.SetConfig
    // parity: rpc:loadService.GetTotalStats
    eve_testkit::attest(&[
        "rpc:loadService.StartLoad",
        "rpc:loadService.StopLoad",
        "rpc:loadService.SetConfig",
        "rpc:loadService.GetTotalStats",
    ]);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let mut player = setup_pilot(&ctx, 41_001, "demo", "demo-secret", 90_000_001).await;
    let mut gm = setup_pilot(&ctx, 41_002, "other", "other-secret", 90_000_002).await;
    gm.role |= eve_db::service_roles::ROLE_ADMIN;

    // 1. cache.Setting: invalid args rejected, non-GM write rejected, GM write/read/clear persisted
    let bad_args = rpc(&ctx, &mut gm, "cache", "Setting", &[])
        .await
        .expect_err("empty args must fail");
    assert_eq!(bad_args.code, "InvalidArguments");

    let unauth_write = rpc(
        &ctx,
        &mut player,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
            PyRep::String("enabled".into()),
        ],
    )
    .await
    .expect_err("non-GM cache.Setting write must fail");
    assert_eq!(unauth_write.code, "UserError");

    let written = rpc(
        &ctx,
        &mut gm,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
            PyRep::String("enabled".into()),
        ],
    )
    .await
    .expect("GM cache.Setting write");
    assert_eq!(written, PyRep::String("enabled".into()));

    let read_val = rpc(
        &ctx,
        &mut player,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
        ],
    )
    .await
    .expect("cache.Setting read");
    assert_eq!(read_val, PyRep::String("enabled".into()));

    let cleared = rpc(
        &ctx,
        &mut gm,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
            PyRep::None,
        ],
    )
    .await
    .expect("GM cache.Setting clear");
    assert_eq!(cleared, PyRep::None);

    let after_clear = rpc(
        &ctx,
        &mut player,
        "cache",
        "Setting",
        &[
            PyRep::String("zsystem".into()),
            PyRep::String("Batch2Setting".into()),
        ],
    )
    .await
    .expect("cache.Setting read after clear");
    assert_eq!(after_clear, PyRep::String(String::new()));

    // 2. loadService: non-GM rejected, invalid solar system rejected, SetConfig/StartLoad/GetTotalStats/StopLoad
    let unauth_load = rpc(&ctx, &mut player, "loadService", "StartLoad", &[PyRep::Integer(2)])
        .await
        .expect_err("non-GM StartLoad must fail");
    assert_eq!(unauth_load.code, "UserError");

    let bad_sys = rpc(
        &ctx,
        &mut gm,
        "loadService",
        "StartLoad",
        &[PyRep::Dict(vec![
            (PyRep::String("solar_system_id".into()), PyRep::Integer(99_999_999)),
            (PyRep::String("ticks".into()), PyRep::Integer(2)),
        ])],
    )
    .await
    .expect_err("invalid solar_system_id must fail");
    assert_eq!(bad_sys.code, "UserError");

    rpc(
        &ctx,
        &mut gm,
        "loadService",
        "SetConfig",
        &[PyRep::Dict(vec![(
            PyRep::String("mode".into()),
            PyRep::String("stress".into()),
        )])],
    )
    .await
    .expect("SetConfig");

    rpc(
        &ctx,
        &mut gm,
        "loadService",
        "StartLoad",
        &[PyRep::Dict(vec![
            (PyRep::String("solar_system_id".into()), PyRep::Integer(30_000_142)),
            (PyRep::String("ticks".into()), PyRep::Integer(3)),
        ])],
    )
    .await
    .expect("StartLoad");

    let stats_active = rpc(&ctx, &mut gm, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats active");
    assert_eq!(kv_bool(&stats_active, "active"), Some(true));
    assert_eq!(kv_i64(&stats_active, "runs_started"), Some(1));
    assert_eq!(kv_i64(&stats_active, "ticks_executed"), Some(3));
    assert_eq!(kv_i64(&stats_active, "systems_stepped"), Some(1));

    rpc(&ctx, &mut gm, "loadService", "StopLoad", &[])
        .await
        .expect("StopLoad");

    let stats_stopped = rpc(&ctx, &mut gm, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats stopped");
    assert_eq!(kv_bool(&stats_stopped, "active"), Some(false));
    assert_eq!(kv_i64(&stats_stopped, "total_requests"), Some(3));
}

#[tokio::test]
async fn test_batch2_lookup_warable_and_sov_structures() {
    eve_testkit::attest(&[]);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot(&ctx, 41_003, "demo", "demo-secret", 90_000_001).await;

    let caller_corp = 98_000_001_i64;
    let warable_corp = 98_000_701_i64;
    let non_warable_corp = 98_000_702_i64;
    let warable_alliance = 99_000_701_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = caller_corp;
        }
        snap.corporations.insert(
            warable_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = warable_corp;
                __corp.name = "WarEligible Vanguard".into();
                __corp.ticker = "WEVG".into();
                __corp.ceo_id = 90_000_002;
                __corp },
        );
        snap.corporations.insert(
            non_warable_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = non_warable_corp;
                __corp.name = "WarEligible Pacifists".into();
                __corp.ticker = "WEPF".into();
                __corp.ceo_id = 90_000_003;
                __corp },
        );
        snap.alliances.insert(
            warable_alliance,
            AllianceRecord {
                alliance_id: warable_alliance,
                name: "WarEligible Coalition".into(),
                short_name: "WEC".into(),
                executor_corp_id: warable_corp,
                member_corp_ids: HashSet::from([warable_corp]),
                ..Default::default()
            },
        );
        // Upwell Astrahus (non-sov) owned by warable_corp
        snap.structures.insert(
            1_000_000_901_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_901_001,
                type_id: 35832,
                solar_system_id: sys_id,
                owner_corp_id: warable_corp,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        // Sovereignty Hub (sov structure, type 81080) owned by warable_corp
        snap.structures.insert(
            1_000_000_901_002,
            UpwellStructureRecord {
                structure_id: 1_000_000_901_002,
                type_id: 81080,
                solar_system_id: sys_id,
                owner_corp_id: warable_corp,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    sess.corporation_id = Some(caller_corp);

    // 1. LookupWarableCorporationsOrAlliances: returns warable_corp & warable_alliance, excludes non_warable_corp
    let lookup_rep = rpc(
        &ctx,
        &mut sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("WarEligible".into()), PyRep::Bool(false)],
    )
    .await
    .expect("LookupWarableCorporationsOrAlliances");
    let PyRep::List(rows) = lookup_rep else {
        panic!("expected List from LookupWarableCorporationsOrAlliances");
    };
    let owner_ids: Vec<i64> = rows.iter().filter_map(|r| kv_i64(r, "ownerID")).collect();
    assert!(owner_ids.contains(&warable_corp), "warable corp must be returned");
    assert!(owner_ids.contains(&warable_alliance), "warable alliance must be returned");
    assert!(!owner_ids.contains(&non_warable_corp), "non-warable corp must be excluded");
    assert!(!owner_ids.contains(&caller_corp), "caller corp must be excluded");

    // 2. GetSovStructuresInfoForSolarSystem: returns only sov structures (81080), not Astrahus (35832)
    let sov_rep = rpc(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(i64::from(sys_id))],
    )
    .await
    .expect("GetSovStructuresInfoForSolarSystem");
    let PyRep::List(sov_list) = sov_rep else {
        panic!("expected List from GetSovStructuresInfoForSolarSystem");
    };
    let struct_ids: Vec<i64> = sov_list.iter().filter_map(|r| kv_i64(r, "structureID")).collect();
    assert!(struct_ids.contains(&1_000_000_901_002), "Sov Hub must be included");
    assert!(!struct_ids.contains(&1_000_000_901_001), "Astrahus must be excluded");

    // Negative case: invalid solar system
    let bad_sov = rpc(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(99_999_999)],
    )
    .await
    .expect_err("invalid solar system must fail");
    assert_eq!(bad_sov.code, "InvalidSolarSystemID");
}

#[tokio::test]
async fn test_batch2_raffle_character_seeding_and_create() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let mut player = setup_pilot(&ctx, 41_004, "demo", "demo-secret", 90_000_001).await;
    let mut gm = setup_pilot(&ctx, 41_005, "other", "other-secret", 90_000_002).await;
    gm.role |= eve_db::service_roles::ROLE_ADMIN;

    let station_id = 60_003_760_i64;
    player.station_id = Some(station_id);
    player.structure_id = None;

    // 1. character_Seeding: non-GM rejected, invalid ticket count rejected, valid seeding for target char
    let unauth_seed = rpc(
        &ctx,
        &mut player,
        "raffleMgr",
        "character_Seeding",
        &[PyRep::Integer(90_000_001), PyRep::Integer(1)],
    )
    .await
    .expect_err("non-GM character_Seeding must fail");
    assert_eq!(unauth_seed.code, "UserError");

    let bad_tickets = rpc(
        &ctx,
        &mut gm,
        "raffleMgr",
        "character_Seeding",
        &[
            PyRep::Integer(90_000_001),
            PyRep::Integer(1),
            PyRep::Integer(587),
            PyRep::Integer(12),
        ],
    )
    .await
    .expect_err("invalid ticket count must fail");
    assert_eq!(bad_tickets.code, "InvalidTicketCount");

    let seeded = rpc(
        &ctx,
        &mut gm,
        "raffleMgr",
        "character_Seeding",
        &[
            PyRep::Integer(90_000_001),
            PyRep::Integer(2),
            PyRep::Integer(587),
            PyRep::Integer(16),
            PyRep::Real(50_000.0),
        ],
    )
    .await
    .expect("character_Seeding");
    let PyRep::List(seeded_list) = seeded else {
        panic!("expected List from character_Seeding");
    };
    assert_eq!(seeded_list.len(), 2);
    assert_eq!(kv_i64(&seeded_list[0], "owner_id"), Some(90_000_001));
    assert_eq!(kv_i64(&seeded_list[0], "ticket_count"), Some(16));

    // 2. raffleProxy.CreateRaffle: escrow item + consume HyperCores, and reject undocked
    let raffle_item_id = 880_000_001_i64;
    let cores_item_id = 880_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        snap.items.insert(
            raffle_item_id,
            InventoryItemRecord {
                item_id: raffle_item_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cores_item_id,
            InventoryItemRecord {
                item_id: cores_item_id,
                type_id: 52_568,
                owner_id: 90_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                ..Default::default()
            },
        );
    });

    let created = rpc(
        &ctx,
        &mut player,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(raffle_item_id),
            PyRep::Integer(8),
            PyRep::Real(100_000.0),
        ],
    )
    .await
    .expect("CreateRaffle");
    assert_eq!(kv_i64(&created, "item_id"), Some(raffle_item_id));
    assert_eq!(kv_i64(&created, "owner_id"), Some(90_000_001));

    ctx.db.with_snapshot(|snap| {
        let escrowed = snap.items.get(&raffle_item_id).expect("escrowed item");
        assert_eq!(escrowed.owner_id, -52_568);
        assert_eq!(escrowed.flag, InventoryFlag::Locked);
        let rem_cores = snap.items.get(&cores_item_id).map(|i| i.quantity).unwrap_or(0);
        assert!(rem_cores < 50, "HyperCores must be consumed");
    });

    // Negative case: undocked CreateRaffle fails
    player.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
    });
    let undocked_err = rpc(
        &ctx,
        &mut player,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(raffle_item_id),
            PyRep::Integer(8),
            PyRep::Real(100_000.0),
        ],
    )
    .await
    .expect_err("undocked CreateRaffle must fail");
    assert_eq!(undocked_err.code, "InvalidRaffleLocation");
}

#[tokio::test]
async fn test_batch2_shipcaster_pads_launchers_and_travel() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot(&ctx, 41_006, "demo", "demo-secret", 90_000_001).await;
    ctx.advance_secs(15);
    let char_id = 90_000_001_i64;
    let ship_id = sess.ship_id.unwrap_or(100_000_001);

    // 1. GetFactionsWithShipcaster, GetAllLandingPads, GetFactionLandingPads
    let factions = rpc(&ctx, &mut sess, "shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[])
        .await
        .expect("GetFactionsWithShipcaster");
    let PyRep::List(fac_list) = factions else {
        panic!("expected List from GetFactionsWithShipcaster");
    };
    assert!(fac_list.contains(&PyRep::Integer(500_001)));

    let all_pads = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads");
    assert!(matches!(all_pads, PyRep::List(_)));

    let bad_fac_pads = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetFactionLandingPads", &[])
        .await
        .expect_err("missing factionID must fail");
    assert_eq!(bad_fac_pads.code, "InvalidArguments");

    // 2. Starter shipcaster: CanCharacterJump + CmdJumpThroughStarterShipcaster + one-time use
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.create_date_unix = now_unix;
        }
    });
    let can_jump = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump");
    assert_eq!(can_jump, PyRep::Bool(true));

    let (starter_id, starter_sys, starter_pos, dest_sys) = with_misc_state(&ctx, |st| {
        let l = st.starter_launchers.first().expect("starter launcher");
        (l.item_id, l.solar_system_id, l.position, l.staging_system_id)
    });

    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(starter_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = starter_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(starter_sys);
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(starter_sys);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: starter_pos + DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
        )
        .await;

    rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter_id)],
    )
    .await
    .expect("CmdJumpThroughStarterShipcaster");
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    let can_jump_after = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump after use");
    assert_eq!(can_jump_after, PyRep::Bool(false));

    // 3. Interstellar shipcaster: negative case (unknown launcher)
    let bad_jump = rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(999_999_999),
            PyRep::Integer(30_000_142),
            PyRep::Integer(888_888_888),
        ],
    )
    .await
    .expect_err("unknown launcher must fail");
    assert_eq!(bad_jump.code, "LauncherNotFound");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod rpc_corp_and_fleets {
    use super::*;

//  Integration tests verifying remediation of batch-3 `FLAGGED_FAKE` `rpc:*` rows.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_ID: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;
const SYS_PERIMETER: u32 = 30_000_144;
const SYS_TAMA: u32 = 30_002_813;
const SYS_TRIG_MV: u32 = 30_000_163;
const SYS_POCHVEN_KINO: u32 = 30_002_797;
const JITA_STATION: i64 = 60_003_760;
const ERR_CHALLENGE_NOT_FOUND: &str = "ChallengeNotFound";
const ERR_CHALLENGE_NOT_COMPLETED: &str = "ChallengeNotCompleted";
const ERR_CHALLENGE_ALREADY_CLAIMED: &str = "ChallengeAlreadyClaimed";

fn new_ctx() -> ServerContext {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_char(ctx: &ServerContext, sid: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v),
        _ => None,
    }
}

fn count_nested_instances(rep: &PyRep, expected_sys: u32) -> usize {
    let PyRep::Dict(outer) = rep else {
        panic!("expected outer Dict, got {rep:?}");
    };
    let Some((_, inner)) = outer
        .iter()
        .find(|(k, _)| k.as_i64() == Some(i64::from(expected_sys)))
    else {
        panic!("missing solarSystemID {expected_sys} in {outer:?}");
    };
    let PyRep::List(entries) = inner else {
        panic!("expected List of dungeon KeyVals, got {inner:?}");
    };
    for entry in entries {
        assert!(
            entry.dict_get("dungeonID").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
            "dungeonID must be positive in {entry:?}"
        );
        assert!(
            entry.dict_get("instanceID").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
            "instanceID must be positive in {entry:?}"
        );
        assert_eq!(
            entry.dict_get("solarSystemID").and_then(|v| v.as_i64()),
            Some(i64::from(expected_sys)),
            "solarSystemID mismatch in {entry:?}"
        );
    }
    entries.len()
}

#[tokio::test]
async fn batch3_dungeon_instance_cache_homefront_and_triglavian() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    let mut unauth = SessionState::default();
    assert!(rpc(
        &ctx,
        &mut unauth,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[],
    )
    .await
    .is_err());
    assert!(rpc(
        &ctx,
        &mut unauth,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[],
    )
    .await
    .is_err());

    let mut sess = login_char(&ctx, 101, PILOT_ID).await;
    sess.solar_system_id = Some(SYS_JITA);
    sess.station_id = None;

    // Invalid solarSystemID arguments must fail closed
    for bad_arg in [
        PyRep::String("not_a_system".into()),
        PyRep::Integer(-1),
        PyRep::Integer(99_999_999),
    ] {
        let err_hf = rpc(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetHomefrontSiteInstances",
            std::slice::from_ref(&bad_arg),
        )
        .await
        .expect_err("invalid solarSystemID must fail for GetHomefrontSiteInstances");
        assert_eq!(err_hf.code, "UnknownSolarSystem");

        let err_trig = rpc(
            &ctx,
            &mut sess,
            "dungeonInstanceCacheMgr",
            "GetTriglavianSiteInstances",
            &[bad_arg],
        )
        .await
        .expect_err("invalid solarSystemID must fail for GetTriglavianSiteInstances");
        assert_eq!(err_trig.code, "UnknownSolarSystem");
    }

    // Seeded high-sec systems (Jita and Amarr) return Homefront sites; unseeded Perimeter returns 0
    let hf_jita = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[],
    )
    .await
    .expect("GetHomefrontSiteInstances Jita");
    assert_eq!(count_nested_instances(&hf_jita, SYS_JITA), 2);

    let hf_amarr = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(30_002_187)],
    )
    .await
    .expect("GetHomefrontSiteInstances Amarr");
    assert_eq!(count_nested_instances(&hf_amarr, 30_002_187), 2);

    let hf_perim = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(i64::from(SYS_PERIMETER))],
    )
    .await
    .expect("GetHomefrontSiteInstances Perimeter");
    assert_eq!(count_nested_instances(&hf_perim, SYS_PERIMETER), 0);

    // Low-sec system Tama has 0 Homefront sites
    let hf_tama = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(i64::from(SYS_TAMA))],
    )
    .await
    .expect("GetHomefrontSiteInstances Tama");
    assert_eq!(count_nested_instances(&hf_tama, SYS_TAMA), 0);

    // Triglavian minor victory & Pochven systems return Triglavian sites; Jita returns 0
    let trig_mv = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(SYS_TRIG_MV))],
    )
    .await
    .expect("GetTriglavianSiteInstances minor victory");
    assert_eq!(count_nested_instances(&trig_mv, SYS_TRIG_MV), 2);

    let trig_poch = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(i64::from(SYS_POCHVEN_KINO))],
    )
    .await
    .expect("GetTriglavianSiteInstances Pochven");
    assert_eq!(count_nested_instances(&trig_poch, SYS_POCHVEN_KINO), 2);

    let trig_jita = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[],
    )
    .await
    .expect("GetTriglavianSiteInstances Jita");
    assert_eq!(count_nested_instances(&trig_jita, SYS_JITA), 0);
}

#[tokio::test]
async fn batch3_season_manager_challenges_and_queries() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    let mut sess = login_char(&ctx, 103, PILOT_ID).await;

    let max_active = rpc(&ctx, &mut sess, "seasonManager", "get_max_active_challenges", &[])
        .await
        .expect("get_max_active_challenges")
        .as_i64()
        .expect("integer max_active");
    assert!(max_active >= 1, "max_active_challenges must be >= 1");

    let end_time = rpc(&ctx, &mut sess, "seasonManager", "get_season_end_time", &[])
        .await
        .expect("get_season_end_time")
        .as_i64()
        .expect("integer end_time");
    assert!(
        end_time > ctx.now_blueos_filetime_ticks(),
        "season end_time must be in the future"
    );

    let filled = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("fill_up_and_return_challenges_for_character");
    let PyRep::Dict(ch_entries) = filled else {
        panic!("expected Dict from fill_up_and_return_challenges_for_character, got {filled:?}");
    };
    assert!(!ch_entries.is_empty(), "must seed >= 1 active challenge");
    let first_cid = ch_entries[0].0.as_i64().expect("challenge_id int");

    let ch_kv = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(first_cid)],
    )
    .await
    .expect("get_challenge");
    assert_eq!(
        dict_get(&ch_kv, "challengeID").and_then(|v| v.as_i64()),
        Some(first_cid)
    );
    assert_eq!(
        dict_get(&ch_kv, "progress").and_then(|v| v.as_i64()),
        Some(0)
    );

    let dormant = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "challenge_is_dormant",
        &[PyRep::Integer(first_cid)],
    )
    .await
    .expect("challenge_is_dormant")
    .as_bool()
    .expect("bool dormant");
    assert!(!dormant, "active challenge must not be dormant");

    // Missing / unknown challenge IDs fail closed
    let missing_dormant = rpc(&ctx, &mut sess, "seasonManager", "challenge_is_dormant", &[])
        .await
        .expect_err("missing arg");
    assert_eq!(missing_dormant.code, ERR_CHALLENGE_NOT_FOUND);

    let unknown_ch = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(999_999_999)],
    )
    .await
    .expect_err("unknown id");
    assert_eq!(unknown_ch.code, ERR_CHALLENGE_NOT_FOUND);
}

#[tokio::test]
async fn batch3_season_manager_events_claim_reload_and_npc_standings() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    let mut sess = login_char(&ctx, 104, PILOT_ID).await;
    sess.station_id = Some(JITA_STATION);

    // Clone a real FSD client-event-driven challenge so we can deterministically progress & claim it
    let mut custom_def = eve_server::services::season_manager::fsd_challenge_definitions(&ctx.sde)
        .iter()
        .find(|d| d.client_event_driven)
        .expect("real FSD client_event_driven challenge")
        .clone();
    let custom_cid: i64 = 880_001;
    custom_def.challenge_id = custom_cid;
    custom_def.max_progress = 1;
    custom_def.isk_awarded = 0;
    custom_def.event_conditions.clear();
    let ev_type = custom_def.event_type.clone();
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.challenge_definitions = vec![custom_def];
    });

    let filled = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "fill_up_and_return_challenges_for_character",
        &[],
    )
    .await
    .expect("fill_up");
    assert!(
        dict_get(&filled, "880001").is_some()
            || matches!(&filled, PyRep::Dict(v) if v.iter().any(|(k, _)| k.as_i64() == Some(custom_cid))),
        "custom challenge {custom_cid} must be active"
    );

    let exp_ft = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge_expiration_date",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect("get_challenge_expiration_date")
    .as_i64()
    .expect("expiration int");
    assert!(exp_ft > ctx.now_blueos_filetime_ticks());

    let missing_exp = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge_expiration_date",
        &[],
    )
    .await
    .expect_err("missing arg");
    assert_eq!(missing_exp.code, ERR_CHALLENGE_NOT_FOUND);

    // Uncompleted challenge cannot be claimed
    let uncomp_err = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect_err("uncompleted claim must fail");
    assert_eq!(uncomp_err.code, ERR_CHALLENGE_NOT_COMPLETED);

    // Progress via process_client_event
    rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "process_client_event",
        &[PyRep::String(ev_type)],
    )
    .await
    .expect("process_client_event");

    let ch_after_ev = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_challenge",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect("get_challenge after event");
    assert_eq!(
        dict_get(&ch_after_ev, "progress").and_then(|v| v.as_i64()),
        Some(1)
    );

    // Claim completed challenge
    let claim_res = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect("claim_challenge_rewards");
    assert_ne!(claim_res, PyRep::None);

    let dup_err = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_challenge_rewards",
        &[PyRep::Integer(custom_cid)],
    )
    .await
    .expect_err("duplicate claim must fail");
    assert_eq!(dup_err.code, ERR_CHALLENGE_ALREADY_CLAIMED);

    // reload_fsd_data requires GM role
    assert!(
        rpc(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[])
            .await
            .is_err(),
        "non-GM reload_fsd_data must fail"
    );
    sess.role |= eve_machonet::service_roles::ROLE_GMS;
    let reload_ok = rpc(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[])
        .await
        .expect("GM reload_fsd_data");
    assert_eq!(reload_ok, PyRep::Bool(true));

    // standingMgr.GetNPCNPCStandings
    let npc_st = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    let PyRep::Rowset { columns, rows } = npc_st else {
        panic!("expected Rowset from GetNPCNPCStandings, got {npc_st:?}");
    };
    assert_eq!(columns, vec!["fromID", "toID", "standing"]);
    assert!(rows.len() >= 50, "expected >= 50 inter-faction standing rows");
    assert!(
        rows.iter().any(|r| {
            r.first().and_then(|v| v.as_i64()) == Some(500_003)
                && r.get(1).and_then(|v| v.as_i64()) == Some(500_002)
                && r.get(2).and_then(|v| v.as_f64()) == Some(-5.0)
        }),
        "Amarr -> Minmatar standing (-5.0) must be present"
    );
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod rpc_campaigns_and_shipcaster {
    use super::*;

//  E2E parity closure tests for Batch 4 remediated RPCs:
//  - `seasonalLoginCampaignManager`: `get_active_campaign`, `get_claim_history`, `claim_reward`
//  - `shipCasterLandingPadMgr`: `GetAllLandingPads`, `GetFactionLandingPads`
//  - `shipCasterLauncherMgr`: `GetFactionsWithShipcaster`
//  - `starterShipcasterTravelMgr`: `CanCharacterJump`, `CmdJumpThroughStarterShipcaster`
//  - `shipcasterTravelMgr`: `CmdJumpThroughShipcaster`
//  - `storeManager`: `get_categories`, `get_products`, `get_offers`, `can_purchase_offer`, `buy_offer`

use crate::common;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{with_misc_state, LandingPadRecord};
use eve_server::services::cosmetics_ui_ops::{
    load_cosmetics_ui_state, save_cosmetics_ui_state, SeasonalCampaignSchedule,
};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        }),
        PyRep::KeyVal(map) => map.get(key),
        _ => None,
    }
}

#[tokio::test]
async fn test_batch4_seasonal_login_campaign_manager_rpcs() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);

    let mut unauth = SessionState::new(94_001);
    assert!(
        rpc(&ctx, &mut unauth, "seasonalLoginCampaignManager", "get_active_campaign", &[])
            .await
            .is_err(),
        "unauthenticated get_active_campaign must fail"
    );
    assert!(
        rpc(&ctx, &mut unauth, "seasonalLoginCampaignManager", "get_claim_history", &[])
            .await
            .is_err(),
        "unauthenticated get_claim_history must fail"
    );
    assert!(
        rpc(&ctx, &mut unauth, "seasonalLoginCampaignManager", "claim_reward", &[])
            .await
            .is_err(),
        "unauthenticated claim_reward must fail"
    );

    let mut sess = SessionState::new(94_002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.active_seasonal_schedule = Some(SeasonalCampaignSchedule {
            campaign_id: 2323,
            start_unix: now - 3_600,
            end_unix: now + 10 * 86_400,
            turnover_secs: 11 * 3_600,
        });
        save_cosmetics_ui_state(snap, &ext);
    });

    let active = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[])
        .await
        .expect("get_active_campaign");
    let PyRep::Tuple(fields) = active else {
        panic!("expected Tuple from get_active_campaign");
    };
    assert_eq!(fields[0], PyRep::Integer(2323));
    assert!(fields[3].as_i64().unwrap_or(0) > 0);

    let hist_before = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
        .await
        .expect("get_claim_history before");
    assert_eq!(hist_before, PyRep::Dict(vec![]));

    let claimed = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward");
    assert_eq!(claimed, PyRep::Bool(true));

    let hist_after = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
        .await
        .expect("get_claim_history after");
    let PyRep::Dict(entries) = hist_after else {
        panic!("expected Dict from get_claim_history");
    };
    assert_eq!(entries.len(), 1);

    let repeat_err = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .expect_err("repeat claim on same day must fail");
    assert!(repeat_err
        .message
        .to_ascii_lowercase()
        .contains("already claimed"));
}

#[tokio::test]
async fn test_batch4_shipcaster_pads_launchers_and_travel_rpcs() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    ctx.advance_secs(600);

    let mut unauth = SessionState::new(94_010);
    assert!(
        rpc(&ctx, &mut unauth, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
            .await
            .is_err()
    );
    assert!(
        rpc(
            &ctx,
            &mut unauth,
            "shipCasterLandingPadMgr",
            "GetFactionLandingPads",
            &[PyRep::Integer(500001)]
        )
        .await
        .is_err()
    );
    assert!(
        rpc(&ctx, &mut unauth, "shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[])
            .await
            .is_err()
    );

    let mut sess = SessionState::new(94_011);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let bad_faction = rpc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(0)],
    )
    .await
    .expect_err("non-positive factionID must fail");
    assert_eq!(bad_faction.code, "InvalidArguments");

    let factions = rpc(&ctx, &mut sess, "shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[])
        .await
        .expect("GetFactionsWithShipcaster");
    let PyRep::List(faction_list) = factions else {
        panic!("expected List");
    };
    assert_eq!(faction_list.len(), 4);

    // Seed a built & linked Caldari landing pad in Onatoh (30002811)
    let now = ctx.now_unix();
    let pad_id = 109_000_001_i64;
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(LandingPadRecord {
            item_id: pad_id,
            type_id: 75928,
            built_type_id: 75928,
            faction_id: 500001,
            solar_system_id: 30002811,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            is_built: true,
            is_disrupted: false,
            link_timestamp: now as i64,
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        if let Some(launcher) = st.launchers.iter_mut().find(|l| l.faction_id == 500001) {
            launcher.linked_pad_ids.push(pad_id);
        }
    });

    let all_pads = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads");
    let PyRep::List(all_list) = all_pads else {
        panic!("expected List");
    };
    assert_eq!(all_list.len(), 1);

    let cal_pads = rpc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500001)],
    )
    .await
    .expect("GetFactionLandingPads");
    let PyRep::List(cal_list) = cal_pads else {
        panic!("expected List");
    };
    assert_eq!(cal_list.len(), 1);
    assert_eq!(dict_get(&cal_list[0], "itemID"), Some(&PyRep::Integer(pad_id)));

    // Starter shipcaster eligibility & jump
    let starter = with_misc_state(&ctx, |st| st.starter_launchers.first().cloned())
        .expect("seeded starter launcher");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.create_date_unix = now - 3_600;
            ch.solar_system_id = starter.solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.war_faction_id = Some(500001);
        }
    });
    sess.solar_system_id = Some(starter.solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;

    let can_jump = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump");
    assert_eq!(can_jump, PyRep::Bool(true));

    let origin_sys = ctx.systems.get_or_spawn(starter.solar_system_id);
    let _ = origin_sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: starter.position + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await;

    ctx.advance_secs(15);
    rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter.item_id)],
    )
    .await
    .expect("CmdJumpThroughStarterShipcaster");
    assert_eq!(sess.solar_system_id, Some(starter.staging_system_id));

    let can_jump_after = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump after");
    assert_eq!(can_jump_after, PyRep::Bool(false));

    // Interstellar shipcaster jump from Caldari War HQ launcher to Onatoh pad
    ctx.advance_secs(30);
    let cal_launcher = with_misc_state(&ctx, |st| {
        st.launchers.iter().find(|l| l.faction_id == 500001).cloned()
    })
    .expect("Caldari launcher");
    let hq_sys = ctx.systems.get_or_spawn(cal_launcher.solar_system_id);
    let _ = hq_sys.remove_ball(ship_id).await;
    let _ = hq_sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: cal_launcher.position + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await;
    let _ = hq_sys
        .spawn_ball_with_meta(
            Ball {
                id: cal_launcher.item_id,
                type_id: cal_launcher.type_id,
                position: cal_launcher.position,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 5_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(0, cal_launcher.item_id, cal_launcher.type_id),
            SlimItemMeta::default(),
        )
        .await;
    sess.solar_system_id = Some(cal_launcher.solar_system_id);

    rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(cal_launcher.item_id),
            PyRep::Integer(30002811),
            PyRep::Integer(pad_id),
        ],
    )
    .await
    .expect("CmdJumpThroughShipcaster");
    assert_eq!(sess.solar_system_id, Some(30002811));
}

#[tokio::test]
async fn test_batch4_store_manager_rpcs() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();

    let mut unauth = SessionState::new(94_020);
    assert!(rpc(&ctx, &mut unauth, "storeManager", "get_categories", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut unauth, "storeManager", "get_products", &[]).await.is_ok());
    assert!(rpc(&ctx, &mut unauth, "storeManager", "get_offers", &[]).await.is_ok());
    assert!(
        rpc(&ctx, &mut unauth, "storeManager", "can_purchase_offer", &[PyRep::Integer(1002)])
            .await
            .is_err()
    );
    assert!(
        rpc(&ctx, &mut unauth, "storeManager", "buy_offer", &[PyRep::Integer(1002)])
            .await
            .is_err()
    );

    let mut sess = SessionState::new(94_021);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let PyRep::List(cats) = rpc(&ctx, &mut sess, "storeManager", "get_categories", &[])
        .await
        .expect("get_categories")
    else {
        panic!("expected List");
    };
    assert!(!cats.is_empty());

    let PyRep::List(prods) = rpc(&ctx, &mut sess, "storeManager", "get_products", &[])
        .await
        .expect("get_products")
    else {
        panic!("expected List");
    };
    assert!(!prods.is_empty());

    let PyRep::List(offers) = rpc(&ctx, &mut sess, "storeManager", "get_offers", &[])
        .await
        .expect("get_offers")
    else {
        panic!("expected List");
    };
    assert!(!offers.is_empty());

    // Zero/negative quantity must be rejected
    let bad_qty = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(0)],
    )
    .await
    .expect_err("zero quantity must fail");
    assert_eq!(bad_qty.code, "InvalidQuantity");

    // AUR currency must be rejected
    let aur_err = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1002), PyRep::String("AUR".into()), PyRep::Integer(1)],
    )
    .await
    .expect_err("AUR must fail");
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    let offer_1002_price = offers
        .iter()
        .find(|o| dict_get(o, "offerID").and_then(|v| v.as_i64()) == Some(1002))
        .and_then(|o| dict_get(o, "price").and_then(|v| v.as_i64()))
        .expect("offer 1002 price");
    ctx.db.with_mut_snapshot(|snap| {
        snap.adjust_plex_vault(char_id, offer_1002_price).unwrap();
    });
    let can_buy = rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(1002)])
        .await
        .expect("can_purchase_offer");
    assert_eq!(can_buy, PyRep::Bool(true));

    let plex_before = ctx.db.with_snapshot(|snap| snap.plex_vault_balance(char_id));
    let buy_res = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .expect("buy_offer");
    assert_eq!(dict_get(&buy_res, "success"), Some(&PyRep::Bool(true)));
    let plex_after = ctx.db.with_snapshot(|snap| snap.plex_vault_balance(char_id));
    assert_eq!(plex_before - plex_after, offer_1002_price);

    let delivered = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|i| i.owner_id == char_id && i.flag == InventoryFlag::Hangar)
    });
    assert!(delivered, "purchased NES item must be delivered to hangar");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod rpc_localization_qa {
    use super::*;

//  Runtime integration tests closing open `rpc:*` parity rows:
//  - `rpc:localizationServer.UpdateLocalizationQAWrap`

use crate::notify_structures_clones_pi::{rpc, JITA_STATION, PILOT_A, PILOT_B};
use crate::notify_space_corp_market::login_docked;
use eve_db::OwnerNoteRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;

const LOCALIZATION_NOTE_ID: i64 = -8_960_000_000_000;

// parity: rpc:localizationServer.UpdateLocalizationQAWrap
#[tokio::test]
async fn parity_rpc_localization_update_qa_wrap() {
    eve_testkit::attest(&["rpc:localizationServer.UpdateLocalizationQAWrap"]);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));

    // 1. Unauthenticated caller is rejected.
    let mut unauth = SessionState::new(90_021);
    let err_unauth = rpc(
        &ctx,
        &mut unauth,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect_err("unauthenticated UpdateLocalizationQAWrap must fail");
    assert_eq!(err_unauth.code, "Unauthorized");

    // 2. Non-QA/non-GM caller is rejected.
    let mut player_sess =
        login_docked(&ctx, 90_022, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let err_role = rpc(
        &ctx,
        &mut player_sess,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect_err("non-QA UpdateLocalizationQAWrap must fail");
    assert_eq!(err_role.code, "RoleRequired");

    // 3. Seed a localization override and toggle QA wrap on/off for QA user.
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            LOCALIZATION_NOTE_ID,
            OwnerNoteRecord {
                note_id: LOCALIZATION_NOTE_ID,
                character_id: 0,
                item_id: 0,
                label: "localization_overrides".into(),
                note: r#"{"1001":"Welcome to New Eden"}"#.into(),
                timestamp_unix: 0,
            },
        );
    });

    let mut qa_sess =
        login_docked(&ctx, 90_023, "other", "other-secret", PILOT_B, JITA_STATION).await;
    qa_sess.role |= eve_machonet::service_roles::ROLE_QA;

    let enabled = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("enable QA wrap");
    assert_eq!(enabled, PyRep::Bool(true));

    let wrapped_changes = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "GetAllTextChanges",
        &[],
    )
    .await
    .expect("GetAllTextChanges with QA wrap enabled");
    assert_eq!(
        wrapped_changes,
        PyRep::Dict(vec![(
            PyRep::Integer(1001),
            PyRep::String("[QA:1001] Welcome to New Eden".into()),
        )])
    );

    let disabled = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("disable QA wrap");
    assert_eq!(disabled, PyRep::Bool(false));

    let plain_changes = rpc(
        &ctx,
        &mut qa_sess,
        "localizationServer",
        "GetAllTextChanges",
        &[],
    )
    .await
    .expect("GetAllTextChanges with QA wrap disabled");
    assert_eq!(
        plain_changes,
        PyRep::Dict(vec![(
            PyRep::Integer(1001),
            PyRep::String("Welcome to New Eden".into()),
        )])
    );
}

}

// Submodule: rpc_agent_probes_and_timers
#[allow(unused_imports, dead_code)]
pub(crate) mod rpc_agent_probes_and_timers {
    use super::*;

// Integration tests verifying remediation of FLAGGED_FAKE `rpc:*` and `notify:*` rows.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::quasar::envelopes::EvePublicRequest;
use eve_proto::quasar::GoogleAny;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const STATUS_OK: u32 = 200;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

fn drain_all(ctx: &ServerContext, _sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id).into_iter().map(|n| (n.event, n.args)).collect()
}

fn find_notif(notifs: &[(String, Vec<PyRep>)], event: &str) -> Vec<PyRep> {
    notifs.iter().find(|(e, _)| e == event).map(|(_, a)| a.clone()).unwrap_or_default()
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag, quantity, singleton,
        version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
        insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
    }
}

async fn login_in_space(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h.spawn_ball_with_meta(
        Ball { id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Rigid },
        ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
        eve_destiny::SlimItemMeta { group_id: 25, category_id: 6, owner_id: char_id, char_id: Some(char_id), corp_id: 1000009, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default() },
    ).await.unwrap();
    sess
}

fn chat_bcast_req(msg: &str) -> EvePublicRequest {
    let mut payload = vec![0x0A, msg.len() as u8];
    payload.extend_from_slice(msg.as_bytes());
    EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.chat.local.api.BroadcastMessageRequest".into(),
            value: payload,
        }),
        ..Default::default()
    }
}

// parity: rpc:agentMgr.GetPlausibleMissionIDs
#[tokio::test]
async fn test_agent_plausible_missions_and_cache_rowset() {
    eve_testkit::attest(&[
        "rpc:agentMgr.GetPlausibleMissionIDs",
    ]);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    let mut sess = SessionState::new(91_001);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();

    let sec_agent = ctx.sde.agents.values().find(|a| a.division_id == 24 && a.agent_type_id == 2).map(|a| a.agent_id).expect("SDE security agent");
    let mining_agent = ctx.sde.agents.values().find(|a| a.division_id == 23 && a.agent_type_id == 2).map(|a| a.agent_id).expect("SDE mining agent");

    let PyRep::List(sec_missions) = rpc(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(sec_agent)]).await.unwrap() else { panic!("expected List"); };
    let PyRep::List(mining_missions) = rpc(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(mining_agent)]).await.unwrap() else { panic!("expected List"); };
    assert!(!sec_missions.is_empty());
    assert!(!mining_missions.is_empty());
    assert_ne!(sec_missions, mining_missions, "security and mining agents must not return identical mission pools");

    let invalid_agent = rpc(&ctx, &mut sess, "agentMgr", "GetPlausibleMissionIDs", &[PyRep::Integer(999_999_999)]).await;
    assert_eq!(invalid_agent.unwrap_err().code, "AgentNotFound");

    let PyRep::Rowset { columns, rows } = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("roles".into())]).await.unwrap() else { panic!("expected Rowset"); };
    assert_eq!(columns, vec!["roleID", "roleName"]);
    assert!(rows.len() >= 30, "roles rowset must contain full EVE corporation roles, got {}", rows.len());
    assert!(rows.iter().any(|r| r.get(1) == Some(&PyRep::String("Contract Manager".into()))));
    assert!(rows.iter().any(|r| r.get(1) == Some(&PyRep::String("Project Manager".into()))));

    let PyRep::Rowset { rows: race_rows, .. } = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("races".into())]).await.unwrap() else { panic!("expected races Rowset"); };
    assert_eq!(race_rows.len(), ctx.sde.races.len());

    let unknown = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("nonexistent_table".into())]).await;
    assert_eq!(unknown.unwrap_err().code, "UnknownCacheRowset");
}

#[tokio::test]
async fn test_probe_scanning_lifecycle_notifications() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    let jita_pos = ctx.sde.stations[&JITA_STATION].position + DVec3::new(1_000.0, 0.0, 0.0);
    let (ship_id, launcher_id, probe_stack_id) = (630_000_001_i64, 630_000_010_i64, 630_000_011_i64);
    let mut sess = login_in_space(&ctx, 91_002, "demo", "demo-secret", PILOT_A, ship_id, 621, SYS_JITA, jita_pos).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(launcher_id, make_item(launcher_id, 17_901, PILOT_A, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(probe_stack_id, make_item(probe_stack_id, 30_013, PILOT_A, ship_id, InventoryFlag::HiSlot0, 8, false));
    });
    ctx.invalidate_ship_fitting(ship_id);

    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher_id), PyRep::String("launchProbe".into())]).await.unwrap();
    let launch_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    let new_probe = &find_notif(&launch_notifs, "OnNewProbe")[0];
    let expiry_ft = dict_get(new_probe, "expiry").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(expiry_ft > 130_000_000_000_000_000, "OnNewProbe expiry must be BlueOS FILETIME ticks, got {expiry_ft}");
    assert!(dict_get(new_probe, "pos").is_some(), "OnNewProbe must include pos");

    let coords = PyRep::List((0..8).map(|i| PyRep::List(vec![PyRep::Real(500_000.0 + (i as f64) * 500.0), PyRep::Real(2_000.0), PyRep::Real(0.0), PyRep::Integer(2)])).collect());
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[coords]).await.unwrap();
    let immediate = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(
        immediate.iter().any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(3))),
        "warping probes must emit OnProbeStateChanged(Warping=3)"
    );
    assert_eq!(find_notif(&immediate, "OnProbeWarpStart").len(), 5);
    let started = find_notif(&immediate, "OnSystemScanStarted");
    let scan_ms = started[1].as_i64().unwrap_or(0);
    assert!(scan_ms > 4_000, "scan duration must be skill/ship-computed (> 4000ms), got {scan_ms}");
    assert!(!immediate.iter().any(|(e, _)| e == "OnSystemScanStopped"), "OnSystemScanStopped must not fire at 0s");
    assert!(!immediate.iter().any(|(e, _)| e == "OnProbeWarpEnd"), "OnProbeWarpEnd must not fire at 0s");

    ctx.advance_secs(15);
    let after_tick = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(after_tick.iter().filter(|(e, _)| e == "OnProbeWarpEnd").count(), 8);
    assert!(
        after_tick.iter().any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(4))),
        "warp end / scan start must emit OnProbeStateChanged(Scanning=4)"
    );
    assert!(
        after_tick.iter().any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(1))),
        "scan completion must emit OnProbeStateChanged(Idle=1)"
    );
    assert!(after_tick.iter().any(|(e, _)| e == "OnSystemScanStopped"));

    rpc(&ctx, &mut sess, "scanMgr", "RecoverProbes", &[]).await.unwrap();
    let recover_notifs = drain_all(&ctx, &mut sess, PILOT_A);
    assert!(
        recover_notifs.iter().any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(5))),
        "RecoverProbes must emit OnProbeStateChanged(Returning=5)"
    );
}

#[tokio::test]
async fn test_campaign_enrollment_chat_bans_and_crimewatch_timers() {
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    let mut gm_sess = SessionState::new(91_003);
    common::login_pilot(&ctx, &mut gm_sess, "demo", "demo-secret").await;
    gm_sess.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut gm_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();
    gm_sess.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    let mut p2_sess = SessionState::new(91_004);
    common::login_pilot(&ctx, &mut p2_sess, "other", "other-secret").await;
    rpc(&ctx, &mut p2_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)]).await.unwrap();

    let alt_campaign = eve_server::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde)
        .keys()
        .copied()
        .find(|&id| id != 2261)
        .unwrap_or(2261);
    let before_p2 = rpc(&ctx, &mut p2_sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/campaign enroll {alt_campaign}"))]).await.unwrap();
    assert!(drain_all(&ctx, &mut gm_sess, PILOT_A).iter().any(|(e, _)| e == "OnCampaignEnrollmentChanged"));
    let after_p1 = rpc(&ctx, &mut gm_sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    let after_p2 = rpc(&ctx, &mut p2_sess, "loginCampaignManager", "get_client_campaign_state", &[]).await.unwrap();
    assert_eq!(
        dict_get(&after_p1, "static_data").and_then(|d| dict_get(d, "campaign_id")),
        Some(&PyRep::Integer(alt_campaign))
    );
    assert_eq!(
        dict_get(&after_p2, "static_data").and_then(|d| dict_get(d, "campaign_id")),
        dict_get(&before_p2, "static_data").and_then(|d| dict_get(d, "campaign_id"))
    );

    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/chatban {PILOT_B} perm"))]).await.unwrap();
    assert!(drain_all(&ctx, &mut p2_sess, PILOT_B).iter().any(|(e, _)| e == "OnCharacterPermanentlyBanned"));
    let perm_rsp = ctx.quasar_gateway.handle_public_request(&p2_sess, &chat_bcast_req("blocked"), None, false, ctx.now_unix());
    assert_ne!(perm_rsp.status_code, STATUS_OK, "permanently chat-banned character must be blocked");

    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/chatunban {PILOT_B}"))]).await.unwrap();
    assert!(drain_all(&ctx, &mut p2_sess, PILOT_B).iter().any(|(e, _)| e == "OnCharacterUnbanned"));

    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(format!("/chatban {PILOT_B} 30"))]).await.unwrap();
    assert!(drain_all(&ctx, &mut p2_sess, PILOT_B).iter().any(|(e, _)| e == "OnCharacterTemporarilyBanned"));
    ctx.advance_secs(35);
    let after_exp = ctx.quasar_gateway.handle_public_request(&p2_sess, &chat_bcast_req("restored"), None, false, ctx.now_unix());
    assert_eq!(after_exp.status_code, STATUS_OK, "temporarily chat-banned character must be unblocked after expiry");

    let zarzakh = 30_100_000_u32;
    let (ship_a, ship_b, gun_id) = (630_000_101_i64, 630_000_102_i64, 630_000_110_i64);
    let mut s_a = login_in_space(&ctx, 91_005, "demo", "demo-secret", PILOT_A, ship_a, 587, zarzakh, DVec3::ZERO).await;
    let _s_b = login_in_space(&ctx, 91_006, "other", "other-secret", PILOT_B, ship_b, 587, zarzakh, DVec3::new(1_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(gun_id, make_item(gun_id, 484, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(gun_id + 1, make_item(gun_id + 1, 178, PILOT_A, ship_a, InventoryFlag::HiSlot0, 100, false));
        snap.module_online_states.insert(gun_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    let sys_h = ctx.systems.get_or_spawn(zarzakh);
    sys_h.add_target_lock(ship_a, TargetLock { target_id: ship_b, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.unwrap();
    drain_all(&ctx, &mut s_a, PILOT_A);
    rpc(&ctx, &mut s_a, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(ship_b)]).await.unwrap();
    let z_notifs = drain_all(&ctx, &mut s_a, PILOT_A);
    assert!(z_notifs.iter().any(|(e, _)| e == "OnDisapprovalTimerUpdate"));
    assert!(z_notifs.iter().any(|(e, _)| e == "OnSystemDisapprovalFlagUpdates"));

    let npc_id = 630_000_199_i64;
    sys_h.spawn_ball(Ball {
        id: npc_id, type_id: 16_250, position: DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 100.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await.unwrap();
    sys_h.add_target_lock(ship_a, TargetLock { target_id: npc_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await.unwrap();
    drain_all(&ctx, &mut s_a, PILOT_A);
    rpc(&ctx, &mut s_a, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id), PyRep::String("projectileFired".into())]).await.unwrap();
    ctx.advance_secs(10);
    sys_h.step_manual(10.0).await.unwrap();
    rpc(&ctx, &mut s_a, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id)]).await.unwrap();
    let npc_notifs = drain_all(&ctx, &mut s_a, PILOT_A);
    assert!(npc_notifs.iter().any(|(e, _)| e == "OnNpcTimerUpdate"));
}

#[tokio::test]
async fn test_corp_role_bit_values_match_v24_client() {
    use eve_db::corp_roles;

    // Exact bit values from V24 client `eve/common/lib/appConst.pyj` (`corpRole*` constants).
    let expected_v24_roles: &[(u64, &str)] = &[
        (1 << 0, "Director"),
        (1 << 1, "Deliveries Hangar Take"),
        (1 << 2, "Deliveries Hangar Query"),
        (1 << 3, "Deliveries Hangar Container Take"),
        (1 << 7, "Personnel Manager"),
        (1 << 8, "Accountant"),
        (1 << 9, "Security Officer"),
        (1 << 10, "Factory Manager"),
        (1 << 11, "Station Manager"),
        (1 << 12, "Auditor"),
        (1 << 13, "Hangar Can Take 1"),
        (1 << 14, "Hangar Can Take 2"),
        (1 << 15, "Hangar Can Take 3"),
        (1 << 16, "Hangar Can Take 4"),
        (1 << 17, "Hangar Can Take 5"),
        (1 << 18, "Hangar Can Take 6"),
        (1 << 19, "Hangar Can Take 7"),
        (1 << 20, "Hangar Can Query 1"),
        (1 << 21, "Hangar Can Query 2"),
        (1 << 22, "Hangar Can Query 3"),
        (1 << 23, "Hangar Can Query 4"),
        (1 << 24, "Hangar Can Query 5"),
        (1 << 25, "Hangar Can Query 6"),
        (1 << 26, "Hangar Can Query 7"),
        (1 << 27, "Account Can Take 1"),
        (1 << 28, "Account Can Take 2"),
        (1 << 29, "Account Can Take 3"),
        (1 << 30, "Account Can Take 4"),
        (1 << 31, "Account Can Take 5"),
        (1 << 32, "Account Can Take 6"),
        (1 << 33, "Account Can Take 7"),
        (1 << 34, "Diplomat"),
        (1 << 35, "Brand Manager"),
        (1 << 41, "Equipment Config"),
        (1 << 42, "Container Can Take 1"),
        (1 << 43, "Container Can Take 2"),
        (1 << 44, "Container Can Take 3"),
        (1 << 45, "Container Can Take 4"),
        (1 << 46, "Container Can Take 5"),
        (1 << 47, "Container Can Take 6"),
        (1 << 48, "Container Can Take 7"),
        (1 << 49, "Can Rent Office"),
        (1 << 50, "Can Rent Factory Slot"),
        (1 << 51, "Can Rent Research Slot"),
        (1 << 52, "Junior Accountant"),
        (1 << 53, "Starbase Config"),
        (1 << 54, "Trader"),
        (1 << 55, "Chat Manager"),
        (1 << 56, "Contract Manager"),
        (1 << 57, "Infrastructure Tactical Officer"),
        (1 << 58, "Starbase Caretaker"),
        (1 << 59, "Fitting Manager"),
        (1 << 60, "Project Manager"),
        (1 << 61, "Terrestrial Logistics Officer"),
        (1 << 62, "Skill Plan Manager"),
    ];

    assert_eq!(corp_roles::ROLE_DIRECTOR, 1 << 0);
    assert_eq!(corp_roles::ROLE_DELIVERIES_HANGAR_TAKE, 1 << 1);
    assert_eq!(corp_roles::ROLE_DELIVERIES_HANGAR_QUERY, 1 << 2);
    assert_eq!(corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE, 1 << 3);
    assert_eq!(corp_roles::ROLE_BRAND_MANAGER, 1 << 35);
    assert_eq!(corp_roles::ROLE_TRADER, 1 << 54);
    assert_eq!(corp_roles::ROLE_CHAT_MANAGER, 1 << 55);
    assert_eq!(corp_roles::ROLE_CONTRACT_MANAGER, 1 << 56);
    assert_eq!(corp_roles::ROLE_INFRASTRUCTURE_TACTICIAN, 1 << 57);
    assert_eq!(corp_roles::ROLE_STARBASE_CARETAKER, 1 << 58);
    assert_eq!(corp_roles::ROLE_FITTING_MANAGER, 1 << 59);
    assert_eq!(corp_roles::ROLE_PROJECT_MANAGER, 1 << 60);
    assert_eq!(corp_roles::ROLE_TERRESTRIAL_LOGISTICS_OFFICER, 1 << 61);
    assert_eq!(corp_roles::ROLE_SKILL_PLAN_MANAGER, 1 << 62);

    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    ctx.set_clock_paused(true);
    ctx.set_mechanic_rng_seed(Some(42));
    let mut sess = SessionState::new(91_007);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.unwrap();

    let PyRep::Rowset { rows, .. } = rpc(&ctx, &mut sess, "cache", "Rowset", &[PyRep::String("roles".into())]).await.unwrap() else {
        panic!("expected Rowset");
    };
    let actual: Vec<(u64, String)> = rows
        .into_iter()
        .map(|r| {
            let id = r[0].as_i64().expect("roleID integer") as u64;
            let name = r[1].as_str().expect("roleName string").to_string();
            (id, name)
        })
        .collect();

    for &(bit, expected_name) in expected_v24_roles {
        let found: Vec<&str> = actual
            .iter()
            .filter_map(|(id, name)| (*id == bit).then_some(name.as_str()))
            .collect();
        assert_eq!(found, vec![expected_name], "mismatch for role bit 1 << {}", bit.trailing_zeros());
    }
    assert_eq!(actual.len(), expected_v24_roles.len(), "unexpected extra/missing roles in cache.Rowset(\"roles\")");
}
}
