//! Unit tests for `structure_acl_defense_rules` (ACL group RPCs, docking ACL checks, auto-defense targeting, cyno inhibition, fuel bay management, and character service ACL RPCs).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
};
use eve_db::types::WarRecord;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
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

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("AclType_{new_tid}");
    rec.volume = 100.0;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_session(char_id: i64, corp_id: i64, sys_id: u32, structure_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(730);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.structure_id = structure_id;
    s.corp_role = role;
    s
}

fn seed_char_and_corp(ctx: &ServerContext, char_id: i64, corp_id: i64, sys_id: u32, is_ceo: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp_id).or_insert_with(|| { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = format!("Corp {corp_id}");
            __corp.ticker = "ACL".into();
            __corp.ceo_id = if is_ceo { char_id } else { char_id + 900 };
            __corp.hq_station_id = Some(60003760);
            __corp.tax_rate = 0.0;
            __corp.members = HashSet::from([char_id]);
            __corp.set_division_balance(1, IskCents(0));
            __corp });
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(char_id);
        }
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = 0;
        ch.home_station_id = 60003760;
        ch.balance = IskCents(10_000);
        ch.security_status = 5.0;
        snap.characters.insert(char_id, ch);
    });
}

#[tokio::test]
async fn owner_group_manager_updates_members_edits_metadata_and_queries() {
    let ctx = test_ctx();
    seed_char_and_corp(&ctx, 7101, 98710, 30000142, true);
    seed_char_and_corp(&ctx, 7102, 98711, 30000142, true);

    let grp_id = 400_001i64;
    let prof_id = 500_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut members = HashMap::new();
        members.insert(7101, ACL_ROLE_ADMIN);
        members.insert(7102, ACL_ROLE_BLOCKED);
        ext.owner_groups.insert(
            grp_id,
            AccessGroupRecord {
                group_id: grp_id,
                creator_id: 7101,
                owner_corp_id: 98710,
                name: "Initial Group".into(),
                description: "Desc".into(),
                members,
            },
        );
        let mut prof = StructureAccessProfile {
            profile_id: prof_id,
            owner_corp_id: 98710,
            name: "Default".into(),
            ..Default::default()
        };
        prof.access_group_ids.insert(grp_id);
        ext.profiles_by_id.insert(prof_id, prof.clone());
        ext.access_profiles.insert(98710, prof);
        save_ext_state(snap, &ext);
    });

    let mut sess = make_session(7101, 98710, 30000142, None, 1);
    let _ = handle_edit_group(
        &ctx,
        &sess,
        &[
            PyRep::Integer(grp_id),
            PyRep::String("Renamed Group".into()),
            PyRep::String("Updated Desc".into()),
        ],
    )
    .expect("edit group");

    let _ = handle_set_member_role(
        &ctx,
        &sess,
        &[
            PyRep::Integer(grp_id),
            PyRep::Dict(vec![(PyRep::Integer(7102), PyRep::Integer(ACL_ROLE_MEMBER))]),
        ],
    )
    .expect("update membership types");

    assert!(handle_get_group(&ctx, &sess, &[PyRep::Integer(grp_id)]).is_ok());
    let members_rep = handle_get_members(&ctx, &sess, &[PyRep::Integer(grp_id)]).expect("get members");
    assert!(matches!(members_rep, PyRep::Dict(_)));

    // Unauthorized character cannot edit
    let rogue = make_session(7102, 98711, 30000142, None, 0);
    assert_eq!(
        handle_edit_group(&ctx, &rogue, &[PyRep::Integer(grp_id), PyRep::String("Hack".into())])
            .unwrap_err()
            .code,
        "InsufficientCorpRoles"
    );

    // Dispatch wrapper routes all ownerGroupManager methods
    let kw = HashMap::new();
    assert!(handle(&ctx, &mut sess, "ownerGroupManager", "GetGroup", &[PyRep::Integer(grp_id)], &kw).await.is_some());
    assert!(handle(&ctx, &mut sess, "ownerGroupManager", "GetGroupMembers", &[PyRep::Integer(grp_id)], &kw).await.is_some());
}

#[test]
fn docking_acl_checks_owner_allowed_blocked_corp_only_and_gm() {
    let mut ctx = test_ctx();
    let st_tid = 940_001u32;
    clone_type_with(&mut ctx.sde, 35832, st_tid, 1657, 65, &[]);
    seed_char_and_corp(&ctx, 7201, 98720, 30000142, true);
    seed_char_and_corp(&ctx, 7202, 98721, 30000142, true);
    seed_char_and_corp(&ctx, 7203, 98722, 30000142, true);

    let sid = 10_000_980_001i64;
    let prof_id = 500_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: st_tid,
                owner_corp_id: 98720,
                solar_system_id: 30000142,
                name: "Dock Test".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile {
            profile_id: prof_id,
            owner_corp_id: 98720,
            name: "DockACL".into(),
            ..Default::default()
        };
        prof.allowed_characters.insert(7202);
        prof.blocked_characters.insert(7203);
        ext.access_profiles.insert(sid, prof.clone());
        ext.profiles_by_id.insert(prof_id, prof);
        ext.structure_profile_id.insert(sid, prof_id);
        save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        assert!(verify_structure_docking_acl(&ctx, snap, sid, 7201).is_ok());
        assert!(verify_structure_docking_acl(&ctx, snap, sid, 7202).is_ok());
        assert_eq!(
            verify_structure_docking_acl(&ctx, snap, sid, 7203).unwrap_err().code,
            "StructureDockingAccessDenied"
        );
    });

    // Explicit docking ACL denial overrides owner membership
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.entry(sid).or_default().insert(7201);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            verify_structure_docking_acl(&ctx, snap, sid, 7201).unwrap_err().code,
            "StructureDockingAccessDenied"
        );
    });
}

