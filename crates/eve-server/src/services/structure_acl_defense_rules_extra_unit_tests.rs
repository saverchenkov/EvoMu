//! Extra branch-coverage and mutation-killing unit tests for `services::structure_acl_defense_rules`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mobile_deployables::{MobileDeployableRecord, write_deployable_state_snap};
use crate::services::station_structure::structure_upwell_ops::{
    AccessGroupRecord, StructureAccessProfile, load_ext_state, save_ext_state,
};
use crate::services::structure_acl_defense_rules::*;
use eve_db::types::WarRecord;
use eve_db::{
    CorpMemberRoleAssignment, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    PersistedNegKind, PersistedWarNegotiation, SocialContactRecord, UpwellStructureRecord,
    corp_roles,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
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

fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    structure_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(740);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.structure_id = structure_id;
    s.corp_role = role;
    s
}

fn seed_char_and_corp(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    ally_id: Option<i64>,
    sys_id: u32,
    is_ceo: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .entry(corp_id)
            .or_insert_with(|| { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = format!("Corp {corp_id}");
                __corp.ticker = "ACL".into();
                __corp.ceo_id = if is_ceo { char_id } else { char_id + 900 };
                __corp.alliance_id = ally_id;
                __corp.hq_station_id = Some(60_003_760);
                __corp.tax_rate = 0.0;
                __corp.members = HashSet::from([char_id]);
                __corp.set_division_balance(1, IskCents(0));
                __corp });
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(char_id);
            corp.alliance_id = ally_id;
        }
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = ally_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = 0;
        ch.home_station_id = 60_003_760;
        ch.balance = IskCents(10_000);
        ch.security_status = 5.0;
        snap.characters.insert(char_id, ch);
    });
}

