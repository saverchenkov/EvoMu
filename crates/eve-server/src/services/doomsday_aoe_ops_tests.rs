use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_sde::SolarSystem;
use std::sync::Arc;

fn setup_titan_doomsday_ctx() -> (ServerContext, SessionState, i64, i64, u32, i64) {
    let (mut ctx, mut sess, char_id, ship_id, _highsec_id) = make_fighter_ctx_and_session();
    let null_sys_id = 30_004_759_u32;
    let titan_type_id = 11_567_u32; // Avatar (group 30)
    let dd_mod_type_id = 24_550_u32;
    let mod_id = 550_000_001_i64;
    let mod_id_2 = 550_000_003_i64;
    let mod_id_3 = 550_000_004_i64;
    let fuel_id = 550_000_002_i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.solar_systems.insert(
        null_sys_id,
        SolarSystem {
            solar_system_id: null_sys_id,
            region_id: 10_000_060,
            constellation_id: 20_000_700,
            name: "1DQ1-A".into(),
            security_status: -0.9,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );
    let mut titan_t = make_fighter_type(titan_type_id, 30, 6, 1_000_000.0, vec![]);
    titan_t.name = "Avatar".into();
    sde_mut.types.insert(titan_type_id, titan_t);

    let mut dd_t = make_fighter_type(dd_mod_type_id, 588, 7, 100.0, vec![]);
    dd_t.default_attributes.insert(1074, 1.0); // disallowInEmpireSpace
    dd_t.default_attributes.insert(54, 150_000.0); // maxRange
    dd_t.default_attributes.insert(713, 16274.0); // Helium Isotopes
    dd_t.default_attributes.insert(714, 1000.0); // fuel qty
    dd_t.default_attributes.insert(114, 25_000.0); // em dmg
    sde_mut.types.insert(dd_mod_type_id, dd_t);

    sess.solar_system_id = Some(null_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = titan_type_id;
            sh.location_id = null_sys_id as i64;
        }
        for (mid, flag) in [
            (mod_id, InventoryFlag::HiSlot0),
            (mod_id_2, InventoryFlag::HiSlot1),
            (mod_id_3, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(
                mid,
                make_fighter_item(mid, dd_mod_type_id, char_id, ship_id, flag, 1),
            );
            snap.module_online_states.insert(mid, true);
        }
        snap.items.insert(
            fuel_id,
            make_fighter_item(
                fuel_id,
                16274,
                char_id,
                ship_id,
                InventoryFlag::Cargo,
                5_000,
            ),
        );
    });

    (ctx, sess, char_id, ship_id, null_sys_id, mod_id)
}

#[test]
fn test_validate_doomsday_target_points_and_parse_vec3() {
    let origin = DVec3::new(0.0, 0.0, 0.0);
    assert!(validate_doomsday_target_points(origin, 100_000.0, &[DVec3::new(50_000.0, 0.0, 0.0)]).is_ok());
    assert!(validate_doomsday_target_points(origin, 100_000.0, &[DVec3::new(150_000.0, 0.0, 0.0)]).is_err());
    assert!(validate_doomsday_target_points(origin, 100_000.0, &[DVec3::new(f64::NAN, 0.0, 0.0)]).is_err());

    let args_tuple = vec![
        PyRep::Integer(1),
        PyRep::Tuple(vec![PyRep::Real(10.0), PyRep::Real(20.0), PyRep::Real(30.0)]),
    ];
    assert_eq!(
        parse_vec3_arg_res(&args_tuple, 1),
        Ok(Some(DVec3::new(10.0, 20.0, 30.0)))
    );
    let args_flat = vec![
        PyRep::Integer(1),
        PyRep::Real(1.0),
        PyRep::Real(2.0),
        PyRep::Real(3.0),
    ];
    assert_eq!(
        parse_vec3_arg_res(&args_flat, 1),
        Ok(Some(DVec3::new(1.0, 2.0, 3.0)))
    );
}

#[tokio::test]
async fn test_dispatch_superweapon_aoe_rpc_cone_spatial_and_multipoint() {
    let (ctx, sess, _char_id, ship_id, sys_id, mod_id) = setup_titan_doomsday_ctx();

    // Docked guard
    let mut docked_sess = sess.clone();
    docked_sess.station_id = Some(60_003_760);
    assert!(
        dispatch_superweapon_aoe_rpc(&ctx, &docked_sess, DoomsdayAoeMode::Cone, &[PyRep::Integer(mod_id)])
            .await
            .is_err()
    );

    // Spawn Titan and a subcapital target ball in space
    let sys = ctx.systems.get_or_spawn(sys_id);
    let target_ball_id = 770_000_001_i64;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 11_567,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 80.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 5_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: target_ball_id,
                type_id: 600,
                position: DVec3::new(20_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::new(),
        )
        .await;

    // 1. Cone Doomsday
    let cone_res = dispatch_superweapon_aoe_rpc(
        &ctx,
        &sess,
        DoomsdayAoeMode::Cone,
        &[
            PyRep::Integer(mod_id),
            PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .expect("Cone doomsday should activate in null-sec");
    assert_eq!(cone_res.dict_get("mode").and_then(|v| v.as_str()), Some("Cone"));
    assert_eq!(
        cone_res.dict_get("fuelConsumed").and_then(|v| v.as_i64()),
        Some(1000)
    );

    // Activate Spatial GTFO on second module
    let gtfo_res = dispatch_superweapon_aoe_rpc(
        &ctx,
        &sess,
        DoomsdayAoeMode::Spatial,
        &[PyRep::Integer(550_000_003), PyRep::Integer(target_ball_id)],
    )
    .await
    .expect("Spatial GTFO should activate in null-sec");
    assert_eq!(
        gtfo_res.dict_get("mode").and_then(|v| v.as_str()),
        Some("SpatialGTFO")
    );

    // Activate MultiPoint Reaper on third module
    let reaper_res = dispatch_superweapon_aoe_rpc(
        &ctx,
        &sess,
        DoomsdayAoeMode::MultiPoint,
        &[
            PyRep::Integer(550_000_004),
            PyRep::Tuple(vec![PyRep::Real(20_000.0), PyRep::Real(0.0), PyRep::Real(-10_000.0)]),
            PyRep::Tuple(vec![PyRep::Real(20_000.0), PyRep::Real(0.0), PyRep::Real(10_000.0)]),
        ],
    )
    .await
    .expect("MultiPoint Reaper should activate in null-sec");
    assert_eq!(
        reaper_res.dict_get("mode").and_then(|v| v.as_str()),
        Some("MultiPointReaper")
    );

    // Direct check for `cancel_aoe_superweapon_cycles_for_ship`
    assert!(!ctx
        .db
        .with_snapshot(|s| s.superweapon_runtime.active_cycles.is_empty()));
    cancel_aoe_superweapon_cycles_for_ship(&ctx, ship_id);
    assert!(ctx
        .db
        .with_snapshot(|s| s.superweapon_runtime.active_cycles.is_empty()));
}
