//! Integration tests for Upwell structure access lists, docking ACL, boarding/eject,
//! defense settings, anchoring restrictions, EWAR immunity, fitting state locks,
//! and mobile cynosural inhibitors (`feat:structures.*`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::services::station_structure::structure_upwell_ops as ops;
use eve_server::services::structure_acl_defense_rules as acl_rules;
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn kv_entry<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

async fn login(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

async fn login_with_corp(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let mut sess = login(ctx, sess_id, "demo", "demo-secret", char_id).await;
    ctx.advance_secs(12);
    let corp_name = format!("Upwell Sovereign {char_id}");
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String(corp_name.into()), PyRep::String("UPWL".into())])
        .await
        .expect("AddCorporation");
    sess.corp_role |= eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
    sess
}

fn create_test_structure(
    ctx: &ServerContext,
    structure_id: i64,
    type_id: u32,
    corp_id: i64,
    sys_id: u32,
    pos: [f64; 3],
    state: &str,
) {
    let profile = life::structure_profile(ctx, type_id).expect("valid structure type");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                position: pos,
                state: state.to_string(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: profile.shield_hp,
                armor_hp: profile.armor_hp,
                hull_hp: profile.hull_hp,
                name: format!("Test Structure {structure_id}"),
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            structure_id,
            InventoryItemRecord {
                item_id: structure_id,
                type_id,
                owner_id: corp_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

#[tokio::test]
async fn test_structure_access_lists_and_docking_acl() {
    // parity: feat:structures.access-lists, feat:structures.docking-acl
    eve_testkit::attest(&[
        "feat:structures.access-lists",
        "feat:structures.docking-acl",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001;
    let other_char = 90_000_002;
    let blocked_char = 90_000_003;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get(&90_000_002).cloned() {
            let mut c3 = c;
            c3.character_id = blocked_char;
            c3.corporation_id = 98_000_002;
            c3.name = "Blocked Pilot".into();
            snap.characters.insert(blocked_char, c3);
        }
    });
    let mut sess = login_with_corp(&ctx, 301, char_id).await;

    // 1. Test feat:structures.access-lists: Create group & assign member roles
    let create_res = rpc(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "Create",
        &[PyRep::String("Alpha ACL".into()), PyRep::String("Test Access List".into())],
    )
    .await
    .expect("Create owner group");
    let group_id = match create_res {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(kv) => kv.get("groupID").and_then(|v| v.as_i64()).unwrap_or(1),
        _ => 1,
    };
    assert!(group_id > 0);

    // Set roles: member (0), manager (2), blocked (-1), admin (1)
    rpc(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "SetMemberRole", // rpc-lint: allow canonical ledger feat:structures.access-lists requires SetMemberRole
        &[PyRep::Integer(group_id), PyRep::Integer(other_char), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER)],
    )
    .await
    .expect("SetMemberRole other_char -> member");

    rpc(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "SetMemberRole", // rpc-lint: allow canonical ledger feat:structures.access-lists requires SetMemberRole
        &[PyRep::Integer(group_id), PyRep::Integer(blocked_char), PyRep::Integer(acl_rules::ACL_ROLE_BLOCKED)],
    )
    .await
    .expect("SetMemberRole blocked_char -> blocked");

    // Verify immediate effect via GetMembers
    let members_rep = rpc(&ctx, &mut sess, "ownerGroupManager", "GetMembers", &[PyRep::Integer(group_id)])
        .await
        .expect("GetMembers");
    let PyRep::Dict(entries) = members_rep else { panic!("expected dict of members") };
    let member_role = entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(other_char))
        .and_then(|(_, v)| v.as_i64())
        .expect("other_char present");
    assert_eq!(member_role, acl_rules::ACL_ROLE_MEMBER);

    let blocked_role = entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(blocked_char))
        .and_then(|(_, v)| v.as_i64())
        .expect("blocked_char present");
    assert_eq!(blocked_role, acl_rules::ACL_ROLE_BLOCKED);

    // EditGroup test
    rpc(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "EditGroup",
        &[PyRep::Integer(group_id), PyRep::String("Alpha ACL Renamed".into()), PyRep::String("Updated Desc".into())],
    )
    .await
    .expect("EditGroup");

    let group_info = rpc(&ctx, &mut sess, "ownerGroupManager", "GetGroup", &[PyRep::Integer(group_id)])
        .await
        .expect("GetGroup");
    let PyRep::KeyVal(g_kv) = group_info else { panic!("expected keyval") };
    assert_eq!(g_kv.get("name").and_then(|v| v.as_str()), Some("Alpha ACL Renamed"));

    // 2. Test feat:structures.docking-acl
    let struct_id = 100_001;
    let corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).unwrap().corporation_id);
    create_test_structure(&ctx, struct_id, 35832, corp_id, 30_000_142, [0.0, 0.0, 0.0], life::STATE_SHIELD_VULNERABLE);

    // Configure structure profile requiring group_id for docking
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = ops::load_ext_state(snap);
        let mut profile = ops::StructureAccessProfile {
            profile_id: 1,
            owner_corp_id: corp_id,
            name: "Test Profile".into(),
            description: "Test".into(),
            ..Default::default()
        };
        profile.access_group_ids.insert(group_id);
        ext.access_profiles.insert(struct_id, profile);
        ops::save_ext_state(snap, &ext);
    });

    // Verify other_char (member on group_id) is granted docking access
    ctx.db.with_snapshot(|snap| {
        let access_res = acl_rules::verify_structure_docking_acl(&ctx, snap, struct_id, other_char);
        assert!(access_res.is_ok(), "other_char should have docking access");
    });

    // Verify blocked_char (blocked role on group_id) is denied docking access
    ctx.db.with_snapshot(|snap| {
        let denied_res = acl_rules::verify_structure_docking_acl(&ctx, snap, struct_id, blocked_char);
        assert!(denied_res.is_err(), "blocked_char must be denied docking access");
        assert_eq!(denied_res.unwrap_err().code, "StructureDockingAccessDenied");
    });

    // Verify a random unaffiliated pilot without ACL entry is also denied
    let random_pilot = 99_999_999;
    ctx.db.with_snapshot(|snap| {
        let denied_res = acl_rules::verify_structure_docking_acl(&ctx, snap, struct_id, random_pilot);
        assert!(denied_res.is_err(), "random pilot without ACL entry must be denied");
        assert_eq!(denied_res.unwrap_err().code, "StructureDockingAccessDenied");
    });
}

