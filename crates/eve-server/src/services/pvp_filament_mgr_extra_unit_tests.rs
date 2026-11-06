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
        .find(|&fid| get_pvp_filament_match_spec(&sde, fid).banned_modules_list_id.is_some())
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

#[tokio::test]
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
        .find(|&fid| get_pvp_filament_match_spec(&ctx.sde, fid).banned_modules_list_id.is_some())
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
