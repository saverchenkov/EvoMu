//! `notify_emitters_batch5` — Server-side V24 notification emitters for Batch 5
//! (Station guest lists, scanner results, damage updates, mining, and crimewatch).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_proto::PyRep;

/// Emits `OnCharNowInStation` or `OnCharNoLongerInStation` (`id_type = "charid"`)
/// to all online pilots currently docked in `station_id`.
pub fn emit_char_station_notification(
    ctx: &ServerContext,
    char_id: i64,
    station_id: i64,
    is_docked: bool,
) {
    let (corp_id, alliance_id, war_faction_id) = ctx.db.with_snapshot(|snap| {
        let c = snap.characters.get(&char_id);
        (
            c.map(|c| c.corporation_id).unwrap_or(0),
            c.and_then(|c| c.alliance_id),
            c.and_then(|c| c.war_faction_id),
        )
    });
    let guest_row = PyRep::List(vec![
        PyRep::Integer(char_id),
        PyRep::Integer(corp_id),
        alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        war_faction_id.map(PyRep::Integer).unwrap_or(PyRep::None),
    ]);

    let event = if is_docked {
        "OnCharNowInStation"
    } else {
        "OnCharNoLongerInStation"
    };

    ctx.notify(
        NotificationTarget::Station(station_id),
        event,
        "charid",
        vec![guest_row],
    );
}

/// Emits `OnCargoScanComplete(targetID, cargoList)` (`id_type = "charid"`).
/// `cargoList` consists of tuples `(typeID, quantity)`.
pub fn emit_cargo_scan_complete(
    ctx: &ServerContext,
    char_id: i64,
    target_id: i64,
    items: Vec<(i32, i64)>,
) {
    let cargo_list = items
        .into_iter()
        .map(|(tid, qty)| {
            PyRep::Tuple(vec![
                PyRep::Integer(i64::from(tid)),
                PyRep::Integer(qty),
            ])
        })
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCargoScanComplete",
        "charid",
        vec![PyRep::Integer(target_id), PyRep::List(cargo_list)],
    );
}

/// Emits `OnShipScanCompleted(targetID, capCharge, capCapacity, hardwareList)` (`id_type = "charid"`).
/// `hardwareList` consists of tuples `(typeID, quantity)`.
pub fn emit_ship_scan_completed(
    ctx: &ServerContext,
    char_id: i64,
    target_id: i64,
    cap_charge: f64,
    max_cap: f64,
    hardware: Vec<(i32, i64)>,
) {
    let hw_list = hardware
        .into_iter()
        .map(|(tid, qty)| {
            PyRep::Tuple(vec![
                PyRep::Integer(i64::from(tid)),
                PyRep::Integer(qty),
            ])
        })
        .collect();
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnShipScanCompleted",
        "charid",
        vec![
            PyRep::Integer(target_id),
            PyRep::Real(cap_charge),
            PyRep::Real(max_cap),
            PyRep::List(hw_list),
        ],
    );
}

/// Emits `OnOreMined(oreType, quantity_added, quantity_removed, hasRewards, amountWasted, amountCritBonus, itemID, moduleItemID)`.
#[allow(clippy::too_many_arguments)]
pub fn emit_ore_mined(
    ctx: &ServerContext,
    char_id: i64,
    ore_type_id: i32,
    delivered: i64,
    mined: i64,
    wasted: i64,
    crit_bonus: i64,
    target_id: i64,
    module_id: i64,
) {
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnOreMined",
        "charid",
        vec![
            PyRep::Integer(i64::from(ore_type_id)),
            PyRep::Integer(delivered),
            PyRep::Integer(mined),
            PyRep::Bool(false),
            PyRep::Integer(wasted),
            PyRep::Integer(crit_bonus),
            PyRep::Integer(target_id),
            PyRep::Integer(module_id),
        ],
    );
}

/// Emits `OnDamageStateChange(itemID, ((shield, tau, ft), armor, struct))` (`id_type = "charid"`).
pub fn emit_damage_state_change(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    shield: f64,
    armor: f64,
    structure: f64,
) {
    let damage_tuple = super::notify_emitters_batch3_space::build_damage_state_tuple(
        ctx, ship_id, shield, armor, structure,
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDamageStateChange",
        "charid",
        vec![PyRep::Integer(ship_id), damage_tuple],
    );
}
