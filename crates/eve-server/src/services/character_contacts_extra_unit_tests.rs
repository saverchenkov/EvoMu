//! Extra branch-coverage and mutation-killing unit tests for `services::character_contacts`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::character_contacts::*;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord,
    KillRightRecord, TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn char_session(char_id: Option<i64>, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: char_id,
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        station_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

fn call_contacts(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_contacts(ctx, session, service, method, args, kwargs)
        .expect("character_contacts method must be dispatched")
}

fn call_home_station(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle_contacts(ctx, session, "home_station", method, args, &kw)
        .expect("home_station method must be dispatched")
}

fn rowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    match rep {
        PyRep::Rowset { rows, .. } | PyRep::TupleSet { rows, .. } => rows.clone(),
        _ => rep
            .crowset_rows()
            .map(|r| {
                r.iter()
                    .map(|p| match p {
                        PyRep::List(v) | PyRep::Tuple(v) => v.clone(),
                        _ => vec![p.clone()],
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[test]
fn public_and_private_info_employment_notes_and_respec_edge_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let corp_id = 98_500_001_i64;
    let ally_id = 99_500_001_i64;
    let kw = HashMap::new();

    // Tuple input to parse_id_list
    assert_eq!(
        parse_id_list(Some(&PyRep::Tuple(vec![PyRep::Integer(7), PyRep::String("skip".into()), PyRep::Integer(9)]))),
        vec![7, 9]
    );

    // Unselected character errors on GetPublicInfo3, GetCharacterDescription, GetEmployementRecordAndCharacterTransfers
    let mut no_char = char_session(None, None);
    let err_pub = call_contacts(&ctx, &mut no_char, "charMgr", "GetPublicInfo3", &[], &kw).unwrap_err();
    assert_eq!(err_pub.code, "CharacterNotSelected");
    let err_desc = call_contacts(&ctx, &mut no_char, "charMgr", "GetCharacterDescription", &[], &kw).unwrap_err();
    assert_eq!(err_desc.code, "CharacterNotSelected");
    let err_emp = call_contacts(
        &ctx,
        &mut no_char,
        "corporationSvc",
        "GetEmployementRecordAndCharacterTransfers",
        &[],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_emp.code, "CharacterNotSelected");

    // Populate alliance via corporation, doll portrait, war_faction_id, skills, implants, jump clones, kill rights, employment_history
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Test Corp".into();
                __corp.ticker = "TSTC".into();
                __corp.ceo_id = char_id;
                __corp.alliance_id = Some(ally_id);
                __corp.hq_station_id = Some(60_003_760);
                __corp },
        );
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.corporation_id = corp_id;
        ch.alliance_id = None; // Should inherit Some(ally_id) from corporation in GetPublicInfo3
        ch.war_faction_id = Some(500_001);
        ch.standings.insert(500_001, 4.5);
        snap.character_doll_info
            .insert(char_id, ("doll".into(), "custom:portrait:1".into()));
        snap.employment_history.insert(
            char_id,
            vec![
                (1_000_009, 1_700_000_000, Some(1_750_000_000)),
                (corp_id, 1_750_000_000, None),
            ],
        );
        snap.skills.entry(char_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        snap.active_implants.insert(char_id, vec![9899]);
        snap.jump_clones.insert(
            char_id,
            vec![JumpCloneRecord {
                jump_clone_id: 301,
                character_id: char_id,
                location_id: 60_003_760,
                implants: vec![9899],
            }],
        );
        snap.kill_rights.insert(
            401,
            KillRightRecord {
                kill_right_id: 401,
                owner_id: char_id,
                target_id: other_id,
                activation_fee: IskCents::from_isk(1_000),
                is_active_suspect: false,
            },
        );
    });

    let mut sess = char_session(Some(char_id), Some(60_003_760));
    let pub_info = call_contacts(&ctx, &mut sess, "charMgr", "GetPublicInfo3", &[], &kw).unwrap();
    assert_eq!(pub_info.dict_get("allianceID"), Some(&PyRep::Integer(ally_id)));
    assert_eq!(
        pub_info.dict_get("portraitInfo"),
        Some(&PyRep::String("custom:portrait:1".into()))
    );
    assert_eq!(pub_info.dict_get("warFactionID"), Some(&PyRep::Integer(500_001)));

    let priv_info = call_contacts(&ctx, &mut sess, "charMgr", "GetPrivateInfo", &[], &kw).unwrap();
    assert!(matches!(priv_info.dict_get("jumpClones"), Some(PyRep::List(l)) if l.len() == 1));
    assert!(matches!(priv_info.dict_get("killRights"), Some(PyRep::List(l)) if l.len() == 1));
    assert!(matches!(priv_info.dict_get("implants"), Some(PyRep::List(l)) if l.len() == 1));

    let emp_rep = call_contacts(
        &ctx,
        &mut sess,
        "corporationSvc",
        "GetEmployementRecordAndCharacterTransfers",
        &[],
        &kw,
    )
    .unwrap();
    assert_eq!(rowset_rows(&emp_rep).len(), 2);

    // EditOwnerNote & RemoveOwnerNote for non-existent or other character's note return Bool(false)
    let nid = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AddOwnerNote",
        &[PyRep::Integer(char_id), PyRep::String("Lbl".into()), PyRep::String("Txt".into())],
        &kw,
    )
    .unwrap()
    .as_i64()
    .unwrap();

    let mut other_sess = char_session(Some(other_id), Some(60_003_760));
    assert_eq!(
        call_contacts(&ctx, &mut other_sess, "charMgr", "EditOwnerNote", &[PyRep::Integer(nid), PyRep::String("X".into())], &kw).unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        call_contacts(&ctx, &mut other_sess, "charMgr", "RemoveOwnerNote", &[PyRep::Integer(nid)], &kw).unwrap(),
        PyRep::Bool(false)
    );

    // RespecCharacter via Dict and via positional args, plus invalid total -> RemapFailed
    let dict_arg = PyRep::Dict(vec![
        (PyRep::String("charisma".into()), PyRep::Integer(17)),
        (PyRep::String("intelligence".into()), PyRep::Integer(27)),
        (PyRep::String("memory".into()), PyRep::Integer(21)),
        (PyRep::String("perception".into()), PyRep::Integer(17)),
        (PyRep::String("willpower".into()), PyRep::Integer(17)),
    ]);
    let respec_ok = call_contacts(&ctx, &mut sess, "skillMgr2", "RespecCharacter", &[dict_arg], &kw).unwrap();
    assert_eq!(respec_ok.dict_get("intelligence"), Some(&PyRep::Integer(27)));

    let err_remap = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "RespecCharacter",
        &[
            PyRep::Integer(50),
            PyRep::Integer(50),
            PyRep::Integer(50),
            PyRep::Integer(50),
            PyRep::Integer(50),
        ],
        &kw,
    )
    .unwrap_err();
    assert_eq!(err_remap.code, "RemapFailed");
}

