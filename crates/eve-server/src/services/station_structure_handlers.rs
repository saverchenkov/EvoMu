//! `station_structure_handlers` — Helper routines for orbital and station structure operations.

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub fn handle_upgrade_gantry(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let orbital_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let upgrade_item_arg = args.get(1).and_then(|a| a.as_i64());

    let (tid, pid) = ctx.db.try_with_mut_snapshot(|snap| {
        let char_corp = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(98_000_001);
        let (cur_tid, planet_id) = {
            let it = snap.items.get(&orbital_id).ok_or_else(|| BridgeRpcError {
                code: "OrbitalNotFound".into(),
                message: format!("Orbital {orbital_id} not found"),
            })?;
            (it.type_id, it.location_id)
        };
        let target_tid = ctx
            .sde
            .types
            .get(&cur_tid)
            .and_then(|t| t.default_attributes.get(&1771).copied())
            .map(|v| v as u32)
            .unwrap_or(2233);
        let up_item_opt = upgrade_item_arg
            .filter(|iid| snap.items.get(iid).is_some_and(|u| u.type_id == target_tid && u.quantity >= 1))
            .or_else(|| {
                snap.items
                    .values()
                    .find(|u| u.owner_id == char_id && u.type_id == target_tid && u.quantity >= 1)
                    .map(|u| u.item_id)
            });
        let Some(up_iid) = up_item_opt else {
            return Err(BridgeRpcError {
                code: "CustomsOfficeItemRequired".into(),
                message: format!("Upgrading gantry requires item type {target_tid}"),
            });
        };
        if let Some(u) = snap.items.get_mut(&up_iid) {
            if u.quantity > 1 {
                u.quantity -= 1;
            } else {
                snap.items.remove(&up_iid);
            }
        }
        if let Some(it) = snap.items.get_mut(&orbital_id) {
            it.type_id = target_tid;
        }
        if let Some(st) = snap.structures.get_mut(&orbital_id) {
            st.type_id = target_tid;
            st.state = "ShieldVulnerable".into();
            st.shield_hp = 10_000_000.0;
            st.armor_hp = 2_500_000.0;
            st.hull_hp = 2_000_000.0;
        }
        let cfg = crate::services::industry_pi::OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: target_tid,
            owner_corp_id: char_corp,
            ..Default::default()
        };
        crate::services::industry_pi::save_orbital_settings(snap, planet_id, &cfg);
        Ok((target_tid, planet_id))
    })?;

    Ok(PyRep::keyval([
        ("orbitalID", PyRep::Integer(orbital_id)),
        ("planetID", PyRep::Integer(pid)),
        ("typeID", PyRep::Integer(i64::from(tid))),
        ("state", PyRep::String("ShieldVulnerable".into())),
    ]))
}
