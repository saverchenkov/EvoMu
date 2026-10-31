//! Integration tests for Upwell structure access lists, docking ACL, boarding/eject,
//! defense settings, anchoring restrictions, EWAR immunity, fitting state locks,
//! and mobile cynosural inhibitors (`feat:structures.*`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::Ball;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
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
    ctx.advance_secs(12);
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

    // 2. Test feat:structures.docking-acl end-to-end via structureProfiles + structureDocking.Dock + beyonce.CmdDock
    let struct_id = 1_000_000_100_001;
    let corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).unwrap().corporation_id);
    let st_pos = [25_000_000.0, 0.0, 0.0];
    create_test_structure(&ctx, struct_id, 35832, corp_id, 30_000_142, st_pos, life::STATE_SHIELD_VULNERABLE);

    let prof_id = rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Docking Gate Profile".into()), PyRep::String("ACL Docking Test".into())],
    )
    .await
    .expect("CreateProfile")
    .as_i64()
    .expect("profile id");

    // setting_id 1 = setting_access_docking_tethering
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(group_id),
                PyRep::Real(0.0),
            ])]),
        ],
    )
    .await
    .expect("SaveProfileSettings docking ACL");

    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "ChangeProfiles",
        &[PyRep::List(vec![PyRep::Integer(struct_id)]), PyRep::Integer(prof_id)],
    )
    .await
    .expect("ChangeProfiles");

    // Helper to launch a pilot in space 1,500m from the structure
    let place_near_structure = |cid: i64, ship_id: i64| {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = 30_000_142;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: cid,
                    location_id: 30_000_142,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(ship_id, [st_pos[0] + 1_500.0, st_pos[1], st_pos[2]]);
        });
    };

    // 2A. Allowed pilot (`other_char`, member on `group_id`) docks via `structureDocking.Dock`, undocks, then docks via `beyonce.CmdDock`
    place_near_structure(other_char, 770_001);
    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: 770_001,
            type_id: 587,
            position: DVec3::new(st_pos[0] + 1_500.0, st_pos[1], st_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let mut other_sess = SessionState::new(302);
    other_sess.user_id = Some(2);
    other_sess.character_id = Some(other_char);
    other_sess.corporation_id = Some(ctx.db.with_snapshot(|s| s.characters[&other_char].corporation_id));
    other_sess.solar_system_id = Some(30_000_142);
    other_sess.ship_id = Some(770_001);

    let check_allowed = rpc(
        &ctx,
        &mut other_sess,
        "structureDirectory",
        "CheckMyDockingAccessToStructures",
        &[PyRep::List(vec![PyRep::Integer(struct_id)])],
    )
    .await
    .expect("CheckMyDockingAccessToStructures");
    assert_eq!(check_allowed, PyRep::List(vec![PyRep::Integer(struct_id)]));

    rpc(&ctx, &mut other_sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)])
        .await
        .expect("ACL-allowed pilot must dock via structureDocking.Dock");
    assert_eq!(other_sess.structure_id, Some(struct_id));

    rpc(&ctx, &mut other_sess, "structureDocking", "Undock", &[PyRep::Integer(struct_id)])
        .await
        .expect("Undock from structure");
    assert_eq!(other_sess.structure_id, None);
    ctx.advance_secs(12);

    rpc(&ctx, &mut other_sess, "beyonce", "CmdDock", &[PyRep::Integer(struct_id), PyRep::Integer(770_001)])
        .await
        .expect("ACL-allowed pilot must also dock via beyonce.CmdDock");
    assert_eq!(other_sess.structure_id, Some(struct_id));

    // 2B. Blocked pilot (`blocked_char`, role = -1) is denied by both `structureDocking.Dock` and `beyonce.CmdDock`
    place_near_structure(blocked_char, 770_002);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: 770_002,
            type_id: 587,
            position: DVec3::new(st_pos[0] + 1_500.0, st_pos[1], st_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let mut blocked_sess = SessionState::new(303);
    blocked_sess.user_id = Some(3);
    blocked_sess.character_id = Some(blocked_char);
    blocked_sess.corporation_id = Some(98_000_002);
    blocked_sess.solar_system_id = Some(30_000_142);
    blocked_sess.ship_id = Some(770_002);

    let dock_err = rpc(&ctx, &mut blocked_sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)])
        .await
        .expect_err("blocked pilot must be denied by structureDocking.Dock");
    assert_eq!(dock_err.code, "StructureDockingAccessDenied");

    let cmd_dock_err = rpc(&ctx, &mut blocked_sess, "beyonce", "CmdDock", &[PyRep::Integer(struct_id), PyRep::Integer(770_002)])
        .await
        .expect_err("blocked pilot must be denied by beyonce.CmdDock");
    assert_eq!(cmd_dock_err.code, "StructureDockingAccessDenied");

    // 2C. Multi-ACL block precedence: even if a second ACL group allows `blocked_char`, the `-1` block in `group_id` wins!
    let g2_rep = rpc(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "Create",
        &[PyRep::String("Beta Allow ACL".into()), PyRep::String("Secondary ACL".into())],
    )
    .await
    .expect("Create group 2");
    let group2_id = kv_entry(&g2_rep, "groupID").and_then(|v| v.as_i64()).or_else(|| g2_rep.as_i64()).unwrap();
    rpc(
        &ctx,
        &mut sess,
        "ownerGroupManager",
        "UpdateMembershipTypes",
        &[PyRep::Integer(group2_id), PyRep::List(vec![PyRep::Integer(blocked_char)]), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER)],
    )
    .await
    .expect("Allow blocked_char in group 2");
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(group_id), PyRep::Real(0.0)]),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(group2_id), PyRep::Real(0.0)]),
            ]),
        ],
    )
    .await
    .expect("Attach both ACL groups to docking setting");
    let still_blocked = rpc(&ctx, &mut blocked_sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)])
        .await
        .expect_err("block in group_id must override allow in group2_id");
    assert_eq!(still_blocked.code, "StructureDockingAccessDenied");
    assert_eq!(
        rpc(&ctx, &mut blocked_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(struct_id)])]).await.unwrap(),
        PyRep::List(vec![])
    );

    // 2D. Unaffiliated pilot not on any ACL is denied docking
    let unaffiliated = 90_000_009;
    ctx.db.with_mut_snapshot(|snap| {
        let mut cu = snap.characters[&other_char].clone();
        cu.character_id = unaffiliated;
        cu.corporation_id = 98_000_099;
        cu.name = "Unaffiliated Pilot".into();
        snap.characters.insert(unaffiliated, cu);
    });
    place_near_structure(unaffiliated, 770_003);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: 770_003,
            type_id: 587,
            position: DVec3::new(st_pos[0] + 1_500.0, st_pos[1], st_pos[2]),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let mut unaff_sess = SessionState::new(304);
    unaff_sess.user_id = Some(9);
    unaff_sess.character_id = Some(unaffiliated);
    unaff_sess.corporation_id = Some(98_000_099);
    unaff_sess.solar_system_id = Some(30_000_142);
    unaff_sess.ship_id = Some(770_003);
    assert_eq!(
        rpc(&ctx, &mut unaff_sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)]).await.unwrap_err().code,
        "StructureDockingAccessDenied"
    );
}

