#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::mobile_deployables::{MobileDeployableRecord, write_deployable_state_snap};
use crate::services::station_structure::structure_upwell_ops::{
    AccessGroupRecord, StructureAccessProfile, load_ext_state, save_ext_state,
};
use eve_db::types::WarRecord;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, SocialContactRecord,
    UpwellStructureRecord, corp_roles,
};
use eve_machonet::{SessionState, service_roles::ROLE_GMH};
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

fn clone_type(sde: &mut Arc<SdeRegistry>, base_tid: u32, new_tid: u32, group_id: u32, category_id: u32, attrs: &[(u16, f64)]) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&base_tid)
        .cloned()
        .unwrap_or_else(|| reg.types.values().next().unwrap().clone());
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn seed_char(ctx: &ServerContext, char_id: i64, corp_id: i64, ally_id: Option<i64>, is_ceo: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.entry(corp_id).or_insert_with(|| { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = format!("Corp {corp_id}"); __corp.ticker = "EDG".into(); __corp.ceo_id = if is_ceo { char_id } else { char_id + 900 }; __corp.alliance_id = ally_id; __corp.hq_station_id = Some(60_003_760); __corp.members = HashSet::from([char_id]); __corp.ensure_wallet_divisions(); __corp });
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = corp_id;
        ch.alliance_id = ally_id;
        ch.solar_system_id = 30_000_142;
        snap.characters.insert(char_id, ch);
    });
}

fn sess_for(char_id: i64, corp_id: i64, st_id: Option<i64>, role: u64, is_gm: bool) -> SessionState {
    let mut s = SessionState::new(8601);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30_000_142);
    s.structure_id = st_id;
    s.corp_role = role;
    if is_gm {
        s.role |= ROLE_GMH;
    }
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acl_role_helpers_cross_group_sync_tuple_edit_and_gm_views() {
    let ctx = test_ctx();
    let c_creator = 8901_i64;
    let c_corp_mate = 8902_i64;
    let c_target = 8903_i64;
    seed_char(&ctx, c_creator, 98_950, None, true);
    seed_char(&ctx, c_corp_mate, 98_950, None, false);
    seed_char(&ctx, c_target, 98_951, None, true);

    assert!(is_acl_role_excluded(-1));
    assert!(is_acl_role_excluded(ACL_ROLE_EXCLUDED));
    assert!(!is_acl_role_excluded(ACL_ROLE_MEMBER));
    assert!(is_acl_role_allowed(ACL_ROLE_ADMIN));
    assert!(!is_acl_role_allowed(ACL_ROLE_EXCLUDED));
    assert_eq!(normalize_acl_role_value(-1).unwrap(), ACL_ROLE_EXCLUDED);
    assert_eq!(normalize_acl_role_value(99).unwrap_err().code, "InvalidAclRole");

    let g1 = 420_001_i64;
    let g2 = 420_002_i64;
    let pid = 520_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.owner_groups.insert(
            g1,
            AccessGroupRecord {
                group_id: g1,
                creator_id: c_creator,
                owner_corp_id: 98_950,
                name: "G1".into(),
                description: "D1".into(),
                members: HashMap::from([
                    (c_creator, ACL_ROLE_ADMIN),
                    (c_corp_mate, ACL_ROLE_EXCLUDED),
                    (c_target, ACL_ROLE_MEMBER),
                ]),
            },
        );
        ext.owner_groups.insert(
            g2,
            AccessGroupRecord {
                group_id: g2,
                creator_id: c_creator,
                owner_corp_id: 0, // Personal group
                name: "G2".into(),
                description: "D2".into(),
                members: HashMap::from([(c_creator, ACL_ROLE_ADMIN), (c_target, ACL_ROLE_EXCLUDED)]),
            },
        );
        let mut prof = StructureAccessProfile {
            profile_id: pid,
            owner_corp_id: 98_950,
            name: "Prof".into(),
            ..Default::default()
        };
        prof.access_group_ids.insert(g1);
        prof.access_group_ids.insert(g2);
        prof.allowed_characters.insert(c_target);
        prof.blocked_characters.insert(c_target);
        ext.profiles_by_id.insert(pid, prof);

        // Cross-group exclusion on g2 must keep blocked when g1 sets None; entity_id=0 no-op
        sync_profile_docking_entity_after_group_edit(&mut ext, pid, g1, 0, false, false, None);
        sync_profile_docking_entity_after_group_edit(&mut ext, pid, g1, c_target, false, false, None);
        assert!(ext.profiles_by_id[&pid].blocked_characters.contains(&c_target));
        assert!(!ext.profiles_by_id[&pid].allowed_characters.contains(&c_target));
        save_ext_state(snap, &ext);
    });

    let mut creator_sess = sess_for(c_creator, 98_950, None, corp_roles::ROLE_DIRECTOR, false);
    let mate_sess = sess_for(c_corp_mate, 98_950, None, 0, false);
    let gm_sess = sess_for(c_target, 98_951, None, 0, true);

    // Explicitly excluded corp mate cannot view GetGroup or GetMembers, while GM can
    assert_eq!(
        handle_get_group(&ctx, &mate_sess, &[PyRep::Integer(g1)]).unwrap_err().code,
        "AccessGroupAccessDenied"
    );
    assert_eq!(
        handle_get_members(&ctx, &mate_sess, &[PyRep::Integer(g1)]).unwrap_err().code,
        "AccessGroupAccessDenied"
    );
    assert!(handle_get_group(&ctx, &gm_sess, &[PyRep::Integer(g1)]).is_ok());

    // UpdateMembershipTypes with Tuple + Dict + missing args + EditGroup via UpdateGroup alias
    assert_eq!(
        handle_set_member_role(&ctx, &creator_sess, &[PyRep::Integer(g1), PyRep::Tuple(vec![])]).unwrap_err().code,
        "MissingEntityId"
    );
    assert_eq!(
        handle_set_member_role(
            &ctx,
            &creator_sess,
            &[PyRep::Integer(g1), PyRep::Tuple(vec![PyRep::Integer(c_target)]), PyRep::Integer(ACL_ROLE_MEMBER)],
        )
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(handle_edit_group(&ctx, &creator_sess, &[]).unwrap_err().code, "MissingGroupId");
    let kw = HashMap::new();
    assert!(handle(&ctx, &mut creator_sess, "ownerGroupManager", "UpdateGroup", &[PyRep::Integer(g1), PyRep::String("NewG1".into())], &kw).await.is_some());
    assert!(handle(&ctx, &mut creator_sess, "ownerGroupManager", "GetMembers", &[PyRep::Integer(g1)], &kw).await.is_some());
    assert!(handle(&ctx, &mut creator_sess, "ownerGroupManager", "GetLocalCampaignClientSnapshot", &[], &kw).await.is_none());
}

