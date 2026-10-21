//! Data-driven sweep over all 213 V24.01 client static-data resources and domain tables (`T-1`).

use eve_sde::SdeRegistry;
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Pure-Rust RFC 1321 MD5 digest implementation to independently verify raw client files in `ResFiles/`.
fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    #[inline(always)]
    fn compress(state: &mut [u32; 4], chunk: &[u8]) {
        let mut m = [0u32; 16];
        let mut i = 0;
        while i < 16 {
            let base = i * 4;
            m[i] = u32::from_le_bytes([
                chunk[base],
                chunk[base + 1],
                chunk[base + 2],
                chunk[base + 3],
            ]);
            i += 1;
        }
        let [mut a, mut b, mut c, mut d] = *state;
        let mut r = 0;
        while r < 64 {
            let (f, g) = if r < 16 {
                ((b & c) | (!b & d), r)
            } else if r < 32 {
                ((d & b) | (!d & c), (5 * r + 1) & 15)
            } else if r < 48 {
                (b ^ c ^ d, (3 * r + 5) & 15)
            } else {
                (c ^ (b | !d), (7 * r) & 15)
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[r]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[r]));
            a = temp;
            r += 1;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut state = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    let chunks = data.chunks_exact(64);
    let rem = chunks.remainder();
    for chunk in chunks {
        compress(&mut state, chunk);
    }

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    if rem.len() < 56 {
        tail[56..64].copy_from_slice(&bit_len.to_le_bytes());
        compress(&mut state, &tail[..64]);
    } else {
        tail[120..128].copy_from_slice(&bit_len.to_le_bytes());
        compress(&mut state, &tail[..64]);
        compress(&mut state, &tail[64..128]);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&state[0].to_le_bytes());
    digest[4..8].copy_from_slice(&state[1].to_le_bytes());
    digest[8..12].copy_from_slice(&state[2].to_le_bytes());
    digest[12..16].copy_from_slice(&state[3].to_le_bytes());
    let mut out = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn read_u32_le(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("u32 slice"))
}

fn read_i32_le(buf: &[u8], pos: usize) -> i32 {
    i32::from_le_bytes(buf[pos..pos + 4].try_into().expect("i32 slice"))
}

fn read_u64_le(buf: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(buf[pos..pos + 8].try_into().expect("u64 slice"))
}

fn read_f32_le(buf: &[u8], pos: usize) -> f32 {
    f32::from_le_bytes(buf[pos..pos + 4].try_into().expect("f32 slice"))
}

fn read_f64_le(buf: &[u8], pos: usize) -> f64 {
    f64::from_le_bytes(buf[pos..pos + 8].try_into().expect("f64 slice"))
}

fn parse_fsd_dict(buf: &[u8], pos: usize) -> Vec<(u32, usize)> {
    let byte_len = read_u32_le(buf, pos) as usize;
    let body_start = pos + 4;
    let body_end = body_start + byte_len;
    let footer_size = read_u32_le(buf, body_end - 4) as usize;
    let footer_pos = body_end - 4 - footer_size;
    let cnt = read_u32_le(buf, footer_pos) as usize;
    assert_eq!(footer_size, 4 + 8 * cnt, "FSD dict footer size mismatch");
    let mut out = Vec::with_capacity(cnt);
    for i in 0..cnt {
        let entry_pos = footer_pos + 4 + i * 8;
        let k = read_u32_le(buf, entry_pos);
        let off = read_u32_le(buf, entry_pos + 4) as usize;
        out.push((k, body_start + off));
    }
    out
}

