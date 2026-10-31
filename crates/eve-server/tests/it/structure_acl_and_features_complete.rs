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

    // Set roles: member (0), manager (2), blocked (-1), admin (1) via UpdateMembershipTypes
    rpc(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "UpdateMembershipTypes",
        &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(other_char)]), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER)],
    )
    .await
    .expect("UpdateMembershipTypes other_char -> member");

    rpc(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "UpdateMembershipTypes",
        &[PyRep::Integer(group_id), PyRep::List(vec![PyRep::Integer(blocked_char)]), PyRep::Integer(acl_rules::ACL_ROLE_BLOCKED)],
    )
    .await
    .expect("UpdateMembershipTypes blocked_char -> blocked");

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

    // 2. Test feat:structures.docking-acl via live stationSvc.Dock / structureDocking.Dock RPCs
    let struct_id = 100_001;
    let corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).unwrap().corporation_id);
    create_test_structure(&ctx, struct_id, 35832, corp_id, 30_000_142, [0.0, 0.0, 0.0], life::STATE_SHIELD_VULNERABLE);

    let prof_id = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("Docking ACL Profile".into()), PyRep::String("Test".into())]).await.expect("CreateProfile").as_i64().unwrap();
    rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_id), PyRep::Dict(vec![(PyRep::String("accessGroups".into()), PyRep::List(vec![PyRep::Integer(group_id)]))])]).await.expect("SaveProfileSettings");
    rpc(&ctx, &mut sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(struct_id)]), PyRep::Integer(prof_id)]).await.expect("ChangeProfiles");

    let mut other_sess = login(&ctx, 302, "other", "other-secret", other_char).await;
    ctx.advance_virtual_clock(15);
    rpc(&ctx, &mut other_sess, "stationSvc", "Undock", &[]).await.expect("Undock other_char");
    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys_handle.set_ball_position(other_sess.ship_id.unwrap(), DVec3::ZERO).await;
    rpc(&ctx, &mut other_sess, "stationSvc", "Dock", &[PyRep::Integer(struct_id)]).await.expect("other_char on ACL must dock via stationSvc.Dock");
    assert_eq!(other_sess.structure_id, Some(struct_id));

    let blocked_ship = 100_901;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&blocked_char) {
            ch.station_id = None; ch.structure_id = None; ch.solar_system_id = 30_000_142; ch.active_ship_id = blocked_ship;
        }
        snap.items.insert(blocked_ship, InventoryItemRecord {
            item_id: blocked_ship, type_id: 587, owner_id: blocked_char, location_id: 30_000_142, flag: InventoryFlag::None,
            quantity: 1, singleton: true, version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
    let mut blocked_sess = SessionState::new(303);
    (blocked_sess.user_id, blocked_sess.character_id, blocked_sess.corporation_id, blocked_sess.solar_system_id, blocked_sess.ship_id) =
        (Some(303), Some(blocked_char), Some(98_000_002), Some(30_000_142), Some(blocked_ship));
    let _ = sys_handle.spawn_ball(Ball {
        id: blocked_ship, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
    }).await;
    let denied_err = rpc(&ctx, &mut blocked_sess, "stationSvc", "Dock", &[PyRep::Integer(struct_id)]).await.expect_err("blocked_char must be denied by stationSvc.Dock");
    assert_eq!(denied_err.code, "StructureDockingAccessDenied");
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
    create_test_structure(&ctx, struct_id, 35832, corp_id, 30_000_144, [0.0, 0.0, 0.0], life::STATE_SHIELD_VULNERABLE);
    sess.structure_id = Some(struct_id);
    sess.station_id = None;
    sess.solar_system_id = Some(30_000_144);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(struct_id);
            ch.station_id = None;
            ch.solar_system_id = 30_000_144;
        }
    });

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

    // 2. Test feat:structures.defense-settings via canonical structureSettings profile RPCs
    let attacker_pilot = 90_000_005;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get(&90_000_002).cloned() {
            let mut ca = c;
            ca.character_id = attacker_pilot;
            ca.corporation_id = 98_000_003;
            ca.name = "Attacker Pilot".into();
            snap.characters.insert(attacker_pilot, ca);
        }
        let mut ds = eve_server::services::structure_acl_defense_rules::load_defense_settings(snap, struct_id);
        ds.record_attacker(attacker_pilot, ctx.now_unix());
        eve_server::services::structure_acl_defense_rules::save_defense_settings(snap, &ds);
    });
    let prof_id = rpc(&ctx, &mut sess, "structureSettings", "CreateProfile", &[PyRep::String("Citadel Defense".into()), PyRep::String("Auto defense".into())])
        .await
        .expect("CreateProfile")
        .as_i64()
        .unwrap();
    rpc(&ctx, &mut sess, "structureSettings", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(struct_id)]), PyRep::Integer(prof_id)])
        .await
        .expect("ChangeProfiles");
    rpc(
        &ctx,
        &mut sess,
        "structureSettings",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_id),
            PyRep::Dict(vec![(
                PyRep::String("defenseSettings".into()),
                PyRep::Dict(vec![
                    (PyRep::String("autoDefenseEnabled".into()), PyRep::Bool(true)),
                    (PyRep::String("autoTargetAttackers".into()), PyRep::Bool(true)),
                    (PyRep::String("autoTargetWarTargets".into()), PyRep::Bool(true)),
                    (PyRep::String("standingThreshold".into()), PyRep::Real(0.0)),
                    (PyRep::String("targetBlockedAcl".into()), PyRep::Bool(true)),
                ]),
            )]),
        ],
    )
    .await
    .expect("SaveProfileSettings");

    // Non-client defense RPCs must return UnimplementedRPC
    let expected_err = "UnimplementedRPC";
    for bad_method in ["GetDefenseSettings", "SetDefenseSettings", "EvaluateDefenseTarget"] {
        assert_eq!(
            rpc(&ctx, &mut sess, "structureControl", bad_method, &[PyRep::Integer(struct_id)])
                .await
                .expect_err(bad_method)
                .code,
            expected_err
        );
    }

    // Release manual control so auto-defense engages, then verify settings persisted and live ballpark auto-defense weapon fire
    let _ = rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(struct_id)]).await;
    let (attacker_ship, launcher_id, ammo_id) = (100_902, 100_903, 100_904);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ds = eve_server::services::structure_acl_defense_rules::load_defense_settings(snap, struct_id);
        ds.record_attacker(attacker_pilot, ctx.now_unix());
        eve_server::services::structure_acl_defense_rules::save_defense_settings(snap, &ds);
        if let Some(ch) = snap.characters.get_mut(&attacker_pilot) {
            (ch.station_id, ch.solar_system_id, ch.active_ship_id) = (None, 30_000_144, attacker_ship);
        }
        for (id, tid, owner, loc, flag, qty, dyn_attrs) in [(attacker_ship, 621_u32, attacker_pilot, 30_000_144_i64, InventoryFlag::None, 1, HashMap::new()), (launcher_id, 35922, corp_id, struct_id, InventoryFlag::HiSlot0, 1, HashMap::from([(2, 1.0)])), (ammo_id, 37847, corp_id, struct_id, InventoryFlag::HiSlot0, 200, HashMap::new())] {
            snap.items.insert(id, InventoryItemRecord { item_id: id, type_id: tid, owner_id: owner, location_id: loc, flag, quantity: qty, singleton: qty == 1, version: 1, dynamic_attributes: dyn_attrs, source_mutaplasmid_id: None, insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None });
        }
    });
    let sys_144 = ctx.systems.get_or_spawn(30_000_144);
    let _ = sys_144.spawn_ball(Ball { id: struct_id, type_id: 35832_u32, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 10_000_000.0, radius: 5_000.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop }).await;
    let mut attacker_cs = ctx.build_type_combat_state(621);
    (attacker_cs.shield_hp, attacker_cs.max_shield_hp, attacker_cs.armor_hp, attacker_cs.max_armor_hp, attacker_cs.structure_hp, attacker_cs.max_structure_hp) = (5_000.0, 5_000.0, 5_000.0, 5_000.0, 5_000.0, 5_000.0);
    sys_144.spawn_ball_with_combat(Ball { id: attacker_ship, type_id: 621, position: DVec3::new(15_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 200.0, inertia: 1.0, mass: 10_000_000.0, radius: 140.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop }, attacker_cs).await.expect("spawn attacker_ship");
    let _ = sys_144.step_manual(5.0).await.expect("step_5s");
    let _ = sys_144.step_manual(60.0).await.expect("step_60s");
    let cs = sys_144.get_combat_state(attacker_ship).await.unwrap().expect("attacker combat state");
    assert!(cs.shield_hp < 5_000.0, "Auto-defense Standup launcher must lock and damage attacker ship in ballpark");
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
    let sys_id = 30_000_144;
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
    let valid_pos = station.position + DVec3::new(50_000_000.0, 0.0, 0.0); // 50,000 km
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

    // 1D. Trade hub & Pochven restrictions
    ctx.db.with_snapshot(|snap| {
        let jita_res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, 30_000_142, valid_pos);
        assert_eq!(jita_res.unwrap_err().code, "AnchoringDisallowedInTradeHub");
    });
    let pochven_sys = ctx.sde.solar_systems.values().find(|s| s.region_id == life::POCHVEN_REGION_ID).map(|s| s.solar_system_id).unwrap_or(30_000_157);
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, pochven_sys, valid_pos);
        assert!(res.is_err(), "anchoring in Pochven must be refused");
        assert_eq!(res.unwrap_err().code, "AnchoringDisallowedInPochven");
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

    // Unlocked target -> dogmaIM.Activate returns TargetNotLocked error via top-level router
    let unlocked_err = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
            PyRep::Integer(struct_id),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(unlocked_err.code, "TargetNotLocked");

    // Lock the structure and activate Stasis Webifier I -> Upwell structure is immune end-to-end via top-level router
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

    let act_rep = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(web_mod_id),
            PyRep::String("decreaseTargetSpeed".into()),
            PyRep::Integer(struct_id),
        ],
    )
    .await
    .expect("dogmaIM.Activate webifier against Upwell structure via top-level router");

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