#[test]
fn auto_defense_wormhole_legacy_alliance_standing_and_cyno_edges() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let (owner, target) = (8910_i64, 8911_i64);
    seed_char(&ctx, owner, 98_960, Some(99_960), true);
    seed_char(&ctx, target, 98_961, None, true);
    let sid_wh = 10_000_998_001_i64;
    let sid_off = 10_000_998_002_i64;

    let wh_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id >= 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(31_000_005);

    ctx.db.with_mut_snapshot(|snap| {
        assert!(!evaluate_auto_defense_target(&ctx, snap, 999_888, target));
        snap.structures.insert(
            sid_wh,
            UpwellStructureRecord {
                structure_id: sid_wh,
                type_id: 35832,
                owner_corp_id: 98_960,
                solar_system_id: wh_sys,
                name: "WH".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 10,
                ..Default::default()
            },
        );
        snap.structures.insert(
            sid_off,
            UpwellStructureRecord {
                structure_id: sid_off,
                type_id: 35832,
                owner_corp_id: 98_960,
                solar_system_id: wh_sys,
                name: "Off".into(),
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
        // Alliance contact negative standing fallback + legacy defense settings
        snap.alliance_contacts.insert(
            99_960,
            vec![SocialContactRecord {
                contact_id: target,
                contact_type: "character".into(),
                standing: -4.0,
                is_watched: false,
                is_blocked: false,
                label_ids: Vec::new(),
            }],
        );
        let mut ds = StructureDefenseSettings {
            structure_id: sid_wh,
            auto_defense_enabled: true,
            standing_threshold: -1.0,
            ..Default::default()
        };
        save_defense_settings(snap, &ds);
        assert!(!evaluate_auto_defense_target(&ctx, snap, sid_off, target));
        assert!(evaluate_auto_defense_target(&ctx, snap, sid_wh, target));

        ds.auto_defense_enabled = false;
        save_defense_settings(snap, &ds);
        assert!(!evaluate_auto_defense_target(&ctx, snap, sid_wh, target));

        // Wars edge cases
        assert!(!are_corps_at_war_in_snap(snap, 0, 98_960, now));
        assert!(!are_corps_at_war_in_snap(snap, 98_960, 98_960, now));
        snap.wars.insert(
            55,
            WarRecord {
                war_id: 55,
                aggressor_corp_id: 98_960,
                defender_corp_id: 98_961,
                mutual: true,
                started_unix: now - 100,
                active: false,
            },
        );
        assert!(!are_corps_at_war_in_snap(snap, 98_960, 98_961, now));

        // Unanchored cyno inhibitor returns false
        let inhib = 780_001_i64;
        snap.items.insert(
            inhib,
            InventoryItemRecord {
                item_id: inhib,
                type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id: owner,
                location_id: i64::from(wh_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                ..Default::default()
            },
        );
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                inhib,
                MobileDeployableRecord {
                    item_id: inhib,
                    type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                    group_id: 0,
                    owner_id: owner,
                    solar_system_id: wh_sys,
                    position: [0.0, 0.0, 0.0],
                    anchor_start_unix: now - 500,
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    expires_at_unix: now + 3600,
                    last_interaction_unix: now,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    is_unanchored: true,
                    deployed_fleet_id: None,
                },
            );
        });
        assert!(!is_cyno_inhibited(&ctx, snap, wh_sys, None));
    });
}