#[tokio::test]
async fn auto_defense_settings_pruning_war_targets_and_cyno_inhibitor_work() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let mut ds = StructureDefenseSettings::default();
    ds.structure_id = 10_000_990_001;
    ds.auto_defense_enabled = true;
    ds.record_attacker(8001, now.saturating_sub(STRUCTURE_ATTACKER_TTL_SECS + 50));
    ds.record_attacker(8002, now);
    assert!(!ds.is_active_attacker(8001, now));
    assert!(ds.is_active_attacker(8002, now));

    seed_char_and_corp(&ctx, 7301, 98730, 30000142, true);
    seed_char_and_corp(&ctx, 7302, 98731, 30000142, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            10_000_990_001,
            UpwellStructureRecord {
                structure_id: 10_000_990_001,
                type_id: 35832,
                owner_corp_id: 98730,
                solar_system_id: 30000142,
                name: "Defense Test".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.wars.insert(
            1,
            WarRecord {
                war_id: 1,
                aggressor_corp_id: 98731,
                defender_corp_id: 98730,
                mutual: true,
                started_unix: now.saturating_sub(100),
                active: true,
            },
        );
        save_defense_settings(snap, &ds);
        snap.items.insert(
            777,
            InventoryItemRecord {
                item_id: 777,
                type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id: 98730,
                location_id: 30000142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(777, [1_000.0, 0.0, 0.0]);
    });

    ctx.db.with_snapshot(|snap| {
        assert!(are_corps_at_war_in_snap(snap, 98731, 98730, now));
        assert!(evaluate_auto_defense_target(&ctx, snap, 10_000_990_001, 7302));
        assert!(!evaluate_auto_defense_target(&ctx, snap, 10_000_990_001, 7301));
        assert!(is_cyno_inhibited(&ctx, snap, 30000142, Some(DVec3::new(5_000.0, 0.0, 0.0))));
        assert!(!is_cyno_inhibited(&ctx, snap, 30000142, Some(DVec3::new(500_000.0, 0.0, 0.0))));
    });
}

#[test]
fn fuel_bay_validation_consumption_disposal_and_character_service_rpcs_work() {
    let mut ctx = test_ctx();
    let st_tid = 940_010u32;
    let svc_tid = 940_011u32;
    clone_type_with(
        &mut ctx.sde,
        35832,
        st_tid,
        1657,
        65,
        &[(life::ATTR_STRUCTURE_ROLE_BONUS, 25.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        35832,
        svc_tid,
        1321,
        66,
        &[
            (life::ATTR_SERVICE_FUEL_GROUP, 1136.0),
            (life::ATTR_SERVICE_FUEL_ONLINE, 40.0),
            (life::ATTR_SERVICE_FUEL_PER_HOUR, 10.0),
        ],
    );
    let fuel_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1136)
        .map(|t| t.type_id)
        .unwrap_or(4051);

    seed_char_and_corp(&ctx, 7401, 98740, 30000142, true);
    let sid = 10_000_995_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: st_tid,
                owner_corp_id: 98740,
                solar_system_id: 30000142,
                name: "Fuel Citadel".into(),
                position: [0.0, 0.0, 0.0],
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
        snap.items.insert(
            81001,
            InventoryItemRecord {
                item_id: 81001,
                type_id: fuel_tid,
                owner_id: 7401,
                location_id: sid,
                flag: InventoryFlag::StructureFuel,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert!(is_fuel_block_type(&ctx, fuel_tid));
    assert_eq!(service_module_online_fuel_cost(&ctx, st_tid, svc_tid), 30);

    let mut sess = make_session(7401, 98740, 30000142, Some(sid), 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&81001).unwrap().flag = InventoryFlag::Hangar;
        assert!(consume_structure_fuel_blocks(&ctx, snap, sid, 25));
        assert_eq!(snap.structures[&sid].fuel_blocks, 75);
        assert!(validate_structure_fuel_bay_transfer(
            &ctx,
            snap,
            &sess,
            7401,
            sid,
            fuel_tid,
            true,
            sid,
            30000142,
            0,
            &HashMap::new(),
        )
        .is_ok());
        snap.items.get_mut(&81001).unwrap().flag = InventoryFlag::StructureFuel;
        finalize_fuel_bay_move_in_snap(
            &ctx,
            snap,
            81001,
            sid,
            InventoryFlag::Hangar,
            sid,
            InventoryFlag::StructureFuel,
            7401,
            98740,
        );
        assert_eq!(snap.structures[&sid].fuel_blocks, 95);
        materialize_structure_fuel_for_disposal(&ctx, snap, sid);
        assert_eq!(snap.structures[&sid].fuel_blocks, 0);
    });

    // Character service ACL RPCs
    let get_svcs = handle_character_get_services(&ctx, &mut sess, &[PyRep::Integer(sid)])
        .expect("get services");
    assert!(matches!(get_svcs, PyRep::Dict(_)));
    let has_svc = handle_character_has_service(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::Integer(1)])
        .expect("has service");
    assert!(matches!(has_svc, PyRep::Bool(_)));
    let _ = handle_character_check_service(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::Integer(1)]);
    assert!(handle_character_has_setting(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::Integer(1)]).is_ok());
}

#[test]
fn ex_director_creator_loses_corp_acl_admin_and_can_be_demoted_by_director() {
    let ctx = test_ctx();
    let corp_a = 98_750_i64;
    let corp_b = 98_751_i64;
    let ex_dir = 7_501_i64;
    let active_dir = 7_502_i64;
    let group_id = 80_750_001_i64;
    seed_char_and_corp(&ctx, ex_dir, corp_a, 30_000_142, true);
    seed_char_and_corp(&ctx, active_dir, corp_a, 30_000_142, false);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_a) {
            c.ceo_id = active_dir;
        }
        let mut ext = load_ext_state(snap);
        ext.owner_groups.insert(
            group_id,
            ops::AccessGroupRecord {
                group_id,
                creator_id: ex_dir,
                owner_corp_id: corp_a,
                name: "Corp ACL".into(),
                description: "Corp".into(),
                members: [(ex_dir, ACL_ROLE_ADMIN)].into_iter().collect(),
            },
        );
        save_ext_state(snap, &ext);
    });

    // Active corp CEO/director can demote/exclude creator on a corp-owned group
    let mut dir_sess = SessionState::new(7502);
    dir_sess.user_id = Some(active_dir as u64);
    dir_sess.character_id = Some(active_dir);
    dir_sess.corporation_id = Some(corp_a);
    handle_set_member_role(
        &ctx,
        &dir_sess,
        &[PyRep::Integer(group_id), PyRep::Integer(ex_dir), PyRep::Integer(ACL_ROLE_EXCLUDED)],
    )
    .expect("active corp director can block creator on corp group");

    let mut ex_sess = SessionState::new(7501);
    ex_sess.user_id = Some(ex_dir as u64);
    ex_sess.character_id = Some(ex_dir);
    ex_sess.corporation_id = Some(corp_a);

    assert_eq!(
        handle_edit_group(&ctx, &ex_sess, &[PyRep::Integer(group_id), PyRep::String("Hacked".into())])
            .unwrap_err()
            .code,
        "InsufficientCorpRoles"
    );
    assert_eq!(
        handle_get_group(&ctx, &ex_sess, &[PyRep::Integer(group_id)])
            .unwrap_err()
            .code,
        "AccessGroupAccessDenied"
    );

    // Clear explicit member entry AND move ex_dir to corp_b -> ex_dir still has no creator admin/view
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.owner_groups.get_mut(&group_id).unwrap().members.clear();
        save_ext_state(snap, &ext);
        if let Some(ch) = snap.characters.get_mut(&ex_dir) {
            ch.corporation_id = corp_b;
        }
    });
    ex_sess.corporation_id = Some(corp_b);
    assert_eq!(
        handle_edit_group(&ctx, &ex_sess, &[PyRep::Integer(group_id), PyRep::String("Hacked".into())])
            .unwrap_err()
            .code,
        "InsufficientCorpRoles"
    );
    assert_eq!(
        handle_get_members(&ctx, &ex_sess, &[PyRep::Integer(group_id)])
            .unwrap_err()
            .code,
        "AccessGroupAccessDenied"
    );
}

