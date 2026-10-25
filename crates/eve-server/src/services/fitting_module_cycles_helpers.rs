//! Helpers for active module cycle execution (`feat:mining.survey-scanner`, `dogmaIM.Activate`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::system_actor::SystemHandle;
use eve_dogma::{DogmaItemState, ShipFittingEngine};
use eve_proto::PyRep;
use eve_sde::{attr_ids, InvType};
use glam::DVec3;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_survey_scanner_activation(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &InvType,
    mod_state: &DogmaItemState,
) -> PyRep {
    let mut scan_range = mod_state.get_attribute(197, &ctx.sde);
    if scan_range <= 0.0 {
        scan_range = mod_type
            .default_attributes
            .get(&197)
            .copied()
            .or_else(|| mod_type.default_attributes.get(&54).copied())
            .unwrap_or(15_000.0);
    }
    let mut ship_pos = DVec3::ZERO;
    if let Ok(Some((_, ship_ball))) = sys.get_ball_snapshot(ship_id).await {
        ship_pos = ship_ball.position;
    }
    let asteroids = ctx.db.get_asteroids_in_system(sys_id);
    let mut scanned_asteroids = Vec::new();
    for ast in asteroids {
        if ast.quantity <= 0 {
            continue;
        }
        let ast_pos = DVec3::new(ast.position.0, ast.position.1, ast.position.2);
        let dist = if ship_pos != DVec3::ZERO {
            (ship_pos - ast_pos).length()
        } else {
            0.0
        };
        if dist <= scan_range || ship_pos == DVec3::ZERO {
            let type_name = ctx
                .sde
                .types
                .get(&ast.type_id)
                .map(|t| t.name.clone())
                .unwrap_or_else(|| format!("Asteroid {}", ast.type_id));
            let vol = ctx.sde.types.get(&ast.type_id).map(|t| t.volume).unwrap_or(0.1);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnAsteroidRevealed",
                "character",
                vec![
                    PyRep::Integer(ast.asteroid_id),
                    PyRep::Integer(ast.type_id as i64),
                    PyRep::Integer(ast.quantity),
                ],
            );
            scanned_asteroids.push(PyRep::keyval([
                ("asteroidID", PyRep::Integer(ast.asteroid_id)),
                ("typeID", PyRep::Integer(ast.type_id as i64)),
                ("typeName", PyRep::String(type_name)),
                ("quantity", PyRep::Integer(ast.quantity)),
                ("initialQuantity", PyRep::Integer(ast.initial_quantity)),
                ("distance", PyRep::Real(dist)),
                ("volume", PyRep::Real(vol)),
            ]));
        }
    }
    PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("active", PyRep::Bool(true)),
        ("scanRange", PyRep::Real(scan_range)),
        ("count", PyRep::Integer(scanned_asteroids.len() as i64)),
        ("asteroidCount", PyRep::Integer(scanned_asteroids.len() as i64)),
        ("asteroids", PyRep::List(scanned_asteroids)),
    ])
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_specialized_module_result(
    method: &str,
    fit: &ShipFittingEngine,
    ctx: &ServerContext,
    module_id: i64,
    mod_type: &InvType,
    hic_bubble_radius_m: Option<f64>,
    warp_scramble_points: i32,
    active_burst_charge_tid: Option<u32>,
) -> Option<PyRep> {
    if method == "Overload" {
        let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
        Some(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("overloaded", PyRep::Bool(true)),
            ("effectiveMaxVelocity", PyRep::Real(max_vel)),
        ]))
    } else if mod_type.group_id == 899 {
        let weapon_timer_s = mod_type
            .default_attributes
            .get(&2037)
            .copied()
            .unwrap_or(60_000.0)
            / 1000.0;
        Some(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("active", PyRep::Bool(true)),
            ("scripted", PyRep::Bool(hic_bubble_radius_m.is_none())),
            (
                "bubbleRadius",
                hic_bubble_radius_m.map(PyRep::Real).unwrap_or(PyRep::None),
            ),
            ("warpScrambleStrength", PyRep::Integer(warp_scramble_points as i64)),
            ("disallowAssistance", PyRep::Bool(hic_bubble_radius_m.is_some())),
            ("weaponTimerSeconds", PyRep::Real(weapon_timer_s)),
        ]))
    } else if matches!(mod_type.group_id, 515 | 1770 | 4174) {
        Some(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("active", PyRep::Bool(true)),
            (
                "chargeTypeID",
                active_burst_charge_tid
                    .map(|c| PyRep::Integer(c as i64))
                    .unwrap_or(PyRep::None),
            ),
        ]))
    } else {
        None
    }
}
