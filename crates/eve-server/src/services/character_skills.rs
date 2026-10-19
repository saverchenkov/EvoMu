//! `character_skills` — Domain 2: Character Creation, Lineage, Clones, Implants, Skill Queue, & Standings (`§3.2`).

#![forbid(unsafe_code)]

use crate::formulas::{
    apply_security_tag_repair, clone_jump_cooldown_seconds, skill_injector_sp_yield,
    skill_training_time_seconds, sp_for_skill_level,
};
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, IskCents, JumpCloneRecord, SkillHistoryEntry, SkillQueueEntry,
    TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use eve_sde::{attr_ids, TypeId};
use std::collections::HashMap;

/// Canonical jump clone installation fee at NPC stations (`900,000 ISK` per `Jump_clones.md:20, 54`; `R23-P1-2`).
pub fn jump_clone_install_fee_cents() -> IskCents {
    IskCents::from_isk(900_000)
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
    if lower.starts_with("ccp ")
        || lower.starts_with("ccp-")
        || lower == "ccp"
        || lower.starts_with("gm ")
        || lower.starts_with("isd ")
        || lower.starts_with("dev ")
    {
        return false;
    }
    for banned in [
        "admin",
        "moderator",
        "concord",
        "fuck",
        "shit",
        "hitler",
        "nazi",
    ] {
        if lower.contains(banned) {
            return false;
        }
    }
    true
}

/// DERIVED: Fallback first and family name pools per bloodline when SDE agent names are unavailable (`feat:character.random-name`).
/// CCP's server-side random character name tables are not shipped in the client staticdata archives.
pub fn bloodline_name_pools(bloodline_id: u32) -> (&'static [&'static str], &'static [&'static str]) {
    match bloodline_id {
        // Caldari: Deteis (1), Civire (2), Achura (11)
        1 => (
            &["Tibus", "Kari", "Matti", "Tuomas", "Eetu", "Sanna", "Ilmari", "Aino"],
            &["Heth", "Kallio", "Lindqvist", "Koskinen", "Hakkarainen", "Nieminen", "Vaananen", "Lehtonen"],
        ),
        2 => (
            &["Kael", "Vesper", "Corin", "Bren", "Talia", "Jarek", "Mira", "Rhea"],
            &["Mercer", "Stark", "Kruger", "Holloway", "Vance", "Blackwood", "Keller", "Cross"],
        ),
        11 => (
            &["Kenji", "Akira", "Sora", "Yuki", "Ren", "Haruki", "Kaede", "Mei"],
            &["Takahashi", "Watanabe", "Kobayashi", "Ito", "Saito", "Yamamoto", "Nakamura", "Kato"],
        ),
        // Minmatar: Sebiestor (3), Brutor (4), Vherokior (14)
        3 => (
            &["Toran", "Sari", "Nesta", "Valen", "Eirik", "Freja", "Sigurd", "Astrid"],
            &["Kouris", "Solari", "Nilsen", "Solberg", "Hagen", "Bakke", "Lund", "Strand"],
        ),
        4 => (
            &["Karishal", "Malaetu", "Krag", "Vuk", "Dragan", "Zoran", "Milena", "Tara"],
            &["Shakor", "Saraki", "Novak", "Kovac", "Horvat", "Babic", "Jovanovic", "Vukic"],
        ),
        14 => (
            &["Temujin", "Altai", "Batu", "Chotan", "Sarnai", "Zaya", "Nergui", "Oyun"],
            &["Khasar", "Borjigin", "Chonos", "Olkhunut", "Jalayir", "Taidjut", "Merkit", "Tatar"],
        ),
        // Amarr: Amarr (5), Ni-Kunni (6), Khanid (13)
        5 => (
            &["Jamyl", "Catiz", "Aritcio", "Heideran", "Uriam", "Doriam", "Maximilian", "Aurelia"],
            &["Sarum", "Tash-Murkon", "Kor-Azor", "Kador", "Ardishapur", "Khanid", "Valate", "Dacella"],
        ),
        6 => (
            &["Rashid", "Tariq", "Nadia", "Layla", "Zayd", "Faris", "Samira", "Yasmin"],
            &["Al-Maktoum", "Qadir", "Sarkis", "Mansoor", "Hakim", "Nasser", "Bakr", "Zaman"],
        ),
        13 => (
            &["Garkeh", "Darius", "Cyrus", "Khosrow", "Shirin", "Roxana", "Bahram", "Azadeh"],
            &["Khanid", "Ravan", "Pahlavi", "Sasan", "Arshak", "Karen", "Mihran", "Suren"],
        ),
        // Gallente: Gallente (7), Intaki (8), Jin-Mei (12)
        7 => (
            &["Jacus", "Souro", "Celes", "Laurent", "Camille", "Etienne", "Manon", "Luc"],
            &["Roden", "Foiritan", "Aguard", "Moreau", "Laurent", "Dubois", "Mercier", "Blanc"],
        ),
        8 => (
            &["Vindhya", "Aarav", "Priya", "Kiran", "Dev", "Ananya", "Rohan", "Meera"],
            &["Isu", "Chatterjee", "Sen", "Bose", "Mukherjee", "Dasgupta", "Banerjee", "Roy"],
        ),
        12 => (
            &["Liang", "Wei", "Chen", "Ming", "Xia", "Jian", "Hua", "Zhen"],
            &["Zhang", "Wang", "Liu", "Huang", "Zhao", "Wu", "Zhou", "Xu"],
        ),
        _ => (
            &["Alexander", "Lyra", "Orion", "Selene", "Caelum", "Aethel", "Elena", "Vesper"],
            &["Vander", "Artemis", "Vandermeer", "O'Neill", "Solari", "Mercer", "Kallio", "Stark"],
        ),
    }
}

/// Returns the maximum trainable/active skill level for an Alpha clone (`grade 1`) from `clonegrades.fsdbinary`.
pub fn alpha_skill_cap_for_character(
    sde: &eve_sde::SdeRegistry,
    race_id: u32,
    skill_type_id: TypeId,
) -> u8 {
    sde.clone_grades
        .get(&race_id)
        .and_then(|cg| cg.alpha_skill_caps.get(&skill_type_id).copied())
        .or_else(|| {
            sde.clone_grades
                .values()
                .filter_map(|cg| cg.alpha_skill_caps.get(&skill_type_id).copied())
                .max()
        })
        .unwrap_or(0)
}

fn serialize_doll_pyrep(rep: &PyRep) -> String {
    match rep {
        PyRep::String(s) => s.clone(),
        PyRep::Integer(v) => v.to_string(),
        PyRep::Real(v) => v.to_string(),
        PyRep::Bool(v) => v.to_string(),
        PyRep::None => String::new(),
        PyRep::Tuple(items) | PyRep::List(items) => {
            let parts: Vec<String> = items.iter().map(serialize_doll_pyrep).collect();
            format!("[{}]", parts.join(","))
        }
        PyRep::KeyVal(map) => {
            let mut entries: Vec<(&String, &PyRep)> = map.iter().collect();
            entries.sort_by_key(|(k, _)| *k);
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| format!("{k}={}", serialize_doll_pyrep(v)))
                .collect();
            parts.join("&")
        }
        PyRep::Dict(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}={}", serialize_doll_pyrep(k), serialize_doll_pyrep(v)))
                .collect();
            parts.join("&")
        }
        _ => String::from("paperdoll"),
    }
}