#[tokio::test]
async fn test_structure_board_eject_and_defense_settings() {
    // parity: feat:structures.board-structure, feat:structures.defense-settings
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "feat:structures.defense-settings",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001;
    let mut sess = login_with_corp(&ctx, 401, char_id).await;
    let original_ship = sess.ship_id.expect("ship_id");
    let corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).unwrap().corporation_id);

    let struct_id = 100_002;
    create_test_structure(&ctx, struct_id, 35832, corp_id, 30_000_142, [0.0, 0.0, 0.0], life::STATE_SHIELD_VULNERABLE);

    // 1. Test feat:structures.board-structure
    let board_res = rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("BoardStructure");
    assert_eq!(kv_entry(&board_res, "boarded"), Some(&PyRep::Bool(true)));
    assert_eq!(sess.ship_id, Some(struct_id));

    // Verify OnSessionChanged was emitted with shipID = struct_id
    let notifs = sess.drain_notifications();
    assert!(
        notifs.iter().any(|(name, _)| name == "OnSessionChanged"),
        "OnSessionChanged notification must be emitted on boarding"
    );

    // Verify GetStructurePilot returns char_id
    let pilot_rep = rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)])
        .await
        .expect("GetStructurePilot");
    assert_eq!(pilot_rep, PyRep::Integer(char_id));

    // Verify EjectFromStructure restores original ship
    let eject_res = rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("EjectFromStructure");
    assert_eq!(kv_entry(&eject_res, "boarded"), Some(&PyRep::Bool(false)));
    assert_eq!(sess.ship_id, Some(original_ship));

    // Verify active pilot is cleared
    let cleared_pilot = rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)])
        .await
        .expect("GetStructurePilot after eject");
    assert_eq!(cleared_pilot, PyRep::None);

    // 2. Test feat:structures.defense-settings
    let attacker_pilot = 90_000_005;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get(&90_000_002).cloned() {
            let mut ca = c;
            ca.character_id = attacker_pilot;
            ca.corporation_id = 98_000_003;
            ca.name = "Attacker Pilot".into();
            snap.characters.insert(attacker_pilot, ca);
        }
    });
    let set_args = vec![
        PyRep::Integer(struct_id),
        PyRep::Dict(vec![
            (PyRep::String("autoDefenseEnabled".into()), PyRep::Bool(true)),
            (PyRep::String("autoTargetAttackers".into()), PyRep::Bool(true)),
            (PyRep::String("autoTargetWarTargets".into()), PyRep::Bool(true)),
            (PyRep::String("standingThreshold".into()), PyRep::Real(0.0)),
            (PyRep::String("targetBlockedAcl".into()), PyRep::Bool(true)),
            (PyRep::String("attackerID".into()), PyRep::Integer(attacker_pilot)),
        ]),
    ];
    // rpc-lint: allow canonical ledger feat:structures.defense-settings requires structureControl defense methods
    rpc(&ctx, &mut sess, "structureControl", "SetDefenseSettings", &set_args)
        .await
        .expect("SetDefenseSettings");

    // Verify settings stored in snap.service_state_blobs
    let blob_key = format!("structure_defense:{struct_id}");
    ctx.db.with_snapshot(|snap| {
        let blob = snap.service_state_blobs.get(&blob_key).expect("defense settings blob must exist");
        assert!(blob.contains("\"auto_defense_enabled\":true"));
        assert!(blob.contains(&format!("{attacker_pilot}")));
    });

    // Verify GetDefenseSettings
    // rpc-lint: allow canonical ledger feat:structures.defense-settings requires structureControl defense methods
    let get_rep = rpc(&ctx, &mut sess, "structureControl", "GetDefenseSettings", &[PyRep::Integer(struct_id)])
        .await
        .expect("GetDefenseSettings");
    let PyRep::KeyVal(def_kv) = get_rep else { panic!("expected keyval") };
    assert_eq!(def_kv.get("autoDefenseEnabled"), Some(&PyRep::Bool(true)));
    assert_eq!(def_kv.get("autoTargetAttackers"), Some(&PyRep::Bool(true)));

    // Test EvaluateDefenseTarget
    // Case A: Friendly corp member -> NOT targeted
    let eval_friendly = rpc(
        &ctx,
        &mut sess,
        "structureControl",
        "EvaluateDefenseTarget", // rpc-lint: allow canonical ledger feat:structures.defense-settings requires structureControl defense methods
        &[PyRep::Integer(struct_id), PyRep::Integer(char_id)],
    )
    .await
    .expect("EvaluateDefenseTarget char_id");
    assert_eq!(eval_friendly, PyRep::Bool(false));

    // Case B: Known attacker -> targeted
    let eval_attacker = rpc(
        &ctx,
        &mut sess,
        "structureControl",
        "EvaluateDefenseTarget", // rpc-lint: allow canonical ledger feat:structures.defense-settings requires structureControl defense methods
        &[PyRep::Integer(struct_id), PyRep::Integer(attacker_pilot)],
    )
    .await
    .expect("EvaluateDefenseTarget attacker_pilot");
    assert_eq!(eval_attacker, PyRep::Bool(true));
}

