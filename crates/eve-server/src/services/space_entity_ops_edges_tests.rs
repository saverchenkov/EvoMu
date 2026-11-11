//! Additional edge-case and branch coverage tests for `space_entity_ops`.

#![forbid(unsafe_code)]

use super::super::*;
use super::{clone_type_with, kv_field, make_ball, make_item, make_session, test_ctx};
use eve_db::types::{
    corp_roles::{ROLE_DIRECTOR, ROLE_STARBASE_CONFIG, ROLE_STATION_MANAGER},
    BookmarkFolderRecord, BookmarkRecord, CorpMemberRoleAssignment, FleetMemberRecord, FleetRecord,
    FleetRole, PersistedSpaceObjectMeta, UpwellStructureRecord,
};
use eve_db::InventoryFlag;
use eve_destiny::{CombatState, MovementMode, TargetLock, WarpPhase};
use eve_machonet::service_roles;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandon_loot_all_error_and_group_paths_plus_slim_notification() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;

    clone_type_with(&mut ctx.sde, 34, 991_001, 186, 2, 1_000.0, 1_000.0, &[]); // Wreck
    clone_type_with(&mut ctx.sde, 34, 991_002, 12, 2, 100.0, 100.0, &[]); // Cargo Container
    clone_type_with(&mut ctx.sde, 34, 991_003, 999, 2, 100.0, 100.0, &[]); // Unsupported group

    let mut sess_nospace = make_session(char_id, corp_id, None, None, 0);
    let kw = HashMap::new();
    let e_nosys = handle(
        &ctx,
        &mut sess_nospace,
        None,
        "beyonce",
        "CmdAbandonLoot",
        &[PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(e_nosys.code, "NoSolarSystem");

    sess_nospace.solar_system_id = Some(sys_id);
    let mut sess = sess_nospace;

    // Empty list -> ItemNotFound
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdAbandonLoot",
            &[PyRep::List(vec![])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // Missing item -> ItemNotFound
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdAbandonLoot",
            &[PyRep::Integer(777_001)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    let wreck_wrong_owner = 810_001_i64;
    let wreck_docked = 810_002_i64;
    let wreck_empty = 810_003_i64;
    let cargo_full = 810_004_i64;
    let non_container = 810_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_wrong_owner,
            make_item(wreck_wrong_owner, 991_001, other_char, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            wreck_docked,
            make_item(wreck_docked, 991_001, char_id, 60_003_760, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            wreck_empty,
            make_item(wreck_empty, 991_001, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            cargo_full,
            make_item(cargo_full, 991_002, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            810_099,
            make_item(810_099, 34, char_id, cargo_full, InventoryFlag::Cargo, 50),
        );
        snap.items.insert(
            non_container,
            make_item(non_container, 991_003, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdAbandonLoot",
            &[PyRep::Integer(wreck_wrong_owner)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdAbandonLoot",
            &[PyRep::Integer(wreck_docked)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotInSpace"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            None,
            "beyonce",
            "CmdAbandonLoot",
            &[PyRep::Integer(non_container)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidItemType"
    );

    // Single integer arg abandons empty wreck (owner -> 0, OnSlimItemChange emitted with isEmpty=1)
    let res_wreck = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdAbandonLoot",
        &[PyRep::Integer(wreck_empty)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(kv_field(&res_wreck, "abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(kv_field(&res_wreck, "itemID"), Some(&PyRep::Integer(wreck_empty)));

    // Non-empty cargo container abandons cleanly
    let res_cargo = handle(
        &ctx,
        &mut sess,
        None,
        "beyonce",
        "CmdAbandonLoot",
        &[PyRep::List(vec![PyRep::Integer(cargo_full)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(kv_field(&res_cargo, "itemID"), Some(&PyRep::Integer(cargo_full)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&wreck_empty].owner_id, 0);
        assert_eq!(snap.items[&cargo_full].owner_id, 0);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fleet_regroup_and_tag_target_all_guards_and_scoping() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let leader_cid = 90_000_001_i64;
    let squad_cmd_cid = 90_000_002_i64;
    let squad_mate_ok = 90_000_003_i64;
    let squad_mate_no_regroup = 90_000_004_i64;
    let other_squad_cid = 90_000_005_i64;
    let corp_id = 98_000_001_i64;

    let cmd_ship = 210_001_i64;
    let mate_ship_ok = 210_002_i64;
    let mate_ship_no_regroup = 210_003_i64;
    let other_squad_ship = 210_004_i64;
    let offgrid_ship = 210_005_i64;
    let struct_vessel_id = 9_000_001_i64;

    let mut no_space = make_session(leader_cid, corp_id, None, Some(cmd_ship), 0);
    let kw = HashMap::new();
    assert_eq!(
        handle(&ctx, &mut no_space, None, "beyonce", "CmdFleetRegroup", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    no_space.solar_system_id = Some(sys_id);
    no_space.ship_id = None;
    assert_eq!(
        handle(&ctx, &mut no_space, None, "beyonce", "CmdFleetRegroup", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // Set up structure vessel, fleet 400 with squad 10 & squad 20
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_vessel_id,
            UpwellStructureRecord {
                structure_id: struct_vessel_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Struct".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 10,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        let base_ch = snap.characters[&leader_cid].clone();
        for (cid, sid, s_sys) in [
            (squad_cmd_cid, cmd_ship, sys_id),
            (squad_mate_ok, mate_ship_ok, sys_id),
            (squad_mate_no_regroup, mate_ship_no_regroup, sys_id),
            (other_squad_cid, other_squad_ship, sys_id),
            (90_000_006, offgrid_ship, sys_id),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.active_ship_id = sid;
            ch.solar_system_id = s_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.fleet_id = Some(400);
            snap.characters.insert(cid, ch);
        }
        let mk_mem = |cid: i64, role: FleetRole, squad_id: i64, accepts_regroup: bool| FleetMemberRecord {
            character_id: cid,
            role,
            wing_id: Some(1),
            squad_id: Some(squad_id),
            takes_fleet_warp: true,
            accepts_regroup,
            accepts_conduit_jumps: true,
            job: 0,
            role_name: String::new(),
            join_time_unix: 0,
        };
        let mut member_details = HashMap::new();
        member_details.insert(squad_cmd_cid, mk_mem(squad_cmd_cid, FleetRole::SquadCommander, 10, true));
        member_details.insert(squad_mate_ok, mk_mem(squad_mate_ok, FleetRole::SquadMember, 10, true));
        member_details.insert(
            squad_mate_no_regroup,
            mk_mem(squad_mate_no_regroup, FleetRole::SquadMember, 10, false),
        );
        member_details.insert(other_squad_cid, mk_mem(other_squad_cid, FleetRole::SquadMember, 20, true));
        member_details.insert(90_000_006, mk_mem(90_000_006, FleetRole::SquadMember, 10, true));

        snap.fleets.insert(
            400,
            FleetRecord {
                fleet_id: 400,
                boss_id: squad_cmd_cid,
                members: vec![squad_cmd_cid, squad_mate_ok, squad_mate_no_regroup, other_squad_cid, 90_000_006],
                wings: vec![1],
                squads: vec![10, 20],
                free_move: false,
                is_registered: false,
                member_details,
                ..Default::default()
            },
        );
    });

    // Structure vessel cannot issue fleet regroup
    let mut struct_sess = make_session(leader_cid, corp_id, Some(sys_id), Some(struct_vessel_id), 0);
    assert_eq!(
        handle(&ctx, &mut struct_sess, None, "beyonce", "CmdFleetRegroup", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureCannotNavigate"
    );

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (bid, pos) in [
        (cmd_ship, DVec3::new(0.0, 0.0, 0.0)),
        (mate_ship_ok, DVec3::new(10_000.0, 0.0, 0.0)),
        (mate_ship_no_regroup, DVec3::new(12_000.0, 0.0, 0.0)),
        (other_squad_ship, DVec3::new(14_000.0, 0.0, 0.0)),
        (offgrid_ship, DVec3::new(700_000.0, 0.0, 0.0)),
    ] {
        sys_h.spawn_ball(make_ball(bid, 587, pos)).await.unwrap();
    }

    let mut cmd_sess = make_session(squad_cmd_cid, corp_id, Some(sys_id), Some(cmd_ship), 0);
    let regrouped = handle(&ctx, &mut cmd_sess, None, "beyonce", "CmdFleetRegroup", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(regrouped, PyRep::None);
    // Only mate_ship_ok is in same squad 10 + accepts_regroup + on-grid (<= 500km)
    assert_eq!(
        sys_h.get_ball_snapshot(mate_ship_ok).await.unwrap().unwrap().1.mode,
        MovementMode::Follow { target_id: cmd_ship, range: 1_000.0 }
    );
    assert_eq!(
        sys_h.get_ball_snapshot(other_squad_ship).await.unwrap().unwrap().1.mode,
        MovementMode::Stop
    );
    assert_eq!(
        sys_h.get_ball_snapshot(offgrid_ship).await.unwrap().unwrap().1.mode,
        MovementMode::Stop
    );

    // CmdFleetTagTarget guards
    assert_eq!(
        handle(
            &ctx,
            &mut cmd_sess,
            None,
            "beyonce",
            "CmdFleetTagTarget",
            &[PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    let mut member_sess = make_session(squad_mate_ok, corp_id, Some(sys_id), Some(mate_ship_ok), 0);
    assert_eq!(
        handle(
            &ctx,
            &mut member_sess,
            None,
            "beyonce",
            "CmdFleetTagTarget",
            &[PyRep::Integer(mate_ship_no_regroup), PyRep::String("A".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetAccessDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut cmd_sess,
            None,
            "beyonce",
            "CmdFleetTagTarget",
            &[PyRep::Integer(mate_ship_ok), PyRep::String("*".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTargetTag"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut cmd_sess,
            None,
            "beyonce",
            "CmdFleetTagTarget",
            &[PyRep::Integer(999_888_777), PyRep::String("A".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotOnGrid"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut cmd_sess,
            None,
            "beyonce",
            "CmdFleetTagTarget",
            &[PyRep::Integer(mate_ship_ok), PyRep::String("1".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotLocked"
    );
    sys_h
        .add_target_lock(
            cmd_ship,
            TargetLock {
                target_id: mate_ship_ok,
                lock_time_ms: 1.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut cmd_sess,
            None,
            "beyonce",
            "CmdFleetTagTarget",
            &[PyRep::Integer(mate_ship_ok), PyRep::String("1".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(
        ctx.fleet_extras
            .lock()
            .unwrap()
            .target_tags
            .get(&400)
            .and_then(|m| m.get(&mate_ship_ok))
            .map(String::as_str),
        Some("1")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goto_bookmark_acl_corp_errors_and_warp_state() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 212_001_i64;
    let kw = HashMap::new();

    // NoActiveShip & NotInSpace(structure_id)
    assert_eq!(
        handle(
            &ctx,
            &mut make_session(char_id, corp_id, Some(sys_id), None, 0),
            None,
            "beyonce",
            "CmdGotoBookmark",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoActiveShip"
    );
    let mut struct_sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), 0);
    struct_sess.structure_id = Some(8_000_001);
    assert_eq!(
        handle(&ctx, &mut struct_sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSpace"
    );

    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), 0);
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(999_001)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkNotFound"
    );

    // Private bookmark owned by other_char, ACL folder bookmark, and wrong-system bookmark
    let acl_folder_id = 55_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_folders.insert(
            acl_folder_id,
            BookmarkFolderRecord {
                folder_id: acl_folder_id,
                owner_id: other_char,
                folder_name: "ACLUse".into(),
                is_shared: true,
                corporation_id: None,
                description: String::new(),
                admin_group_id: None,
                manage_group_id: None,
                use_group_id: Some(701),
                view_group_id: None,
            },
        );
        snap.access_groups.insert(701, vec![char_id]);
        snap.bookmarks.insert(
            66_001,
            BookmarkRecord {
                bookmark_id: 66_001,
                owner_id: other_char,
                label: "Secret".into(),
                solar_system_id: sys_id,
                x: 2_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            66_002,
            BookmarkRecord {
                bookmark_id: 66_002,
                owner_id: other_char,
                label: "SharedACL".into(),
                solar_system_id: sys_id,
                x: 2_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: Some(acl_folder_id),
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            66_003,
            BookmarkRecord {
                bookmark_id: 66_003,
                owner_id: char_id,
                label: "WrongSys".into(),
                solar_system_id: 30_000_144,
                x: 2_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(66_001)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(66_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    // Missing ship ball -> ShipNotInSpace
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(66_002)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship ball stopped -> succeeds via ACL folder bookmark
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let b = make_ball(ship_id, 587, DVec3::ZERO);
    sys_h.spawn_ball(b).await.unwrap();
    let res = handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(66_002)], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(res, PyRep::None));

    // Put ship in Warp -> ShipNotInSpace
    let warp_mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1_000_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1_000_000.0,
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    sys_h.set_movement_mode(ship_id, warp_mode).await.unwrap();
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(66_002)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_destruct_transfer_corp_asset_turbo_dock_unanchor_shipping_and_getters() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let char_mgr = 90_000_010_i64;
    let corp_id = 98_000_001_i64;
    let target_corp_id = 98_000_002_i64;
    let ship_id = 213_001_i64;
    let structure_id = 9_100_001_i64;
    let kw = HashMap::new();

    clone_type_with(&mut ctx.sde, 34, 991_101, 1250, 2, 100.0, 100.0, &[]); // Mobile Depot
    clone_type_with(&mut ctx.sde, 34, 991_102, 448, 2, 100.0, 100.0, &[]); // Audit Log Secure Container (ALSC)
    clone_type_with(&mut ctx.sde, 34, 991_103, 1246, 2, 100.0, 200.0, &[]); // Siphon unit

    let deployable_corp = 820_001_i64;
    let shipping_id = 820_002_i64;
    let siphon_id = 820_003_i64;
    let empty_cap_siphon = 820_004_i64;
    let container_id = 820_005_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Citadel".into(),
                position: [1_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        // Set up char_mgr with ROLE_STATION_MANAGER and ROLE_STARBASE_CONFIG for corp_id
        let mut ch_mgr = snap.characters[&char_id].clone();
        ch_mgr.character_id = char_mgr;
        ch_mgr.corporation_id = corp_id;
        ch_mgr.active_ship_id = ship_id;
        ch_mgr.solar_system_id = sys_id;
        ch_mgr.station_id = None;
        snap.characters.insert(char_mgr, ch_mgr);
        snap.corp_role_assignments.insert(
            (corp_id, char_mgr),
            CorpMemberRoleAssignment {
                character_id: char_mgr,
                roles: ROLE_STATION_MANAGER | ROLE_STARBASE_CONFIG | ROLE_DIRECTOR,
                grantable_roles: 0,
                roles_at_hq: 0,
                grantable_roles_at_hq: 0,
                roles_at_base: 0,
                grantable_roles_at_base: 0,
                roles_at_other: 0,
                grantable_roles_at_other: 0,
            },
        );

        let mut base_corp = snap.corporations.values().next().unwrap().clone();
        base_corp.corporation_id = target_corp_id;
        base_corp.name = "TargetCorp".into();
        snap.corporations.insert(target_corp_id, base_corp);

        // Corp-owned deployable in space
        snap.items.insert(
            deployable_corp,
            make_item(deployable_corp, 991_101, corp_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            shipping_id,
            make_item(shipping_id, 991_102, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            siphon_id,
            make_item(siphon_id, 991_103, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        // Ship inside siphon cargo: volume 1000 * 1 = 1000 > capacity 200 -> clamped to 1.0
        snap.items.insert(
            820_090,
            make_item(820_090, 991_101, char_id, siphon_id, InventoryFlag::Cargo, 5),
        );
        snap.items.insert(
            empty_cap_siphon,
            make_item(empty_cap_siphon, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            container_id,
            make_item(container_id, 991_102, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char_mgr, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    fitting_containers_ships::write_container_ship_state(&ctx, |st| {
        st.container_passwords.insert(shipping_id, "secret".into());
    });

    let mut mgr_sess = make_session(char_mgr, corp_id, Some(sys_id), Some(ship_id), 0);
    let mut gm_sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), service_roles::ROLE_GM_DEFAULT);
    let mut plain_sess = make_session(char_id, 98_999_999, Some(sys_id), Some(ship_id), 0);

    // SelfDestruct / AbortSelfDestruct ACLs
    assert_eq!(
        handle(
            &ctx,
            &mut plain_sess,
            None,
            "beyonce",
            "CmdSelfDestructStructure",
            &[PyRep::Integer(999_111)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "StructureNotFound"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut plain_sess,
            None,
            "beyonce",
            "CmdSelfDestructStructure",
            &[PyRep::Integer(structure_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut plain_sess,
            None,
            "beyonce",
            "CmdAbortSelfDestructStructure",
            &[PyRep::Integer(structure_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert!(handle(
        &ctx,
        &mut mgr_sess,
        None,
        "beyonce",
        "CmdSelfDestructStructure",
        &[PyRep::Integer(structure_id)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    assert!(handle(
        &ctx,
        &mut gm_sess,
        None,
        "beyonce",
        "CmdAbortSelfDestructStructure",
        &[PyRep::Integer(structure_id)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // CmdTransferCorpAssetOwnership guards
    assert_eq!(
        handle(
            &ctx,
            &mut plain_sess,
            None,
            "beyonce",
            "CmdTransferCorpAssetOwnership",
            &[PyRep::Integer(deployable_corp), PyRep::Integer(target_corp_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut mgr_sess,
            None,
            "beyonce",
            "CmdTransferCorpAssetOwnership",
            &[PyRep::Integer(deployable_corp), PyRep::Integer(999_999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTargetCorporation"
    );

    // CmdUnanchor on corp deployable: spawn balls first, then wrong corp -> AccessDenied; ROLE_STARBASE_CONFIG -> Ok
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_combat(make_ball(ship_id, 587, DVec3::ZERO), CombatState::default())
        .await
        .unwrap();
    sys_h
        .spawn_ball(make_ball(deployable_corp, 991_101, DVec3::new(1_000.0, 0.0, 0.0)))
        .await
        .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut plain_sess,
            None,
            "entity",
            "CmdUnanchor",
            &[PyRep::Integer(deployable_corp)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert!(handle(
        &ctx,
        &mut mgr_sess,
        None,
        "entity",
        "CmdUnanchor",
        &[PyRep::Integer(deployable_corp)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // OpenShippingUnitStorage: out of range -> TargetOutOfRange; GM owner succeeds
    sys_h
        .spawn_ball(make_ball(shipping_id, 991_102, DVec3::new(10_000.0, 0.0, 0.0)))
        .await
        .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            None,
            "entity",
            "OpenShippingUnitStorage",
            &[PyRep::Integer(shipping_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );
    assert!(sys_h.set_ball_position(shipping_id, DVec3::new(1_000.0, 0.0, 0.0)).await);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.space_object_meta.insert(
            shipping_id,
            PersistedSpaceObjectMeta {
                anchor_completed: false,
                anchor_start_unix: Some(1_000_000),
                ..Default::default()
            },
        );
    });
    assert!(handle(
        &ctx,
        &mut gm_sess,
        None,
        "entity",
        "OpenShippingUnitStorage",
        &[PyRep::Integer(shipping_id)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // GetCapacityOfSiphon, GetContainerExpiryDate, GetFleetLocationAndInfo, GetFleetState, GetOperationSpawnpointPosition, GetRelativity
    let siphon_full = handle(
        &ctx,
        &mut gm_sess,
        None,
        "beyonce",
        "GetCapacityOfSiphon",
        &[PyRep::Integer(siphon_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(siphon_full, PyRep::Real(1.0));
    let siphon_zero_cap = handle(
        &ctx,
        &mut gm_sess,
        None,
        "beyonce",
        "GetCapacityOfSiphon",
        &[PyRep::Integer(empty_cap_siphon)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(siphon_zero_cap, PyRep::Real(0.0));

    let exp = handle(
        &ctx,
        &mut gm_sess,
        None,
        "beyonce",
        "GetContainerExpiryDate",
        &[PyRep::Integer(container_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(exp, PyRep::Integer(v) if v > 0));

    assert_eq!(
        handle(&ctx, &mut gm_sess, None, "beyonce", "GetFleetLocationAndInfo", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Dict(vec![])
    );
    assert_eq!(
        handle(&ctx, &mut gm_sess, None, "beyonce", "GetFleetState", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm_sess,
            None,
            "beyonce",
            "GetOperationSpawnpointPosition",
            &[PyRep::Integer(820_090)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SiteNotFound"
    );
    let rel = handle(
        &ctx,
        &mut gm_sess,
        None,
        "beyonce",
        "GetRelativity",
        &[PyRep::Integer(i64::MAX / 2)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    match &rel {
        PyRep::Tuple(elems) => {
            assert_eq!(elems.len(), 7);
            assert_eq!(elems[0], PyRep::Integer(i64::MAX / 2));
            assert_eq!(elems[3], PyRep::Integer(0));
        }
        other => panic!("expected GetRelativity Tuple, got {other:?}"),
    }

    // CmdTurboDock requires GM role, then docks into an SDE station
    assert_eq!(
        handle(
            &ctx,
            &mut mgr_sess,
            None,
            "beyonce",
            "CmdTurboDock",
            &[PyRep::Integer(60_003_760)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "GmRoleRequired"
    );
    if let Some(&station_id) = ctx.sde.stations.keys().next() {
        let mut dock_sess = make_session(char_mgr, corp_id, Some(sys_id), Some(ship_id), service_roles::ROLE_GM_DEFAULT);
        let _docked = handle(
            &ctx,
            &mut dock_sess,
            None,
            "beyonce",
            "CmdTurboDock",
            &[PyRep::Integer(station_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&char_mgr].station_id, Some(station_id));
        });
    }
}