#[tokio::test]
async fn owner_group_get_my_groups_returns_v24_fields_and_filters_excluded() {
    let ctx = test_ctx();
    let admin_id = 910_801_i64;
    let member_id = 910_802_i64;
    let excluded_id = 910_803_i64;
    let outsider_id = 910_804_i64;
    let corp_a = 98_910_801_i64;
    let corp_b = 98_910_802_i64;
    let g_corp = 80_901_001_i64;
    let g_personal = 80_901_002_i64;
    let g_other = 80_901_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, corp) in [
            (admin_id, corp_a),
            (member_id, corp_a),
            (excluded_id, corp_a),
            (outsider_id, corp_b),
        ] {
            let mut ch = snap
                .characters
                .values()
                .next()
                .cloned()
                .expect("seed character");
            ch.character_id = cid;
            ch.corporation_id = corp;
            snap.characters.insert(cid, ch);
        }
        let mut ext = load_ext_state(snap);
        ext.owner_groups.insert(
            g_corp,
            AccessGroupRecord {
                group_id: g_corp,
                creator_id: admin_id,
                owner_corp_id: corp_a,
                name: "Corp ACL".into(),
                description: "Corp access".into(),
                members: HashMap::from([
                    (member_id, ACL_ROLE_MANAGER),
                    (excluded_id, ACL_ROLE_EXCLUDED),
                ]),
            },
        );
        ext.owner_groups.insert(
            g_personal,
            AccessGroupRecord {
                group_id: g_personal,
                creator_id: admin_id,
                owner_corp_id: 0,
                name: "Personal ACL".into(),
                description: "Shared list".into(),
                members: HashMap::from([(member_id, ACL_ROLE_MEMBER)]),
            },
        );
        ext.owner_groups.insert(
            g_other,
            AccessGroupRecord {
                group_id: g_other,
                creator_id: outsider_id,
                owner_corp_id: corp_b,
                name: "Other Corp ACL".into(),
                description: "Hidden".into(),
                members: HashMap::new(),
            },
        );
        save_ext_state(snap, &ext);
    });

    let kw = HashMap::new();
    let mut admin_sess = make_session(admin_id, corp_a, 30_000_142, None, 0);
    let PyRep::List(admin_groups) = handle(&ctx, &mut admin_sess, "ownerGroupManager", "GetMyGroups", &[], &kw)
        .await
        .expect("GetMyGroups handled")
        .expect("admin GetMyGroups ok")
    else {
        panic!("expected list from GetMyGroups");
    };
    let admin_gids: Vec<i64> = admin_groups
        .iter()
        .filter_map(|g| g.dict_get("groupID").and_then(PyRep::as_i64))
        .collect();
    assert_eq!(admin_gids, vec![g_corp, g_personal]);
    assert_eq!(
        admin_groups[0].dict_get("membershipType"),
        Some(&PyRep::Integer(ACL_ROLE_ADMIN))
    );
    assert_eq!(
        admin_groups[0].dict_get("admins"),
        Some(&PyRep::List(vec![PyRep::Integer(admin_id)]))
    );

    let mut member_sess = make_session(member_id, corp_a, 30_000_142, None, 0);
    let PyRep::List(member_groups) = handle(&ctx, &mut member_sess, "ownerGroupManager", "GetMyGroups", &[], &kw)
        .await
        .expect("GetMyGroups handled")
        .expect("member GetMyGroups ok")
    else {
        panic!("expected list");
    };
    assert_eq!(member_groups.len(), 2);
    assert_eq!(
        member_groups[0].dict_get("membershipType"),
        Some(&PyRep::Integer(ACL_ROLE_MANAGER))
    );
    assert_eq!(
        member_groups[1].dict_get("membershipType"),
        Some(&PyRep::Integer(ACL_ROLE_MEMBER))
    );

    let mut excluded_sess = make_session(excluded_id, corp_a, 30_000_142, None, 0);
    let PyRep::List(excluded_groups) = handle(&ctx, &mut excluded_sess, "ownerGroupManager", "GetMyGroups", &[], &kw)
        .await
        .expect("GetMyGroups handled")
        .expect("excluded GetMyGroups ok")
    else {
        panic!("expected list");
    };
    assert!(excluded_groups.is_empty(), "excluded character must not receive corp-owned group");
}

