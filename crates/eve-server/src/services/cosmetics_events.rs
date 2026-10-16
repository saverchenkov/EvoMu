//! `cosmetics_events` — Domain 12: Ship SKINs, Paragon Hub, SKINR Studio, Events, & Living World Director (`§3.10`, `§3.11`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // Living World NPC Ecology & Supply-Chain Convoy Interdiction (`E2E-17`)
        ("livingWorld", "ListConvoys") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx
                .living_world
                .list_convoys()
                .into_iter()
                .map(|c| {
                    vec![
                        PyRep::Integer(c.convoy_id),
                        PyRep::Integer(c.current_system_id as i64),
                        PyRep::Integer(c.destination_station_id),
                        PyRep::Integer(c.cargo_type_id as i64),
                        PyRep::Integer(c.cargo_quantity as i64),
                        PyRep::Integer(c.lod_level as i64),
                        PyRep::Bool(c.destroyed),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "convoyID",
                    "solarSystemID",
                    "destinationStationID",
                    "cargoTypeID",
                    "cargoQuantity",
                    "lodLevel",
                    "destroyed",
                ],
                rows,
            )))
        }

        ("livingWorld", "MaterializeSystemLOD2") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let count = ctx
                .living_world
                .materialize_system_lod2(sys_id, &ctx.systems)
                .await;
            Some(Ok(PyRep::Integer(count as i64)))
        }

        ("livingWorld", "InterdictConvoy") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let convoy_id = args.first().and_then(|a| a.as_i64()).unwrap_or(800_000_001);
            match ctx
                .living_world
                .interdict_convoy(convoy_id, char_id, &ctx.db)
            {
                Ok((loot_item_id, spiked_ask)) => Some(Ok(PyRep::keyval([
                    ("convoyID", PyRep::Integer(convoy_id)),
                    ("lootItemID", PyRep::Integer(loot_item_id)),
                    ("newDestinationAskISK", PyRep::Real(spiked_ask.to_wire_f64())),
                ]))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "InterdictionFailed".into(),
                    message: e,
                })),
            }
        }

        ("skinrMgr", "SequenceSkinDesign") | ("cosmeticsSvc", "ApplySkin") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(587);
            let design_name = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Crimson Valkyrie SKIN");
            Some(Ok(PyRep::keyval([
                ("shipTypeID", PyRep::Integer(ship_type_id)),
                ("designName", PyRep::String(design_name.to_string())),
                ("sequenced", PyRep::Bool(true)),
            ])))
        }

        ("cosmeticsSvc", "GetAvailableSkins")
        | ("cosmeticsLicenseMgr", "GetOwnedLicenses")
        | ("shipSkinSvc", "GetSkins")
        | ("paragonMgr", "GetMissions")
        | ("eventMgr", "GetActiveEvents")
        | ("rewardMgr", "GetRewards") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(vec![])))
        }

        _ => None,
    }
}
