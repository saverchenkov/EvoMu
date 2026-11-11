//! Additional unit and mutation-killing tests for `pvp_filament_mgr`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pvp_filament_mgr::*;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::InvType;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_filament_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    meta_level: Option<i64>,
    attrs: HashMap<u16, f64>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1.0,
        volume: 0.1,
        capacity: 100.0,
        radius: 20.0,
        base_price: 1000.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: attrs,
    }
}

fn make_pvp_ctx() -> ServerContext {
    let base_ctx = ServerContext::new();
    let _ = get_pvp_filament_match_spec(&base_ctx.sde, 56_088);
    let mut sde = (*base_ctx.sde).clone();
    // Custom filament types for heuristic fallbacks (3v3, 2v2, FFA, 1v1) in group 4050
    sde.types.insert(
        990_001,
        make_filament_type(990_001, 4050, 66, "Glorification-3v3 Proving Filament", None, HashMap::new()),
    );
    sde.types.insert(
        990_002,
        make_filament_type(990_002, 4050, 66, "Glorification-2v2 Proving Filament", None, HashMap::new()),
    );
    sde.types.insert(
        990_003,
        make_filament_type(990_003, 4050, 66, "Glorification-FFA 1v1v1v1 Filament", None, HashMap::new()),
    );
    sde.types.insert(
        990_004,
        make_filament_type(990_004, 4050, 66, "Glorification-1v1 Proving Filament", None, HashMap::new()),
    );
    // High meta-level module (990_010, meta_level 6, attr 633=6.0)
    sde.types.insert(
        990_010,
        make_filament_type(
            990_010,
            60,
            7,
            "Officer Overdrive",
            Some(6),
            HashMap::from([(633, 6.0)]),
        ),
    );
    // Banned Cloaking Device (990_011, group 330)
    sde.types.insert(
        990_011,
        make_filament_type(990_011, 330, 7, "Prototype Cloaking Device I", Some(0), HashMap::new()),
    );
    // High-grade implant (990_012, meta_level 6, attr 633=6.0)
    sde.types.insert(
        990_012,
        make_filament_type(
            990_012,
            300,
            20,
            "High-grade Asklepian Alpha",
            Some(6),
            HashMap::from([(633, 6.0)]),
        ),
    );
    let banned_fid = sde
        .types
        .keys()
        .copied()
        .find(|&fid| {
            let sp = get_pvp_filament_match_spec(&sde, fid);
            sp.fleet_size == 1 && sp.banned_modules_list_id.is_some()
        })
        .unwrap_or(56_088);
    sde.types.insert(
        banned_fid,
        make_filament_type(banned_fid, 4050, 66, "Glorification-1v1 T1 Frigate Filament", None, HashMap::new()),
    );
    let sp56 = get_pvp_filament_match_spec(&sde, banned_fid);
    if let Some(ship_lid) = sp56.ship_restrictions_list_id {
        sde.type_lists.entry(ship_lid).or_insert_with(|| eve_sde::TypeListInfo {
            type_list_id: ship_lid,
            name: String::new(),
            included_type_ids: Vec::new(),
            included_group_ids: Vec::new(),
            included_category_ids: Vec::new(),
            excluded_type_ids: Vec::new(),
            excluded_group_ids: Vec::new(),
            excluded_category_ids: Vec::new(),
        }).included_type_ids.push(587);
    }
    if let Some(ban_lid) = sp56.banned_modules_list_id {
        sde.type_lists.entry(ban_lid).or_insert_with(|| eve_sde::TypeListInfo {
            type_list_id: ban_lid,
            name: String::new(),
            included_type_ids: Vec::new(),
            included_group_ids: Vec::new(),
            included_category_ids: Vec::new(),
            excluded_type_ids: Vec::new(),
            excluded_group_ids: Vec::new(),
            excluded_category_ids: Vec::new(),
        }).included_type_ids.push(990_011);
    }

    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

#[test]
fn date_parsing_fallback_specs_and_leaderboard_tiebreakers() {
    let ctx = make_pvp_ctx();
    let s3v3 = get_pvp_filament_match_spec(&ctx.sde, 990_001);
    assert_eq!((s3v3.fleet_size, s3v3.number_of_fleets), (3, 2));
    let s2v2 = get_pvp_filament_match_spec(&ctx.sde, 990_002);
    assert_eq!((s2v2.fleet_size, s2v2.number_of_fleets), (2, 2));
    let sffa = get_pvp_filament_match_spec(&ctx.sde, 990_003);
    assert_eq!((sffa.fleet_size, sffa.number_of_fleets), (1, 4));
    let s1v1 = get_pvp_filament_match_spec(&ctx.sde, 990_004);
    assert_eq!((s1v1.fleet_size, s1v1.number_of_fleets), (1, 2));

    // Leaderboard tie-breaking: score desc -> wins desc -> kills desc -> damage desc -> char_id asc
    let mut store = PvpFilamentStore::default();
    let mut ev_map = HashMap::new();
    // 201 vs 202: same score (300), 202 has more wins (4 vs 3)
    ev_map.insert(201, (3, 1, 300, 3, 10_000.0));
    ev_map.insert(202, (4, 1, 300, 3, 10_000.0));
    // 203 vs 204: same score (200) and wins (2), 204 has more kills (5 vs 2)
    ev_map.insert(203, (2, 1, 200, 2, 10_000.0));
    ev_map.insert(204, (2, 1, 200, 5, 10_000.0));
    // 205 vs 206: same score (100), wins (1), kills (1), 206 has more damage (9000 vs 5000)
    ev_map.insert(205, (1, 1, 100, 1, 5_000.0));
    ev_map.insert(206, (1, 1, 100, 1, 9_000.0));
    // 207 vs 208: identical stats -> lower char_id (207) ranks first
    ev_map.insert(208, (1, 1, 100, 1, 1_000.0));
    ev_map.insert(207, (1, 1, 100, 1, 1_000.0));
    store.event_stats.insert(777, ev_map);

    let rows = build_leaderboard_rows_for_event(&store, 777);
    let ordered_ids: Vec<i64> = rows
        .iter()
        .filter_map(|r| r.dict_get("characterID").and_then(|v| v.as_i64()))
        .collect();
    assert_eq!(ordered_ids, vec![202, 201, 204, 203, 206, 205, 207, 208]);

    let dict_rows = build_leaderboard_rows_dict_for_event(&store, 777);
    assert_eq!(dict_rows.len(), 8);
    let c207 = build_character_statistics_dict_for_event(&store, 777, 207);
    assert_eq!(c207.dict_get("rank").and_then(|v| v.as_i64()), Some(7));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_pvp_queue_validation_errors_and_full_1v1_matchmaking_and_end_gate() {
    let ctx = make_pvp_ctx();
    let pilot_a = 960_001_i64;
    let pilot_b = 960_002_i64;
    let ship_a = 89_000_001_i64;
    let ship_b = 89_000_002_i64;
    let fil_a = 89_000_011_i64;
    let fil_b = 89_000_012_i64;
    let now = ctx.now_unix() as i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        for (cid, sid, name) in [(pilot_a, ship_a, "DuelistA"), (pilot_b, ship_b, "DuelistB")] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.solar_system_id = 30_000_142;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);

            // Rifter (587) in space
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: 30_000_142,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }

        // Filament stack of 2 in ship_a Cargo; single filament in ship_b Cargo
        snap.items.insert(
            fil_a,
            InventoryItemRecord {
                item_id: fil_a,
                type_id: 990_004,
                owner_id: pilot_a,
                location_id: ship_a,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            fil_b,
            InventoryItemRecord {
                item_id: fil_b,
                type_id: 990_004,
                owner_id: pilot_b,
                location_id: ship_b,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );

        let mut st = load_pvp_store(snap);
        st.seeded_default_events = true;
        st.events = vec![
            PvpEventSchedule {
                event_id: 301,
                schedule_id: 1,
                event_name: "Active 1v1 Frigate".into(),
                description: "1v1 T1 Frigate".into(),
                match_type_id: 10,
                start_unix: now - 3_600,
                end_unix: now + 3_600,
                type_ids: vec![990_004],
            },
            PvpEventSchedule {
                event_id: 302,
                schedule_id: 2,
                event_name: "Expired 2v2".into(),
                description: "2v2".into(),
                match_type_id: 11,
                start_unix: now - 7_200,
                end_unix: now - 3_600,
                type_ids: vec![990_002],
            },
        ];
        save_pvp_store(snap, &st);
    });

    let mut sess_a = SessionState::default();
    sess_a.user_id = Some(1);
    sess_a.character_id = Some(pilot_a);
    sess_a.ship_id = Some(ship_a);
    sess_a.solar_system_id = Some(30_000_142);

    // 1. Missing args -> InvalidArgument
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // 2. Item owned by pilot_b -> ItemNotOwned
    assert_eq!(
        handle(
            &ctx,
            &mut sess_a,
            "pvpFilamentMgr",
            "JoinPVPQueue",
            &[PyRep::Integer(fil_b)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotOwned"
    );

    // 3. Fitted high-meta module (990_010) -> InvalidModuleTierForFilament
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            89_000_020,
            InventoryItemRecord {
                item_id: 89_000_020,
                type_id: 990_010,
                owner_id: pilot_a,
                location_id: ship_a,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess_a,
            "pvpFilamentMgr",
            "JoinPVPQueue",
            &[PyRep::Integer(fil_a)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidModuleTierForFilament"
    );

    // 4. Replace with banned Cloaking Device (990_011) on filament with banned_modules_list_id -> BannedModuleForFilament
    let banned_fid = ctx
        .sde
        .types
        .keys()
        .copied()
        .find(|&fid| {
            let sp = get_pvp_filament_match_spec(&ctx.sde, fid);
            sp.fleet_size == 1 && sp.banned_modules_list_id.is_some()
        })
        .unwrap_or(56_088);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_pvp_store(snap);
        st.events[0].type_ids.push(i64::from(banned_fid));
        save_pvp_store(snap, &st);
        snap.items.get_mut(&fil_a).unwrap().type_id = banned_fid;
        snap.items.get_mut(&89_000_020).unwrap().type_id = 990_011;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess_a,
            "pvpFilamentMgr",
            "JoinPVPQueue",
            &[PyRep::Integer(fil_a)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "BannedModuleForFilament"
    );

    // 5. Remove banned module, add high-grade implant (990_012) -> InvalidImplantsForFilament
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fil_a).unwrap().type_id = 990_004;
        snap.items.remove(&89_000_020);
        snap.active_implants.insert(pilot_a, vec![990_012]);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess_a,
            "pvpFilamentMgr",
            "JoinPVPQueue",
            &[PyRep::Integer(fil_a)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidImplantsForFilament"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&pilot_a);
    });

    // 6. Non-boss fleet member -> NotFleetBoss
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            601,
            FleetRecord {
                fleet_id: 601,
                boss_id: pilot_b,
                members: vec![pilot_b, pilot_a],
                ..Default::default()
            },
        );
        snap.characters.get_mut(&pilot_a).unwrap().fleet_id = Some(601);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess_a,
            "pvpFilamentMgr",
            "JoinPVPQueue",
            &[PyRep::Integer(fil_a)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotFleetBoss"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.remove(&601);
        snap.characters.get_mut(&pilot_a).unwrap().fleet_id = None;
    });

    // 7. Pilot A joins 1v1 queue -> decrements fil_a stack from 2 to 1, returns Integer(30)
    let join_a = handle(
        &ctx,
        &mut sess_a,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(fil_a)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Pilot A JoinPVPQueue");
    assert_eq!(join_a, PyRep::Integer(30));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&fil_a].quantity, 1);
    });

    // LeavePVPQueue for Pilot A merges refunded filament back into existing stack fil_a (1 -> 2)
    handle(
        &ctx,
        &mut sess_a,
        "pvpFilamentMgr",
        "LeavePVPQueue",
        &[PyRep::Integer(fil_a)],
        &kw,
    )
    .await
    .unwrap()
    .expect("LeavePVPQueue merge");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&fil_a].quantity, 2);
    });

    // Re-join Pilot A, then Pilot B joins -> triggers 1v1 arena match in system 32_000_001 (returns Integer(5))!
    let _ = handle(
        &ctx,
        &mut sess_a,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(fil_a)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    let mut sess_b = SessionState::default();
    sess_b.user_id = Some(2);
    sess_b.character_id = Some(pilot_b);
    sess_b.ship_id = Some(ship_b);
    sess_b.solar_system_id = Some(30_000_142);

    let join_b = handle(
        &ctx,
        &mut sess_b,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(fil_b)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Pilot B JoinPVPQueue triggers match");
    assert_eq!(join_b, PyRep::Integer(5));
    assert_eq!(sess_b.solar_system_id, Some(32_000_001));

    let end_gate_id = ctx.db.with_snapshot(|s| {
        let st = load_pvp_store(s);
        assert!(st.queue.is_empty());
        assert_eq!(st.active_matches.len(), 1);
        st.active_matches.values().next().unwrap().end_gate_id
    });

    // Spawn Pilot A and Pilot B ship balls near the end gate in arena system 32_000_001
    let arena_handle = ctx.systems.get_or_spawn(32_000_001);
    for sid in [ship_a, ship_b] {
        let _ = arena_handle.remove_ball(sid).await;
        arena_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: DVec3::new(50_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
    }

    // Before match concludes, AbyssalPVPEndGateActivation fails with MatchNotConcluded
    sess_a.solar_system_id = Some(32_000_001);
    assert_eq!(
        handle(
            &ctx,
            &mut sess_a,
            "pvpFilamentMgr",
            "AbyssalPVPEndGateActivation",
            &[PyRep::Integer(end_gate_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MatchNotConcluded"
    );

    // Conclude match (Pilot A kills Pilot B in arena) and activate end gate to return to origin system (30_000_142)
    record_proving_grounds_combat_result_if_queued(&ctx, pilot_a, pilot_b, 32_000_001, 5_000.0);
    let exit_res = handle(
        &ctx,
        &mut sess_a,
        "pvpFilamentMgr",
        "AbyssalPVPEndGateActivation",
        &[PyRep::Integer(end_gate_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("End gate activation");
    assert_eq!(exit_res.dict_get("activated"), Some(&PyRep::Bool(true)));
    assert_eq!(exit_res.dict_get("originSystemID"), Some(&PyRep::Integer(30_000_142)));
    assert_eq!(sess_a.solar_system_id, Some(30_000_142));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proving_grounds_fleet_queue_errors_type_list_limits_and_ship_heuristics() {
    let mut ctx = make_pvp_ctx();
    // Pick real SDE filaments with banned / limited / ship-restriction specs from fsdbinary
    let sde_snapshot = ctx.sde.clone();
    let is_active_pvp_filament = |fid: u32| {
        sde_snapshot.types.get(&fid).is_some_and(|t| {
            (t.group_id == 4050 || t.group_id == 4743) && !t.name.starts_with("Expired")
        })
    };
    let pick_spec_fid = |pred: fn(&crate::services::pvp_filament_mgr::PvpFilamentMatchSpec) -> bool| -> Option<u32> {
        sde_snapshot
            .types
            .keys()
            .copied()
            .filter(|&fid| is_active_pvp_filament(fid) && pred(&get_pvp_filament_match_spec(&sde_snapshot, fid)))
            .min()
            .or_else(|| {
                sde_snapshot
                    .types
                    .keys()
                    .copied()
                    .filter(|&fid| pred(&get_pvp_filament_match_spec(&sde_snapshot, fid)))
                    .min()
            })
    };
    let ban_fid = pick_spec_fid(|sp| sp.fleet_size == 1 && sp.banned_modules_list_id.is_some()).unwrap_or(56_088);
    let ban_sp = get_pvp_filament_match_spec(&sde_snapshot, ban_fid);
    let lim_fid = pick_spec_fid(|sp| {
        sp.fleet_size == 1 && sp.limited_modules_list_id.is_some() && sp.number_of_limited_modules > 0
    });
    let sde_mut = Arc::make_mut(&mut ctx.sde);
    if let Some(ban_lid) = ban_sp.banned_modules_list_id {
        sde_mut.type_lists.entry(ban_lid).or_insert_with(|| eve_sde::TypeListInfo {
            type_list_id: ban_lid,
            name: "BannedMods".into(),
            included_category_ids: vec![],
            included_group_ids: vec![],
            included_type_ids: vec![],
            excluded_category_ids: vec![],
            excluded_group_ids: vec![],
            excluded_type_ids: vec![],
        }).included_type_ids.push(990_011);
    }
    if let Some(ship_lid) = ban_sp.ship_restrictions_list_id {
        sde_mut.type_lists.entry(ship_lid).or_insert_with(|| eve_sde::TypeListInfo {
            type_list_id: ship_lid,
            name: "AllowedShips".into(),
            included_category_ids: vec![],
            included_group_ids: vec![],
            included_type_ids: vec![],
            excluded_category_ids: vec![],
            excluded_group_ids: vec![],
            excluded_type_ids: vec![],
        }).included_type_ids.push(587);
    }
    sde_mut.types.insert(990_013, make_filament_type(990_013, 60, 7, "T2 Limited Mod", Some(5), HashMap::from([(633, 5.0)])));
    if let Some(lfid) = lim_fid {
        let lsp = get_pvp_filament_match_spec(&sde_snapshot, lfid);
        if let Some(lim_lid) = lsp.limited_modules_list_id {
            sde_mut.type_lists.entry(lim_lid).or_insert_with(|| eve_sde::TypeListInfo {
                type_list_id: lim_lid,
                name: "LimitedMods".into(),
                included_category_ids: vec![],
                included_group_ids: vec![],
                included_type_ids: vec![],
                excluded_category_ids: vec![],
                excluded_group_ids: vec![],
                excluded_type_ids: vec![],
            }).included_type_ids.push(990_013);
        }
        if let Some(ship_lid) = lsp.ship_restrictions_list_id {
            sde_mut.type_lists.entry(ship_lid).or_insert_with(|| eve_sde::TypeListInfo {
                type_list_id: ship_lid,
                name: "AllowedShipsLim".into(),
                included_category_ids: vec![],
                included_group_ids: vec![],
                included_type_ids: vec![],
                excluded_category_ids: vec![],
                excluded_group_ids: vec![],
                excluded_type_ids: vec![],
            }).included_type_ids.push(587);
        }
    }
    // Ensure ban_fid / lim_fid have valid non-expired PvP filament group/name so is_sde_pvp_filament passes even if seed omits or marks Expired
    for fid in std::iter::once(ban_fid).chain(lim_fid) {
        if let Some(t) = sde_mut.types.get_mut(&fid) {
            if t.group_id != 4050 && t.group_id != 4743 {
                t.group_id = 4050;
            }
            if t.name.starts_with("Expired") {
                t.name = format!("Active Test 1v1 Filament {fid}");
            }
        } else {
            sde_mut.types.insert(
                fid,
                make_filament_type(fid, 4050, 66, "Restricted 1v1 Proving Filament", None, HashMap::new()),
            );
        }
    }
    // Custom filaments using name heuristics (Battleship, Battlecruiser, Cruiser, Destroyer, Frigate)
    for (tid, name) in [
        (990_061, "Test Battleship Proving Filament"),
        (990_062, "Test Battlecruiser Proving Filament"),
        (990_063, "Test Cruiser Proving Filament"),
        (990_064, "Test Destroyer Proving Filament"),
        (990_065, "Test Frigate Proving Filament"),
    ] {
        sde_mut.types.insert(
            tid,
            make_filament_type(tid, 4050, 66, name, None, HashMap::new()),
        );
    }
    // Ship type IDs for Battleship(27), Battlecruiser(419), Cruiser(26), Destroyer(420), Titan(30)
    sde_mut.types.insert(990_201, make_filament_type(990_201, 27, 6, "Test BS", None, HashMap::new()));
    sde_mut.types.insert(990_202, make_filament_type(990_202, 419, 6, "Test BC", None, HashMap::new()));
    sde_mut.types.insert(990_203, make_filament_type(990_203, 26, 6, "Test CR", None, HashMap::new()));
    sde_mut.types.insert(990_204, make_filament_type(990_204, 420, 6, "Test DD", None, HashMap::new()));
    sde_mut.types.insert(990_205, make_filament_type(990_205, 30, 6, "Test Titan", None, HashMap::new()));

    // Verify ship restriction list + name heuristic branches through JoinPVPQueue
    let pilot_a = 95_001_001_i64;
    let pilot_b = 95_001_002_i64;
    let ship_a = 95_101_001_i64;
    let ship_b = 95_101_002_i64;
    let sys_id = 30_000_142_u32;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (sid, pos) in [(ship_a, DVec3::new(5e7, 0.0, 0.0)), (ship_b, DVec3::new(5e7 + 1e5, 0.0, 0.0))] {
        sys_h
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
    }

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().unwrap();
        for (pid, sid) in [(pilot_a, ship_a), (pilot_b, ship_b)] {
            let mut ch = base_ch.clone();
            ch.character_id = pid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(pid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: pid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        // Give pilot_a filaments of various types:
        // 95_200_000..=004 -> 990_061..=990_065 (heuristics)
        // 95_200_005 -> ban_fid
        // 95_200_006 -> lim_fid.unwrap_or(ban_fid)
        // 95_200_007 -> 990_002 (2v2)
        for (idx, tid) in [990_061, 990_062, 990_063, 990_064, 990_065, ban_fid, lim_fid.unwrap_or(ban_fid), 990_002].iter().enumerate() {
            let fid = 95_200_000 + idx as i64;
            snap.items.insert(
                fid,
                InventoryItemRecord {
                    item_id: fid,
                    type_id: *tid,
                    owner_id: pilot_a,
                    location_id: ship_a,
                    flag: InventoryFlag::Cargo,
                    quantity: 5,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        let mut store = load_pvp_store(snap);
        let now_i = ctx.now_unix() as i64;
        store.events.push(PvpEventSchedule {
            event_id: 901,
            event_name: "ActiveNow".into(),
            description: String::new(),
            match_type_id: 990_065,
            schedule_id: 901,
            start_unix: now_i - 100,
            end_unix: now_i + 1000,
            type_ids: vec![990_065],
        });
        store.events.push(PvpEventSchedule {
            event_id: 902,
            event_name: "ExpiredEvent".into(),
            description: String::new(),
            match_type_id: 990_061,
            schedule_id: 902,
            start_unix: now_i - 2000,
            end_unix: now_i - 1000,
            type_ids: vec![990_061],
        });
        store.seeded_default_events = true;
        save_pvp_store(snap, &store);
    });

    let mut sess_a = SessionState {
        user_id: Some(1),
        character_id: Some(pilot_a),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_a),
        ..Default::default()
    };
    let kw = HashMap::new();

    // Missing item / type mismatch / inactive event
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(99_999_999), PyRep::Integer(0)], &kw)
            .await.unwrap().unwrap_err().code,
        "ItemNotFound"
    );
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_005), PyRep::Integer(587)], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidFilamentType"
    );
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_000), PyRep::Integer(990_061)], &kw)
            .await.unwrap().unwrap_err().code,
        "EventNotActive"
    );

    // Ship restriction list + name heuristic mismatches (using 990_061..990_064 on a Rifter 587)
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.events.clear();
        save_pvp_store(snap, &store);
    });
    for (idx, tid) in [990_061, 990_062, 990_063, 990_064].iter().enumerate() {
        let fid = 95_200_000 + idx as i64;
        assert_eq!(
            handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(fid), PyRep::Integer(*tid)], &kw)
                .await.unwrap().unwrap_err().code,
            "InvalidShipForFilament"
        );
    }
    // Titan ship (group 30) rejected on any filament
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_a).unwrap().type_id = 990_205;
    });
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_004), PyRep::Integer(990_065)], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidShipForFilament"
    );
    // Battleship (990_201) allowed by heuristic on Battleship filament? Reverse: rejected on Frigate heuristic (990_065)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_a).unwrap().type_id = 990_201;
    });
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_004), PyRep::Integer(990_065)], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidShipForFilament"
    );
    // Restore Rifter 587 and check banned module (if SDE has banned list), limited module cap (if SDE has limited list), unknown implant, high-grade implant
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_a).unwrap().type_id = 587;
        snap.items.insert(
            95_300_001,
            InventoryItemRecord {
                item_id: 95_300_001,
                type_id: 990_011,
                owner_id: pilot_a,
                location_id: ship_a,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    if ban_sp.banned_modules_list_id.is_some() {
        assert_eq!(
            handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_005)], &kw)
                .await.unwrap().unwrap_err().code,
            "BannedModuleForFilament"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&95_300_001);
    });
    if let Some(lfid) = lim_fid {
        let lsp = get_pvp_filament_match_spec(&ctx.sde, lfid);
        let limit_exceed = lsp.number_of_limited_modules + 1;
        ctx.db.with_mut_snapshot(|snap| {
            for i in 0..limit_exceed {
                snap.items.insert(
                    95_300_010 + i as i64,
                    InventoryItemRecord {
                        item_id: 95_300_010 + i as i64,
                        type_id: 990_013,
                        owner_id: pilot_a,
                        location_id: ship_a,
                        flag: InventoryFlag::MedSlot0,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            }
        });
        assert_eq!(
            handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_006)], &kw)
                .await.unwrap().unwrap_err().code,
            "InvalidModuleTierForFilament"
        );
        ctx.db.with_mut_snapshot(|snap| {
            for i in 0..limit_exceed {
                snap.items.remove(&(95_300_010 + i as i64));
            }
        });
    }
    // Check unknown implant + high-grade implant (> max_meta 5)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            95_300_020,
            InventoryItemRecord {
                item_id: 95_300_020,
                type_id: 999_888, // unknown implant
                owner_id: pilot_a,
                location_id: pilot_a,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_004)], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidImplantsForFilament"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&95_300_020);
        snap.active_implants.insert(pilot_a, vec![990_012]); // meta 6 > 5
    });
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_004)], &kw)
            .await.unwrap().unwrap_err().code,
        "InvalidImplantsForFilament"
    );

    // Fleet checks: NotFleetBoss, FleetMemberOutOfRange, FleetTooLargeForFilament, FleetTooSmallForFilament, AlreadyInPvpQueue, AlreadyInPvpMatch
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&pilot_a);
        snap.fleets.insert(
            555,
            FleetRecord {
                fleet_id: 555,
                boss_id: pilot_b,
                members: vec![pilot_a, pilot_b],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_007), PyRep::Integer(990_002)], &kw)
            .await.unwrap().unwrap_err().code,
        "NotFleetBoss"
    );
    // Make pilot_a boss, but ship_b is 100 km away (> 30 km) -> FleetMemberOutOfRange
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&555).unwrap().boss_id = pilot_a;
    });
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_007), PyRep::Integer(990_002)], &kw)
            .await.unwrap().unwrap_err().code,
        "FleetMemberOutOfRange"
    );
    // FleetTooLargeForFilament (2 pilots with 1v1 filament)
    let _ = sys_h.remove_ball(ship_b).await;
    sys_h.spawn_ball(Ball {
        id: ship_b,
        type_id: 587,
        position: DVec3::new(5e7 + 1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_004), PyRep::Integer(990_065)], &kw)
            .await.unwrap().unwrap_err().code,
        "FleetTooLargeForFilament"
    );
    // FleetTooSmallForFilament (solo pilot with 2v2 filament)
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.remove(&555);
    });
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_007), PyRep::Integer(990_002)], &kw)
            .await.unwrap().unwrap_err().code,
        "FleetTooSmallForFilament"
    );
    // AlreadyInPvpQueue & AlreadyInPvpMatch
    let _ = handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_004), PyRep::Integer(990_065)], &kw)
        .await.unwrap().unwrap();
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_004), PyRep::Integer(990_065)], &kw)
            .await.unwrap().unwrap_err().code,
        "AlreadyInPvpQueue"
    );
    let _ = handle(&ctx, &mut sess_a, "pvpFilamentMgr", "LeavePVPQueue", &[], &kw).await;
    sess_a.solar_system_id = Some(32_000_001);
    let arena_h = ctx.systems.get_or_spawn(32_000_001);
    arena_h.spawn_ball(Ball {
        id: ship_a,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();
    assert_eq!(
        handle(&ctx, &mut sess_a, "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(95_200_004), PyRep::Integer(990_065)], &kw)
            .await.unwrap().unwrap_err().code,
        "AlreadyInPvpMatch"
    );
}

#[tokio::test]
async fn mutants_cov_pvp_store_and_join_queue_char_item_guards() {
    let ctx = make_pvp_ctx();
    let pilot_id = 95_400_001_i64;
    let ship_id = 95_400_101_i64;
    let fil_id = 95_400_201_i64;
    let sys_id = 30_000_142_u32;

    // 1. save_pvp_store & load_pvp_store round-trip non-default state
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = PvpFilamentStore {
            seeded_default_events: true,
            ..Default::default()
        };
        store.events.push(PvpEventSchedule {
            event_id: 999,
            event_name: "RoundTripEvent".into(),
            description: String::new(),
            match_type_id: 990_004,
            schedule_id: 999,
            start_unix: 100,
            end_unix: i64::MAX / 2,
            type_ids: vec![990_004],
        });
        save_pvp_store(snap, &store);
        assert!(snap.pvp_filament_state.seeded_default_events);
        assert_eq!(snap.pvp_filament_state.events.len(), 1);
    });
    let loaded = ctx.db.with_snapshot(|snap| load_pvp_store(snap));
    assert!(loaded.seeded_default_events);
    assert_eq!(loaded.events.len(), 1);
    assert_eq!(loaded.events[0].event_id, 999);

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(5e7, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.events.clear();
        store.seeded_default_events = true;
        save_pvp_store(snap, &store);

        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = pilot_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(pilot_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: pilot_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            fil_id,
            InventoryItemRecord {
                item_id: fil_id,
                type_id: 990_004,
                owner_id: pilot_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(pilot_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // 2. Non-pvpFilamentMgr returns None
    let other_svc = ["other", "Svc"].concat();
    assert!(handle(&ctx, &mut sess, &other_svc, "JoinPVPQueue", &[], &kw).await.is_none());

    // 3. Character snapshot guards (station_id, structure_id, solar_system_id, active_ship_id) -> MustBeInSpace
    for (st_id, str_id, ch_sys, ch_ship) in [
        (Some(60_003_760), None, sys_id, ship_id),
        (None, Some(10_000_000_001), sys_id, ship_id),
        (None, None, sys_id + 1, ship_id),
        (None, None, sys_id, ship_id + 1),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&pilot_id).unwrap();
            ch.station_id = st_id;
            ch.structure_id = str_id;
            ch.solar_system_id = ch_sys;
            ch.active_ship_id = ch_ship;
        });
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "pvpFilamentMgr",
                "JoinPVPQueue",
                &[PyRep::Integer(fil_id), PyRep::Integer(990_004)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "MustBeInSpace"
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&pilot_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
    });

    // 4. Item guards (owner_id, location_id, flag, quantity < 1) -> exact per-guard codes
    for (owner, loc, flag, qty, expected_code) in [
        (pilot_id + 1, ship_id, InventoryFlag::Cargo, 1, "ItemNotOwned"),
        (pilot_id, ship_id + 1, InventoryFlag::Cargo, 1, "ItemNotInCargo"),
        (pilot_id, ship_id, InventoryFlag::Hangar, 1, "ItemNotInCargo"),
        (pilot_id, ship_id, InventoryFlag::Cargo, 0, "ItemNotFound"),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            let it = snap.items.get_mut(&fil_id).unwrap();
            it.owner_id = owner;
            it.location_id = loc;
            it.flag = flag;
            it.quantity = qty;
        });
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "pvpFilamentMgr",
                "JoinPVPQueue",
                &[PyRep::Integer(fil_id), PyRep::Integer(990_004)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            expected_code
        );
    }

    // 5. Restore valid item with quantity == 1 -> succeeds with non-default PyRep
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&fil_id).unwrap();
        it.owner_id = pilot_id;
        it.location_id = ship_id;
        it.flag = InventoryFlag::Cargo;
        it.quantity = 1;
    });
    let ok_rep = handle(
        &ctx,
        &mut sess,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(fil_id), PyRep::Integer(990_004)],
        &kw,
    )
    .await
    .unwrap()
    .expect("valid JoinPVPQueue with qty=1");
    assert_eq!(ok_rep, PyRep::Integer(30));
}
