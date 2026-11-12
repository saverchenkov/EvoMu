//! `character_gm_tools` — Unbound Character Creation/Selection (`CH-3`) & GM Slash/QA Command Dispatcher (`CH-5`).

#![forbid(unsafe_code)]

use super::{catch_up_skill_queue, is_valid_doll_or_portrait_payload, notify_watchlist_logon, serialize_doll_pyrep};
use crate::combat::death_pipeline::execute_death_pipeline;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, KillmailRecord, SkillHistoryEntry};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use glam::DVec3;
use std::collections::HashMap;

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
                    }));
                };
                if !ctx.sde.bloodlines.contains_key(&bid) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBloodline".into(),
                        message: format!("Bloodline {bid} does not exist in SDE"),
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
                }));
            };
            if !is_valid_character_name_syntax(name) || !crate::services::lookup_svc::is_word_allowed(name) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCharacterName".into(),
                    message: format!("Character name '{name}' violates EVE naming rules"),
                }));
            }

            // V24.01 ccSvc.CreateCharacterWithDoll passes 10 args:
            // (charactername, raceID, bloodlineID, genderID, ancestryID, charInfo, portraitInfo, schoolID, None, qa_starter_system_id)
            // Also support legacy 7-arg callers without raceID at args[1].
            if args.len() < 7 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires (charactername, [raceID,] bloodlineID, genderID, ancestryID, charInfo, portraitInfo, schoolID)".into(),
                }));
            }
            let has_race_arg = !is_valid_doll_or_portrait_payload(args.get(4).unwrap_or(&PyRep::None))
                && is_valid_doll_or_portrait_payload(args.get(5).unwrap_or(&PyRep::None));
            let offset = usize::from(has_race_arg);
            let Some(bloodline_id) = args.get(1 + offset).and_then(|a| a.as_i64()).map(|v| v as u32) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer bloodlineID".into(),
                }));
            };
            let Some(gid) = args.get(2 + offset).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer genderID (0 or 1)".into(),
                }));
            };
            if !(0..=1).contains(&gid) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidGender".into(),
                    message: format!("Invalid genderID {gid}; expected 0 (female) or 1 (male)"),
                }));
            }
            let gender_id = gid as u32;
            let Some(ancestry_id) = args.get(3 + offset).and_then(|a| a.as_i64()).map(|v| v as u32) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer ancestryID".into(),
                }));
            };
            let Some(char_info_rep) = args.get(4 + offset) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "Missing charInfo doll appearance argument".into(),
                }));
            };
            let Some(portrait_info_rep) = args.get(5 + offset) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "Missing portraitInfo argument".into(),
                }));
            };
            if !is_valid_doll_or_portrait_payload(char_info_rep)
                || !is_valid_doll_or_portrait_payload(portrait_info_rep)
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "charInfo and portraitInfo must be non-empty structured appearance descriptors (KeyVal or Dict)".into(),
                }));
            }
            let Some(school_id) = args.get(6 + offset).and_then(|a| a.as_i64()).map(|v| v as u32) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer schoolID".into(),
                }));
            };
            let Some(race_id) = ctx.sde.bloodlines.get(&bloodline_id).map(|b| b.race_id) else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterCreationFailed".into(),
                    message: format!("Unknown bloodline_id {bloodline_id}"),
                }));
            };
            let doll_str = serialize_doll_pyrep(char_info_rep);
            let portrait_str = serialize_doll_pyrep(portrait_info_rep);
            let is_qa_or_gm = session.has_gm_role()
                || (session.role
                    & (eve_machonet::service_roles::ROLE_QA
                        | eve_machonet::service_roles::ROLE_GM_MASK))
                    != 0;
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
                            crate::starter_characters::grant_starter_plex_in_snapshot(
                                snap, user_id, ch.character_id, ctx.feature_flags.starter_plex_balance, now_unix,
                            );
                        }
                    });
                    Some(Ok(PyRep::Integer(ch.character_id)))
                }
                Err(e) => Some(Err(BridgeRpcError { code: "CharacterCreationFailed".into(), message: e.to_string() })),
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
                }));
            };

            let mut c = match ctx.db.select_character_for_user(user_id, char_id) {
                Ok(ch) => ch,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "CharacterSelectionDenied".into(),
                        message: e.to_string(),
                    }))
                }
            };

            if let Some(prev_cid) = session.character_id.filter(|&id| id != c.character_id) {
                crate::services::fleet_social::revoke_xmpp_auth_token(ctx, prev_cid);
                crate::services::fleet_social::fleet_social_notify::on_character_logout(ctx, prev_cid);
                ctx.clear_notifications_for_char(prev_cid);
            }
            let was_online = session.character_id == Some(c.character_id)
                || crate::services::character_contacts::is_character_online(ctx, c.character_id);
            ctx.release_session_bindings(session.session_id);
            if let Some(os) = ctx.db.get_online_session(user_id) {
                if os.session_id == session.session_id {
                    let _ = ctx.db.acquire_online_lock(user_id, session.session_id, Some(c.character_id), false, ctx.now_unix());
                } else if os.character_id == Some(c.character_id) {
                    return Some(Err(BridgeRpcError {
                        code: "CharacterAlreadyOnline".into(),
                        message: format!("Character {} is already online on another active session", c.character_id),
                    }));
                }
            }
            ctx.clear_notifications_for_char(c.character_id);
            crate::router::cache_notifications::set_active_dispatch_char(Some(c.character_id));
            let eff_sys = crate::services::pve_special_sites::reconcile_abyssal_login(
                ctx, c.character_id, c.solar_system_id, c.active_ship_id,
            );
            crate::services::structure_control_ops::release_all_structure_control_for_character(
                ctx, None, c.character_id,
            );
            c = ctx.db.get_character(c.character_id).unwrap_or(c);
            let now_unix = ctx.now_unix();
            session.character_id = Some(c.character_id); session.corporation_id = Some(c.corporation_id);
            session.alliance_id = c.alliance_id; session.war_faction_id = c.war_faction_id;
            crate::router::apply_session_from_character(session, &c, None, Some(&ctx.sde));
            session.solar_system_id = Some(eff_sys);
            session.race_id = Some(c.race_id);
            session.corp_role = ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&c.corporation_id) {
                    return 0;
                }
                let (is_ceo, eff_roles) = snap.effective_corp_roles_at(
                    c.corporation_id,
                    c.character_id,
                    None,
                    0,
                    now_unix,
                );
                if is_ceo {
                    u64::MAX
                } else {
                    eff_roles
                }
            });

            if c.station_id.is_none() && c.structure_id.is_none() {
                let _ = crate::sp6_lifecycle::reconnect_character_in_space_sync(ctx, c.character_id);
            }

            ctx.sync_character_local_chat(c.character_id, None, eff_sys);
            crate::services::fleet_social::fleet_social_notify::on_character_select(ctx, session, c.character_id, was_online);
            let _ = crate::services::fleet_social::mint_or_rotate_xmpp_auth_token(ctx, c.character_id);
            catch_up_skill_queue(ctx, c.character_id);
            notify_watchlist_logon(ctx, c.character_id);
            crate::services::space_destiny::record_session_change_timer(
                ctx, c.character_id, now_unix, eff_sys, 0, "login",
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

/// Splits a slash command argument string into tokens, stripping matching outer
/// single or double quotes (e.g. `"me"` -> `me`) while preserving quoted spaces.
pub(crate) fn parse_slash_tokens(rest: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut in_token = false;
    for ch in rest.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                cur.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
            in_token = true;
        } else if ch.is_whitespace() {
            if in_token {
                tokens.push(std::mem::take(&mut cur));
                in_token = false;
            }
        } else {
            cur.push(ch);
            in_token = true;
        }
    }
    if in_token {
        tokens.push(cur);
    }
    tokens
}

