//! `character_gm_tools` — Unbound Character Creation/Selection (`CH-3`) & GM Slash/QA Command Dispatcher (`CH-5`).

#![forbid(unsafe_code)]

use super::{
    catch_up_skill_queue, is_valid_doll_or_portrait_payload, notify_watchlist_logon,
    serialize_doll_pyrep, serialize_portrait_pyrep,
};
use crate::combat::death_pipeline::execute_death_pipeline;
use crate::router::notify_registry::{
    OnModuleFitted, OnSecurityStatusUpdate, OnSetPlayerStanding,
};
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, KillmailRecord, SkillHistoryEntry};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use glam::DVec3;
use std::collections::HashMap;

fn clear_session_selection_state(session: &mut SessionState) {
    session.character_id = None;
    session.corporation_id = None;
    session.alliance_id = None;
    session.war_faction_id = None;
    session.corp_role = 0;
    session.fleet_id = None;
    crate::router::clear_session_character_state(session);
}

fn displaced_selection_error(
    ctx: &ServerContext,
    user_id: u64,
    session_id: u64,
) -> BridgeRpcError {
    match ctx.db.check_session_revoked(user_id, session_id, ctx.now_unix()) {
        Some((code, msg)) if code == "MultiLoginBlocked" => {
            crate::router::binding::session_kicked_error(msg)
        }
        Some((code, msg)) => crate::router::binding::session_revoked_error(code, msg),
        None => crate::router::binding::session_kicked_error(
            "Session displaced before character selection completed",
        ),
    }
}

pub(crate) fn finalize_displaced_character_selection(
    ctx: &ServerContext,
    session: &mut SessionState,
    user_id: u64,
    char_id: i64,
) -> BridgeRpcError {
    let err = displaced_selection_error(ctx, user_id, session.session_id);
    let _ = ctx.take_pending_displaced_logoff(session.session_id);
    let should_relogoff = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .is_some_and(|ch| ch.user_id == user_id)
    }) && !ctx.is_character_online(char_id, user_id)
        && !crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id);
    if should_relogoff {
        ctx.logoff_character_sync(char_id, None);
    }
    clear_session_selection_state(session);
    err
}

/// DERIVED: Validates character name syntax, ASCII alphanumeric + space/hyphen/apostrophe rules,
/// word count (1..=3 words, 3..=37 chars), and reserved prefix/banned token checks (`feat:character.name-validation`).
/// CCP's server-side profanity/reserved-word table is not part of the client staticdata distribution.
pub fn is_valid_character_name_syntax(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.len() != name.len() || !(3..=37).contains(&trimmed.len()) {
        return false;
    }
    let bytes = trimmed.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    let mut space_count = 0usize;
    let mut prev_special = false;
    for ch in trimmed.chars() {
        let is_special = matches!(ch, ' ' | '-' | '\'');
        if !ch.is_ascii_alphanumeric() && !is_special {
            return false;
        }
        if ch == ' ' {
            space_count += 1;
        }
        if is_special && prev_special {
            return false;
        }
        prev_special = is_special;
    }
    if space_count > 2 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if ["ccp ", "ccp-", "ccp", "gm ", "isd ", "dev "].iter().any(|p| lower.starts_with(p)) {
        return false;
    }
    if ["admin", "moderator", "concord", "fuck", "shit", "hitler", "nazi"].iter().any(|b| lower.contains(b)) {
        return false;
    }
    true
}

/// DERIVED: Fallback first and family name pools per bloodline when SDE agent names are unavailable (`feat:character.random-name`).
/// CCP's server-side random character name tables are not shipped in the client staticdata archives.
pub fn bloodline_name_pools(bloodline_id: u32) -> (&'static [&'static str], &'static [&'static str]) {
    match bloodline_id {
        // Caldari: Deteis (1), Civire (2), Achura (11)
        1 => (&["Tibus", "Kari", "Matti", "Tuomas", "Eetu", "Sanna", "Ilmari", "Aino"], &["Heth", "Kallio", "Lindqvist", "Koskinen", "Hakkarainen", "Nieminen", "Vaananen", "Lehtonen"]),
        2 => (&["Kael", "Vesper", "Corin", "Bren", "Talia", "Jarek", "Mira", "Rhea"], &["Mercer", "Stark", "Kruger", "Holloway", "Vance", "Blackwood", "Keller", "Cross"]),
        11 => (&["Kenji", "Akira", "Sora", "Yuki", "Ren", "Haruki", "Kaede", "Mei"], &["Takahashi", "Watanabe", "Kobayashi", "Ito", "Saito", "Yamamoto", "Nakamura", "Kato"]),
        // Minmatar: Sebiestor (3), Brutor (4), Vherokior (14)
        3 => (&["Toran", "Sari", "Nesta", "Valen", "Eirik", "Freja", "Sigurd", "Astrid"], &["Kouris", "Solari", "Nilsen", "Solberg", "Hagen", "Bakke", "Lund", "Strand"]),
        4 => (&["Karishal", "Malaetu", "Krag", "Vuk", "Dragan", "Zoran", "Milena", "Tara"], &["Shakor", "Saraki", "Novak", "Kovac", "Horvat", "Babic", "Jovanovic", "Vukic"]),
        14 => (&["Temujin", "Altai", "Batu", "Chotan", "Sarnai", "Zaya", "Nergui", "Oyun"], &["Khasar", "Borjigin", "Chonos", "Olkhunut", "Jalayir", "Taidjut", "Merkit", "Tatar"]),
        // Amarr: Amarr (5), Ni-Kunni (6), Khanid (13)
        5 => (&["Jamyl", "Catiz", "Aritcio", "Heideran", "Uriam", "Doriam", "Maximilian", "Aurelia"], &["Sarum", "Tash-Murkon", "Kor-Azor", "Kador", "Ardishapur", "Khanid", "Valate", "Dacella"]),
        6 => (&["Rashid", "Tariq", "Nadia", "Layla", "Zayd", "Faris", "Samira", "Yasmin"], &["Al-Maktoum", "Qadir", "Sarkis", "Mansoor", "Hakim", "Nasser", "Bakr", "Zaman"]),
        13 => (&["Garkeh", "Darius", "Cyrus", "Khosrow", "Shirin", "Roxana", "Bahram", "Azadeh"], &["Khanid", "Ravan", "Pahlavi", "Sasan", "Arshak", "Karen", "Mihran", "Suren"]),
        // Gallente: Gallente (7), Intaki (8), Jin-Mei (12)
        7 => (&["Jacus", "Souro", "Celes", "Laurent", "Camille", "Etienne", "Manon", "Luc"], &["Roden", "Foiritan", "Aguard", "Moreau", "Laurent", "Dubois", "Mercier", "Blanc"]),
        8 => (&["Vindhya", "Aarav", "Priya", "Kiran", "Dev", "Ananya", "Rohan", "Meera"], &["Isu", "Chatterjee", "Sen", "Bose", "Mukherjee", "Dasgupta", "Banerjee", "Roy"]),
        12 => (&["Liang", "Wei", "Chen", "Ming", "Xia", "Jian", "Hua", "Zhen"], &["Zhang", "Wang", "Liu", "Huang", "Zhao", "Wu", "Zhou", "Xu"]),
        _ => (&["Alexander", "Lyra", "Orion", "Selene", "Caelum", "Aethel", "Elena", "Vesper"], &["Vander", "Artemis", "Vandermeer", "O'Neill", "Solari", "Mercer", "Kallio", "Stark"]),
    }
}

/// Returns the maximum trainable/active skill level for an Alpha clone (`grade 1`) from `clonegrades.fsdbinary`.

