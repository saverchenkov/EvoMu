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
    bound_entry: Option<&crate::router::BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let bound_planet = bound_entry.and_then(|b| {
        if b.service == "planetMgr" {
            Some(b.object_id)
        } else if b.service == "planetOrbitalRegistryBroker" {
            let is_sys = u32::try_from(b.object_id)
                .ok()
                .is_some_and(|id| ctx.sde.solar_systems.contains_key(&id));
            (!is_sys).then_some(b.object_id)
        } else {
            None
        }
    });

    match (service, method) {
        // Planetary Customs Office & Skyhook Registry Broker (`planetOrbitalRegistryBroker`, `feat:pi.customs-office`, `feat:pi.poco-settings`, `feat:pi.skyhook-pi`)
        ("planetOrbitalRegistryBroker", "GetTaxRate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(raw_id) = args.first().and_then(|a| a.as_i64()).or(bound_planet) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetTaxRate requires planetID".into(),
                    client_error: None,
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let (_, planet_id) = resolve_orbital_and_planet(snap, raw_id);
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
            let Some(raw_id) = args.first().and_then(|a| a.as_i64()).or(bound_planet) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetSettingsInfo requires planetID".into(),
                    client_error: None,
                }));
            };
            let cfg = ctx.db.with_snapshot(|snap| {
                let (_, pid) = resolve_orbital_and_planet(snap, raw_id);
                load_orbital_settings(snap, pid).unwrap_or(OrbitalCustomsSettings {
                    planet_id: pid,
                    ..Default::default()
                })
            });
            // V24 client `OrbitalConfigurationWindow.ApplyAttributes` unpacks 6-tuple:
            // `(selectedHour, taxRateValues, standingLevel, allowAlliance, allowStandings, selectedAcl)`.
            let tax_rate_values = PyRep::keyval([
                ("corporation", PyRep::Real(cfg.corporation_tax_rate)),
                ("alliance", PyRep::Real(cfg.alliance_tax_rate)),
                ("standingHigh", PyRep::Real(cfg.standing_high_tax_rate)),
                ("standingGood", PyRep::Real(cfg.standing_good_tax_rate)),
                (
                    "standingNeutral",
                    PyRep::Real(cfg.standing_neutral_tax_rate),
                ),
                ("standingBad", PyRep::Real(cfg.standing_bad_tax_rate)),
                (
                    "standingHorrible",
                    PyRep::Real(cfg.standing_horrible_tax_rate),
                ),
            ]);
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(0),
                tax_rate_values,
                PyRep::Real(cfg.minimum_standing),
                PyRep::Bool(cfg.allow_alliance),
                PyRep::Bool(cfg.allow_standings),
                PyRep::None,
            ])))
        }

        ("planetOrbitalRegistryBroker", "UpdateSettings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(raw_id) = args.first().and_then(|a| a.as_i64()).or(bound_planet) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidOrbitalID".into(),
                    message: "UpdateSettings requires orbitalID".into(),
                    client_error: None,
                }));
            };
            let (office_id, planet_id, res) = match ctx.db.try_with_mut_snapshot(|snap| {
                let (orbital_id_opt, planet_id) = resolve_orbital_and_planet(snap, raw_id);
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
                            client_error: None,
                        });
                    };
                    if existing.owner_corp_id != char_corp {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: format!("Orbital at planet {planet_id} belongs to corporation {}", existing.owner_corp_id),
                            client_error: None,
                        });
                    }
                    if !snap.corporations.contains_key(&char_corp) {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Updating orbital settings requires a player corporation".into(),
                            client_error: None,
                        });
                    }
                    let (is_ceo, role) = snap.effective_corp_roles_at(
                        char_corp,
                        char_id,
                        None,
                        session.corp_role,
                        ctx.now_unix(),
                    );
                    let allowed = eve_db::corp_roles::ROLE_DIRECTOR
                        | eve_db::corp_roles::ROLE_STATION_MANAGER
                        | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG;
                    if !is_ceo && (role & allowed) == 0 {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Updating orbital settings requires Director or Station Manager role".into(),
                            client_error: None,
                        });
                    }
                }
                let mut cfg = existing_opt.unwrap_or(OrbitalCustomsSettings {
                    planet_id,
                    owner_corp_id: char_corp,
                    ..Default::default()
                });
                if let Some(otid) = kwargs.get("orbitalTypeID").and_then(|v| v.as_i64()) {
                    if otid == 4318 && !session.has_gm_role() {
                        return Err(BridgeRpcError {
                            code: "InvalidOrbitalType".into(),
                            message: "Players cannot set orbitalTypeID to NPC InterBus Customs Office (4318)".into(),
                            client_error: None,
                        });
                    }
                    if matches!(otid, 2233 | 4318 | 81080) {
                        cfg.orbital_type_id = otid as TypeId;
                    }
                }
                // Parse either V24 `(orbitalID, selectedHour, taxRateValues, standingLevel, allowAlliance, allowStandings, selectedAcl)` or KeyVal/kwargs
                let tax_rep = args
                    .get(2)
                    .filter(|v| matches!(v, PyRep::KeyVal(_) | PyRep::Dict(_)))
                    .or_else(|| {
                        args.get(1)
                            .filter(|v| matches!(v, PyRep::KeyVal(_) | PyRep::Dict(_)))
                    })
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
                        if v == 4318 && !session.has_gm_role() {
                            return Err(BridgeRpcError {
                                code: "InvalidOrbitalType".into(),
                                message: "Players cannot set orbitalTypeID to NPC InterBus Customs Office (4318)".into(),
                                client_error: None,
                            });
                        }
                        if matches!(v, 2233 | 4318 | 81080) {
                            cfg.orbital_type_id = v as TypeId;
                        }
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
                    .or_else(|| kwargs.get("standingLevel").and_then(|v| v.as_f64()))
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
                let save_key = orbital_id_opt.unwrap_or(planet_id);
                save_orbital_settings(snap, save_key, &cfg);
                Ok((save_key, planet_id, cfg))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // `planetSvc.OnMajorPlanetStateUpdate(planetID, numCommandCentersChanged)`.
            let _ = ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnMajorPlanetStateUpdate {
                    planet_id,
                    num_command_centers_changed: false,
                },
            );
            Some(Ok(PyRep::keyval([
                ("orbitalID", PyRep::Integer(office_id)),
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
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()).or(bound_planet) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "RevertOrbitalsToInterBus requires planetID".into(),
                    client_error: None,
                }));
            };
            if let Err(e) = ctx.db.try_with_mut_snapshot(|snap| {
                if !session.has_gm_role() {
                    let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                    let Some(existing) = load_orbital_settings(snap, planet_id) else {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Only an existing corporation-owned Customs Office can be reverted to InterBus".into(),
                            client_error: None,
                        });
                    };
                    if existing.owner_corp_id != char_corp {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Only the owning corporation or a GM can revert orbital ownership to InterBus".into(),
                            client_error: None,
                        });
                    }
                    if !snap.corporations.contains_key(&char_corp) {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Reverting orbital ownership to InterBus requires a player corporation Director or Station Manager role".into(),
                            client_error: None,
                        });
                    }
                    let (is_ceo, role) = snap.effective_corp_roles_at(
                        char_corp,
                        char_id,
                        None,
                        session.corp_role,
                        ctx.now_unix(),
                    );
                    let allowed = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
                    if !is_ceo && (role & allowed) == 0 {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Reverting orbital ownership to InterBus requires Director or Station Manager role".into(),
                            client_error: None,
                        });
                    }
                }
                let cfg = OrbitalCustomsSettings {
                    planet_id,
                    orbital_type_id: 4318, // InterBus Customs Office
                    owner_corp_id: 1_000_148,
                    corporation_tax_rate: 0.17,
                    alliance_tax_rate: 0.17,
                    standing_high_tax_rate: 0.17,
                    standing_good_tax_rate: 0.17,
                    standing_neutral_tax_rate: 0.17,
                    standing_bad_tax_rate: 0.17,
                    standing_horrible_tax_rate: 0.17,
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
            let _ = ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnMajorPlanetStateUpdate {
                    planet_id,
                    num_command_centers_changed: false,
                },
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

#[cfg(test)]
#[path = "pi_orbital_registry_tests.rs"]
mod pi_orbital_registry_tests;

