use eve_sde::SdeRegistry;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub mod map_static;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Independently verifies every row and every loaded field of a `.static` client file
/// against the parsed domain table in `SdeRegistry`. Returns `true` iff the resource is loaded
/// into `SdeRegistry` and verified across all rows against the client `.static` file.
pub fn verify_client_sqlite_static_loaded_rows(
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
        _ => map_static::verify_client_sqlite_static_map_rows(
            res_path, &rows, raw_bytes, cache_cnt, sde, sde_conn,
        ),
    }
}
