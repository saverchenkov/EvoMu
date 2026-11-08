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


pub(crate) mod structure_anchoring_ewar_immunity_and_defense {
    #[allow(unused_imports)]
    use super::*;

// Upwell structure anchoring restrictions, fitting state locks, EWAR immunity,
// and mobile cynosural inhibitors (`feat:structures.*`).

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
                ..Default::default()
            },
        );
    });
}

#[tokio::test]
async fn test_structure_anchor_restrictions_and_fitting_state_locks() {
    // parity: feat:structures.fitting-state-locks
    eve_testkit::attest(&[
        "feat:structures.fitting-state-locks",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let astrahus_type = 35832;
    let keepstar_type = 35834;
    let sys_id = 30_000_144;
    let profile = life::structure_profile(&ctx, astrahus_type).expect("astrahus profile");
    let keepstar_prof = life::structure_profile(&ctx, keepstar_type).expect("keepstar profile");

    let station = ctx.sde.stations.values().find(|s| s.solar_system_id == sys_id).expect("station");
    let too_close_pos = station.position + DVec3::new(500_000.0, 0.0, 0.0);
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, sys_id, too_close_pos);
        assert_eq!(res.unwrap_err().code, "TooCloseToStructureOrStation");
    });

    let valid_pos = station.position + DVec3::new(50_000_000.0, 0.0, 0.0);
    ctx.db.with_snapshot(|snap| {
        assert!(acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, sys_id, valid_pos).is_ok());
    });

    let ks_struct_id = 100_003;
    create_test_structure(&ctx, ks_struct_id, keepstar_type, 1001, sys_id, [valid_pos.x, valid_pos.y, valid_pos.z], life::STATE_SHIELD_VULNERABLE);
    let far_ks_pos = valid_pos + DVec3::new(10_000_000.0, 0.0, 0.0);
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &keepstar_prof, sys_id, far_ks_pos);
        assert_eq!(res.unwrap_err().code, "KeepstarPerSystemLimit");
    });

    ctx.db.with_snapshot(|snap| {
        let jita_res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, 30_000_142, valid_pos);
        assert_eq!(jita_res.unwrap_err().code, "AnchoringDisallowedInTradeHub");
    });
    let pochven_sys = ctx.sde.solar_systems.values().find(|s| s.region_id == life::POCHVEN_REGION_ID).map(|s| s.solar_system_id).unwrap_or(30_000_157);
    ctx.db.with_snapshot(|snap| {
        let res = acl_rules::validate_anchoring_restrictions(&ctx, snap, &profile, pochven_sys, valid_pos);
        assert_eq!(res.unwrap_err().code, "AnchoringDisallowedInPochven");
    });

    let refit_struct_id = 100_004;
    create_test_structure(&ctx, refit_struct_id, astrahus_type, 1001, sys_id, [valid_pos.x, valid_pos.y, valid_pos.z], life::STATE_SHIELD_VULNERABLE);
    let ext = ops::StructureExtendedState::default();
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        assert!(acl_rules::validate_structure_fitting_state(&ctx, snap, &ext, refit_struct_id, now).is_ok());
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&refit_struct_id) {
            st.shield_hp *= 0.5;
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(acl_rules::validate_structure_fitting_state(&ctx, snap, &ext, refit_struct_id, now).unwrap_err().code, "StructureDamagedCannotRefit");
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&refit_struct_id) {
            st.shield_hp = profile.shield_hp;
            st.state = life::STATE_ANCHORING.to_string();
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(acl_rules::validate_structure_fitting_state(&ctx, snap, &ext, refit_struct_id, now).unwrap_err().code, "StructureFittingLockedInState");
    });
}

#[tokio::test]
async fn test_structure_ewar_immunity_and_mobile_cyno_inhibitor() {
    // parity: feat:structures.ewar-immunity
    eve_testkit::attest(&[
        "feat:structures.ewar-immunity",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let struct_id = 100_005;
    let ship_id = 100_006;
    let sys_id = 30_000_142;

    create_test_structure(&ctx, struct_id, 35832, 1001, sys_id, [0.0, 0.0, 0.0], life::STATE_SHIELD_VULNERABLE);

    ctx.db.with_snapshot(|snap| {
        assert!(acl_rules::is_structure_immune_to_ewar(snap, struct_id));
        assert!(!acl_rules::is_structure_immune_to_ewar(snap, ship_id));
    });

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
                ..Default::default()
            },
        );
        snap.items.insert(
            web_mod_id,
            InventoryItemRecord {
                item_id: web_mod_id,
                type_id: 526,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
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
    .expect("dogmaIM.Activate webifier against Upwell structure");

    assert_eq!(kv_entry(&act_rep, "immune"), Some(&PyRep::Bool(true)));
    assert_eq!(kv_entry(&act_rep, "damageDealt"), Some(&PyRep::Real(0.0)));

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
                ..Default::default()
            },
        );
        snap.item_positions.insert(inhibitor_id, inhibitor_pos);
    });

    let test_pos_close = DVec3::new(10_000.0 + 50_000.0, 20_000.0, 30_000.0);
    let test_pos_far = DVec3::new(10_000.0 + 150_000.0, 20_000.0, 30_000.0);
    ctx.db.with_snapshot(|snap| {
        assert!(acl_rules::is_cyno_inhibited(&ctx, snap, sys_id, Some(test_pos_close)));
        assert!(!acl_rules::is_cyno_inhibited(&ctx, snap, sys_id, Some(test_pos_far)));
        assert!(!acl_rules::is_cyno_inhibited(&ctx, snap, 30_000_143, Some(test_pos_close)));
    });
}