#[tokio::test]
async fn owner_group_get_groups_many_handles_public_zero_dedup_and_visibility() {
    let ctx = test_ctx();
    let admin_id = 910_901_i64;
    let member_id = 910_902_i64;
    let excluded_id = 910_903_i64;
    let outsider_id = 910_904_i64;
    let gm_id = 910_905_i64;
    let corp_a = 98_910_901_i64;
    let corp_b = 98_910_902_i64;
    let g_corp = 80_902_001_i64;
    let g_public = 80_902_002_i64;
    let g_other = 80_902_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, corp) in [
            (admin_id, corp_a),
            (member_id, corp_a),
            (excluded_id, corp_a),
            (outsider_id, corp_b),
            (gm_id, corp_b),
        ] {
            let mut ch = snap
                .characters
                .values()
                .next()
                .cloned()
                .expect("seed character");
            ch.character_id = cid;
            ch.corporation_id = corp;
            snap.characters.insert(cid, ch);
        }
        let mut ext = load_ext_state(snap);
        ext.owner_groups.insert(
            g_corp,
            AccessGroupRecord {
                group_id: g_corp,
                creator_id: admin_id,
                owner_corp_id: corp_a,
                name: "Corp ACL".into(),
                description: "Corp access".into(),
                members: HashMap::from([
                    (member_id, ACL_ROLE_MANAGER),
                    (excluded_id, ACL_ROLE_EXCLUDED),
                ]),
            },
        );
        ext.owner_groups.insert(
            g_public,
            AccessGroupRecord {
                group_id: g_public,
                creator_id: admin_id,
                owner_corp_id: 0,
                name: "Public ACL".into(),
                description: "Open access".into(),
                members: HashMap::from([
                    (0, ACL_ROLE_MEMBER),
                    (excluded_id, ACL_ROLE_EXCLUDED),
                ]),
            },
        );
        ext.owner_groups.insert(
            g_other,
            AccessGroupRecord {
                group_id: g_other,
                creator_id: outsider_id,
                owner_corp_id: corp_b,
                name: "Private Corp B".into(),
                description: "Hidden".into(),
                members: HashMap::new(),
            },
        );
        save_ext_state(snap, &ext);
    });

    let kw = HashMap::new();
    let mut admin_sess = make_session(admin_id, corp_a, 30_000_142, None, 0);
    let PyRep::List(admin_rows) = handle(
        &ctx,
        &mut admin_sess,
        "ownerGroupManager",
        "GetGroupsMany",
        &[PyRep::List(vec![
            PyRep::Integer(0),
            PyRep::Integer(g_corp),
            PyRep::Integer(0),
            PyRep::Integer(g_corp),
            PyRep::Integer(999_999),
            PyRep::Integer(g_other),
        ])],
        &kw,
    )
    .await
    .expect("GetGroupsMany handled")
    .expect("admin GetGroupsMany ok")
    else {
        panic!("expected List from GetGroupsMany");
    };
    let admin_gids: Vec<i64> = admin_rows
        .iter()
        .filter_map(|g| g.dict_get("groupID").and_then(PyRep::as_i64))
        .collect();
    assert_eq!(admin_gids, vec![0, g_corp]);
    assert_eq!(admin_rows[0].dict_get("name"), Some(&PyRep::String("Public".into())));
    assert_eq!(admin_rows[0].dict_get("creatorID"), Some(&PyRep::Integer(1)));
    assert_eq!(admin_rows[0].dict_get("ownerCorpID"), Some(&PyRep::Integer(0)));
    assert_eq!(admin_rows[0].dict_get("memberCount"), Some(&PyRep::Integer(0)));
    assert_eq!(
        admin_rows[0].dict_get("membershipType"),
        Some(&PyRep::Integer(ACL_ROLE_NONE))
    );
    assert_eq!(admin_rows[0].dict_get("admins"), Some(&PyRep::List(vec![])));
    assert_eq!(
        admin_rows[1].dict_get("membershipType"),
        Some(&PyRep::Integer(ACL_ROLE_ADMIN))
    );
    assert_eq!(
        admin_rows[1].dict_get("admins"),
        Some(&PyRep::List(vec![PyRep::Integer(admin_id)]))
    );

    // Outsider sees synthetic group 0 and public group (with PUBLIC_MEMBER_ID = 0), but not corp_a private group
    let mut outsider_sess = make_session(outsider_id, corp_b, 30_000_142, None, 0);
    let PyRep::List(outsider_rows) = handle(
        &ctx,
        &mut outsider_sess,
        "ownerGroupManager",
        "GetGroupsMany",
        &[PyRep::List(vec![
            PyRep::Integer(0),
            PyRep::Integer(g_corp),
            PyRep::Integer(g_public),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .expect("outsider GetGroupsMany ok")
    else {
        panic!("expected List");
    };
    let outsider_gids: Vec<i64> = outsider_rows
        .iter()
        .filter_map(|g| g.dict_get("groupID").and_then(PyRep::as_i64))
        .collect();
    assert_eq!(outsider_gids, vec![0, g_public]);
    assert_eq!(
        outsider_rows[1].dict_get("membershipType"),
        Some(&PyRep::Integer(ACL_ROLE_NONE))
    );

    // Excluded character is omitted from both g_corp and g_public, while keeping synthetic 0
    let mut excluded_sess = make_session(excluded_id, corp_a, 30_000_142, None, 0);
    let PyRep::List(excluded_rows) = handle(
        &ctx,
        &mut excluded_sess,
        "ownerGroupManager",
        "GetGroupsMany",
        &[PyRep::List(vec![
            PyRep::Integer(0),
            PyRep::Integer(g_corp),
            PyRep::Integer(g_public),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .expect("excluded GetGroupsMany ok")
    else {
        panic!("expected List");
    };
    let excluded_gids: Vec<i64> = excluded_rows
        .iter()
        .filter_map(|g| g.dict_get("groupID").and_then(PyRep::as_i64))
        .collect();
    assert_eq!(excluded_gids, vec![0]);

    // Empty list returns empty list; missing arg returns MissingGroupId; GM sees private groups
    let empty_rep = handle(
        &ctx,
        &mut admin_sess,
        "ownerGroupManager",
        "GetGroupsMany",
        &[PyRep::List(vec![])],
        &kw,
    )
    .await
    .unwrap()
    .expect("empty GetGroupsMany ok");
    assert_eq!(empty_rep, PyRep::List(vec![]));
    let missing_err = handle(&ctx, &mut admin_sess, "ownerGroupManager", "GetGroupsMany", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(missing_err.code, "MissingGroupId");

    let mut gm_sess = make_session(gm_id, corp_b, 30_000_142, None, 0);
    gm_sess.role |= eve_machonet::service_roles::ROLE_GMH;
    let PyRep::List(gm_rows) = handle(
        &ctx,
        &mut gm_sess,
        "ownerGroupManager",
        "GetGroupsMany",
        &[PyRep::List(vec![PyRep::Integer(g_corp), PyRep::Integer(g_other)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("gm GetGroupsMany ok")
    else {
        panic!("expected List");
    };
    let gm_gids: Vec<i64> = gm_rows
        .iter()
        .filter_map(|g| g.dict_get("groupID").and_then(PyRep::as_i64))
        .collect();
    assert_eq!(gm_gids, vec![g_corp, g_other]);
}

#[test]
fn character_get_services_includes_builtin_upwell_services_and_agrees_with_single_checks() {
    use crate::services::structure_profile_services as prof_svc;

    let ctx = test_ctx();
    let owner_corp = 98_750_i64;
    let owner_id = 75_001_i64;
    let outsider_corp = 98_751_i64;
    let outsider_id = 75_002_i64;
    seed_char_and_corp(&ctx, owner_id, owner_corp, 30_000_142, true);
    seed_char_and_corp(&ctx, outsider_id, outsider_corp, 30_000_142, true);

    let astra_id = 1_075_001_001_i64;
    let uncored_id = 1_075_001_002_i64;
    let destroyed_id = 1_075_001_003_i64;
    let nocore_id = 1_075_001_004_i64;
    let jb_flex_id = 1_075_001_005_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mk_struct = |id: i64, type_id: u32, state: &str, cored: bool| UpwellStructureRecord {
            structure_id: id,
            type_id,
            owner_corp_id: owner_corp,
            solar_system_id: 30_000_142,
            name: format!("AclSvc_{id}"),
            state: state.into(),
            quantum_core_installed: cored,
            fuel_blocks: 100,
            ..Default::default()
        };
        snap.structures.insert(astra_id, mk_struct(astra_id, 35_832, "ShieldVulnerable", true));
        snap.structures.insert(uncored_id, mk_struct(uncored_id, 35_832, "ShieldVulnerable", false));
        snap.structures.insert(destroyed_id, mk_struct(destroyed_id, 35_832, "Destroyed", true));
        snap.structures.insert(nocore_id, mk_struct(nocore_id, 35_832, "NoCore", false));
        snap.structures.insert(jb_flex_id, mk_struct(jb_flex_id, 35_841, "ShieldVulnerable", true));
    });

    let get_services_map = |ctx: &ServerContext, sess: &mut SessionState, sid: i64| -> HashMap<i64, f64> {
        let rep = handle_character_get_services(ctx, sess, &[PyRep::Integer(sid)])
            .expect("CharacterGetServices ok");
        match rep {
            PyRep::Dict(entries) => entries
                .into_iter()
                .filter_map(|(k, v)| match (k.as_i64(), v) {
                    (Some(id), PyRep::Real(tax)) => Some((id, tax)),
                    _ => None,
                })
                .collect(),
            other => panic!("expected Dict from CharacterGetServices, got {other:?}"),
        }
    };

    let assert_all_rpcs_agree = |ctx: &ServerContext, sess: &mut SessionState, sid: i64, svcs: &[i64]| {
        let map = get_services_map(ctx, sess, sid);
        for &svc_id in svcs {
            let expected_tax = map.get(&svc_id).copied();
            assert_eq!(
                handle_character_has_service(ctx, sess, &[PyRep::Integer(sid), PyRep::Integer(svc_id)])
                    .expect("CharacterHasService"),
                PyRep::Bool(expected_tax.is_some()),
                "HasService disagreement on structure {sid} svc {svc_id}"
            );
            let check_res = handle_character_check_service(ctx, sess, &[PyRep::Integer(sid), PyRep::Integer(svc_id)]);
            if expected_tax.is_some() {
                assert_eq!(check_res.expect("CheckService allow"), PyRep::None);
            } else {
                assert_eq!(check_res.unwrap_err().code, "StructureServiceAccessDenied");
            }
            let get_res = handle_character_get_service(ctx, sess, &[PyRep::Integer(sid), PyRep::Integer(svc_id)])
                .expect("CharacterGetService");
            match expected_tax {
                Some(t) => assert_eq!(get_res, PyRep::Real(t)),
                None => assert_eq!(get_res, PyRep::None),
            }
        }
        map
    };

    let mut owner_sess = make_session(owner_id, owner_corp, 30_000_142, Some(astra_id), 1);
    let mut outsider_sess = make_session(outsider_id, outsider_corp, 30_000_142, None, 0);

    // 1. Operational cored Astrahus with zero fitted modules exposes built-ins 1, 2, 3, 8 when no profile blocks them.
    let probe_svcs = [
        prof_svc::SERVICE_DOCKING,
        prof_svc::SERVICE_FITTING,
        prof_svc::SERVICE_OFFICES,
        prof_svc::SERVICE_REPAIR,
        prof_svc::SERVICE_MARKET,
        prof_svc::SERVICE_JUMP_BRIDGE,
    ];
    let map = assert_all_rpcs_agree(&ctx, &mut owner_sess, astra_id, &probe_svcs);
    assert!(map.contains_key(&prof_svc::SERVICE_DOCKING));
    assert!(map.contains_key(&prof_svc::SERVICE_FITTING));
    assert!(map.contains_key(&prof_svc::SERVICE_OFFICES));
    assert!(map.contains_key(&prof_svc::SERVICE_REPAIR));
    assert!(!map.contains_key(&prof_svc::SERVICE_MARKET));
    assert!(!map.contains_key(&prof_svc::SERVICE_JUMP_BRIDGE));

    // Also verify fallback to session.structure_id when arg 0 is omitted.
    let session_map = handle_character_get_services(&ctx, &mut owner_sess, &[]).expect("session structure");
    assert!(matches!(session_map, PyRep::Dict(entries) if entries.len() == map.len()));

    // 2. Attached profile with domain 20 unconfigured denies SERVICE_OFFICES (3) while 1/2/8 stay accessible.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile {
            profile_id: 750_001,
            owner_corp_id: owner_corp,
            corp_only: false,
            ..Default::default()
        };
        prof.general_settings.insert(prof_svc::SETTING_HOUSING_CAN_DOCK, true);
        ext.profiles_by_id.insert(750_001, prof.clone());
        ext.access_profiles.insert(astra_id, prof);
        save_ext_state(snap, &ext);
    });
    let attached_unconfigured = assert_all_rpcs_agree(&ctx, &mut outsider_sess, astra_id, &probe_svcs);
    assert!(attached_unconfigured.contains_key(&prof_svc::SERVICE_DOCKING));
    assert!(attached_unconfigured.contains_key(&prof_svc::SERVICE_FITTING));
    assert!(attached_unconfigured.contains_key(&prof_svc::SERVICE_REPAIR));
    assert!(!attached_unconfigured.contains_key(&prof_svc::SERVICE_OFFICES));

    // Configuring domain 20 enables SERVICE_OFFICES (3); setting domain 20 = false denies it.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let prof = ext.access_profiles.get_mut(&astra_id).unwrap();
        prof.general_settings.insert(prof_svc::SETTING_CORP_RENT_OFFICE, true);
        save_ext_state(snap, &ext);
    });
    let offices_enabled = assert_all_rpcs_agree(&ctx, &mut outsider_sess, astra_id, &probe_svcs);
    assert!(offices_enabled.contains_key(&prof_svc::SERVICE_OFFICES));

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let prof = ext.access_profiles.get_mut(&astra_id).unwrap();
        prof.general_settings.insert(prof_svc::SETTING_CORP_RENT_OFFICE, false);
        save_ext_state(snap, &ext);
    });
    let offices_disabled = assert_all_rpcs_agree(&ctx, &mut outsider_sess, astra_id, &probe_svcs);
    assert!(!offices_disabled.contains_key(&prof_svc::SERVICE_OFFICES));

    // 3. Online fitted Standup Market Hub I (35892) appears alongside built-ins.
    ctx.db.with_mut_snapshot(|snap| {
        let mid = 750_901_i64;
        snap.items.insert(
            mid,
            InventoryItemRecord {
                item_id: mid,
                type_id: 35_892,
                owner_id: owner_corp,
                location_id: astra_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mid, true);
    });
    let with_market = assert_all_rpcs_agree(&ctx, &mut outsider_sess, astra_id, &probe_svcs);
    assert!(with_market.contains_key(&prof_svc::SERVICE_MARKET));

    // 4. Un-cored operational Astrahus exposes SERVICE_DOCKING (1) only; 2/3/8 remain offline/denied.
    let uncored_map = assert_all_rpcs_agree(&ctx, &mut owner_sess, uncored_id, &probe_svcs);
    assert_eq!(uncored_map.keys().copied().collect::<Vec<_>>(), vec![prof_svc::SERVICE_DOCKING]);

    // 5. Destroyed / NoCore Astrahus structures expose empty service dicts and deny all built-ins.
    for dead_id in [destroyed_id, nocore_id] {
        let dead_map = assert_all_rpcs_agree(&ctx, &mut owner_sess, dead_id, &probe_svcs);
        assert!(dead_map.is_empty(), "non-operational structure {dead_id} must expose no services");
    }

    // 6. FLEX Ansiblex (35841) with allow-all profile exposes SERVICE_JUMP_BRIDGE (15) only.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile {
            profile_id: 750_002,
            owner_corp_id: owner_corp,
            corp_only: false,
            ..Default::default()
        };
        prof.general_settings.insert(prof_svc::SETTING_HOUSING_CAN_DOCK, true);
        prof.general_settings.insert(prof_svc::SETTING_JUMP_BRIDGE_ACTIVATION, true);
        ext.profiles_by_id.insert(750_002, prof.clone());
        ext.access_profiles.insert(jb_flex_id, prof);
        save_ext_state(snap, &ext);
    });
    let flex_map = assert_all_rpcs_agree(&ctx, &mut outsider_sess, jb_flex_id, &probe_svcs);
    assert_eq!(
        flex_map.keys().copied().collect::<Vec<_>>(),
        vec![prof_svc::SERVICE_JUMP_BRIDGE]
    );
}

#[test]
fn abandoned_repair_services_and_fitting_profile_disablement_work() {
    use crate::services::structure_profile_services as prof_svc;

    let ctx = test_ctx();
    let owner_corp = 98_755_001_i64;
    let owner_id = 755_001_i64;
    let sys_id = 30_000_142_u32;
    let sid = 1_000_000_755_001_i64;
    seed_char_and_corp(&ctx, owner_id, owner_corp, sys_id, true);

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35_832,
                owner_corp_id: owner_corp,
                solar_system_id: sys_id,
                name: "AbnRepairCitadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(sid).or_default().low_power_since_unix = Some(now - 60);
        save_ext_state(snap, &ext);
    });

    let mut owner_sess = make_session(owner_id, owner_corp, sys_id, Some(sid), 0);
    let get_services_map = |sess: &mut SessionState| -> HashMap<i64, f64> {
        let rep = handle_character_get_services(&ctx, sess, &[PyRep::Integer(sid)])
            .expect("CharacterGetServices ok");
        match rep {
            PyRep::Dict(entries) => entries
                .into_iter()
                .filter_map(|(k, v)| match (k.as_i64(), v) {
                    (Some(id), PyRep::Real(tax)) => Some((id, tax)),
                    _ => None,
                })
                .collect(),
            other => panic!("expected Dict, got {other:?}"),
        }
    };

    // Before abandonment (LowPower): CharacterGetServices exposes V24 SERVICE_REPAIR (8), and
    // HasService/CheckService/GetService expose both SERVICE_REPAIR (8) and legacy 13.
    let low_map = get_services_map(&mut owner_sess);
    assert!(
        low_map.contains_key(&prof_svc::SERVICE_REPAIR),
        "LowPower should expose SERVICE_REPAIR (8)"
    );
    for svc in [prof_svc::SERVICE_REPAIR, 13] {
        assert_eq!(
            handle_character_has_service(&ctx, &mut owner_sess, &[PyRep::Integer(sid), PyRep::Integer(svc)])
                .unwrap(),
            PyRep::Bool(true)
        );
        assert_eq!(
            handle_character_check_service(&ctx, &mut owner_sess, &[PyRep::Integer(sid), PyRep::Integer(svc)])
                .unwrap(),
            PyRep::None
        );
        assert!(matches!(
            handle_character_get_service(&ctx, &mut owner_sess, &[PyRep::Integer(sid), PyRep::Integer(svc)])
                .unwrap(),
            PyRep::Real(_)
        ));
    }

    // Once Abandoned: CharacterGetServices / HasService / CheckService / GetService reject SERVICE_REPAIR | 13,
    // agreeing with StructureServiceKind::Repair -> "StructureAbandoned", while keeping SERVICE_DOCKING online.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(sid).or_default().low_power_since_unix =
            Some(now.saturating_sub(life::ABANDON_AFTER_LOW_POWER_SECS + 60));
        save_ext_state(snap, &ext);
    });
    let abn_map = get_services_map(&mut owner_sess);
    assert!(abn_map.contains_key(&prof_svc::SERVICE_DOCKING));
    for svc in [prof_svc::SERVICE_REPAIR, 13] {
        assert!(!abn_map.contains_key(&svc), "Abandoned must omit service {svc}");
        assert_eq!(
            handle_character_has_service(&ctx, &mut owner_sess, &[PyRep::Integer(sid), PyRep::Integer(svc)])
                .unwrap(),
            PyRep::Bool(false)
        );
        assert_eq!(
            handle_character_check_service(&ctx, &mut owner_sess, &[PyRep::Integer(sid), PyRep::Integer(svc)])
                .unwrap_err()
                .code,
            "StructureServiceAccessDenied"
        );
        assert_eq!(
            handle_character_get_service(&ctx, &mut owner_sess, &[PyRep::Integer(sid), PyRep::Integer(svc)])
                .unwrap(),
            PyRep::None
        );
    }
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            prof_svc::require_structure_service_access_in_snap(
                Some(&ctx),
                snap,
                sid,
                owner_id,
                prof_svc::StructureServiceKind::Repair,
                ctx.now_unix(),
            )
            .unwrap_err()
            .code,
            "StructureAbandoned"
        );
        assert!(validate_docked_ship_fitting_facility_in_snap(&ctx, snap, sid).is_ok());
    });

    // P2-1: general_settings[&SERVICE_FITTING] = false / SETTING_HOUSING_CAN_DOCK = false
    // disables docked fitting cleanly without breaking unconfigured structures.
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile {
            profile_id: 755_101,
            owner_corp_id: owner_corp,
            corp_only: false,
            ..Default::default()
        };
        prof.general_settings.insert(prof_svc::SETTING_HOUSING_CAN_DOCK, true);
        prof.general_settings.insert(prof_svc::SERVICE_FITTING, false);
        ext.structure_profile_id.insert(sid, 755_101);
        ext.profiles_by_id.insert(755_101, prof.clone());
        ext.access_profiles.insert(sid, prof);
        save_ext_state(snap, &ext);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            validate_docked_ship_fitting_facility_in_snap(&ctx, snap, sid)
                .unwrap_err()
                .code,
            "StructureServiceAccessDenied"
        );
    });
    assert_eq!(
        handle_character_has_service(
            &ctx,
            &mut owner_sess,
            &[PyRep::Integer(sid), PyRep::Integer(prof_svc::SERVICE_FITTING)]
        )
        .unwrap(),
        PyRep::Bool(false)
    );
}