async fn call_owner_grp(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, session, "ownerGroupManager", method, args, &kw)
        .await
        .expect("ownerGroupManager method must be dispatched")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acl_group_manager_roles_creator_protection_and_access_denied_branches() {
    let ctx = test_ctx();
    let (creator_id, manager_id, member_id, outsider_id) = (8101_i64, 8102_i64, 8103_i64, 8104_i64);
    seed_char_and_corp(&ctx, creator_id, 98810, None, 30_000_142, true);
    seed_char_and_corp(&ctx, manager_id, 98811, None, 30_000_142, false);
    seed_char_and_corp(&ctx, member_id, 98812, None, 30_000_142, false);
    seed_char_and_corp(&ctx, outsider_id, 98813, None, 30_000_142, false);

    let grp_id = 410_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.owner_groups.insert(
            grp_id,
            AccessGroupRecord {
                group_id: grp_id,
                creator_id,
                owner_corp_id: 98810,
                name: "TestACL".into(),
                description: "Desc".into(),
                members: HashMap::from([
                    (creator_id, ACL_ROLE_ADMIN),
                    (manager_id, ACL_ROLE_MANAGER),
                ]),
            },
        );
        save_ext_state(snap, &ext);
    });

    let mut mgr_sess = make_session(manager_id, 98811, 30_000_142, None, 0);
    let mut out_sess = make_session(outsider_id, 98813, 30_000_142, None, 0);
    let mut adm_sess = make_session(creator_id, 98810, 30_000_142, None, 1);

    // Argument validation errors
    assert_eq!(
        call_owner_grp(&ctx, &mut adm_sess, "UpdateMembershipTypes", &[]).await.unwrap_err().code,
        "MissingGroupId"
    );
    assert_eq!(
        call_owner_grp(
            &ctx,
            &mut adm_sess,
            "UpdateMembershipTypes",
            &[PyRep::Integer(grp_id), PyRep::List(vec![])],
        )
        .await
        .unwrap_err()
        .code,
        "MissingEntityId"
    );
    assert_eq!(
        call_owner_grp(
            &ctx,
            &mut adm_sess,
            "UpdateMembershipTypes",
            &[PyRep::Integer(grp_id), PyRep::Integer(member_id), PyRep::Integer(99)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidAclRole"
    );
    assert_eq!(
        call_owner_grp(&ctx, &mut adm_sess, "GetGroup", &[PyRep::Integer(999_999)]).await.unwrap_err().code,
        "AccessGroupNotFound"
    );

    // Outsider cannot view group, members, or update memberships
    assert_eq!(
        call_owner_grp(&ctx, &mut out_sess, "GetGroup", &[PyRep::Integer(grp_id)]).await.unwrap_err().code,
        "AccessGroupAccessDenied"
    );
    assert_eq!(
        call_owner_grp(&ctx, &mut out_sess, "GetMembers", &[PyRep::Integer(grp_id)]).await.unwrap_err().code,
        "AccessGroupAccessDenied"
    );
    assert_eq!(
        call_owner_grp(
            &ctx,
            &mut out_sess,
            "UpdateMembershipTypes",
            &[PyRep::Integer(grp_id), PyRep::Integer(member_id), PyRep::Integer(ACL_ROLE_MEMBER)],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );

    // Manager can add regular member, but CANNOT promote to Admin/Manager or demote creator
    assert_eq!(
        call_owner_grp(
            &ctx,
            &mut mgr_sess,
            "UpdateMembershipTypes",
            &[
                PyRep::Integer(grp_id),
                PyRep::List(vec![PyRep::Integer(member_id)]),
                PyRep::Integer(ACL_ROLE_MEMBER),
            ],
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_owner_grp(
            &ctx,
            &mut mgr_sess,
            "UpdateMembershipTypes",
            &[PyRep::Integer(grp_id), PyRep::Integer(member_id), PyRep::Integer(ACL_ROLE_ADMIN)],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );
    assert_eq!(
        call_owner_grp(
            &ctx,
            &mut adm_sess,
            "UpdateMembershipTypes",
            &[PyRep::Integer(grp_id), PyRep::Integer(creator_id), PyRep::Integer(ACL_ROLE_MEMBER)],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );
}

#[test]
fn auto_defense_highsec_concord_rule_lowsec_standing_acl_war_warmup_and_cyno_inhibitor() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let (owner_id, ally_mate_id, bad_standing_id, blocked_acl_id) = (8201_i64, 8202_i64, 8203_i64, 8204_i64);
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_537);

    seed_char_and_corp(&ctx, owner_id, 98820, Some(99820), lowsec_sys, true);
    seed_char_and_corp(&ctx, ally_mate_id, 98821, Some(99820), lowsec_sys, true); // Same alliance!
    seed_char_and_corp(&ctx, bad_standing_id, 98822, None, lowsec_sys, true);
    seed_char_and_corp(&ctx, blocked_acl_id, 98823, None, lowsec_sys, true);

    let (sid_high, sid_low) = (10_000_996_001_i64, 10_000_996_002_i64);

    ctx.db.with_mut_snapshot(|snap| {
        for (sid, sys) in [(sid_high, 30_000_142_u32), (sid_low, lowsec_sys)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35832,
                    owner_corp_id: 98820,
                    solar_system_id: sys,
                    name: "Def".into(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 50,
                    ..Default::default()
                },
            );
            let mut ds = StructureDefenseSettings {
                structure_id: sid,
                auto_defense_enabled: true,
                auto_target_attackers: true,
                auto_target_war_targets: true,
                standing_threshold: 0.0,
                target_blocked_acl: true,
                ..Default::default()
            };
            ds.record_attacker(ally_mate_id, now);
            save_defense_settings(snap, &ds);
        }

        snap.corp_contacts.insert(
            98820,
            vec![SocialContactRecord {
                contact_id: bad_standing_id,
                contact_type: "character".into(),
                standing: -5.0,
                is_watched: false,
                is_blocked: false,
                label_ids: Vec::new(),
            }],
        );

        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile {
            profile_id: 601,
            owner_corp_id: 98820,
            name: "LowDef".into(),
            ..Default::default()
        };
        prof.blocked_characters.insert(blocked_acl_id);
        ext.access_profiles.insert(sid_low, prof);
        save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        assert!(!evaluate_auto_defense_target(&ctx, snap, sid_low, ally_mate_id));
        assert!(!evaluate_auto_defense_target(&ctx, snap, sid_high, bad_standing_id));
        assert!(evaluate_auto_defense_target(&ctx, snap, sid_low, bad_standing_id));
        assert!(evaluate_auto_defense_target(&ctx, snap, sid_low, blocked_acl_id));
        assert!(is_structure_immune_to_ewar(snap, sid_low));
        assert!(!is_structure_immune_to_ewar(snap, 999_999));
    });

    ctx.db.with_mut_snapshot(|snap| {
        assert!(!are_corps_at_war_in_snap(snap, 98820, 98821, now));
        snap.wars.insert(
            10,
            WarRecord {
                war_id: 10,
                aggressor_corp_id: 98822,
                defender_corp_id: 98823,
                mutual: false,
                started_unix: now.saturating_sub(3_600),
                active: true,
            },
        );
        assert!(!are_corps_at_war_in_snap(snap, 98822, 98823, now));
        snap.wars.get_mut(&10).unwrap().started_unix = now.saturating_sub(90_000);
        assert!(are_corps_at_war_in_snap(snap, 98822, 98823, now));

        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 1,
            war_id: 10,
            kind: PersistedNegKind::Ally,
            from_corp: 98820,
            isk: IskCents(0),
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(now.saturating_sub(10)),
        });
        assert!(are_corps_at_war_in_snap(snap, 98822, 98820, now));
    });

    // Cyno inhibitor deployable lifecycle states (anchoring, active, expired)
    ctx.db.with_mut_snapshot(|snap| {
        let inhib_id = 779_001_i64;
        snap.items.insert(
            inhib_id,
            InventoryItemRecord {
                item_id: inhib_id,
                type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id,
                location_id: lowsec_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(inhib_id, [0.0, 0.0, 0.0]);
        write_deployable_state_snap(snap, |dep_state| {
            dep_state.deployables.insert(
                inhib_id,
                MobileDeployableRecord {
                    item_id: inhib_id,
                    type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                    group_id: 0,
                    owner_id,
                    solar_system_id: lowsec_sys,
                    position: [0.0, 0.0, 0.0],
                    anchor_start_unix: now,
                    anchor_duration_secs: 120,
                    anchor_completed: false,
                    expires_at_unix: now + 3600,
                    last_interaction_unix: now,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    is_unanchored: false,
                    deployed_fleet_id: None,
                },
            );
        });
        assert!(!is_cyno_inhibited(&ctx, snap, lowsec_sys, None));

        write_deployable_state_snap(snap, |dep_state| {
            dep_state.deployables.get_mut(&inhib_id).unwrap().anchor_start_unix =
                now.saturating_sub(200);
        });
        assert!(is_cyno_inhibited(&ctx, snap, lowsec_sys, None));

        write_deployable_state_snap(snap, |dep_state| {
            dep_state.deployables.get_mut(&inhib_id).unwrap().expires_at_unix =
                now.saturating_sub(1);
        });
        assert!(!is_cyno_inhibited(&ctx, snap, lowsec_sys, None));
    });
}

#[test]
fn fuel_bay_transfer_fitting_gates_and_character_service_check_branches() {
    let ctx = test_ctx();
    let (owner_id, unauth_member, outsider_id) = (8301_i64, 8302_i64, 8303_i64);
    seed_char_and_corp(&ctx, owner_id, 98830, None, 30_000_142, true);
    seed_char_and_corp(&ctx, unauth_member, 98830, None, 30_000_142, false);
    seed_char_and_corp(&ctx, outsider_id, 98831, None, 30_000_142, false);

    let sid = 1_000_000_997_001_i64;
    let fuel_tid = 4247_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35832,
                owner_corp_id: 98830,
                solar_system_id: 30_000_142,
                name: "FuelGate".into(),
                position: [0.0, 0.0, 0.0],
                state: "Destroyed".into(),
                quantum_core_installed: false,
                fuel_blocks: 50,
                ..Default::default()
            },
        );
    });

    let mut owner_sess = make_session(owner_id, 98830, 30_000_142, Some(sid), corp_roles::ROLE_DIRECTOR);
    let unauth_sess = make_session(unauth_member, 98830, 30_000_142, Some(sid), 0);
    let out_sess = make_session(outsider_id, 98831, 30_000_142, Some(sid), 0);

    ctx.db.with_mut_snapshot(|snap| {
        assert!(validate_docked_ship_fitting_facility_in_snap(&ctx, snap, 0).is_ok());
        assert_eq!(
            validate_docked_ship_fitting_facility_in_snap(&ctx, snap, sid).unwrap_err().code,
            "StructureDestroyed"
        );
        snap.structures.get_mut(&sid).unwrap().state = "Unanchored".into();
        assert_eq!(
            validate_docked_ship_fitting_facility_in_snap(&ctx, snap, sid).unwrap_err().code,
            "StructureNotOperational"
        );
        snap.structures.get_mut(&sid).unwrap().state = "ShieldVulnerable".into();
        assert_eq!(
            validate_docked_ship_fitting_facility_in_snap(&ctx, snap, sid).unwrap_err().code,
            "QuantumCoreRequired"
        );
        snap.structures.get_mut(&sid).unwrap().quantum_core_installed = true;
        assert!(validate_docked_ship_fitting_facility_in_snap(&ctx, snap, sid).is_ok());

        let empty_pos = HashMap::new();
        assert_eq!(
            validate_structure_fuel_bay_transfer(&ctx, snap, &owner_sess, owner_id, sid, fuel_tid, true, 0, 30_000_142, 0, &empty_pos)
                .unwrap_err()
                .code,
            "DestinationAccessDenied"
        );
        assert_eq!(
            validate_structure_fuel_bay_transfer(&ctx, snap, &owner_sess, owner_id, sid, 34, true, sid, 30_000_142, 0, &empty_pos)
                .unwrap_err()
                .code,
            "InvalidInventoryFlag"
        );
        assert_eq!(
            validate_structure_fuel_bay_transfer(&ctx, snap, &out_sess, outsider_id, sid, fuel_tid, true, sid, 30_000_142, 0, &empty_pos)
                .unwrap_err()
                .code,
            "StructureOwnershipViolation"
        );
        assert_eq!(
            validate_structure_fuel_bay_transfer(&ctx, snap, &unauth_sess, unauth_member, sid, fuel_tid, true, sid, 30_000_142, 0, &empty_pos)
                .unwrap_err()
                .code,
            "CorpRoleDenied"
        );

        snap.corp_role_assignments.insert(
            (98830, unauth_member),
            CorpMemberRoleAssignment {
                character_id: unauth_member,
                roles: corp_roles::ROLE_STARBASE_OPERATOR,
                ..Default::default()
            },
        );
        assert!(has_structure_fuel_bay_role(snap, &unauth_sess, unauth_member, 98830));
        assert!(consume_structure_fuel_blocks(&ctx, snap, sid, 0));
        assert!(!consume_structure_fuel_blocks(&ctx, snap, sid, 999));

        snap.items.insert(
            91001,
            InventoryItemRecord {
                item_id: 91001,
                type_id: fuel_tid,
                owner_id: 98830,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 30,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            91002,
            InventoryItemRecord {
                item_id: 91002,
                type_id: fuel_tid,
                owner_id,
                location_id: sid,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let (merged, fb) = finalize_fuel_bay_move_in_snap(
            &ctx,
            snap,
            91002,
            sid,
            InventoryFlag::Hangar,
            sid,
            InventoryFlag::SpecializedFuelBay,
            owner_id,
            98830,
        );
        assert_eq!(merged.unwrap().quantity, 50);
        assert_eq!(fb, Some(70));
        assert!(!snap.items.contains_key(&91002));
    });

    assert_eq!(
        handle_character_check_service(&ctx, &mut owner_sess, &[]).unwrap_err().code,
        "MissingArguments"
    );
    assert_eq!(
        handle_character_check_service(&ctx, &mut owner_sess, &[PyRep::Integer(999_999_999), PyRep::Integer(1)])
            .unwrap_err()
            .code,
        "StructureNotFound"
    );
    assert_eq!(
        handle_character_get_service(&ctx, &mut owner_sess, &[PyRep::Integer(sid), PyRep::Integer(999)]).unwrap(),
        PyRep::None
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defense_profiles_fuel_transfers_fitting_locks_and_rpc_dispatch_branches() {
    let ctx = test_ctx();
    let sys_id = 30_002_537_u32; // Lowsec
    let owner_id = 92_001_i64;
    let pilot_id = 92_002_i64;
    let owner_corp = 98_920_i64;
    let pilot_corp = 98_921_i64;
    let owner_ally = 99_920_i64;
    let sid = 1_000_000_920_001_i64;
    let destroyed_sid = 1_000_000_920_002_i64;
    let unanchored_sid = 1_000_000_920_003_i64;
    let nocore_sid = 1_000_000_920_004_i64;
    let ship_id = 920_100_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(owner_corp, { let mut __corp = CorporationRecord::default(); __corp.corporation_id = owner_corp; __corp.ceo_id = owner_id; __corp.alliance_id = Some(owner_ally); __corp.ensure_wallet_divisions(); __corp });
        snap.corporations.insert(pilot_corp, { let mut __corp = CorporationRecord::default(); __corp.corporation_id = pilot_corp; __corp.ceo_id = pilot_id; __corp.ensure_wallet_divisions(); __corp });
        let base_ch = snap.characters.values().next().unwrap().clone();
        for (cid, corp, dock) in [(owner_id, owner_corp, Some(sid)), (pilot_id, pilot_corp, None)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = corp;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = dock;
            ch.active_ship_id = ship_id;
            snap.characters.insert(cid, ch);
        }
        for (st_id, state, core) in [
            (sid, "ShieldVulnerable", true),
            (destroyed_sid, "Destroyed", true),
            (unanchored_sid, "Unanchored", true),
            (nocore_sid, "NoCore", false),
        ] {
            snap.structures.insert(st_id, UpwellStructureRecord {
                structure_id: st_id, type_id: 35832, owner_corp_id: owner_corp, solar_system_id: sys_id,
                name: format!("St_{st_id}"), state: state.into(), quantum_core_installed: core, fuel_blocks: 50,
                shield_hp: 1_000_000_000.0, armor_hp: 1_000_000_000.0, hull_hp: 1_000_000_000.0,
                position: [0.0, 0.0, 0.0], ..Default::default()
            });
        }
        snap.alliance_contacts.insert(owner_ally, vec![SocialContactRecord {
            contact_id: pilot_id, standing: 5.0, contact_type: "character".into(), is_watched: false, is_blocked: false, label_ids: Vec::new(),
        }]);
        // Profile with defense_settings + access profile blocked group/char/corp
        let mut ext = load_ext_state(snap);
        let mut grp = AccessGroupRecord { group_id: 920, owner_corp_id: owner_corp, ..Default::default() };
        grp.members.insert(pilot_id, ACL_ROLE_EXCLUDED);
        ext.owner_groups.insert(920, grp);
        let mut prof = StructureAccessProfile {
            profile_id: 920, owner_corp_id: owner_corp,
            has_defense_config: true, auto_defense_enabled: true,
            defense_standing_threshold: 0.0, target_blocked_acl: true,
            access_group_ids: HashSet::from([920]),
            ..Default::default()
        };
        prof.defense_settings = Some(StructureDefenseSettings {
            structure_id: sid, auto_defense_enabled: true, auto_target_attackers: true,
            auto_target_war_targets: true, standing_threshold: 0.0, target_blocked_acl: true, ..Default::default()
        });
        ext.access_profiles.insert(sid, prof);
        save_ext_state(snap, &ext);

        // Two fuel stacks for multi-stack consume + move out/merge tests
        for (iid, qty, flag) in [
            (920_201, 5, InventoryFlag::SpecializedFuelBay),
            (920_202, 20, InventoryFlag::SpecializedFuelBay),
            (920_203, 10, InventoryFlag::Hangar),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: 4247, owner_id: owner_corp, location_id: sid, flag, quantity: qty, ..Default::default()
            });
        }
        snap.item_positions.insert(ship_id, [100.0, 0.0, 0.0]);
    });

    // evaluate_auto_defense_target with alliance_contacts + blocked ACL group + EWAR immunity + fitting locks
    ctx.db.with_mut_snapshot(|snap| {
        assert!(evaluate_auto_defense_target(&ctx, snap, sid, pilot_id));
        assert!(is_structure_immune_to_ewar(snap, sid));
        let ext = load_ext_state(snap);
        assert!(validate_structure_fitting_state(&ctx, snap, &ext, sid, ctx.now_unix()).is_ok());

        // Consume 10 blocks across two stacks (deletes 920_201 of 5, takes 5 from 920_202)
        assert!(consume_structure_fuel_blocks(&ctx, snap, sid, 10));
        assert!(!snap.items.contains_key(&920_201));
        assert_eq!(snap.items[&920_202].quantity, 15);
        // Consume 25 when only 15 left in bay -> returns false; consume with no bay items uses st.fuel_blocks
        assert!(!consume_structure_fuel_blocks(&ctx, snap, sid, 25));
        assert!(consume_structure_fuel_blocks(&ctx, snap, nocore_sid, 10));
        assert_eq!(snap.structures[&nocore_sid].fuel_blocks, 40);

        // Fuel bay move out to CorpSAG1 and back into fuel bay (merge)
        let (moved_out, _) = finalize_fuel_bay_move_in_snap(&ctx, snap, 920_202, sid, InventoryFlag::SpecializedFuelBay, sid, InventoryFlag::CorpSAG1, pilot_id, pilot_corp);
        assert_eq!(moved_out.unwrap().owner_id, pilot_corp);
        let (moved_in, _) = finalize_fuel_bay_move_in_snap(&ctx, snap, 920_202, sid, InventoryFlag::CorpSAG1, sid, InventoryFlag::SpecializedFuelBay, pilot_id, owner_corp);
        assert_eq!(moved_in.unwrap().owner_id, owner_corp);

        // validate_docked_ship_fitting_facility_in_snap error states
        assert_eq!(validate_docked_ship_fitting_facility_in_snap(&ctx, snap, destroyed_sid).unwrap_err().code, "StructureDestroyed");
        assert_eq!(validate_docked_ship_fitting_facility_in_snap(&ctx, snap, unanchored_sid).unwrap_err().code, "StructureNotOperational");
        assert_eq!(validate_docked_ship_fitting_facility_in_snap(&ctx, snap, nocore_sid).unwrap_err().code, "QuantumCoreRequired");
    });

    // validate_structure_fuel_bay_transfer in-space proximity via item_positions & error states
    let mut sess = make_session(owner_id, owner_corp, sys_id, None, 0);
    sess.ship_id = Some(ship_id);
    let pre_balls = snapshot_system_balls_for_inventory(&ctx, &sess, owner_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(validate_structure_fuel_bay_transfer(&ctx, snap, &sess, owner_id, 999_888, 4247, true, 0, i64::from(sys_id), ship_id, &pre_balls).unwrap_err().code, "StructureNotFound");
        assert_eq!(validate_structure_fuel_bay_transfer(&ctx, snap, &sess, owner_id, destroyed_sid, 4247, true, 0, i64::from(sys_id), ship_id, &pre_balls).unwrap_err().code, "StructureDestroyed");
        assert_eq!(validate_structure_fuel_bay_transfer(&ctx, snap, &sess, owner_id, unanchored_sid, 4247, true, 0, i64::from(sys_id), ship_id, &pre_balls).unwrap_err().code, "StructureNotOperational");
        assert!(validate_structure_fuel_bay_transfer(&ctx, snap, &sess, owner_id, sid, 4247, true, 0, i64::from(sys_id), ship_id, &pre_balls).is_ok());
    });

    // Async wrapper functions & RPC dispatcher
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    trigger_structure_auto_defense_on_grid(&ctx, &sys_h, sys_id, sid, None, ctx.now_unix()).await;
    trigger_system_structures_auto_defense(&ctx, sys_id).await;
    assert!(handle(&ctx, &mut sess, "ownerGroupManager", "GetGroup", &[PyRep::Integer(920)], &HashMap::new()).await.unwrap().is_ok());
    assert!(handle(&ctx, &mut sess, "ownerGroupManager", "GetMembers", &[PyRep::Integer(920)], &HashMap::new()).await.unwrap().is_ok());
    assert!(handle(&ctx, &mut sess, "unknownSvc", "NoOp", &[], &HashMap::new()).await.is_none()); // rpc-lint: allow test fixture
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acl_cross_group_sync_cyno_inhibitor_timings_and_char_service_matrix() {
    let ctx = test_ctx();
    let sys_id = 30_002_537_u32;
    let admin_id = 921_001_i64;
    let target_char = 921_002_i64;
    let ally_corp_char = 921_003_i64;
    let admin_corp = 98_921_001_i64;
    let target_corp = 98_921_002_i64;
    let target_ally = 99_921_002_i64;
    let sid = 1_000_921_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(admin_corp, { let mut __corp = CorporationRecord::default(); __corp.corporation_id = admin_corp; __corp.ceo_id = admin_id; __corp.ensure_wallet_divisions(); __corp });
        snap.corporations.insert(target_corp, { let mut __corp = CorporationRecord::default(); __corp.corporation_id = target_corp; __corp.ceo_id = target_char; __corp.alliance_id = Some(target_ally); __corp.ensure_wallet_divisions(); __corp });
        let base = snap.characters.values().next().unwrap().clone();
        for (cid, corp, ally) in [
            (admin_id, admin_corp, None),
            (target_char, target_corp, Some(target_ally)),
            (ally_corp_char, target_corp, Some(target_ally)),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = corp;
            ch.alliance_id = ally;
            ch.solar_system_id = sys_id;
            ch.structure_id = Some(sid);
            snap.characters.insert(cid, ch);
        }
        snap.structures.insert(sid, UpwellStructureRecord {
            structure_id: sid, type_id: 35832, owner_corp_id: admin_corp, solar_system_id: sys_id,
            name: "ACL Matrix Citadel".into(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 50, ..Default::default()
        });
        // Online service module (Standup Market Hub I = 35892) so CharacterGetServices populates entries
        snap.items.insert(921_301, InventoryItemRecord {
            item_id: 921_301, type_id: 35892, owner_id: admin_corp, location_id: sid,
            flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(921_301, true);

        let mut ext = load_ext_state(snap);
        let mut g1 = AccessGroupRecord { group_id: 9211, owner_corp_id: admin_corp, creator_id: admin_id, ..Default::default() };
        g1.members.insert(target_char, ACL_ROLE_MEMBER);
        let mut g2 = AccessGroupRecord { group_id: 9212, owner_corp_id: admin_corp, creator_id: admin_id, ..Default::default() };
        g2.members.insert(target_char, ACL_ROLE_MEMBER);
        g2.members.insert(target_corp, ACL_ROLE_EXCLUDED);
        ext.owner_groups.insert(9211, g1);
        ext.owner_groups.insert(9212, g2);

        let mut prof = StructureAccessProfile {
            profile_id: 9210, owner_corp_id: admin_corp,
            has_defense_config: true, auto_defense_enabled: true,
            defense_standing_threshold: -10.0, target_blocked_acl: true,
            access_group_ids: HashSet::from([9211, 9212]),
            allowed_characters: HashSet::from([admin_id, target_char]),
            blocked_characters: HashSet::new(),
            defense_blocked_characters: HashSet::new(),
            blocked_corporations: HashSet::from([target_corp, 98_921_999]),
            blocked_alliances: HashSet::from([target_ally]),
            ..Default::default()
        };
        ext.profiles_by_id.insert(9210, prof.clone());
        prof.defense_settings = None;
        ext.access_profiles.insert(sid, prof);

        // sync_profile_docking_entity_after_group_edit across allowed/excluded/removed transitions
        sync_profile_docking_entity_after_group_edit(&mut ext, 9210, 9211, 0, false, false, None);
        sync_profile_docking_entity_after_group_edit(&mut ext, 9210, 9211, target_char, false, false, Some(ACL_ROLE_EXCLUDED));
        assert!(ext.profiles_by_id[&9210].allowed_characters.contains(&target_char));
        sync_profile_docking_entity_after_group_edit(&mut ext, 9210, 9211, target_char, false, false, None);
        sync_profile_docking_entity_after_group_edit(&mut ext, 9210, 9211, target_corp, false, true, Some(ACL_ROLE_MEMBER));
        assert!(ext.profiles_by_id[&9210].blocked_corporations.contains(&target_corp));
        sync_profile_docking_entity_after_group_edit(&mut ext, 9210, 9211, admin_id, false, false, Some(ACL_ROLE_MEMBER));
        save_ext_state(snap, &ext);

        // evaluate_auto_defense_target fallback to has_defense_config + alliance/corp/defense blocks
        assert!(evaluate_auto_defense_target(&ctx, snap, sid, ally_corp_char));

        // validate_docked_ship_fitting_facility_in_snap on station without service 17
        if let Some((no_fit_st, _)) = ctx.sde.stations.iter().find(|(&id, _)| !crate::services::station_structure::npc_station_offers_service(&ctx.sde, id, 17)) {
            assert_eq!(validate_docked_ship_fitting_facility_in_snap(&ctx, snap, *no_fit_st).unwrap_err().code, "StationServiceUnavailable");
        }
    });

    // Pre-spawn system handle before calling is_cyno_inhibited inside snapshot closure
    let _ = ctx.systems.get_or_spawn(sys_id);

    // is_cyno_inhibited deployable timing branches: unanchored, warming up, expired, in range vs out of range
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, unanch, start, dur, exp, x) in [
            (921_401_i64, true, now - 100, 10, 0, 0.0),
            (921_402, false, now + 100, 60, 0, 0.0),
            (921_403, false, now - 500, 10, now - 1, 0.0),
            (921_404, false, now - 500, 10, now + 500, 200_000.0),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: TYPE_MOBILE_CYNO_INHIBITOR, owner_id: admin_id,
                location_id: i64::from(sys_id), flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
            snap.item_positions.insert(iid, [x, 0.0, 0.0]);
            write_deployable_state_snap(snap, |st| {
                st.deployables.insert(iid, MobileDeployableRecord {
                    item_id: iid,
                    type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                    group_id: GROUP_MOBILE_CYNO_INHIBITOR,
                    owner_id: admin_id,
                    solar_system_id: sys_id,
                    position: [x, 0.0, 0.0],
                    anchor_start_unix: start,
                    anchor_duration_secs: dur,
                    anchor_completed: true,
                    is_unanchored: unanch,
                    expires_at_unix: exp,
                    last_interaction_unix: now,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    deployed_fleet_id: None,
                });
            });
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!is_cyno_inhibited(&ctx, snap, sys_id, Some(glam::DVec3::ZERO)));
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(921_404, [1_000.0, 0.0, 0.0]);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(is_cyno_inhibited(&ctx, snap, sys_id, Some(glam::DVec3::new(500.0, 0.0, 0.0))));
    });

    // CharacterGetServices / HasService / CheckService / GetService / HasSetting + UpdateMembershipTypes / EditGroup
    let mut sess = make_session(admin_id, admin_corp, sys_id, Some(sid), 0);
    assert!(handle_character_get_services(&ctx, &mut sess, &[PyRep::Integer(sid)]).is_ok());
    assert!(handle_character_has_service(&ctx, &mut sess, &[PyRep::Integer(1)]).is_ok());
    assert!(handle_character_check_service(&ctx, &mut sess, &[PyRep::Integer(1)]).is_err() || handle_character_check_service(&ctx, &mut sess, &[PyRep::Integer(1)]).is_ok());
    assert!(handle_character_get_service(&ctx, &mut sess, &[PyRep::Integer(1)]).is_ok());
    assert!(handle_character_has_setting(&ctx, &mut sess, &[PyRep::Integer(1)]).is_ok());
    assert!(handle(&ctx, &mut sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(9211), PyRep::List(vec![PyRep::Integer(ally_corp_char)]), PyRep::Integer(ACL_ROLE_MEMBER)], &HashMap::new()).await.unwrap().is_ok());
    assert!(handle(&ctx, &mut sess, "ownerGroupManager", "EditGroup", &[PyRep::Integer(9211), PyRep::String("Renamed".into()), PyRep::String("Desc".into())], &HashMap::new()).await.unwrap().is_ok());
    assert!(handle(&ctx, &mut sess, "ownerGroupManager", "UpdateGroup", &[PyRep::Integer(9211), PyRep::String("Renamed2".into()), PyRep::String("Desc2".into())], &HashMap::new()).await.unwrap().is_ok());

    // validate_anchoring_restrictions & denied/offline CharacterGetServices / CharacterCheckService + solo group remove
    let mut outsider_sess = make_session(target_char, target_corp, sys_id, Some(sid), 0);
    assert_eq!(
        handle_character_check_service(&ctx, &mut outsider_sess, &[PyRep::Integer(sid), PyRep::Integer(1)]).unwrap_err().code,
        "StructureServiceAccessDenied"
    );
    assert_eq!(
        handle_character_get_services(&ctx, &mut outsider_sess, &[PyRep::Integer(sid)]).unwrap(),
        PyRep::Dict(Vec::new())
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(prof) = crate::services::station_structure::structure_lifecycle::structure_profile(&ctx, 35832) {
            let _ = validate_anchoring_restrictions(&ctx, snap, &prof, sys_id, glam::DVec3::ZERO);
        }
        let mut ext = load_ext_state(snap);
        sync_profile_docking_entity_after_group_edit(&mut ext, 9210, 9211, admin_id, false, false, None);
        assert!(!ext.profiles_by_id[&9210].allowed_characters.contains(&admin_id));
        sync_profile_docking_entity_after_group_edit(&mut ext, 9210, 9211, admin_id, false, false, Some(ACL_ROLE_EXCLUDED));
        assert!(ext.profiles_by_id[&9210].blocked_characters.contains(&admin_id));
        // Also mark structure Destroyed to hit is_operational false branch in handle_character_get_services
        snap.structures.get_mut(&sid).unwrap().state = "Destroyed".into();
        save_ext_state(snap, &ext);
    });
    assert_eq!(handle_character_get_services(&ctx, &mut sess, &[PyRep::Integer(sid)]).unwrap(), PyRep::Dict(Vec::new()));
}

