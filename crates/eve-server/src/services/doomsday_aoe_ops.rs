//! `doomsday_aoe_ops` — Bosonic Field Generator (Cone), Spatial Phenomena Generator (GTFO),
//! and Reaper (MultiPoint) Titan Doomsday handlers (`feat:modules.doomsday-bosonic-field`,
//! `feat:modules.doomsday-gtfo-reaper`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::tactical_modules::is_combatant_ship_or_entity_ball;
use eve_machonet::SessionState;
use eve_destiny::{DamageVector, DVec3};
use eve_proto::{BridgeRpcError, PyRep};

pub async fn dispatch_superweapon_aoe_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Activating a superweapon requires an active Titan".into(),
        });
    };
    let Some(sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Superweapons can only be activated in space".into(),
        });
    };
    let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    match method {
        "ActivateConeModule" => activate_cone_doomsday(ctx, char_id, ship_id, sys_id, module_id, args).await,
        "ActivateSpatialModule" => activate_spatial_gtfo_doomsday(ctx, char_id, ship_id, sys_id, module_id, args).await,
        _ => activate_multipoint_reaper_doomsday(ctx, char_id, ship_id, sys_id, module_id, args).await,
    }
}

const DOOMSDAY_ISOTOPE_COST: i32 = 50_000;
const BOSONIC_CONE_LENGTH_M: f64 = 150_000.0;
const BOSONIC_CONE_COS_HALF_ANGLE: f64 = 0.9659258262890683; // cos(15 deg)
const BOSONIC_TICK_COUNT: usize = 5;
const GTFO_SPHERE_RADIUS_M: f64 = 25_000.0;
const REAPER_BEAM_RADIUS_M: f64 = 5_000.0;

struct TitanDoomsdaySetup {
    sys: crate::system_actor::SystemHandle,
    all_balls: Vec<eve_destiny::Ball>,
    ship_pos: DVec3,
    ship_type_id: u32,
    mod_type_id: u32,
    dmg_vec: DamageVector,
    now_unix: u64,
}