pub fn handle_char_unbound(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("charUnboundMgr", "GetCharactersToSelect") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let chars = ctx.db.get_characters_for_user(user_id);
            let rows = ctx.db.with_snapshot(|snap| {
                chars
                    .into_iter()
                    .map(|c| {
                        let (ship_type_rep, ship_name_rep) = snap
                            .items
                            .get(&c.active_ship_id)
                            .map(|item| {
                                let name = snap
                                    .item_custom_names
                                    .get(&item.item_id)
                                    .cloned()
                                    .or_else(|| ctx.esi_state.get_custom_item_name(item.item_id))
                                    .or_else(|| ctx.sde.types.get(&item.type_id).map(|t| t.name.clone()))
                                    .map(PyRep::String)
                                    .unwrap_or(PyRep::None);
                                (PyRep::Integer(item.type_id as i64), name)
                            })
                            .unwrap_or((PyRep::None, PyRep::None));
                        vec![
                            PyRep::Integer(c.character_id),
                            PyRep::String(c.name),
                            PyRep::Integer(c.corporation_id),
                            c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            c.station_id
                                .or(c.structure_id)
                                .map(PyRep::Integer)
                                .unwrap_or(PyRep::None),
                            PyRep::Integer(c.solar_system_id as i64),
                            PyRep::Integer(c.constellation_id as i64),
                            PyRep::Integer(c.region_id as i64),
                            PyRep::Real(c.balance.to_wire_f64()),
                            PyRep::Real(c.skill_points),
                            ship_type_rep,
                            ship_name_rep,
                            PyRep::Real(c.security_status),
                        ]
                    })
                    .collect()
            });

            Some(Ok(
                DBRowDescriptor::character_select_list_header().build_crowset(rows),
            ))
        }

        ("charUnboundMgr", "ValidateNameEx") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let valid = is_valid_character_name_syntax(name)
                && crate::services::lookup_svc::is_word_allowed(name)
                && ctx.db.with_snapshot(|s| !s.characters.values().any(|c| c.name.eq_ignore_ascii_case(name)));
            Some(Ok(PyRep::Bool(valid)))
        }

        ("charUnboundMgr", "GetValidRandomName") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let bloodline_id = if let Some(arg0) = args.first() {
                let Some(bid) = arg0.as_i64().filter(|&v| v > 0).map(|v| v as u32) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBloodline".into(),
                        message: "GetValidRandomName requires a valid positive bloodlineID".into(),
                        client_error: None,
                    }));
                };
                if !ctx.sde.bloodlines.contains_key(&bid) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBloodline".into(),
                        message: format!("Bloodline {bid} does not exist in SDE"),
                        client_error: None,
                    }));
                }
                bid
            } else {
                1
            };
            let (first_names, last_names) = bloodline_name_pools(bloodline_id);
            let seed = ctx.now_unix() ^ (session.session_id as u64) ^ ((bloodline_id as u64) << 16);
            let mut chosen = String::new();
            for i in 0..100 {
                let f_idx = ((seed + i) as usize) % first_names.len();
                let l_idx = (((seed / 7) + i * 3) as usize) % last_names.len();
                let candidate = format!("{} {}", first_names[f_idx], last_names[l_idx]);
                let exists = ctx.db.with_snapshot(|s| {
                    s.characters
                        .values()
                        .any(|c| c.name.eq_ignore_ascii_case(&candidate))
                });
                if !exists && is_valid_character_name_syntax(&candidate) {
                    chosen = candidate;
                    break;
                }
            }
            if chosen.is_empty() {
                chosen = format!("{} {}", first_names[0], ctx.now_unix() % 1_000_000);
            }
            Some(Ok(PyRep::String(chosen)))
        }

        ("charUnboundMgr", "CreateCharacterWithDoll") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(name) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCharacterName".into(),
                    message: "CreateCharacterWithDoll requires charactername string".into(),
                    client_error: None,
                }));
            };
            if !is_valid_character_name_syntax(name) || !crate::services::lookup_svc::is_word_allowed(name) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCharacterName".into(),
                    message: format!("Character name '{name}' violates EVE naming rules"),
                    client_error: None,
                }));
            }

            // V24.01 ccSvc.CreateCharacterWithDoll passes 10 args:
            // (charactername, raceID, bloodlineID, genderID, ancestryID, charInfo, portraitInfo, schoolID, None, qa_starter_system_id)
            // Also support legacy 7-arg callers without raceID at args[1].
            if args.len() < 7 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires (charactername, [raceID,] bloodlineID, genderID, ancestryID, charInfo, portraitInfo, schoolID)".into(),
                    client_error: None,
                }));
            }
            let has_race_arg = !is_valid_doll_or_portrait_payload(args.get(4).unwrap_or(&PyRep::None))
                && is_valid_doll_or_portrait_payload(args.get(5).unwrap_or(&PyRep::None));
            let offset = usize::from(has_race_arg);
            let Some(bloodline_id) = args.get(1 + offset).and_then(|a| a.as_i64()).map(|v| v as u32) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer bloodlineID".into(),
                    client_error: None,
                }));
            };
            let Some(gid) = args.get(2 + offset).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer genderID (0 or 1)".into(),
                    client_error: None,
                }));
            };
            if !(0..=1).contains(&gid) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidGender".into(),
                    message: format!("Invalid genderID {gid}; expected 0 (female) or 1 (male)"),
                    client_error: None,
                }));
            }
            let gender_id = gid as u32;
            let Some(ancestry_id) = args.get(3 + offset).and_then(|a| a.as_i64()).map(|v| v as u32) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer ancestryID".into(),
                    client_error: None,
                }));
            };
            let Some(char_info_rep) = args.get(4 + offset) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "Missing charInfo doll appearance argument".into(),
                    client_error: None,
                }));
            };
            let Some(portrait_info_rep) = args.get(5 + offset) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "Missing portraitInfo argument".into(),
                    client_error: None,
                }));
            };
            if !is_valid_doll_or_portrait_payload(char_info_rep)
                || !is_valid_doll_or_portrait_payload(portrait_info_rep)
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "charInfo and portraitInfo must be non-empty structured appearance descriptors (KeyVal or Dict)".into(),
                    client_error: None,
                }));
            }
            let Some(school_id) = args.get(6 + offset).and_then(|a| a.as_i64()).map(|v| v as u32) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer schoolID".into(),
                    client_error: None,
                }));
            };
            let Some(race_id) = ctx.sde.bloodlines.get(&bloodline_id).map(|b| b.race_id) else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterCreationFailed".into(),
                    message: format!("Unknown bloodline_id {bloodline_id}"),
                    client_error: None,
                }));
            };
            let doll_str = serialize_doll_pyrep(char_info_rep);
            let portrait_str = serialize_portrait_pyrep(portrait_info_rep);
            let is_qa_or_gm = ServerContext::has_gm_role_mask(
                session.role,
                ServerContext::ROLE_GM_QA_MASK,
            );
            let qa_starter_system_id = if is_qa_or_gm {
                args.get(8 + offset)
                    .and_then(|a| a.as_i64())
                    .map(|v| v as u32)
                    .filter(|&sys_id| {
                        ctx.sde
                            .solar_systems
                            .get(&sys_id)
                            .is_some_and(|sys| !sys.stations.is_empty())
                    })
            } else {
                None
            };

            let res = ctx.db.create_character_with_system(
                user_id,
                name,
                race_id,
                bloodline_id,
                ancestry_id,
                school_id,
                &ctx.sde,
                qa_starter_system_id,
            );
            match res {
                Ok(ch) => {
                    let now_ft = ctx.now_blueos_filetime_ticks();
                    let now_unix = ctx.now_unix();
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(c_mut) = snap.characters.get_mut(&ch.character_id) {
                            c_mut.gender = gender_id;
                            c_mut.create_date_unix = now_unix;
                        }
                        snap.character_doll_info
                            .insert(ch.character_id, (doll_str, portrait_str));
                        crate::services::operations_manager::mark_new_character_tutorial_pending(
                            snap,
                            ch.character_id,
                            now_unix,
                        );
                        if let Some(smap) = snap.skills.get(&ch.character_id).cloned() {
                            let hist = snap.skill_history.entry(ch.character_id).or_default();
                            let mut sorted: Vec<_> = smap.values().collect();
                            sorted.sort_by_key(|s| s.type_id);
                            for s in sorted {
                                hist.push(SkillHistoryEntry {
                                    log_date_ft: now_ft,
                                    event_type_id: 33, // 33 = SkillEventCharCreation
                                    skill_type_id: s.type_id,
                                    skill_level: s.level,
                                    absolute_points: s.skill_points,
                                });
                            }
                        }
                        if crate::starter_characters::should_grant_default_starter_plex() {
                            let starter_plex_balance = ctx.feature_flags_snapshot().starter_plex_balance;
                            crate::starter_characters::grant_starter_plex_in_snapshot(
                                snap, user_id, ch.character_id, starter_plex_balance, now_unix,
                            );
                        }
                    });
                    Some(Ok(PyRep::Integer(ch.character_id)))
                }
                Err(e) => Some(Err(BridgeRpcError { code: "CharacterCreationFailed".into(), message: e.to_string(), client_error: None })),
            }
        }

        ("charUnboundMgr", "SelectCharacterID") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(char_id) = args.first().and_then(|a| a.as_i64()).or_else(|| _kwargs.get("charID").and_then(|a| a.as_i64())) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "SelectCharacterID requires integer characterID".into(),
                    client_error: None,
                }));
            };

            let mut c = match ctx.db.select_character_for_user(user_id, char_id) {
                Ok(ch) => ch,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "CharacterSelectionDenied".into(),
                        message: e.to_string(),
                        client_error: None,
                    }))
                }
            };

            let prev_cid = session.character_id.filter(|&id| id != c.character_id);
            let was_online = session.character_id == Some(c.character_id)
                || crate::services::character_contacts::is_character_online(ctx, c.character_id);
            let mut holds_online_lock = false;
            if let Some(os) = ctx.db.get_online_session(user_id) {
                if os.session_id != session.session_id {
                    clear_session_selection_state(session);
                    if os.character_id == Some(c.character_id) {
                        return Some(Err(BridgeRpcError {
                            code: "CharacterAlreadyOnline".into(),
                            message: format!(
                                "Character {} is already online on another active session",
                                c.character_id
                            ),
                            client_error: None,
                        }));
                    }
                    return Some(Err(crate::router::binding::session_kicked_error(
                        "Session displaced before character selection completed",
                    )));
                }
                holds_online_lock = true;
            }
            if let Some((code, msg)) =
                ctx.db
                    .check_session_revoked(user_id, session.session_id, ctx.now_unix())
            {
                clear_session_selection_state(session);
                return Some(Err(if code == "MultiLoginBlocked" {
                    crate::router::binding::session_kicked_error(msg)
                } else {
                    crate::router::binding::session_revoked_error(code, msg)
                }));
            }
            if let Some(prev_cid) = prev_cid {
                // Switch the OnlineSession character binding first so concurrent cluster ticks
                // observe `prev_cid` as offline once `logoff_character_sync` records its ship hold.
                if holds_online_lock
                    && ctx
                        .db
                        .acquire_online_lock(
                            user_id,
                            session.session_id,
                            None,
                            false,
                            ctx.now_unix(),
                        )
                        .is_err()
                {
                    clear_session_selection_state(session);
                    return Some(Err(crate::router::binding::session_kicked_error(
                        "Session displaced before character switch completed",
                    )));
                }
                ctx.logoff_character_sync(prev_cid, Some(session));
                clear_session_selection_state(session);
            }
            // Hold `reconnect_hold_sessions[char_id]` across `acquire_online_lock(Some(char_id))`
            // and `reconnect_character_in_space_sync` so claiming the online lock before
            // reconnect blocks concurrent displaced-session cleanup from emergency-warping the ship
            // without letting `on_advance_secs(0)` prune a still-active disconnect hold.
            let reconnect_guard = crate::router::binding::ReconnectHoldGuard::acquire(
                ctx,
                c.character_id,
                session.session_id,
            );
            if holds_online_lock
                && ctx
                    .db
                    .acquire_online_lock(
                        user_id,
                        session.session_id,
                        Some(c.character_id),
                        false,
                        ctx.now_unix(),
                    )
                    .is_err()
            {
                drop(reconnect_guard);
                clear_session_selection_state(session);
                return Some(Err(displaced_selection_error(ctx, user_id, session.session_id)));
            }
            ctx.clear_pending_displaced_logoffs_for_char(user_id, c.character_id);
            ctx.release_session_bindings(session.session_id);
            ctx.clear_notifications_for_char(c.character_id);
            crate::router::cache_notifications::set_active_dispatch_char(Some(c.character_id));
            let eff_sys = crate::services::pve_special_sites::reconcile_abyssal_login(
                ctx, c.character_id, c.solar_system_id, c.active_ship_id,
            );
            crate::services::structure_control_ops::release_all_structure_control_for_character(
                ctx, None, c.character_id,
            );
            c = ctx.db.get_character(c.character_id).unwrap_or(c);
            let (resolved_bloodline, resolved_race) =
                crate::router::session_transition::resolve_character_bloodline_and_race(
                    &c,
                    Some(&ctx.sde),
                );
            if c.bloodline_id != resolved_bloodline || c.race_id != resolved_race {
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(ch_mut) = snap.characters.get_mut(&c.character_id) {
                        ch_mut.bloodline_id = resolved_bloodline;
                        ch_mut.race_id = resolved_race;
                        ch_mut.version += 1;
                    }
                });
                c.bloodline_id = resolved_bloodline;
                c.race_id = resolved_race;
            }
            let now_unix = ctx.now_unix();
            let pre_reconnect_sys = c.solar_system_id;
            if c.station_id.is_none() && c.structure_id.is_none() {
                let _ = crate::sp6_lifecycle::reconnect_character_in_space_sync(ctx, c.character_id);
                drop(reconnect_guard);
                if let Ok(reloaded) = ctx.db.get_character(c.character_id) {
                    c = reloaded;
                }
            } else {
                drop(reconnect_guard);
            }
            let eff_sys = if c.solar_system_id != pre_reconnect_sys && c.solar_system_id != 0 {
                c.solar_system_id
            } else {
                eff_sys
            };
            ctx.record_prelogoff_hold_session(c.character_id, None);
            let displaced_during_reconnect = ctx
                .db
                .check_session_revoked(user_id, session.session_id, ctx.now_unix())
                .is_some()
                || match ctx.db.get_online_session(user_id) {
                    Some(os) => {
                        os.session_id != session.session_id
                            || (holds_online_lock && os.character_id != Some(c.character_id))
                    }
                    None => holds_online_lock,
                };
            if displaced_during_reconnect {
                return Some(Err(finalize_displaced_character_selection(
                    ctx,
                    session,
                    user_id,
                    c.character_id,
                )));
            }

            if !eve_db::is_npc_corporation(&ctx.sde, c.corporation_id) {
                crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(
                    ctx,
                    c.corporation_id,
                );
            }
            crate::services::fleet_social::fleet_social_persistence::clear_forced_session_syncs(
                ctx,
                c.character_id,
            );
            let _ = crate::services::fw_enlistment::take_forced_war_faction_sync(
                ctx,
                c.character_id,
            );
            let actor = ctx.db.with_snapshot(|snap| {
                eve_db::CorpActor::resolve(snap, &ctx.sde, c.character_id, now_unix)
            });
            session.character_id = Some(c.character_id);
            session.corporation_id = (actor.corp_id > 0).then_some(actor.corp_id);
            session.alliance_id = actor.alliance_id;
            session.war_faction_id = c.war_faction_id;
            crate::router::apply_session_from_character(session, &c, None, Some(&ctx.sde));
            session.solar_system_id = Some(eff_sys);
            session.corp_role = actor.effective_roles;
            if c.station_id.is_none() && c.structure_id.is_none() {
                let _ = crate::services::serverops_cluster::emit_sim_clock_rebase_on_entry_if_needed(
                    ctx,
                    c.character_id,
                    eff_sys,
                );
            }

            crate::quasar_gateway::isolate_side_effect(
                "character_gm_tools.SelectCharacterID.post_select_hooks",
                || {
                    ctx.sync_character_local_chat(c.character_id, None, eff_sys);
                    crate::services::fleet_social::fleet_social_notify::on_character_select(ctx, session, c.character_id, was_online);
                    let _ = crate::services::fleet_social::mint_or_rotate_xmpp_auth_token(ctx, c.character_id);
                    catch_up_skill_queue(ctx, c.character_id);
                    notify_watchlist_logon(ctx, c.character_id);
                    crate::services::space_destiny::record_session_change_timer(
                        ctx, c.character_id, now_unix, eff_sys, 0, "login",
                    );
                },
            );

            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                ("shipID", PyRep::Integer(c.active_ship_id)),
                ("solarSystemID", PyRep::Integer(eff_sys as i64)),
                ("sessionChangeReason", PyRep::String("charsel".into())),
            ])))
        }

        _ => None,
    }
}