#[test]
fn are_corps_at_war_in_snap_honors_ally_expiry_and_alliance_allies() {
    use eve_db::{
        clock::VirtualServerClock, AllianceRecord, PersistedNegKind, PersistedWarNegotiation,
        ServerClock,
    };

    let ctx = test_ctx();
    let now = 1_750_000_000_u64;
    let eff = now + 100;
    let exp = now + 500;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(eff - 1));
    ctx.set_clock(clock.clone());

    let sys_id = 30_000_142_u32;
    let sid = 10_000_990_777_i64;
    let agg_corp = 98_760_001_i64;
    let def_corp = 98_760_002_i64;
    let corp_ally = 98_760_003_i64;
    let ext_alliance = 99_760_004_i64;
    let ext_member_corp = 98_760_004_i64;
    let agg_pilot = 760_001_i64;
    let corp_ally_pilot = 760_003_i64;
    let alliance_ally_pilot = 760_004_i64;
    let war_id = 700_760_001_i64;

    seed_char_and_corp(&ctx, agg_pilot, agg_corp, sys_id, true);
    seed_char_and_corp(&ctx, corp_ally_pilot, corp_ally, sys_id, true);
    seed_char_and_corp(&ctx, alliance_ally_pilot, ext_member_corp, sys_id, true);

    ctx.db.with_mut_snapshot(|snap| {
        let mut def_c = CorporationRecord::default();
        def_c.corporation_id = def_corp;
        snap.corporations.insert(def_corp, def_c);
        snap.corporations
            .get_mut(&ext_member_corp)
            .unwrap()
            .alliance_id = Some(ext_alliance);
        snap.alliances.insert(
            ext_alliance,
            AllianceRecord {
                alliance_id: ext_alliance,
                name: "Defense Ally Alliance".into(),
                short_name: "DALA".into(),
                executor_corp_id: ext_member_corp,
                member_corp_ids: [ext_member_corp].into_iter().collect(),
            },
        );
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35_832,
                owner_corp_id: agg_corp,
                solar_system_id: sys_id,
                name: "Ally Expiry Defense".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                ..Default::default()
            },
        );
        let mut ds = StructureDefenseSettings::default();
        ds.structure_id = sid;
        ds.auto_defense_enabled = true;
        ds.auto_target_war_targets = true;
        save_defense_settings(snap, &ds);

        snap.wars.insert(
            war_id,
            WarRecord {
                war_id,
                aggressor_corp_id: agg_corp,
                defender_corp_id: def_corp,
                mutual: true,
                started_unix: now,
                active: true,
            },
        );
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 31,
            war_id,
            kind: PersistedNegKind::Ally,
            from_corp: corp_ally,
            isk: IskCents::ZERO,
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(eff),
        });
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 32,
            war_id,
            kind: PersistedNegKind::Ally,
            from_corp: ext_alliance,
            isk: IskCents::ZERO,
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(eff),
        });
        snap.war_state.ally_expires_unix.insert((war_id, corp_ally), exp);
        snap.war_state
            .ally_expires_unix
            .insert((war_id, ext_alliance), exp);
    });

    // Before effective_unix (eff - 1): neither ally is at war or auto-targeted.
    ctx.db.with_snapshot(|snap| {
        assert!(!are_corps_at_war_in_snap(snap, agg_corp, corp_ally, eff - 1));
        assert!(!are_corps_at_war_in_snap(snap, agg_corp, ext_member_corp, eff - 1));
        assert!(!evaluate_auto_defense_target(&ctx, snap, sid, corp_ally_pilot));
        assert!(!evaluate_auto_defense_target(&ctx, snap, sid, alliance_ally_pilot));
    });

    // Active window [eff, exp): both corp ally and alliance-ID ally are at war and auto-targeted.
    clock.advance_secs(1);
    assert_eq!(ctx.now_unix(), eff);
    ctx.db.with_snapshot(|snap| {
        for ts in [eff, exp - 1] {
            assert!(are_corps_at_war_in_snap(snap, agg_corp, corp_ally, ts));
            assert!(are_corps_at_war_in_snap(snap, corp_ally, agg_corp, ts));
            assert!(are_corps_at_war_in_snap(snap, agg_corp, ext_member_corp, ts));
            assert!(are_corps_at_war_in_snap(snap, ext_member_corp, agg_corp, ts));
        }
        assert!(evaluate_auto_defense_target(&ctx, snap, sid, corp_ally_pilot));
        assert!(evaluate_auto_defense_target(&ctx, snap, sid, alliance_ally_pilot));
    });

    // At and after ally_expires_unix: expired allies are no longer at war or auto-targeted.
    clock.advance_secs(exp - eff);
    assert_eq!(ctx.now_unix(), exp);
    ctx.db.with_snapshot(|snap| {
        for ts in [exp, exp + 60] {
            assert!(!are_corps_at_war_in_snap(snap, agg_corp, corp_ally, ts));
            assert!(!are_corps_at_war_in_snap(snap, corp_ally, agg_corp, ts));
            assert!(!are_corps_at_war_in_snap(snap, agg_corp, ext_member_corp, ts));
            assert!(!are_corps_at_war_in_snap(snap, ext_member_corp, agg_corp, ts));
        }
        assert!(!evaluate_auto_defense_target(&ctx, snap, sid, corp_ally_pilot));
        assert!(!evaluate_auto_defense_target(&ctx, snap, sid, alliance_ally_pilot));
    });
}