#[test]
fn contacts_block_only_filtering_entity_types_labels_and_watchlist_privacy() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let blocked_only_id = 90_000_003_i64;
    let corp_contact_id = 98_000_123_i64;
    let ally_contact_id = 99_000_123_i64;
    let mut sess = char_session(Some(char_id), Some(60_003_760));
    let kw = HashMap::new();

    // Error branches on AddContact, EditContact, DeleteContacts, BlockOwners, UnblockOwners
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "AddContact", &[], &kw).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "AddContact", &[PyRep::Integer(50)], &kw).unwrap_err().code,
        "ContactNotFound"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "EditContactsRelationshipID", &[], &kw).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "DeleteContacts", &[], &kw).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "BlockOwners", &[PyRep::Integer(char_id)], &kw).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "UnblockOwners", &[], &kw).unwrap_err().code,
        "InvalidArgument"
    );

    // Block an owner without adding as a contact -> appears in `blocked` but NOT `addresses`
    let _ = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "BlockOwners",
        &[PyRep::Integer(blocked_only_id)],
        &kw,
    )
    .unwrap();

    // Add corp, alliance, and watched character contact with labelMask kwarg
    let mut add_kw = HashMap::new();
    add_kw.insert("labelMask".into(), PyRep::Integer(5)); // bits 1 and 4
    let added = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AddContact",
        &[PyRep::Integer(other_id), PyRep::Real(5.0), PyRep::Integer(1)],
        &add_kw,
    )
    .unwrap();
    assert_eq!(added.dict_get("inWatchlist"), Some(&PyRep::Bool(true)));
    assert_eq!(added.dict_get("labelMask"), Some(&PyRep::Integer(5)));

    let _ = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AddContact",
        &[PyRep::Integer(corp_contact_id), PyRep::Real(-5.0)],
        &kw,
    )
    .unwrap();
    let _ = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AddContact",
        &[PyRep::Integer(ally_contact_id), PyRep::Real(-10.0)],
        &kw,
    )
    .unwrap();

    // EditContactsRelationshipID changes standing without clearing watchlist or labelMask
    let _ = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "EditContactsRelationshipID",
        &[PyRep::Integer(other_id), PyRep::Real(8.0)],
        &kw,
    )
    .unwrap();

    let clist = call_contacts(&ctx, &mut sess, "charMgr", "GetContacts", &[], &kw).unwrap();
    let addr_rows = rowset_rows(clist.dict_get("addresses").unwrap());
    let blk_rows = rowset_rows(clist.dict_get("blocked").unwrap());
    assert_eq!(blk_rows.len(), 1);
    assert_eq!(addr_rows.len(), 3); // blocked_only_id is excluded from addresses!
    let other_row = addr_rows
        .iter()
        .find(|r| r[0] == PyRep::Integer(other_id))
        .unwrap();
    assert_eq!(other_row[1], PyRep::Bool(true)); // watchlist preserved
    assert_eq!(other_row[2], PyRep::Real(8.0));
    assert_eq!(other_row[3], PyRep::Integer(5)); // labelMask preserved

    // Label error branches & GetLabels dict
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "CreateLabel", &[PyRep::String("   ".into())], &kw)
            .unwrap_err()
            .code,
        "InvalidLabelName"
    );
    let l1 = call_contacts(&ctx, &mut sess, "charMgr", "CreateLabel", &[PyRep::String("Alpha".into())], &kw)
        .unwrap()
        .as_i64()
        .unwrap();
    let l2 = call_contacts(&ctx, &mut sess, "charMgr", "CreateLabel", &[PyRep::String("Beta".into())], &kw)
        .unwrap()
        .as_i64()
        .unwrap();
    assert_eq!((l1, l2), (1, 2));

    let labels_dict = call_contacts(&ctx, &mut sess, "charMgr", "GetLabels", &[], &kw).unwrap();
    let PyRep::Dict(labels_entries) = labels_dict else {
        panic!("expected labels dict");
    };
    assert_eq!(labels_entries.len(), 2);

    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "EditLabel", &[], &kw).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "EditLabel", &[PyRep::Integer(l1), PyRep::String("".into())], &kw)
            .unwrap_err()
            .code,
        "InvalidLabelName"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "EditLabel", &[PyRep::Integer(999), PyRep::String("Valid".into())], &kw)
            .unwrap_err()
            .code,
        "LabelNotFound"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "DeleteLabel", &[], &kw).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "AssignLabels", &[PyRep::Integer(other_id), PyRep::Integer(0)], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "RemoveLabels", &[PyRep::Integer(other_id), PyRep::Integer(-1)], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // GetOnlineStatus batch list vs blocked privacy check
    let other_uid = ctx.db.with_snapshot(|s| s.characters[&other_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(other_uid, 888, Some(other_id), false, ctx.now_unix());

    let batch_online = call_contacts(
        &ctx,
        &mut sess,
        "onlineStatus",
        "GetOnlineStatus",
        &[PyRep::List(vec![PyRep::Integer(other_id), PyRep::Integer(blocked_only_id)])],
        &kw,
    )
    .unwrap();
    let PyRep::Dict(batch_entries) = batch_online else {
        panic!("expected dict");
    };
    assert_eq!(batch_entries.len(), 2);

    // If other_id blocks char_id, GetOnlineStatus returns false and notify_watchlist_presence is suppressed
    let mut other_sess = char_session(Some(other_id), Some(60_003_760));
    let _ = call_contacts(
        &ctx,
        &mut other_sess,
        "charMgr",
        "BlockOwners",
        &[PyRep::Integer(char_id)],
        &kw,
    )
    .unwrap();
    assert_eq!(
        call_contacts(&ctx, &mut sess, "onlineStatus", "GetOnlineStatus", &[PyRep::Integer(other_id)], &kw).unwrap(),
        PyRep::Bool(false)
    );
    notify_watchlist_presence(&ctx, other_id, false);

    assert_eq!(
        call_contacts(&ctx, &mut sess, "onlineStatus", "GetOnlineStatus", &[], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
}

#[test]
fn home_station_remote_cooldown_pochven_rejection_and_self_destruct_non_capsule() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;

    // 1. Undocked without target station -> MustBeDockedAtTargetStation
    let mut sess = char_session(Some(char_id), None);
    let err_no_st = call_home_station(&ctx, &mut sess, "set_home_station", &[]).unwrap_err();
    assert_eq!(err_no_st.code, "MustBeDockedAtTargetStation");

    // 2. Remote change to non-school, non-HQ station -> MustBeDockedAtTargetStation
    let (school_st, corp_hq_st) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        let school_st = ctx.sde.schools.get(&ch.school_id).map(|s| s.station_id).filter(|&id| id > 0);
        let corp_hq_st = snap.corporations.get(&ch.corporation_id).and_then(|c| c.hq_station_id).filter(|&id| id > 0);
        (school_st, corp_hq_st)
    });
    let non_cand_st = ctx
        .sde
        .stations
        .values()
        .filter(|st| {
            st.station_id != st_id
                && st.region_id != 10_000_070
                && st.solar_system_id < 31_000_000
                && Some(st.station_id) != school_st
                && Some(st.station_id) != corp_hq_st
        })
        .map(|st| st.station_id)
        .min()
        .unwrap_or(60_008_494);
    let err_non_cand = call_home_station(
        &ctx,
        &mut sess,
        "set_home_station",
        &[PyRep::Integer(non_cand_st)],
    )
    .unwrap_err();
    assert_eq!(err_non_cand.code, "MustBeDockedAtTargetStation");

    // 3. Remote change to corporation HQ station succeeds once, then sets 365d cooldown
    let corp_id = 98_500_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "HQ Corp".into();
                __corp.ticker = "HQC".into();
                __corp.ceo_id = char_id;
                __corp.hq_station_id = Some(non_cand_st);
                __corp },
        );
        snap.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;
    });
    let rem_ok = call_home_station(
        &ctx,
        &mut sess,
        "set_home_station",
        &[PyRep::Integer(non_cand_st)],
    )
    .unwrap();
    assert_eq!(rem_ok, PyRep::Integer(non_cand_st));

    let next_cd = call_home_station(&ctx, &mut sess, "get_next_remote_change_time", &[])
        .unwrap()
        .as_i64()
        .unwrap();
    assert!(next_cd > 0);

    let err_cd = call_home_station(
        &ctx,
        &mut sess,
        "set_home_station",
        &[PyRep::Integer(non_cand_st)],
    )
    .unwrap_err();
    assert_eq!(err_cd.code, "HomeStationRemoteCooldownActive");

    // 4. Pochven station rejection
    if let Some(poch_st) = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == 10_000_070)
        .flat_map(|s| s.stations.iter())
        .map(|st| st.station_id)
        .min()
    {
        let err_poch = call_home_station(
            &ctx,
            &mut sess,
            "set_home_station",
            &[PyRep::Integer(poch_st)],
        )
        .unwrap_err();
        assert_eq!(err_poch.code, "CannotSetMedicalCloneInPochven");
    }

    // 5. self_destruct_clone while docked in a non-capsule ship -> SelfDestructOnlyInCapsule
    sess.station_id = Some(st_id);
    let frigate_id = 778_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            frigate_id,
            InventoryItemRecord {
                item_id: frigate_id,
                type_id: 606, // Velator (not a capsule)
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    sess.ship_id = Some(frigate_id);
    let err_sd = call_home_station(&ctx, &mut sess, "self_destruct_clone", &[]).unwrap_err();
    assert!(
        err_sd.message.contains("SelfDestructOnlyInCapsule")
            || err_sd.code.contains("SelfDestructOnlyInCapsule")
    );
}