#[tokio::test]
async fn test_station_standing_check_diplomacy_connections_and_corp_office_gates() {
    // parity: feat:structures.station-standing-check, rpc:corpStationMgr.DoStandingCheckForStationService
    eve_testkit::attest(&[
        "feat:structures.station-standing-check",
        "rpc:corpStationMgr.DoStandingCheckForStationService",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let pilot1 = 90_000_001i64;
    let pilot2 = 90_000_002i64;
    let mut s1 = login(&ctx, 901, "demo", "demo-secret", pilot1).await;
    let mut s2 = login(&ctx, 902, "other", "other-secret", pilot2).await;

    // Undocked caller with no explicit stationID must fail with NotDocked (no hardcoded Jita fallback)
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[]).await.expect("Undock pilot2");
    ctx.sync_session(&mut s2);
    let err_undocked = rpc(&ctx, &mut s2, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(13)])
        .await
        .expect_err("Undocked pilot without stationID must get NotDocked");
    assert_eq!(err_undocked.code, "NotDocked");

    // FSD stationstandingsrestrictions.fsdbinary gates on Triglavian Pochven station 60000355 (faction 500026):
    // - Docking (22) & Repair (13) require >= -0.05; Jump Clone Facility (24) & Office Rental (23, corp standing) require >= 3.0.
    // - Diplomacy (3357) lifts negative standing (+4%/lvl toward 10); Connections (3359) lifts positive standing (+4%/lvl toward 10).
    const STATION_POCHVEN: i64 = 60_000_355;
    const FACTION_TRIGLAVIAN: i64 = 500_026;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot1).unwrap().standings.insert(FACTION_TRIGLAVIAN, -0.20);
    });
    let err_trig_dock = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(22), PyRep::Integer(STATION_POCHVEN)],
    )
    .await
    .expect_err("Triglavian docking requires >= -0.05 standing");
    assert_eq!(err_trig_dock.code, "StandingTooLowForStationService");

    // Triglavian (500026) is exempt from Diplomacy/Connections in compute_effective_standing_breakdown:
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot1).unwrap().standings.insert(FACTION_TRIGLAVIAN, 0.0);
    });
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(22), PyRep::Integer(STATION_POCHVEN)]).await.unwrap(),
        PyRep::Bool(true),
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(13), PyRep::Integer(STATION_POCHVEN)]).await.unwrap(),
        PyRep::Bool(true),
    );
    let err_trig_clone = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(24), PyRep::Integer(STATION_POCHVEN)],
    )
    .await
    .expect_err("Triglavian jump clone facility (24) requires >= 3.0 standing");
    assert_eq!(err_trig_clone.code, "StandingTooLowForStationService");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot1).unwrap().standings.insert(FACTION_TRIGLAVIAN, 3.1);
    });
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(24), PyRep::Integer(STATION_POCHVEN)]).await.unwrap(),
        PyRep::Bool(true),
    );

    // Office rental (23) checks player-corporation standing (CEO/corp standing), not solo member personal standing:
    let player_corp = 98_000_777i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            player_corp,
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = player_corp;
                __corp.name = "Test Pochven Corp".into();
                __corp.ticker = "TPCHV".into();
                __corp.ceo_id = pilot2;
                __corp.members = [pilot1, pilot2].into_iter().collect();
                __corp },
        );
        snap.characters.get_mut(&pilot1).unwrap().corporation_id = player_corp;
        snap.characters.get_mut(&pilot2).unwrap().corporation_id = player_corp;
        snap.characters.get_mut(&pilot2).unwrap().standings.insert(FACTION_TRIGLAVIAN, 1.0);
    });
    let err_trig_office = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(23), PyRep::Integer(STATION_POCHVEN)],
    )
    .await
    .expect_err("Triglavian office rental must reject corp standing < 3.0 even when caller personal standing >= 3.0");
    assert_eq!(err_trig_office.code, "StandingTooLowForStationService");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot2).unwrap().standings.insert(FACTION_TRIGLAVIAN, 3.2);
    });
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(23), PyRep::Integer(STATION_POCHVEN)]).await.unwrap(),
        PyRep::Bool(true),
    );
}

