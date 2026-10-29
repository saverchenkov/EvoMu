//! `character_certificates` — Certificate manager service (`feat:skills.certificates`).
//!
//! Reports certificate levels derived from character trained skills.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::WorldStateSnapshot;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Authorizes mutating certificate operations: requires GM role (`ROLE_GM_MASK` or `has_gm_role()`).
fn check_target_auth(
    session: &SessionState,
    _caller_cid: i64,
    _target_cid: i64,
) -> Result<(), BridgeRpcError> {
    if session.has_gm_role() || (session.role & eve_machonet::service_roles::ROLE_GM_MASK) != 0 {
        return Ok(());
    }
    Err(BridgeRpcError::user_error("AccessDenied"))
}

/// Authorizes updating certificate display flags (own character or GM).
fn check_flags_auth(
    session: &SessionState,
    caller_cid: i64,
    target_cid: i64,
) -> Result<(), BridgeRpcError> {
    if target_cid == caller_cid
        || session.has_gm_role()
        || (session.role & eve_machonet::service_roles::ROLE_GM_MASK) != 0
    {
        return Ok(());
    }
    Err(BridgeRpcError::user_error("AccessDenied"))
}

/// Persistent certificate state for a character stored in `snap.service_state_blobs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CharacterCertificateState {
    #[serde(default)]
    pub certificates: HashMap<u32, u8>,
    #[serde(default)]
    pub flags: HashMap<u32, u32>,
}

fn cert_blob_key(char_id: i64) -> String {
    format!("character_certificates:{char_id}")
}

pub fn load_certificate_state(snap: &WorldStateSnapshot, char_id: i64) -> CharacterCertificateState {
    let key = cert_blob_key(char_id);
    if let Some(blob) = snap.service_state_blobs.get(&key) {
        if let Ok(st) = serde_json::from_str::<CharacterCertificateState>(blob) {
            return st;
        }
    }
    let legacy_key = format!("char_certs:{char_id}");
    if let Some(blob) = snap.service_state_blobs.get(&legacy_key) {
        if let Ok(certs) = serde_json::from_str::<HashMap<u32, u8>>(blob) {
            return CharacterCertificateState {
                certificates: certs,
                flags: HashMap::new(),
            };
        }
    }
    CharacterCertificateState::default()
}

pub fn save_certificate_state(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    state: &CharacterCertificateState,
) {
    let key = cert_blob_key(char_id);
    if let Ok(blob) = serde_json::to_string(state) {
        snap.service_state_blobs.insert(key, blob);
    }
}

/// Derives certificate levels (0..=5) for a character based on their trained skill levels.
pub fn derive_certificate_levels(
    ctx: &ServerContext,
    char_id: i64,
) -> HashMap<u32, u8> {
    let trained_skills = ctx.character_skill_levels(char_id);
    let mut cert_levels = HashMap::new();

    if let Ok(bundle) = ctx.sde.load_certificates_and_masteries() {
        for (&cid, cert) in &bundle.certificates {
            if cert.skill_types.is_empty() {
                continue;
            }
            let mut cert_level = 0u8;
            for lvl in (1u8..=5u8).rev() {
                let tier_idx = (lvl - 1) as usize;
                let has_any_req = cert
                    .skill_types
                    .values()
                    .any(|req_levels| req_levels[tier_idx] > 0);
                let all_met = has_any_req
                    && cert.skill_types.iter().all(|(&req_skill_id, req_levels)| {
                        let req_lvl = req_levels[tier_idx];
                        req_lvl == 0
                            || trained_skills.get(&req_skill_id).copied().unwrap_or(0) >= req_lvl
                    });
                if all_met {
                    cert_level = lvl;
                    break;
                }
            }
            if cert_level > 0 {
                cert_levels.insert(cid, cert_level);
            }
        }
    }

    cert_levels
}

