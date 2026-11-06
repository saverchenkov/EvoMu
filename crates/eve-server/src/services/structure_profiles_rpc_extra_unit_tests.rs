//! Additional branch-coverage and mutation-killing unit tests for `structure_profiles_rpc`.

#![forbid(unsafe_code)]

use super::station_structure::structure_profiles_rpc::*;
use super::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AccessGroupRecord,
};
use super::structure_acl_defense_rules::{
    load_defense_settings, save_defense_settings, StructureDefenseSettings,
};
use super::structure_profile_services::{
    CATEGORY_HULL_DEFENSE, SETTING_DEFENSE_CAN_CONTROL_STRUCTURE, SETTING_HOUSING_CAN_DOCK,
};
use crate::router::ServerContext;
use eve_db::{AllianceRecord, CorporationRecord, UpwellStructureRecord, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    let sde = Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())));
    ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_session(char_id: i64, corp_id: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(750);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30_000_142);
    s.corp_role = role;
    s
}

fn seed_entities(ctx: &ServerContext) {
    ctx.db.with_mut_snapshot(|snap| {
        for (corp_id, ceo_id) in [(98_701_i64, 7101_i64), (98_702_i64, 7201_i64)] {
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: format!("Corp {corp_id}"),
                    ticker: "CRP".into(),
                    ceo_id,
                    hq_station_id: Some(60_003_760),
                    ..Default::default()
                },
            );
        }
        snap.alliances.insert(
            99_001_001,
            AllianceRecord {
                alliance_id: 99_001_001,
                name: "Test Alliance".into(),
                short_name: "TALL".into(),
                executor_corp_id: 98_701,
                member_corp_ids: HashSet::from([98_701]),
            },
        );
        let base_char = snap.characters.values().next().cloned().expect("demo char");
        for (cid, corp_id) in [(7101_i64, 98_701_i64), (7102, 98_701), (7201, 98_702)] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.name = format!("Char {cid}");
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_142;
            snap.characters.insert(cid, ch);
        }
        for (sid, corp_id) in [(10_000_950_001_i64, 98_701_i64), (10_000_950_002, 98_702)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: corp_id,
                    solar_system_id: 30_000_142,
                    name: format!("Citadel {sid}"),
                    position: [1_000_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    hull_hp: 100_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
    });
}

