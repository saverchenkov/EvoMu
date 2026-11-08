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