#[test]
fn contact_labels_edit_agent_max_contacts_and_home_station_structure_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 101, Some(char_id), false, ctx.now_unix());
    let mut sess = char_session(Some(char_id), Some(st_id));
    let kw = HashMap::new();

    // 1. AddContact for an SDE agent -> triggers OnAgentAdded notification
    if let Some(&agent_id) = ctx.sde.agents.keys().min() {
        let mut add_kw = HashMap::new();
        add_kw.insert("labelMask".into(), PyRep::Integer(2));
        let added = call_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(agent_id), PyRep::Real(5.0), PyRep::Bool(true)],
            &add_kw,
        )
        .unwrap();
        assert_eq!(added.dict_get("labelMask"), Some(&PyRep::Integer(2)));
        // Update same contact -> hits existing_pos = Some(idx) branch with labelMask
        let updated = call_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(agent_id), PyRep::Real(7.5), PyRep::Bool(false)],
            &add_kw,
        )
        .unwrap();
        assert_eq!(updated.dict_get("standing"), Some(&PyRep::Real(7.5)));
        let notes = ctx.drain_notifications_for_char(char_id);
        assert!(notes.iter().any(|n| n.event == "OnAgentAdded"));
    }

    // 2. EditContact on a new contact not yet in list + integer watchlist + labelMask kwarg
    let new_cid = 90_123_456_i64;
    let mut edit_kw = HashMap::new();
    edit_kw.insert("labelMask".into(), PyRep::Integer(4));
    let edited = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "EditContact",
        &[PyRep::Integer(new_cid), PyRep::Real(-2.5), PyRep::Integer(1)],
        &edit_kw,
    )
    .unwrap();
    assert_eq!(edited.dict_get("updatedCount"), Some(&PyRep::Integer(1)));

    // EditContact empty list -> InvalidArgument
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "EditContact", &[PyRep::List(vec![])], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // 3. AssignLabels / RemoveLabels validation and happy paths
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "AssignLabels", &[PyRep::List(vec![]), PyRep::Integer(1)], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_contacts(&ctx, &mut sess, "charMgr", "RemoveLabels", &[PyRep::Integer(new_cid), PyRep::Integer(0)], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    let lbl_id = call_contacts(&ctx, &mut sess, "charMgr", "CreateLabel", &[PyRep::String("VIP".into())], &kw)
        .unwrap()
        .as_i64()
        .unwrap();
    let assigned = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AssignLabels",
        &[PyRep::Integer(new_cid), PyRep::Integer(lbl_id)],
        &kw,
    )
    .unwrap();
    assert_eq!(assigned.dict_get("updatedCount"), Some(&PyRep::Integer(1)));
    let removed_lbl = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "RemoveLabels",
        &[PyRep::Integer(new_cid), PyRep::Integer(lbl_id)],
        &kw,
    )
    .unwrap();
    assert_eq!(removed_lbl.dict_get("updatedCount"), Some(&PyRep::Integer(1)));

    // Re-assign and delete label -> strips label from contacts in ESI, DB, and social_extras
    let _ = call_contacts(
        &ctx,
        &mut sess,
        "charMgr",
        "AssignLabels",
        &[PyRep::Integer(new_cid), PyRep::Integer(lbl_id)],
        &kw,
    )
    .unwrap();
    ctx.social_extras.lock().unwrap().contacts.entry(char_id).or_default().insert(new_cid, (-2.5, true, lbl_id as u64));
    let del = call_contacts(&ctx, &mut sess, "charMgr", "DeleteLabel", &[PyRep::Integer(lbl_id)], &kw).unwrap();
    assert_eq!(del.dict_get("deleted"), Some(&PyRep::Bool(true)));

    // 4. MaxContactsExceeded when 1024 contacts already exist
    {
        let mut guard = ctx.esi_state.char_contacts.write().unwrap();
        let list = guard.entry(char_id).or_default();
        let start = 91_000_000_i64;
        let mut k = 0i64;
        while list.len() < 1_024 {
            list.push(crate::esi::runtime_state::EsiContactEntry {
                contact_id: start + k,
                contact_type: "character".into(),
                standing: 0.0,
                is_watched: false,
                is_blocked: false,
                label_ids: Vec::new(),
            });
            k += 1;
        }
    }
    assert_eq!(
        call_contacts(
            &ctx,
            &mut sess,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(99_999_888), PyRep::Real(1.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "MaxContactsExceeded"
    );

    // 5. charMgr.GetHomeStation + home_station.get_home_station with Upwell structure home
    let get_hs = call_contacts(&ctx, &mut sess, "charMgr", "GetHomeStation", &[], &kw).unwrap();
    assert!(get_hs.as_i64().is_some());
    let upwell_id = 1_000_000_888_777_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            upwell_id,
            eve_db::UpwellStructureRecord {
                structure_id: upwell_id,
                solar_system_id: 30_000_142,
                owner_corp_id: 1_000_009,
                type_id: 35_832,
                name: "Home Astrahus".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.characters.get_mut(&char_id).unwrap().home_station_id = upwell_id;
    });
    let hs_info = call_home_station(&ctx, &mut sess, "get_home_station", &[]).unwrap();
    assert_eq!(hs_info.dict_get("station_id"), Some(&PyRep::Integer(upwell_id)));
    assert_eq!(hs_info.dict_get("type_id"), Some(&PyRep::Integer(35_832)));
}
