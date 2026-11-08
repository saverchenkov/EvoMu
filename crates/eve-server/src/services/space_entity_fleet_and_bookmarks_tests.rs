//! Semantic unit tests split from `space_entity_ops_tests`.

#![forbid(unsafe_code)]

use super::*;

use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, ContainerLifetimeEntry, DroneSpaceState,
    ExtendedNpcEntity,
};
use crate::services::inventory_fitting::fitting_containers_ships;
use eve_db::types::FleetRole;
use eve_db::{
    BookmarkRecord, CorpGovState, CorporationRecord, InventoryFlag,
    UpwellStructureRecord,
};
use eve_destiny::{CombatState, MovementMode, TargetLock};
use eve_machonet::service_roles;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn behavior_debugging_clusters_and_extra_fleet_members() {
    let ctx = test_ctx();
    let (sys_id, char_id, corp_id) = (30_000_142, 90_000_001, 98_000_001);

    let mut player = make_session(char_id, corp_id, Some(sys_id), Some(55), service_roles::ROLE_PLAYER_DEFAULT);
    assert_eq!(handle(&ctx, &mut player, None, "entity", "EnableBehaviorDebugging", &[], &HashMap::new()).await.unwrap().unwrap_err().code, "InsufficientRole");
    assert_eq!(handle(&ctx, &mut player, None, "entity", "DisableBehaviorDebugging", &[], &HashMap::new()).await.unwrap().unwrap_err().code, "InsufficientRole");

    let mut gm = make_session(char_id, corp_id, Some(sys_id), Some(55), service_roles::ROLE_GM_DEFAULT);
    let en1 = handle(&ctx, &mut gm, None, "entity", "EnableBehaviorDebugging", &[PyRep::Integer(8001)], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&en1, "behaviorDebuggingEnabled").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(kv_field(&en1, "targetEntityID").and_then(|v| v.as_i64()), Some(8001));

    let en2 = handle(&ctx, &mut gm, None, "entity", "EnableBehaviorDebugging", &[], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&en2, "targetEntityID").and_then(|v| v.as_i64()), Some(char_id));
    let _ = handle(&ctx, &mut gm, None, "entity", "DisableBehaviorDebugging", &[PyRep::Integer(8001)], &HashMap::new()).await.unwrap().unwrap();
    let dis2 = handle(&ctx, &mut gm, None, "entity", "DisableBehaviorDebugging", &[], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&dis2, "behaviorDebuggingEnabled").and_then(|v| v.as_bool()), Some(false));

    with_mut_combat_runtime(&ctx, |rt| {
        for (bid, gid, arch, bounty) in [(901, 11, "Kiter", 15_000.0), (902, 11, "Kiter", 25_000.0), (903, 12, "Brawler", 50_000.0)] {
            rt.npc_entities.insert(
                bid,
                ExtendedNpcEntity {
                    ball_id: bid,
                    type_id: 23_793,
                    faction_id: 500_010,
                    archetype: arch.into(),
                    solar_system_id: sys_id,
                    spawn_group_id: gid,
                    target_id: None,
                    aggression_threat: HashMap::new(),
                    threat_table: HashMap::new(),
                    bounty_isk: bounty,
                    optimal_range: 15_000.0,
                    orbit_range: 12_000.0,
                    damage_vector: Default::default(),
                    entropic_cycles: 0,
                    drifter_overshield_hp: 0.0,
                    drifter_doomsday_fired: false,
                    web_speed_factor: 0.0,
                    web_range: 0.0,
                    scram_strength: 0.0,
                    scram_range: 0.0,
                    neut_amount: 0.0,
                    ecm_jammed: false,
                    remote_shield_rep: 0.0,
                    remote_armor_rep: 0.0,
                    remote_rep_range: 0.0,
                    tag_drop_type_id: None,
                    spawn_position: DVec3::ZERO,
                    sleeper_wh_class: 0,
                    custom_cargo_loot: None,
                },
            );
        }
    });

    let btm = handle(&ctx, &mut player, None, "entity", "GetBehaviorTreeManager", &[], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&btm, "activeTreesCount").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(kv_field(&btm, "managerStatus").and_then(|v| v.as_str()), Some("Active"));

    let clusters = handle(&ctx, &mut player, None, "entity", "GetDebugSpawnPoolFleetEnemyClusters", &[], &HashMap::new()).await.unwrap().unwrap();
    let PyRep::List(cluster_list) = clusters else { panic!("expected list") };
    assert_eq!(cluster_list.len(), 2);
    let total_threat: f64 = cluster_list.iter().filter_map(|c| kv_field(c, "threatScore").and_then(|v| v.as_f64())).sum();
    assert!((total_threat - 90_000.0).abs() < 1e-6);

    assert_eq!(handle(&ctx, &mut player, None, "entity", "GetExtraFleetMembers", &[], &HashMap::new()).await.unwrap().unwrap(), PyRep::List(Vec::new()));

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(700, make_fleet(700, char_id, &[(char_id, FleetRole::FleetCommander)]));
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            991,
            DroneSpaceState {
                ball_id: 991,
                drone_item_id: 991,
                type_id: 2454,
                group_id: 100,
                owner_char_id: 90_000_002,
                original_owner_char_id: 90_000_002,
                parent_ship_id: 56,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 1,
                assist_char_id: Some(char_id),
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 400.0,
                optimal_range: 5_000.0,
                damage_vector: Default::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });
    let extra = handle(&ctx, &mut player, None, "entity", "GetExtraFleetMembers", &[], &HashMap::new()).await.unwrap().unwrap();
    let PyRep::List(extra_list) = extra else { panic!("expected list") };
    assert_eq!(extra_list.len(), 1);
    assert_eq!(kv_field(&extra_list[0], "role").and_then(|v| v.as_str()), Some("Assist"));
}