#[tokio::test(flavor = "current_thread")]
async fn profile_crud_edge_cases_and_default_scoping() {
    let ctx = test_ctx();
    seed_entities(&ctx);
    let mut sess1 = make_session(7101, 98_701, 1);
    let sess2 = make_session(7201, 98_702, 1);

    let list1 = handle_get_profiles(&ctx, &sess1).expect("get profiles corp 1");
    assert_eq!(list1, PyRep::List(Vec::new()));

    let pid_a = handle_create_profile(&ctx, &sess1, &[])
        .expect("create default name")
        .as_i64()
        .unwrap();
    let pid_b = handle_create_profile(
        &ctx,
        &sess1,
        &[PyRep::String("Custom".into()), PyRep::String("Desc".into())],
    )
    .expect("create custom")
    .as_i64()
    .unwrap();
    let pid_c2 = handle_create_profile(&ctx, &sess2, &[PyRep::String("Corp2".into())])
        .expect("create corp2")
        .as_i64()
        .unwrap();

    assert_eq!(handle_update_profile(&ctx, &sess1, &[]).unwrap_err().code, "MissingProfileId");
    assert_eq!(
        handle_update_profile(&ctx, &sess1, &[PyRep::Integer(999_999)]).unwrap_err().code,
        "ProfileNotFound"
    );
    let non_mgr = make_session(7102, 98_701, 0);
    assert_eq!(
        handle_update_profile(&ctx, &non_mgr, &[PyRep::Integer(pid_a)]).unwrap_err().code,
        "InsufficientCorpRoles"
    );

    assert_eq!(handle_duplicate_profile(&ctx, &sess1, &[]).unwrap_err().code, "MissingProfileId");
    assert_eq!(
        handle_duplicate_profile(&ctx, &sess1, &[PyRep::Integer(999_999)]).unwrap_err().code,
        "ProfileNotFound"
    );
    assert_eq!(
        handle_set_default_profile(&ctx, &sess1, &[PyRep::Integer(pid_a)]).unwrap(),
        PyRep::Bool(true)
    );
    let pid_copy = handle_duplicate_profile(&ctx, &sess1, &[PyRep::Integer(pid_a)])
        .expect("dup default copy")
        .as_i64()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let cp = ext.profiles_by_id.get(&pid_copy).unwrap();
        assert_eq!(cp.name, "New Profile (Copy)");
        assert!(!cp.is_default);
    });

    assert_eq!(handle_set_default_profile(&ctx, &sess1, &[]).unwrap_err().code, "MissingProfileId");
    assert_eq!(
        handle_set_default_profile(&ctx, &sess1, &[PyRep::Integer(999_999)]).unwrap_err().code,
        "ProfileNotFound"
    );
    let _ = handle_set_default_profile(&ctx, &sess2, &[PyRep::Integer(pid_c2)]).unwrap();
    let _ = handle_set_default_profile(&ctx, &sess1, &[PyRep::Integer(pid_b)]).unwrap();
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert!(!ext.profiles_by_id[&pid_a].is_default);
        assert!(ext.profiles_by_id[&pid_b].is_default);
        assert!(ext.profiles_by_id[&pid_c2].is_default);
    });

    assert_eq!(handle_delete_profile(&ctx, &sess1, &[]).unwrap_err().code, "MissingProfileId");
    assert_eq!(
        handle_delete_profile(&ctx, &sess1, &[PyRep::Integer(999_999)]).unwrap_err().code,
        "ProfileNotFound"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.pending_profile_applications.insert(10_000_950_001, (pid_copy, 2_000_000_000));
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_delete_profile(&ctx, &sess1, &[PyRep::Integer(pid_copy)]).unwrap_err().code,
        "ProfileInUse"
    );

    let kw = HashMap::new();
    let res_sp = handle(&ctx, &mut sess1, "structureProfiles", "GetProfiles", &[], &kw).await.unwrap();
    assert!(res_sp.is_ok());
    let res_ss = handle(&ctx, &mut sess1, "structureSettings", "CreateProfile", &[], &kw).await.unwrap();
    assert!(res_ss.is_ok());
}

#[test]
fn get_profile_settings_and_change_profiles_branches() {
    let ctx = test_ctx();
    seed_entities(&ctx);
    let sess1 = make_session(7101, 98_701, 1);
    let sess2 = make_session(7201, 98_702, 1);
    let mut gm_sess = make_session(7201, 98_702, 0);
    gm_sess.role = service_roles::ROLE_GM_MASK;

    let pid1 = handle_create_profile(&ctx, &sess1, &[PyRep::String("P1".into())])
        .unwrap()
        .as_i64()
        .unwrap();

    assert_eq!(handle_get_profile_settings(&ctx, &sess1, &[]).unwrap_err().code, "MissingProfileId");
    assert_eq!(
        handle_get_profile_settings(&ctx, &sess1, &[PyRep::Integer(888_888)]).unwrap_err().code,
        "ProfileNotFound"
    );
    assert_eq!(
        handle_get_profile_settings(&ctx, &sess2, &[PyRep::Integer(pid1)]).unwrap_err().code,
        "StructureOwnershipViolation"
    );
    assert!(handle_get_profile_settings(&ctx, &gm_sess, &[PyRep::Integer(pid1)]).is_ok());

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let prof = ext.profiles_by_id.get_mut(&pid1).unwrap();
        prof.corp_only = false;
        prof.general_settings.insert(3, true);
        prof.general_settings.insert(15, true);
        prof.general_settings.insert(26, false);
        prof.service_acl_rules.insert(23, vec![(501, 250)]);
        prof.service_settings.insert(23, 999);
        prof.service_settings.insert(24, 500);
        prof.defense_group_ids.insert(601);
        prof.defense_allowed_characters.insert(7102);
        save_ext_state(snap, &ext);
    });

    let PyRep::Dict(entries) =
        handle_get_profile_settings(&ctx, &sess1, &[PyRep::Integer(pid1)]).unwrap()
    else {
        panic!("expected dict");
    };
    let map: BTreeMap<i64, &PyRep> = entries
        .iter()
        .filter_map(|(k, v)| k.as_i64().map(|id| (id, v)))
        .collect();
    assert!(map.contains_key(&SETTING_HOUSING_CAN_DOCK));
    assert!(map.contains_key(&3));
    assert!(!map.contains_key(&15));
    assert!(map.contains_key(&26));
    assert!(map.contains_key(&23));
    assert!(map.contains_key(&24));
    let PyRep::List(def_list) = map[&17] else {
        panic!("expected list under 17");
    };
    assert_eq!(def_list.len(), 2);

    assert_eq!(handle_change_profiles(&ctx, &sess1, &[]).unwrap_err().code, "MissingStructureId");
    assert_eq!(
        handle_change_profiles(&ctx, &sess1, &[PyRep::List(vec![])]).unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_change_profiles(&ctx, &sess1, &[PyRep::Integer(10_000_950_001)]).unwrap_err().code,
        "MissingProfileId"
    );
    assert_eq!(
        handle_change_profiles(
            &ctx,
            &sess1,
            &[PyRep::Integer(10_000_950_001), PyRep::Integer(999_999)]
        )
        .unwrap_err()
        .code,
        "ProfileNotFound"
    );
    assert_eq!(
        handle_change_profiles(
            &ctx,
            &gm_sess,
            &[PyRep::Integer(10_000_950_002), PyRep::Integer(pid1)]
        )
        .unwrap_err()
        .code,
        "StructureOwnershipViolation"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut existing_ds = StructureDefenseSettings::default();
        existing_ds.structure_id = 10_000_950_001;
        existing_ds.known_attackers.push(7201);
        existing_ds.known_attacker_timestamps.insert(7201, 123_456);
        save_defense_settings(snap, &existing_ds);

        let mut ext = load_ext_state(snap);
        let mut prof_ds = StructureDefenseSettings::default();
        prof_ds.standing_threshold = -2.5;
        ext.profiles_by_id.get_mut(&pid1).unwrap().defense_settings = Some(prof_ds);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_change_profiles(
            &ctx,
            &sess1,
            &[PyRep::Integer(10_000_950_001), PyRep::Integer(pid1)]
        )
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let ds = load_defense_settings(snap, 10_000_950_001);
        assert_eq!(ds.standing_threshold, -2.5);
        assert!(ds.known_attackers.contains(&7201));
    });
}