fn parse_fsd_var_offsets<'a>(
    buf: &[u8],
    pos: usize,
    end_of_fixed: usize,
    var_defs: &[(&'a str, u64)],
) -> HashMap<&'a str, usize> {
    let bitmask = read_u64_le(buf, pos + end_of_fixed);
    let present: Vec<&'a str> = var_defs
        .iter()
        .filter_map(|&(name, mask)| {
            if mask == 0 || (bitmask & mask) != 0 {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    let table_start = pos + end_of_fixed + 8;
    let base = table_start + 4 * present.len();
    let mut out = HashMap::with_capacity(present.len());
    for (i, name) in present.into_iter().enumerate() {
        let off = read_u32_le(buf, table_start + 4 * i) as usize;
        out.insert(name, base + off);
    }
    out
}

/// Independently verifies every row and every loaded field of a `.static` client file
/// against the parsed domain table in `SdeRegistry`. Returns `true` iff the resource is loaded
/// into `SdeRegistry` and verified across all rows against the client `.static` file.
fn verify_client_sqlite_static_loaded_rows(
    res_path: &str,
    c_conn: Option<&Connection>,
    raw_bytes: &[u8],
    cache_cnt: usize,
    sde: &SdeRegistry,
    sde_conn: &Connection,
) -> bool {
    let rows: Vec<(String, serde_json::Value)> = if let Some(conn) = c_conn {
        let mut stmt = conn
            .prepare("SELECT CAST(key AS TEXT), value FROM cache")
            .expect("prepare SELECT key, value FROM cache");
        let parsed: Vec<(String, serde_json::Value)> = stmt
            .query_map([], |r| {
                let k: String = r.get(0)?;
                let v_str: String = r.get(1)?;
                let v: serde_json::Value =
                    serde_json::from_str(&v_str).expect("parse client .static row JSON");
                Ok((k, v))
            })
            .expect("query cache rows")
            .map(|r| r.expect("valid cache row"))
            .collect();
        assert_eq!(parsed.len(), cache_cnt);
        parsed
    } else {
        Vec::new()
    };

    // These arms verify row-by-row from the client SQLite `cache` table. They must never fall
    // through with an empty `rows` (which would reduce them to a count check against the
    // importer's own record_count); review 20261020-200244 R3-P1-1.
    const SQLITE_ROW_ARMS: [&str; 8] = [
        "res:/staticdata/blueprints.static",
        "res:/staticdata/clonegrades.static",
        "res:/staticdata/dbuffcollections.static",
        "res:/staticdata/fighterabilities.static",
        "res:/staticdata/fighterabilitiesbytype.static",
        "res:/staticdata/skinlicenses.static",
        "res:/staticdata/skinmaterials.static",
        "res:/staticdata/skins.static",
    ];
    if SQLITE_ROW_ARMS.contains(&res_path) {
        assert!(
            c_conn.is_some(),
            "{res_path}: client file must be SQLite for row-by-row verification"
        );
        assert!(cache_cnt > 0, "{res_path}: client SQLite cache must have rows");
        assert_eq!(rows.len(), cache_cnt, "{res_path}: every client row must be checked");
    }

    match res_path {
        "res:/staticdata/blueprints.static" => {
            assert_eq!(sde.blueprints.len(), cache_cnt);
            for (key_str, row_json) in &rows {
                let bp_id: u32 = key_str.parse().expect("blueprint key u32");
                let bp = sde
                    .blueprints
                    .get(&bp_id)
                    .expect("blueprint row must exist in SdeRegistry");
                assert_eq!(
                    row_json["blueprintTypeID"].as_u64(),
                    Some(u64::from(bp.blueprint_type_id))
                );
                assert_eq!(
                    row_json
                        .get("maxProductionLimit")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    u64::from(bp.max_production_limit)
                );
                let client_acts = row_json["activities"]
                    .as_object()
                    .expect("client blueprint activities object");
                assert_eq!(bp.activities.len(), client_acts.len());
                for (act_name, client_act) in client_acts {
                    let actual_act = bp
                        .activities
                        .get(act_name)
                        .expect("activity must exist on BlueprintInfo");
                    assert_eq!(
                        actual_act.time,
                        client_act.get("time").and_then(|v| v.as_u64()).unwrap_or(0)
                    );
                    let empty_arr = Vec::new();
                    let c_mats = client_act
                        .get("materials")
                        .and_then(|v| v.as_array())
                        .unwrap_or(&empty_arr);
                    assert_eq!(actual_act.materials.len(), c_mats.len());
                    for (am, cm) in actual_act.materials.iter().zip(c_mats.iter()) {
                        assert_eq!(Some(u64::from(am.type_id)), cm["typeID"].as_u64());
                        assert_eq!(Some(am.quantity), cm["quantity"].as_u64());
                    }
                    let c_prods = client_act
                        .get("products")
                        .and_then(|v| v.as_array())
                        .unwrap_or(&empty_arr);
                    assert_eq!(actual_act.products.len(), c_prods.len());
                    for (ap, cp) in actual_act.products.iter().zip(c_prods.iter()) {
                        assert_eq!(Some(u64::from(ap.type_id)), cp["typeID"].as_u64());
                        assert_eq!(Some(ap.quantity), cp["quantity"].as_u64());
                        assert_eq!(
                            ap.probability,
                            cp.get("probability").and_then(|v| v.as_f64())
                        );
                    }
                    let c_skills = client_act
                        .get("skills")
                        .and_then(|v| v.as_array())
                        .unwrap_or(&empty_arr);
                    assert_eq!(actual_act.skills.len(), c_skills.len());
                    for (ask, csk) in actual_act.skills.iter().zip(c_skills.iter()) {
                        assert_eq!(Some(u64::from(ask.type_id)), csk["typeID"].as_u64());
                        assert_eq!(Some(u64::from(ask.level)), csk["level"].as_u64());
                    }
                }
                if let Some(mfg) = bp.activities.get("manufacturing") {
                    assert_eq!(bp.manufacturing_time_sec, mfg.time);
                    assert_eq!(bp.materials, mfg.materials);
                }
            }
            true
        }
        "res:/staticdata/clonegrades.static" => {
            assert_eq!(sde.clone_grades.len(), cache_cnt);
            for (key_str, row_json) in &rows {
                let grade_id: u32 = key_str.parse().expect("clonegrade key u32");
                let cg = sde
                    .clone_grades
                    .get(&grade_id)
                    .expect("clone grade must exist in SdeRegistry");
                let client_skills = row_json["skills"]
                    .as_array()
                    .expect("client clonegrade skills array");
                assert_eq!(cg.alpha_skill_caps.len(), client_skills.len());
                for sk in client_skills {
                    let tid = sk["typeID"].as_u64().expect("skill typeID") as u32;
                    let lvl = sk
                        .get("level")
                        .or_else(|| sk.get("maxTrainingLevel"))
                        .and_then(|v| v.as_u64())
                        .expect("clonegrade skill level") as u8;
                    assert_eq!(cg.alpha_skill_caps.get(&tid).copied(), Some(lvl));
                }
            }
            true
        }
        "res:/staticdata/dbuffcollections.static" => {
            assert_eq!(sde.dbuff_collections.len(), cache_cnt);
            for (key_str, row_json) in &rows {
                let dbuff_id: u32 = key_str.parse().expect("dbuff key u32");
                let dbuff = sde
                    .dbuff_collections
                    .get(&dbuff_id)
                    .expect("dbuff collection must exist in SdeRegistry");
                assert_eq!(
                    Some(dbuff.operation.as_str()),
                    row_json["operationName"].as_str()
                );
                assert_eq!(
                    Some(dbuff.aggregate_mode.as_str()),
                    row_json["aggregateMode"].as_str()
                );
                assert_eq!(
                    Some(dbuff.name.as_str()),
                    row_json["developerDescription"].as_str()
                );
                assert_eq!(
                    dbuff.display_name_id.map(u64::from),
                    row_json.get("displayNameID").and_then(|v| v.as_u64())
                );
                assert_eq!(
                    dbuff.show_output_value_in_ui.as_deref(),
                    row_json.get("showOutputValueInUI").and_then(|v| v.as_str())
                );
                let empty_arr = Vec::new();
                let exp_item_mods: Vec<u16> = row_json
                    .get("itemModifiers")
                    .and_then(|v| v.as_array())
                    .unwrap_or(&empty_arr)
                    .iter()
                    .map(|m| m["dogmaAttributeID"].as_u64().unwrap() as u16)
                    .collect();
                assert_eq!(dbuff.item_modifiers, exp_item_mods);
                let exp_loc_mods: Vec<u16> = row_json
                    .get("locationModifiers")
                    .and_then(|v| v.as_array())
                    .unwrap_or(&empty_arr)
                    .iter()
                    .map(|m| m["dogmaAttributeID"].as_u64().unwrap() as u16)
                    .collect();
                assert_eq!(dbuff.location_modifiers, exp_loc_mods);
                let exp_grp_mods: Vec<eve_sde::DbuffGroupModifier> = row_json
                    .get("locationGroupModifiers")
                    .and_then(|v| v.as_array())
                    .unwrap_or(&empty_arr)
                    .iter()
                    .map(|m| eve_sde::DbuffGroupModifier {
                        attribute_id: m["dogmaAttributeID"].as_u64().unwrap() as u16,
                        group_id: m["groupID"].as_u64().unwrap() as u32,
                    })
                    .collect();
                assert_eq!(dbuff.location_group_modifiers, exp_grp_mods);
                let exp_srq_mods: Vec<eve_sde::DbuffSkillModifier> = row_json
                    .get("locationRequiredSkillModifiers")
                    .and_then(|v| v.as_array())
                    .unwrap_or(&empty_arr)
                    .iter()
                    .map(|m| eve_sde::DbuffSkillModifier {
                        attribute_id: m["dogmaAttributeID"].as_u64().unwrap() as u16,
                        skill_id: m["skillID"].as_u64().unwrap() as u32,
                    })
                    .collect();
                assert_eq!(dbuff.location_required_skill_modifiers, exp_srq_mods);
            }
            true
        }
        "res:/staticdata/fighterabilities.static" => {
            assert_eq!(sde.fighter_abilities.len(), cache_cnt);
            for (key_str, row_json) in &rows {
                let ability_id: u32 = key_str.parse().expect("fighter ability key u32");
                let fa = sde
                    .fighter_abilities
                    .get(&ability_id)
                    .expect("fighter ability must exist in SdeRegistry");
                assert_eq!(fa.ability_id, ability_id);
                let obj = row_json
                    .as_object()
                    .expect("fighter ability row must be JSON object");
                for k in obj.keys() {
                    assert!(
                        matches!(
                            k.as_str(),
                            "disallowInHighSec"
                                | "disallowInLowSec"
                                | "displayNameID"
                                | "iconID"
                                | "targetMode"
                                | "tooltipTextID"
                                | "turretGraphicID"
                        ),
                        "Unexpected key {k} on fighter ability {ability_id}"
                    );
                }
                assert_eq!(
                    Some(fa.target_mode.as_str()),
                    row_json["targetMode"].as_str()
                );
                assert_eq!(
                    fa.disallow_in_high_sec,
                    row_json
                        .get("disallowInHighSec")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                );
                assert_eq!(
                    fa.disallow_in_low_sec,
                    row_json
                        .get("disallowInLowSec")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                );
                assert_eq!(
                    fa.icon_id.map(u64::from),
                    row_json.get("iconID").and_then(|v| v.as_u64())
                );
                assert_eq!(
                    fa.display_name_id.map(u64::from),
                    row_json.get("displayNameID").and_then(|v| v.as_u64())
                );
                assert_eq!(
                    fa.tooltip_text_id.map(u64::from),
                    row_json.get("tooltipTextID").and_then(|v| v.as_u64())
                );
                assert_eq!(
                    fa.turret_graphic_id.map(u64::from),
                    row_json.get("turretGraphicID").and_then(|v| v.as_u64())
                );
            }
            true
        }
        "res:/staticdata/fighterabilitiesbytype.static" => {
            assert_eq!(sde.fighter_abilities_by_type.len(), cache_cnt);
            for (key_str, row_json) in &rows {
                let type_id: u32 = key_str.parse().expect("fighter type key u32");
                let slots = sde
                    .fighter_abilities_by_type
                    .get(&type_id)
                    .expect("fighter abilities by type must exist in SdeRegistry");
                let mut expected_slots = Vec::new();
                for (slot_idx, slot_key) in ["abilitySlot0", "abilitySlot1", "abilitySlot2"]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(slot_obj) = row_json.get(slot_key).and_then(|s| s.as_object()) {
                        if let Some(aid) = slot_obj.get("abilityID").and_then(|v| v.as_u64()) {
                            let cooldown_seconds =
                                slot_obj.get("cooldownSeconds").and_then(|v| v.as_f64());
                            let charges =
                                slot_obj
                                    .get("charges")
                                    .and_then(|c| c.as_object())
                                    .map(|c| eve_sde::FighterAbilityCharges {
                                        charge_count: c["chargeCount"].as_u64().unwrap() as u32,
                                        rearm_time_seconds: c["rearmTimeSeconds"].as_f64().unwrap(),
                                    });
                            expected_slots.push(eve_sde::FighterAbilitySlot {
                                slot_index: slot_idx as u8,
                                ability_id: aid as u32,
                                cooldown_seconds,
                                charges,
                            });
                        }
                    }
                }
                assert_eq!(slots, &expected_slots);
            }
            true
        }
        "res:/staticdata/skinlicenses.static" => {
            assert_eq!(sde.skin_licenses.len(), cache_cnt);
            for (key_str, row_json) in &rows {
                let license_tid: u32 = key_str.parse().expect("skin license key u32");
                let sl = sde
                    .skin_licenses
                    .get(&license_tid)
                    .expect("skin license must exist in SdeRegistry");
                assert_eq!(
                    row_json["licenseTypeID"].as_u64(),
                    Some(u64::from(sl.license_type_id))
                );
                assert_eq!(row_json["skinID"].as_u64(), Some(u64::from(sl.skin_id)));
                assert_eq!(row_json["duration"].as_i64(), Some(i64::from(sl.duration)));
            }
            true
        }
        "res:/staticdata/skinmaterials.static" => {
            assert_eq!(sde.skin_materials.len(), cache_cnt);
            assert_eq!(cache_cnt, 875);
            for (key_str, row_json) in &rows {
                let skin_mat_id: u32 = key_str.parse().expect("skin material key u32");
                let sm = sde
                    .skin_materials
                    .get(&skin_mat_id)
                    .expect("skin material must exist in SdeRegistry");
                assert_eq!(sm.skin_material_id, skin_mat_id);
                assert_eq!(
                    row_json["skinMaterialID"].as_u64(),
                    Some(u64::from(sm.skin_material_id))
                );
                assert_eq!(
                    sm.display_name_id.map(u64::from),
                    row_json.get("displayNameID").and_then(|v| v.as_u64())
                );
                assert_eq!(
                    sm.material_set_id.map(u64::from),
                    row_json.get("materialSetID").and_then(|v| v.as_u64())
                );
            }
            true
        }
        "res:/staticdata/skins.static" => {
            assert_eq!(sde.skins.len(), cache_cnt);
            for (key_str, row_json) in &rows {
                let skin_id: u32 = key_str.parse().expect("skin key u32");
                let sk = sde
                    .skins
                    .get(&skin_id)
                    .expect("skin must exist in SdeRegistry");
                assert_eq!(row_json["skinID"].as_u64(), Some(u64::from(sk.skin_id)));
                assert_eq!(
                    row_json["internalName"].as_str(),
                    Some(sk.internal_name.as_str())
                );
                assert_eq!(
                    row_json["skinMaterialID"].as_u64(),
                    Some(u64::from(sk.skin_material_id))
                );
                let expected_types: Vec<u32> = row_json["types"]
                    .as_array()
                    .expect("client skin types array")
                    .iter()
                    .map(|v| v.as_u64().expect("skin type u64") as u32)
                    .collect();
                assert_eq!(sk.types, expected_types);
                assert_eq!(
                    sk.allow_ccp_devs,
                    row_json
                        .get("allowCCPDevs")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                );
                assert_eq!(
                    sk.visible_serenity,
                    row_json
                        .get("visibleSerenity")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                );
                assert_eq!(
                    sk.visible_tranquility,
                    row_json
                        .get("visibleTranquility")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                );
                assert_eq!(
                    sk.is_structure_skin,
                    row_json
                        .get("isStructureSkin")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                );
                let expected_desc = row_json.get("skinDescription").and_then(|v| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| v.as_i64().map(|n| n.to_string()))
                });
                assert_eq!(sk.skin_description, expected_desc);
            }
            true
        }
        "res:/staticdata/asteroidcountbytypeidroomiddungeonid.static" => {
            let sch_len = read_u32_le(raw_bytes, 0) as usize;
            let top = parse_fsd_dict(raw_bytes, 4 + sch_len);
            assert_eq!(top.len(), cache_cnt);
            assert_eq!(sde.dungeon_asteroids.len(), cache_cnt);
            let mut total_entries = 0usize;
            for (dungeon_id, d_pos) in top {
                let actual = sde
                    .dungeon_asteroids
                    .get(&dungeon_id)
                    .expect("dungeon_id must exist in sde.dungeon_asteroids");
                let mut expected = Vec::new();
                for (room_id, r_pos) in parse_fsd_dict(raw_bytes, d_pos) {
                    for (type_id, t_pos) in parse_fsd_dict(raw_bytes, r_pos) {
                        let asteroid_count = read_u32_le(raw_bytes, t_pos);
                        assert!(asteroid_count > 0);
                        expected.push(eve_sde::DungeonAsteroidEntry {
                            room_id,
                            type_id,
                            asteroid_count,
                        });
                    }
                }
                expected.sort_by_key(|e| (e.room_id, e.type_id));
                assert_eq!(actual, &expected);
                total_entries += expected.len();
            }
            assert_eq!(total_entries, 3_054);
            true
        }
        "res:/staticdata/gastypesbydungeonid.static" => {
            let sch_len = read_u32_le(raw_bytes, 0) as usize;
            let top = parse_fsd_dict(raw_bytes, 4 + sch_len);
            assert_eq!(top.len(), cache_cnt);
            assert_eq!(sde.dungeon_gas_types.len(), cache_cnt);
            let mut total_entries = 0usize;
            for (dungeon_id, pos) in top {
                let actual = sde
                    .dungeon_gas_types
                    .get(&dungeon_id)
                    .expect("dungeon_id must exist in sde.dungeon_gas_types");
                let list_cnt = read_u32_le(raw_bytes, pos) as usize;
                let mut expected: Vec<u32> = (0..list_cnt)
                    .map(|j| read_u32_le(raw_bytes, pos + 4 + j * 4))
                    .collect();
                expected.sort_unstable();
                assert_eq!(actual, &expected);
                for &tid in &expected {
                    let ty = sde
                        .types
                        .get(&tid)
                        .expect("gas type_id must exist in sde.types");
                    assert_eq!(ty.group_id, 711, "gas cloud type {tid} must be group 711");
                }
                total_entries += expected.len();
            }
            assert_eq!(total_entries, 92);
            true
        }
        "res:/staticdata/regions.static" => {
            let entries = parse_fsd_dict(raw_bytes, 0);
            assert_eq!(entries.len(), cache_cnt);
            assert_eq!(sde.regions.len(), cache_cnt);
            for (rid, pos) in entries {
                let reg_id = read_u32_le(raw_bytes, pos);
                let cx = read_f64_le(raw_bytes, pos + 8);
                let cy = read_f64_le(raw_bytes, pos + 16);
                let cz = read_f64_le(raw_bytes, pos + 24);
                let nebula_id = read_u32_le(raw_bytes, pos + 32);
                assert_eq!(reg_id, rid);
                assert!(nebula_id > 0);
                let voffs = parse_fsd_var_offsets(
                    raw_bytes,
                    pos,
                    36,
                    &[
                        ("descriptionID", 1),
                        ("neighbours", 0),
                        ("constellationIDs", 0),
                        ("solarSystemIDs", 0),
                        ("factionID", 2),
                        ("wormholeClassID", 4),
                        ("nebulaPath", 0),
                    ],
                );
                let faction_id = voffs
                    .get("factionID")
                    .map(|&off| read_u32_le(raw_bytes, off));
                let wormhole_class_id = voffs
                    .get("wormholeClassID")
                    .map(|&off| read_i32_le(raw_bytes, off));
                let reg = sde
                    .regions
                    .get(&rid)
                    .expect("region must exist in sde.regions");
                assert_eq!(reg.region_id, rid);
                assert_eq!(reg.center, [cx, cy, cz]);
                assert_eq!(reg.faction_id, faction_id);
                assert_eq!(reg.wormhole_class_id, wormhole_class_id);
            }
            true
        }
        "res:/staticdata/constellations.static" => {
            let entries = parse_fsd_dict(raw_bytes, 0);
            assert_eq!(entries.len(), cache_cnt);
            assert_eq!(sde.constellations.len(), cache_cnt);
            for (cid, pos) in entries {
                let con_id = read_u32_le(raw_bytes, pos);
                let reg_id = read_u32_le(raw_bytes, pos + 4);
                let cx = read_f64_le(raw_bytes, pos + 12);
                let cy = read_f64_le(raw_bytes, pos + 20);
                let cz = read_f64_le(raw_bytes, pos + 28);
                assert_eq!(con_id, cid);
                let voffs = parse_fsd_var_offsets(
                    raw_bytes,
                    pos,
                    36,
                    &[
                        ("solarSystemIDs", 0),
                        ("factionID", 1),
                        ("wormholeClassID", 2),
                        ("neighbours", 0),
                    ],
                );
                let faction_id = voffs
                    .get("factionID")
                    .map(|&off| read_u32_le(raw_bytes, off));
                let wormhole_class_id = voffs
                    .get("wormholeClassID")
                    .map(|&off| read_i32_le(raw_bytes, off));
                let con = sde
                    .constellations
                    .get(&cid)
                    .expect("constellation must exist in sde.constellations");
                assert_eq!(con.constellation_id, cid);
                assert_eq!(con.region_id, reg_id);
                assert_eq!(con.center, [cx, cy, cz]);
                assert_eq!(con.faction_id, faction_id);
                assert_eq!(con.wormhole_class_id, wormhole_class_id);
            }
            true
        }
        "res:/staticdata/jumps.static" => {
            let jmp_cnt = read_u32_le(raw_bytes, 0) as usize;
            assert_eq!(jmp_cnt, cache_cnt);
            assert_eq!(raw_bytes.len(), 4 + jmp_cnt * 65);
            assert_eq!(sde.jumps.len(), jmp_cnt * 2);
            let mut sg_type_stmt = sde_conn
                .prepare("SELECT stargate_id, type_id FROM sde_stargates")
                .expect("prepare sde_stargates query");
            let sg_types: HashMap<i64, u32> = sg_type_stmt
                .query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u32))
                })
                .expect("query sde_stargates")
                .map(|r| r.expect("valid stargate row"))
                .collect();
            let mut seen_gates = HashSet::with_capacity(sde.jumps.len());
            for i in 0..jmp_cnt {
                let rec = 4 + i * 65;
                let sg_id = i64::from(read_u32_le(raw_bytes, rec + 4));
                let from_sys = read_u32_le(raw_bytes, rec + 8);
                let to_sys = read_u32_le(raw_bytes, rec + 12);
                let sg = sde
                    .jumps
                    .get(&sg_id)
                    .expect("stargate must exist in sde.jumps");
                assert_eq!(sg.stargate_id, sg_id);
                assert_eq!(sg.from_solar_system_id, from_sys);
                assert_eq!(sg.to_solar_system_id, to_sys);
                let sg_tid = sg_types[&sg_id];
                assert!(sde.types.contains_key(&sg_tid));
                let dest = sde
                    .jumps
                    .get(&sg.destination_stargate_id)
                    .expect("destination stargate must exist in sde.jumps");
                assert_eq!(dest.stargate_id, sg.destination_stargate_id);
                assert_eq!(dest.destination_stargate_id, sg_id);
                assert_eq!(dest.from_solar_system_id, to_sys);
                assert_eq!(dest.to_solar_system_id, from_sys);
                let dest_tid = sg_types[&dest.stargate_id];
                assert!(sde.types.contains_key(&dest_tid));
                seen_gates.insert(sg_id);
                seen_gates.insert(dest.stargate_id);
            }
            assert_eq!(seen_gates.len(), sde.jumps.len());
            true
        }
        "res:/staticdata/systems.static" => {
            const METERS_PER_LY: f64 = 9_460_730_472_580_800.0;
            let entries = parse_fsd_dict(raw_bytes, 0);
            assert_eq!(entries.len(), cache_cnt);
            assert_eq!(sde.solar_systems.len(), cache_cnt);
            let mut sys_stmt = sde_conn
                .prepare(
                    "SELECT system_id, x, y, z, luminosity, sun_type_id, faction_id, wormhole_class_id FROM sde_solar_systems",
                )
                .expect("prepare sde_solar_systems query");
            let db_sys: HashMap<u32, (f64, f64, f64, f64, Option<u32>, Option<u32>, Option<i32>)> =
                sys_stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, i64>(0)? as u32,
                            (
                                r.get::<_, f64>(1)?,
                                r.get::<_, f64>(2)?,
                                r.get::<_, f64>(3)?,
                                r.get::<_, f64>(4)?,
                                r.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                                r.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                                r.get::<_, Option<i64>>(7)?.map(|v| v as i32),
                            ),
                        ))
                    })
                    .expect("query sde_solar_systems")
                    .map(|r| r.expect("valid solar system row"))
                    .collect();
            for (sid, pos) in entries {
                let sys_id = read_u32_le(raw_bytes, pos);
                let sec_f32 = read_f32_le(raw_bytes, pos + 4);
                let con_id = read_u32_le(raw_bytes, pos + 8);
                let reg_id = read_u32_le(raw_bytes, pos + 12);
                let cx = read_f64_le(raw_bytes, pos + 20);
                let cy = read_f64_le(raw_bytes, pos + 28);
                let cz = read_f64_le(raw_bytes, pos + 36);
                assert_eq!(sys_id, sid);
                let voffs = parse_fsd_var_offsets(
                    raw_bytes,
                    pos,
                    48,
                    &[
                        ("securityClass", 1),
                        ("factionID", 2),
                        ("sunTypeID", 4),
                        ("sunFlareGraphicID", 8),
                        ("wormholeClassID", 16),
                        ("warpTunnelOverwrite", 32),
                        ("systemWideCloud", 64),
                        ("neighbours", 0),
                        ("planetCountByType", 0),
                        ("planetItemIDs", 0),
                    ],
                );
                let faction_id = voffs
                    .get("factionID")
                    .map(|&off| read_u32_le(raw_bytes, off));
                let sun_type_id = voffs
                    .get("sunTypeID")
                    .map(|&off| read_u32_le(raw_bytes, off));
                let wormhole_class_id = voffs
                    .get("wormholeClassID")
                    .map(|&off| read_i32_le(raw_bytes, off));
                let p_pos = voffs["planetItemIDs"];
                let p_cnt = read_u32_le(raw_bytes, p_pos) as usize;
                let mut planet_ids: Vec<i64> = (0..p_cnt)
                    .map(|j| i64::from(read_i32_le(raw_bytes, p_pos + 4 + j * 4)))
                    .collect();
                planet_ids.sort_unstable();

                let sys = sde
                    .solar_systems
                    .get(&sid)
                    .expect("solar system must exist in sde.solar_systems");
                assert_eq!(sys.solar_system_id, sid);
                assert_eq!(sys.constellation_id, con_id);
                assert_eq!(sys.region_id, reg_id);
                assert!((sys.security_status - f64::from(sec_f32)).abs() < 1e-6);
                assert!(sys.radius > 0.0 && sys.radius.is_finite());
                assert_eq!(
                    sys.galactic_pos_ly,
                    [cx / METERS_PER_LY, cy / METERS_PER_LY, cz / METERS_PER_LY]
                );
                assert_eq!(sys.planet_ids, planet_ids);

                let (db_x, db_y, db_z, db_lum, db_sun, db_fac, db_wh) = db_sys[&sid];
                assert_eq!((db_x, db_y, db_z), (cx, cy, cz));
                assert!(db_lum >= 0.0 && db_lum.is_finite());
                assert_eq!(db_sun, sun_type_id);
                assert_eq!(db_fac, faction_id);
                assert_eq!(db_wh, wormhole_class_id);
            }
            true
        }
        _ => false,
    }
}

