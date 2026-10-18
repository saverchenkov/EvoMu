//! Data-driven sweep over all 213 V24.01 client static-data resources and domain tables (`T-1`).

use eve_sde::{attr_ids, SdeRegistry, TypeMaterialEntry};
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

/// Independently verifies a SQLite3 `.static` client file's record count and spot JSON record
/// against the parsed domain table in `SdeRegistry`. Returns `true` iff the resource is loaded
/// into `SdeRegistry` and verified against the client `.static` file.
fn verify_client_sqlite_static_loaded_spot(
    res_path: &str,
    spot_key: &str,
    spot_json: &serde_json::Value,
    cache_cnt: usize,
    sde: &SdeRegistry,
) -> bool {
    match res_path {
        "res:/staticdata/blueprints.static" => {
            assert_eq!(sde.blueprints.len(), cache_cnt);
            let bp_id: u32 = spot_key.parse().expect("blueprint spot key u32");
            let bp = sde
                .blueprints
                .get(&bp_id)
                .expect("spot blueprint must exist in SdeRegistry");
            assert_eq!(
                spot_json["blueprintTypeID"].as_u64(),
                Some(u64::from(bp.blueprint_type_id))
            );
            let mfg = &spot_json["activities"]["manufacturing"];
            assert_eq!(mfg["time"].as_u64(), Some(bp.manufacturing_time_sec));
            let prod = &mfg["products"][0];
            assert_eq!(
                prod["typeID"].as_u64(),
                Some(u64::from(bp.product_type_id))
            );
            assert_eq!(
                prod["quantity"].as_u64(),
                Some(u64::from(bp.product_quantity))
            );
            let client_mats = mfg["materials"].as_array().expect("client blueprint materials");
            assert_eq!(bp.materials.len(), client_mats.len());
            for (actual, expected) in bp.materials.iter().zip(client_mats.iter()) {
                assert_eq!(
                    Some(u64::from(actual.type_id)),
                    expected["typeID"].as_u64()
                );
                assert_eq!(Some(actual.quantity), expected["quantity"].as_u64());
            }
            // Pending lossless non-manufacturing activity loading (R5-P1-1)
            false
        }
        "res:/staticdata/clonegrades.static" => {
            assert_eq!(sde.clone_grades.len(), cache_cnt);
            let grade_id: u32 = spot_key.parse().expect("clonegrade spot key u32");
            let cg = sde
                .clone_grades
                .get(&grade_id)
                .expect("spot clone grade must exist in SdeRegistry");
            let client_skills = spot_json["skills"]
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
            true
        }
        "res:/staticdata/dbuffcollections.static" => {
            assert_eq!(sde.dbuff_collections.len(), cache_cnt);
            let dbuff_id: u32 = spot_key.parse().expect("dbuff spot key u32");
            let dbuff = sde
                .dbuff_collections
                .get(&dbuff_id)
                .expect("spot dbuff collection must exist in SdeRegistry");
            assert_eq!(
                Some(dbuff.operation.as_str()),
                spot_json["operationName"].as_str()
            );
            assert_eq!(
                Some(dbuff.aggregate_mode.as_str()),
                spot_json["aggregateMode"].as_str()
            );
            assert_eq!(
                Some(dbuff.name.as_str()),
                spot_json["developerDescription"].as_str()
            );
            // Pending lossless locationGroupModifiers/locationRequiredSkillModifiers loading (R5-P1-1)
            false
        }
        "res:/staticdata/fighterabilities.static" => {
            assert_eq!(sde.fighter_abilities.len(), cache_cnt);
            let ability_id: u32 = spot_key.parse().expect("fighter ability spot key u32");
            let fa = sde
                .fighter_abilities
                .get(&ability_id)
                .expect("spot fighter ability must exist in SdeRegistry");
            assert_eq!(
                Some(fa.target_mode.as_str()),
                spot_json["targetMode"].as_str()
            );
            assert_eq!(
                fa.disallow_in_high_sec,
                spot_json
                    .get("disallowInHighSec")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            );
            assert_eq!(
                fa.disallow_in_low_sec,
                spot_json
                    .get("disallowInLowSec")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            );
            assert_eq!(
                fa.icon_id.map(u64::from),
                spot_json.get("iconID").and_then(|v| v.as_u64())
            );
            true
        }
        "res:/staticdata/fighterabilitiesbytype.static" => {
            assert_eq!(sde.fighter_abilities_by_type.len(), cache_cnt);
            let type_id: u32 = spot_key.parse().expect("fighter type spot key u32");
            let abilities = sde
                .fighter_abilities_by_type
                .get(&type_id)
                .expect("spot fighter abilities by type must exist in SdeRegistry");
            let mut expected_aids = Vec::new();
            for slot_key in ["abilitySlot0", "abilitySlot1", "abilitySlot2"] {
                if let Some(aid) = spot_json
                    .get(slot_key)
                    .and_then(|s| s.get("abilityID"))
                    .and_then(|v| v.as_u64())
                {
                    expected_aids.push(aid as u32);
                }
            }
            assert_eq!(abilities, &expected_aids);
            // Pending lossless slot cooldownSeconds and charges loading (R5-P1-1)
            false
        }
        "res:/staticdata/skinlicenses.static" => {
            assert_eq!(sde.skin_licenses.len(), cache_cnt);
            let license_tid: u32 = spot_key.parse().expect("skin license spot key u32");
            let sl = sde
                .skin_licenses
                .get(&license_tid)
                .expect("spot skin license must exist in SdeRegistry");
            assert_eq!(
                spot_json["licenseTypeID"].as_u64(),
                Some(u64::from(sl.license_type_id))
            );
            assert_eq!(spot_json["skinID"].as_u64(), Some(u64::from(sl.skin_id)));
            assert_eq!(
                spot_json["duration"].as_i64(),
                Some(i64::from(sl.duration))
            );
            true
        }
        "res:/staticdata/skins.static" => {
            assert_eq!(sde.skins.len(), cache_cnt);
            let skin_id: u32 = spot_key.parse().expect("skin spot key u32");
            let sk = sde
                .skins
                .get(&skin_id)
                .expect("spot skin must exist in SdeRegistry");
            assert_eq!(spot_json["skinID"].as_u64(), Some(u64::from(sk.skin_id)));
            assert_eq!(
                spot_json["internalName"].as_str(),
                Some(sk.internal_name.as_str())
            );
            assert_eq!(
                spot_json["skinMaterialID"].as_u64(),
                Some(u64::from(sk.skin_material_id))
            );
            let expected_types: Vec<u32> = spot_json["types"]
                .as_array()
                .expect("client skin types array")
                .iter()
                .map(|v| v.as_u64().expect("skin type u64") as u32)
                .collect();
            assert_eq!(sk.types, expected_types);
            true
        }
        _ => false,
    }
}

