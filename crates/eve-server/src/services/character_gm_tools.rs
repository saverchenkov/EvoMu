//! `character_gm_tools` — Unbound Character Creation/Selection (`CH-3`) & GM Slash/QA Command Dispatcher (`CH-5`).

#![forbid(unsafe_code)]

use super::{catch_up_skill_queue, is_valid_doll_or_portrait_payload, notify_watchlist_logon, serialize_doll_pyrep};
use crate::combat::death_pipeline::execute_death_pipeline;
use crate::router::{NotificationTarget, ServerContext};
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

            // V24 signature: CreateCharacterWithDoll(charactername, bloodlineID, genderID, ancestryID, charInfo, portraitInfo, schoolID)
            if args.len() < 7 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires (charactername, bloodlineID, genderID, ancestryID, charInfo, portraitInfo, schoolID)".into(),
                }));
            }
            let Some(bloodline_id) = args.get(1).and_then(|a| a.as_i64()).map(|v| v as u32) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer bloodlineID".into(),
                }));
            };
            let Some(gid) = args.get(2).and_then(|a| a.as_i64()) else {
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
            let Some(ancestry_id) = args.get(3).and_then(|a| a.as_i64()).map(|v| v as u32) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreateCharacterWithDoll requires integer ancestryID".into(),
                }));
            };
            let Some(char_info_rep) = args.get(4) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "Missing charInfo doll appearance argument".into(),
                }));
            };
            let Some(portrait_info_rep) = args.get(5) else {
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
            let Some(school_id) = args.get(6).and_then(|a| a.as_i64()).map(|v| v as u32) else {
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

            let res = ctx.db.create_character(
                user_id,
                name,
                race_id,
                bloodline_id,
                ancestry_id,
                school_id,
                &ctx.sde,
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
                        if let Some(smap) = snap.skills.get(&ch.character_id).cloned() {
                            let hist = snap.skill_history.entry(ch.character_id).or_default();
                            let mut sorted: Vec<_> = smap.values().collect();
                            sorted.sort_by_key(|s| s.type_id);
                            for s in sorted {
                                hist.push(SkillHistoryEntry {
                                    log_date_ft: now_ft,
                                    // 33 = SkillEventCharCreation (evemu_Crucible EVE_Skills.h:24)
                                    event_type_id: 33,
                                    skill_type_id: s.type_id,
                                    skill_level: s.level,
                                    absolute_points: s.skill_points,
                                });
                            }
                        }
                    });
                    Some(Ok(PyRep::Integer(ch.character_id)))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "CharacterCreationFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("charUnboundMgr", "SelectCharacterID") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(char_id) = args.first().and_then(|a| a.as_i64()) else {
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
                crate::services::fleet_social::fleet_social_notify::on_character_logout(ctx, prev_cid);
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
            let eff_sys = crate::services::pve_special_sites::reconcile_abyssal_login(
                ctx, c.character_id, c.solar_system_id, c.active_ship_id,
            );
            c = ctx.db.get_character(c.character_id).unwrap_or(c);
            session.character_id = Some(c.character_id); session.corporation_id = Some(c.corporation_id);
            session.alliance_id = c.alliance_id; session.war_faction_id = c.war_faction_id;
            session.station_id = c.station_id; session.structure_id = c.structure_id;
            session.solar_system_id = Some(eff_sys); session.constellation_id = Some(c.constellation_id);
            session.region_id = Some(c.region_id); session.ship_id = Some(c.active_ship_id); session.race_id = Some(c.race_id);

            if c.station_id.is_none() && c.structure_id.is_none() {
                let _ = crate::sp6_lifecycle::reconnect_character_in_space_sync(ctx, c.character_id);
            }

            ctx.sync_character_local_chat(c.character_id, None, eff_sys);
            crate::services::fleet_social::fleet_social_notify::on_character_select(ctx, session, c.character_id, was_online);
            catch_up_skill_queue(ctx, c.character_id);
            notify_watchlist_logon(ctx, c.character_id);
            let now_unix = ctx.now_unix();
            crate::services::space_destiny::record_session_change_timer(
                ctx, c.character_id, now_unix, eff_sys, 0, "login",
            );

            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                ("shipID", PyRep::Integer(c.active_ship_id)),
                ("solarSystemID", PyRep::Integer(eff_sys as i64)),
            ])))
        }

        _ => None,
    }
}