async fn validate_and_consume_titan_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
) -> Result<TitanDoomsdaySetup, BridgeRpcError> {
    let now_unix = ctx.now_unix_secs();
    let (ship_type_id, ship_group_id, mod_type_id) = ctx.db.with_snapshot(|snap| {
        let ship = snap.items.get(&ship_id).ok_or_else(|| BridgeRpcError {
            code: "ShipNotFound".into(),
            message: format!("Active ship {ship_id} not found"),
        })?;
        let s_group = ctx
            .sde
            .types
            .get(&ship.type_id)
            .map(|t| t.group_id)
            .unwrap_or(0);
        let module = snap.items.get(&module_id).ok_or_else(|| BridgeRpcError {
            code: "ModuleNotFound".into(),
            message: format!("Doomsday module {module_id} not found"),
        })?;
        if module.location_id != ship_id {
            return Err(BridgeRpcError {
                code: "ModuleNotFitted".into(),
                message: format!("Module {module_id} is not fitted on ship {ship_id}"),
            });
        }
        Ok((ship.type_id, s_group, module.type_id))
    })?;

    if ship_group_id != 30 {
        return Err(BridgeRpcError {
            code: "ShipCannotActivateSuperweapon".into(),
            message: "Only Titan-class hulls (group 30) can activate Doomsday superweapons".into(),
        });
    }

    let mod_type = ctx.sde.types.get(&mod_type_id);
    let em_dmg = mod_type.and_then(|t| t.default_attributes.get(&114).copied()).unwrap_or(0.0);
    let ex_dmg = mod_type.and_then(|t| t.default_attributes.get(&116).copied()).unwrap_or(0.0);
    let ki_dmg = mod_type.and_then(|t| t.default_attributes.get(&117).copied()).unwrap_or(0.0);
    let th_dmg = mod_type.and_then(|t| t.default_attributes.get(&118).copied()).unwrap_or(0.0);
    let dmg_vec = if em_dmg + ex_dmg + ki_dmg + th_dmg > 0.0 {
        DamageVector::new(em_dmg, th_dmg, ki_dmg, ex_dmg)
    } else {
        DamageVector::new(300_000.0, 300_000.0, 300_000.0, 300_000.0)
    };

    ctx.db
        .with_mut_snapshot(|snap| {
            let cand = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && matches!(i.type_id, 16274 | 17887..=17889)
                        && i.quantity >= DOOMSDAY_ISOTOPE_COST
                })
                .or_else(|| {
                    snap.items.values().find(|i| {
                        i.location_id == ship_id
                            && matches!(i.type_id, 16274 | 17887..=17889)
                            && i.quantity > 0
                    })
                })
                .map(|i| (i.item_id, i.quantity));
            match cand {
                Some((fid, avail)) if avail >= DOOMSDAY_ISOTOPE_COST => {
                    let mut rem = 0;
                    if let Some(it) = snap.items.get_mut(&fid) {
                        it.quantity -= DOOMSDAY_ISOTOPE_COST;
                        rem = it.quantity;
                    }
                    if rem <= 0 {
                        snap.items.remove(&fid);
                    }
                    Ok(())
                }
                Some((_, avail)) => Err(format!(
                    "Insufficient superweapon isotopes: need {DOOMSDAY_ISOTOPE_COST}, have {avail}"
                )),
                None => Err(format!(
                    "Missing required superweapon isotopes (need {DOOMSDAY_ISOTOPE_COST})"
                )),
            }
        })
        .map_err(|message| BridgeRpcError {
            code: "InsufficientFuel".into(),
            message,
        })?;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let all_balls = sys.list_balls().await.unwrap_or_default();
    let ship_pos = all_balls
        .iter()
        .find(|b| b.id == ship_id)
        .map(|b| b.position)
        .unwrap_or(DVec3::ZERO);

    // Immobilize & tether-block the Titan during Doomsday firing cycle
    let mut cs = sys
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| ctx.build_type_combat_state(ship_type_id));
    cs.warp_scramble_strength = cs.warp_scramble_strength.max(100);
    cs.disallow_assistance = true;
    cs.is_tethered = false;
    if let Ok(Some((_, mut titan_ball))) = sys.get_ball_snapshot(ship_id).await {
        titan_ball.max_velocity = 0.0;
        let _ = sys.spawn_ball_with_combat(titan_ball, cs).await;
    } else {
        let _ = sys.set_combat_state(ship_id, cs).await;
    }
    {
        let db_key = ctx.instance_id as usize;
        let mut st = ctx
            .tactical_state
            .write()
            .unwrap_or_else(|e| e.into_inner());
        st.lance_caster_lock_until
            .insert((db_key, ship_id), now_unix + 300);
    }

    ctx.crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_create(char_id)
        .weapon_timer_until_unix = now_unix + 60;
    ctx.record_module_reactivation_delay(ship_id, module_id, 300.0);

    Ok(TitanDoomsdaySetup {
        sys,
        all_balls,
        ship_pos,
        ship_type_id,
        mod_type_id,
        dmg_vec,
        now_unix,
    })
}

fn parse_vec3_from_args(args: &[PyRep], start_idx: usize) -> Option<DVec3> {
    if let Some(PyRep::Tuple(vals) | PyRep::List(vals)) = args.get(start_idx) {
        if vals.len() >= 3 {
            if let (Some(x), Some(y), Some(z)) = (
                vals[0].as_f64(),
                vals[1].as_f64(),
                vals[2].as_f64(),
            ) {
                return Some(DVec3::new(x, y, z));
            }
        }
    }
    if let (Some(x), Some(y), Some(z)) = (
        args.get(start_idx).and_then(|a| a.as_f64()),
        args.get(start_idx + 1).and_then(|a| a.as_f64()),
        args.get(start_idx + 2).and_then(|a| a.as_f64()),
    ) {
        return Some(DVec3::new(x, y, z));
    }
    None
}

