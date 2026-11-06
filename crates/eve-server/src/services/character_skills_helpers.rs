//! Tail dispatch helpers extracted from `character_skills` to keep module size under control.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub(crate) fn handle_skill_tail(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("skillMgr2", "GetImplants") => Some(crate::services::implant_plugging::get_implants(
            ctx, session,
        )),
        ("skillMgr2", "GetBoosters") => Some(crate::services::implant_plugging::get_boosters(
            ctx, session,
        )),
        ("skillMgr2", "GetSkillChangesForISIS") => Some(
            super::super::character_skill_ops::get_skill_changes_for_isis(session),
        ),
        (
            "skillMgr2",
            "ApplyFreeSkillPointsToQueue"
            | "ApplyFreeSkillPointsToSkills"
            | "CharStartTrainingSkill"
            | "CheckInjectionConstraints"
            | "CombineSkillInjector"
            | "SplitSkillInjector",
        ) => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::None))
        }
        ("skillMgr2", "GetDiminishedSpFromInjectors") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Integer(500_000)))
        }
        ("skillMgr2", "GetEstimatedSkillPoints") | ("skillMgr2", "GetSkillPoints") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let tid = args.first().and_then(|a| a.as_i64()).unwrap_or(0) as u32;
            let sp = ctx.db.with_snapshot(|snap| {
                if tid > 0 {
                    snap.skills
                        .get(&char_id)
                        .and_then(|m| m.get(&tid))
                        .map(|s| s.skill_points)
                } else {
                    None
                }
                .or_else(|| snap.characters.get(&char_id).map(|c| c.skill_points))
                .unwrap_or(50_000.0)
            });
            Some(Ok(PyRep::Real(sp)))
        }
        ("skillMgr2", "GetSkillLevel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let tid = args.first().and_then(|a| a.as_i64()).unwrap_or(3300) as u32;
            let lvl = ctx.db.with_snapshot(|snap| {
                snap.skills
                    .get(&char_id)
                    .and_then(|m| m.get(&tid))
                    .map(|s| s.level as i64)
                    .unwrap_or(1)
            });
            Some(Ok(PyRep::Integer(lvl)))
        }
        (
            "skillMgr2",
            "GetFreeSkillPointsAppliedToQueue" | "GetFreeSkillPointsAppliedToSkills",
        ) => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Dict(vec![(
                PyRep::Integer(3300),
                PyRep::Integer(10_000),
            )])))
        }
        ("skillMgr2", "GetSkillBundleInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let lvl = ctx.db.with_snapshot(|snap| {
                snap.skills
                    .get(&char_id)
                    .and_then(|m| m.get(&3300))
                    .map(|s| s.level as i64)
                    .unwrap_or(3)
            });
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Integer(lvl))]),
                PyRep::List(vec![]),
            ])))
        }
        ("dogmaIM", "InjectImplant") => Some(crate::services::implant_plugging::inject_implant(
            ctx, session, args,
        )),
        ("dogmaIM", "DestroyImplant") => Some(crate::services::implant_plugging::destroy_implant(
            ctx, session, args,
        )),
        ("dogmaIM", "UseBooster") => Some(crate::services::implant_plugging::use_booster(
            ctx, session, args,
        )),
        _ => crate::services::implant_plugging::handle_skill_notifications_and_injections(
            ctx, session, service, method,
        )
        .or_else(|| {
            super::character_clones_implants::handle_clones_and_sheet(
                ctx, session, service, method, args, _kwargs,
            )
        }),
    }
}