#[tokio::test]
async fn test_structure_board_eject_and_defense_acl_control() {
    // parity: feat:structures.board-structure
    eve_testkit::attest(&["feat:structures.board-structure"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let char_id = 90_000_001;
    let gunner_char = 90_000_002;
    let mut sess = login_with_corp(&ctx, 401, char_id).await;
    let original_ship = sess.ship_id.expect("ship_id");
    let corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).unwrap().corporation_id);

    let struct_id = 1_000_000_100_002;
    let st_pos = [30_000_000.0, 0.0, 0.0];
    create_test_structure(&ctx, struct_id, 35832, corp_id, 30_000_142, st_pos, life::STATE_SHIELD_VULNERABLE);

    // 1. Negative test: BoardStructure while docked at Jita 4-4 (NOT docked inside struct_id) must fail!
    let undocked_err = rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect_err("BoardStructure outside structure must fail");
    assert_eq!(undocked_err.code, "NotDockedInStructure");

    // Undock from NPC station and dock inside struct_id via structureDocking.Dock
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock from NPC station");
    ctx.advance_secs(12);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(original_ship, [st_pos[0] + 1_000.0, st_pos[1], st_pos[2]]);
    });
    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(b) = bp.balls.get_mut(&original_ship) {
            b.position = DVec3::new(st_pos[0] + 1_000.0, st_pos[1], st_pos[2]);
        }
    }
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)])
        .await
        .expect("Dock inside Upwell structure");
    assert_eq!(sess.structure_id, Some(struct_id));
    sess.drain_notifications();

    // 2. Positive test: BoardStructure while docked inside struct_id transitions shipID -> struct_id
    let board_res = rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("BoardStructure while docked");
    assert_eq!(kv_entry(&board_res, "boarded"), Some(&PyRep::Bool(true)));
    assert_eq!(sess.ship_id, Some(struct_id));

    let notifs = sess.drain_notifications();
    assert!(notifs.iter().any(|(name, _)| name == "OnSessionChanged"));

    let pilot_rep = rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)])
        .await
        .expect("GetStructurePilot");
    assert_eq!(pilot_rep, PyRep::Integer(char_id));

    // 3. Configure defense ACL granting `gunner_char` structure weapon control (`setting_id = 3`), but NOT before testing unauthorized denial
    let mut gunner_sess = login(&ctx, 402, "other", "other-secret", gunner_char).await;
    ctx.advance_secs(12);
    let gunner_ship = gunner_sess.ship_id.expect("gunner ship");
    let acl_rep = rpc(&ctx, &mut sess, "ownerGroupManager", "Create", &[PyRep::String("Gunner ACL".into()), PyRep::String("Gunners".into())])
        .await
        .unwrap();
    let acl_id = kv_entry(&acl_rep, "groupID").and_then(|v| v.as_i64()).or_else(|| acl_rep.as_i64()).unwrap();
    rpc(&ctx, &mut sess, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(acl_id), PyRep::List(vec![PyRep::Integer(gunner_char)]), PyRep::Integer(acl_rules::ACL_ROLE_MEMBER)])
        .await
        .unwrap();
    let prof_id = rpc(&ctx, &mut sess, "structureProfiles", "CreateProfile", &[PyRep::String("Combat Profile".into()), PyRep::String("Dock only first".into())])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    // Initially grant ONLY docking (`1`), NOT defense control (`3`)
    rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(acl_id), PyRep::Real(0.0)])])])
        .await
        .unwrap();
    rpc(&ctx, &mut sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(struct_id)]), PyRep::Integer(prof_id)])
        .await
        .unwrap();

    rpc(&ctx, &mut gunner_sess, "stationSvc", "Undock", &[]).await.unwrap();
    ctx.advance_secs(12);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(gunner_ship, [st_pos[0] + 1_200.0, st_pos[1], st_pos[2]]);
    });
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        if let Some(b) = bp.balls.get_mut(&gunner_ship) {
            b.position = DVec3::new(st_pos[0] + 1_200.0, st_pos[1], st_pos[2]);
        }
    }
    rpc(&ctx, &mut gunner_sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)]).await.unwrap();

    // Without defense ACL or corp role, gunner_char cannot board even after char_id ejects
    let eject_res = rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("EjectFromStructure");
    assert_eq!(kv_entry(&eject_res, "boarded"), Some(&PyRep::Bool(false)));
    assert_eq!(sess.ship_id, Some(original_ship));
    assert!(sess.drain_notifications().iter().any(|(n, _)| n == "OnSessionChanged"));
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(), PyRep::None);

    let unauth_board = rpc(&ctx, &mut gunner_sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect_err("docked non-corp pilot without defense ACL must be rejected");
    assert!(matches!(unauth_board.code.as_str(), "StructureOwnershipViolation" | "NotStructureMember" | "InsufficientCorpRoles" | "DefenseAccessDenied"));

    // Grant defense control (`setting_id = 17` = setting_defense_can_control_structure) on `prof_id` -> gunner_char can now board, and single-controller lock blocks `char_id`
    rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(acl_id), PyRep::Real(0.0)]),
                PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(acl_id), PyRep::Real(0.0)]),
            ]),
        ],
    )
    .await
    .unwrap();

    rpc(&ctx, &mut gunner_sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect("ACL gunner must be able to board structure");
    assert_eq!(gunner_sess.ship_id, Some(struct_id));

    // Second pilot attempting to board while occupied fails with StructureAlreadyControlled
    let occupied_err = rpc(&ctx, &mut sess, "structureControl", "BoardStructure", &[PyRep::Integer(struct_id)])
        .await
        .expect_err("second controller must fail while occupied");
    assert_eq!(occupied_err.code, "StructureAlreadyControlled");

    // Undocking while controlling structure automatically relinquishes structure control and launches gunner_ship
    rpc(&ctx, &mut gunner_sess, "structureDocking", "Undock", &[PyRep::Integer(struct_id)])
        .await
        .expect("Undock while controlling structure");
    assert_eq!(gunner_sess.ship_id, Some(gunner_ship));
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.unwrap(), PyRep::None);
}