pub(crate) use crate::services::character_gm_helpers::{
    execute_gm_heal, is_me_or_myship, parse_slash_tokens, remove_npc_runtime_state,
    resolve_gm_char_prefix, resolve_item_or_char_active_ship,
};

/// Rolls back partially spawned `/spawn` DB, runtime NPC, and ballpark records when an actor spawn fails.
async fn rollback_spawned_entities(
    ctx: &ServerContext,
    handle: &crate::system_actor::SystemHandle,
    sys_id: u32,
    failed_id: i64,
    prior_ids: &[i64],
) {
    ctx.db.with_mut_snapshot(|snap| {
        for &id in std::iter::once(&failed_id).chain(prior_ids.iter()) {
            snap.items.remove(&id);
            snap.asteroids.remove(&id);
        }
    });
    for &id in std::iter::once(&failed_id).chain(prior_ids.iter()) {
        remove_npc_runtime_state(ctx, sys_id, id);
    }
    for &id in prior_ids {
        let _ = handle.remove_ball(id).await;
    }
}

pub async fn handle_gm_slash_ext(
    ctx: &ServerContext,
    session: &mut SessionState,
    cid: i64,
    cmd: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if cmd == "/heal" || cmd.starts_with("/heal ") {
        return Some(execute_gm_heal(ctx, session, cid, cmd).await);
    }

    if let Some(rest) = cmd.strip_prefix("/spawn ") {
        let tokens = parse_slash_tokens(rest);
        let (target_cid, arg_tokens) = resolve_gm_char_prefix(ctx, cid, &tokens, 1);
        let Some(type_id) = arg_tokens.first().and_then(|s| s.parse::<u32>().ok()) else {
            return Some(Err(BridgeRpcError { code: "InvalidSlashArgument".into(), message: "Usage: /spawn [char_id] <type_id> [quantity]".into(), client_error: None }));
        };
        let qty = arg_tokens.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(1);
        if !(1..=100).contains(&qty) {
            return Some(Err(BridgeRpcError { code: "InvalidSlashArgument".into(), message: "Quantity for /spawn must be between 1 and 100".into(), client_error: None }));
        }
        let Some(inv_type) = ctx.sde.types.get(&type_id).cloned() else {
            return Some(Err(BridgeRpcError { code: "TypeNotFound".into(), message: format!("Type ID {type_id} not found in SDE"), client_error: None }));
        };
        let (db_sys_id, db_ship_id) = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&target_cid).map(|c| (c.solar_system_id, c.active_ship_id)))
            .unwrap_or((0, 0));
        let sys_id = if target_cid != cid {
            db_sys_id
        } else {
            session.solar_system_id.filter(|&s| s > 0).unwrap_or(db_sys_id)
        };
        let ship_id = if target_cid != cid {
            db_ship_id
        } else {
            session.ship_id.filter(|&id| id > 0).unwrap_or(db_ship_id)
        };

        let handle = ctx.systems.get_or_spawn(sys_id);
        let origin = if ship_id > 0 {
            handle.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(DVec3::ZERO)
        } else {
            DVec3::ZERO
        };

        let mut spawned_ids = Vec::with_capacity(qty as usize);
        for idx in 0..qty {
            let pos = origin + DVec3::new(1_500.0 + (idx as f64) * 250.0, 0.0, 0.0);
            let ball_radius = if inv_type.category_id == 25 {
                inv_type.radius.max(200.0)
            } else {
                inv_type.radius.max(1.0)
            };
            let is_celestial_or_asteroid = matches!(inv_type.category_id, 2 | 25);
            // Asteroids/celestials belong to ownerSystem (1) with no player corp/char so bracket/standing UI stays neutral.
            let spawn_owner_id = if is_celestial_or_asteroid { 1 } else { target_cid };
            let entity_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.items.insert(
                    id,
                    InventoryItemRecord {
                        item_id: id,
                        type_id,
                        owner_id: spawn_owner_id,
                        location_id: sys_id as i64,
                        flag: InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                if inv_type.category_id == 25 {
                    snap.asteroids.insert(
                        id,
                        eve_db::AsteroidRecord {
                            asteroid_id: id,
                            solar_system_id: sys_id,
                            belt_id: 0,
                            type_id,
                            quantity: 200_000,
                            initial_quantity: 200_000,
                            position: (pos.x, pos.y, pos.z),
                            radius: ball_radius,
                        },
                    );
                }
                id
            });
            let spawn_res = if inv_type.category_id == 25 {
                handle
                    .spawn_asteroid(
                        crate::system_actor::AsteroidState {
                            asteroid_id: entity_id,
                            belt_id: 0,
                            type_id,
                            initial_units: 200_000,
                            remaining_units: 200_000,
                            unit_volume: inv_type.volume.max(0.01),
                            depleted: false,
                            respawn_at_tick: None,
                        },
                        Ball {
                            id: entity_id,
                            type_id,
                            position: pos,
                            velocity: DVec3::ZERO,
                            max_velocity: 0.0,
                            inertia: 0.0,
                            mass: inv_type.mass.max(1.0),
                            radius: ball_radius,
                            speed_fraction: 0.0,
                            mode: MovementMode::Rigid,
                        },
                    )
                    .await
                    .map(|_| ())
            } else if inv_type.category_id == 2 {
                let ball = Ball {
                    id: entity_id,
                    type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: inv_type.mass.max(1.0),
                    radius: ball_radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                };
                let combat = ctx.build_type_combat_state(type_id);
                let meta = eve_destiny::SlimItemMeta {
                    group_id: inv_type.group_id,
                    category_id: inv_type.category_id,
                    owner_id: 1,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    name: (!inv_type.name.is_empty()).then(|| inv_type.name.clone()),
                    ..Default::default()
                };
                let res = handle.spawn_ball_with_meta(ball, combat, meta).await;
                if res.is_ok() {
                    if let Ok(mut bp) = handle.ballpark.write() {
                        bp.static_ball_ids.insert(entity_id);
                    }
                }
                res.map(|_| ())
            } else if inv_type.category_id == 11 {
                let archetype =
                    crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(
                        ctx, type_id,
                    );
                let (ball, combat, entity) =
                    crate::services::wormhole_pve_seeding::build_specialized_npc_components(
                        ctx,
                        sys_id,
                        entity_id,
                        type_id,
                        archetype,
                        0,
                        pos,
                    );
                let fac_owner = if entity.faction_id > 0 {
                    i64::from(entity.faction_id)
                } else {
                    500_010
                };
                let meta = eve_destiny::SlimItemMeta {
                    group_id: inv_type.group_id,
                    category_id: 11,
                    owner_id: fac_owner,
                    char_id: None,
                    corp_id: crate::services::combat_crimewatch::combat_npc_ai::resolve_npc_corp_id_for_faction(
                        ctx,
                        fac_owner,
                    ),
                    alliance_id: -1,
                    security_status: -1.0,
                    is_cloaked: false,
                    name: (!inv_type.name.is_empty()).then(|| inv_type.name.clone()),
                    ..Default::default()
                };
                let res = handle.spawn_ball_with_meta(ball, combat, meta).await;
                if res.is_ok() {
                    let legacy_ent = crate::combat::npc_ai::NpcCombatEntity {
                        ball_id: entity_id,
                        type_id,
                        faction_id: entity.faction_id,
                        state: crate::combat::npc_ai::NpcAiState::Idle,
                        spawn_group_id: 0,
                        spawn_pos: pos,
                        target_id: None,
                        bounty_isk: entity.bounty_isk,
                        optimal_range: entity.optimal_range,
                        orbit_range: entity.orbit_range,
                        base_dps: entity.damage_vector.total(),
                        damage_vector: entity.damage_vector,
                        last_attack_tick: 0,
                        has_shield_rep: entity.remote_shield_rep > 0.0,
                        has_armor_rep: entity.remote_armor_rep > 0.0,
                    };
                    if let Ok(mut map) = ctx.npc_spawns.write() {
                        map.entry(sys_id)
                            .or_insert_with(crate::combat::npc_ai::NpcSpawnManager::new)
                            .entities
                            .insert(entity_id, legacy_ent);
                    }
                    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                        rt.npc_entities.insert(entity_id, entity);
                    });
                }
                res.map(|_| ())
            } else {
                let ball = Ball {
                    id: entity_id,
                    type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: inv_type.mass.max(1.0),
                    radius: ball_radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                };
                let combat = ctx.build_type_combat_state(type_id);
                let mut meta = ctx.build_ship_slim_meta(target_cid, type_id);
                meta.char_id = None;
                let res = handle.spawn_ball_with_meta(ball, combat, meta).await;
                if res.is_ok() && inv_type.category_id == 6 {
                    // Record `isInteractive = false` on the ballpark for unpiloted spawned ship hulls
                    // so subsequent `SetState` / `AddBalls2` / `SetBallInteractive` frames keep
                    // `ball.isInteractive == False` until boarded (`#983`).
                    let _ = handle
                        .emit_destiny_event(
                            entity_id,
                            eve_destiny::DestinyEvent::SetBallInteractive {
                                ball_id: entity_id,
                                is_interactive: false,
                            },
                        )
                        .await;
                }
                res.map(|_| ())
            };
            if let Err(err) = spawn_res {
                rollback_spawned_entities(ctx, &handle, sys_id, entity_id, &spawned_ids).await;
                return Some(Err(BridgeRpcError {
                    code: "SpawnFailed".into(),
                    message: err,
                    client_error: None,
                }));
            }
            spawned_ids.push(entity_id);
        }

        let first_id = spawned_ids.first().copied().unwrap_or(0);
        let id_reps: Vec<PyRep> = spawned_ids.into_iter().map(PyRep::Integer).collect();
        return Some(Ok(PyRep::keyval([
            ("command", PyRep::String(cmd.to_string())),
            ("spawnedID", PyRep::Integer(first_id)),
            ("itemID", PyRep::Integer(first_id)),
            ("spawnedIDs", PyRep::List(id_reps)),
            ("typeID", PyRep::Integer(type_id as i64)),
            ("quantity", PyRep::Integer(qty as i64)),
            ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ])));
    }

    if let Some(rest) = cmd.strip_prefix("/unspawn ").or_else(|| cmd.strip_prefix("/despawn ")) {
        let tokens = parse_slash_tokens(rest);
        let Some(target_id) = tokens.first().and_then(|s| s.parse::<i64>().ok()).filter(|&id| id > 0) else {
            return Some(Err(BridgeRpcError { code: "InvalidSlashArgument".into(), message: "Usage: /unspawn <entity_id>".into(), client_error: None }));
        };
        let (sys_id, removed_db) = ctx.db.with_mut_snapshot(|snap| {
            let caller_sys = session.solar_system_id.or_else(|| snap.characters.get(&cid).map(|c| c.solar_system_id)).unwrap_or(0);
            let removed = snap.items.remove(&target_id).is_some()
                || snap.asteroids.remove(&target_id).is_some();
            snap.asteroids.remove(&target_id);
            let children: Vec<i64> = snap.items.values().filter(|i| i.location_id == target_id).map(|i| i.item_id).collect();
            for child_id in children { snap.items.remove(&child_id); }
            (caller_sys, removed)
        });
        let handle = ctx.systems.get_or_spawn(sys_id);
        let removed_ball = handle.remove_ball(target_id).await.is_some();
        remove_npc_runtime_state(ctx, sys_id, target_id);
        if removed_ball {
            crate::services::pve_incursion_sites::mark_incursion_boss_destroyed(ctx, sys_id, target_id);
            crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, sys_id, target_id).await;
        }
        if !removed_db && !removed_ball {
            return Some(Err(BridgeRpcError { code: "EntityNotFound".into(), message: format!("Entity {target_id} not found in space or inventory"), client_error: None }));
        }
        return Some(Ok(PyRep::keyval([
            ("command", PyRep::String(cmd.to_string())),
            ("unspawnedID", PyRep::Integer(target_id)),
            ("removedFromSpace", PyRep::Bool(removed_ball)),
            ("removedFromDb", PyRep::Bool(removed_db)),
        ])));
    }

    if let Some(rest) = cmd.strip_prefix("/setattr ") {
        let tokens = parse_slash_tokens(rest);
        let Some(item_tok) = tokens.first().map(String::as_str) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /setattr <item_id|me> <attribute_id> <value>".into(),
                client_error: None,
            }));
        };
        let Some(attr_id) = tokens.get(1).and_then(|s| s.parse::<u16>().ok()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /setattr <item_id|me> <attribute_id> <value>".into(),
                client_error: None,
            }));
        };
        let Some(val) = tokens
            .get(2)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite())
        else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Attribute value for /setattr must be a finite number".into(),
                client_error: None,
            }));
        };

        let now_ft = ctx.now_blueos_filetime_ticks();
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(item_id) = resolve_item_or_char_active_ship(snap, cid, item_tok) else {
                return Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: format!("Invalid item_id '{item_tok}' for /setattr"),
                    client_error: None,
                });
            };
            let Some(item) = snap.items.get_mut(&item_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {item_id} not found"),
                    client_error: None,
                });
            };
            let default_val = ctx
                .sde
                .types
                .get(&item.type_id)
                .and_then(|t| t.default_attributes.get(&attr_id).copied())
                .unwrap_or(0.0);
            let old_val = item
                .dynamic_attributes
                .insert(attr_id, val)
                .unwrap_or(default_val);
            item.version += 1;
            Ok((item_id, item.owner_id, item.location_id, old_val))
        });

        return match res {
            Ok((item_id, owner_id, loc_id, old_val)) => {
                ctx.invalidate_and_push_fit(item_id);
                ctx.invalidate_and_push_fit(loc_id);
                crate::services::notify_emitters_character_services::emit_setattr_notifications(
                    ctx, owner_id, loc_id, item_id, attr_id, now_ft, val, old_val,
                );
                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("itemID", PyRep::Integer(item_id)),
                    ("attributeID", PyRep::Integer(attr_id as i64)),
                    ("oldValue", PyRep::Real(old_val)),
                    ("newValue", PyRep::Real(val)),
                    ("value", PyRep::Real(val)),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }

    if let Some(rest) = cmd.strip_prefix("/getattr ") {
        let tokens = parse_slash_tokens(rest);
        let Some(item_tok) = tokens.first().map(String::as_str) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /getattr <item_id|me> <attribute_id>".into(),
                client_error: None,
            }));
        };
        let Some(attr_id) = tokens.get(1).and_then(|s| s.parse::<u16>().ok()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /getattr <item_id|me> <attribute_id>".into(),
                client_error: None,
            }));
        };
        let res = ctx.db.with_snapshot(|snap| {
            let item_id = resolve_item_or_char_active_ship(snap, cid, item_tok).unwrap_or(0);
            let item = snap.items.get(&item_id)?;
            let val = item
                .dynamic_attributes
                .get(&attr_id)
                .copied()
                .or_else(|| {
                    ctx.sde
                        .types
                        .get(&item.type_id)
                        .and_then(|t| t.default_attributes.get(&attr_id).copied())
                })
                .unwrap_or(0.0);
            Some((item_id, item.type_id, val))
        });
        let Some((item_id, type_id, val)) = res else {
            return Some(Err(BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Item '{item_tok}' not found for /getattr"),
                client_error: None,
            }));
        };
        return Some(Ok(PyRep::keyval([
            ("command", PyRep::String(cmd.to_string())),
            ("itemID", PyRep::Integer(item_id)),
            ("typeID", PyRep::Integer(type_id as i64)),
            ("attributeID", PyRep::Integer(attr_id as i64)),
            ("value", PyRep::Real(val)),
        ])));
    }

    if let Some(rest) = cmd.strip_prefix("/fit ") {
        let tokens = parse_slash_tokens(rest);
        if tokens.is_empty() {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /fit [ship_id|me] <type_id> [flag_id]".into(),
                client_error: None,
            }));
        }
        let caller_ship_id = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&cid).map(|c| c.active_ship_id))
            .or(session.ship_id)
            .unwrap_or(0);

        let (ship_id, type_id, explicit_flag) = if is_me_or_myship(&tokens[0]) {
            let Some(tid) = tokens.get(1).and_then(|s| s.parse::<u32>().ok()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: "Usage: /fit me <type_id> [flag_id]".into(),
                    client_error: None,
                }));
            };
            let flg = tokens
                .get(2)
                .and_then(|s| s.parse::<u16>().ok())
                .map(InventoryFlag::from_u16);
            (caller_ship_id, tid, flg)
        } else if tokens.len() >= 2 {
            let first_i64 = tokens[0].parse::<i64>().unwrap_or(0);
            let matched_target_ship = ctx.db.with_snapshot(|snap| {
                if snap
                    .items
                    .get(&first_i64)
                    .and_then(|i| ctx.sde.types.get(&i.type_id))
                    .is_some_and(|t| matches!(t.category_id, 6 | 65))
                {
                    Some(first_i64)
                } else {
                    snap.characters.get(&first_i64).map(|c| c.active_ship_id)
                }
            });
            if let Some(target_ship_id) = matched_target_ship {
                let Some(tid) = tokens.get(1).and_then(|s| s.parse::<u32>().ok()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /fit <ship_id|char_id> <type_id> [flag_id]".into(),
                        client_error: None,
                    }));
                };
                let flg = tokens
                    .get(2)
                    .and_then(|s| s.parse::<u16>().ok())
                    .map(InventoryFlag::from_u16);
                (target_ship_id, tid, flg)
            } else {
                let Some(tid) = tokens[0].parse::<u32>().ok() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /fit <type_id> [flag_id]".into(),
                        client_error: None,
                    }));
                };
                let flg = tokens
                    .get(1)
                    .and_then(|s| s.parse::<u16>().ok())
                    .map(InventoryFlag::from_u16);
                (caller_ship_id, tid, flg)
            }
        } else {
            let Some(tid) = tokens[0].parse::<u32>().ok() else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: "Usage: /fit <type_id> [flag_id]".into(),
                    client_error: None,
                }));
            };
            (caller_ship_id, tid, None)
        };

        let Some(mod_type) = ctx.sde.types.get(&type_id) else {
            return Some(Err(BridgeRpcError {
                code: "TypeNotFound".into(),
                message: format!("Module type {type_id} not found in SDE"),
                client_error: None,
            }));
        };
        let effects = &mod_type.effect_ids;

        let Some((ship_owner_id, ship_cat_id)) = ctx.db.with_snapshot(|snap| {
            snap.items.get(&ship_id).map(|ship_item| {
                let cat = ctx
                    .sde
                    .types
                    .get(&ship_item.type_id)
                    .map(|t| t.category_id)
                    .unwrap_or(6);
                (ship_item.owner_id, cat)
            })
        }) else {
            return Some(Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: format!("Target ship {ship_id} not found"),
                client_error: None,
            }));
        };

        let pre_fit_opt = if ship_cat_id == 6 {
            ctx.fit(ship_owner_id, ship_id).ok()
        } else {
            None
        };
        let before_ship_attrs = pre_fit_opt.as_ref().map(|f| {
            crate::services::notify_emitters_skills_trade_godma::snapshot_ship_fitting_attrs(ctx, f)
        });

        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(ship_item) = snap.items.get(&ship_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Target ship {ship_id} not found"),
                    client_error: None,
                });
            };
            let occupied_flags: std::collections::HashSet<InventoryFlag> = snap
                .items
                .values()
                .filter(|i| i.location_id == ship_id)
                .map(|i| i.flag)
                .collect();

            let candidate_flags: Vec<InventoryFlag> = if effects.contains(&12) {
                vec![
                    InventoryFlag::HiSlot0,
                    InventoryFlag::HiSlot1,
                    InventoryFlag::HiSlot2,
                    InventoryFlag::HiSlot3,
                    InventoryFlag::HiSlot4,
                    InventoryFlag::HiSlot5,
                    InventoryFlag::HiSlot6,
                    InventoryFlag::HiSlot7,
                ]
            } else if effects.contains(&13) {
                vec![
                    InventoryFlag::MedSlot0,
                    InventoryFlag::MedSlot1,
                    InventoryFlag::MedSlot2,
                    InventoryFlag::MedSlot3,
                    InventoryFlag::MedSlot4,
                    InventoryFlag::MedSlot5,
                    InventoryFlag::MedSlot6,
                    InventoryFlag::MedSlot7,
                ]
            } else if effects.contains(&11) {
                vec![
                    InventoryFlag::LoSlot0,
                    InventoryFlag::LoSlot1,
                    InventoryFlag::LoSlot2,
                    InventoryFlag::LoSlot3,
                    InventoryFlag::LoSlot4,
                    InventoryFlag::LoSlot5,
                    InventoryFlag::LoSlot6,
                    InventoryFlag::LoSlot7,
                ]
            } else if effects.contains(&2663) {
                vec![
                    InventoryFlag::RigSlot0,
                    InventoryFlag::RigSlot1,
                    InventoryFlag::RigSlot2,
                ]
            } else if mod_type.category_id == 32 || effects.contains(&3772) {
                if let Some(&req_slot_f) = mod_type.default_attributes.get(&1366) {
                    vec![InventoryFlag::from_u16(req_slot_f as u16)]
                } else {
                    vec![
                        InventoryFlag::SubSystemSlot0,
                        InventoryFlag::SubSystemSlot1,
                        InventoryFlag::SubSystemSlot2,
                        InventoryFlag::SubSystemSlot3,
                        InventoryFlag::SubSystemSlot4,
                    ]
                }
            } else if mod_type.category_id == 18 {
                vec![InventoryFlag::DroneBay]
            } else {
                vec![
                    InventoryFlag::HiSlot0,
                    InventoryFlag::HiSlot1,
                    InventoryFlag::MedSlot0,
                    InventoryFlag::LoSlot0,
                    InventoryFlag::Cargo,
                ]
            };

            let has_slot_effect = effects.iter().any(|e| matches!(e, 11 | 12 | 13 | 2663 | 3772));
            let has_structure_slot_effect = has_slot_effect || effects.contains(&6306);
            let ship_group_id = ctx.sde.types.get(&ship_item.type_id).map(|t| t.group_id);
            let chosen_flag = if explicit_flag == Some(InventoryFlag::Cargo) {
                InventoryFlag::Cargo
            } else if mod_type.category_id == 18 {
                match explicit_flag {
                    None | Some(InventoryFlag::DroneBay) => InventoryFlag::DroneBay,
                    Some(other) => {
                        return Err(BridgeRpcError {
                            code: "FittingValidationFailed".into(),
                            message: format!(
                                "Drone type {type_id} cannot be fitted into slot flag {}",
                                other as u16
                            ),
                            client_error: None,
                        });
                    }
                }
            } else if ship_cat_id == 6 && mod_type.category_id == 8 {
                match explicit_flag {
                    Some(ef) if matches!(ef as u16, 11..=34) => ef,
                    _ => {
                        return Err(BridgeRpcError {
                            code: "FittingValidationFailed".into(),
                            message: format!(
                                "Charge type {type_id} ({}) requires an explicit module slot or Cargo flag",
                                mod_type.name
                            ),
                            client_error: None,
                        });
                    }
                }
            } else if ship_cat_id == 6 && matches!(mod_type.category_id, 7 | 32) && has_slot_effect
            {
                if mod_type.category_id == 32 && ship_group_id != Some(963) {
                    return Err(BridgeRpcError {
                        code: "FittingValidationFailed".into(),
                        message: format!(
                            "Subsystem type {type_id} ({}) can only be fitted to Strategic Cruisers",
                            mod_type.name
                        ),
                        client_error: None,
                    });
                }
                if let Some(ef) = explicit_flag {
                    let raw = ef as u16;
                    let slot_matches = match raw {
                        11..=18 => effects.contains(&11),
                        19..=26 => effects.contains(&13),
                        27..=34 => effects.contains(&12),
                        92..=94 => effects.contains(&2663),
                        125..=132 => mod_type.category_id == 32 || effects.contains(&3772),
                        _ => false,
                    };
                    if !slot_matches {
                        return Err(BridgeRpcError {
                            code: "FittingValidationFailed".into(),
                            message: format!(
                                "Type {type_id} ({}) is incompatible with slot flag {raw}",
                                mod_type.name
                            ),
                            client_error: None,
                        });
                    }
                    ef
                } else {
                    // Validate hull slot limits, subsystem slots, and turret/launcher hardpoints before creating the item.
                    let mut chosen: Option<InventoryFlag> = None;
                    let mut first_err: Option<String> = None;
                    for &cand in &candidate_flags {
                        if occupied_flags.contains(&cand) {
                            continue;
                        }
                        if let Some(ref base_fit) = pre_fit_opt {
                            let mut sim_fit = base_fit.clone();
                            match sim_fit.fit_module_with_state(
                                -(snap.next_entity_id + 1),
                                type_id,
                                cand as u16,
                                false,
                                &ctx.sde,
                            ) {
                                Ok(()) => {
                                    chosen = Some(cand);
                                    break;
                                }
                                Err(msg) => {
                                    if first_err.is_none() {
                                        first_err = Some(msg);
                                    }
                                }
                            }
                        } else {
                            chosen = Some(cand);
                            break;
                        }
                    }
                    let Some(valid_flag) = chosen else {
                        return Err(BridgeRpcError {
                            code: "FittingValidationFailed".into(),
                            message: first_err.unwrap_or_else(|| {
                                format!(
                                    "No free compatible fitting slot for module type {type_id} on ship {ship_id}"
                                )
                            }),
                            client_error: None,
                        });
                    };
                    valid_flag
                }
            } else if ship_cat_id == 65
                && mod_type.category_id == 66
                && has_structure_slot_effect
            {
                if let Some(ef) = explicit_flag {
                    if !matches!(ef as u16, 11..=34 | 92..=94 | 164..=171) {
                        return Err(BridgeRpcError {
                            code: "FittingValidationFailed".into(),
                            message: format!(
                                "Structure module type {type_id} is incompatible with slot flag {}",
                                ef as u16
                            ),
                            client_error: None,
                        });
                    }
                    ef
                } else if effects.contains(&6306) {
                    (164u16..=171)
                        .map(InventoryFlag::from_u16)
                        .find(|f| !occupied_flags.contains(f))
                        .unwrap_or_else(|| InventoryFlag::from_u16(164))
                } else {
                    candidate_flags
                        .iter()
                        .copied()
                        .find(|f| !occupied_flags.contains(f))
                        .unwrap_or(candidate_flags[0])
                }
            } else {
                return Err(BridgeRpcError {
                    code: "FittingValidationFailed".into(),
                    message: format!(
                        "Type {type_id} ({}) cannot be fitted onto target {ship_id}",
                        mod_type.name
                    ),
                    client_error: None,
                });
            };

            snap.next_entity_id += 1;
            let item_id = snap.next_entity_id;
            let rec = InventoryItemRecord {
                item_id,
                type_id,
                owner_id: ship_item.owner_id,
                location_id: ship_id,
                flag: chosen_flag,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            };
            snap.items.insert(item_id, rec.clone());
            snap.module_online_states.insert(item_id, false);
            Ok((rec, chosen_flag))
        });

        return match res {
            Ok((rec, chosen_flag)) => {
                ctx.invalidate_ship_fitting(ship_id);
                let flag_u16 = chosen_flag as u16;
                let is_fitting_slot = matches!(flag_u16, 11..=34 | 92..=94 | 125..=132);
                let mut is_online = false;
                if is_fitting_slot {
                    if let Ok(mut fit) = ctx.fit(rec.owner_id, ship_id) {
                        // Explicit-flag GM `/fit <ship> <type> <flag>` forces the chosen slot online
                        // when hull CPU/PG permits; auto-slot `/fit <type>` checks pilot skills (#648).
                        let online_ok = if explicit_flag.is_some() {
                            fit.set_module_online_skip_skills(rec.item_id, &ctx.sde).is_ok()
                        } else {
                            fit.set_module_online(rec.item_id, &ctx.sde).is_ok()
                        };
                        if fit.modules.contains_key(&rec.item_id) && online_ok {
                            is_online = true;
                        }
                    }
                    if is_online {
                        ctx.db.with_mut_snapshot(|snap| {
                            snap.module_online_states.insert(rec.item_id, true);
                        });
                    }
                }
                // ixLocationID (3) = 0, ixFlag (4) = 0 notifies `invCache`, `godma`, `dogmaLocation`,
                // and `SlotsContainer.OnInvChange` BEFORE `invalidate_and_push_fit` / `OnModuleFitted`
                // (#1252, parity with `inventory_fitting.rs`).
                crate::services::inventory_bound::emit_inv_item_change(
                    ctx,
                    rec.owner_id,
                    &rec,
                    vec![(3, PyRep::Integer(0)), (4, PyRep::Integer(0))],
                );
                ctx.emit_typed(
                    NotificationTarget::Character(rec.owner_id),
                    crate::router::notify_registry::ProcessInventoryChange {
                        arg0: PyRep::Integer(rec.item_id),
                        arg1: PyRep::Integer(rec.location_id),
                        arg2: PyRep::Integer(chosen_flag as i64),
                        arg3: None,
                    },
                );
                ctx.invalidate_and_push_fit(ship_id);
                if is_fitting_slot {
                    let group_id = ctx
                        .sde
                        .types
                        .get(&type_id)
                        .map(|t| i64::from(t.group_id))
                        .unwrap_or(0);
                    ctx.emit_typed(
                        NotificationTarget::Character(rec.owner_id),
                        OnModuleFitted {
                            char_id: rec.owner_id,
                            flag: chosen_flag as i64,
                            type_id: type_id as i64,
                            group_id,
                        },
                    );
                    let _ = ctx.notify_module_online_state(rec.owner_id, rec.item_id, is_online);
                    if is_online {
                        let eff_id = ctx.effect_id_for_module(type_id, Some("online")).unwrap_or(16) as i32;
                        ctx.notify_godma_ship_effect(
                            crate::router::NotificationTarget::Character(rec.owner_id),
                            rec.item_id,
                            eff_id,
                            true,
                            0,
                            0,
                        );
                    }
                    crate::services::notify_emitters_skills_trade_godma::emit_ship_fitting_attr_changes_for_ship(
                        ctx,
                        rec.owner_id,
                        ship_id,
                        before_ship_attrs.as_ref(),
                        true,
                    );
                }
                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("shipID", PyRep::Integer(ship_id)),
                    ("itemID", PyRep::Integer(rec.item_id)),
                    ("typeID", PyRep::Integer(type_id as i64)),
                    ("flagID", PyRep::Integer(chosen_flag as i64)),
                    ("online", PyRep::Bool(is_online)),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }

    if cmd == "/load"
        || cmd
            .strip_prefix("/load")
            .is_some_and(|r| r.starts_with(char::is_whitespace))
    {
        let tokens = parse_slash_tokens(cmd.strip_prefix("/load").unwrap_or(""));
        if tokens.is_empty() {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /load [target|me] <type_id> [quantity]".into(),
                client_error: None,
            }));
        }

        let parse_qty = |tok_opt: Option<&String>| -> Result<i32, BridgeRpcError> {
            match tok_opt {
                None => Ok(1),
                Some(s) => s
                    .parse::<i32>()
                    .ok()
                    .filter(|&q| q > 0)
                    .ok_or_else(|| BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Quantity for /load must be a positive integer".into(),
                        client_error: None,
                    }),
            }
        };

        let (explicit_target_tok, type_id_tok, qty_tok) = if is_me_or_myship(&tokens[0]) {
            let Some(tid_s) = tokens.get(1) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: "Usage: /load me <type_id> [quantity]".into(),
                    client_error: None,
                }));
            };
            (Some(tokens[0].as_str()), tid_s.as_str(), tokens.get(2))
        } else if tokens.len() >= 2 {
            let first_i64 = tokens[0].parse::<i64>().unwrap_or(0);
            let is_known_target = first_i64 > 0
                && (ctx.sde.stations.contains_key(&first_i64)
                    || ctx.db.with_snapshot(|snap| {
                        snap.characters.contains_key(&first_i64)
                            || snap.items.contains_key(&first_i64)
                            || snap.structures.contains_key(&first_i64)
                    }));
            if is_known_target {
                (Some(tokens[0].as_str()), tokens[1].as_str(), tokens.get(2))
            } else {
                (None, tokens[0].as_str(), tokens.get(1))
            }
        } else {
            (None, tokens[0].as_str(), None)
        };

        let Ok(type_id) = type_id_tok.parse::<u32>() else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /load [target|me] <type_id> [quantity]".into(),
                client_error: None,
            }));
        };
        let qty = match parse_qty(qty_tok) {
            Ok(q) => q,
            Err(e) => return Some(Err(e)),
        };
        let Some(inv_type) = ctx.sde.types.get(&type_id) else {
            return Some(Err(BridgeRpcError {
                code: "TypeNotFound".into(),
                message: format!("Type ID {type_id} not found in SDE"),
                client_error: None,
            }));
        };
        let is_ship = inv_type.category_id == 6;

        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let resolve_char_dest = |char_id: i64| -> Result<(i64, i64, InventoryFlag), BridgeRpcError> {
                let ch = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {char_id} not found"),
                    client_error: None,
                })?;
                if ch.active_ship_id > 0 && snap.items.contains_key(&ch.active_ship_id) {
                    Ok((char_id, ch.active_ship_id, InventoryFlag::Cargo))
                } else if let Some(st_id) = ch.station_id.or(ch.structure_id) {
                    Ok((char_id, st_id, InventoryFlag::Hangar))
                } else {
                    Err(BridgeRpcError {
                        code: "TargetNotFound".into(),
                        message: format!("Character {char_id} has no active ship or docked station"),
                        client_error: None,
                    })
                }
            };

            let (owner_id, location_id, flag) = match explicit_target_tok {
                None => resolve_char_dest(cid)?,
                Some(tok) if is_me_or_myship(tok) => resolve_char_dest(cid)?,
                Some(tok) => {
                    let target_id = tok.parse::<i64>().map_err(|_| BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: format!("Invalid target '{tok}' for /load"),
                        client_error: None,
                    })?;
                    if snap.characters.contains_key(&target_id) {
                        resolve_char_dest(target_id)?
                    } else if let Some(target_item) = snap.items.get(&target_id) {
                        (target_item.owner_id, target_id, InventoryFlag::Cargo)
                    } else if snap.structures.contains_key(&target_id)
                        || ctx.sde.stations.contains_key(&target_id)
                    {
                        (cid, target_id, InventoryFlag::Hangar)
                    } else {
                        return Err(BridgeRpcError {
                            code: "TargetNotFound".into(),
                            message: format!("Target {target_id} not found for /load"),
                            client_error: None,
                        });
                    }
                }
            };

            snap.next_entity_id += 1;
            let item_id = snap.next_entity_id;
            let rec = InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity: qty,
                singleton: is_ship && qty == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            };
            snap.items.insert(item_id, rec.clone());
            Ok(rec)
        });

        return match res {
            Ok(rec) => {
                ctx.notify_item_mutation(rec.owner_id, None, Some(&rec));
                // V24.02 `menu_implant.PlugEmIn`, `dna.Load`, and `svc_invtools.SpawnRandom`
                // index `slash.SlashCmd('/load ...')[0]` as the spawned `itemID`, while chat
                // and `svc_slash.JobHandler` format non-`None` replies via `'slash result: %s' % res`.
                // Returning a `PyRep::List` (rather than a 2-tuple) satisfies `res[0]` without
                // triggering Python `TypeError` tuple unpacking in `%s` formatting.
                Some(Ok(PyRep::List(vec![
                    PyRep::Integer(rec.item_id),
                    PyRep::keyval([
                        ("command", PyRep::String(cmd.to_string())),
                        ("itemID", PyRep::Integer(rec.item_id)),
                        ("typeID", PyRep::Integer(type_id as i64)),
                        ("quantity", PyRep::Integer(qty as i64)),
                        ("locationID", PyRep::Integer(rec.location_id)),
                        ("flagID", PyRep::Integer(rec.flag as i64)),
                    ]),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }

    if let Some(rest) = cmd.strip_prefix("/kill ") {
        let tokens = parse_slash_tokens(rest);
        let Some(tok) = tokens.first() else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /kill <target_id>".into(),
                client_error: None,
            }));
        };
        let target_id = if is_me_or_myship(tok) {
            cid
        } else {
            let Some(tid) = tok.parse::<i64>().ok().filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: "Usage: /kill <target_id>".into(),
                    client_error: None,
                }));
            };
            tid
        };
        let lookup = ctx.db.with_snapshot(|snap| {
            let caller_sys = session
                .solar_system_id
                .or_else(|| snap.characters.get(&cid).map(|c| c.solar_system_id))
                .unwrap_or(0);
            if let Some(ch) = snap.characters.get(&target_id) {
                Some((ch.character_id, ch.active_ship_id, ch.solar_system_id, true))
            } else if let Some(item) = snap.items.get(&target_id) {
                let sys = snap
                    .characters
                    .get(&item.owner_id)
                    .map(|c| c.solar_system_id)
                    .unwrap_or(caller_sys);
                Some((item.owner_id, target_id, sys, true))
            } else {
                Some((cid, target_id, caller_sys, false))
            }
        });
        let default_sys = session.solar_system_id.unwrap_or(0);
        let (victim_char_id, victim_ship_id, sys_id, in_db_items) =
            lookup.unwrap_or((cid, target_id, default_sys, false));
        let handle = ctx.systems.get_or_spawn(sys_id);
        let ball_opt = handle
            .get_ball_snapshot(victim_ship_id)
            .await
            .ok()
            .flatten();
        let victim_pos = ball_opt
            .as_ref()
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::ZERO);

        if in_db_items {
            match execute_death_pipeline(
                ctx,
                sys_id,
                victim_char_id,
                victim_ship_id,
                cid,
                victim_pos,
                ctx.now_unix(),
            )
            .await
            {
                Ok(outcome) => {
                    if victim_char_id == cid {
                        if let Some(cap_id) = outcome.ejected_capsule_id {
                            let _ = ctx.execute_session_transition_sync(
                                Some(session),
                                crate::router::SessionTransitionSpec::for_kind(
                                    cid,
                                    crate::router::TransitionKind::EjectToPod { capsule_id: cap_id },
                                    "gm_kill",
                                )
                                .with_apply_db(false)
                                .with_emit_on_session_changed(false),
                            );
                        }
                    }
                    return Some(Ok(PyRep::keyval([
                        ("command", PyRep::String(cmd.to_string())),
                        ("targetID", PyRep::Integer(victim_ship_id)),
                        ("wreckID", PyRep::Integer(outcome.wreck_item_id)),
                        ("killmailID", PyRep::Integer(outcome.killmail_id)),
                        ("destroyed", PyRep::Bool(true)),
                    ])));
                }
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "KillFailed".into(),
                        message: e,
                        client_error: None,
                    }))
                }
            }
        } else {
            let Some((_, ball)) = ball_opt else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotFound".into(),
                    message: format!("Target entity {target_id} not found"),
                    client_error: None,
                }));
            };
            let _ = handle.remove_ball(victim_ship_id).await;
            ctx.systems
                .invoke_npc_death_hook(sys_id, victim_ship_id, Some(cid))
                .await;
            crate::services::fw_gameplay::on_space_entity_destroyed(
                ctx,
                sys_id,
                Some(cid),
                victim_ship_id,
                0,
            )
            .await;
            if let Ok(mut spawns) = ctx.npc_spawns.write() {
                if let Some(mgr) = spawns.get_mut(&sys_id) {
                    mgr.entities.remove(&victim_ship_id);
                }
            }
            let wreck_type_id = ctx.sde.resolve_wreck_type_id(ball.type_id, None).unwrap_or(26557);
            let (wreck_id, killmail_id) = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let wid = snap.next_entity_id;
                snap.items.insert(
                    wid,
                    InventoryItemRecord {
                        item_id: wid,
                        type_id: wreck_type_id,
                        owner_id: cid,
                        location_id: sys_id as i64,
                        flag: InventoryFlag::Cargo,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                snap.next_entity_id += 1;
                let kid = snap.next_entity_id;
                snap.record_killmail(
                    KillmailRecord {
                        kill_id: kid,
                        victim_character_id: 0,
                        attacker_character_id: cid,
                        solar_system_id: sys_id,
                        ship_type_id: ball.type_id,
                        total_value_isk: 10_000.0,
                        ..Default::default()
                    },
                    ctx.now_unix(),
                );
                (wid, kid)
            });
            let _ = handle
                .spawn_ball_with_combat(
                    Ball {
                        id: wreck_id,
                        type_id: wreck_type_id,
                        position: victim_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: 10_000.0,
                        radius: 50.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState::default(),
                )
                .await;
            return Some(Ok(PyRep::keyval([
                ("command", PyRep::String(cmd.to_string())),
                ("targetID", PyRep::Integer(victim_ship_id)),
                ("wreckID", PyRep::Integer(wreck_id)),
                ("killmailID", PyRep::Integer(killmail_id)),
                ("destroyed", PyRep::Bool(true)),
            ])));
        }
    }

    if cmd == "/location" || cmd.starts_with("/location ") {
        let target_cid = cmd
            .strip_prefix("/location")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(cid);
        let ch_opt = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&target_cid).cloned());
        let Some(ch) = ch_opt else {
            return Some(Err(BridgeRpcError {
                code: "CharacterNotFound".into(),
                message: format!("Character {target_cid} not found"),
                client_error: None,
            }));
        };
        let handle = ctx.systems.get_or_spawn(ch.solar_system_id);
        let pos = handle
            .get_ball_snapshot(ch.active_ship_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::ZERO);

        return Some(Ok(PyRep::keyval([
            ("command", PyRep::String(cmd.to_string())), ("characterID", PyRep::Integer(ch.character_id)),
            ("solarSystemID", PyRep::Integer(ch.solar_system_id as i64)), ("constellationID", PyRep::Integer(ch.constellation_id as i64)),
            ("regionID", PyRep::Integer(ch.region_id as i64)), ("stationID", ch.station_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("structureID", ch.structure_id.map(PyRep::Integer).unwrap_or(PyRep::None)), ("shipID", PyRep::Integer(ch.active_ship_id)),
            ("x", PyRep::Real(pos.x)), ("y", PyRep::Real(pos.y)), ("z", PyRep::Real(pos.z)),
        ])));
    }

    if cmd == "/whoami" || cmd.starts_with("/whoami ") {
        let ch = match ctx.db.get_character(cid) {
            Ok(c) => c,
            Err(e) => {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: e.to_string(),
                    client_error: None,
                }))
            }
        };
        let summary = format!(
            "userID={} charID={} ({}) corpID={} solarSystemID={} stationID={:?} structureID={:?} shipID={} role=0x{:x}",
            session.user_id.unwrap_or(0),
            cid,
            ch.name,
            ch.corporation_id,
            ch.solar_system_id,
            ch.station_id,
            ch.structure_id,
            ch.active_ship_id,
            session.role,
        );
        return Some(Ok(PyRep::String(summary)));
    }

    if cmd == "/online" || cmd.starts_with("/online ") {
        let user_ids: Vec<u64> = ctx.db.with_snapshot(|snap| snap.accounts.values().map(|a| a.user_id).collect());
        let mut online_pairs = Vec::new();
        for uid in user_ids {
            if let Some(os) = ctx.db.get_online_session(uid) { online_pairs.push((uid, os.character_id.unwrap_or(0))); }
        }
        let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
            online_pairs.into_iter().map(|(uid, char_id_val)| {
                let (name, sys_id) = snap.characters.get(&char_id_val).map(|c| (c.name.clone(), c.solar_system_id as i64)).unwrap_or_else(|| ("Unselected".to_string(), 0));
                vec![PyRep::Integer(uid as i64), PyRep::Integer(char_id_val), PyRep::String(name), PyRep::Integer(sys_id)]
            }).collect()
        });
        let count = rows.len() as i64;
        return Some(Ok(PyRep::keyval([
            ("command", PyRep::String(cmd.to_string())),
            ("onlineCount", PyRep::Integer(count)),
            ("characters", PyRep::rowset(vec!["userID", "characterID", "characterName", "solarSystemID"], rows)),
        ])));
    }

    if cmd.starts_with("/contact ") || cmd.starts_with("/standing ") {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let target_id = match parts.get(1).and_then(|s| s.parse::<i64>().ok()) {
            Some(tid) if tid > 0 => tid,
            _ => return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /contact <targetID> <standing>".into(), client_error: None })),
        };
        let standing = match parts.get(2).and_then(|s| s.parse::<f64>().ok()) {
            Some(s) if s.is_finite() => s.clamp(-10.0, 10.0),
            Some(_) => return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Standing must be finite".into(), client_error: None })),
            None => 5.0,
        };
        let valid_entity = ctx.db.with_snapshot(|snap| {
            snap.characters.contains_key(&target_id) || snap.corporations.contains_key(&target_id) || snap.alliances.contains_key(&target_id)
        }) || ctx.sde.npc_corporations.contains_key(&target_id) || ctx.sde.factions.contains_key(&(target_id as u32));
        if !valid_entity {
            return Some(Err(BridgeRpcError { code: "ContactNotFound".into(), message: format!("Target {target_id} not found"), client_error: None }));
        }
        let contact_type = if (99_000_000..100_000_000).contains(&target_id) { "alliance" } else if (1_000_000..2_000_000).contains(&target_id) || (98_000_000..99_000_000).contains(&target_id) { "corporation" } else { "character" };
        if let Ok(mut guard) = ctx.esi_state.char_contacts.write() {
            let list = guard.entry(cid).or_default();
            if let Some(entry) = list.iter_mut().find(|e| e.contact_id == target_id) {
                entry.standing = standing;
            } else {
                list.push(crate::esi::runtime_state::EsiContactEntry {
                    contact_id: target_id, contact_type: contact_type.into(), standing, is_watched: false, is_blocked: false, label_ids: Vec::new(),
                });
            }
        }
        if ctx.sde.npc_corporations.contains_key(&target_id) || ctx.sde.factions.contains_key(&(target_id as u32)) {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(c) = snap.characters.get_mut(&cid) {
                    c.standings.insert(target_id, standing);
                }
            });
        }
        ctx.emit_typed(
            NotificationTarget::Character(cid),
            OnSetPlayerStanding {
                contact_id: target_id,
                standing,
            },
        );
        return Some(Ok(PyRep::keyval([("contactID", PyRep::Integer(target_id)), ("standing", PyRep::Real(standing))])));
    }

    if cmd == "/downtime" || cmd.starts_with("/downtime ") {
        let rep = crate::services::serverops_cluster::run_daily_downtime_and_jobs(ctx);
        let entries = rep.into_iter().map(|(k, v)| (PyRep::String(k), PyRep::Integer(v))).collect();
        return Some(Ok(PyRep::Dict(entries)));
    }
    if let Some(r) = cmd.strip_prefix("/tidi") {
        let arg = r.trim();
        let Some(sys_id) = session.solar_system_id else {
            return Some(Err(BridgeRpcError { code: "InvalidLocation".into(), message: "Pilot not in solar system".into(), client_error: None }));
        };
        if arg.eq_ignore_ascii_case("status") || arg.is_empty() {
            return Some(Ok(crate::services::serverops_cluster::system_tidi_status_pyrep(ctx, sys_id)));
        }
        if arg.eq_ignore_ascii_case("clear") || arg.eq_ignore_ascii_case("reset") {
            return Some(Ok(PyRep::Real(crate::services::serverops_cluster::clear_manual_system_tidi_override(ctx, sys_id))));
        }
        let f = match arg.parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 && v <= 1.0 => v,
            _ => return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /tidi <factor 0.0..1.0|status|clear>".into(), client_error: None })),
        };
        return Some(Ok(PyRep::Real(crate::services::serverops_cluster::set_system_tidi_factor(ctx, sys_id, f))));
    }
    if cmd == "/reinforce" || cmd.starts_with("/reinforce ") {
        let Some(sys_id) = session.solar_system_id else {
            return Some(Err(BridgeRpcError { code: "InvalidLocation".into(), message: "Pilot not in solar system".into(), client_error: None }));
        };
        let custom_node = cmd.strip_prefix("/reinforce").unwrap_or("").trim().parse::<u32>().ok().filter(|&n| n > 0);
        let nid = match custom_node {
            Some(n) => crate::services::serverops_cluster::assign_or_reinforce_system_node(ctx, sys_id, Some(n)),
            None => crate::services::serverops_cluster::reinforce_solar_system_for_fleet_fight(ctx, sys_id),
        };
        return Some(Ok(PyRep::Integer(nid as i64)));
    }
    if let Some(r) = cmd.strip_prefix("/autotidi") {
        let arg = r.trim();
        if arg.eq_ignore_ascii_case("off") || arg == "0" {
            crate::services::serverops_cluster::set_auto_cluster_tidi_enabled(ctx, false);
            return Some(Ok(PyRep::Integer(0)));
        }
        if arg.eq_ignore_ascii_case("on") {
            crate::services::serverops_cluster::set_latency_auto_tidi_enabled(ctx, true);
            return Some(Ok(PyRep::Integer(1)));
        }
        let thr = match arg.parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /autotidi <threshold|on|off>".into(), client_error: None })),
        };
        crate::services::serverops_cluster::set_cluster_node_load_threshold(ctx, thr);
        crate::services::serverops_cluster::set_auto_cluster_tidi_enabled(ctx, true);
        return Some(Ok(PyRep::Integer(thr as i64)));
    }
    if cmd == "/survey" || cmd.starts_with("/survey ") {
        return crate::services::operations_manager::handle_survey_slash(ctx, cid, cmd);
    }
    if cmd == "/season" || cmd.starts_with("/season ") {
        return crate::services::notify_emitters_seasons_tactical::handle_season_slash(ctx, cid, cmd);
    }
    if cmd == "/operation" || cmd.starts_with("/operation ") {
        return crate::services::notify_emitters_operations_nodegraph::handle_operation_slash(ctx, session, cid, cmd);
    }
    if cmd == "/pd" || cmd.starts_with("/pd ") {
        return crate::services::project_discovery::handle_project_discovery_slash(ctx, cmd);
    }
    if let Some(rest) = cmd.strip_prefix("/energy ") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let resolve_cid_arg = |s: &str| -> Option<i64> {
            if is_me_or_myship(s) || s.eq_ignore_ascii_case("self") {
                Some(cid)
            } else {
                s.parse::<i64>().ok()
            }
        };
        let (target_cid, new_energy) = match parts.as_slice() {
            [val_s] => (Some(cid), val_s.parse::<i64>().ok()),
            [cid_s, val_s] => (resolve_cid_arg(cid_s), val_s.parse::<i64>().ok()),
            _ => (None, None),
        };
        let (Some(target_cid), Some(energy_val)) = (target_cid.filter(|&id| id > 0), new_energy) else {
            return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /energy [char_id] <amount>".into(), client_error: None }));
        };
        if !ctx.db.with_snapshot(|snap| snap.characters.contains_key(&target_cid)) {
            return Some(Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {target_cid} not found"), client_error: None }));
        }
        // Update character CONCORD Encounter Surveillance System / CEQ energy state and emit OnCharacterEnergyChanged.
        let st = crate::services::character_energy::set_character_energy(ctx, target_cid, energy_val);
        return Some(Ok(crate::services::character_energy::energy_to_keyval(&st)));
    }
    if let Some(rest) = cmd.strip_prefix("/secstatus ") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let resolve_cid_arg = |s: &str| -> Option<i64> {
            if is_me_or_myship(s) || s.eq_ignore_ascii_case("self") {
                Some(cid)
            } else {
                s.parse::<i64>().ok()
            }
        };
        let (target_cid, new_sec) = match parts.as_slice() {
            [val_s] => (Some(cid), val_s.parse::<f64>().ok()),
            [cid_s, val_s] => (resolve_cid_arg(cid_s), val_s.parse::<f64>().ok()),
            _ => (None, None),
        };
        let (Some(target_cid), Some(sec_val)) = (target_cid.filter(|&id| id > 0), new_sec.filter(|v| v.is_finite())) else {
            return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /secstatus [char_id] <value>".into(), client_error: None }));
        };
        let clamped = sec_val.clamp(-10.0, 10.0);
        let updated = ctx.db.with_mut_snapshot(|snap| {
            let Some(ch) = snap.characters.get_mut(&target_cid) else {
                return false;
            };
            ch.security_status = clamped;
            true
        });
        if !updated {
            return Some(Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {target_cid} not found"), client_error: None }));
        }
        ctx.emit_typed(
            NotificationTarget::Character(target_cid),
            OnSecurityStatusUpdate {
                new_security_status: clamped,
            },
        );
        return Some(Ok(PyRep::Real(clamped)));
    }

    if cmd == "/session" || cmd.starts_with("/session ") {
        return Some(crate::services::gm_soak_commands::handle_session_cmd(
            ctx, session, cid, cmd,
        ));
    }

    if cmd == "/spawnwreck" || cmd.starts_with("/spawnwreck ") {
        let tokens = parse_slash_tokens(cmd.strip_prefix("/spawnwreck").unwrap_or(""));
        let (target_cid, arg_tokens) = resolve_gm_char_prefix(ctx, cid, &tokens, 1);
        let tail = arg_tokens.join(" ");
        let forwarded_cmd = if arg_tokens.len() < tokens.len() {
            if tail.is_empty() {
                "/spawnwreck".to_string()
            } else {
                format!("/spawnwreck {tail}")
            }
        } else {
            cmd.to_string()
        };
        let mut target_session = session.clone();
        if target_cid != cid {
            ctx.db.with_snapshot(|snap| {
                if let Some(ch) = snap.characters.get(&target_cid) {
                    target_session.character_id = Some(target_cid);
                    target_session.solar_system_id = Some(ch.solar_system_id);
                    target_session.station_id = ch.station_id;
                    target_session.structure_id = ch.structure_id;
                    target_session.ship_id = Some(ch.active_ship_id);
                }
            });
        }
        if let Some(res) = crate::services::gm_soak_commands::handle_gm_soak_slash_cmd(
            ctx,
            &target_session,
            target_cid,
            &forwarded_cmd,
        )
        .await
        {
            return Some(res);
        }
    }

    if let Some(res) =
        crate::services::gm_soak_commands::handle_gm_soak_slash_cmd(ctx, session, cid, cmd).await
    {
        return Some(res);
    }

    if let Some(res) = crate::services::notify_emitters_character_services::handle_batch3_gm_slash_cmd(ctx, cid, session.solar_system_id, cmd) {
        if let Ok(ref rep) = res {
            let head_lower = cmd
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(head_lower.as_str(), "/advancetime" | "/fastforward" | "/ff") {
                let adv_cid = rep.dict_get("characterID").and_then(PyRep::as_i64).unwrap_or(cid);
                let adv_secs = rep
                    .dict_get("advancedSeconds")
                    .and_then(PyRep::as_i64)
                    .unwrap_or(0)
                    .max(0) as u64;
                if adv_secs > 0 {
                    let explicit_space = if adv_cid == cid {
                        match (session.solar_system_id, session.ship_id) {
                            (Some(sys_id), Some(ship_id))
                                if session.station_id.is_none()
                                    && session.structure_id.is_none()
                                    && sys_id > 0
                                    && ship_id > 0 =>
                            {
                                Some((sys_id, ship_id))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    crate::services::notify_emitters_character_services::fast_forward_and_sync_space_clock(
                        ctx,
                        adv_cid,
                        explicit_space,
                        adv_secs,
                    )
                    .await;
                }
            }
        }
        return Some(res);
    }

    None
}

pub async fn handle_gm_qa_rpcs(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if (service, method) == ("slash", "SlashCmd") {
        let cmd = args.first().and_then(|a| a.as_str()).unwrap_or("").trim();
        if cmd == "/" {
            // Defer remote command list query (`FetchRemoteCommandList`) to `auth_cache`.
            return None;
        }
        let (_, cid) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let req_role = crate::services::auth_cache::required_slash_role(cmd);
        if !ServerContext::has_gm_role_mask(session.role, req_role) {
            crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
            return Some(Err(BridgeRpcError {
                code: "InsufficientRole".into(),
                message: format!("slash.SlashCmd '{cmd}' requires role mask 0x{req_role:x}"),
                client_error: None,
            }));
        }
        if let Some(ext_res) = handle_gm_slash_ext(ctx, session, cid, cmd).await {
            crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, ext_res.is_ok());
            return Some(ext_res);
        }
        return None;
    }
    match (service, method) {
        ("publicQaToolsServer", "SlashCmd") => {
            let (_, cid) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cmd = args.first().and_then(|a| a.as_str()).unwrap_or("").trim();
            let req_role = crate::services::auth_cache::required_slash_role(cmd);
            if !ServerContext::has_gm_role_mask(session.role, req_role) {
                crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: format!("publicQaToolsServer.SlashCmd '{cmd}' requires role mask 0x{req_role:x}"),
                    client_error: None,
                }));
            }
            if cmd == "/" {
                crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
                return Some(Err(BridgeRpcError {
                    code: "SlashError".into(),
                    message: crate::services::auth_cache::format_remote_command_list_reason(
                        session.role,
                    ),
                    client_error: None,
                }));
            }
            if let Some(ext_res) = handle_gm_slash_ext(ctx, session, cid, cmd).await {
                crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, ext_res.is_ok());
                return Some(ext_res);
            }
            crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
            Some(Err(BridgeRpcError {
                code: "UnknownSlashCommand".into(),
                message: format!("Unknown or unsupported QA slash command '{cmd}'"),
                client_error: None,
            }))
        }
        // `publicQaToolsServer.MoveMeTo` and `publicQaToolsServer.CanGiveItemForMultifit`
        // dispatch via `jump_clone_names::RPCS` in `RpcRegistry`.
        _ => None,
    }
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_qa_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    _kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_gm_qa_rpcs(ctx, session, service, method, args))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("publicQaToolsServer", "SlashCmd", RpcDispatchHandler::Async(dispatch_qa_rpc)),
    rpc_entry("charUnboundMgr", "GetCharactersToSelect", RpcDispatchHandler::Sync(handle_char_unbound)),
    rpc_entry("charUnboundMgr", "ValidateNameEx", RpcDispatchHandler::Sync(handle_char_unbound)),
    rpc_entry("charUnboundMgr", "GetValidRandomName", RpcDispatchHandler::Sync(handle_char_unbound)),
    rpc_entry("charUnboundMgr", "CreateCharacterWithDoll", RpcDispatchHandler::Sync(handle_char_unbound)),
];

#[cfg(test)] #[path = "character_gm_tools_unit_tests.rs"] mod character_gm_tools_unit_tests;