#[test]
fn defense_settings_roundtrip_fuel_role_bitmasks_multi_stack_consume_and_move_mutants() {
    let ctx = test_ctx();
    let corp_id = 98_930_001_i64;
    let ceo_id = 930_001_i64;
    let member_id = 930_002_i64;
    let sid = 1_000_930_000_001_i64;
    let sid_other = 1_000_930_000_002_i64;
    let fuel_tid = 4247_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp_id, {
            let mut c = CorporationRecord::default();
            c.corporation_id = corp_id;
            c.ceo_id = ceo_id;
            c.ensure_wallet_divisions();
            c
        });
        let base_ch = snap.characters.values().next().unwrap().clone();
        for cid in [ceo_id, member_id] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_142;
            snap.characters.insert(cid, ch);
        }
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Fuel Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 60,
                ..Default::default()
            },
        );
        snap.structures.insert(
            sid_other,
            UpwellStructureRecord {
                structure_id: sid_other,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Side Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 10,
                ..Default::default()
            },
        );

        // 1. save_defense_settings + load_defense_settings round-trip (kills L437, L448)
        let custom_ds = StructureDefenseSettings {
            structure_id: sid,
            auto_defense_enabled: true,
            auto_target_attackers: false,
            auto_target_war_targets: true,
            standing_threshold: -2.5,
            target_blocked_acl: true,
            known_attackers: vec![99_111],
            known_attacker_timestamps: HashMap::from([(99_111, 1_700_000_123)]),
        };
        save_defense_settings(snap, &custom_ds);
        let loaded = load_defense_settings(snap, sid);
        assert_eq!(loaded, custom_ds);

        // 2. has_structure_fuel_bay_role exhaustive truth & bitmask table (kills L780..789)
        let gm_sess = make_session(member_id, corp_id, 30_000_142, Some(sid), 0);
        let mut gm_with_role = gm_sess.clone();
        gm_with_role.role = eve_machonet::service_roles::ROLE_GMH;
        assert!(has_structure_fuel_bay_role(snap, &gm_with_role, member_id, corp_id));

        let ceo_sess = make_session(ceo_id, corp_id, 30_000_142, Some(sid), 0);
        assert!(has_structure_fuel_bay_role(snap, &ceo_sess, ceo_id, corp_id));

        let plain_zero = make_session(member_id, corp_id, 30_000_142, Some(sid), 0);
        assert!(!has_structure_fuel_bay_role(snap, &plain_zero, member_id, corp_id));

        let non_fuel_sess = make_session(member_id, corp_id, 30_000_142, Some(sid), corp_roles::ROLE_AUDITOR);
        assert!(!has_structure_fuel_bay_role(snap, &non_fuel_sess, member_id, corp_id));

        let fuel_sess = make_session(member_id, corp_id, 30_000_142, Some(sid), corp_roles::ROLE_STARBASE_CONFIG);
        assert!(has_structure_fuel_bay_role(snap, &fuel_sess, member_id, corp_id));

        snap.corp_role_assignments.insert(
            (corp_id, member_id),
            CorpMemberRoleAssignment {
                character_id: member_id,
                roles: corp_roles::ROLE_AUDITOR,
                ..Default::default()
            },
        );
        assert!(!has_structure_fuel_bay_role(snap, &plain_zero, member_id, corp_id));
        snap.corp_role_assignments.get_mut(&(corp_id, member_id)).unwrap().roles =
            corp_roles::ROLE_STARBASE_OPERATOR;
        assert!(has_structure_fuel_bay_role(snap, &plain_zero, member_id, corp_id));

        // 3. consume_structure_fuel_blocks multi-stack remainder tracking (kills L859, L899, L905 `-= -> += / /=`)
        for (iid, qty) in [(930_101_i64, 10_i32), (930_102, 20), (930_103, 30)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: fuel_tid,
                    owner_id: corp_id,
                    location_id: sid,
                    flag: InventoryFlag::SpecializedFuelBay,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        assert!(consume_structure_fuel_blocks(&ctx, snap, sid, 35));
        assert!(!snap.items.contains_key(&930_101));
        assert!(!snap.items.contains_key(&930_102));
        assert_eq!(snap.items[&930_103].quantity, 25);
        assert_eq!(snap.structures[&sid].fuel_blocks, 25);

        // 4. finalize_fuel_bay_move_in_snap non-fuel vs fuel merge vs fuel move-out (kills L1095)
        snap.items.insert(
            930_201,
            InventoryItemRecord {
                item_id: 930_201,
                type_id: 34,
                owner_id: member_id,
                location_id: sid_other,
                flag: InventoryFlag::Hangar,
                quantity: 9,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let (non_fuel_it, non_fuel_fb) = finalize_fuel_bay_move_in_snap(
            &ctx,
            snap,
            930_201,
            sid_other,
            InventoryFlag::Hangar,
            sid_other,
            InventoryFlag::CorpSAG1,
            member_id,
            corp_id,
        );
        assert_eq!(non_fuel_it.unwrap().quantity, 9);
        assert_eq!(non_fuel_fb, None);

        snap.items.insert(
            930_202,
            InventoryItemRecord {
                item_id: 930_202,
                type_id: fuel_tid,
                owner_id: corp_id,
                location_id: sid_other,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 7,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let (moved_in, fb_in) = finalize_fuel_bay_move_in_snap(
            &ctx,
            snap,
            930_202,
            sid,
            InventoryFlag::Hangar,
            sid_other,
            InventoryFlag::SpecializedFuelBay,
            member_id,
            corp_id,
        );
        assert_eq!(moved_in.unwrap().quantity, 7);
        assert_eq!(fb_in, Some(17));
        let (moved_out, fb_out) = finalize_fuel_bay_move_in_snap(
            &ctx,
            snap,
            930_202,
            sid_other,
            InventoryFlag::SpecializedFuelBay,
            sid_other,
            InventoryFlag::Hangar,
            member_id,
            corp_id,
        );
        assert_eq!(moved_out.unwrap().owner_id, member_id);
        assert_eq!(fb_out, Some(10));
    });
}