#[tokio::test]
async fn open_shipping_unit_storage_and_abandon_loot_flows() {
    let mut ctx = test_ctx();
    let (sys_id, owner_id, other_id, corp_id) = (30_000_142, 90_000_001, 90_000_002, 98_000_001);
    let (ship_id, alsc_id, child_item_id, ore_item_id) = (1_020_000_001, 1_020_000_010, 1_020_000_011, 1_020_000_012);

    clone_type_with(&mut ctx.sde, 34, 991_001, 448, 2, 100.0, 2_000.0, &[]);
    clone_type_with(&mut ctx.sde, 34, 991_002, 18, 25, 1.0, 0.0, &[]);

    let handle_sys = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos) in [
        (ship_id, 606, DVec3::ZERO),
        (alsc_id, 991_001, DVec3::new(1_500.0, 0.0, 0.0)),
        (ore_item_id, 991_002, DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let _ = handle_sys.spawn_ball(make_ball(id, tid, pos)).await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(alsc_id, make_item(alsc_id, 991_001, owner_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(child_item_id, make_item(child_item_id, 34, owner_id, alsc_id, InventoryFlag::Cargo, 50));
        snap.items.insert(ore_item_id, make_item(ore_item_id, 991_002, owner_id, sys_id as i64, InventoryFlag::None, 10));
    });

    fitting_containers_ships::write_container_ship_state(&ctx, |st| {
        st.container_passwords.insert(alsc_id, "secret42".into());
    });

    let mut other_sess = make_session(other_id, 98_000_002, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER_DEFAULT);
    assert_eq!(handle(&ctx, &mut other_sess, None, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(ore_item_id)], &HashMap::new()).await.unwrap().unwrap_err().code, "InvalidTarget");
    assert_eq!(handle(&ctx, &mut other_sess, None, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(alsc_id)], &HashMap::new()).await.unwrap().unwrap_err().code, "AccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            701,
            make_fleet(701, owner_id, &[(owner_id, FleetRole::SquadMember), (other_id, FleetRole::SquadMember)]),
        );
    });
    assert_eq!(handle(&ctx, &mut other_sess, None, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(alsc_id), PyRep::String("wrong".into())], &HashMap::new()).await.unwrap().unwrap_err().code, "ALSCPasswordRequired");
    let open_ok = handle(&ctx, &mut other_sess, None, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(alsc_id), PyRep::String("secret42".into())], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&open_ok, "storageOpened").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(kv_field(&open_ok, "itemCount").and_then(|v| v.as_i64()), Some(1));

    let st_after = fitting_containers_ships::read_container_ship_state(&ctx);
    assert!(st_after.unlocked_containers.contains(&(alsc_id, other_id)));
    assert_eq!(st_after.alsc_logs.get(&alsc_id).map(|v| v.len()), Some(1));

    let mut owner_sess = make_session(owner_id, corp_id, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER_DEFAULT);
    assert_eq!(handle(&ctx, &mut owner_sess, None, "beyonce", "CmdAbandonLoot", &[PyRep::Integer(ore_item_id)], &HashMap::new()).await.unwrap().unwrap_err().code, "InvalidItemType");
    with_mut_combat_runtime(&ctx, |rt| {
        rt.container_lifetimes.insert(
            alsc_id,
            ContainerLifetimeEntry {
                item_id: alsc_id,
                solar_system_id: sys_id,
                type_id: 991_001,
                owner_id,
                created_unix: 1_000,
                lifetime_secs: 7_200,
                is_abandoned: false,
            },
        );
    });
    let ab_ok = handle(&ctx, &mut owner_sess, None, "beyonce", "CmdAbandonLoot", &[PyRep::List(vec![PyRep::Integer(alsc_id)])], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&ab_ok, "abandoned").and_then(|v| v.as_bool()), Some(true));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&alsc_id).map(|i| i.owner_id), Some(0));
        assert_eq!(snap.items.get(&child_item_id).map(|i| i.owner_id), Some(0));
    });
    assert!(with_combat_runtime(&ctx, |rt| rt.container_lifetimes.get(&alsc_id).is_some_and(|c| c.is_abandoned)));
}