#[test]
fn save_profile_settings_3tuple_2tuple_and_dict_matrix() {
    let ctx = test_ctx();
    seed_entities(&ctx);
    let mut sess1 = make_session(7101, 98_701, 1);

    let pid = handle_create_profile(&ctx, &sess1, &[PyRep::String("Matrix".into())])
        .unwrap()
        .as_i64()
        .unwrap();

    let gid_ok = 5001_i64;
    let gid_other_corp = 5002_i64;
    let gid_other_char = 5003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut members = HashMap::new();
        members.insert(0, 0);
        members.insert(7102, 1);
        members.insert(98_701, 1);
        members.insert(99_001_001, -1);
        ext.owner_groups.insert(
            gid_ok,
            AccessGroupRecord {
                group_id: gid_ok,
                owner_corp_id: 98_701,
                creator_id: 7101,
                name: "G1".into(),
                description: String::new(),
                members,
            },
        );
        ext.owner_groups.insert(
            gid_other_corp,
            AccessGroupRecord {
                group_id: gid_other_corp,
                owner_corp_id: 98_702,
                creator_id: 7201,
                name: "G2".into(),
                description: String::new(),
                members: HashMap::new(),
            },
        );
        ext.owner_groups.insert(
            gid_other_char,
            AccessGroupRecord {
                group_id: gid_other_char,
                owner_corp_id: 0,
                creator_id: 7201,
                name: "G3".into(),
                description: String::new(),
                members: HashMap::new(),
            },
        );
        save_ext_state(snap, &ext);
    });

    assert_eq!(handle_save_profile_settings(&ctx, &sess1, &[]).unwrap_err().code, "MissingProfileId");

    let rows = vec![
        PyRep::Tuple(vec![
            PyRep::Integer(SETTING_DEFENSE_CAN_CONTROL_STRUCTURE),
            PyRep::Integer(0),
            PyRep::Tuple(vec![
                PyRep::Bool(true),
                PyRep::Bool(false),
                PyRep::Bool(true),
                PyRep::Real(4.5),
                PyRep::Bool(true),
            ]),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(SETTING_DEFENSE_CAN_CONTROL_STRUCTURE),
            PyRep::Integer(1),
            PyRep::Integer(gid_ok),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(SETTING_DEFENSE_CAN_CONTROL_STRUCTURE),
            PyRep::Integer(7201),
            PyRep::Integer(-1),
        ]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(0), PyRep::Bool(true)]),
        PyRep::Tuple(vec![
            PyRep::Integer(SETTING_HOUSING_CAN_DOCK),
            PyRep::Integer(gid_ok),
            PyRep::Integer(1),
        ]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(7201), PyRep::Bool(false)]),
        PyRep::Tuple(vec![PyRep::Integer(23), PyRep::Integer(gid_ok), PyRep::Integer(5)]),
        PyRep::Tuple(vec![PyRep::Integer(CATEGORY_HULL_DEFENSE), PyRep::Real(-3.5)]),
        PyRep::Tuple(vec![
            PyRep::Integer(SETTING_DEFENSE_CAN_CONTROL_STRUCTURE),
            PyRep::List(vec![PyRep::Integer(7101)]),
        ]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(7101)])]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Bool(true)]),
    ];
    assert_eq!(
        handle_save_profile_settings(&ctx, &sess1, &[PyRep::Integer(pid), PyRep::List(rows)]).unwrap(),
        PyRep::Bool(true)
    );

    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let prof = &ext.profiles_by_id[&pid];
        assert!(!prof.corp_only);
        assert!(prof.access_group_ids.contains(&gid_ok));
        assert!(prof.allowed_characters.contains(&7101));
        assert!(prof.allowed_characters.contains(&7102));
        assert!(prof.allowed_corporations.contains(&98_701));
        assert!(prof.blocked_alliances.contains(&99_001_001));
        assert!(prof.blocked_characters.contains(&7201));
        assert!(prof.defense_group_ids.contains(&gid_ok));
        assert!(prof.defense_allowed_characters.contains(&7101));
        assert!(prof.defense_blocked_characters.contains(&7201));
        let ds = prof.defense_settings.as_ref().unwrap();
        assert!(ds.auto_defense_enabled);
        assert!(!ds.auto_target_attackers);
        assert!(ds.auto_target_war_targets);
        assert_eq!(ds.standing_threshold, -3.5);
        assert!(ds.target_blocked_acl);
    });

    sess1.structure_id = Some(10_000_950_001);
    let dict_payload = PyRep::Dict(vec![
        (
            PyRep::Integer(SETTING_HOUSING_CAN_DOCK),
            PyRep::List(vec![PyRep::keyval([
                ("groupID", PyRep::Integer(gid_ok)),
                ("value", PyRep::Integer(1)),
            ])]),
        ),
        (
            PyRep::Integer(SETTING_DEFENSE_CAN_CONTROL_STRUCTURE),
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("groupID".into()), PyRep::Integer(0)),
                (PyRep::String("value".into()), PyRep::Bool(false)),
            ])]),
        ),
        (
            PyRep::Integer(24),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(gid_ok), PyRep::Bool(true)])]),
        ),
        (PyRep::Integer(2), PyRep::Integer(1)),
    ]);
    assert_eq!(
        handle_save_profile_settings(&ctx, &sess1, &[PyRep::None, dict_payload]).unwrap(),
        PyRep::Bool(true)
    );

    for (entry, expected) in [
        (
            (PyRep::Integer(CATEGORY_HULL_DEFENSE), PyRep::Real(15.0)),
            "InvalidDefenseStandingThreshold",
        ),
        (
            (
                PyRep::Integer(SETTING_DEFENSE_CAN_CONTROL_STRUCTURE),
                PyRep::Tuple(vec![PyRep::Bool(true), PyRep::String("bad".into())]),
            ),
            "InvalidDefenseSettings",
        ),
        (
            (PyRep::Integer(SETTING_DEFENSE_CAN_CONTROL_STRUCTURE), PyRep::Integer(98_701)),
            "InvalidAccessEntity",
        ),
        ((PyRep::Integer(1), PyRep::Integer(gid_other_corp)), "StructureOwnershipViolation"),
        ((PyRep::Integer(1), PyRep::Integer(gid_other_char)), "StructureOwnershipViolation"),
    ] {
        assert_eq!(
            handle_save_profile_settings(&ctx, &sess1, &[PyRep::Integer(pid), PyRep::Dict(vec![entry])])
                .unwrap_err()
                .code,
            expected
        );
    }
}