#[test]
fn consume_structure_fuel_blocks_rejects_non_commodity_bay_stacks() {
    let mut ctx = test_ctx();
    let st_tid = 35_832_u32;
    let fuel_tid = 4247_u32;
    clone_type_with(&mut ctx.sde, 35_832, st_tid, 1657, 65, &[]);
    clone_type_with(&mut ctx.sde, 34, fuel_tid, 1136, 4, &[]);
    seed_char_and_corp(&ctx, 7491, 98_749, 30_000_142, true);
    let sid = 10_000_995_999_i64;
    let bad_fuel_id = 81_991_i64;
    let good_fuel_id = 81_992_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: st_tid,
                owner_corp_id: 98_749,
                solar_system_id: 30_000_142,
                name: "Fuel Policy Citadel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 30,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            bad_fuel_id,
            InventoryItemRecord {
                item_id: bad_fuel_id,
                type_id: fuel_tid,
                owner_id: 98_749,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 30,
                singleton: false,
                is_blueprint_copy: true,
                ..Default::default()
            },
        );

        assert!(!consume_structure_fuel_blocks(&ctx, snap, sid, 10));
        assert_eq!(snap.items[&bad_fuel_id].quantity, 30);
        assert_eq!(snap.structures[&sid].fuel_blocks, 30);

        snap.items.get_mut(&bad_fuel_id).unwrap().is_blueprint_copy = false;
        snap.items.get_mut(&bad_fuel_id).unwrap().singleton = true;
        assert!(!consume_structure_fuel_blocks(&ctx, snap, sid, 10));
        assert_eq!(snap.structures[&sid].fuel_blocks, 30);

        snap.items.insert(
            good_fuel_id,
            InventoryItemRecord {
                item_id: good_fuel_id,
                type_id: fuel_tid,
                owner_id: 98_749,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        assert!(!consume_structure_fuel_blocks(&ctx, snap, sid, 15));
        assert!(consume_structure_fuel_blocks(&ctx, snap, sid, 10));
        assert!(!snap.items.contains_key(&good_fuel_id));
        assert_eq!(snap.items[&bad_fuel_id].quantity, 30);
        assert_eq!(snap.structures[&sid].fuel_blocks, 0);
    });
}