#[test]
// parity: plan:T-1, plan:T-2, plan:D-2
// parity: sde:res:/staticdata/blueprints.static, sde:res:/staticdata/clonegrades.static, sde:res:/staticdata/dbuffcollections.static
// parity: sde:res:/staticdata/fighterabilities.static, sde:res:/staticdata/fighterabilitiesbytype.static
// parity: sde:res:/staticdata/skinlicenses.static, sde:res:/staticdata/skinmaterials.static, sde:res:/staticdata/skins.static
// parity: sde:res:/staticdata/asteroidcountbytypeidroomiddungeonid.static, sde:res:/staticdata/gastypesbydungeonid.static
// parity: sde:res:/staticdata/regions.static, sde:res:/staticdata/constellations.static, sde:res:/staticdata/jumps.static, sde:res:/staticdata/systems.static
fn test_t2_sde_all_213_resources_and_domain_tables_sweep() {
    let root = repo_root();
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("real V24.01 SDE database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let sde_conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();

    // 1. Load data/sde_import_manifest.json and docs/index/staticdata_catalog.json
    let manifest_path = root.join("data").join("sde_import_manifest.json");
    let manifest_val: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path).expect("read data/sde_import_manifest.json"),
    )
    .expect("parse data/sde_import_manifest.json");

    assert_eq!(manifest_val["schema_version"].as_i64(), Some(1));
    assert_eq!(manifest_val["client_build"].as_i64(), Some(3_619_879));
    assert_eq!(manifest_val["client_release"].as_str(), Some("V24.01"));

    let catalog_path = root
        .join("docs")
        .join("index")
        .join("staticdata_catalog.json");
    let catalog_arr: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(&catalog_path).expect("read docs/index/staticdata_catalog.json"),
    )
    .expect("parse docs/index/staticdata_catalog.json");
    assert_eq!(catalog_arr.len(), 213);
    let catalog_by_res: HashMap<String, &serde_json::Value> = catalog_arr
        .iter()
        .map(|v| (v["res_path"].as_str().unwrap().to_string(), v))
        .collect();

    let manifest_resources = manifest_val["resources"]
        .as_array()
        .expect("manifest resources array");
    assert_eq!(manifest_resources.len(), 213);
    assert_eq!(sde.resources.len(), 213);

    let manifest_by_res: HashMap<String, &serde_json::Value> = manifest_resources
        .iter()
        .map(|v| (v["res_path"].as_str().unwrap().to_string(), v))
        .collect();

    // Unconditionally require the local V24.01 client SharedCache/ResFiles directory (R-P1-3)
    let home = std::env::var("HOME").expect("HOME environment variable must be set");
    let resfiles_dir =
        PathBuf::from(&home).join("Library/Application Support/EVE Online/SharedCache/ResFiles");
    assert!(
        resfiles_dir.is_dir(),
        "V24.01 client SharedCache/ResFiles directory must exist at {:?} for independent SDE verification",
        resfiles_dir
    );

    let mut attested_sde_rows: Vec<String> = Vec::new();
    let mut sqlite_static_verified = 0usize;

    for cat_entry in &catalog_arr {
        let res_path = cat_entry["res_path"].as_str().expect("catalog res_path");
        let expected_md5 = cat_entry["md5"].as_str().expect("catalog md5");
        let row_id = format!("sde:{res_path}");

        let info = sde
            .resources
            .get(res_path)
            .unwrap_or_else(|| panic!("Missing SDE resource in sde.resources: {res_path}"));
        let man = manifest_by_res
            .get(res_path)
            .unwrap_or_else(|| panic!("Missing SDE resource in manifest: {res_path}"));
        let cat = catalog_by_res
            .get(res_path)
            .unwrap_or_else(|| panic!("Missing SDE resource in catalog: {res_path}"));

        // Verify md5 across sde_resources, manifest, and staticdata_catalog
        assert_eq!(info.md5.len(), 32, "Invalid MD5 length for {res_path}");
        assert!(
            info.md5.chars().all(|c| c.is_ascii_hexdigit()),
            "Non-hex MD5 for {res_path}"
        );
        assert_eq!(
            info.md5, expected_md5,
            "MD5 mismatch vs catalog for {res_path}"
        );
        assert_eq!(
            Some(info.md5.as_str()),
            man["md5"].as_str(),
            "MD5 mismatch vs manifest for {res_path}"
        );
        assert_eq!(
            Some(info.md5.as_str()),
            cat["md5"].as_str(),
            "MD5 mismatch vs staticdata_catalog for {res_path}"
        );

        // Verify cdn_relative_path and format
        assert!(!info.cdn_relative_path.is_empty());
        assert!(
            info.cdn_relative_path.ends_with(&format!("_{}", info.md5)),
            "cdn_relative_path {} must end with _{}",
            info.cdn_relative_path,
            info.md5
        );
        assert_eq!(
            Some(info.cdn_relative_path.as_str()),
            man["cdn_relative_path"].as_str()
        );
        assert_eq!(
            Some(info.cdn_relative_path.as_str()),
            cat["cdn_relative_path"].as_str()
        );
        assert!(
            matches!(
                info.format.as_str(),
                "FSD Binary" | "SQLite3" | "Pickle/Binary"
            ),
            "Unexpected format {} for {res_path}",
            info.format
        );
        assert_eq!(Some(info.format.as_str()), man["format"].as_str());
        assert_eq!(Some(info.format.as_str()), cat["format"].as_str());

        // Verify byte_size and record_count
        assert!(info.byte_size > 0, "Zero byte_size for {res_path}");
        assert_eq!(Some(info.byte_size), man["byte_size"].as_u64());
        assert_eq!(Some(info.byte_size), cat["uncompressed_bytes"].as_u64());
        assert_eq!(
            Some(info.record_count as u64),
            man["record_count"].as_u64(),
            "record_count mismatch vs manifest for {res_path}"
        );
        if res_path == "res:/staticdata/skilltags.static" {
            assert_eq!(info.record_count, 0);
            assert_eq!(info.spot, "empty");
        } else {
            assert!(
                info.record_count > 0,
                "Expected positive record_count for {res_path}"
            );
            assert!(!info.spot.is_empty(), "Empty spot value for {res_path}");
        }

        // Independently verify raw client file byte size and RFC 1321 MD5 digest in ResFiles/
        let client_file = resfiles_dir.join(&info.cdn_relative_path);
        let raw_bytes = std::fs::read(&client_file).unwrap_or_else(|e| {
            panic!(
                "Missing client resource file {:?} for {res_path}: {e}",
                client_file
            )
        });
        assert_eq!(
            raw_bytes.len() as u64,
            info.byte_size,
            "Client file size mismatch for {res_path}"
        );
        let computed_md5 = md5_hex(&raw_bytes);
        assert_eq!(
            computed_md5, expected_md5,
            "Client file MD5 mismatch vs catalog for {res_path}"
        );

        // For SQLite3 / .static client files, independently verify record_count, spot key, and all loaded rows
        if info.format == "SQLite3" {
            let c_conn = if raw_bytes.starts_with(b"SQLite format 3\0") {
                let conn =
                    Connection::open_with_flags(&client_file, OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .expect("open client sqlite .static");
                let cache_cnt: i64 = conn
                    .query_row("SELECT count(*) FROM cache", [], |r| r.get(0))
                    .expect("query cache count in client .static");
                assert_eq!(
                    cache_cnt as usize, info.record_count,
                    "Client SQLite cache count mismatch for {res_path}"
                );
                let first_key: String = conn
                    .query_row("SELECT CAST(key AS TEXT) FROM cache LIMIT 1", [], |r| {
                        r.get(0)
                    })
                    .expect("query first key in client .static");
                assert_eq!(
                    first_key, info.spot,
                    "Client SQLite spot key mismatch for {res_path}"
                );
                sqlite_static_verified += 1;
                Some(conn)
            } else {
                None
            };

            // Only attest sde: rows that are both loaded into SdeRegistry AND independently verified
            // across all rows and fields against the client file (R-P0-1, R5-P1-1).
            if verify_client_sqlite_static_loaded_rows(
                res_path,
                c_conn.as_ref(),
                &raw_bytes,
                info.record_count,
                &sde,
                &sde_conn,
            ) {
                attested_sde_rows.push(row_id);
            }
        }
    }

    assert_eq!(sqlite_static_verified, 14);
    assert_eq!(attested_sde_rows.len(), 14);

    // 2. Verify all 46 SQLite table_counts in manifest against data/evomu_sde.sqlite and SdeRegistry
    let tc = manifest_val["table_counts"]
        .as_object()
        .expect("table_counts object");
    assert_eq!(tc.len(), 46);
    for (table_name, expected_val) in tc {
        let expected_cnt = expected_val.as_u64().expect("u64 table count") as usize;
        let actual_cnt: i64 = sde_conn
            .query_row(&format!("SELECT count(*) FROM \"{table_name}\""), [], |r| {
                r.get(0)
            })
            .unwrap_or_else(|e| panic!("Failed counting {table_name}: {e}"));
        assert_eq!(
            actual_cnt as usize, expected_cnt,
            "SQLite table count mismatch for {table_name}"
        );
    }

    // Per-dungeon asteroid layouts and gas-site gas types load into SdeRegistry 1:1
    assert_eq!(
        sde.dungeon_asteroids
            .values()
            .map(|v| v.len())
            .sum::<usize>(),
        tc["sde_dungeon_asteroid_counts"].as_u64().unwrap() as usize
    );
    assert_eq!(
        sde.dungeon_gas_types
            .values()
            .map(|v| v.len())
            .sum::<usize>(),
        tc["sde_dungeon_gas_types"].as_u64().unwrap() as usize
    );

    // Verify exact 1:1 correspondence between .static client resource record_counts and SdeRegistry domain tables
    assert_eq!(
        sde.blueprints.len(),
        sde.resources["res:/staticdata/blueprints.static"].record_count
    );
    assert_eq!(
        sde.regions.len(),
        sde.resources["res:/staticdata/regions.static"].record_count
    );
    assert_eq!(
        sde.constellations.len(),
        sde.resources["res:/staticdata/constellations.static"].record_count
    );
    assert_eq!(
        sde.solar_systems.len(),
        sde.resources["res:/staticdata/systems.static"].record_count
    );
    assert_eq!(
        sde.solar_systems.len(),
        sde.resources["res:/staticdata/solarsystemcontent.static"].record_count
    );
    assert_eq!(
        sde.jumps.len(),
        sde.resources["res:/staticdata/jumps.static"].record_count * 2
    );
    assert_eq!(
        sde.clone_grades.len(),
        sde.resources["res:/staticdata/clonegrades.static"].record_count
    );
    assert_eq!(
        sde.dbuff_collections.len(),
        sde.resources["res:/staticdata/dbuffcollections.static"].record_count
    );
    assert_eq!(
        sde.fighter_abilities.len(),
        sde.resources["res:/staticdata/fighterabilities.static"].record_count
    );
    assert_eq!(
        sde.fighter_abilities_by_type.len(),
        sde.resources["res:/staticdata/fighterabilitiesbytype.static"].record_count
    );
    assert_eq!(
        sde.skin_licenses.len(),
        sde.resources["res:/staticdata/skinlicenses.static"].record_count
    );
    assert_eq!(
        sde.skins.len(),
        sde.resources["res:/staticdata/skins.static"].record_count
    );
    assert_eq!(
        sde.skin_materials.len(),
        sde.resources["res:/staticdata/skinmaterials.static"].record_count
    );

    // 3. Domain sweep: Blueprints (5,100)
    let mut valid_bp_products = 0usize;
    let mut total_bp_mats = 0usize;
    let mut valid_bp_mats = 0usize;
    for bp in sde.blueprints.values() {
        assert!(sde.types.contains_key(&bp.blueprint_type_id));
        if sde.types.contains_key(&bp.product_type_id) {
            valid_bp_products += 1;
        }
        for mat in &bp.materials {
            total_bp_mats += 1;
            if sde.types.contains_key(&mat.type_id) {
                valid_bp_mats += 1;
            }
            assert!(mat.quantity >= 1);
        }
    }
    assert_eq!(valid_bp_products, 5_080);
    assert_eq!(total_bp_mats, 27_123);
    assert_eq!(valid_bp_mats, 27_122);
    let rifter_bp = &sde.blueprints[&691];
    assert_eq!(rifter_bp.product_type_id, 587);
    assert_eq!(rifter_bp.product_quantity, 1);
    assert_eq!(rifter_bp.manufacturing_time_sec, 6_000);
    let raven_bp = &sde.blueprints[&688];
    assert_eq!(raven_bp.product_type_id, 638);
    assert_eq!(raven_bp.manufacturing_time_sec, 18_000);

    // 4. Domain sweep: Universe Map (Regions, Constellations, SolarSystems, Stargates, Stations, Celestials)
    let mut total_stations = 0usize;
    for sys in sde.solar_systems.values() {
        assert!(!sys.name.is_empty());
        assert!(sys.radius > 0.0 && sys.radius.is_finite());
        let con = sde
            .constellations
            .get(&sys.constellation_id)
            .expect("system constellation must exist");
        assert_eq!(con.region_id, sys.region_id);
        assert!(sde.regions.contains_key(&sys.region_id));
        for sta in &sys.stations {
            total_stations += 1;
            assert!(!sta.name.is_empty());
            assert!(sde.types.contains_key(&sta.station_type_id));
            assert!(sde.npc_corporations.contains_key(&sta.owner_corporation_id));
        }
    }
    assert_eq!(total_stations, 5_210);

    // parity: plan:D-2
    // SdeRegistry::stations indexed map for O(1) station lookups
    assert_eq!(sde.stations.len(), 5_210);
    for (sid, sta) in &sde.stations {
        assert_eq!(*sid, sta.station_id);
        assert!(!sta.name.is_empty());
        assert!(sde.types.contains_key(&sta.station_type_id));
        assert!(sde.npc_corporations.contains_key(&sta.owner_corporation_id));
        assert!(sde.solar_systems.contains_key(&sta.solar_system_id));
        assert!(sde.constellations.contains_key(&sta.constellation_id));
        assert!(sde.regions.contains_key(&sta.region_id));
    }
    let jita_4_4 = &sde.stations[&60003760];
    assert_eq!(
        jita_4_4.name,
        "Jita IV - Moon 4 - Caldari Navy Assembly Plant"
    );
    assert_eq!(jita_4_4.solar_system_id, 30000142);
    assert_eq!(jita_4_4.constellation_id, 20000020);
    assert_eq!(jita_4_4.region_id, 10000002);
    assert_eq!(jita_4_4.operation_id, 14);
    assert_eq!(jita_4_4.reprocessing_efficiency, 0.5);
    assert_eq!(jita_4_4.reprocessing_stations_take, 0.05);
    assert!(!jita_4_4.is_conquerable);
    assert_eq!(jita_4_4.orbit_id, Some(40009087));
    assert_eq!(jita_4_4.owner_faction_id, Some(500001));

    // Station without NPC corp faction_id must have None owner_faction_id (no hardcoded fallback 500001)
    let non_empire_sta = &sde.stations[&60014839];
    assert_eq!(non_empire_sta.owner_faction_id, None);

    for gate in sde.jumps.values() {
        assert!(sde.solar_systems.contains_key(&gate.from_solar_system_id));
        assert!(sde.solar_systems.contains_key(&gate.to_solar_system_id));
        let dest = sde
            .jumps
            .get(&gate.destination_stargate_id)
            .expect("destination stargate must exist");
        assert_eq!(dest.stargate_id, gate.destination_stargate_id);
        assert_eq!(dest.destination_stargate_id, gate.stargate_id);
        assert_eq!(dest.from_solar_system_id, gate.to_solar_system_id);
        assert_eq!(dest.to_solar_system_id, gate.from_solar_system_id);
    }

    assert_eq!(sde.regions[&10000002].name, "The Forge");
    assert_eq!(sde.regions[&10000043].name, "Domain");
    assert_eq!(sde.regions[&10000032].name, "Sinq Laison");
    assert_eq!(sde.regions[&10000030].name, "Heimatar");
    assert_eq!(sde.constellations[&20000020].name, "Kimotoro");
    assert_eq!(sde.constellations[&20000322].name, "Throne Worlds");
    assert_eq!(sde.solar_systems[&30002187].name, "Amarr");
    assert_eq!(sde.solar_systems[&30002659].name, "Dodixie");
    assert_eq!(sde.solar_systems[&30002510].name, "Rens");

    // 5. Domain sweep: Wormholes, Clone Grades, Dbuffs, Fighters, Skins
    for wh in sde.wormhole_types.values() {
        assert!(sde.types.contains_key(&wh.type_id));
        assert!(!wh.name.is_empty());
    }
    assert_eq!(sde.wormhole_types[&30677].name, "Wormhole B274");
    assert_eq!(sde.wormhole_types[&30677].max_stable_mass, 2_000_000_000.0);
    assert_eq!(sde.wormhole_types[&30677].max_jump_mass, 375_000_000.0);
    assert_eq!(sde.wormhole_types[&30831].name, "Wormhole K162");

    for cg in sde.clone_grades.values() {
        assert!(!cg.alpha_skill_caps.is_empty());
        for (&stid, &lvl) in &cg.alpha_skill_caps {
            assert!(sde.types.contains_key(&stid));
            assert!(lvl <= 5);
        }
    }

    for dbuff in sde.dbuff_collections.values() {
        assert!(!dbuff.name.is_empty());
        assert!(!dbuff.operation.is_empty());
        assert!(!dbuff.aggregate_mode.is_empty());
    }
    assert!(sde
        .dbuff_collections
        .values()
        .any(|d| !d.item_modifiers.is_empty()));
    assert!(sde
        .dbuff_collections
        .values()
        .any(|d| !d.location_modifiers.is_empty()));
    assert!(sde
        .dbuff_collections
        .values()
        .any(|d| !d.location_group_modifiers.is_empty()));
    assert!(sde
        .dbuff_collections
        .values()
        .any(|d| !d.location_required_skill_modifiers.is_empty()));

    // parity: sde:res:/staticdata/fighterabilities.static
    for fa in sde.fighter_abilities.values() {
        assert!(!fa.target_mode.is_empty());
    }
    let fa2 = &sde.fighter_abilities[&2];
    assert_eq!(fa2.tooltip_text_id, Some(516650));
    let fa18 = &sde.fighter_abilities[&18];
    assert_eq!(fa18.turret_graphic_id, Some(11515));
    assert_eq!(fa18.tooltip_text_id, Some(516660));
    for (&ftid, slots) in &sde.fighter_abilities_by_type {
        assert!(sde.types.contains_key(&ftid));
        assert!(!slots.is_empty());
        for slot in slots {
            assert!(slot.slot_index <= 2);
            assert!(sde.fighter_abilities.contains_key(&slot.ability_id));
            if let Some(cd) = slot.cooldown_seconds {
                assert!(cd > 0.0);
            }
            if let Some(ref ch) = slot.charges {
                assert!(ch.charge_count > 0);
                assert!(ch.rearm_time_seconds > 0.0);
            }
        }
    }

    // parity: sde:res:/staticdata/skins.static
    assert_eq!(sde.skin_materials.len(), 875);
    let mat1 = &sde.skin_materials[&1];
    assert_eq!(mat1.skin_material_id, 1);
    assert_eq!(mat1.display_name_id, Some(505346));
    assert_eq!(mat1.material_set_id, Some(1));

    let mut resolved_skin_licenses = 0usize;
    for sl in sde.skin_licenses.values() {
        if sde.types.contains_key(&sl.license_type_id) && sde.skins.contains_key(&sl.skin_id) {
            resolved_skin_licenses += 1;
        }
    }
    assert_eq!(resolved_skin_licenses, 11_870);
    for sk in sde.skins.values() {
        assert!(!sk.internal_name.is_empty());
        assert!(!sk.types.is_empty());
        assert!(sde.skin_materials.contains_key(&sk.skin_material_id));
    }
    assert_eq!(sde.skins[&5].internal_name, "Megathron Quafe");
    assert_eq!(sde.skins[&5].types, vec![641]);
    assert_eq!(sde.skins[&5].skin_material_id, 3);
    assert!(sde.skins[&5].allow_ccp_devs);
    assert!(!sde.skins[&5].visible_serenity);
    assert!(sde.skins[&5].visible_tranquility);
    assert!(!sde.skins[&5].is_structure_skin);

    // 6. Attest only the 14 sde:res:/... rows that are loaded into SdeRegistry AND independently
    //    verified against the raw client files in ResFiles/ for MD5, byte size, record count, and all loaded rows (R-P0-1).
    let unique_attested: HashSet<&str> = attested_sde_rows.iter().map(String::as_str).collect();
    assert_eq!(unique_attested.len(), 14);
    let attested_refs: Vec<&str> = attested_sde_rows.iter().map(String::as_str).collect();
    eve_testkit::attest(&attested_refs);
}