fn is_me_or_myship(tok: &str) -> bool {
    tok.eq_ignore_ascii_case("me") || tok.eq_ignore_ascii_case("myship")
}

/// Handles GM slash commands added in `CH-5`:
/// `/heal [target_id]`, `/spawn <type_id> [qty]`, `/unspawn <id>`, `/setattr <item_id> <attr_id> <val>`,
/// `/getattr <item_id> <attr_id>`, `/fit [ship_id|"me"] <type_id> [flag]`, `/load [target|"me"] <type_id> [qty]`,
/// `/kill <target_id>`, `/location [char_id]`, and `/online`.
/// Executes `/heal [target_id]` across the live ballpark `CombatState`, persisted SP6/DB HP &
/// capacitor dynamic attributes, fitting heat/damage state, Destiny `DoDestinyUpdate`
/// `OnDamageStateChange`, and `godma` `OnModuleAttributeChanges` (`GH #949`).
pub(crate) async fn execute_gm_heal(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let tokens = parse_slash_tokens(cmd.strip_prefix("/heal").unwrap_or(""));
    let arg_id = tokens
        .first()
        .filter(|s| !is_me_or_myship(s))
        .and_then(|s| s.parse::<i64>().ok());

    let (target_char_id, sys_id, target_ship_id, ship_type_id) = ctx.db.with_snapshot(|snap| {
        let caller_sys = session
            .solar_system_id
            .or_else(|| snap.characters.get(&cid).map(|c| c.solar_system_id))
            .unwrap_or(0);
        let caller_ship = session
            .ship_id
            .or_else(|| snap.characters.get(&cid).map(|c| c.active_ship_id))
            .unwrap_or(0);
        if let Some(tid) = arg_id {
            if let Some(ch) = snap.characters.get(&tid) {
                let sid = ch.active_ship_id;
                let stid = snap.items.get(&sid).map(|i| i.type_id).unwrap_or(0);
                (tid, ch.solar_system_id, sid, stid)
            } else if let Some(item) = snap.items.get(&tid) {
                let owner_sys = snap
                    .characters
                    .get(&item.owner_id)
                    .map(|c| c.solar_system_id)
                    .unwrap_or(caller_sys);
                (item.owner_id, owner_sys, tid, item.type_id)
            } else {
                (cid, caller_sys, tid, 0)
            }
        } else {
            let stid = snap.items.get(&caller_ship).map(|i| i.type_id).unwrap_or(0);
            (cid, caller_sys, caller_ship, stid)
        }
    });

    let fit_opt = if target_ship_id > 0 && target_char_id > 0 {
        ctx.get_or_rebuild_ship_fitting(target_char_id, target_ship_id)
            .ok()
    } else {
        None
    };
    let (fit_max_hull, fit_max_armor, fit_max_shield, fit_max_cap) = if let Some(ref fit) = fit_opt
    {
        let mut eff_attrs: std::collections::HashMap<u16, f64> = [
            eve_sde::attr_ids::HP_STRUCT,
            eve_sde::attr_ids::ARMOR_HP,
            eve_sde::attr_ids::SHIELD_CAPACITY,
            eve_sde::attr_ids::CAPACITOR_CAPACITY,
        ]
        .into_iter()
        .map(|aid| (aid, fit.ship.get_attribute(aid, &ctx.sde)))
        .collect();
        crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(
            ctx,
            target_ship_id,
            &mut eff_attrs,
        );
        (
            eff_attrs
                .get(&eve_sde::attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(0.0),
            eff_attrs
                .get(&eve_sde::attr_ids::ARMOR_HP)
                .copied()
                .unwrap_or(0.0),
            eff_attrs
                .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                .copied()
                .unwrap_or(0.0),
            eff_attrs
                .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
                .copied()
                .unwrap_or(0.0),
        )
    } else if let Some(t) = ctx.sde.types.get(&ship_type_id) {
        (
            t.default_attributes
                .get(&eve_sde::attr_ids::HP_STRUCT)
                .copied()
                .unwrap_or(0.0),
            t.default_attributes
                .get(&eve_sde::attr_ids::ARMOR_HP)
                .copied()
                .unwrap_or(0.0),
            t.default_attributes
                .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                .copied()
                .unwrap_or(0.0),
            t.default_attributes
                .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
                .copied()
                .unwrap_or(0.0),
        )
    } else {
        (0.0, 0.0, 0.0, 0.0)
    };

    let handle_opt = if target_ship_id > 0 {
        Some(ctx.systems.get_or_spawn(sys_id))
    } else {
        None
    };
    let pre_cs = if let Some(ref handle) = handle_opt {
        handle
            .get_combat_state(target_ship_id)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let unified_fallback_maxes = if let Some(ref cs) = pre_cs {
        (
            if cs.max_structure_hp > 0.0 {
                cs.max_structure_hp
            } else {
                fit_max_hull
            },
            if cs.max_armor_hp > 0.0 {
                cs.max_armor_hp
            } else {
                fit_max_armor
            },
            if cs.max_shield_hp > 0.0 {
                cs.max_shield_hp
            } else {
                fit_max_shield
            },
        )
    } else {
        (fit_max_hull, fit_max_armor, fit_max_shield)
    };
    let (pre_unified, pre_dyn_shield_charge, pre_dyn_cap, mut repaired_child_attrs, pre_ship_heat) =
        ctx.db.with_snapshot(|snap| {
            let unified = if target_ship_id > 0 {
                Some(crate::sp6_lifecycle::read_unified_ship_damage_state(
                    ctx,
                    snap,
                    target_ship_id,
                    ship_type_id,
                    Some(unified_fallback_maxes),
                ))
            } else {
                None
            };
            let ship_it = snap.items.get(&target_ship_id);
            let dyn_shield = ship_it.and_then(|it| {
                it.dynamic_attributes
                    .get(&eve_sde::attr_ids::SHIELD_CHARGE)
                    .copied()
            });
            let dyn_cap = ship_it.and_then(|it| it.dynamic_attributes.get(&18u16).copied());
            let ship_heat = ship_it
                .and_then(|it| it.dynamic_attributes.get(&1211u16).copied())
                .unwrap_or(0.0);
            let mut child_changes: Vec<(i64, u16, f64)> = Vec::new();
            if target_ship_id > 0 {
                for item in snap.items.values() {
                    if item.location_id == target_ship_id {
                        if let Some(&old_d) = item.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)
                        {
                            if old_d > 0.0 {
                                child_changes.push((
                                    item.item_id,
                                    eve_sde::attr_ids::DAMAGE,
                                    old_d,
                                ));
                            }
                        }
                        if let Some(&old_h) = item.dynamic_attributes.get(&1211u16) {
                            if old_h > 0.0 {
                                child_changes.push((item.item_id, 1211u16, old_h));
                            }
                        }
                    }
                }
            }
            (unified, dyn_shield, dyn_cap, child_changes, ship_heat)
        });

    let mut combat_healed = CombatState::default();
    if let Some(ref handle) = handle_opt {
        if let Ok(cs) = handle.heal_combat_state(target_ship_id).await {
            combat_healed = cs;
        }
        // Flush `DoDestinyUpdate(OnDamageStateChange)` to ALL ready observers, including the healed ship itself.
        let _ = handle.flush_peer_observer_updates(None).await;
    }

    // Clear module and hull dynamic damage + shield/cap/heat attributes in DB, SP6 HP, and fitting cache.
    if target_ship_id > 0 {
        ctx.db.with_mut_snapshot(|snap| {
            crate::sp6_lifecycle::clear_ship_combat_damage_in_snap(snap, target_ship_id);
            for item in snap.items.values_mut() {
                if item.item_id == target_ship_id || item.location_id == target_ship_id {
                    let r1 = item
                        .dynamic_attributes
                        .remove(&eve_sde::attr_ids::DAMAGE)
                        .is_some();
                    let r2 = item
                        .dynamic_attributes
                        .remove(&eve_sde::attr_ids::ARMOR_DAMAGE)
                        .is_some();
                    let r3 = item.dynamic_attributes.remove(&1211u16).is_some(); // heatDamage
                    if (r1 || r2 || r3) && item.item_id != target_ship_id {
                        item.version += 1;
                    }
                }
            }
        });
    }
    let pre_rack_heat = fit_opt
        .as_ref()
        .map(|f| f.rack_heat)
        .unwrap_or([0.0, 0.0, 0.0]);
    if let Some(mut fit) = fit_opt {
        for m in fit.modules.values_mut() {
            if m.damage_hp > 0.0
                && !repaired_child_attrs
                    .iter()
                    .any(|&(id, aid, _)| id == m.item_id && aid == eve_sde::attr_ids::DAMAGE)
            {
                repaired_child_attrs.push((m.item_id, eve_sde::attr_ids::DAMAGE, m.damage_hp));
            }
            m.damage_hp = 0.0;
        }
        fit.rack_heat = [0.0, 0.0, 0.0];
        ctx.store_ship_fitting(target_ship_id, fit);
    }

    let eff_max_shield = combat_healed.max_shield_hp.max(fit_max_shield);
    let eff_max_armor = combat_healed.max_armor_hp.max(fit_max_armor);
    let eff_max_hull = combat_healed.max_structure_hp.max(fit_max_hull);
    let eff_max_cap = combat_healed.max_capacitor.max(fit_max_cap);

    let final_shield = if combat_healed.max_shield_hp > 0.0 {
        combat_healed.shield_hp
    } else {
        eff_max_shield
    };
    let final_armor = if combat_healed.max_armor_hp > 0.0 {
        combat_healed.armor_hp
    } else {
        eff_max_armor
    };
    let final_hull = if combat_healed.max_structure_hp > 0.0 {
        combat_healed.structure_hp
    } else {
        eff_max_hull
    };
    let final_cap = if combat_healed.max_capacitor > 0.0 {
        combat_healed.capacitor
    } else {
        eff_max_cap
    };

    if target_char_id > 0 && target_ship_id > 0 {
        let now_ft = ctx.now_blueos_filetime_ticks();
        let mut attr_changes: Vec<PyRep> = Vec::new();
        let push_change =
            |list: &mut Vec<PyRep>, item_id: i64, attr_id: u16, new_val: f64, old_val: f64| {
                list.push(PyRep::Tuple(vec![
                    PyRep::String("OnModuleAttributeChange".into()),
                    PyRep::Integer(target_char_id),
                    PyRep::Integer(item_id),
                    PyRep::Integer(i64::from(attr_id)),
                    PyRep::Integer(now_ft),
                    PyRep::Real(new_val),
                    PyRep::Real(old_val),
                    PyRep::None,
                ]));
            };

        let (old_shield, old_armor_dmg, old_hull_dmg) = if let Some(u) = pre_unified {
            let s = match pre_dyn_shield_charge {
                Some(dc) if pre_cs.as_ref().is_none_or(|c| c.max_shield_hp <= 0.0) => {
                    dc.clamp(0.0, final_shield.max(dc))
                }
                Some(dc) => u.shield_hp.min(dc).max(0.0),
                None => u.shield_hp,
            };
            (s, u.armor_damage, u.hull_damage)
        } else {
            (pre_dyn_shield_charge.unwrap_or(final_shield), 0.0, 0.0)
        };

        let old_cap = match (pre_cs.as_ref().filter(|c| c.max_capacitor > 0.0), pre_dyn_cap) {
            (Some(cs), Some(dc)) => cs.capacitor.min(dc).max(0.0),
            (Some(cs), None) => cs.capacitor.max(0.0),
            (None, Some(dc)) => dc.max(0.0),
            (None, None) => final_cap,
        };

        if final_shield > 0.0 || pre_dyn_shield_charge.is_some() {
            push_change(
                &mut attr_changes,
                target_ship_id,
                eve_sde::attr_ids::SHIELD_CHARGE,
                final_shield,
                old_shield,
            );
        }
        if final_armor > 0.0 || old_armor_dmg > 0.0 {
            push_change(
                &mut attr_changes,
                target_ship_id,
                eve_sde::attr_ids::ARMOR_DAMAGE,
                0.0,
                old_armor_dmg,
            );
        }
        if final_hull > 0.0 || old_hull_dmg > 0.0 {
            push_change(
                &mut attr_changes,
                target_ship_id,
                eve_sde::attr_ids::DAMAGE,
                0.0,
                old_hull_dmg,
            );
        }
        if final_cap > 0.0 || pre_dyn_cap.is_some() {
            push_change(
                &mut attr_changes,
                target_ship_id,
                18u16,
                final_cap,
                old_cap,
            );
        }
        if pre_ship_heat > 0.0 {
            push_change(&mut attr_changes, target_ship_id, 1211, 0.0, pre_ship_heat);
        }
        for (idx, rack_attr) in [1175u16, 1176, 1177].into_iter().enumerate() {
            if pre_rack_heat[idx] > 0.0 {
                push_change(
                    &mut attr_changes,
                    target_ship_id,
                    rack_attr,
                    0.0,
                    pre_rack_heat[idx],
                );
            }
        }
        repaired_child_attrs.sort_unstable_by_key(|&(id, aid, _)| (id, aid));
        for (mod_id, aid, old_val) in repaired_child_attrs {
            push_change(&mut attr_changes, mod_id, aid, 0.0, old_val);
        }
        if !attr_changes.is_empty() {
            ctx.notify(
                crate::router::NotificationTarget::Character(target_char_id),
                "OnModuleAttributeChanges",
                "charid",
                vec![PyRep::List(attr_changes)],
            );
        }
        let _ = ctx.notify_damage_state_change(
            crate::router::NotificationTarget::Character(target_char_id),
            target_ship_id,
            1.0,
            1.0,
            1.0,
        );
        let _ = ctx.notify_remote_message(
            crate::router::NotificationTarget::Character(target_char_id),
            "CustomNotify",
            vec![("notify", PyRep::String("Ship healed".into()))],
        );
    }

    Ok(PyRep::keyval([
        ("command", PyRep::String(cmd.to_string())),
        ("characterID", PyRep::Integer(target_char_id)),
        ("shipID", PyRep::Integer(target_ship_id)),
        ("targetID", PyRep::Integer(target_ship_id)),
        ("shieldHP", PyRep::Real(final_shield)),
        ("armorHP", PyRep::Real(final_armor)),
        ("hullHP", PyRep::Real(final_hull)),
        ("capacitor", PyRep::Real(final_cap)),
        ("moduleHeatCleared", PyRep::Bool(true)),
        ("healed", PyRep::Bool(true)),
    ]))
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
        let Some(type_id) = tokens.first().and_then(|s| s.parse::<u32>().ok()) else {
            return Some(Err(BridgeRpcError { code: "InvalidSlashArgument".into(), message: "Usage: /spawn <type_id> [quantity]".into() }));
        };
        let qty = tokens.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(1);
        if !(1..=100).contains(&qty) {
            return Some(Err(BridgeRpcError { code: "InvalidSlashArgument".into(), message: "Quantity for /spawn must be between 1 and 100".into() }));
        }
        let Some(inv_type) = ctx.sde.types.get(&type_id).cloned() else {
            return Some(Err(BridgeRpcError { code: "TypeNotFound".into(), message: format!("Type ID {type_id} not found in SDE") }));
        };
        let (sys_id, ship_id) = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&cid).map(|c| (c.solar_system_id, c.active_ship_id)))
            .or_else(|| session.solar_system_id.map(|s| (s, 0)))
            .unwrap_or((0, 0));

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
            let spawn_owner_id = if is_celestial_or_asteroid { 1 } else { cid };
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
            if inv_type.category_id == 25 {
                let _ = handle
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
                    .await;
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
                let _ = handle.spawn_ball_with_meta(ball, combat, meta).await;
                if let Ok(mut bp) = handle.ballpark.write() {
                    bp.static_ball_ids.insert(entity_id);
                }
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
                let _ = handle.spawn_ball_with_meta(ball, combat, meta).await;
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
                let meta = ctx.build_ship_slim_meta(cid, type_id);
                let _ = handle.spawn_ball_with_meta(ball, combat, meta).await;
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
            return Some(Err(BridgeRpcError { code: "InvalidSlashArgument".into(), message: "Usage: /unspawn <entity_id>".into() }));
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
        if let Ok(mut map) = ctx.npc_spawns.write() {
            if let Some(mgr) = map.get_mut(&sys_id) {
                mgr.entities.remove(&target_id);
            }
        }
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.remove(&target_id);
        });
        if removed_ball {
            crate::services::pve_incursion_sites::mark_incursion_boss_destroyed(ctx, sys_id, target_id);
            crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, sys_id, target_id).await;
        }
        if !removed_db && !removed_ball {
            return Some(Err(BridgeRpcError { code: "EntityNotFound".into(), message: format!("Entity {target_id} not found in space or inventory") }));
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
            }));
        };
        let Some(attr_id) = tokens.get(1).and_then(|s| s.parse::<u16>().ok()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /setattr <item_id|me> <attribute_id> <value>".into(),
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
            }));
        };

        let now_ft = ctx.now_blueos_filetime_ticks();
        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let item_id = if is_me_or_myship(item_tok) {
                snap.characters
                    .get(&cid)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0)
            } else {
                item_tok.parse::<i64>().map_err(|_| BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: format!("Invalid item_id '{item_tok}' for /setattr"),
                })?
            };
            let Some(item) = snap.items.get_mut(&item_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {item_id} not found"),
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
                ctx.invalidate_ship_fitting(item_id);
                ctx.invalidate_ship_fitting(loc_id);
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
            }));
        };
        let Some(attr_id) = tokens.get(1).and_then(|s| s.parse::<u16>().ok()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /getattr <item_id|me> <attribute_id>".into(),
            }));
        };
        let res = ctx.db.with_snapshot(|snap| {
            let item_id = if is_me_or_myship(item_tok) {
                snap.characters
                    .get(&cid)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0)
            } else {
                item_tok.parse::<i64>().unwrap_or(0)
            };
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
                }));
            };
            let flg = tokens
                .get(2)
                .and_then(|s| s.parse::<u16>().ok())
                .map(InventoryFlag::from_u16);
            (caller_ship_id, tid, flg)
        } else if tokens.len() >= 2 {
            let first_i64 = tokens[0].parse::<i64>().unwrap_or(0);
            let is_known_ship = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&first_i64)
                    .and_then(|i| ctx.sde.types.get(&i.type_id))
                    .is_some_and(|t| matches!(t.category_id, 6 | 65))
            });
            if is_known_ship {
                let Some(tid) = tokens.get(1).and_then(|s| s.parse::<u32>().ok()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /fit <ship_id> <type_id> [flag_id]".into(),
                    }));
                };
                let flg = tokens
                    .get(2)
                    .and_then(|s| s.parse::<u16>().ok())
                    .map(InventoryFlag::from_u16);
                (first_i64, tid, flg)
            } else {
                let Some(tid) = tokens[0].parse::<u32>().ok() else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSlashArgument".into(),
                        message: "Usage: /fit <type_id> [flag_id]".into(),
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
                }));
            };
            (caller_ship_id, tid, None)
        };

        let Some(mod_type) = ctx.sde.types.get(&type_id) else {
            return Some(Err(BridgeRpcError {
                code: "TypeNotFound".into(),
                message: format!("Module type {type_id} not found in SDE"),
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
            }));
        };

        let pre_fit_opt = if ship_cat_id == 6 {
            ctx.get_or_rebuild_ship_fitting(ship_owner_id, ship_id).ok()
        } else {
            None
        };
        let old_cpu = pre_fit_opt
            .as_ref()
            .map(|f| f.ship.get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde))
            .unwrap_or(0.0);
        let old_pg = pre_fit_opt
            .as_ref()
            .map(|f| f.ship.get_attribute(eve_sde::attr_ids::POWER_LOAD, &ctx.sde))
            .unwrap_or(0.0);

        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let Some(ship_item) = snap.items.get(&ship_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Target ship {ship_id} not found"),
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

            let has_slot_effect = mod_type.category_id == 32
                || effects.iter().any(|e| matches!(e, 11 | 12 | 13 | 2663 | 3772));
            let chosen_flag = if let Some(ef) = explicit_flag {
                ef
            } else if mod_type.category_id == 18 {
                InventoryFlag::DroneBay
            } else if ship_cat_id == 6 && matches!(mod_type.category_id, 7 | 32) && has_slot_effect {
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
                    });
                };
                valid_flag
            } else {
                candidate_flags
                    .iter()
                    .copied()
                    .find(|f| !occupied_flags.contains(f))
                    .unwrap_or(candidate_flags[0])
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
                let mut new_loads_opt: Option<(f64, f64)> = None;
                if is_fitting_slot {
                    if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(rec.owner_id, ship_id) {
                        // Explicit-flag GM `/fit <ship> <type> <flag>` forces the chosen slot online
                        // when hull CPU/PG permits; auto-slot `/fit <type>` checks pilot skills (#648).
                        let online_ok = if explicit_flag.is_some() {
                            fit.set_module_online_skip_skills(rec.item_id, &ctx.sde).is_ok()
                        } else {
                            fit.set_module_online(rec.item_id, &ctx.sde).is_ok()
                        };
                        if fit.modules.contains_key(&rec.item_id) && online_ok {
                            is_online = true;
                            let new_cpu = fit
                                .ship
                                .get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde);
                            let new_pg = fit
                                .ship
                                .get_attribute(eve_sde::attr_ids::POWER_LOAD, &ctx.sde);
                            new_loads_opt = Some((new_cpu, new_pg));
                        }
                    }
                    if is_online {
                        ctx.db.with_mut_snapshot(|snap| {
                            snap.module_online_states.insert(rec.item_id, true);
                        });
                        ctx.invalidate_ship_fitting(ship_id);
                    }
                }
                let item_row = eve_proto::DBRowDescriptor::inv_item_header()
                    .build_dbrow(crate::services::inventory_bound::item_to_packed_cells(ctx, &rec));
                // ixLocationID (3) = 0, ixFlag (4) = 0 notifies `invCache`, `godma`, and `dogmaLocation`
                // of the newly fitted module on `ship_id` at `chosen_flag`.
                let _ = ctx.notify_item_change(
                    rec.owner_id,
                    item_row,
                    vec![(3, PyRep::Integer(0)), (4, PyRep::Integer(0))],
                );
                if is_fitting_slot {
                    let group_id = ctx
                        .sde
                        .types
                        .get(&type_id)
                        .map(|t| i64::from(t.group_id))
                        .unwrap_or(0);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(rec.owner_id),
                        "OnModuleFitted",
                        "charid",
                        vec![
                            PyRep::Integer(rec.owner_id),
                            PyRep::Integer(chosen_flag as i64),
                            PyRep::Integer(type_id as i64),
                            PyRep::Integer(group_id),
                        ],
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
                        if let Some((new_cpu, new_pg)) = new_loads_opt {
                            if (new_cpu - old_cpu).abs() > 1e-6 || (new_pg - old_pg).abs() > 1e-6 {
                                let _ = ctx.notify_ship_fitting_load_changes(
                                    rec.owner_id, ship_id, old_cpu, new_cpu, old_pg, new_pg,
                                );
                            }
                        }
                    }
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

    if cmd == "/load" || cmd.starts_with("/load ") {
        let tokens = parse_slash_tokens(cmd.strip_prefix("/load").unwrap_or(""));
        if tokens.is_empty() {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /load [target|me] <type_id> [quantity]".into(),
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
                    }),
            }
        };

        let (explicit_target_tok, type_id_tok, qty_tok) = if is_me_or_myship(&tokens[0]) {
            let Some(tid_s) = tokens.get(1) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: "Usage: /load me <type_id> [quantity]".into(),
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
            }));
        };
        let is_ship = inv_type.category_id == 6;

        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let resolve_char_dest = |char_id: i64| -> Result<(i64, i64, InventoryFlag), BridgeRpcError> {
                let ch = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {char_id} not found"),
                })?;
                if ch.active_ship_id > 0 && snap.items.contains_key(&ch.active_ship_id) {
                    Ok((char_id, ch.active_ship_id, InventoryFlag::Cargo))
                } else if let Some(st_id) = ch.station_id.or(ch.structure_id) {
                    Ok((char_id, st_id, InventoryFlag::Hangar))
                } else {
                    Err(BridgeRpcError {
                        code: "TargetNotFound".into(),
                        message: format!("Character {char_id} has no active ship or docked station"),
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
                let inv_row = eve_proto::DBRowDescriptor::inv_item_header()
                    .build_dbrow(crate::services::inventory_fitting::item_to_packed_cells(ctx, &rec));
                let _ = ctx.notify_item_change(
                    rec.owner_id,
                    inv_row,
                    vec![(3, PyRep::Integer(0)), (4, PyRep::Integer(0))],
                );
                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("itemID", PyRep::Integer(rec.item_id)),
                    ("typeID", PyRep::Integer(type_id as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
                    ("locationID", PyRep::Integer(rec.location_id)),
                    ("flagID", PyRep::Integer(rec.flag as i64)),
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
            }));
        };
        let target_id = if is_me_or_myship(tok) {
            cid
        } else {
            let Some(tid) = tok.parse::<i64>().ok().filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSlashArgument".into(),
                    message: "Usage: /kill <target_id>".into(),
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
                    }))
                }
            }
        } else {
            let Some((_, ball)) = ball_opt else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotFound".into(),
                    message: format!("Target entity {target_id} not found"),
                }));
            };
            let _ = handle.remove_ball(victim_ship_id).await;
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
            _ => return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /contact <targetID> <standing>".into() })),
        };
        let standing = match parts.get(2).and_then(|s| s.parse::<f64>().ok()) {
            Some(s) if s.is_finite() => s.clamp(-10.0, 10.0),
            Some(_) => return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Standing must be finite".into() })),
            None => 5.0,
        };
        let valid_entity = ctx.db.with_snapshot(|snap| {
            snap.characters.contains_key(&target_id) || snap.corporations.contains_key(&target_id) || snap.alliances.contains_key(&target_id)
        }) || ctx.sde.npc_corporations.contains_key(&target_id) || ctx.sde.factions.contains_key(&(target_id as u32));
        if !valid_entity {
            return Some(Err(BridgeRpcError { code: "ContactNotFound".into(), message: format!("Target {target_id} not found") }));
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
        ctx.notify(crate::router::NotificationTarget::Character(cid), "OnSetPlayerStanding", "charid", vec![PyRep::Integer(target_id), PyRep::Real(standing)]);
        return Some(Ok(PyRep::keyval([("contactID", PyRep::Integer(target_id)), ("standing", PyRep::Real(standing))])));
    }

    if cmd == "/downtime" || cmd.starts_with("/downtime ") {
        let rep = crate::services::serverops_cluster::run_daily_downtime_and_jobs(ctx);
        let entries = rep.into_iter().map(|(k, v)| (PyRep::String(k), PyRep::Integer(v))).collect();
        return Some(Ok(PyRep::Dict(entries)));
    }
    if let Some(r) = cmd.strip_prefix("/tidi") {
        let f = match r.trim().parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 && v <= 1.0 => v,
            _ => return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /tidi <factor 0.0..1.0>".into() })),
        };
        let Some(sys_id) = session.solar_system_id else {
            return Some(Err(BridgeRpcError { code: "InvalidLocation".into(), message: "Pilot not in solar system".into() }));
        };
        return Some(Ok(PyRep::Real(crate::services::wallet_serverops::set_system_tidi_factor(ctx, sys_id, f))));
    }
    if cmd == "/reinforce" || cmd.starts_with("/reinforce ") {
        let Some(sys_id) = session.solar_system_id else {
            return Some(Err(BridgeRpcError { code: "InvalidLocation".into(), message: "Pilot not in solar system".into() }));
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
        let thr = match arg.parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /autotidi <threshold|off>".into() })),
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
        let (target_cid, new_energy) = match parts.as_slice() {
            [val_s] => (Some(cid), val_s.parse::<i64>().ok()),
            [cid_s, val_s] => (cid_s.parse::<i64>().ok(), val_s.parse::<i64>().ok()),
            _ => (None, None),
        };
        let (Some(target_cid), Some(energy_val)) = (target_cid.filter(|&id| id > 0), new_energy) else {
            return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /energy [char_id] <amount>".into() }));
        };
        if !ctx.db.with_snapshot(|snap| snap.characters.contains_key(&target_cid)) {
            return Some(Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {target_cid} not found") }));
        }
        // Update character CONCORD Encounter Surveillance System / CEQ energy state and emit OnCharacterEnergyChanged.
        let st = crate::services::character_energy::set_character_energy(ctx, target_cid, energy_val);
        return Some(Ok(crate::services::character_energy::energy_to_keyval(&st)));
    }
    if let Some(rest) = cmd.strip_prefix("/secstatus ") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let (target_cid, new_sec) = match parts.as_slice() {
            [val_s] => (Some(cid), val_s.parse::<f64>().ok()),
            [cid_s, val_s] => (cid_s.parse::<i64>().ok(), val_s.parse::<f64>().ok()),
            _ => (None, None),
        };
        let (Some(target_cid), Some(sec_val)) = (target_cid.filter(|&id| id > 0), new_sec.filter(|v| v.is_finite())) else {
            return Some(Err(BridgeRpcError { code: "InvalidArguments".into(), message: "Usage: /secstatus [char_id] <value>".into() }));
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
            return Some(Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {target_cid} not found") }));
        }
        ctx.notify(crate::router::NotificationTarget::Character(target_cid), "OnSecurityStatusUpdate", "charid", vec![PyRep::Real(clamped)]);
        return Some(Ok(PyRep::Real(clamped)));
    }

    if let Some(res) = crate::services::notify_emitters_character_services::handle_batch3_gm_slash_cmd(ctx, cid, session.solar_system_id, cmd) {
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
        if (session.role & req_role) == 0 {
            crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
            return Some(Err(BridgeRpcError {
                code: "InsufficientRole".into(),
                message: format!("slash.SlashCmd '{cmd}' requires role mask 0x{req_role:x}"),
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
            if (session.role & req_role) == 0 {
                crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: format!("publicQaToolsServer.SlashCmd '{cmd}' requires role mask 0x{req_role:x}"),
                }));
            }
            if cmd == "/" {
                crate::services::auth_cache::record_gm_audit(ctx, cid, cmd, false);
                return Some(Err(BridgeRpcError {
                    code: "SlashError".into(),
                    message: crate::services::auth_cache::format_remote_command_list_reason(
                        session.role,
                    ),
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
            }))
        }
        ("publicQaToolsServer", method) => {
            crate::services::jump_clone_names::handle_public_qa_tools(ctx, session, method, args)
                .await
        }
        _ => None,
    }
}
#[cfg(test)] #[path = "character_gm_tools_unit_tests.rs"] mod character_gm_tools_unit_tests;