#[tokio::test]
async fn fleet_regroup_tag_target_state_and_location_info() {
    let ctx = test_ctx();
    let (sys_id, fc_id, mem_id, fc_ship, mem_ship, target_id) = (
        30_000_142,
        90_000_001,
        90_000_002,
        1_030_000_001,
        1_030_000_002,
        1_030_000_099,
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&fc_id) {
            c.solar_system_id = sys_id;
            c.active_ship_id = fc_ship;
        }
        let mut ch2 = snap.characters.get(&fc_id).cloned().unwrap();
        ch2.character_id = mem_id;
        ch2.active_ship_id = mem_ship;
        ch2.solar_system_id = sys_id;
        snap.characters.insert(mem_id, ch2);
        snap.items.insert(mem_ship, make_item(mem_ship, 606, mem_id, sys_id as i64, InventoryFlag::None, 1));
        snap.fleets.insert(
            750,
            make_fleet(750, fc_id, &[(fc_id, FleetRole::FleetCommander), (mem_id, FleetRole::SquadMember)]),
        );
    });

    let handle_sys = ctx.systems.get_or_spawn(sys_id);
    for (id, pos) in [(fc_ship, DVec3::ZERO), (mem_ship, DVec3::new(10_000.0, 0.0, 0.0)), (target_id, DVec3::new(20_000.0, 0.0, 0.0))] {
        let _ = handle_sys.spawn_ball(make_ball(id, 606, pos)).await;
    }

    let mut fc_sess = make_session(fc_id, 98_000_001, Some(sys_id), Some(fc_ship), service_roles::ROLE_PLAYER_DEFAULT);
    let mut mem_sess = make_session(mem_id, 98_000_001, Some(sys_id), Some(mem_ship), service_roles::ROLE_PLAYER_DEFAULT);

    assert_eq!(handle(&ctx, &mut mem_sess, None, "beyonce", "CmdFleetRegroup", &[], &HashMap::new()).await.unwrap().unwrap_err().code, "FleetAccessDenied");
    assert_eq!(handle(&ctx, &mut fc_sess, None, "beyonce", "CmdFleetRegroup", &[], &HashMap::new()).await.unwrap().unwrap(), PyRep::None);
    let (_, mem_ball) = handle_sys.get_ball_snapshot(mem_ship).await.unwrap().unwrap();
    assert!(matches!(mem_ball.mode, MovementMode::Follow { target_id: id, .. } if id == fc_ship));

    assert_eq!(handle(&ctx, &mut fc_sess, None, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(target_id), PyRep::String("AB".into())], &HashMap::new()).await.unwrap().unwrap_err().code, "InvalidTargetTag");
    assert_eq!(handle(&ctx, &mut fc_sess, None, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(target_id), PyRep::String("A".into())], &HashMap::new()).await.unwrap().unwrap_err().code, "TargetNotLocked");

    let _ = handle_sys
        .add_target_lock(
            fc_ship,
            TargetLock {
                target_id,
                lock_time_ms: 2_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    assert_eq!(handle(&ctx, &mut fc_sess, None, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(target_id), PyRep::String("a".into())], &HashMap::new()).await.unwrap().unwrap(), PyRep::None);
    let fstate = handle(&ctx, &mut fc_sess, None, "beyonce", "GetFleetState", &[], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&fstate, "isLeader").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(kv_field(&fstate, "targetTags"), Some(&PyRep::Dict(vec![(PyRep::Integer(target_id), PyRep::String("A".into()))])));

    let loc_info = handle(&ctx, &mut fc_sess, None, "beyonce", "GetFleetLocationAndInfo", &[], &HashMap::new()).await.unwrap().unwrap();
    let PyRep::Dict(entries) = loc_info else { panic!("expected dict") };
    assert_eq!(entries.len(), 2);

    assert_eq!(handle(&ctx, &mut fc_sess, None, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(target_id), PyRep::String(String::new())], &HashMap::new()).await.unwrap().unwrap(), PyRep::None);
    let fstate2 = handle(&ctx, &mut fc_sess, None, "beyonce", "GetFleetState", &[], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&fstate2, "targetTags"), Some(&PyRep::Dict(Vec::new())));
}

