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
        // Living World NPC Ecology & Supply-Chain Convoy Interdiction (`taleMgr` / `E2E-17`)
        ("taleMgr", "ListConvoys") | ("taleMgr", "GetGlobalWorldEventTales") => {
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

        ("taleMgr", "MaterializeSystemLOD2") | ("taleMgr", "get_active_tales_by_template") => {
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

        ("taleMgr", "InterdictConvoy") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot interdict a space convoy while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let convoy_id = args.first().and_then(|a| a.as_i64()).unwrap_or(800_000_001);
            let Some(convoy) = ctx.living_world.get_convoy(convoy_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ConvoyNotFound".into(),
                    message: format!("Convoy {convoy_id} does not exist"),
                }));
            };
            if session.solar_system_id != Some(convoy.current_system_id) {
                return Some(Err(BridgeRpcError {
                    code: "ConvoyWrongSystem".into(),
                    message: format!(
                        "Convoy {convoy_id} is in system {}, but attacker is in {:?}",
                        convoy.current_system_id, session.solar_system_id
                    ),
                }));
            }

            let sys_handle = ctx.systems.get_or_spawn(convoy.current_system_id);
            let Some((_, ship_ball)) = (match sys_handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Attacker ship is not in space".into(),
                }));
            };

            let Some((_, convoy_ball)) = (match sys_handle.get_ball_snapshot(convoy_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ConvoyNotMaterializedOnGrid".into(),
                    message: format!(
                        "Convoy {convoy_id} is not materialized on grid in Ballpark"
                    ),
                }));
            };

            let dist = ship_ball.position.distance(convoy_ball.position);
            if dist > 30_000.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutsideConvoyInterdictionRange".into(),
                    message: format!(
                        "Ship is {:.1} m from convoy {convoy_id} (must be <= 30000 m)",
                        dist
                    ),
                }));
            }

            let _ = sys_handle.remove_ball(convoy_id).await;
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

        ("shipCosmeticsMgr", "ApplySkinToShip")
        | ("shipCosmeticsMgr", "ActivateSkinLicense")
        | ("shipCosmeticsMgr", "GetLicencedSkins")
        | ("shipCosmeticsMgr", "GetFirstPartySkinData")
        | ("shipCosmeticsMgr", "GetEnabledCosmetics")
        | ("rewardMgr", "GetDelayedRewardsByGroupIDs")
        | ("rewardMgr", "GetRewardLPLogs")
        | ("seasonalLoginCampaignManager", "get_active_campaign")
        | ("seasonalLoginCampaignManager", "claim_reward") => {
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

        _ => None,
    }
}