#[tokio::test]
async fn test_structure_anchor_restrictions_and_fitting_state_locks() {
    // parity: feat:structures.anchor-restrictions, feat:structures.fitting-state-locks
    eve_testkit::attest(&[
        "feat:structures.anchor-restrictions",
        "feat:structures.fitting-state-locks",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let astrahus_type = 35832;
    let keepstar_type = 35834;
    let sys_id = 30_000_142;
    let profile = life::structure_profile(&ctx, astrahus_type).expect("astrahus profile");
    let keepstar_prof = life::structure_profile(&ctx, keepstar_type).expect("keepstar profile");

    // 1. Test feat:structures.anchor-restrictions
    // 1A. Minimum distance < 1,000 km from station fails
    let station = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).expect("station");
    let too_close_pos = station.position + DVec3::new(500_000.0, 0.0, 0.0); // 500 km < 1,000 km
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, sys_id, too_close_pos);
        assert!(res.is_err(), "must fail when < 1,000 km from station");
        assert_eq!(res.unwrap_err().code, "TooCloseToStructureOrStation");
    });

    // 1B. Distance >= 1,000 km succeeds
    let valid_pos = station.position + DVec3::new(5_000_000.0, 0.0, 0.0); // 5,000 km
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, sys_id, valid_pos);
        assert!(res.is_ok(), "must succeed when >= 1,000 km clear");
    });

    // 1C. Per-system Keepstar limit: 1 per system
    let ks_struct_id = 100_003;
    create_test_structure(&ctx, ks_struct_id, keepstar_type, 1001, sys_id, [valid_pos.x, valid_pos.y, valid_pos.z], life::STATE_SHIELD_VULNERABLE);
    let far_ks_pos = valid_pos + DVec3::new(10_000_000.0, 0.0, 0.0);
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &keepstar_prof, sys_id, far_ks_pos);
        assert!(res.is_err(), "second Keepstar in same system must be refused");
        assert_eq!(res.unwrap_err().code, "KeepstarPerSystemLimit");
    });

    // 1D. Pochven restriction
    let pochven_sys = ctx.sde.solar_systems.values().find(|s| s.region_id == life::POCHVEN_REGION_ID).map(|s| s.solar_system_id).unwrap_or(30_000_157);
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, pochven_sys, valid_pos);
        assert!(res.is_err(), "anchoring in Pochven must be refused");
        assert_eq!(res.unwrap_err().code, "CannotAnchorInPochven");
    });

    // 2. Test feat:structures.fitting-state-locks
    let refit_struct_id = 100_004;
    create_test_structure(&ctx, refit_struct_id, astrahus_type, 1001, sys_id, [valid_pos.x, valid_pos.y, valid_pos.z], life::STATE_SHIELD_VULNERABLE);

    let ext = ops::StructureExtendedState::default();
    let now = ctx.now_unix();

    // Full shields and SHIELD_VULNERABLE -> Refit allowed
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_structure_fitting_state(&ctx, snap, &ext, refit_struct_id, now);
        assert!(res.is_ok(), "refit must be allowed when at 100% shields in vulnerable state");
    });

    // Refit refused when damaged (shields < 100%)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&refit_struct_id) {
            st.shield_hp *= 0.5; // 50% shields
        }
    });
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_structure_fitting_state(&ctx, snap, &ext, refit_struct_id, now);
        assert!(res.is_err(), "refit must be refused when damaged");
        assert_eq!(res.unwrap_err().code, "StructureDamagedCannotRefit");
    });

    // Restore shields, but set state to Anchoring -> Refit refused
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&refit_struct_id) {
            st.shield_hp = profile.shield_hp;
            st.state = life::STATE_ANCHORING.to_string();
        }
    });
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_structure_fitting_state(&ctx, snap, &ext, refit_struct_id, now);
        assert!(res.is_err(), "refit must be refused during anchoring");
        assert_eq!(res.unwrap_err().code, "StructureFittingLockedInState");
    });
}