#[test]
fn structure_fuel_lifecycle_and_fit_move_align_with_consume_policy() {
    let ctx = test_ctx();
    let char_id = 7501_i64;
    let corp_id = 98_750_i64;
    let sys_id = 30_000_142_u32;
    let sid = 10_000_996_100_i64;
    let svc_tid = 35_878_u32; // Standup Manufacturing Plant I
    let fuel_tid = 4247_u32;
    seed_char_and_corp(&ctx, char_id, corp_id, sys_id, true);
    let online_cost = service_module_online_fuel_cost(&ctx, 35_825, svc_tid);
    assert!(online_cost > 0);
    let per_hour = ctx
        .sde
        .types
        .get(&svc_tid)
        .and_then(|t| t.default_attributes.get(&life::ATTR_SERVICE_FUEL_PER_HOUR))
        .copied()
        .unwrap_or(0.0);
    let mod_burn = (per_hour * service_module_fuel_multiplier(&ctx, 35_825, svc_tid))
        .round()
        .max(1.0) as u32;
    assert!(mod_burn > 0);

    let bpc_fuel_id = 82_001_i64;
    let valid_fuel_id = 82_002_i64;
    let installed_svc_id = 82_003_i64;
    let to_fit_svc_id = 82_004_i64;
    let foreign_fuel_id = 82_005_i64;
    let base_now = 1_700_000_000_u64;
    let profile = life::structure_profile(&ctx, 35_825).expect("raitaru profile");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(sid);
        }
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35_825,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Fuel Alignment Raitaru".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: mod_burn + 1_000,
                shield_hp: profile.shield_hp,
                armor_hp: profile.armor_hp,
                hull_hp: profile.hull_hp,
                ..Default::default()
            },
        );
        snap.items.insert(
            installed_svc_id,
            InventoryItemRecord {
                item_id: installed_svc_id,
                type_id: svc_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(installed_svc_id, true);
        let mut ext = load_ext_state(snap);
        ext.lifecycle.insert(
            sid,
            crate::services::station_structure::structure_upwell_ops::StructureLifecycleState {
                last_fuel_tick_unix: Some(base_now),
                online_modules: vec![installed_svc_id],
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);

        snap.items.insert(
            bpc_fuel_id,
            InventoryItemRecord {
                item_id: bpc_fuel_id,
                type_id: fuel_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 1_000,
                singleton: false,
                is_blueprint_copy: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            valid_fuel_id,
            InventoryItemRecord {
                item_id: valid_fuel_id,
                type_id: fuel_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: mod_burn as i32,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            to_fit_svc_id,
            InventoryItemRecord {
                item_id: to_fit_svc_id,
                type_id: svc_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            foreign_fuel_id,
            InventoryItemRecord {
                item_id: foreign_fuel_id,
                type_id: fuel_tid,
                owner_id: corp_id + 99,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );

        let st = snap.structures[&sid].clone();
        assert_eq!(available_structure_fuel_blocks(&ctx, snap, sid), mod_burn);
        assert_eq!(
            life::estimated_fuel_expires_unix(&ctx, snap, &st, base_now),
            Some(base_now + 3_600)
        );
    });

    // Advance 5 hours: only 1 hour (`mod_burn`) is affordable; 1000 BPC blocks and foreign-owner blocks must NOT inflate `affordable`
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let _ = life::sync_structure(&ctx, snap, &mut ext, sid, base_now + 5 * 3_600);
        assert!(!snap.items.contains_key(&valid_fuel_id));
        assert_eq!(snap.items[&bpc_fuel_id].quantity, 1_000);
        assert_eq!(snap.items[&foreign_fuel_id].quantity, 500);
        assert_eq!(snap.structures[&sid].fuel_blocks, 0);
        assert_eq!(snap.module_online_states.get(&installed_svc_id), Some(&false));
        assert_eq!(
            ext.lifecycle.get(&sid).and_then(|l| l.low_power_since_unix),
            Some(base_now + 2 * 3_600)
        );
        save_ext_state(snap, &ext);
    });

    // Stale `st.fuel_blocks = 500` alongside BPC bay stack must still be treated as 0 blocks in fit_move
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().fuel_blocks = 500;
    });
    let mut sess = make_session(char_id, corp_id, sys_id, Some(sid), u64::MAX);
    let err_fit = crate::services::station_structure::structure_lifecycle_rpc::handle_structure_fit_move(
        &ctx,
        &mut sess,
        to_fit_svc_id,
        sid,
        InventoryFlag::ServiceSlot1,
        Some(true),
    )
    .expect_err("explicit online with only BPC bay blocks must fail");
    assert_eq!(err_fit.code, "InsufficientStructureFuelToOnline");

    let ok_fit = crate::services::station_structure::structure_lifecycle_rpc::handle_structure_fit_move(
        &ctx,
        &mut sess,
        to_fit_svc_id,
        sid,
        InventoryFlag::ServiceSlot1,
        None,
    )
    .expect("implicit online fits offline instead of free onlining");
    assert_eq!(ok_fit.dict_get("online"), Some(&PyRep::Bool(false)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.module_online_states.get(&to_fit_svc_id), Some(&false));
    });
}

