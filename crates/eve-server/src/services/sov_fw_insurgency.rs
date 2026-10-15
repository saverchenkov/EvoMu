//! `sov_fw_insurgency` — Domain 11: Sovereignty, Equinox Skyhooks/Mercenary Dens, Faction Warfare, & Pirate Insurgencies (`§3.8`, `§3.11`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("facWarMgr", "EnlistMe")
        | ("fwCharacterEnlistmentMgr", "RequestEnlistment")
        | ("fwEnlistmentMgr", "Enlist") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let faction_id = args.first().and_then(|a| a.as_i64()).unwrap_or(500_001);
            Some(Ok(PyRep::keyval([
                ("factionID", PyRep::Integer(faction_id)),
                ("enlisted", PyRep::Bool(true)),
            ])))
        }

        ("facWarMgr", "CaptureComplex") | ("insurgencyMgr", "AdvanceCorruption") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = session.solar_system_id.unwrap_or(30_002_813);
            let lp_payout = 15_000i64;
            let (contested, corruption) = ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    *ch.loyalty_points.entry(500_001).or_insert(0) += lp_payout;
                }
                if let Some(st) = snap.system_states.get_mut(&sys_id) {
                    st.fw_contested_pct = (st.fw_contested_pct + 25.0).min(100.0);
                    st.insurgency_corruption_stage = (st.insurgency_corruption_stage + 1).min(5);
                    (st.fw_contested_pct, st.insurgency_corruption_stage)
                } else {
                    (70.0, 5)
                }
            });

            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("contestedPct", PyRep::Real(contested)),
                ("corruptionStage", PyRep::Integer(corruption as i64)),
                ("lpAwarded", PyRep::Integer(lp_payout)),
            ])))
        }

        ("sovMgr", "ClaimSovereignty")
        | ("devIndexMgr", "UpgradeDevelopmentIndex")
        | ("orbitalSkyhookMgr", "AnchorSkyhook")
        | ("mercenaryDenMgr", "DeployMercenaryDen") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = session.solar_system_id.unwrap_or(30_002_904);
            let alliance_id = session.alliance_id.unwrap_or(99_000_001);
            let (mil_idx, ind_idx) = ctx.db.with_mut_snapshot(|snap| {
                let st = snap.system_states.entry(sys_id).or_insert_with(|| {
                    eve_db::SystemDynamicState {
                        solar_system_id: sys_id,
                        dbs_multiplier: 1.35,
                        ess_main_bank: eve_db::IskCents(0),
                        ess_reserve_bank: eve_db::IskCents(0),
                        sov_alliance_id: Some(alliance_id),
                        military_index: 3,
                        industrial_index: 3,
                        strategic_index: 3,
                        fw_contested_pct: 0.0,
                        insurgency_corruption_stage: 0,
                        insurgency_suppression_stage: 0,
                        wormhole_remaining_mass_kg: 0.0,
                    }
                });
                st.sov_alliance_id = Some(alliance_id);
                st.military_index = (st.military_index + 1).min(5);
                st.industrial_index = (st.industrial_index + 1).min(5);
                (st.military_index, st.industrial_index)
            });

            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("allianceID", PyRep::Integer(alliance_id)),
                ("militaryIndex", PyRep::Integer(mil_idx as i64)),
                ("industrialIndex", PyRep::Integer(ind_idx as i64)),
                ("skyhookOnline", PyRep::Bool(true)),
            ])))
        }

        ("sovMgr", _)
        | ("devIndexMgr", _)
        | ("facWarMgr", _)
        | ("fwCharacterEnlistmentMgr", _)
        | ("fwEnlistmentMgr", _)
        | ("insurgencyMgr", _)
        | ("pirateInsurgencySvc", _)
        | ("mercenaryDenMgr", _)
        | ("orbitalSkyhookMgr", _)
        | ("territorialClaimSvc", _)
        | ("allianceSovMgr", _) => Some(Ok(PyRep::Bool(true))),

        _ => None,
    }
}