#[test]
fn test_t2_sde_all_213_resources_and_domain_tables_sweep() {
    let root = repo_root();
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("real V24.01 SDE database must open");

    // 1. Load data/sde_import_manifest.json and docs/index/staticdata_catalog.json
    let manifest_path = root.join("data").join("sde_import_manifest.json");
    let manifest_val: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path).expect("read data/sde_import_manifest.json"),
    )
    .expect("parse data/sde_import_manifest.json");

    assert_eq!(manifest_val["schema_version"].as_i64(), Some(1));
    assert_eq!(manifest_val["client_build"].as_i64(), Some(3_619_879));
    assert_eq!(manifest_val["client_release"].as_str(), Some("V24.01"));

    let catalog_path = root.join("docs").join("index").join("staticdata_catalog.json");
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
    let resfiles_dir = PathBuf::from(&home)
        .join("Library/Application Support/EVE Online/SharedCache/ResFiles");
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
        assert_eq!(info.md5, expected_md5, "MD5 mismatch vs catalog for {res_path}");
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
            panic!("Missing client resource file {:?} for {res_path}: {e}", client_file)
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

        // For SQLite3 .static client files, independently verify record_count and spot record
        if info.format == "SQLite3" && raw_bytes.starts_with(b"SQLite format 3\0") {
            let c_conn = Connection::open_with_flags(&client_file, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("open client sqlite .static");
            let cache_cnt: i64 = c_conn
                .query_row("SELECT count(*) FROM cache", [], |r| r.get(0))
                .expect("query cache count in client .static");
            assert_eq!(
                cache_cnt as usize, info.record_count,
                "Client SQLite cache count mismatch for {res_path}"
            );
            let first_key: String = c_conn
                .query_row("SELECT CAST(key AS TEXT) FROM cache LIMIT 1", [], |r| {
                    r.get(0)
                })
                .expect("query first key in client .static");
            assert_eq!(
                first_key, info.spot,
                "Client SQLite spot key mismatch for {res_path}"
            );
            let first_val_str: String = c_conn
                .query_row(
                    "SELECT value FROM cache WHERE CAST(key AS TEXT) = ?1 LIMIT 1",
                    [&first_key],
                    |r| r.get(0),
                )
                .expect("query spot value in client .static");
            let spot_json: serde_json::Value =
                serde_json::from_str(&first_val_str).expect("parse client .static spot JSON");
            sqlite_static_verified += 1;

            // Only attest sde: rows that are both losslessly loaded into SdeRegistry AND independently verified
            // against the client file for MD5, byte size, record count, and spot record contents (R-P0-1, R5-P1-1).
            if verify_client_sqlite_static_loaded_spot(
                res_path,
                &first_key,
                &spot_json,
                cache_cnt as usize,
                &sde,
            ) {
                attested_sde_rows.push(row_id);
            }
        }
    }

    assert_eq!(sqlite_static_verified, 14);
    assert_eq!(attested_sde_rows.len(), 4);

    // 2. Verify all 42 SQLite table_counts in manifest against data/evomu_sde.sqlite and SdeRegistry
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let sde_conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    let tc = manifest_val["table_counts"]
        .as_object()
        .expect("table_counts object");
    assert_eq!(tc.len(), 42);
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

    // Verify exact 1:1 correspondence between client resource record_counts and SdeRegistry domain tables
    assert_eq!(
        sde.categories.len(),
        sde.resources["res:/staticdata/categories.fsdbinary"].record_count
    );
    assert_eq!(
        sde.groups.len(),
        sde.resources["res:/staticdata/groups.fsdbinary"].record_count
    );
    assert_eq!(
        sde.types.len(),
        sde.resources["res:/staticdata/types.fsdbinary"].record_count
    );
    assert_eq!(
        sde.attributes.len(),
        sde.resources["res:/staticdata/dogmaattributes.fsdbinary"].record_count
    );
    assert_eq!(
        sde.effects.len(),
        sde.resources["res:/staticdata/dogmaeffects.fsdbinary"].record_count
    );
    assert_eq!(
        sde.type_materials.len(),
        sde.resources["res:/staticdata/typematerials.fsdbinary"].record_count
    );
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
        sde.races.len(),
        sde.resources["res:/staticdata/races.fsdbinary"].record_count
    );
    assert_eq!(
        sde.bloodlines.len(),
        sde.resources["res:/staticdata/bloodlines.fsdbinary"].record_count
    );
    assert_eq!(
        sde.ancestries.len(),
        sde.resources["res:/staticdata/ancestries.fsdbinary"].record_count
    );
    assert_eq!(
        sde.schools.len(),
        sde.resources["res:/staticdata/schools.fsdbinary"].record_count
    );
    assert_eq!(
        sde.factions.len(),
        sde.resources["res:/staticdata/factions.fsdbinary"].record_count
    );
    assert_eq!(
        sde.npc_corporations.len(),
        sde.resources["res:/staticdata/npccorporations.fsdbinary"].record_count
    );
    assert_eq!(
        sde.market_groups.len(),
        sde.resources["res:/staticdata/marketgroups.fsdbinary"].record_count
    );
    assert_eq!(
        sde.planet_schematics.len(),
        sde.resources["res:/staticdata/schematics.fsdbinary"].record_count
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
        sde.compressible_types.len(),
        sde.resources["res:/staticdata/compressibletypes.fsdbinary"].record_count
    );
    assert_eq!(
        sde.dynamic_item_attributes.len(),
        sde.resources["res:/staticdata/dynamicitemattributes.fsdbinary"].record_count
    );
    assert_eq!(
        sde.space_components_by_type.len(),
        sde.resources["res:/staticdata/spacecomponentsbytype.fsdbinary"].record_count
    );
    assert_eq!(
        sde.missions.len(),
        sde.resources["res:/staticdata/missions.fsdbinary"].record_count
    );
    assert_eq!(
        sde.dungeons.len(),
        sde.resources["res:/staticdata/dungeons.fsdbinary"].record_count
    );
    assert_eq!(
        sde.epic_arcs.len(),
        sde.resources["res:/staticdata/epicarcs.fsdbinary"].record_count
    );
    assert_eq!(
        sde.node_graphs.len(),
        sde.resources["res:/staticdata/nodegraphs.fsdbinary"].record_count
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
        sde.station_operations.len(),
        sde.resources["res:/staticdata/stationoperations.fsdbinary"].record_count
    );
    assert_eq!(
        sde.station_services.len(),
        sde.resources["res:/staticdata/stationservices.fsdbinary"].record_count
    );

    // 3. Domain sweep: Categories (48) & Groups (1,611)
    for cat in sde.categories.values() {
        assert!(!cat.name.is_empty());
    }
    assert_eq!(sde.categories[&4].name, "Material");
    assert_eq!(sde.categories[&6].name, "Ship");
    assert_eq!(sde.categories[&7].name, "Module");
    assert_eq!(sde.categories[&8].name, "Charge");
    assert_eq!(sde.categories[&9].name, "Blueprint");
    assert_eq!(sde.categories[&16].name, "Skill");
    assert_eq!(sde.categories[&18].name, "Drone");
    assert_eq!(sde.categories[&25].name, "Asteroid");
    assert_eq!(sde.categories[&32].name, "Subsystem");
    assert_eq!(sde.categories[&87].name, "Fighter");

    for grp in sde.groups.values() {
        assert!(!grp.name.is_empty());
        assert!(
            sde.categories.contains_key(&grp.category_id),
            "Group {} references missing category {}",
            grp.group_id,
            grp.category_id
        );
    }
    assert_eq!(sde.groups[&18].name, "Mineral");
    assert_eq!(sde.groups[&18].category_id, 4);
    assert_eq!(sde.groups[&25].name, "Frigate");
    assert_eq!(sde.groups[&25].category_id, 6);
    assert_eq!(sde.groups[&27].name, "Battleship");
    assert_eq!(sde.groups[&27].category_id, 6);
    assert_eq!(sde.groups[&46].name, "Propulsion Module");
    assert_eq!(sde.groups[&46].category_id, 7);
    assert_eq!(sde.groups[&55].name, "Projectile Weapon");
    assert_eq!(sde.groups[&55].category_id, 7);
    assert_eq!(sde.groups[&462].name, "Veldspar");
    assert_eq!(sde.groups[&462].category_id, 25);

    // 4. Domain sweep: Types (53,288), Wrecks (5,703), Variations, Attributes (2,871), Effects (3,427)
    for attr in sde.attributes.values() {
        assert!(!attr.name.is_empty());
        assert!(attr.default_value.is_finite());
    }
    assert_eq!(sde.attributes[&attr_ids::MAX_VELOCITY].name, "maxVelocity");
    assert!(sde.attributes[&attr_ids::MAX_VELOCITY].high_is_good);
    assert!(!sde.attributes[&attr_ids::MAX_VELOCITY].stackable);
    assert_eq!(sde.attributes[&attr_ids::CAPACITY].name, "capacity");
    assert!(sde.attributes[&attr_ids::CAPACITY].stackable);
    assert_eq!(
        sde.attributes[&attr_ids::DAMAGE_MULTIPLIER].default_value,
        1.0
    );
    assert!(!sde.attributes[&attr_ids::DAMAGE_MULTIPLIER].stackable);

    let mut total_modifiers = 0usize;
    let mut effect_stopper_count = 0usize;
    for eff in sde.effects.values() {
        assert!(!eff.name.is_empty());
        for m in &eff.modifiers {
            total_modifiers += 1;
            if m.func == "EffectStopper" {
                effect_stopper_count += 1;
                let stopped_eid = m.effect_id.expect("EffectStopper must specify effect_id");
                assert!(sde.effects.contains_key(&stopped_eid));
            } else {
                assert!(
                    sde.attributes.contains_key(&m.modified_attribute_id),
                    "Effect {} references missing modified_attribute_id {}",
                    eff.effect_id,
                    m.modified_attribute_id
                );
                assert!(
                    sde.attributes.contains_key(&m.modifying_attribute_id),
                    "Effect {} references missing modifying_attribute_id {}",
                    eff.effect_id,
                    m.modifying_attribute_id
                );
                if let Some(gid) = m.group_id {
                    assert!(sde.groups.contains_key(&gid));
                }
                if let Some(stid) = m.skill_type_id {
                    assert!(sde.types.contains_key(&stid));
                }
            }
        }
    }
    assert_eq!(total_modifiers, 5_178);
    assert_eq!(effect_stopper_count, 10);

    for t in sde.types.values() {
        assert!(!t.name.is_empty());
        let grp = sde
            .groups
            .get(&t.group_id)
            .unwrap_or_else(|| panic!("Type {} missing group {}", t.type_id, t.group_id));
        assert_eq!(t.category_id, grp.category_id);
        assert!(t.portion_size >= 1);
        assert!(t.mass >= 0.0 && t.mass.is_finite());
        assert!(t.volume >= 0.0 && t.volume.is_finite());
        assert!(t.radius >= 0.0 && t.radius.is_finite());
        for &eid in &t.effect_ids {
            assert!(
                sde.effects.contains_key(&eid),
                "Type {} references missing effect {}",
                t.type_id,
                eid
            );
        }
        for (&aid, &val) in &t.default_attributes {
            assert!(
                sde.attributes.contains_key(&aid),
                "Type {} references missing attribute {}",
                t.type_id,
                aid
            );
            assert!(val.is_finite());
        }
        if let Some(parent_id) = t.variation_parent_type_id {
            assert!(sde.variations_of(parent_id).contains(&t.type_id));
        }
    }
    assert_eq!(sde.wrecks.len(), 5_703);
    for (&ship_tid, &wreck_tid) in &sde.wrecks {
        assert!(sde.types.contains_key(&ship_tid));
        assert!(sde.types.contains_key(&wreck_tid));
    }

    // Spot-check Raven (638) and minerals (34..37)
    let raven = &sde.types[&638];
    assert_eq!(raven.name, "Raven");
    assert_eq!(raven.group_id, 27);
    assert_eq!(raven.category_id, 6);
    assert_eq!(raven.default_attributes[&attr_ids::MAX_VELOCITY], 113.0);
    assert_eq!(raven.default_attributes[&attr_ids::SHIELD_CAPACITY], 7_700.0);
    assert_eq!(raven.default_attributes[&attr_ids::ARMOR_HP], 6_380.0);
    assert_eq!(raven.default_attributes[&attr_ids::HP_STRUCT], 7_040.0);
    assert_eq!(raven.default_attributes[&attr_ids::RIG_SIZE], 3.0);

    // 5. Domain sweep: Type Materials (9,866), Compressible Types (212), Blueprints (5,100)
    assert!(!sde.type_materials.contains_key(&34)); // Tritanium cannot be reprocessed
    let mut non_empty_type_materials = 0usize;
    for (&tid, mats) in &sde.type_materials {
        assert!(sde.types.contains_key(&tid));
        if !mats.is_empty() {
            non_empty_type_materials += 1;
        }
        for m in mats {
            assert!(sde.types.contains_key(&m.material_type_id));
            assert!(m.quantity >= 1);
        }
    }
    assert_eq!(non_empty_type_materials, 9_556);
    assert_eq!(
        sde.type_materials[&1230],
        vec![TypeMaterialEntry {
            material_type_id: 34,
            quantity: 400
        }]
    );
    assert_eq!(
        sde.type_materials[&1228],
        vec![
            TypeMaterialEntry {
                material_type_id: 34,
                quantity: 150
            },
            TypeMaterialEntry {
                material_type_id: 35,
                quantity: 110
            },
        ]
    );
    assert_eq!(
        sde.type_materials[&1224],
        vec![
            TypeMaterialEntry {
                material_type_id: 35,
                quantity: 90
            },
            TypeMaterialEntry {
                material_type_id: 36,
                quantity: 30
            },
        ]
    );
    assert_eq!(
        sde.type_materials[&18],
        vec![
            TypeMaterialEntry {
                material_type_id: 34,
                quantity: 175
            },
            TypeMaterialEntry {
                material_type_id: 36,
                quantity: 70
            },
        ]
    );

    for (&raw_tid, &comp_tid) in &sde.compressible_types {
        let raw_t = sde.types.get(&raw_tid).expect("raw compressible type");
        let comp_t = sde.types.get(&comp_tid).expect("compressed target type");
        assert!(
            comp_t.volume < raw_t.volume,
            "Compressed type {} volume {} must be < raw type {} volume {}",
            comp_t.name,
            comp_t.volume,
            raw_t.name,
            raw_t.volume
        );
    }
    assert_eq!(sde.compressible_types[&1230], 62516);
    assert_eq!(sde.types[&62516].name, "Compressed Veldspar");
    assert_eq!(sde.types[&62516].volume, 0.001);
    assert_eq!(sde.compressible_types[&1228], 62520);
    assert_eq!(sde.types[&62520].name, "Compressed Scordite");
    assert_eq!(sde.compressible_types[&1224], 62524);
    assert_eq!(sde.types[&62524].name, "Compressed Pyroxeres");
    assert_eq!(sde.compressible_types[&18], 62528);
    assert_eq!(sde.types[&62528].name, "Compressed Plagioclase");

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

    // 6. Domain sweep: Universe Map (Regions, Constellations, SolarSystems, Stargates, Stations, Celestials)
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

    // 7. Domain sweep: Lineages, Schools, Factions, NPC Corps, Agents
    for race in sde.races.values() {
        assert!(!race.name.is_empty());
        assert!(sde.types.contains_key(&race.corvette_type_id));
    }
    assert_eq!(sde.races[&1].name, "Caldari");
    assert_eq!(sde.races[&1].corvette_type_id, 601);
    assert_eq!(sde.races[&2].name, "Minmatar");
    assert_eq!(sde.races[&2].corvette_type_id, 588);
    assert_eq!(sde.races[&4].name, "Amarr");
    assert_eq!(sde.races[&4].corvette_type_id, 596);
    assert_eq!(sde.races[&8].name, "Gallente");
    assert_eq!(sde.races[&8].corvette_type_id, 606);

    for bl in sde.bloodlines.values() {
        assert!(!bl.name.is_empty());
        assert!(sde.races.contains_key(&bl.race_id));
        assert!(sde.npc_corporations.contains_key(&bl.corporation_id));
    }
    for anc in sde.ancestries.values() {
        assert!(!anc.name.is_empty());
        assert!(sde.bloodlines.contains_key(&anc.bloodline_id));
    }
    for sch in sde.schools.values() {
        assert!(!sch.name.is_empty());
        assert!(sde.races.contains_key(&sch.race_id));
        assert!(sde.npc_corporations.contains_key(&sch.corporation_id));
    }
    for fac in sde.factions.values() {
        assert!(!fac.name.is_empty());
        if let Some(cid) = fac.corporation_id {
            assert!(sde.npc_corporations.contains_key(&cid));
        }
        if let Some(sys_id) = fac.solar_system_id {
            assert!(sde.solar_systems.contains_key(&sys_id));
        }
    }
    assert_eq!(sde.factions[&500001].name, "Caldari State");
    assert_eq!(sde.factions[&500002].name, "Minmatar Republic");
    assert_eq!(sde.factions[&500003].name, "Amarr Empire");
    assert_eq!(sde.factions[&500004].name, "Gallente Federation");

    for corp in sde.npc_corporations.values() {
        assert!(!corp.name.is_empty());
        assert!(!corp.ticker_name.is_empty());
        assert!((0.0..=1.0).contains(&corp.tax_rate));
    }
    assert_eq!(sde.npc_corporations[&1000035].name, "Caldari Navy");
    assert_eq!(sde.npc_corporations[&1000035].ticker_name, "CN");
    assert_eq!(sde.npc_corporations[&1000125].name, "CONCORD");
    assert_eq!(sde.npc_corporations[&1000125].ticker_name, "CONCO");

    for ag in sde.agents.values() {
        assert!(!ag.name.is_empty());
        assert!(sde.npc_corporations.contains_key(&ag.corporation_id));
        assert!((1..=5).contains(&ag.level));
    }
    assert_eq!(sde.agents[&3008416].name, "Antaken Kamola");
    assert_eq!(sde.agents[&3008416].corporation_id, 1000002);

    // 8. Domain sweep: Market Groups, Planet Schematics, Wormholes, Clone Grades, Dbuffs, Fighters,
    //    Mutaplasmids, Space Components, Dungeons, Missions, Epic Arcs, Node Graphs, Skins, Station Ops/Services
    for mg in sde.market_groups.values() {
        assert!(!mg.name.is_empty());
        if let Some(pid) = mg.parent_group_id {
            assert!(sde.market_groups.contains_key(&pid));
        }
    }
    assert_eq!(sde.market_groups[&4].name, "Ships");
    assert_eq!(sde.market_groups[&9].name, "Ship Equipment");

    for ps in sde.planet_schematics.values() {
        assert!(!ps.name.is_empty());
        assert!(ps.cycle_time > 0);
        assert!(!ps.pins.is_empty());
        assert!(ps.types.iter().any(|t| t.is_input));
        assert!(ps.types.iter().any(|t| !t.is_input));
        for pt in &ps.types {
            assert!(sde.types.contains_key(&pt.type_id));
            assert!(pt.quantity >= 1);
        }
    }
    assert_eq!(sde.planet_schematics[&65].name, "Superconductors");
    assert_eq!(sde.planet_schematics[&65].cycle_time, 3_600);
    assert_eq!(sde.planet_schematics[&66].name, "Coolant");
    assert_eq!(sde.planet_schematics[&67].name, "Rocket Fuel");

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
        assert!(!dbuff.operation.is_empty());
        assert!(!dbuff.aggregate_mode.is_empty());
    }

    for fa in sde.fighter_abilities.values() {
        assert!(!fa.target_mode.is_empty());
    }
    for (&ftid, aids) in &sde.fighter_abilities_by_type {
        assert!(sde.types.contains_key(&ftid));
        assert!(!aids.is_empty());
        for aid in aids {
            assert!(sde.fighter_abilities.contains_key(aid));
        }
    }

    for muta in sde.dynamic_item_attributes.values() {
        assert!(sde.types.contains_key(&muta.mutaplasmid_type_id));
        assert!(sde.types.contains_key(&muta.resulting_type_id));
        assert!(!muta.applicable_type_ids.is_empty());
        assert!(!muta.attribute_ranges.is_empty());
    }

    let mut non_empty_space_components = 0usize;
    for (&tid, comps) in &sde.space_components_by_type {
        assert!(sde.types.contains_key(&tid));
        if !comps.is_empty() {
            non_empty_space_components += 1;
        }
    }
    assert_eq!(non_empty_space_components, 11_180);
    assert!(sde.space_components_by_type[&35832].contains(&"autoWarpScrambler".to_string()));

    for d in sde.dungeons.values() {
        assert!(!d.name.is_empty());
    }
    assert_eq!(sde.dungeons[&43].name, "Pith Merchant Depot");

    for m in sde.missions.values() {
        assert!(!m.title.is_empty());
    }
    assert_eq!(sde.missions[&875].title, "The Guristas Spies");

    for ea in sde.epic_arcs.values() {
        assert!(!ea.name.is_empty());
        assert!(ea.missions_count > 0);
    }
    assert_eq!(sde.epic_arcs[&29].name, "The Blood-Stained Stars");
    assert_eq!(sde.epic_arcs[&40].name, "Penumbra");
    assert_eq!(sde.epic_arcs[&48].name, "Right to Rule");
    assert_eq!(sde.epic_arcs[&52].name, "Syndication");
    assert_eq!(sde.epic_arcs[&53].name, "Wildfire");

    for ng in sde.node_graphs.values() {
        assert!(!ng.name.is_empty());
        assert!(ng.node_count > 0);
    }

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
    }
    assert_eq!(sde.skins[&5].internal_name, "Megathron Quafe");
    assert_eq!(sde.skins[&5].types, vec![641]);

    for op in sde.station_operations.values() {
        assert!(!op.name.is_empty());
        for svc_id in &op.services {
            assert!(sde.station_services.contains_key(svc_id));
        }
    }
    assert_eq!(sde.station_services[&1].name, "Bounty Missions");
    assert_eq!(sde.station_services[&2].name, "Assassination Missions");
    assert_eq!(sde.station_services[&3].name, "Courier Missions");

    // 9. Attest only the 4 sde:res:/... SQLite rows that are losslessly loaded into SdeRegistry AND independently
    //    verified against the raw client files in ResFiles/ for MD5, byte size, record count, and spot record (R-P0-1, R5-P1-1).
    let unique_attested: HashSet<&str> = attested_sde_rows.iter().map(String::as_str).collect();
    assert_eq!(unique_attested.len(), 4);
    let attested_refs: Vec<&str> = attested_sde_rows.iter().map(String::as_str).collect();
    eve_testkit::attest(&attested_refs);
}