#[tokio::test]
async fn bookmarks_self_destruct_corp_transfer_turbo_dock_and_telemetry() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_a, corp_b) = (30_000_142, 90_000_001, 98_000_001, 98_000_002);
    let (ship_id, struct_id, siphon_id) = (1_040_000_001, 1_040_000_050, 1_040_000_060);
    clone_type_with(&mut ctx.sde, 34, 991_999, 1250, 22, 50.0, 1_200.0, &[]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_b,
            CorporationRecord {
                corporation_id: corp_b,
                name: "Beta Corp".into(),
                ticker: "BETA".into(),
                ceo_id: 90_000_099,
                hq_station_id: Some(60003760),
                tax_rate: 0.0,
                master_wallet: Default::default(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_a,
                solar_system_id: sys_id,
                name: "Astrahus Alpha".into(),
                position: [250_000.0, 0.0, 0.0],
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
        snap.bookmarks.insert(101, BookmarkRecord { bookmark_id: 101, owner_id: char_id, label: "Warp".into(), solar_system_id: sys_id, x: 500_000.0, y: 0.0, z: 0.0, item_id: None, memo: "Warp Spot".into(), folder_id: None, is_shared: false });
        snap.bookmarks.insert(102, BookmarkRecord { bookmark_id: 102, owner_id: char_id, label: "Sub".into(), solar_system_id: sys_id, x: 10_000.0, y: 0.0, z: 0.0, item_id: None, memo: "Subwarp Spot".into(), folder_id: None, is_shared: false });
        snap.items.insert(siphon_id, make_item(siphon_id, 991_999, corp_a, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(siphon_id + 1, make_item(siphon_id + 1, 34, corp_a, siphon_id, InventoryFlag::Cargo, 6_000));
    });

    let handle_sys = ctx.systems.get_or_spawn(sys_id);
    let _ = handle_sys.spawn_ball_with_combat(make_ball(ship_id, 606, DVec3::ZERO), CombatState { capacitor: 500.0, max_capacitor: 500.0, ..Default::default() }).await;

    let mut sess = make_session(char_id, corp_a, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER_DEFAULT);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    assert_eq!(handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(102)], &HashMap::new()).await.unwrap().unwrap(), PyRep::None);
    assert!(matches!(handle_sys.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode, MovementMode::GotoDirection { .. }));
    assert_eq!(handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(101)], &HashMap::new()).await.unwrap().unwrap(), PyRep::None);
    assert!(matches!(handle_sys.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));

    assert_eq!(handle(&ctx, &mut sess, None, "beyonce", "CmdSelfDestructStructure", &[PyRep::Integer(struct_id)], &HashMap::new()).await.unwrap().unwrap(), PyRep::None);
    ctx.db.with_snapshot(|snap| assert!(snap.structures[&struct_id].self_destruct_at_unix.is_some()));
    assert_eq!(handle(&ctx, &mut sess, None, "beyonce", "CmdAbortSelfDestructStructure", &[PyRep::Integer(struct_id)], &HashMap::new()).await.unwrap().unwrap(), PyRep::None);
    ctx.db.with_snapshot(|snap| assert_eq!(snap.structures[&struct_id].self_destruct_at_unix, None));

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance.insert(corp_b, CorpGovState { accept_structures_set: true, accept_structures: false, ..Default::default() });
    });
    assert_eq!(handle(&ctx, &mut sess, None, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(struct_id), PyRep::Integer(corp_b)], &HashMap::new()).await.unwrap().unwrap_err().code, "CorpDoesNotAcceptStructures");
    ctx.db.with_mut_snapshot(|snap| { snap.corp_governance.get_mut(&corp_b).unwrap().accept_structures = true; });
    let xfer = handle(&ctx, &mut sess, None, "beyonce", "CmdTransferCorpAssetOwnership", &[PyRep::Integer(struct_id), PyRep::Integer(corp_b)], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&xfer, "ownerID").and_then(|v| v.as_i64()), Some(corp_b));

    let ratio = handle(&ctx, &mut sess, None, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_id)], &HashMap::new()).await.unwrap().unwrap().as_f64().unwrap();
    assert!((ratio - 0.05).abs() < 1e-6);
    assert!(handle(&ctx, &mut sess, None, "beyonce", "GetContainerExpiryDate", &[PyRep::Integer(siphon_id)], &HashMap::new()).await.unwrap().unwrap().as_i64().unwrap() > 0);
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "GetOperationSpawnpointPosition", &[PyRep::Integer(struct_id)], &HashMap::new()).await.unwrap().unwrap(),
        PyRep::Tuple(vec![PyRep::Real(250_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])
    );

    let PyRep::Tuple(rel_tup) = handle(&ctx, &mut sess, None, "beyonce", "GetRelativity", &[PyRep::Integer(99), PyRep::Integer(1_000)], &HashMap::new()).await.unwrap().unwrap() else { panic!("expected tuple") };
    assert_eq!(rel_tup.len(), 7);
    assert_eq!(rel_tup[0], PyRep::Integer(99));

    assert_eq!(handle(&ctx, &mut sess, None, "beyonce", "CmdTurboDock", &[PyRep::Integer(struct_id)], &HashMap::new()).await.unwrap().unwrap_err().code, "GmRoleRequired");
    sess.role |= service_roles::ROLE_GM_DEFAULT;
    let tdock = handle(&ctx, &mut sess, None, "beyonce", "CmdTurboDock", &[PyRep::Integer(struct_id)], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(kv_field(&tdock, "docked").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(sess.structure_id, Some(struct_id));
}

#[tokio::test]
async fn r9_p0_561_cmd_goto_bookmark_enforces_warp_cloak_interdiction_and_subgrid_immobilization() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 100_000_561_i64;
    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER_DEFAULT);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let far_bm = 950_561_i64;
    let near_bm = 950_562_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            far_bm,
            BookmarkRecord {
                bookmark_id: far_bm,
                owner_id: char_id,
                label: "Far Spot".into(),
                solar_system_id: sys_id,
                x: 500_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            near_bm,
            BookmarkRecord {
                bookmark_id: near_bm,
                owner_id: char_id,
                label: "Near Spot".into(),
                solar_system_id: sys_id,
                x: 50_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    // 1. Non-covert cloaked ship is rejected with CloakedCannotWarp and cloak remains intact
    let cloak_mod_id = 100_950_561_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: 11_370, // Prototype Cloaking Device I (non-covert)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 587, DVec3::ZERO),
            CombatState {
                is_cloaked: true,
                is_covert_cloak: false,
                gate_cloak_remaining_ticks: 0,
                capacitor: 500.0,
                max_capacitor: 500.0,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(far_bm)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CloakedCannotWarp"
    );
    assert!(
        sys_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .is_cloaked
    );

    // 2. Covert-ops cloaked ship warps cleanly while remaining cloaked
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&cloak_mod_id).unwrap().type_id = 11_578; // Covert Ops Cloaking Device II
    });
    let _ = sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                is_covert_cloak: true,
                gate_cloak_remaining_ticks: 0,
                capacitor: 500.0,
                max_capacitor: 500.0,
                ..Default::default()
            },
        )
        .await;
    handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(far_bm)], &HashMap::new())
        .await
        .unwrap()
        .expect("covert cloak bookmark warp succeeds");
    assert!(
        sys_handle
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .is_cloaked
    );

    // 3. Reset to stop, test sub-150km MJD/MJFG spooling, siege immobilization, & structure controller rejections
    let _ = sys_handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    let _ = sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                capacitor: 500.0,
                max_capacitor: 500.0,
                mjfg_spool_remaining_s: 5.0,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(near_bm)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantMoveWhileModuleActive"
    );
    let _ = sys_handle
        .set_combat_state(
            ship_id,
            CombatState {
                capacitor: 500.0,
                max_capacitor: 500.0,
                ..Default::default()
            },
        )
        .await;

    {
        let mut ts = ctx.tactical_state.write().unwrap();
        ts.lance_caster_lock_until
            .insert((ctx.instance_id as usize, ship_id), ctx.now_unix() + 60);
    }
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(near_bm)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "LanceFiringLockActive"
    );
    ctx.tactical_state.write().unwrap().lance_caster_lock_until.clear();

    // Normal sub-150km approach succeeds, then structure control is rejected
    handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(near_bm)], &HashMap::new())
        .await
        .unwrap()
        .expect("sub-150km bookmark approach succeeds");

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            ship_id,
            UpwellStructureRecord {
                structure_id: ship_id,
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                type_id: 35832,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(&ctx, &mut sess, None, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(near_bm)], &HashMap::new())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StructureCannotNavigate"
    );
}