pub fn handle(
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

        ("charUnboundMgr", "GetCharacterInfo") => {
            let user_id = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let chars = ctx.db.get_characters_for_user(user_id);
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id);
            let Some(cid) = target_char_id else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "GetCharacterInfo requires an explicit charID argument or selected character".into(),
                }));
            };
            let Some(c) = chars.into_iter().find(|ch| ch.character_id == cid) else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipMismatch".into(),
                    message: format!("Character {cid} does not belong to user {user_id}"),
                }));
            };
            let sheet = ctx.db.with_snapshot(|snap| {
                let (ship_type_id, ship_name) = snap
                    .items
                    .get(&c.active_ship_id)
                    .map(|item| {
                        let nm = snap
                            .item_custom_names
                            .get(&item.item_id)
                            .cloned()
                            .or_else(|| ctx.esi_state.get_custom_item_name(item.item_id))
                            .or_else(|| ctx.sde.types.get(&item.type_id).map(|t| t.name.clone()))
                            .unwrap_or_else(|| format!("Ship {}", item.type_id));
                        (item.type_id as i64, nm)
                    })
                    .unwrap_or((0, String::new()));
                let (doll_dna, portrait_meta) = snap
                    .character_doll_info
                    .get(&cid)
                    .cloned()
                    .unwrap_or_else(|| (format!("bloodline:{}", c.bloodline_id), "portrait:default".to_string()));
                let queue = snap.skill_queues.get(&cid).cloned().unwrap_or_default();
                let (training_skill_id, training_level, training_start_sp, training_dest_sp) = queue
                    .first()
                    .map(|q| (q.type_id as i64, q.target_level as i64, q.start_sp, q.end_sp))
                    .unwrap_or((0, 0, 0.0, 0.0));
                let mut skills_list = Vec::new();
                if let Some(smap) = snap.skills.get(&cid) {
                    let mut sorted: Vec<_> = smap.values().collect();
                    sorted.sort_by_key(|s| s.type_id);
                    for s in sorted {
                        let active_lvl = if c.clone_grade <= 1 {
                            s.level.min(alpha_skill_cap_for_character(&ctx.sde, c.race_id, s.type_id))
                        } else {
                            s.level
                        };
                        skills_list.push(PyRep::keyval([
                            ("typeID", PyRep::Integer(s.type_id as i64)),
                            ("skillLevel", PyRep::Integer(s.level as i64)),
                            ("activeSkillLevel", PyRep::Integer(active_lvl as i64)),
                            ("skillPoints", PyRep::Real(s.skill_points)),
                        ]));
                    }
                }
                let implants_list: Vec<PyRep> = snap
                    .active_implants
                    .get(&cid)
                    .map(|v| v.iter().map(|&tid| PyRep::Integer(tid as i64)).collect())
                    .unwrap_or_default();
                let jump_clones_list: Vec<PyRep> = snap
                    .jump_clones
                    .get(&cid)
                    .map(|v| {
                        v.iter()
                            .map(|jc| {
                                PyRep::keyval([
                                    ("jumpCloneID", PyRep::Integer(jc.jump_clone_id)),
                                    ("locationID", PyRep::Integer(jc.location_id)),
                                    (
                                        "implants",
                                        PyRep::List(
                                            jc.implants.iter().map(|&t| PyRep::Integer(t as i64)).collect(),
                                        ),
                                    ),
                                ])
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut standings_entries: Vec<_> = c.standings.iter().collect();
                standings_entries.sort_by_key(|(&fid, _)| fid);
                let standings_list: Vec<PyRep> = standings_entries
                    .into_iter()
                    .map(|(&from_id, &val)| {
                        PyRep::keyval([
                            ("fromID", PyRep::Integer(from_id)),
                            ("standing", PyRep::Real(val)),
                        ])
                    })
                    .collect();
                let kill_rights_list: Vec<PyRep> = snap
                    .kill_rights
                    .values()
                    .filter(|kr| kr.owner_id == cid || kr.target_id == cid)
                    .map(|kr| {
                        PyRep::keyval([
                            ("killRightID", PyRep::Integer(kr.kill_right_id)),
                            ("fromID", PyRep::Integer(kr.owner_id)),
                            ("toID", PyRep::Integer(kr.target_id)),
                            ("isActiveSuspect", PyRep::Bool(kr.is_active_suspect)),
                            ("price", PyRep::Real(kr.activation_fee.to_wire_f64())),
                        ])
                    })
                    .collect();
                let attrs_kv = PyRep::keyval([
                    ("charisma", PyRep::Integer(c.neural.charisma as i64)),
                    ("intelligence", PyRep::Integer(c.neural.intelligence as i64)),
                    ("memory", PyRep::Integer(c.neural.memory as i64)),
                    ("perception", PyRep::Integer(c.neural.perception as i64)),
                    ("willpower", PyRep::Integer(c.neural.willpower as i64)),
                    ("bonusRemaps", PyRep::Integer(c.neural.bonus_remaps as i64)),
                    (
                        "lastRemapDate",
                        c.neural.last_remap_unix.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None),
                    ),
                    (
                        "accruedRemapCooldownDate",
                        c.neural
                            .remap_cooldown_until_unix
                            .map(|u| PyRep::Integer(u as i64))
                            .unwrap_or(PyRep::None),
                    ),
                ]);

                PyRep::keyval([
                    ("characterID", PyRep::Integer(c.character_id)),
                    ("characterName", PyRep::String(c.name)),
                    ("corporationID", PyRep::Integer(c.corporation_id)),
                    (
                        "allianceID",
                        c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    ("stationID", PyRep::Integer(c.station_id.unwrap_or(0))),
                    (
                        "structureID",
                        c.structure_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                    ("solarSystemID", PyRep::Integer(c.solar_system_id as i64)),
                    ("constellationID", PyRep::Integer(c.constellation_id as i64)),
                    ("regionID", PyRep::Integer(c.region_id as i64)),
                    ("raceID", PyRep::Integer(c.race_id as i64)),
                    ("bloodlineID", PyRep::Integer(c.bloodline_id as i64)),
                    ("ancestryID", PyRep::Integer(c.ancestry_id as i64)),
                    ("schoolID", PyRep::Integer(c.school_id as i64)),
                    ("gender", PyRep::Integer(c.gender as i64)),
                    ("createDateTime", PyRep::Integer(c.create_date_unix as i64)),
                    ("balance", PyRep::Real(c.balance.to_wire_f64())),
                    ("skillPoints", PyRep::Real(c.skill_points)),
                    ("freeSkillPoints", PyRep::Real(c.free_skill_points)),
                    ("securityStatus", PyRep::Real(c.security_status)),
                    ("cloneGrade", PyRep::Integer(c.clone_grade as i64)),
                    ("shipID", PyRep::Integer(c.active_ship_id)),
                    ("shipTypeID", PyRep::Integer(ship_type_id)),
                    ("shipName", PyRep::String(ship_name)),
                    ("dollInfo", PyRep::String(doll_dna.clone())),
                    ("paperdollState", PyRep::String(doll_dna)),
                    ("portraitInfo", PyRep::String(portrait_meta)),
                    ("trainingSkillTypeID", PyRep::Integer(training_skill_id)),
                    ("trainingToLevel", PyRep::Integer(training_level)),
                    ("trainingStartSP", PyRep::Real(training_start_sp)),
                    ("trainingDestinationSP", PyRep::Real(training_dest_sp)),
                    ("skillQueueLength", PyRep::Integer(queue.len() as i64)),
                    ("skills", PyRep::List(skills_list)),
                    ("attributes", attrs_kv),
                    ("implants", PyRep::List(implants_list)),
                    ("jumpClones", PyRep::List(jump_clones_list)),
                    ("standings", PyRep::List(standings_list)),
                    ("killRights", PyRep::List(kill_rights_list)),
                    ("medals", PyRep::List(Vec::new())),
                    ("decorations", PyRep::List(Vec::new())),
                ])
            });
            Some(Ok(sheet))
        }

        ("charUnboundMgr", "ValidateNameEx") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
            let valid = is_valid_character_name_syntax(name)
                && ctx.db.with_snapshot(|s| {
                    !s.characters
                        .values()
                        .any(|c| c.name.eq_ignore_ascii_case(name))
                });
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
            if !is_valid_character_name_syntax(name) {
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
                    code: "InvalidPortraitInfo".into(),
                    message: "Missing portraitInfo argument".into(),
                }));
            };
            if matches!(char_info_rep, PyRep::None) || matches!(portrait_info_rep, PyRep::None) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDollInfo".into(),
                    message: "charInfo and portraitInfo must not be None".into(),
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

            let c = match ctx.db.select_character_for_user(user_id, char_id) {
                Ok(ch) => ch,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "CharacterSelectionDenied".into(),
                        message: e.to_string(),
                    }))
                }
            };

            ctx.release_session_bindings(session.session_id);
            if ctx
                .db
                .get_online_session(user_id)
                .is_some_and(|os| os.session_id == session.session_id)
            {
                let _ = ctx.db.acquire_online_lock(
                    user_id,
                    session.session_id,
                    Some(c.character_id),
                    false,
                    ctx.now_unix(),
                );
            }
            session.character_id = Some(c.character_id);
            session.corporation_id = Some(c.corporation_id);
            session.alliance_id = c.alliance_id;
            session.war_faction_id = c.war_faction_id;
            session.station_id = c.station_id;
            session.structure_id = c.structure_id;
            session.solar_system_id = Some(c.solar_system_id);
            session.constellation_id = Some(c.constellation_id);
            session.region_id = Some(c.region_id);
            session.ship_id = Some(c.active_ship_id);
            session.race_id = Some(c.race_id);

            ctx.sync_character_local_chat(c.character_id, None, c.solar_system_id);

            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                ("shipID", PyRep::Integer(c.active_ship_id)),
                ("solarSystemID", PyRep::Integer(c.solar_system_id as i64)),
            ])))
        }

        ("skillMgr2", "GetSkills") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.skills
                    .get(&char_id)
                    .map(|m| {
                        m.values()
                            .map(|s| {
                                vec![
                                    PyRep::Integer(s.type_id as i64),
                                    PyRep::Integer(s.level as i64),
                                    PyRep::Real(s.skill_points),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec!["typeID", "skillLevel", "skillPoints"],
                rows,
            )))
        }

        ("skillMgr2", "GetSkillQueue") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.skill_queues
                    .get(&char_id)
                    .map(|q| {
                        q.iter()
                            .map(|e| {
                                vec![
                                    PyRep::Integer(e.type_id as i64),
                                    PyRep::Integer(e.target_level as i64),
                                    PyRep::Real(e.start_sp),
                                    PyRep::Real(e.end_sp),
                                    PyRep::Real(e.duration_seconds),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "trainingTypeID",
                    "trainingToLevel",
                    "trainingStartSP",
                    "trainingDestinationSP",
                    "durationSeconds",
                ],
                rows,
            )))
        }

        ("skillMgr2", "SaveSkillQueue") | ("skillMgr2", "AddToEndOfSkillQueue") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_add_to_end = method == "AddToEndOfSkillQueue";

            let mut skills_to_queue: Vec<(TypeId, u8)> = Vec::new();
            match args.first() {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => {
                    for it in items {
                        match it {
                            PyRep::Tuple(pair) | PyRep::List(pair) => {
                                if let Some(tid) = pair.first().and_then(|a| a.as_i64()) {
                                    let lvl = pair.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
                                    skills_to_queue.push((tid as TypeId, lvl.clamp(1, 5) as u8));
                                }
                            }
                            single_val => {
                                if let Some(tid) = single_val.as_i64() {
                                    let lvl = items.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
                                    skills_to_queue.push((tid as TypeId, lvl.clamp(1, 5) as u8));
                                    break;
                                }
                            }
                        }
                    }
                }
                _ => {
                    if let Some(tid) = args.first().and_then(|a| a.as_i64()) {
                        let lvl = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
                        skills_to_queue.push((tid as TypeId, lvl.clamp(1, 5) as u8));
                    }
                }
            }

            if skills_to_queue.is_empty() {
                if !is_add_to_end {
                    ctx.db.with_mut_snapshot(|snap| {
                        snap.skill_queues.insert(char_id, Vec::new());
                        snap.paused_skill_queues.insert(char_id);
                    });
                    let _ = ctx.notify_skill_queue_paused(char_id);
                    return Some(Ok(PyRep::List(vec![])));
                } else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillType".into(),
                        message: "AddToEndOfSkillQueue requires an explicit skill typeID".into(),
                    }));
                }
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };

                if !is_add_to_end {
                    snap.skill_queues.insert(char_id, Vec::new());
                    snap.paused_skill_queues.remove(&char_id);
                }

                let mut last_ret = (0 as TypeId, 1 as u8, 0.0, 0.0);

                for (skill_type_id, target_level) in skills_to_queue {
                    let Some(skill_type) = ctx.sde.types.get(&skill_type_id) else {
                        return Err(BridgeRpcError {
                            code: "InvalidSkillType".into(),
                            message: format!("Skill type {skill_type_id} not found in SDE"),
                        });
                    };
                    if skill_type.category_id != 16 {
                        return Err(BridgeRpcError {
                            code: "InvalidSkillType".into(),
                            message: format!("Type {skill_type_id} is not a skill (category 16)"),
                        });
                    }

                    let rank = skill_type
                        .default_attributes
                        .get(&attr_ids::SKILL_TIME_CONSTANT)
                        .copied()
                        .unwrap_or(1.0);
                    let end_sp = sp_for_skill_level(rank, target_level);

                    // Enforce skill prerequisites
                    for (skill_attr, level_attr) in [
                        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
                        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
                        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
                    ] {
                        if let Some(&req_skill_f) = skill_type.default_attributes.get(&skill_attr) {
                            let req_skill_id = req_skill_f as TypeId;
                            if req_skill_id > 0 {
                                let req_lvl = skill_type
                                    .default_attributes
                                    .get(&level_attr)
                                    .copied()
                                    .unwrap_or(1.0)
                                    .max(1.0) as u8;
                                let trained_lvl = snap
                                    .skills
                                    .get(&char_id)
                                    .and_then(|m| m.get(&req_skill_id))
                                    .map(|s| s.level)
                                    .unwrap_or(0);
                                let queued_lvl = snap
                                    .skill_queues
                                    .get(&char_id)
                                    .and_then(|q| {
                                        q.iter()
                                            .filter(|e| e.type_id == req_skill_id)
                                            .map(|e| e.target_level)
                                            .max()
                                    })
                                    .unwrap_or(0);
                                if trained_lvl.max(queued_lvl) < req_lvl {
                                    return Err(BridgeRpcError {
                                        code: "SkillPrerequisiteNotMet".into(),
                                        message: format!(
                                            "Skill {} (type {skill_type_id}) requires prerequisite skill {req_skill_id} at level {req_lvl}",
                                            skill_type.name
                                        ),
                                    });
                                }
                            }
                        }
                    }

                    let current_lvl = snap
                        .skills
                        .get(&char_id)
                        .and_then(|m| m.get(&skill_type_id))
                        .map(|s| s.level)
                        .unwrap_or(0);
                    let (max_queued_lvl, max_queued_sp) = snap
                        .skill_queues
                        .get(&char_id)
                        .and_then(|q| {
                            q.iter()
                                .filter(|e| e.type_id == skill_type_id)
                                .max_by_key(|e| e.target_level)
                                .map(|e| (e.target_level, Some(e.end_sp)))
                        })
                        .unwrap_or((0, None));
                    let effective_base_lvl = current_lvl.max(max_queued_lvl);
                    if target_level <= effective_base_lvl {
                        return Err(BridgeRpcError {
                            code: "SkillAlreadyTrained".into(),
                            message: format!(
                                "Skill {} (type {skill_type_id}) is already trained or queued to level {effective_base_lvl}",
                                skill_type.name
                            ),
                        });
                    }

                    let start_sp = max_queued_sp.unwrap_or_else(|| {
                        snap.skills
                            .get(&char_id)
                            .and_then(|m| m.get(&skill_type_id))
                            .map(|s| s.skill_points)
                            .unwrap_or(0.0)
                    });
                    let sp_delta = (end_sp - start_sp).max(0.0);
                    let queued_sp: f64 = snap
                        .skill_queues
                        .get(&char_id)
                        .map(|q| q.iter().map(|e| (e.end_sp - e.start_sp).max(0.0)).sum())
                        .unwrap_or(0.0);

                    // Enforce per-skill Alpha Clone level cap from clonegrades.fsdbinary and 5,000,000 SP total cap (grade 1)
                    if ch.clone_grade <= 1 {
                        let alpha_cap =
                            alpha_skill_cap_for_character(&ctx.sde, ch.race_id, skill_type_id);
                        if target_level > alpha_cap {
                            return Err(BridgeRpcError {
                                code: "AlphaCloneSkillLevelCapExceeded".into(),
                                message: format!(
                                    "Alpha clones cannot train {} (type {skill_type_id}) to level {target_level} (Alpha cap is level {alpha_cap})",
                                    skill_type.name
                                ),
                            });
                        }
                        if ch.skill_points + ch.free_skill_points + queued_sp + sp_delta > 5_000_000.0
                        {
                            return Err(BridgeRpcError {
                                code: "AlphaCloneSpCapExceeded".into(),
                                message:
                                    "Alpha clones cannot queue skills beyond 5,000,000 SP".into(),
                            });
                        }
                    }

                    let primary_attr_id = skill_type
                        .default_attributes
                        .get(&eve_sde::attr_ids::PRIMARY_ATTRIBUTE)
                        .copied()
                        .map(|v| v as u32)
                        .unwrap_or(eve_sde::attr_ids::PERCEPTION as u32);
                    let secondary_attr_id = skill_type
                        .default_attributes
                        .get(&eve_sde::attr_ids::SECONDARY_ATTRIBUTE)
                        .copied()
                        .map(|v| v as u32)
                        .unwrap_or(eve_sde::attr_ids::WILLPOWER as u32);
                    let primary_attr = ch.neural.get_attribute_by_id(primary_attr_id);
                    let secondary_attr = ch.neural.get_attribute_by_id(secondary_attr_id);
                    let clone_mult = if ch.clone_grade == 2 { 1.0 } else { 0.5 };
                    let duration_sec = skill_training_time_seconds(
                        start_sp,
                        end_sp,
                        primary_attr,
                        secondary_attr,
                        clone_mult,
                    );
                    snap.skill_queues
                        .entry(char_id)
                        .or_default()
                        .push(SkillQueueEntry {
                            type_id: skill_type_id,
                            target_level,
                            start_sp,
                            end_sp,
                            duration_seconds: duration_sec,
                        });
                    last_ret = (skill_type_id, target_level, end_sp, duration_sec);
                }
                Ok(last_ret)
            });

            match res {
                Ok((tid, lvl, esp, dur)) => {
                    let _ = ctx.notify_skill_queue_changed(char_id);
                    Some(Ok(PyRep::keyval([
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("targetLevel", PyRep::Integer(lvl as i64)),
                        ("endSP", PyRep::Real(esp)),
                        ("durationSeconds", PyRep::Real(dur)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("skillMgr2", "ApplyFreeSkillPoints") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let requested_sp_opt = args.first().and_then(|a| a.as_f64());
            if let Some(req_sp) = requested_sp_opt {
                if !req_sp.is_finite() || req_sp <= 0.0 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidSkillPointAmount".into(),
                        message: "Requested free skill points to apply must be finite and positive"
                            .into(),
                    }));
                }
            }
            let now_ft = ctx.now_blueos_filetime_ticks();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let available_free_sp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.free_skill_points)
                    .unwrap_or(0.0);
                if available_free_sp <= 0.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFreeSP".into(),
                        message: "No unallocated free skill points available to apply".into(),
                    });
                }

                let mut remaining_budget = requested_sp_opt
                    .map(|req| available_free_sp.min(req))
                    .unwrap_or(available_free_sp);
                let queue = snap.skill_queues.entry(char_id).or_default().clone();
                let mut remaining_queue = Vec::new();
                let mut history_to_append = Vec::new();
                let skills_map = snap.skills.entry(char_id).or_default();
                let mut total_applied = 0.0;

                for mut q in queue {
                    let needed = (q.end_sp - q.start_sp).max(0.0);
                    if remaining_budget >= needed && needed > 0.0 {
                        remaining_budget -= needed;
                        total_applied += needed;
                        skills_map.insert(
                            q.type_id,
                            TrainedSkillRecord {
                                type_id: q.type_id,
                                level: q.target_level,
                                skill_points: q.end_sp,
                            },
                        );
                        history_to_append.push(SkillHistoryEntry {
                            log_date_ft: now_ft,
                            // 307 = SkillEventSkillPointsApplied (evemu_Crucible EVE_Skills.h:35)
                            event_type_id: 307,
                            skill_type_id: q.type_id,
                            skill_level: q.target_level,
                            absolute_points: q.end_sp,
                        });
                    } else if remaining_budget > 0.0 && needed > 0.0 {
                        let applied = remaining_budget;
                        remaining_budget = 0.0;
                        total_applied += applied;
                        q.start_sp += applied;
                        q.duration_seconds *= ((q.end_sp - q.start_sp).max(0.0)) / needed;
                        let current_lvl = skills_map.get(&q.type_id).map(|s| s.level).unwrap_or(0);
                        let rank = ctx
                            .sde
                            .types
                            .get(&q.type_id)
                            .and_then(|t| {
                                t.default_attributes
                                    .get(&attr_ids::SKILL_TIME_CONSTANT)
                                    .copied()
                            })
                            .unwrap_or(1.0);
                        let mut achieved_lvl = current_lvl;
                        for lvl in (current_lvl + 1)..=q.target_level {
                            if q.start_sp + 0.5 >= sp_for_skill_level(rank, lvl) {
                                achieved_lvl = lvl;
                            } else {
                                break;
                            }
                        }
                        skills_map.insert(
                            q.type_id,
                            TrainedSkillRecord {
                                type_id: q.type_id,
                                level: achieved_lvl,
                                skill_points: q.start_sp,
                            },
                        );
                        history_to_append.push(SkillHistoryEntry {
                            log_date_ft: now_ft,
                            // 307 = SkillEventSkillPointsApplied (evemu_Crucible EVE_Skills.h:35)
                            event_type_id: 307,
                            skill_type_id: q.type_id,
                            skill_level: achieved_lvl,
                            absolute_points: q.start_sp,
                        });
                        remaining_queue.push(q);
                    } else {
                        remaining_queue.push(q);
                    }
                }
                snap.skill_queues.insert(char_id, remaining_queue);
                if !history_to_append.is_empty() {
                    snap.skill_history
                        .entry(char_id)
                        .or_default()
                        .extend(history_to_append);
                }

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.free_skill_points = (ch.free_skill_points - total_applied).max(0.0);
                    ch.skill_points += total_applied;
                    return Ok(ch.skill_points);
                }
                Ok(0.0)
            });
            match res {
                Ok(sp) => {
                    if let Some(sid) = session.ship_id {
                        ctx.invalidate_ship_fitting(sid);
                    }
                    Some(Ok(PyRep::Real(sp)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("skillMgr2", "InjectSkillPoints") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Locate an owned, unlocked, reachable Large Skill Injector (type_id 40520) with quantity >= 1
                let injector_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.type_id == 40520
                            && i.quantity >= 1
                            && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                    })
                    .map(|i| i.item_id);
                let Some(item_id) = injector_item_id else {
                    return Err(BridgeRpcError {
                        code: "InjectorNotFound".into(),
                        message: "Character does not own a Skill Injector (typeID 40520)".into(),
                    });
                };

                if let Some(item) = snap.items.get_mut(&item_id) {
                    if item.quantity > 1 {
                        item.quantity -= 1;
                        item.version += 1;
                    } else {
                        snap.items.remove(&item_id);
                    }
                }

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    let y = skill_injector_sp_yield(ch.skill_points);
                    ch.free_skill_points += y;
                    Ok(y)
                } else {
                    Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })
                }
            });
            match res {
                Ok(yielded) => Some(Ok(PyRep::Real(yielded))),
                Err(e) => Some(Err(e)),
            }
        }

        ("skillMgr2", "PauseSkillQueue")
        | ("skillMgr2", "CharStopTrainingSkill")
        | ("skillMgr2", "PauseTraining") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ctx.db.with_mut_snapshot(|snap| {
                snap.paused_skill_queues.insert(char_id);
            });
            let _ = ctx.notify_skill_queue_paused(char_id);
            Some(Ok(PyRep::Bool(true)))
        }

        ("skillMgr2", "ResumeSkillQueue")
        | ("skillMgr2", "CharStartTrainingSkill")
        | ("skillMgr2", "ResumeTraining") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ctx.db.with_mut_snapshot(|snap| {
                snap.paused_skill_queues.remove(&char_id);
            });
            let _ = ctx.notify_skill_queue_changed(char_id);
            Some(Ok(PyRep::Bool(true)))
        }

        ("skillMgr2", "GetSkillHistory") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                let mut out: Vec<Vec<PyRep>> = Vec::new();
                if let Some(events) = snap.skill_history.get(&char_id) {
                    for ev in events {
                        out.push(vec![
                            PyRep::Integer(ev.log_date_ft),
                            PyRep::Integer(ev.event_type_id),
                            PyRep::Integer(ev.skill_type_id as i64),
                            PyRep::Integer(ev.skill_level as i64),
                            PyRep::Real(ev.absolute_points),
                        ]);
                    }
                }
                out
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "logDate",
                    "eventTypeID",
                    "skillTypeID",
                    "skillLevel",
                    "absolutePoints",
                ],
                rows,
            )))
        }

        ("skillMgr2", "InjectSkillIntoBrain") | ("skillMgr2", "InjectSkill") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;
            let raw_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if raw_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "InjectSkillIntoBrain requires itemID or skill typeID".into(),
                }));
            }
            let now_ft = ctx.now_blueos_filetime_ticks();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (item_id_opt, skill_type_id) = if let Some(item) = snap.items.get(&raw_id) {
                    if !snap.is_item_consumable_by(item, char_id, current_dock, current_ship) {
                        return Err(BridgeRpcError {
                            code: "ItemNotReachable".into(),
                            message: format!("Skillbook item {raw_id} is not reachable by character"),
                        });
                    }
                    (Some(item.item_id), item.type_id)
                } else if let Some(inv_item) = snap.items.values().find(|i| {
                    i.type_id == raw_id as u32
                        && i.owner_id == char_id
                        && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                }) {
                    (Some(inv_item.item_id), inv_item.type_id)
                } else if ctx.sde.types.contains_key(&(raw_id as u32)) {
                    (None, raw_id as u32)
                } else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Skillbook item or type {raw_id} not found"),
                    });
                };

                let Some(skill_type) = ctx.sde.types.get(&skill_type_id) else {
                    return Err(BridgeRpcError {
                        code: "SkillNotFound".into(),
                        message: format!("Skill type {skill_type_id} not found in SDE"),
                    });
                };
                if skill_type.category_id != 16 {
                    return Err(BridgeRpcError {
                        code: "InvalidSkillType".into(),
                        message: format!("Type {skill_type_id} is not a skill (category 16)"),
                    });
                }

                // Note: In EVE Online (Skills_and_learning), a skillbook can be injected into the brain
                // at any time without having trained its prerequisite skills; prerequisites are enforced
                // when adding the skill to the training queue (SaveSkillQueue).
                let skills_map = snap.skills.entry(char_id).or_default();
                if skills_map.contains_key(&skill_type_id) {
                    return Err(BridgeRpcError {
                        code: "SkillAlreadyInjected".into(),
                        message: format!("Skill {} is already known to character", skill_type.name),
                    });
                }

                if let Some(iid) = item_id_opt {
                    if let Some(item) = snap.items.get_mut(&iid) {
                        if item.quantity > 1 {
                            item.quantity -= 1;
                            item.version += 1;
                        } else {
                            snap.items.remove(&iid);
                        }
                    }
                }

                skills_map.insert(
                    skill_type_id,
                    TrainedSkillRecord {
                        type_id: skill_type_id,
                        level: 0,
                        skill_points: 0.0,
                    },
                );
                snap.skill_history
                    .entry(char_id)
                    .or_default()
                    .push(SkillHistoryEntry {
                        log_date_ft: now_ft,
                        // 56 = SkillEventSkillInjected (evemu_Crucible EVE_Skills.h:30)
                        event_type_id: 56,
                        skill_type_id,
                        skill_level: 0,
                        absolute_points: 0.0,
                    });

                Ok(skill_type_id)
            });

            match res {
                Ok(tid) => Some(Ok(PyRep::keyval([
                    ("typeID", PyRep::Integer(tid as i64)),
                    ("injected", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("charMgr", "GetPublicInfo") | ("charMgr", "GetPublicInfo3") => {
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id);
            let Some(cid) = target_char_id else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "GetPublicInfo requires an explicit charID or selected character".into(),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&cid).cloned().map(|c| {
                    let (_, portrait) = snap
                        .character_doll_info
                        .get(&cid)
                        .cloned()
                        .unwrap_or_else(|| (String::new(), format!("portrait:bloodline:{}", c.bloodline_id)));
                    (c, portrait)
                })
            });
            let Some((c, portrait_info)) = res else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {cid} not found"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("characterName", PyRep::String(c.name)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                ("allianceID", c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("raceID", PyRep::Integer(c.race_id as i64)),
                ("bloodlineID", PyRep::Integer(c.bloodline_id as i64)),
                ("ancestryID", PyRep::Integer(c.ancestry_id as i64)),
                ("schoolID", PyRep::Integer(c.school_id as i64)),
                ("securityStatus", PyRep::Real(c.security_status)),
                ("description", PyRep::String(c.description)),
                ("title", PyRep::String(c.title)),
                ("gender", PyRep::Integer(c.gender as i64)),
                ("createDateTime", PyRep::Integer(c.create_date_unix as i64)),
                ("portraitInfo", PyRep::String(portrait_info)),
            ])))
        }

        ("charMgr", "GetPrivateInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_cid = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            if target_cid != char_id {
                return Some(Err(BridgeRpcError {
                    code: "CharacterOwnershipMismatch".into(),
                    message: format!("GetPrivateInfo can only be queried for the active character {char_id}"),
                }));
            }
            let Some(c) = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).cloned()) else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {char_id} not found"),
                }));
            };
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(c.character_id)),
                ("charName", PyRep::String(c.name.clone())),
                ("characterName", PyRep::String(c.name)),
                ("gender", PyRep::Integer(c.gender as i64)),
                ("createDateTime", PyRep::Integer(c.create_date_unix as i64)),
                ("raceID", PyRep::Integer(c.race_id as i64)),
                ("bloodlineID", PyRep::Integer(c.bloodline_id as i64)),
                ("ancestryID", PyRep::Integer(c.ancestry_id as i64)),
                ("schoolID", PyRep::Integer(c.school_id as i64)),
                ("corporationID", PyRep::Integer(c.corporation_id)),
                ("allianceID", c.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("balance", PyRep::Real(c.balance.to_wire_f64())),
                ("skillPoints", PyRep::Real(c.skill_points)),
                ("freeSkillPoints", PyRep::Real(c.free_skill_points)),
                ("securityStatus", PyRep::Real(c.security_status)),
                ("cloneStationID", PyRep::Integer(c.home_station_id)),
            ])))
        }

        ("charMgr", "GetCharacterDescription") => {
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id);
            let Some(cid) = target_char_id else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "GetCharacterDescription requires charID or selected character".into(),
                }));
            };
            let desc = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&cid).map(|c| c.description.clone())
            }).unwrap_or_default();
            Some(Ok(PyRep::String(desc)))
        }

        ("charMgr", "SetCharacterDescription") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let desc = args.first().and_then(|a| a.as_str()).unwrap_or("");
            if desc.len() > 5000 {
                return Some(Err(BridgeRpcError {
                    code: "DescriptionTooLong".into(),
                    message: "Character description exceeds 5000 characters limit".into(),
                }));
            }
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.description = desc.to_string();
                }
            });
            Some(Ok(PyRep::None))
        }

        ("corporationSvc", "GetEmployementRecordAndCharacterTransfers")
        | ("charMgr", "GetEmployementRecordAndCharacterTransfers") => {
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.character_id);
            let Some(cid) = target_char_id else {
                return Some(Err(BridgeRpcError {
                    code: "CharacterNotSelected".into(),
                    message: "GetEmployementRecordAndCharacterTransfers requires charID or selected character".into(),
                }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&cid).map(|c| {
                    vec![vec![
                        PyRep::Integer(1),
                        PyRep::Integer(c.corporation_id),
                        PyRep::Integer(c.create_date_unix as i64),
                    ]]
                }).unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec!["recordID", "corporationID", "startDate"],
                rows,
            )))
        }

        ("charMgr", "AddOwnerNote") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let label = args.get(1).and_then(|a| a.as_str()).unwrap_or("Default").to_string();
            let note = args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string();
            let now_unix = ctx.now_unix();
            let note_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.owner_notes.insert(id, eve_db::OwnerNoteRecord {
                    note_id: id,
                    character_id: char_id,
                    item_id: target_id,
                    label,
                    note,
                    timestamp_unix: now_unix,
                });
                id
            });
            Some(Ok(PyRep::Integer(note_id)))
        }

        ("charMgr", "GetOwnerNote") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let rows = ctx.db.with_snapshot(|snap| {
                snap.owner_notes
                    .values()
                    .filter(|n| n.character_id == char_id && n.item_id == target_id)
                    .map(|n| {
                        vec![
                            PyRep::Integer(n.note_id),
                            PyRep::Integer(n.item_id),
                            PyRep::String(n.label.clone()),
                            PyRep::String(n.note.clone()),
                            PyRep::Integer(n.timestamp_unix as i64),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["noteID", "itemID", "label", "note", "timestamp"],
                rows,
            )))
        }

        ("charMgr", "EditOwnerNote") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let note_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let label = args.get(1).and_then(|a| a.as_str());
            let note = args.get(2).and_then(|a| a.as_str());
            let updated = ctx.db.with_mut_snapshot(|snap| {
                if let Some(n) = snap.owner_notes.get_mut(&note_id) {
                    if n.character_id == char_id {
                        if let Some(l) = label { n.label = l.to_string(); }
                        if let Some(nt) = note { n.note = nt.to_string(); }
                        n.timestamp_unix = ctx.now_unix();
                        return true;
                    }
                }
                false
            });
            Some(Ok(PyRep::Bool(updated)))
        }

        ("charMgr", "RemoveOwnerNote") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let note_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let removed = ctx.db.with_mut_snapshot(|snap| {
                if let Some(n) = snap.owner_notes.get(&note_id) {
                    if n.character_id == char_id {
                        snap.owner_notes.remove(&note_id);
                        return true;
                    }
                }
                false
            });
            Some(Ok(PyRep::Bool(removed)))
        }

        ("charMgr", "GetOwnerNoteLabels") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let labels = ctx.db.with_snapshot(|snap| {
                let mut set = std::collections::BTreeSet::new();
                for n in snap.owner_notes.values() {
                    if n.character_id == char_id {
                        set.insert(n.label.clone());
                    }
                }
                set.into_iter().map(PyRep::String).collect()
            });
            Some(Ok(PyRep::List(labels)))
        }

        ("charUnboundMgr", "ApplyNeuralRemap")
        | ("charUnboundMgr", "RemapAttributes")
        | ("skillMgr2", "RemapAttributes")
        | ("character", "RemapAttributes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (cha_opt, int_opt, mem_opt, per_opt, wil_opt) = match args.first() {
                Some(PyRep::Dict(map)) => {
                    let get_u8 = |k: &str| {
                        map.iter()
                            .find(|(key, _)| key.as_str() == Some(k))
                            .and_then(|(_, v)| v.as_i64())
                            .map(|v| v as u8)
                    };
                    (
                        get_u8("charisma"),
                        get_u8("intelligence"),
                        get_u8("memory"),
                        get_u8("perception"),
                        get_u8("willpower"),
                    )
                }
                Some(PyRep::KeyVal(kv)) => (
                    kv.get("charisma").and_then(|v| v.as_i64()).map(|v| v as u8),
                    kv.get("intelligence").and_then(|v| v.as_i64()).map(|v| v as u8),
                    kv.get("memory").and_then(|v| v.as_i64()).map(|v| v as u8),
                    kv.get("perception").and_then(|v| v.as_i64()).map(|v| v as u8),
                    kv.get("willpower").and_then(|v| v.as_i64()).map(|v| v as u8),
                ),
                _ => (
                    args.first()
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("charisma").and_then(|v| v.as_i64()).map(|v| v as u8)),
                    args.get(1)
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("intelligence").and_then(|v| v.as_i64()).map(|v| v as u8)),
                    args.get(2)
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("memory").and_then(|v| v.as_i64()).map(|v| v as u8)),
                    args.get(3)
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("perception").and_then(|v| v.as_i64()).map(|v| v as u8)),
                    args.get(4)
                        .and_then(|v| v.as_i64())
                        .map(|v| v as u8)
                        .or_else(|| _kwargs.get("willpower").and_then(|v| v.as_i64()).map(|v| v as u8)),
                ),
            };
            let (Some(cha), Some(int), Some(mem), Some(per), Some(wil)) =
                (cha_opt, int_opt, mem_opt, per_opt, wil_opt)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRemapArguments".into(),
                    message: "Remap requires charisma, intelligence, memory, perception, and willpower values".into(),
                }));
            };

            let now_unix = ctx.now_unix();
            match ctx.db.remap_neural_attributes(char_id, cha, int, mem, per, wil, now_unix) {
                Ok(ch) => Some(Ok(PyRep::keyval([
                    ("characterID", PyRep::Integer(char_id)),
                    ("charisma", PyRep::Integer(ch.neural.charisma as i64)),
                    ("intelligence", PyRep::Integer(ch.neural.intelligence as i64)),
                    ("memory", PyRep::Integer(ch.neural.memory as i64)),
                    ("perception", PyRep::Integer(ch.neural.perception as i64)),
                    ("willpower", PyRep::Integer(ch.neural.willpower as i64)),
                    ("bonusRemaps", PyRep::Integer(ch.neural.bonus_remaps as i64)),
                    (
                        "lastRemapUnix",
                        PyRep::Integer(ch.neural.last_remap_unix.unwrap_or(0) as i64),
                    ),
                    (
                        "remapCooldownUntilUnix",
                        PyRep::Integer(ch.neural.remap_cooldown_until_unix.unwrap_or(0) as i64),
                    ),
                ]))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "RemapFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("jumpCloneSvc", "InstallCloneInStation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "InstallCloneInStation requires being docked at an NPC station".into(),
                }));
            }
            let Some(station_id) = ctx
                .db
                .with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.station_id))
                .or(session.station_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "InstallCloneInStation requires being docked at an NPC station".into(),
                }));
            };
            let station_info = ctx
                .sde
                .solar_systems
                .values()
                .flat_map(|s| s.stations.iter())
                .find(|st| st.station_id == station_id);
            let Some(station) = station_info else {
                return Some(Err(BridgeRpcError {
                    code: "StationNotFound".into(),
                    message: format!("Station {station_id} not found in SDE"),
                }));
            };
            if !station.has_clone_bay {
                return Some(Err(BridgeRpcError {
                    code: "StationHasNoCloneBay".into(),
                    message: format!(
                        "Station {} ({station_id}) does not have a medical/clone bay service",
                        station.name
                    ),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Read Infomorph Psychology (24242), Advanced Infomorph Psychology (33407), Elite Infomorph Psychology (73910),
                // and Infomorph Synchronizing (33399) from character's trained skills
                let skill_map = snap.skills.get(&char_id);
                let psych_level = skill_map
                    .and_then(|m| m.get(&24242))
                    .map(|s| s.level)
                    .unwrap_or(0);
                if psych_level < 1 {
                    return Err(BridgeRpcError {
                        code: "InfomorphPsychologyRequired".into(),
                        message: "Installing a jump clone requires Infomorph Psychology level 1 or higher".into(),
                    });
                }
                let adv_psych_level = skill_map
                    .and_then(|m| m.get(&33407))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let elite_psych_level = skill_map
                    .and_then(|m| m.get(&73910))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let max_clones = (psych_level as usize)
                    .saturating_add(adv_psych_level as usize)
                    .saturating_add(elite_psych_level as usize);
                let sync_level = skill_map
                    .and_then(|m| m.get(&33399))
                    .map(|s| s.level)
                    .unwrap_or(0);

                let clones = snap.jump_clones.entry(char_id).or_default();
                if clones.iter().any(|c| c.location_id == station_id) {
                    return Err(BridgeRpcError {
                        code: "JumpCloneAlreadyAtStation".into(),
                        message: format!(
                            "Character already has a jump clone installed at station {station_id}"
                        ),
                    });
                }
                if clones.len() >= max_clones {
                    return Err(BridgeRpcError {
                        code: "MaxJumpClonesReached".into(),
                        message: format!(
                            "Character has reached the maximum of {max_clones} installed jump clones"
                        ),
                    });
                }

                // Deduct canonical 900,000 ISK clone installation fee (`Jump_clones.md:20, 54`)
                let install_fee = jump_clone_install_fee_cents();
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let Some(new_bal) = ch.balance.checked_sub(install_fee) else {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Installing a jump clone requires 900,000 ISK".into(),
                    });
                };
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let clones = snap.jump_clones.entry(char_id).or_default();
                clones.push(JumpCloneRecord {
                    jump_clone_id: id,
                    character_id: char_id,
                    location_id: station_id,
                    implants: Vec::new(),
                });
                Ok((id, clone_jump_cooldown_seconds(sync_level)))
            });
            match res {
                Ok((jc_id, cooldown_sec)) => Some(Ok(PyRep::keyval([
                    ("jumpCloneID", PyRep::Integer(jc_id)),
                    ("locationID", PyRep::Integer(station_id)),
                    ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("jumpCloneSvc", "InstallCloneInStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(structure_id) = ctx
                .db
                .with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.structure_id))
                .or(session.structure_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message:
                        "InstallCloneInStructure requires being docked at an Upwell structure"
                            .into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                if !st.quantum_core_installed || st.fuel_blocks == 0 {
                    return Err(BridgeRpcError {
                        code: "StructureHasNoCloneBay".into(),
                        message: format!(
                            "Structure {structure_id} does not have an online Standup Cloning Center service"
                        ),
                    });
                }

                let skill_map = snap.skills.get(&char_id);
                let psych_level = skill_map
                    .and_then(|m| m.get(&24242))
                    .map(|s| s.level)
                    .unwrap_or(0);
                if psych_level < 1 {
                    return Err(BridgeRpcError {
                        code: "InfomorphPsychologyRequired".into(),
                        message: "Installing a jump clone requires Infomorph Psychology level 1 or higher".into(),
                    });
                }
                let adv_psych_level = skill_map
                    .and_then(|m| m.get(&33407))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let elite_psych_level = skill_map
                    .and_then(|m| m.get(&73910))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let max_clones = (psych_level as usize)
                    .saturating_add(adv_psych_level as usize)
                    .saturating_add(elite_psych_level as usize);
                let sync_level = skill_map
                    .and_then(|m| m.get(&33399))
                    .map(|s| s.level)
                    .unwrap_or(0);

                let clones = snap.jump_clones.entry(char_id).or_default();
                if clones.iter().any(|c| c.location_id == structure_id) {
                    return Err(BridgeRpcError {
                        code: "JumpCloneAlreadyAtStation".into(),
                        message: format!(
                            "Character already has a jump clone installed at structure {structure_id}"
                        ),
                    });
                }
                if clones.len() >= max_clones {
                    return Err(BridgeRpcError {
                        code: "MaxJumpClonesReached".into(),
                        message: format!(
                            "Character has reached the maximum of {max_clones} installed jump clones"
                        ),
                    });
                }

                let install_fee = jump_clone_install_fee_cents();
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let Some(new_bal) = ch.balance.checked_sub(install_fee) else {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Installing a jump clone requires 900,000 ISK".into(),
                    });
                };
                ch.balance = new_bal;

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let clones = snap.jump_clones.entry(char_id).or_default();
                clones.push(JumpCloneRecord {
                    jump_clone_id: id,
                    character_id: char_id,
                    location_id: structure_id,
                    implants: Vec::new(),
                });
                Ok((id, clone_jump_cooldown_seconds(sync_level)))
            });
            match res {
                Ok((jc_id, cooldown_sec)) => Some(Ok(PyRep::keyval([
                    ("jumpCloneID", PyRep::Integer(jc_id)),
                    ("locationID", PyRep::Integer(structure_id)),
                    ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("jumpCloneSvc", "GetCloneState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.jump_clones
                    .get(&char_id)
                    .map(|clones| {
                        clones
                            .iter()
                            .map(|c| {
                                vec![
                                    PyRep::Integer(c.jump_clone_id),
                                    PyRep::Integer(c.location_id),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["jumpCloneID", "locationID"], rows)))
        }

        ("jumpCloneSvc", "GetStationCloneState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_loc_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message:
                        "GetStationCloneState requires being docked at a station or structure"
                            .into(),
                }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.jump_clones
                    .get(&char_id)
                    .map(|clones| {
                        clones
                            .iter()
                            .filter(|c| c.location_id == docked_loc_id)
                            .map(|c| {
                                vec![
                                    PyRep::Integer(c.jump_clone_id),
                                    PyRep::Integer(c.location_id),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["jumpCloneID", "locationID"], rows)))
        }

        ("jumpCloneSvc", "GetPriceForClone") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_loc_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "GetPriceForClone requires being docked at a station or structure"
                        .into(),
                }));
            };
            let valid_facility = ctx
                .sde
                .solar_systems
                .values()
                .flat_map(|s| s.stations.iter())
                .any(|st| st.station_id == docked_loc_id && st.has_clone_bay)
                || ctx.db.with_snapshot(|snap| {
                    snap.structures
                        .get(&docked_loc_id)
                        .map(|st| st.quantum_core_installed && st.fuel_blocks > 0)
                        .unwrap_or(false)
                });
            if !valid_facility {
                return Some(Err(BridgeRpcError {
                    code: "StationHasNoCloneBay".into(),
                    message: format!("Facility {docked_loc_id} does not have an active clone bay"),
                }));
            }
            let price = jump_clone_install_fee_cents().to_wire_f64();
            Some(Ok(PyRep::Real(price)))
        }

        ("jumpCloneSvc", "CloneJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "Clone jump requires being docked at a station or structure".into(),
                }));
            }
            let Some(departure_loc_id) = ctx
                .db
                .with_snapshot(|snap| {
                    snap.characters
                        .get(&char_id)
                        .and_then(|c| c.station_id.or(c.structure_id))
                })
                .or_else(|| session.station_id.or(session.structure_id))
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDocked".into(),
                    message: "Clone jump requires being docked at a station or structure".into(),
                }));
            };
            let target_jc_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if target_jc_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidJumpCloneID".into(),
                    message: "CloneJump requires a positive jumpCloneID".into(),
                }));
            }
            let departure_has_clone_bay = if departure_loc_id >= 1_000_000_000_000 {
                ctx.db.with_snapshot(|snap| {
                    snap.structures
                        .get(&departure_loc_id)
                        .map(|st| st.quantum_core_installed && st.fuel_blocks > 0)
                        .unwrap_or(false)
                })
            } else {
                ctx.sde
                    .solar_systems
                    .values()
                    .flat_map(|s| s.stations.iter())
                    .find(|st| st.station_id == departure_loc_id)
                    .map(|st| st.has_clone_bay)
                    .unwrap_or(false)
            };
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let sync_level = snap
                    .skills
                    .get(&char_id)
                    .and_then(|m| m.get(&33399))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let full_cooldown_sec = clone_jump_cooldown_seconds(sync_level);

                let Some(ch_check) = snap.characters.get(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };

                let clones = snap.jump_clones.entry(char_id).or_default();
                let Some(jc_idx) = clones.iter().position(|c| c.jump_clone_id == target_jc_id)
                else {
                    return Err(BridgeRpcError {
                        code: "JumpCloneNotFound".into(),
                        message: format!("Jump clone {target_jc_id} not found"),
                    });
                };
                let dest_loc_id = clones[jc_idx].location_id;
                let same_station = dest_loc_id == departure_loc_id;
                if !departure_has_clone_bay {
                    return Err(BridgeRpcError {
                        code: "StationHasNoCloneBay".into(),
                        message: format!(
                            "Clone jump from {departure_loc_id} requires an active clone bay to store your left-behind clone"
                        ),
                    });
                }
                let effective_cooldown = if same_station { 0 } else { full_cooldown_sec };

                if !same_station
                    && ch_check.last_clone_jump_unix > 0
                    && now_unix < ch_check.last_clone_jump_unix.saturating_add(full_cooldown_sec)
                {
                    return Err(BridgeRpcError {
                        code: "CloneJumpCooldownActive".into(),
                        message: format!(
                            "Clone jump is on cooldown for {} seconds",
                            ch_check
                                .last_clone_jump_unix
                                .saturating_add(full_cooldown_sec)
                                .saturating_sub(now_unix)
                        ),
                    });
                }

                // Resolve destination solar_system_id, constellation_id, and region_id and verify destination clone bay
                let (dest_sys_id, dest_const_id, dest_reg_id) = if dest_loc_id >= 1_000_000_000_000 {
                    let Some(st) = snap.structures.get(&dest_loc_id) else {
                        return Err(BridgeRpcError {
                            code: "CloneDestinationNotFound".into(),
                            message: format!(
                                "Destination structure {dest_loc_id} no longer exists"
                            ),
                        });
                    };
                    if !st.quantum_core_installed || st.fuel_blocks == 0 {
                        return Err(BridgeRpcError {
                            code: "StructureHasNoCloneBay".into(),
                            message: format!(
                                "Destination structure {dest_loc_id} does not have an online Standup Cloning Center service"
                            ),
                        });
                    }
                    let sys_id = st.solar_system_id;
                    let Some(sys) = ctx.sde.solar_systems.get(&sys_id) else {
                        return Err(BridgeRpcError {
                            code: "CloneDestinationNotFound".into(),
                            message: format!(
                                "Destination solar system {sys_id} not found in SDE"
                            ),
                        });
                    };
                    (sys_id, sys.constellation_id, sys.region_id)
                } else {
                    let Some((sys, dest_station)) = ctx
                        .sde
                        .solar_systems
                        .values()
                        .find_map(|sys| {
                            sys.stations
                                .iter()
                                .find(|st| st.station_id == dest_loc_id)
                                .map(|st| (sys, st))
                        })
                    else {
                        return Err(BridgeRpcError {
                            code: "CloneDestinationNotFound".into(),
                            message: format!(
                                "Destination station {dest_loc_id} not found in SDE"
                            ),
                        });
                    };
                    if !dest_station.has_clone_bay {
                        return Err(BridgeRpcError {
                            code: "StationHasNoCloneBay".into(),
                            message: format!(
                                "Destination station {dest_loc_id} does not have an active clone bay"
                            ),
                        });
                    }
                    (sys.solar_system_id, sys.constellation_id, sys.region_id)
                };

                // Swap clone record to departure station (old body becomes jump clone at departure_loc_id)
                clones[jc_idx].location_id = departure_loc_id;

                // Ensure character is placed in a Capsule (type_id 670) at dest_loc_id, leaving any ship behind at departure_loc_id
                let existing_capsule_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.type_id == 670
                            && i.location_id == dest_loc_id
                    })
                    .map(|i| i.item_id);
                let capsule_id = if let Some(cid) = existing_capsule_id {
                    cid
                } else {
                    snap.next_entity_id += 1;
                    let cid = snap.next_entity_id;
                    snap.items.insert(
                        cid,
                        eve_db::InventoryItemRecord {
                            item_id: cid,
                            type_id: 670, // Capsule
                            owner_id: char_id,
                            location_id: dest_loc_id,
                            flag: InventoryFlag::Hangar,
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
                    cid
                };

                let active_implants = {
                    let char_implants = snap.active_implants.entry(char_id).or_default();
                    if let Some(clones_mut) = snap.jump_clones.get_mut(&char_id) {
                        std::mem::swap(char_implants, &mut clones_mut[jc_idx].implants);
                    }
                    char_implants.clone()
                };
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if dest_loc_id >= 1_000_000_000_000 {
                        ch.structure_id = Some(dest_loc_id);
                        ch.station_id = None;
                    } else {
                        ch.station_id = Some(dest_loc_id);
                        ch.structure_id = None;
                    }
                    ch.solar_system_id = dest_sys_id;
                    ch.constellation_id = dest_const_id;
                    ch.region_id = dest_reg_id;
                    ch.active_ship_id = capsule_id;
                    if !same_station {
                        ch.last_clone_jump_unix = now_unix;
                    }
                    ch.version += 1;
                    ch.system_epoch += 1;
                }
                Ok((
                    target_jc_id,
                    dest_loc_id,
                    dest_sys_id,
                    dest_const_id,
                    dest_reg_id,
                    capsule_id,
                    effective_cooldown,
                    active_implants,
                ))
            });
            match res {
                Ok((
                    jc_id,
                    dest_loc_id,
                    dest_sys_id,
                    dest_const_id,
                    dest_reg_id,
                    capsule_id,
                    cooldown_sec,
                    active_implants,
                )) => {
                    if dest_loc_id >= 1_000_000_000_000 {
                        session.structure_id = Some(dest_loc_id);
                        session.station_id = None;
                    } else {
                        session.station_id = Some(dest_loc_id);
                        session.structure_id = None;
                    }
                    session.solar_system_id = Some(dest_sys_id);
                    session.constellation_id = Some(dest_const_id);
                    session.region_id = Some(dest_reg_id);
                    session.ship_id = Some(capsule_id);
                    let implant_reps: Vec<PyRep> = active_implants
                        .into_iter()
                        .map(|t| PyRep::Integer(t as i64))
                        .collect();
                    Some(Ok(PyRep::keyval([
                        ("jumpCloneID", PyRep::Integer(jc_id)),
                        ("locationID", PyRep::Integer(dest_loc_id)),
                        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                        ("shipID", PyRep::Integer(capsule_id)),
                        ("cooldownSeconds", PyRep::Integer(cooldown_sec as i64)),
                        ("activeImplants", PyRep::List(implant_reps)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("jumpCloneSvc", "DestroyInstalledClone") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let jc_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let removed = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let clones = snap.jump_clones.entry(char_id).or_default();
                    let before = clones.len();
                    clones.retain(|c| c.jump_clone_id != jc_id);
                    Ok::<_, std::convert::Infallible>(clones.len() < before)
                })
                .unwrap();
            Some(Ok(PyRep::Bool(removed)))
        }

        ("standingMgr", "GetCharStandings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| {
                        c.standings
                            .iter()
                            .map(|(&from_id, &val)| {
                                vec![PyRep::Integer(from_id), PyRep::Real(val)]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(vec!["fromID", "standing"], rows)))
        }

        ("standingMgr", "GetStandingTransactions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let from_filter = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
            let rows = ctx.db.with_snapshot(|snap| {
                snap.standing_transactions
                    .get(&char_id)
                    .map(|txs| {
                        txs.iter()
                            .filter(|tx| from_filter.map_or(true, |f| f == tx.from_id))
                            .map(|tx| {
                                vec![
                                    PyRep::Integer(tx.event_id),
                                    PyRep::Integer(tx.from_id),
                                    PyRep::Integer(tx.to_id),
                                    PyRep::Real(tx.modification),
                                    PyRep::Integer(tx.event_type_id),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::rowset(
                vec!["eventID", "fromID", "toID", "modification", "eventTypeID"],
                rows,
            )))
        }

        ("crimewatch", "ApplyTagsForSecStatus") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedInStation".into(),
                    message:
                        "Security status tag turn-in requires being docked at a CONCORD station"
                            .into(),
                }));
            }
            let raw_tags = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let Ok(tags_i32) = i32::try_from(raw_tags) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTagQuantity".into(),
                    message: format!("Invalid security tag quantity {raw_tags}"),
                }));
            };
            if tags_i32 <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTagQuantity".into(),
                    message: "Security tag quantity must be strictly positive".into(),
                }));
            }
            let tags_requested = tags_i32 as u32;
            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Find owned, unlocked, reachable Clone Soldier Tag stack (35661..=35664) with sufficient quantity
                let tag_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        (35661..=35664).contains(&i.type_id)
                            && i.quantity >= tags_i32
                            && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                    })
                    .map(|i| i.item_id);
                let Some(item_id) = tag_item_id else {
                    return Err(BridgeRpcError {
                        code: "InsufficientSecurityTags".into(),
                        message: format!(
                            "Character does not own {tags_requested} Clone Soldier Tag(s) (35661..35664)"
                        ),
                    });
                };

                if let Some(item) = snap.items.get_mut(&item_id) {
                    if item.quantity > tags_i32 {
                        item.quantity -= tags_i32;
                        item.version += 1;
                    } else {
                        snap.items.remove(&item_id);
                    }
                }

                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.security_status =
                        apply_security_tag_repair(ch.security_status, tags_requested, 0.0);
                    Ok(ch.security_status)
                } else {
                    Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })
                }
            });
            match res {
                Ok(new_sec) => Some(Ok(PyRep::Real(new_sec))),
                Err(e) => Some(Err(e)),
            }
        }

        ("home_station", "get_home_station") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            match ctx.db.get_character(char_id) {
                Ok(ch) => Some(Ok(PyRep::Integer(ch.home_station_id))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("home_station", "set_home_station") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(new_home_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDockedAtTargetStation".into(),
                    message: "set_home_station requires being docked at the target station".into(),
                }));
            };
            if session.station_id != Some(new_home_id) {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDockedAtTargetStation".into(),
                    message: format!(
                        "Setting home station {new_home_id} requires being docked at that station"
                    ),
                }));
            }
            let exists_in_sde = ctx
                .sde
                .solar_systems
                .values()
                .any(|s| s.stations.iter().any(|st| st.station_id == new_home_id));
            if !exists_in_sde {
                return Some(Err(BridgeRpcError {
                    code: "StationNotFound".into(),
                    message: format!("Station {new_home_id} does not exist in SDE"),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                ch.home_station_id = new_home_id;
                Ok(ch.home_station_id)
            });
            match res {
                Ok(hid) => Some(Ok(PyRep::Integer(hid))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