#[tokio::test]
async fn test_station_standing_check_allows_when_no_fsd_entry() {
    // parity: feat:structures.station-standing-check, rpc:corpStationMgr.DoStandingCheckForStationService
    eve_testkit::attest(&[
        "feat:structures.station-standing-check",
        "rpc:corpStationMgr.DoStandingCheckForStationService",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let pilot1 = 90_000_001i64;
    let mut s1 = login(&ctx, 903, "demo", "demo-secret", pilot1).await;
    const STATION_JITA_4_4: i64 = 60_003_760;

    let (owner_corp, owner_fac) = ctx
        .sde
        .stations
        .get(&STATION_JITA_4_4)
        .map(|st| (st.owner_corporation_id, st.owner_faction_id))
        .expect("Jita 4-4 in SDE");
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot1).unwrap();
        ch.standings.insert(owner_corp, -5.0);
        if let Some(fid) = owner_fac {
            ch.standings.insert(fid, -5.0);
        }
    });

    // Jita 4-4 has no entry in stationstandingsrestrictions.fsdbinary: services offered by the station are allowed without invented -2.0/0.0 gates
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(13), PyRep::Integer(STATION_JITA_4_4)]).await.unwrap(),
        PyRep::Bool(true),
    );
    assert_eq!(
        rpc(&ctx, &mut s1, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(23), PyRep::Integer(STATION_JITA_4_4)]).await.unwrap(),
        PyRep::Bool(true),
    );
}

#[tokio::test]
async fn test_structure_auto_defense_round_robin_no_volley_multiplication() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut dir_sess = login(&ctx, 560, "demo", "demo-secret", 90_000_001).await;
    dir_sess.corp_role = 1;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&90_000_001].corporation_id);
    let (lowsec_sys, h1, h2) = (30_002_718u32, 90_000_021i64, 90_000_022i64);
    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&90_000_002].clone();
        for (cid, sid) in [(h1, 921_001i64), (h2, 921_002i64)] {
            let mut ch = base.clone();
            ch.character_id = cid; ch.corporation_id = 98_000_599; ch.solar_system_id = lowsec_sys; ch.station_id = None; ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.items.insert(sid, eve_db::InventoryItemRecord { item_id: sid, type_id: 626, owner_id: cid, location_id: lowsec_sys as i64, flag: eve_db::InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default() });
            snap.item_positions.insert(sid, [15_000.0, 0.0, 0.0]);
            snap.corp_contacts.entry(corp_id).or_default().push(eve_db::SocialContactRecord { contact_id: cid, contact_type: "character".into(), standing: -5.0, is_watched: false, is_blocked: false, label_ids: Vec::new() });
        }
    });
    let prof = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("RR".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(true), PyRep::Real(0.0), PyRep::Bool(true)]), PyRep::Integer(0)])])]).await.unwrap();
    let st_id = 1_000_000_300_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_id, eve_db::UpwellStructureRecord { structure_id: st_id, type_id: 35833, owner_corp_id: corp_id, solar_system_id: lowsec_sys, state: eve_server::services::station_structure::structure_lifecycle::STATE_SHIELD_VULNERABLE.into(), position: [0.0, 0.0, 0.0], quantum_core_installed: true, ..Default::default() });
        snap.items.insert(911_001, eve_db::InventoryItemRecord { item_id: 911_001, type_id: 35922, owner_id: corp_id, location_id: st_id, flag: eve_db::InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
        snap.items.insert(911_011, eve_db::InventoryItemRecord { item_id: 911_011, type_id: 37847, owner_id: corp_id, location_id: st_id, flag: eve_db::InventoryFlag::HiSlot0, quantity: 10, singleton: false, version: 1, ..Default::default() });
        snap.module_online_states.insert(911_001, true);
    });
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(prof)]).await.unwrap();
    let sys_h = ctx.systems.get_or_spawn(lowsec_sys);
    for (sid, cid) in [(921_001i64, h1), (921_002i64, h2)] {
        let _ = sys_h.spawn_ball_with_meta(
            eve_destiny::Ball { id: sid, type_id: 626, position: glam::DVec3::new(15_000.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 250.0, inertia: 4.0, mass: 11_000_000.0, radius: 130.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop },
            ctx.build_ship_combat_state(cid, sid, 626),
            ctx.build_ship_slim_meta(cid, 626),
        ).await;
    }
    eve_server::services::structure_weapons_ops::trigger_system_structures_auto_defense(&ctx, lowsec_sys).await;
    let (cs1, cs2) = (sys_h.get_combat_state(921_001).await.unwrap().unwrap(), sys_h.get_combat_state(921_002).await.unwrap().unwrap());
    let (dmg1, dmg2) = (cs1.max_shield_hp - cs1.shield_hp, cs2.max_shield_hp - cs2.shield_hp);
    assert!(dmg1 > 0.0 && dmg2 == 0.0, "1 online module must hit only 1 target (got dmg1={dmg1}, dmg2={dmg2})");
}
}
