//! Data-driven sweep over all 213 V24.01 client static-data resources and domain tables (`T-1`).

use eve_sde::SdeRegistry;
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

mod raw_client;
mod sde_sweep_support;
use raw_client::md5_hex;
use sde_sweep_support::{repo_root, verify_client_sqlite_static_loaded_rows};

#[test]
fn test_t2_sde_all_213_resources_and_domain_tables_sweep() {
    // parity: plan:T-1, plan:D-2
    // parity: sde:res:/staticdata/blueprints.static, sde:res:/staticdata/clonegrades.static, sde:res:/staticdata/dbuffcollections.static
    // parity: sde:res:/staticdata/fighterabilities.static, sde:res:/staticdata/fighterabilitiesbytype.static
    // parity: sde:res:/staticdata/skinlicenses.static, sde:res:/staticdata/skinmaterials.static, sde:res:/staticdata/skins.static
    // parity: sde:res:/staticdata/asteroidcountbytypeidroomiddungeonid.static, sde:res:/staticdata/gastypesbydungeonid.static
    // parity: sde:res:/staticdata/regions.static, sde:res:/staticdata/constellations.static, sde:res:/staticdata/jumps.static, sde:res:/staticdata/systems.static
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
        assert_eq!(Some(info.format.as_str()), man["format"].as_str());

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

        // The importer now labels `format` from the file's magic bytes (T-2). Until
        // data/evomu_sde.sqlite is regenerated, the database still carries the legacy
        // extension-derived catalog label, so accept exactly one of the two.
        let has_sibling_schema = res_path
            .strip_suffix(".static")
            .is_some_and(|stem| catalog_by_res.contains_key(&format!("{stem}.schema")));
        let detected = raw_client::detect_format(&raw_bytes, has_sibling_schema);
        assert!(
            info.format == detected || Some(info.format.as_str()) == cat["format"].as_str(),
            "format {} for {res_path} is neither the detected {detected} nor the catalog label",
            info.format
        );

        // For .static client files, independently verify record_count, spot key, and all loaded rows
        if res_path.ends_with(".static") {
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

    // 2. Verify all 47 SQLite table_counts in manifest against data/evomu_sde.sqlite and SdeRegistry
    let tc = manifest_val["table_counts"]
        .as_object()
        .expect("table_counts object");
    assert_eq!(tc.len(), 47);
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
    let rand_mat_db_cnt: i64 = sde_conn
        .query_row(
            "SELECT count(*) FROM sde_type_materials WHERE randomized_materials_json != '[]'",
            [],
            |r| r.get(0),
        )
        .expect("query sde_type_materials.randomized_materials_json");
    assert_eq!(rand_mat_db_cnt, 10);
    assert_eq!(sde.randomized_type_materials.len(), 10);
    assert_eq!(
        sde.randomized_type_materials
            .values()
            .map(|v| v.len())
            .sum::<usize>(),
        24
    );
    assert_eq!(sde.randomized_type_materials_for(90041).len(), 8);

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
    let jita_4_4 = sde.station(60003760).expect("O(1) station lookup for Jita 4-4");
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
    let non_empire_sta = sde.station(60014839).expect("O(1) station lookup for 60014839");
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
    let (fa_raw_bytes, raw_fas) = sde
        .load_raw_fighter_abilities_static()
        .expect("open raw client fighterabilities.static");
    assert_eq!(fa_raw_bytes, 32_768);
    assert_eq!(raw_fas.len(), 36);
    assert_eq!(raw_fas, sde.fighter_abilities);
    for fa in sde.fighter_abilities.values() {
        assert!(!fa.target_mode.is_empty());
        assert_eq!(fa.ability_name_id, fa.display_name_id);
        assert!(fa.raw_json.is_object());
    }
    let fa2 = &sde.fighter_abilities[&2];
    assert_eq!(fa2.ability_name_id, Some(513537));
    assert_eq!(fa2.tooltip_text_id, Some(516650));
    let fa18 = &sde.fighter_abilities[&18];
    assert_eq!(fa18.turret_graphic_id, Some(11515));
    assert_eq!(fa18.tooltip_text_id, Some(516660));
    assert_eq!(fa18.raw_json["turretGraphicID"], serde_json::json!(11515));
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
    let mut attested_refs: Vec<&str> = attested_sde_rows.iter().map(String::as_str).collect();
    attested_refs.push("plan:T-1");
    attested_refs.push("plan:D-2");
    eve_testkit::attest(&attested_refs);
}