/// `superWeaponMgr.ActivateConeModule` — Bosonic Field Generator cone AoE doomsday.
pub async fn activate_cone_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let setup = validate_and_consume_titan_doomsday(ctx, char_id, ship_id, sys_id, module_id).await?;
    let dir_vec = if let Some(v) = parse_vec3_from_args(args, 1) {
        if v.length_squared() > 1e-6 {
            v.normalize()
        } else {
            DVec3::new(1.0, 0.0, 0.0)
        }
    } else if let Some(tid) = args.get(1).and_then(|a| a.as_i64()) {
        setup
            .all_balls
            .iter()
            .find(|b| b.id == tid)
            .map(|b| (b.position - setup.ship_pos).normalize_or_zero())
            .filter(|v| v.length_squared() > 1e-6)
            .unwrap_or(DVec3::new(1.0, 0.0, 0.0))
    } else {
        DVec3::new(1.0, 0.0, 0.0)
    };

    let tick_dmg = DamageVector::new(
        setup.dmg_vec.em / (BOSONIC_TICK_COUNT as f64),
        setup.dmg_vec.thermal / (BOSONIC_TICK_COUNT as f64),
        setup.dmg_vec.kinetic / (BOSONIC_TICK_COUNT as f64),
        setup.dmg_vec.explosive / (BOSONIC_TICK_COUNT as f64),
    );
    let mut hit_targets = Vec::new();
    let mut total_applied = 0.0_f64;

    for b in &setup.all_balls {
        if b.id == ship_id
            || !is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id)
        {
            continue;
        }
        let delta = b.position - setup.ship_pos;
        let dist = delta.length();
        if dist <= 0.0 || dist > BOSONIC_CONE_LENGTH_M {
            continue;
        }
        let cos_angle = delta.normalize().dot(dir_vec);
        if cos_angle < BOSONIC_CONE_COS_HALF_ANGLE {
            continue;
        }
        let mut target_destroyed = false;
        for _tick in 0..BOSONIC_TICK_COUNT {
            if target_destroyed {
                break;
            }
            let (app, destroyed) =
                crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                    ctx,
                    &setup.sys,
                    sys_id,
                    char_id,
                    b.id,
                    tick_dmg,
                    10.0,
                    setup.now_unix,
                )
                .await
                .ok()
                .flatten()
                .map(|(a, d, _)| (a, d))
                .unwrap_or((0.0, false));
            total_applied += app;
            crate::services::combat_crimewatch::record_killmail_assist_action(
                ctx,
                b.id,
                char_id,
                setup.ship_type_id,
                setup.mod_type_id,
                app,
                false,
                setup.now_unix,
            );
            if destroyed {
                target_destroyed = true;
                crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                    ctx,
                    sys_id,
                    char_id,
                    b.id,
                    b.type_id,
                    b.position,
                    setup.now_unix,
                )
                .await;
            }
        }
        hit_targets.push(b.id);
    }

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![
            PyRep::Integer(module_id),
            PyRep::String("superWeaponCone".into()),
            PyRep::Bool(true),
        ],
    );

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("mode", PyRep::String("Cone".into())),
        ("warmupSeconds", PyRep::Real(10.0)),
        ("tickCount", PyRep::Integer(BOSONIC_TICK_COUNT as i64)),
        ("immobilizeSeconds", PyRep::Real(30.0)),
        ("tetherBlocked", PyRep::Bool(true)),
        ("jumpBlockSeconds", PyRep::Real(300.0)),
        ("fuelConsumed", PyRep::Integer(i64::from(DOOMSDAY_ISOTOPE_COST))),
        ("totalDamageApplied", PyRep::Real(total_applied)),
        (
            "hitTargetIDs",
            PyRep::List(hit_targets.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// `superWeaponMgr.ActivateSpatialModule` — Spatial Phenomena Generator (GTFO) sphere teleport doomsday.
pub async fn activate_spatial_gtfo_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let setup = validate_and_consume_titan_doomsday(ctx, char_id, ship_id, sys_id, module_id).await?;
    let center = if let Some(v) = parse_vec3_from_args(args, 1) {
        v
    } else if let Some(tid) = args.get(1).and_then(|a| a.as_i64()) {
        setup
            .all_balls
            .iter()
            .find(|b| b.id == tid)
            .map(|b| b.position)
            .unwrap_or(setup.ship_pos + DVec3::new(30_000.0, 0.0, 0.0))
    } else {
        setup.ship_pos + DVec3::new(30_000.0, 0.0, 0.0)
    };

    let mut teleported_ids = Vec::new();
    for (idx, b) in setup.all_balls.iter().enumerate() {
        if b.id == ship_id
            || !is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id)
        {
            continue;
        }
        if (b.position - center).length() <= GTFO_SPHERE_RADIUS_M {
            let mut updated_ball = b.clone();
            let offset_m = 750_000.0 + ((idx as f64) * 125_000.0);
            updated_ball.position = DVec3::new(
                center.x + offset_m,
                center.y + 250_000.0,
                center.z - offset_m * 0.5,
            );
            updated_ball.velocity = DVec3::ZERO;
            let _ = setup.sys.spawn_ball(updated_ball).await;
            teleported_ids.push(b.id);
        }
    }

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![
            PyRep::Integer(module_id),
            PyRep::String("superWeaponSpatial".into()),
            PyRep::Bool(true),
        ],
    );

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("mode", PyRep::String("SpatialGTFO".into())),
        ("sphereRadiusMeters", PyRep::Real(GTFO_SPHERE_RADIUS_M)),
        ("immobilizeSeconds", PyRep::Real(30.0)),
        ("tetherBlocked", PyRep::Bool(true)),
        ("fuelConsumed", PyRep::Integer(i64::from(DOOMSDAY_ISOTOPE_COST))),
        (
            "teleportedShipIDs",
            PyRep::List(teleported_ids.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// `superWeaponMgr.ActivateMultiPointModule` — Reaper sweeping beam path doomsday.
pub async fn activate_multipoint_reaper_doomsday(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let setup = validate_and_consume_titan_doomsday(ctx, char_id, ship_id, sys_id, module_id).await?;
    let (start_pt, end_pt) = if let (Some(s), Some(e)) = (
        parse_vec3_from_args(args, 1),
        parse_vec3_from_args(args, 4),
    ) {
        (s, e)
    } else if let (Some(PyRep::Tuple(s) | PyRep::List(s)), Some(PyRep::Tuple(e) | PyRep::List(e))) =
        (args.get(1), args.get(2))
    {
        let sv = DVec3::new(
            s.first().and_then(|v| v.as_f64()).unwrap_or(0.0),
            s.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0),
            s.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0),
        );
        let ev = DVec3::new(
            e.first().and_then(|v| v.as_f64()).unwrap_or(50_000.0),
            e.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0),
            e.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0),
        );
        (sv, ev)
    } else {
        (
            setup.ship_pos + DVec3::new(20_000.0, 0.0, 0.0),
            setup.ship_pos + DVec3::new(20_000.0, 0.0, 60_000.0),
        )
    };

    let seg = end_pt - start_pt;
    let seg_len_sq = seg.length_squared().max(1.0);
    let mut hit_targets = Vec::new();
    let mut total_applied = 0.0_f64;

    for b in &setup.all_balls {
        if b.id == ship_id
            || !is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id)
        {
            continue;
        }
        let t = ((b.position - start_pt).dot(seg) / seg_len_sq).clamp(0.0, 1.0);
        let closest = start_pt + seg * t;
        if (b.position - closest).length() <= REAPER_BEAM_RADIUS_M {
            let (app, destroyed) =
                crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                    ctx,
                    &setup.sys,
                    sys_id,
                    char_id,
                    b.id,
                    setup.dmg_vec,
                    10.0,
                    setup.now_unix,
                )
                .await
                .ok()
                .flatten()
                .map(|(a, d, _)| (a, d))
                .unwrap_or((0.0, false));
            total_applied += app;
            crate::services::combat_crimewatch::record_killmail_assist_action(
                ctx,
                b.id,
                char_id,
                setup.ship_type_id,
                setup.mod_type_id,
                app,
                false,
                setup.now_unix,
            );
            if destroyed {
                crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                    ctx,
                    sys_id,
                    char_id,
                    b.id,
                    b.type_id,
                    b.position,
                    setup.now_unix,
                )
                .await;
            }
            hit_targets.push(b.id);
        }
    }

    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnGodmaShipEffect",
        "charid",
        vec![
            PyRep::Integer(module_id),
            PyRep::String("superWeaponMultiPoint".into()),
            PyRep::Bool(true),
        ],
    );

    Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("mode", PyRep::String("MultiPointReaper".into())),
        ("beamRadiusMeters", PyRep::Real(REAPER_BEAM_RADIUS_M)),
        ("immobilizeSeconds", PyRep::Real(30.0)),
        ("tetherBlocked", PyRep::Bool(true)),
        ("fuelConsumed", PyRep::Integer(i64::from(DOOMSDAY_ISOTOPE_COST))),
        ("totalDamageApplied", PyRep::Real(total_applied)),
        (
            "hitTargetIDs",
            PyRep::List(hit_targets.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}
