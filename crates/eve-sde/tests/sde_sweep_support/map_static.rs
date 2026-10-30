use eve_sde::SdeRegistry;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

pub fn read_u32_le(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("u32 slice"))
}

pub fn read_i32_le(buf: &[u8], pos: usize) -> i32 {
    i32::from_le_bytes(buf[pos..pos + 4].try_into().expect("i32 slice"))
}

pub fn read_u64_le(buf: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(buf[pos..pos + 8].try_into().expect("u64 slice"))
}

pub fn read_f32_le(buf: &[u8], pos: usize) -> f32 {
    f32::from_le_bytes(buf[pos..pos + 4].try_into().expect("f32 slice"))
}

pub fn read_f64_le(buf: &[u8], pos: usize) -> f64 {
    f64::from_le_bytes(buf[pos..pos + 8].try_into().expect("f64 slice"))
}

pub fn parse_fsd_dict(buf: &[u8], pos: usize) -> Vec<(u32, usize)> {
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

pub fn parse_fsd_var_offsets<'a>(
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

pub fn verify_client_sqlite_static_map_rows(
    res_path: &str,
    rows: &[(String, serde_json::Value)],
    raw_bytes: &[u8],
    cache_cnt: usize,
    sde: &SdeRegistry,
    sde_conn: &Connection,
) -> bool {
    match res_path {
        "res:/staticdata/skinmaterials.static" => {
            assert_eq!(sde.skin_materials.len(), cache_cnt);
            assert_eq!(cache_cnt, 875);
            for (key_str, row_json) in rows {
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
            for (key_str, row_json) in rows {
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
