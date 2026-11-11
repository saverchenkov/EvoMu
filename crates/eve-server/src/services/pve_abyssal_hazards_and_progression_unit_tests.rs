//! Semantic unit tests split from `pve_special_sites_abyssal_deep_extra_unit_tests`.

use super::pve_special_sites_abyssal_deep_extra_unit_tests::*;
use crate::services::abyssal_void_gates::{
    handle_abyssal_end_gate_activation, handle_abyssal_gate_activation,
};
use crate::services::pve_abyssal_hazards::{
    ConsumedAbyssalFilament, check_abyssal_in_pocket_rpc_restrictions,
    check_filament_1000km_proximity, check_not_in_deadspace_pocket,
    is_abyssal_local_channel_disabled, is_abyssal_system, validate_abyssal_hull_and_capacity,
    validate_and_consume_abyssal_filaments,
};
use crate::services::pve_abyssal_lifecycle::{
    collapse_abyssal_pocket_and_pod_pilot_at, evacuate_abyssal_pockets_for_downtime,
    reconcile_abyssal_login, sweep_expired_void_space_sessions_sync,
};
use crate::services::pve_special_sites::{
    with_abyssal_state, AbyssalTraceRecord, VoidSpaceSessionRecord,
};
use crate::services::pve_special_sites_abyssal::{
    enter_abyssal_pocket_for_character, on_abyssal_room_npc_destroyed_core, pocket_grid_offset,
};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_hull_capacity_filament_validation_and_in_pocket_rpc_restrictions() {
    let ctx = new_ctx();
    let origin_sys: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_303_001;
    let (frig_ship_id, dessie_ship_id, bs_ship_id, fil_item_id) =
        (1_303_000_001, 1_303_000_002, 1_303_000_003, 1_303_000_050);

    let frig_tid = ctx.sde.types.values().find(|t| t.group_id == 25).unwrap().type_id;
    let dessie_tid = ctx.sde.types.values().find(|t| t.group_id == 420).unwrap().type_id;
    let bs_tid = ctx.sde.types.values().find(|t| t.group_id == 27).unwrap().type_id;

    ctx.db.provision_gm_account("val_pilot", "pw", char_id, "ValPilot");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
            ch.station_id = None;
            ch.active_ship_id = frig_ship_id;
        }
        for (sid, tid) in [(frig_ship_id, frig_tid), (dessie_ship_id, dessie_tid), (bs_ship_id, bs_tid)] {
            snap.items.insert(sid, make_item(sid, tid, char_id, origin_sys as i64, InventoryFlag::None, 1));
        }
        snap.items.insert(
            fil_item_id,
            make_item(fil_item_id, 56131, char_id, frig_ship_id, InventoryFlag::Cargo, 2),
        );
    });

    assert_eq!(validate_abyssal_hull_and_capacity(&ctx, frig_ship_id).unwrap().2, 3);
    assert_eq!(validate_abyssal_hull_and_capacity(&ctx, dessie_ship_id).unwrap().2, 2);
    assert_eq!(
        validate_abyssal_hull_and_capacity(&ctx, bs_ship_id).unwrap_err().code,
        "DisallowedShipType"
    );
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, frig_ship_id, origin_sys, 0)
            .await
            .unwrap_err()
            .code,
        "MissingRequiredArgument"
    );
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, frig_ship_id, origin_sys, fil_item_id)
            .await
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    let far_pos = DVec3::new(9.0e11, -8.0e11, 7.0e11);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            8801,
            eve_db::types::FleetRecord {
                fleet_id: 8801,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
        snap.item_positions.insert(frig_ship_id, far_pos.to_array());
    });
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, frig_ship_id, origin_sys, fil_item_id)
            .await
            .unwrap_err()
            .code,
        "InsufficientFilamentQuantity"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fil_item_id).unwrap().quantity = 4;
    });
    let ConsumedAbyssalFilament {
        filament_type_id: consumed_tid,
        tier,
        max_ships,
        opener_fleet_id: fleet_opt,
        ..
    } = validate_and_consume_abyssal_filaments(&ctx, char_id, frig_ship_id, origin_sys, fil_item_id)
        .await
        .expect("frigate filament activation succeeds");
    assert_eq!((consumed_tid, tier, max_ships, fleet_opt), (56131, 0, 3, Some(8801)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&fil_item_id].quantity), 1);

    with_abyssal_state(&ctx, |st| {
        st.traces.insert(
            1_897_000_999,
            AbyssalTraceRecord {
                trace_ball_id: 1_897_000_999,
                char_id,
                filament_type_id: 56131,
                origin_solar_system_id: origin_sys,
                position: (far_pos + DVec3::new(100_000.0, 0.0, 0.0)).to_array(),
                created_unix: ctx.now_unix(),
                expires_at_sec: ctx.now_unix() + 900,
                max_ships: 3,
                entered_char_ids: Vec::new(),
                pocket_id: None,
                fleet_id: Some(8801),
            },
        );
    });
    assert_eq!(
        check_filament_1000km_proximity(&ctx, origin_sys, far_pos).await.unwrap_err().code,
        "ProximityRestriction"
    );
    assert_eq!(
        check_not_in_deadspace_pocket(&ctx, 32_000_050, far_pos).await.unwrap_err().code,
        "ShipInDungeon"
    );

    assert!(is_abyssal_system(32_000_001));
    assert!(!is_abyssal_system(30_000_142));
    assert!(is_abyssal_local_channel_disabled("solarsystemid2:32000001", None));
    assert!(is_abyssal_local_channel_disabled("local", Some(32_000_001)));
    assert!(!is_abyssal_local_channel_disabled("local", Some(30_000_142)));

    let mut sess = SessionState::new(1);
    sess.solar_system_id = Some(32_000_001);
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(&sess, "scanSvc", "ConeScan", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDscanInThisSystem"
    );
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(&sess, "LSC", "SendMessage", &[PyRep::String("local".into())])
            .unwrap()
            .unwrap_err()
            .code,
        "LocalChannelDisabledInAbyssalSpace"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_room_1_2_3_conduit_progression_pocket_collapse_and_downtime_evacuation() {
    let ctx = new_ctx();
    let origin_sys: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_304_001;
    let ship_id: i64 = 1_304_000_001;
    let cruiser_tid = ctx.sde.types.values().find(|t| t.group_id == 26).unwrap().type_id;

    ctx.db.provision_gm_account("prog_pilot", "pw", char_id, "ProgPilot");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, cruiser_tid, char_id, origin_sys as i64, InventoryFlag::None, 1),
        );
    });

    let mut session = SessionState::new(77);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(origin_sys);
    session.ship_id = Some(ship_id);

    let (pocket_id, r1_npcs) =
        enter_abyssal_pocket_for_character(&ctx, &mut session, char_id, 56131, 0, origin_sys).await;
    let ab_sys = session.solar_system_id.unwrap();
    let ab_handle = ctx.systems.get_or_spawn(ab_sys);
    let base = DVec3::from_array(pocket_grid_offset(&ctx, pocket_id));
    let _ = ab_handle
        .spawn_ball(make_ball(ship_id, cruiser_tid, base, 120.0))
        .await;
    let kwargs = HashMap::new();

    let r1_gate = 1_895_000_000 + pocket_id * 10 + 1;
    assert_eq!(
        handle_abyssal_gate_activation(&ctx, &mut session, char_id, &[PyRep::Integer(r1_gate)], &kwargs)
            .await
            .unwrap_err()
            .code,
        "GateLocked"
    );

    for r_idx in 1..=3_i64 {
        let npcs = with_abyssal_state(&ctx, |st| {
            st.pockets[&char_id].rooms[(r_idx - 1) as usize].npc_ball_ids.clone()
        });
        for &nid in &npcs {
            let _ = ab_handle.remove_ball(nid).await;
            on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, ab_sys, nid).await;
        }
        let gate_id = 1_895_000_000 + pocket_id * 10 + r_idx;
        let gate_pos = base + DVec3::new(0.0, 0.0, ((r_idx - 1) as f64) * 50_000.0);
        let _ = ab_handle.set_ball_position(ship_id, gate_pos).await;
        if r_idx < 3 {
            let res = handle_abyssal_gate_activation(
                &ctx,
                &mut session,
                char_id,
                &[PyRep::Integer(gate_id)],
                &kwargs,
            )
            .await
            .expect("transfer gate activation succeeds");
            assert_eq!(res.dict_get("room").and_then(|v| v.as_i64()), Some(r_idx + 1));
        } else {
            let res = handle_abyssal_end_gate_activation(
                &ctx,
                &mut session,
                char_id,
                &[PyRep::Integer(gate_id)],
            )
            .await
            .expect("exit conduit activation succeeds");
            assert_eq!(res.dict_get("exitedAbyssalSpace").and_then(|v| v.as_bool()), Some(true));
        }
    }
    assert_eq!(session.solar_system_id, Some(origin_sys));
    assert_eq!(r1_npcs.len(), 2);

    let (pid2, _) =
        enter_abyssal_pocket_for_character(&ctx, &mut session, char_id, 47896, 4, origin_sys).await;
    assert!(pid2 > pocket_id);
    let ab_sys2 = session.solar_system_id.unwrap();
    assert_eq!(reconcile_abyssal_login(&ctx, char_id, ab_sys2, ship_id), ab_sys2);
    assert_eq!(evacuate_abyssal_pockets_for_downtime(&ctx), 1);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].solar_system_id),
        origin_sys
    );

    with_abyssal_state(&ctx, |st| {
        st.void_space_sessions.insert(
            char_id,
            VoidSpaceSessionRecord {
                char_id,
                ship_id,
                origin_solar_system_id: origin_sys,
                origin_position: [1000.0, 2000.0, 3000.0],
                entered_unix: 100,
                expires_at_sec: 200,
                target_id: 77,
                nebula_graphic_id: None,
            },
        );
    });
    sweep_expired_void_space_sessions_sync(&ctx, 250);
    assert!(with_abyssal_state(&ctx, |st| st.void_space_sessions.is_empty()));

    let home_sys = collapse_abyssal_pocket_and_pod_pilot_at(&ctx, char_id, ship_id, ab_sys2, ctx.now_unix());
    assert!(home_sys > 0);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ship_id)));
}
