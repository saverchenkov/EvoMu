use crate::router::ServerContext;
pub use crate::services::industry_pi::pi_helpers::*;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub fn handle_pi_orbital_registry(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // Planetary Customs Office & Skyhook Registry Broker (`planetOrbitalRegistryBroker`, `feat:pi.customs-office`, `feat:pi.poco-settings`, `feat:pi.skyhook-pi`)
        ("planetOrbitalRegistryBroker", "GetTaxRate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let res = ctx.db.with_snapshot(|snap| {
                let (player_rate, _) =
                    resolve_character_orbital_access_and_tax(snap, char_id, planet_id)?;
                Ok(PyRep::Real(player_rate))
            });
            Some(res)
        }

        ("planetOrbitalRegistryBroker", "GetSettingsInfo") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let cfg = ctx.db.with_snapshot(|snap| {
                load_orbital_settings(snap, planet_id).unwrap_or(OrbitalCustomsSettings {
                    planet_id,
                    ..Default::default()
                })
            });
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("orbitalTypeID", PyRep::Integer(cfg.orbital_type_id as i64)),
                ("ownerCorpID", PyRep::Integer(cfg.owner_corp_id)),
                ("corporationTaxRate", PyRep::Real(cfg.corporation_tax_rate)),
                ("allianceTaxRate", PyRep::Real(cfg.alliance_tax_rate)),
                (
                    "standingHighTaxRate",
                    PyRep::Real(cfg.standing_high_tax_rate),
                ),
                (
                    "standingGoodTaxRate",
                    PyRep::Real(cfg.standing_good_tax_rate),
                ),
                (
                    "standingNeutralTaxRate",
                    PyRep::Real(cfg.standing_neutral_tax_rate),
                ),
                ("standingBadTaxRate", PyRep::Real(cfg.standing_bad_tax_rate)),
                (
                    "standingHorribleTaxRate",
                    PyRep::Real(cfg.standing_horrible_tax_rate),
                ),
                ("allowAlliance", PyRep::Bool(cfg.allow_alliance)),
                ("allowStandings", PyRep::Bool(cfg.allow_standings)),
                ("minimumStanding", PyRep::Real(cfg.minimum_standing)),
            ])))
        }

        ("planetOrbitalRegistryBroker", "UpdateSettings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let res = match ctx.db.try_with_mut_snapshot(|snap| {
                let char_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(1_000_009);
                let existing_opt = load_orbital_settings(snap, planet_id);
                if !session.has_gm_role() {
                    let Some(existing) = &existing_opt else {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: format!("No player corporation orbital deployed at planet {planet_id}"),
                        });
                    };
                    if existing.owner_corp_id != char_corp {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: format!("Orbital at planet {planet_id} belongs to corporation {}", existing.owner_corp_id),
                        });
                    }
                    let Some(corp) = snap.corporations.get(&char_corp) else {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Updating orbital settings requires a player corporation".into(),
                        });
                    };
                    let role = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
                    let allowed = eve_db::corp_roles::ROLE_DIRECTOR
                        | eve_db::corp_roles::ROLE_STATION_MANAGER
                        | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG;
                    if corp.ceo_id != char_id && (role & allowed) == 0 {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Updating orbital settings requires Director or Station Manager role".into(),
                        });
                    }
                }
                let mut cfg = existing_opt.unwrap_or(OrbitalCustomsSettings {
                    planet_id,
                    owner_corp_id: char_corp,
                    ..Default::default()
                });
                if let Some(otid) = kwargs
                    .get("orbitalTypeID")
                    .and_then(|v| v.as_i64())
                    .filter(|&id| matches!(id, 2233 | 4318 | 81080))
                {
                    cfg.orbital_type_id = otid as TypeId;
                }
                // Parse either V24 `(orbitalID, selectedHour, taxRateValues, standingValue, allowAlliance, allowStandings)` or KeyVal/kwargs
                let tax_rep = args
                    .get(2)
                    .or_else(|| args.get(1))
                    .or_else(|| kwargs.get("taxRateValues"));
                if let Some(PyRep::KeyVal(kv)) = tax_rep {
                    if let Some(v) = kv.get("corporation").and_then(|x| x.as_f64()) {
                        cfg.corporation_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("alliance").and_then(|x| x.as_f64()) {
                        cfg.alliance_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingHigh").and_then(|x| x.as_f64()) {
                        cfg.standing_high_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingGood").and_then(|x| x.as_f64()) {
                        cfg.standing_good_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingNeutral").and_then(|x| x.as_f64()) {
                        cfg.standing_neutral_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingBad").and_then(|x| x.as_f64()) {
                        cfg.standing_bad_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingHorrible").and_then(|x| x.as_f64()) {
                        cfg.standing_horrible_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("orbitalTypeID").and_then(|x| x.as_i64()) {
                        cfg.orbital_type_id = v as TypeId;
                    }
                    if let Some(v) = kv.get("minimumStanding").and_then(|x| x.as_f64()) {
                        cfg.minimum_standing = v.clamp(-10.0, 10.0);
                    }
                    if let Some(v) = kv.get("allowAlliance").and_then(|x| x.as_bool()) {
                        cfg.allow_alliance = v;
                    }
                    if let Some(v) = kv.get("allowStandings").and_then(|x| x.as_bool()) {
                        cfg.allow_standings = v;
                    }
                }
                if let Some(st_val) = args
                    .get(3)
                    .and_then(|a| a.as_f64())
                    .or_else(|| kwargs.get("minimumStanding").and_then(|v| v.as_f64()))
                {
                    cfg.minimum_standing = st_val.clamp(-10.0, 10.0);
                }
                if let Some(aa) = args
                    .get(4)
                    .and_then(|a| a.as_bool())
                    .or_else(|| kwargs.get("allowAlliance").and_then(|v| v.as_bool()))
                {
                    cfg.allow_alliance = aa;
                }
                if let Some(as_flag) = args
                    .get(5)
                    .and_then(|a| a.as_bool())
                    .or_else(|| kwargs.get("allowStandings").and_then(|v| v.as_bool()))
                {
                    cfg.allow_standings = as_flag;
                }
                save_orbital_settings(snap, planet_id, &cfg);
                Ok(cfg)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // `planetSvc.OnMajorPlanetStateUpdate(planetID, numCommandCentersChanged)`.
            let _ = ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnMajorPlanetStateUpdate",
                "charid",
                vec![PyRep::Integer(planet_id), PyRep::Bool(false)],
            );
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("orbitalTypeID", PyRep::Integer(res.orbital_type_id as i64)),
                ("ownerCorpID", PyRep::Integer(res.owner_corp_id)),
                ("corporationTaxRate", PyRep::Real(res.corporation_tax_rate)),
                ("allianceTaxRate", PyRep::Real(res.alliance_tax_rate)),
                (
                    "standingNeutralTaxRate",
                    PyRep::Real(res.standing_neutral_tax_rate),
                ),
                ("allowAlliance", PyRep::Bool(res.allow_alliance)),
                ("allowStandings", PyRep::Bool(res.allow_standings)),
                ("minimumStanding", PyRep::Real(res.minimum_standing)),
            ])))
        }

        ("planetOrbitalRegistryBroker", "RevertOrbitalsToInterBus") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            if let Err(e) = ctx.db.try_with_mut_snapshot(|snap| {
                if !session.has_gm_role() {
                    let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                    let existing_owner = load_orbital_settings(snap, planet_id)
                        .map(|c| c.owner_corp_id)
                        .unwrap_or(char_corp);
                    if existing_owner != char_corp {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Only the owning corporation or a GM can revert orbital ownership to InterBus".into(),
                        });
                    }
                    let Some(corp) = snap.corporations.get(&char_corp) else {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Reverting orbital ownership to InterBus requires a player corporation Director or Station Manager role".into(),
                        });
                    };
                    let role = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
                    let allowed = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
                    if corp.ceo_id != char_id && (role & allowed) == 0 {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Reverting orbital ownership to InterBus requires Director or Station Manager role".into(),
                        });
                    }
                }
                let cfg = OrbitalCustomsSettings {
                    planet_id,
                    orbital_type_id: 4318, // InterBus Customs Office
                    owner_corp_id: 1_000_148,
                    corporation_tax_rate: 0.0,
                    alliance_tax_rate: 0.0,
                    standing_high_tax_rate: 0.0,
                    standing_good_tax_rate: 0.0,
                    standing_neutral_tax_rate: 0.0,
                    standing_bad_tax_rate: 0.0,
                    standing_horrible_tax_rate: 0.0,
                    allow_alliance: true,
                    allow_standings: true,
                    minimum_standing: -10.0,
                };
                save_orbital_settings(snap, planet_id, &cfg);
                Ok(())
            }) {
                return Some(Err(e));
            }
            // `planetSvc.OnMajorPlanetStateUpdate(planetID, numCommandCentersChanged)`.
            let _ = ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnMajorPlanetStateUpdate",
                "charid",
                vec![PyRep::Integer(planet_id), PyRep::Bool(false)],
            );
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("orbitalTypeID", PyRep::Integer(4318)),
                ("ownerCorpID", PyRep::Integer(1_000_148)),
            ])))
        }

        _ => None,
    }
}