#[tokio::test]
async fn test_structure_ewar_immunity_and_mobile_cyno_inhibitor() {
    // parity: feat:structures.ewar-immunity, feat:structures.mobile-cyno-inhibitor
    eve_testkit::attest(&[
        "feat:structures.ewar-immunity",
        "feat:structures.mobile-cyno-inhibitor",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let struct_id = 100_005;
    let ship_id = 100_006;
    let sys_id = 30_000_142;

    create_test_structure(&ctx, struct_id, 35832, 1001, sys_id, [0.0, 0.0, 0.0], life::STATE_SHIELD_VULNERABLE);

    // 1. Test feat:structures.ewar-immunity
    ctx.db.with_snapshot(|snap| {
        // Upwell structure is immune to EWAR
        let struct_immune = acl_rules::is_structure_immune_to_ewar(snap, struct_id);
        assert_eq!(struct_immune, true, "Upwell structure must be immune to EWAR");
        // Regular ship is NOT immune to EWAR
        let ship_immune = acl_rules::is_structure_immune_to_ewar(snap, ship_id);
        assert_eq!(ship_immune, false, "Regular ship must not be immune to EWAR");
    });

    // Live dogmaIM.Activate RPC with an EWAR module (Stasis Webifier I, group 65) against Upwell structure asserting immune: true end-to-end
    let char_id = 90_000_001;
    let mut sess = login(&ctx, 401, "demo", "demo-secret", char_id).await;
    let web_mod_id = 100_008;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            web_mod_id,
            InventoryItemRecord {
                item_id: web_mod_id,
                type_id: 526, // Stasis Webifier I (group 65)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 3.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    // Unlocked target -> dogmaIM.Activate returns TargetNotLocked error
    let unlocked_err = eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
            PyRep::Integer(struct_id),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(unlocked_err.code, "TargetNotLocked");

    // Lock the structure and activate Stasis Webifier I -> Upwell structure is immune end-to-end
    let _ = sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: struct_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    let act_rep = eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
            PyRep::Integer(struct_id),
        ],
        &HashMap::new(),
    )
    .await
    .expect("dogmaIM.Activate webifier against Upwell structure");

    assert_eq!(kv_entry(&act_rep, "moduleID"), Some(&PyRep::Integer(web_mod_id)));
    assert_eq!(kv_entry(&act_rep, "targetID"), Some(&PyRep::Integer(struct_id)));
    assert_eq!(kv_entry(&act_rep, "immune"), Some(&PyRep::Bool(true)));
    assert_eq!(kv_entry(&act_rep, "ewarAssistRecorded"), Some(&PyRep::Bool(false)));
    assert_eq!(kv_entry(&act_rep, "damageDealt"), Some(&PyRep::Real(0.0)));

    // 2. Test feat:structures.mobile-cyno-inhibitor
    assert_eq!(acl_rules::TYPE_MOBILE_CYNO_INHIBITOR, 33476);
    assert_eq!(acl_rules::GROUP_MOBILE_CYNO_INHIBITOR, 1249);
    assert_eq!(acl_rules::MOBILE_CYNO_INHIBITOR_RANGE_M, 100_000.0);

    let inhibitor_id = 100_007;
    let inhibitor_pos = [10_000.0, 20_000.0, 30_000.0];
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            inhibitor_id,
            InventoryItemRecord {
                item_id: inhibitor_id,
                type_id: acl_rules::TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id: 1001,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions.insert(inhibitor_id, inhibitor_pos);
    });

    // Position within 100 km (e.g. 50 km distance) -> inhibited
    let test_pos_close = DVec3::new(10_000.0 + 50_000.0, 20_000.0, 30_000.0);
    ctx.db.with_snapshot(|snap| {
        let is_inhibited = acl_rules::is_cyno_inhibited(&ctx, snap, sys_id, Some(test_pos_close));
        assert_eq!(is_inhibited, true, "cyno must be inhibited within 100 km of active inhibitor");
    });

    // Position beyond 100 km (e.g. 150 km distance) -> NOT inhibited
    let test_pos_far = DVec3::new(10_000.0 + 150_000.0, 20_000.0, 30_000.0);
    ctx.db.with_snapshot(|snap| {
        let is_inhibited = acl_rules::is_cyno_inhibited(&ctx, snap, sys_id, Some(test_pos_far));
        assert_eq!(is_inhibited, false, "cyno must NOT be inhibited beyond 100 km");
    });

    // Different solar system -> NOT inhibited
    let other_sys = 30_000_143;
    ctx.db.with_snapshot(|snap| {
        let is_inhibited = acl_rules::is_cyno_inhibited(&ctx, snap, other_sys, Some(test_pos_close));
        assert_eq!(is_inhibited, false, "cyno inhibitor in sys A must not affect sys B");
    });
}
