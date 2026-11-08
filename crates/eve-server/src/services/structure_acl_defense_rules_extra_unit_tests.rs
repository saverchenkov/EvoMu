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

#[tokio::test]
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