#[test]
fn structure_no_core_skips_bpc_core_and_installs_valid_sibling() {
    let ctx = test_ctx();
    let corp_id = 98_751_i64;
    let sid = 10_000_996_200_i64;
    let core_tid = life::structure_profile(&ctx, 35_832)
        .and_then(|p| p.core_type_id)
        .expect("Astrahus core type");
    let bpc_core_id = 82_101_i64;
    let good_core_id = 82_102_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Core Test Citadel".into(),
                position: [0.0, 0.0, 0.0],
                state: life::STATE_NO_CORE.into(),
                quantum_core_installed: false,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            bpc_core_id,
            InventoryItemRecord {
                item_id: bpc_core_id,
                type_id: core_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::StructureDeedBay,
                quantity: 1,
                singleton: false,
                is_blueprint_copy: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            good_core_id,
            InventoryItemRecord {
                item_id: good_core_id,
                type_id: core_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::StructureDeedBay,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );

        let mut ext = load_ext_state(snap);
        let _ = life::sync_structure(&ctx, snap, &mut ext, sid, 1_700_000_000);
        assert!(snap.structures[&sid].quantum_core_installed);
        assert!(snap.items.contains_key(&bpc_core_id));
        assert!(!snap.items.contains_key(&good_core_id));
    });
}

#[cfg(test)]
#[path = "structure_acl_defense_rules_extra_unit_tests.rs"]
mod extra_unit_tests;