/// Dispatches `certificateMgr` RPCs (`feat:skills.certificates`).
pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    // rpc-lint: allow crucible legacy service certificateMgr in rpc_service_catalog.json
    if service != "certificateMgr" {
        return None;
    }

    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    match method {
        "GetMyCertificates" => {
            let mut cert_map = derive_certificate_levels(ctx, char_id);
            if session.has_gm_role()
                || (session.role & eve_machonet::service_roles::ROLE_GM_MASK) != 0
            {
                ctx.db.with_snapshot(|snap| {
                    let state = load_certificate_state(snap, char_id);
                    for (cid, lvl) in state.certificates {
                        cert_map
                            .entry(cid)
                            .and_modify(|cur| *cur = (*cur).max(lvl))
                            .or_insert(lvl);
                    }
                });
            }
            let entries: Vec<(PyRep, PyRep)> = cert_map
                .into_iter()
                .map(|(cid, lvl)| (PyRep::Integer(cid as i64), PyRep::Integer(lvl as i64)))
                .collect();
            let flags_map = ctx.db.with_snapshot(|snap| load_certificate_state(snap, char_id).flags);
            let flag_entries: Vec<(PyRep, PyRep)> = flags_map
                .into_iter()
                .map(|(cid, f)| (PyRep::Integer(cid as i64), PyRep::Integer(f as i64)))
                .collect();
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("certificates", PyRep::Dict(entries)),
                ("certificateFlags", PyRep::Dict(flag_entries)),
            ])))
        }

        "GetCertificatesByCharacter" => {
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or(char_id);
            let mut cert_map = derive_certificate_levels(ctx, target_char_id);
            if session.has_gm_role()
                || (session.role & eve_machonet::service_roles::ROLE_GM_MASK) != 0
            {
                ctx.db.with_snapshot(|snap| {
                    let state = load_certificate_state(snap, target_char_id);
                    for (cid, lvl) in state.certificates {
                        cert_map
                            .entry(cid)
                            .and_modify(|cur| *cur = (*cur).max(lvl))
                            .or_insert(lvl);
                    }
                });
            }
            let entries: Vec<(PyRep, PyRep)> = cert_map
                .into_iter()
                .map(|(cid, lvl)| (PyRep::Integer(cid as i64), PyRep::Integer(lvl as i64)))
                .collect();
            let flags_map = ctx.db.with_snapshot(|snap| load_certificate_state(snap, target_char_id).flags);
            let flag_entries: Vec<(PyRep, PyRep)> = flags_map
                .into_iter()
                .map(|(cid, f)| (PyRep::Integer(cid as i64), PyRep::Integer(f as i64)))
                .collect();
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(target_char_id)),
                ("certificates", PyRep::Dict(entries)),
                ("certificateFlags", PyRep::Dict(flag_entries)),
            ])))
        }

        "GetCertificateCategories" => {
            let mut categories_map: HashMap<u32, String> = HashMap::new();
            if let Ok(bundle) = ctx.sde.load_certificates_and_masteries() {
                for cert in bundle.certificates.values() {
                    if let Some(grp) = ctx.sde.groups.get(&cert.group_id) {
                        let cat_id = grp.category_id;
                        let cat_name = ctx
                            .sde
                            .categories
                            .get(&cat_id)
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| format!("Category {cat_id}"));
                        categories_map.insert(cat_id, cat_name);
                    }
                }
            }
            let mut list: Vec<(u32, String)> = categories_map.into_iter().collect();
            list.sort_by_key(|(id, _)| *id);
            let categories: Vec<PyRep> = list
                .into_iter()
                .map(|(id, name)| {
                    PyRep::keyval([
                        ("categoryID", PyRep::Integer(id as i64)),
                        ("categoryName", PyRep::String(name)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(categories)))
        }

        "GetCertificateClasses" => {
            let mut classes_map: HashMap<u32, (String, u32)> = HashMap::new();
            if let Ok(bundle) = ctx.sde.load_certificates_and_masteries() {
                for cert in bundle.certificates.values() {
                    if let Some(grp) = ctx.sde.groups.get(&cert.group_id) {
                        classes_map.insert(grp.group_id, (grp.name.clone(), grp.category_id));
                    }
                }
            }
            let mut list: Vec<(u32, String, u32)> = classes_map
                .into_iter()
                .map(|(gid, (name, cid))| (gid, name, cid))
                .collect();
            list.sort_by_key(|(id, _, _)| *id);
            let classes: Vec<PyRep> = list
                .into_iter()
                .map(|(id, name, cat_id)| {
                    PyRep::keyval([
                        ("classID", PyRep::Integer(id as i64)),
                        ("className", PyRep::String(name)),
                        ("categoryID", PyRep::Integer(cat_id as i64)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(classes)))
        }

        "GetAllShipCertificateRecommendations" => {
            let ship_type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(587) as u32;
            let mut cert_ids = Vec::new();
            if let Ok(bundle) = ctx.sde.load_certificates_and_masteries() {
                if let Some(levels) = bundle.masteries_by_ship_type.get(&ship_type_id) {
                    for lvl_certs in levels {
                        for &cid in lvl_certs {
                            if !cert_ids.contains(&cid) {
                                cert_ids.push(cid);
                            }
                        }
                    }
                }
                for (&cid, cert) in &bundle.certificates {
                    if cert.recommended_for.contains(&ship_type_id) && !cert_ids.contains(&cid) {
                        cert_ids.push(cid);
                    }
                }
            }
            let recs: Vec<PyRep> = cert_ids
                .into_iter()
                .map(|cid| PyRep::Integer(cid as i64))
                .collect();
            Some(Ok(PyRep::keyval([
                ("shipTypeID", PyRep::Integer(ship_type_id as i64)),
                ("recommendations", PyRep::List(recs)),
            ])))
        }

        "GrantCertificate" => {
            let (target_cid, cert_id, level) = if args.len() >= 3 {
                (
                    args[0].as_i64().unwrap_or(char_id),
                    args[1].as_i64().unwrap_or(0) as u32,
                    args[2].as_i64().unwrap_or(1) as u8,
                )
            } else if args.len() == 2 {
                (
                    char_id,
                    args[0].as_i64().unwrap_or(0) as u32,
                    args[1].as_i64().unwrap_or(1) as u8,
                )
            } else {
                (
                    char_id,
                    args.first().and_then(|a| a.as_i64()).unwrap_or(0) as u32,
                    1u8,
                )
            };

            if let Err(e) = check_target_auth(session, char_id, target_cid) {
                return Some(Err(e));
            }

            let level = level.clamp(0, 5);
            ctx.db.with_mut_snapshot(|snap| {
                let mut st = load_certificate_state(snap, target_cid);
                st.certificates.insert(cert_id, level);
                save_certificate_state(snap, target_cid, &st);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "BatchCertificateGrant" => {
            // args: [items] or [target_cid, items]
            let (target_cid, items_rep) = if args.len() >= 2
                && args[0].as_i64().is_some()
                && !matches!(args[0], PyRep::List(_) | PyRep::Dict(_))
            {
                (args[0].as_i64().unwrap(), &args[1])
            } else if let Some(first) = args.first() {
                (char_id, first)
            } else {
                return Some(Ok(PyRep::Bool(true)));
            };

            if let Err(e) = check_target_auth(session, char_id, target_cid) {
                return Some(Err(e));
            }

            ctx.db.with_mut_snapshot(|snap| {
                let mut st = load_certificate_state(snap, target_cid);
                match items_rep {
                    PyRep::List(items) => {
                        for item in items {
                            match item {
                                PyRep::Integer(cid) => {
                                    st.certificates.insert(*cid as u32, 1);
                                }
                                PyRep::Tuple(t) | PyRep::List(t) => {
                                    if let (Some(cid), Some(lvl)) = (
                                        t.first().and_then(|v| v.as_i64()),
                                        t.get(1).and_then(|v| v.as_i64()),
                                    ) {
                                        st.certificates.insert(cid as u32, (lvl as u8).clamp(0, 5));
                                    } else if let Some(cid) = t.first().and_then(|v| v.as_i64()) {
                                        st.certificates.insert(cid as u32, 1);
                                    }
                                }
                                PyRep::Dict(entries) => {
                                    for (k, v) in entries {
                                        if let (Some(cid), Some(lvl)) = (k.as_i64(), v.as_i64()) {
                                            st.certificates.insert(cid as u32, (lvl as u8).clamp(0, 5));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    PyRep::Dict(entries) => {
                        for (k, v) in entries {
                            if let (Some(cid), Some(lvl)) = (k.as_i64(), v.as_i64()) {
                                st.certificates.insert(cid as u32, (lvl as u8).clamp(0, 5));
                            }
                        }
                    }
                    _ => {}
                }
                save_certificate_state(snap, target_cid, &st);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "BatchCertificateUpdate" => {
            // args: [items] or [target_cid, items]
            let (target_cid, items_rep) = if args.len() >= 2
                && args[0].as_i64().is_some()
                && !matches!(args[0], PyRep::List(_) | PyRep::Dict(_))
            {
                (args[0].as_i64().unwrap(), &args[1])
            } else if let Some(first) = args.first() {
                (char_id, first)
            } else {
                return Some(Ok(PyRep::Bool(true)));
            };

            if let Err(e) = check_target_auth(session, char_id, target_cid) {
                return Some(Err(e));
            }

            ctx.db.with_mut_snapshot(|snap| {
                let mut st = load_certificate_state(snap, target_cid);
                match items_rep {
                    PyRep::List(items) => {
                        for item in items {
                            match item {
                                PyRep::Tuple(t) | PyRep::List(t) => {
                                    if t.len() >= 3 {
                                        if let (Some(cid), Some(lvl), Some(flg)) =
                                            (t[0].as_i64(), t[1].as_i64(), t[2].as_i64())
                                        {
                                            st.certificates.insert(cid as u32, (lvl as u8).clamp(0, 5));
                                            st.flags.insert(cid as u32, flg as u32);
                                        }
                                    } else if let (Some(cid), Some(lvl)) = (
                                        t.first().and_then(|v| v.as_i64()),
                                        t.get(1).and_then(|v| v.as_i64()),
                                    ) {
                                        st.certificates.insert(cid as u32, (lvl as u8).clamp(0, 5));
                                    }
                                }
                                PyRep::Dict(entries) => {
                                    for (k, v) in entries {
                                        if let (Some(cid), Some(lvl)) = (k.as_i64(), v.as_i64()) {
                                            st.certificates.insert(cid as u32, (lvl as u8).clamp(0, 5));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    PyRep::Dict(entries) => {
                        for (k, v) in entries {
                            if let (Some(cid), Some(lvl)) = (k.as_i64(), v.as_i64()) {
                                st.certificates.insert(cid as u32, (lvl as u8).clamp(0, 5));
                            }
                        }
                    }
                    _ => {}
                }
                save_certificate_state(snap, target_cid, &st);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        "UpdateCertificateFlags" => {
            // args: [cert_id, flags] or [target_cid, cert_id, flags] or [dict_or_list]
            let (target_cid, updates) = if args.len() >= 3
                && args[1].as_i64().is_some()
                && args[2].as_i64().is_some()
            {
                let tcid = args[0].as_i64().unwrap_or(char_id);
                let cid = args[1].as_i64().unwrap() as u32;
                let flg = args[2].as_i64().unwrap() as u32;
                (tcid, vec![(cid, flg)])
            } else if args.len() >= 2
                && args[0].as_i64().is_some()
                && args[1].as_i64().is_some()
            {
                let cid = args[0].as_i64().unwrap() as u32;
                let flg = args[1].as_i64().unwrap() as u32;
                (char_id, vec![(cid, flg)])
            } else if let Some(first) = args.first() {
                let mut v = Vec::new();
                match first {
                    PyRep::Dict(entries) => {
                        for (k, val) in entries {
                            if let (Some(cid), Some(flg)) = (k.as_i64(), val.as_i64()) {
                                v.push((cid as u32, flg as u32));
                            }
                        }
                    }
                    PyRep::List(items) => {
                        for item in items {
                            if let PyRep::Tuple(t) | PyRep::List(t) = item {
                                if let (Some(cid), Some(flg)) = (
                                    t.first().and_then(|x| x.as_i64()),
                                    t.get(1).and_then(|x| x.as_i64()),
                                ) {
                                    v.push((cid as u32, flg as u32));
                                }
                            }
                        }
                    }
                    _ => {}
                }
                (char_id, v)
            } else {
                (char_id, Vec::new())
            };

            if let Err(e) = check_flags_auth(session, char_id, target_cid) {
                return Some(Err(e));
            }

            ctx.db.with_mut_snapshot(|snap| {
                let mut st = load_certificate_state(snap, target_cid);
                for (cid, flg) in updates {
                    st.flags.insert(cid, flg);
                }
                save_certificate_state(snap, target_cid, &st);
            });
            Some(Ok(PyRep::Bool(true)))
        }

        _ => None,
    }
}