/// Handles GM slash commands added in `CH-5`:
/// `/heal [target_id]`, `/spawn <type_id> [qty]`, `/unspawn <id>`, `/setattr <item_id> <attr_id> <val>`,
/// `/getattr <item_id> <attr_id>`, `/fit [ship_id|"me"] <type_id> [flag]`, `/kill <target_id>`,
/// `/location [char_id]`, and `/online`.
pub async fn handle_gm_slash_ext(
    ctx: &ServerContext,
    session: &mut SessionState,
    cid: i64,
    cmd: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if cmd == "/heal" || cmd.starts_with("/heal ") {
        let arg_id = cmd
            .strip_prefix("/heal")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<i64>().ok());

        let (target_char_id, sys_id, target_ship_id) = ctx.db.with_snapshot(|snap| {
            let caller_sys = snap
                .characters
                .get(&cid)
                .map(|c| c.solar_system_id)
                .unwrap_or(30_000_142);
            let caller_ship = snap
                .characters
                .get(&cid)
                .map(|c| c.active_ship_id)
                .unwrap_or(0);
            if let Some(tid) = arg_id {
                if let Some(ch) = snap.characters.get(&tid) {
                    (tid, ch.solar_system_id, ch.active_ship_id)
                } else if let Some(item) = snap.items.get(&tid) {
                    let owner_sys = snap
                        .characters
                        .get(&item.owner_id)
                        .map(|c| c.solar_system_id)
                        .unwrap_or(caller_sys);
                    (item.owner_id, owner_sys, tid)
                } else {
                    (cid, caller_sys, tid)
                }
            } else {
                (cid, caller_sys, caller_ship)
            }
        });

        let mut combat_healed = CombatState::default();
        if target_ship_id > 0 {
            let handle = ctx.systems.get_or_spawn(sys_id);
            if let Ok(cs) = handle.heal_combat_state(target_ship_id).await {
                combat_healed = cs;
            }
        }

        // Clear module and hull dynamic damage + heat attributes in DB and fitting cache
        ctx.db.with_mut_snapshot(|snap| {
            for item in snap.items.values_mut() {
                if item.item_id == target_ship_id || item.location_id == target_ship_id {
                    item.dynamic_attributes.remove(&eve_sde::attr_ids::DAMAGE);
                    item.dynamic_attributes
                        .remove(&eve_sde::attr_ids::ARMOR_DAMAGE);
                    item.dynamic_attributes.remove(&1211u16); // heatDamage
                }
            }
        });
        if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(target_char_id, target_ship_id) {
            for m in fit.modules.values_mut() { m.damage_hp = 0.0; }
            fit.rack_heat = [0.0, 0.0, 0.0];
            ctx.store_ship_fitting(target_ship_id, fit);
        }
        let _ = ctx.notify_damage_state_change(crate::router::NotificationTarget::Character(target_char_id), target_ship_id, 1.0, 1.0, 1.0);
        let _ = ctx.notify_remote_message(crate::router::NotificationTarget::Character(target_char_id), "CustomInfo", vec![("info", PyRep::String("Ship healed".into()))]);

        return Some(Ok(PyRep::keyval([
            ("command", PyRep::String(cmd.to_string())),
            ("characterID", PyRep::Integer(target_char_id)),
            ("shipID", PyRep::Integer(target_ship_id)),
            ("targetID", PyRep::Integer(target_ship_id)),
            ("shieldHP", PyRep::Real(combat_healed.shield_hp)),
            ("armorHP", PyRep::Real(combat_healed.armor_hp)),
            ("hullHP", PyRep::Real(combat_healed.structure_hp)),
            ("capacitor", PyRep::Real(combat_healed.capacitor)),
            ("moduleHeatCleared", PyRep::Bool(true)),
            ("healed", PyRep::Bool(true)),
        ])));
    }

    if let Some(rest) = cmd.strip_prefix("/spawn ") {
        let mut parts = rest.split_whitespace();
        let Some(type_id) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /spawn <type_id> [quantity]".into(),
            }));
        };
        let qty = parts
            .next()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(1);
        if !(1..=100).contains(&qty) {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Quantity for /spawn must be between 1 and 100".into(),
            }));
        }
        let Some(inv_type) = ctx.sde.types.get(&type_id).cloned() else {
            return Some(Err(BridgeRpcError {
                code: "TypeNotFound".into(),
                message: format!("Type ID {type_id} not found in SDE"),
            }));
        };
        let (sys_id, ship_id) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&cid)
                    .map(|c| (c.solar_system_id, c.active_ship_id))
            })
            .unwrap_or((30_000_142, 0));

        let handle = ctx.systems.get_or_spawn(sys_id);
        let origin = if ship_id > 0 {
            handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO)
        } else {
            DVec3::ZERO
        };

        let mut spawned_ids = Vec::with_capacity(qty as usize);
        for idx in 0..qty {
            let entity_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.items.insert(
                    id,
                    InventoryItemRecord {
                        item_id: id,
                        type_id,
                        owner_id: cid,
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
                id
            });
            let pos = origin + DVec3::new(1_500.0 + (idx as f64) * 250.0, 0.0, 0.0);
            let ball = Ball {
                id: entity_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: inv_type.mass.max(1.0),
                radius: inv_type.radius.max(1.0),
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            let combat = ctx.build_type_combat_state(type_id);
            let meta = ctx.build_ship_slim_meta(cid, type_id);
            let _ = handle.spawn_ball_with_meta(ball, combat, meta).await;
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

    if let Some(rest) = cmd
        .strip_prefix("/unspawn ")
        .or_else(|| cmd.strip_prefix("/despawn "))
    {
        let Some(target_id) = rest.trim().parse::<i64>().ok().filter(|&id| id > 0) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /unspawn <entity_id>".into(),
            }));
        };
        let (sys_id, removed_db) = ctx.db.with_mut_snapshot(|snap| {
            let caller_sys = snap
                .characters
                .get(&cid)
                .map(|c| c.solar_system_id)
                .unwrap_or(30_000_142);
            let removed = snap.items.remove(&target_id).is_some();
            let children: Vec<i64> = snap.items.values().filter(|i| i.location_id == target_id).map(|i| i.item_id).collect();
            for child_id in children { snap.items.remove(&child_id); }
            (caller_sys, removed)
        });
        let handle = ctx.systems.get_or_spawn(sys_id);
        let removed_ball = handle.remove_ball(target_id).await.is_some();
        if removed_ball {
            crate::services::pve_incursion_sites::mark_incursion_boss_destroyed(ctx, sys_id, target_id);
        }
        if !removed_db && !removed_ball {
            return Some(Err(BridgeRpcError {
                code: "EntityNotFound".into(),
                message: format!("Entity {target_id} not found in space or inventory"),
            }));
        }
        return Some(Ok(PyRep::keyval([
            ("command", PyRep::String(cmd.to_string())),
            ("unspawnedID", PyRep::Integer(target_id)),
            ("removedFromSpace", PyRep::Bool(removed_ball)),
            ("removedFromDb", PyRep::Bool(removed_db)),
        ])));
    }

    if let Some(rest) = cmd.strip_prefix("/setattr ") {
        let mut parts = rest.split_whitespace();
        let Some(item_tok) = parts.next() else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /setattr <item_id|me> <attribute_id> <value>".into(),
            }));
        };
        let Some(attr_id) = parts.next().and_then(|s| s.parse::<u16>().ok()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /setattr <item_id|me> <attribute_id> <value>".into(),
            }));
        };
        let Some(val) = parts
            .next()
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
            let item_id = if item_tok.eq_ignore_ascii_case("me")
                || item_tok.eq_ignore_ascii_case("myship")
            {
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
                ctx.notify(
                    NotificationTarget::Character(owner_id),
                    "OnModuleAttributeChange",
                    "charid",
                    vec![
                        PyRep::Integer(owner_id),
                        PyRep::Integer(item_id),
                        PyRep::Integer(attr_id as i64),
                        PyRep::Integer(now_ft),
                        PyRep::Real(val),
                        PyRep::Real(old_val),
                    ],
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
        let mut parts = rest.split_whitespace();
        let Some(item_tok) = parts.next() else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /getattr <item_id|me> <attribute_id>".into(),
            }));
        };
        let Some(attr_id) = parts.next().and_then(|s| s.parse::<u16>().ok()) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /getattr <item_id|me> <attribute_id>".into(),
            }));
        };
        let res = ctx.db.with_snapshot(|snap| {
            let item_id = if item_tok.eq_ignore_ascii_case("me")
                || item_tok.eq_ignore_ascii_case("myship")
            {
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
        let tokens: Vec<&str> = rest.split_whitespace().collect();
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

        let (ship_id, type_id, explicit_flag) = if tokens[0].eq_ignore_ascii_case("me")
            || tokens[0].eq_ignore_ascii_case("myship")
        {
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
                    .is_some_and(|t| t.category_id == 6)
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

            let candidate_flags: &[InventoryFlag] = if effects.contains(&12) {
                &[
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
                &[
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
                &[
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
                &[
                    InventoryFlag::RigSlot0,
                    InventoryFlag::RigSlot1,
                    InventoryFlag::RigSlot2,
                ]
            } else if mod_type.category_id == 32 || effects.contains(&3772) {
                &[
                    InventoryFlag::SubSystemSlot0,
                    InventoryFlag::SubSystemSlot1,
                    InventoryFlag::SubSystemSlot2,
                    InventoryFlag::SubSystemSlot3,
                    InventoryFlag::SubSystemSlot4,
                ]
            } else if mod_type.category_id == 18 {
                &[InventoryFlag::DroneBay]
            } else {
                &[
                    InventoryFlag::HiSlot0,
                    InventoryFlag::HiSlot1,
                    InventoryFlag::MedSlot0,
                    InventoryFlag::LoSlot0,
                    InventoryFlag::Cargo,
                ]
            };

            let chosen_flag = explicit_flag.unwrap_or_else(|| {
                candidate_flags
                    .iter()
                    .copied()
                    .find(|f| !occupied_flags.contains(f))
                    .unwrap_or(candidate_flags[0])
            });

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
            snap.module_online_states.insert(item_id, true);
            Ok((rec, chosen_flag))
        });

        return match res {
            Ok((rec, chosen_flag)) => {
                ctx.invalidate_ship_fitting(ship_id);
                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("shipID", PyRep::Integer(ship_id)),
                    ("itemID", PyRep::Integer(rec.item_id)),
                    ("typeID", PyRep::Integer(type_id as i64)),
                    ("flagID", PyRep::Integer(chosen_flag as i64)),
                    ("online", PyRep::Bool(true)),
                ])))
            }
            Err(e) => Some(Err(e)),
        };
    }

    if let Some(rest) = cmd.strip_prefix("/kill ") {
        let Some(target_id) = rest.trim().parse::<i64>().ok().filter(|&id| id > 0) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidSlashArgument".into(),
                message: "Usage: /kill <target_id>".into(),
            }));
        };
        let lookup = ctx.db.with_snapshot(|snap| {
            let caller_sys = snap
                .characters
                .get(&cid)
                .map(|c| c.solar_system_id)
                .unwrap_or(30_000_142);
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
        let (victim_char_id, victim_ship_id, sys_id, in_db_items) =
            lookup.unwrap_or((cid, target_id, 30_000_142, false));
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
                            session.ship_id = Some(cap_id);
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
            let wreck_type_id = ctx.sde.wrecks.get(&ball.type_id).copied().unwrap_or(26557);
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
        ctx.notify(crate::router::NotificationTarget::Character(cid), "OnSetPlayerStanding", "charid", vec![PyRep::Integer(target_id), PyRep::Real(standing)]);
        return Some(Ok(PyRep::keyval([("contactID", PyRep::Integer(target_id)), ("standing", PyRep::Real(standing))])));
    }

    if cmd == "/downtime" || cmd.starts_with("/downtime ") {
        let rep = crate::services::wallet_serverops::run_daily_downtime_and_jobs(ctx);
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
    if cmd == "/survey" || cmd.starts_with("/survey ") {
        return crate::services::operations_manager::handle_survey_slash(ctx, cid, cmd);
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
        let (_, cid) = ServerContext::require_character(session).ok()?;
        if !session.has_gm_role() {
            return None;
        }
        let cmd = args.first().and_then(|a| a.as_str()).unwrap_or("").trim();
        return handle_gm_slash_ext(ctx, session, cid, cmd).await;
    }
    match (service, method) {
        ("publicQaToolsServer", "SlashCmd") => {
            let (_, cid) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let has_gm_or_qa = session.has_gm_role()
                || (session.role & eve_machonet::service_roles::ROLE_QA) != 0;
            if !has_gm_or_qa {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "publicQaToolsServer.SlashCmd requires GM/QA role".into(),
                }));
            }
            let cmd = args.first().and_then(|a| a.as_str()).unwrap_or("").trim();
            if session.has_gm_role() {
                if let Some(ext_res) = handle_gm_slash_ext(ctx, session, cid, cmd).await {
                    return Some(ext_res);
                }
            } else if cmd.starts_with("/moveme ")
                || cmd.starts_with("/translocate ")
                || cmd == "/online"
                || cmd.starts_with("/online ")
                || cmd == "/survey"
                || cmd.starts_with("/survey ")
            {
                if let Some(ext_res) = handle_gm_slash_ext(ctx, session, cid, cmd).await {
                    return Some(ext_res);
                }
            }
            let _ = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&cid));
            Some(Err(BridgeRpcError {
                code: "UnknownSlashCommand".into(),
                message: format!("Unknown or unsupported QA slash command '{cmd}'"),
            }))
        }
        ("publicQaToolsServer", "MoveMeTo") => {
            let (_, cid) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let has_gm_or_qa = session.has_gm_role()
                || (session.role & eve_machonet::service_roles::ROLE_QA) != 0;
            if !has_gm_or_qa {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "publicQaToolsServer.MoveMeTo requires GM/QA role".into(),
                }));
            }
            let dest_raw = args
                .first()
                .and_then(|a| a.as_i64().or_else(|| a.as_str().and_then(|s| s.trim().parse::<i64>().ok())))
                .unwrap_or(0);
            let dest_sys_id = if ctx.sde.solar_systems.contains_key(&(dest_raw as u32)) {
                dest_raw as u32
            } else if let Some(st) = ctx.sde.stations.get(&dest_raw) {
                st.solar_system_id
            } else {
                return Some(Err(BridgeRpcError {
                    code: "DestinationNotFound".into(),
                    message: format!("Destination {dest_raw} not found in SDE"),
                }));
            };
            let (old_sys_id, ship_id) = ctx
                .db
                .with_snapshot(|snap| snap.characters.get(&cid).map(|c| (c.solar_system_id, c.active_ship_id)))
                .unwrap_or((30_000_142, 0));
            let updated_ch = match ctx.db.set_character_solar_system(cid, dest_sys_id, &ctx.sde) {
                Ok(c) => c,
                Err(e) => return Some(Err(BridgeRpcError { code: "TeleportFailed".into(), message: e.to_string() })),
            };
            if old_sys_id != dest_sys_id && ship_id > 0 {
                let old_handle = ctx.systems.get_or_spawn(old_sys_id);
                let existing_ball = old_handle.remove_ball(ship_id).await;
                let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
                let mut new_ball = existing_ball.unwrap_or(Ball {
                    id: ship_id,
                    type_id: 601,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 0.5,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                });
                new_ball.position = DVec3::new(10_000.0, 0.0, 0.0);
                new_ball.velocity = DVec3::ZERO;
                let combat_state = ctx.build_ship_combat_state(cid, ship_id, new_ball.type_id);
                let slim_meta = ctx.build_ship_slim_meta(cid, new_ball.type_id);
                let _ = new_handle.spawn_ball_with_meta(new_ball, combat_state, slim_meta).await;
            }
            session.solar_system_id = Some(dest_sys_id);
            session.constellation_id = Some(updated_ch.constellation_id);
            session.region_id = Some(updated_ch.region_id);
            session.station_id = None;
            session.structure_id = None;
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(cid)),
                ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("constellationID", PyRep::Integer(updated_ch.constellation_id as i64)),
                ("regionID", PyRep::Integer(updated_ch.region_id as i64)),
                ("moved", PyRep::Bool(true)),
            ])))
        }
        ("publicQaToolsServer", "CanGiveItemForMultifit") => {
            let (_, cid) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_exists = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&cid));
            let has_gm_or_qa = session.has_gm_role()
                || (session.role & eve_machonet::service_roles::ROLE_QA) != 0;
            Some(Ok(PyRep::Bool(char_exists && has_gm_or_qa)))
        }
        _ => None,
    }
}