#[test]
fn fuel_bay_edge_branches_disposal_and_character_service_auth_guards() {
    let mut ctx = test_ctx();
    let (owner, outsider) = (8920_i64, 8921_i64);
    seed_char(&ctx, owner, 98_970, None, true);
    seed_char(&ctx, outsider, 98_971, None, false);

    let st_tid = 941_001_u32;
    let svc_zero_tid = 941_002_u32;
    clone_type(&mut ctx.sde, 35_832, st_tid, 1657, 65, &[]);
    clone_type(&mut ctx.sde, 35_832, svc_zero_tid, 1321, 66, &[]);
    assert_eq!(service_module_online_fuel_cost(&ctx, st_tid, svc_zero_tid), 0);
    assert!(is_valid_structure_fuel_bay_item(&ctx, 16_273));
    assert!(is_structure_fuel_flag(InventoryFlag::SpecializedFuelBay));

    let sid = 10_000_999_001_i64;
    let fuel_tid = 4247_u32;
    let ship_id = 9_920_001_i64;
    let empty_pos = HashMap::new();
    let sess = sess_for(owner, 98_970, Some(sid), corp_roles::ROLE_DIRECTOR, false);
    let mut unauth = SessionState::new(8602);

    ctx.db.with_mut_snapshot(|snap| {
        // Missing structure
        assert_eq!(
            validate_structure_fuel_bay_transfer(&ctx, snap, &sess, owner, sid, fuel_tid, true, sid, 30_000_142, 0, &empty_pos)
                .unwrap_err()
                .code,
            "StructureNotFound"
        );
        materialize_structure_fuel_for_disposal(&ctx, snap, sid);

        // NPC-owned structure allows deposit without matching corp, while Unanchored fails
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: st_tid,
                owner_corp_id: 1_000_035,
                solar_system_id: 30_000_142,
                state: "Unanchored".into(),
                fuel_blocks: 35,
                ..Default::default()
            },
        );
        assert_eq!(
            validate_structure_fuel_bay_transfer(&ctx, snap, &sess, owner, sid, fuel_tid, true, sid, 30_000_142, 0, &empty_pos)
                .unwrap_err()
                .code,
            "StructureNotOperational"
        );
        snap.structures.get_mut(&sid).unwrap().state = "ShieldVulnerable".into();
        // Ship in space within 5,000m using item_positions
        let mut pos_map = HashMap::new();
        pos_map.insert(ship_id, (DVec3::new(3_000.0, 0.0, 0.0), 50.0));
        assert!(validate_structure_fuel_bay_transfer(&ctx, snap, &sess, owner, sid, fuel_tid, true, 0, 30_000_142, ship_id, &pos_map).is_ok());

        // Withdrawal out of range returns ItemOwnershipViolation
        assert_eq!(
            validate_structure_fuel_bay_transfer(&ctx, snap, &sess, owner, sid, fuel_tid, false, 0, 30_000_142, 0, &empty_pos)
                .unwrap_err()
                .code,
            "ItemOwnershipViolation"
        );

        // Multi-stack fuel consumption removes depleted stack, leaves remainder, then materialize creates 4247 for unbacked blocks
        snap.items.insert(
            92_001,
            InventoryItemRecord {
                item_id: 92_001,
                type_id: fuel_tid,
                owner_id: 1_000_035,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 10,
                ..Default::default()
            },
        );
        snap.items.insert(
            92_002,
            InventoryItemRecord {
                item_id: 92_002,
                type_id: fuel_tid,
                owner_id: 1_000_035,
                location_id: sid,
                flag: InventoryFlag::StructureFuel,
                quantity: 15,
                ..Default::default()
            },
        );
        assert!(consume_structure_fuel_blocks(&ctx, snap, sid, 15));
        assert!(!snap.items.contains_key(&92_001));
        assert_eq!(snap.items[&92_002].quantity, 10);
        assert_eq!(snap.structures[&sid].fuel_blocks, 10);

        snap.structures.get_mut(&sid).unwrap().fuel_blocks = 30;
        materialize_structure_fuel_for_disposal(&ctx, snap, sid);
        assert!(snap.items.values().any(|i| i.location_id == sid && i.type_id == 4247 && i.quantity == 20));
    });

    // Character service ACL auth & arg guards
    let mut sess_mut = sess.clone();
    assert!(handle_character_get_services(&ctx, &mut unauth, &[PyRep::Integer(sid)]).is_err());
    assert_eq!(
        handle_character_get_services(&ctx, &mut sess_mut, &[PyRep::Integer(999_999_888)]).unwrap_err().code,
        "StructureNotFound"
    );
    assert_eq!(
        handle_character_has_service(&ctx, &mut sess_mut, &[PyRep::Integer(1)]).unwrap(),
        PyRep::Bool(false)
    );
    let no_st_sess = sess_for(owner, 98_970, None, 0, false);
    let mut no_st_mut = no_st_sess.clone();
    assert_eq!(
        handle_character_get_services(&ctx, &mut no_st_mut, &[]).unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_character_check_service(&ctx, &mut no_st_mut, &[PyRep::Integer(1)]).unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_character_check_service(&ctx, &mut sess_mut, &[PyRep::Integer(sid), PyRep::Integer(999)])
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );
}
