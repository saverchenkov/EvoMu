//! Unit tests for `structure_profiles_rpc` (`GetProfiles`, `CreateProfile`, `SaveProfileSettings`, `UpdateProfile`, `ChangeProfiles`, `DeleteProfile`, `DuplicateProfile`, `SetDefaultProfile`, `GetProfileSettings`, and `ownerGroupManager` RPCs).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::structure_acl_defense_rules::ACL_ROLE_ADMIN;
use eve_db::{CorporationRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_session(char_id: i64, corp_id: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(740);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30000142);
    s.corp_role = role;
    s
}

fn seed_corp_and_director(ctx: &ServerContext, char_id: i64, corp_id: i64, sid: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = format!("Profile Corp {corp_id}");
                __corp.ticker = "PRF".into();
                __corp.ceo_id = char_id;
                __corp.hq_station_id = Some(60003760);
                __corp },
        );
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.name = format!("Dir {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = 30000142;
        snap.characters.insert(char_id, ch);
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30000142,
                name: "Profile Citadel".into(),
                position: [1_000_000_000.0, 0.0, 0.0],
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
    });
}

#[tokio::test(flavor = "current_thread")]
async fn create_duplicate_update_default_save_change_and_delete_profiles() {
    let ctx = test_ctx();
    let sid = 10_000_910_001i64;
    seed_corp_and_director(&ctx, 6101, 98610, sid);
    seed_corp_and_director(&ctx, 6201, 98999, 10_000_910_002);
    let mut sess = make_session(6101, 98610, 1);
    let kw = HashMap::new();

    // Non-member character without roles cannot create profile
    let mut rogue = make_session(6999, 98999, 0);
    let denied = handle(
        &ctx,
        &mut rogue,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Unauthorized".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(denied.code, "InsufficientCorpRoles");

    // Create two profiles
    let pid1 = handle(
        &ctx,
        &mut sess,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("LowSec Doctrine".into()), PyRep::String("Main".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("create pid1")
    .as_i64()
    .expect("pid1 int");

    let pid_dup = handle(
        &ctx,
        &mut sess,
        "structureProfiles",
        "DuplicateProfile",
        &[PyRep::Integer(pid1), PyRep::String("Clone Doctrine".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("duplicate profile")
    .as_i64()
    .expect("pid_dup int");
    assert_ne!(pid1, pid_dup);

    // Update name/desc and set default profile
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureProfiles",
            "UpdateProfile",
            &[
                PyRep::Integer(pid_dup),
                PyRep::String("Updated Doctrine".into()),
                PyRep::String("New Desc".into()),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("update"),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureProfiles",
            "SetDefaultProfile",
            &[PyRep::Integer(pid_dup)],
            &kw,
        )
        .await
        .unwrap()
        .expect("set default"),
        PyRep::Bool(true)
    );

    // Create owner group and save V24 3-tuple settings (docking + defense + cloning service ACL)
    let grp_rep = handle(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "Create",
        &[PyRep::String("Blue Fleet".into()), PyRep::String("Friends".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("create group");
    let gid = match &grp_rep {
        PyRep::KeyVal(kv) => kv
            .iter()
            .find(|(k, _)| *k == "groupID")
            .and_then(|(_, v)| v.as_i64())
            .expect("gid int"),
        other => other.as_i64().expect("gid int"),
    };

    let _ = handle(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "AddMembers",
        &[
            PyRep::Integer(gid),
            PyRep::Dict(vec![(PyRep::Integer(6201), PyRep::Integer(ACL_ROLE_ADMIN))]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("add members");

    // 3-tuple entries: docking (1) + defense (17, 5-tuple) + clone bay service (7, group + tax)
    let docking_entry = PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::Integer(gid),
        PyRep::Integer(0),
    ]);
    let defense_entry = PyRep::Tuple(vec![
        PyRep::Integer(17),
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Real(-5.0),
            PyRep::Integer(1),
            PyRep::Integer(gid),
        ]),
        PyRep::Integer(0),
    ]);
    let cloning_entry = PyRep::Tuple(vec![
        PyRep::Integer(23),
        PyRep::Integer(gid),
        PyRep::Real(0.05),
    ]);
    let _ = handle(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(pid1),
            PyRep::List(vec![docking_entry, defense_entry, cloning_entry]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("save 3-tuple settings");

    // Dict form of SaveProfileSettings (legacy + V24 settingIDs)
    let _ = handle(
        &ctx,
        &mut sess,
        "structureSettings",
        "SaveProfileSettings",
        &[
            PyRep::Integer(pid1),
            PyRep::Dict(vec![
                (PyRep::String("corpOnly".into()), PyRep::Bool(true)),
                (
                    PyRep::Integer(19),
                    PyRep::Dict(vec![
                        (PyRep::String("allowedCharacters".into()), PyRep::List(vec![PyRep::Integer(6201)])),
                        (PyRep::String("blockedCorporations".into()), PyRep::List(vec![PyRep::Integer(98999)])),
                    ]),
                ),
            ]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("save dict settings");

    // GetProfiles & GetProfileSettings return populated rows
    let profs = handle(&ctx, &mut sess, "structureProfiles", "GetProfiles", &[], &kw)
        .await
        .unwrap()
        .expect("get profiles");
    assert!(matches!(profs, PyRep::List(ref l) if l.len() >= 2));

    let settings = handle(
        &ctx,
        &mut sess,
        "structureProfiles",
        "GetProfileSettings",
        &[PyRep::Integer(pid1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("get settings");
    assert!(matches!(settings, PyRep::Dict(_)));

    // ChangeProfiles assigns pid1 immediately when structure has no prior profile
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureProfiles",
            "ChangeProfiles",
            &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(pid1)],
            &kw,
        )
        .await
        .unwrap()
        .expect("change profiles"),
        PyRep::Bool(true)
    );

    // Deleting active in-use profile fails; assigning default (no prior assigned/changed profile -> wait: pid1 was assigned, so changing to pid_dup queues pending; after advancing time pid1 is freed!)
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureProfiles",
            "DeleteProfile",
            &[PyRep::Integer(pid1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ProfileInUse"
    );

    let _ = handle(
        &ctx,
        &mut sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(sid)]), PyRep::Integer(pid_dup)],
        &kw,
    )
    .await
    .unwrap()
    .expect("queue change to dup");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.structure_profile_id.insert(sid, pid_dup);
        ext.pending_profile_applications.remove(&sid);
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureProfiles",
            "DeleteProfile",
            &[PyRep::Integer(pid1)],
            &kw,
        )
        .await
        .unwrap()
        .expect("delete old profile after delay"),
        PyRep::Bool(true)
    );

    // Delete group members & fallback method returns None
    let _ = handle(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "DeleteMembers",
        &[PyRep::Integer(gid), PyRep::List(vec![PyRep::Integer(6201)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("delete members");
    assert!(handle(&ctx, &mut sess, "structureProfiles", "UnknownMethod", &[], &kw)
        .await
        .is_none());
}

#[cfg(test)]
#[path = "structure_profiles_rpc_extra_unit_tests.rs"]
mod extra_unit_tests;
