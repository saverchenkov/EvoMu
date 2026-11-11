use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;

#[test]
fn test_operations_manager_tutorial_categories_tasks_and_telemetry() {
    let (ctx, mut session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    session.role = u64::MAX;

    // 1. Initialize new character tutorial pending and verify eligibility
    ctx.db.with_mut_snapshot(|snap| {
        mark_new_character_tutorial_pending(snap, char_id, 1_700_000_000);
        assert!(can_character_play_tutorial(snap, char_id));
    });

    let tut_state = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "get_tutorial_state",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        tut_state
            .dict_get("is_tutorial_active")
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "has_skipped_tutorial",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "is_main_tutorial_finished",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );

    // 2. Pick a real SDE category, operation, and task
    let op_defs = get_operation_defs(&ctx.sde);
    let sample_cat = *op_defs.sde_categories.iter().next().unwrap();
    let sample_op = *op_defs.sde_operations.iter().next().unwrap();
    let sample_task = *op_defs.task_to_op.keys().next().unwrap();

    // Activate operation in solar system
    let act = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[
            PyRep::Integer(sample_op),
            PyRep::Integer(sys_id as i64),
            PyRep::Integer(sample_cat),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(act, PyRep::Bool(true));

    assert!(is_operation_site_or_system(&ctx, char_id, sample_op));
    assert_eq!(
        resolve_operation_spawnpoint_solar_system(&ctx, char_id, Some(sample_op)),
        Some(sys_id)
    );

    // get_active_category_id, get_category_state, is_category_complete
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "get_active_category_id",
            &[],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(sample_cat)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "get_category_state",
            &[PyRep::Integer(sample_cat)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(2) // ACTIVE
    );

    // block_task, unblock_task, skip_task, complete_category_for_character
    for method in ["block_task", "unblock_task", "skip_task"] {
        let r = handle(
            &ctx,
            &mut session,
            "operationsManager",
            method,
            &[PyRep::Integer(sample_task)],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r, PyRep::Bool(true));
    }

    let comp_cat = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "complete_category_for_character",
        &[PyRep::Integer(sample_cat)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(comp_cat, PyRep::Bool(true));
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "is_category_complete",
            &[PyRep::Integer(sample_cat)],
            &kw
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // process_client_event for set_career_path, add_mission_avoidance_system, skip_tutorial
    for (ev, arg) in [
        ("set_career_path", PyRep::Integer(3)),
        ("add_mission_avoidance_system", PyRep::Integer(sys_id as i64)),
        ("skip_tutorial", PyRep::Integer(0)),
    ] {
        let r = handle(
            &ctx,
            &mut session,
            "operationsManager",
            "process_client_event",
            &[PyRep::String(ev.into()), arg],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r, PyRep::Bool(true));
    }

    let avoid = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "get_mission_avoidance_systems",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(avoid, PyRep::List(vec![PyRep::Integer(sys_id as i64)]));

    let prog = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "get_character_progress",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(prog, PyRep::Dict(_)));

    // eventLog RPCs
    for method in [
        "LogClientEvent",
        "LogClientStats",
        "LogPlayerRequestedDisconnect",
    ] {
        let r = handle(&ctx, &mut session, "eventLog", method, &[], &kw)
            .unwrap()
            .unwrap();
        assert_eq!(r, PyRep::None);
    }
}

#[test]
fn test_activate_operation_and_start_site_docked_system_transition_location_variants() {
    use eve_db::UpwellStructureRecord;

    let (ctx, mut session, char_id, _ship_id, _init_sys) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    session.role = u64::MAX;

    let mut stations: Vec<(i64, u32)> = ctx
        .sde
        .stations
        .values()
        .map(|s| (s.station_id, s.solar_system_id))
        .collect();
    stations.sort_unstable();
    let (station_in, op_sys) = stations[0];
    let (station_out, other_sys) = stations
        .iter()
        .copied()
        .find(|(_, sys)| *sys != op_sys)
        .expect("distinct station in another solar system");

    let op_defs = get_operation_defs(&ctx.sde);
    let sample_cat = *op_defs.sde_categories.iter().next().unwrap();
    let sample_op = *op_defs.sde_operations.iter().next().unwrap();

    // Seed an Upwell structure inside `op_sys` and another in `other_sys`.
    let struct_in = 1_050_000_001i64;
    let struct_out = 1_050_000_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (sid, ssys, name) in [
            (struct_in, op_sys, "OpSys Fortizar"),
            (struct_out, other_sys, "OtherSys Fortizar"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35833,
                    owner_corp_id: 1_000_001,
                    solar_system_id: ssys,
                    name: name.into(),
                    position: [0.0, 0.0, 0.0],
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
        }
    });

    session.solar_system_id = Some(op_sys);
    session.station_id = None;
    session.structure_id = None;

    // 1. Docked at station in `op_sys`: preserves station_id == Some(station_in).
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = op_sys;
        ch.station_id = Some(station_in);
        ch.structure_id = None;
    });
    let r1 = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[
            PyRep::Integer(sample_op),
            PyRep::Integer(i64::from(op_sys)),
            PyRep::Integer(sample_cat),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r1, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.solar_system_id, op_sys);
        assert_eq!(ch.station_id, Some(station_in));
        assert_eq!(ch.structure_id, None);
    });

    // 2. Docked at station in `other_sys` != `op_sys`: clears station_id & resolves Space { op_sys }.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = other_sys;
        ch.station_id = Some(station_out);
        ch.structure_id = None;
    });
    let r2 = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[
            PyRep::Integer(sample_op),
            PyRep::Integer(i64::from(op_sys)),
            PyRep::Integer(sample_cat),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r2, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.solar_system_id, op_sys);
        assert_eq!(ch.station_id, None);
        assert_eq!(ch.structure_id, None);
    });

    // 3. Docked at Upwell structure in `op_sys`: preserves structure_id == Some(struct_in).
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = op_sys;
        ch.station_id = None;
        ch.structure_id = Some(struct_in);
    });
    let r3 = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[
            PyRep::Integer(sample_op),
            PyRep::Integer(i64::from(op_sys)),
            PyRep::Integer(sample_cat),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r3, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.solar_system_id, op_sys);
        assert_eq!(ch.station_id, None);
        assert_eq!(ch.structure_id, Some(struct_in));
    });

    // 4. Docked at Upwell structure in `other_sys` != `op_sys`: clears structure_id & resolves Space { op_sys }.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = other_sys;
        ch.station_id = None;
        ch.structure_id = Some(struct_out);
    });
    let r4 = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "activate_operation_in_solar_system",
        &[
            PyRep::Integer(sample_op),
            PyRep::Integer(i64::from(op_sys)),
            PyRep::Integer(sample_cat),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r4, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.solar_system_id, op_sys);
        assert_eq!(ch.station_id, None);
        assert_eq!(ch.structure_id, None);
    });

    // 5a. Already in space in `other_sys`: resolves Space { op_sys }.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = other_sys;
        ch.station_id = None;
        ch.structure_id = None;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "activate_operation_in_solar_system",
            &[
                PyRep::Integer(sample_op),
                PyRep::Integer(i64::from(op_sys)),
                PyRep::Integer(sample_cat),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.solar_system_id, op_sys);
        assert_eq!(ch.station_id, None);
        assert_eq!(ch.structure_id, None);
    });

    // 5b. Dangling/unknown docked IDs (station_id not in SDE, structure_id not in snap.structures)
    //     safely fall back to Space { op_sys } and clear both docked fields.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = other_sys;
        ch.station_id = Some(999_999);
        ch.structure_id = Some(888_888);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "operationsManager",
            "activate_operation_in_solar_system",
            &[
                PyRep::Integer(sample_op),
                PyRep::Integer(i64::from(op_sys)),
                PyRep::Integer(sample_cat),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.solar_system_id, op_sys);
        assert_eq!(ch.station_id, None);
        assert_eq!(ch.structure_id, None);
    });

    // 6. Exercise `start_site` in-space transition branch (with active_op & matching site/dungeon).
    let (site_op, site_arg) = op_defs
        .sde_operations
        .iter()
        .find_map(|&oid| {
            if let Some(&s) = op_defs.op_site_ids.get(&oid).filter(|&&s| s > 0) {
                op_defs.resolve_site_dungeon_id(&ctx.sde, s, 0).map(|_| (oid, s))
            } else {
                ctx.sde.dungeons.keys().next().map(|&d| (oid, i64::from(d)))
            }
        })
        .expect("operation for start_site");
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = op_sys;
        ch.station_id = None;
        ch.structure_id = None;
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.active_operation_id = Some(site_op);
        rec.active_operation_system_id = Some(op_sys);
        save_store(snap, &store, now);
    });
    let r_site = handle(
        &ctx,
        &mut session,
        "operationsManager",
        "start_site",
        &[PyRep::Integer(site_op), PyRep::Integer(site_arg)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r_site, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert_eq!(ch.solar_system_id, op_sys);
        assert_eq!(ch.station_id, None);
        assert_eq!(ch.structure_id, None);
        let store = load_store(snap);
        let rec = store.by_character.get(&char_id).unwrap();
        assert_eq!(rec.active_site_id, Some(site_arg));
        assert!(rec.active_dungeon_id.is_some());
        assert_eq!(rec.active_operation_system_id, Some(op_sys));
    });
}

#[cfg(test)]
#[path = "operations_manager_extra_unit_tests.rs"]
mod extra_unit_tests;
