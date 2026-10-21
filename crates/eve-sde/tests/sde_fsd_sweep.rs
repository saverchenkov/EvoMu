//! Independent raw client-file verification of V24.01 `.fsdbinary` resources (`T-1`, `T-2`, `D-2`).
//!
//! Directly parses the raw `cfsd` binary files in `SharedCache/ResFiles/` and verifies byte size,
//! RFC 1321 MD5 digest, record count, hash bucket count, and every loaded record and field
//! against `SdeRegistry` and `evomu_sde.sqlite`.

use eve_sde::{
    attr_ids, EffectModifierSpec, PlanetSchematicTypeEntry, SdeRegistry, TypeMaterialEntry,
};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::PathBuf;

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

fn read_i64_le(buf: &[u8], pos: usize) -> i64 {
    i64::from_le_bytes(buf[pos..pos + 8].try_into().expect("i64 slice"))
}

fn read_f32_le(buf: &[u8], pos: usize) -> f32 {
    f32::from_le_bytes(buf[pos..pos + 4].try_into().expect("f32 slice"))
}

fn read_f64_le(buf: &[u8], pos: usize) -> f64 {
    f64::from_le_bytes(buf[pos..pos + 8].try_into().expect("f64 slice"))
}

fn read_cfsd_str(buf: &[u8], rel_off: u64) -> String {
    if rel_off == 0 {
        return String::new();
    }
    let abs_off = 32 + rel_off as usize;
    let byte_len = read_u64_le(buf, abs_off - 8) as usize;
    std::str::from_utf8(&buf[abs_off..abs_off + byte_len])
        .expect("valid UTF-8 cfsd string")
        .to_string()
}

fn read_cfsd_u32_vec(buf: &[u8], rel_off: u64) -> Vec<u32> {
    if rel_off == 0 {
        return Vec::new();
    }
    let abs_off = 32 + rel_off as usize;
    let cnt = read_u64_le(buf, abs_off - 8) as usize;
    (0..cnt)
        .map(|i| read_u32_le(buf, abs_off + i * 4))
        .collect()
}

/// Opens a raw `.fsdbinary` client file in `ResFiles/`, verifies its byte size, MD5, and `cfsd`
/// record count against `sde.resources[res_path]`, and returns the raw bytes and record offsets
/// for the given fixed record `stride`.
fn load_and_verify_cfsd_records(
    resfiles_dir: &std::path::Path,
    sde: &SdeRegistry,
    res_path: &str,
    stride: usize,
) -> (Vec<u8>, Vec<usize>) {
    let info = sde
        .resources
        .get(res_path)
        .unwrap_or_else(|| panic!("Missing SDE resource {res_path}"));
    assert_eq!(info.format, "FSD Binary");
    let client_file = resfiles_dir.join(&info.cdn_relative_path);
    let raw = std::fs::read(&client_file)
        .unwrap_or_else(|e| panic!("Missing client file {:?} for {res_path}: {e}", client_file));
    assert_eq!(
        raw.len() as u64,
        info.byte_size,
        "Byte size mismatch for {res_path}"
    );
    assert_eq!(md5_hex(&raw), info.md5, "MD5 mismatch for {res_path}");
    let cnt = read_u64_le(&raw, 40) as usize;
    let buckets = read_u64_le(&raw, 48) as usize;
    assert_eq!(
        cnt, info.record_count,
        "Record count mismatch for {res_path}"
    );
    assert!(buckets > 0);
    let mut rec_offsets = Vec::with_capacity(cnt);
    for b_idx in 0..buckets {
        let b_off = read_u64_le(&raw, 56 + b_idx * 8) as usize;
        if b_off == 0 {
            continue;
        }
        let abs_off = 32 + b_off;
        let n_in_bucket = read_u64_le(&raw, abs_off - 8) as usize;
        for i in 0..n_in_bucket {
            rec_offsets.push(abs_off + i * stride);
        }
    }
    assert_eq!(
        rec_offsets.len(),
        cnt,
        "Bucket record sum mismatch for {res_path}"
    );
    (raw, rec_offsets)
}

#[test]
// parity: plan:T-1, plan:T-2, plan:D-2
// parity: sde:res:/staticdata/types.fsdbinary, sde:res:/staticdata/categories.fsdbinary, sde:res:/staticdata/groups.fsdbinary
// parity: sde:res:/staticdata/typematerials.fsdbinary, sde:res:/staticdata/compressibletypes.fsdbinary, sde:res:/staticdata/typelist.fsdbinary
// parity: sde:res:/staticdata/races.fsdbinary, sde:res:/staticdata/bloodlines.fsdbinary, sde:res:/staticdata/ancestries.fsdbinary
// parity: sde:res:/staticdata/schools.fsdbinary, sde:res:/staticdata/factions.fsdbinary, sde:res:/staticdata/npccorporations.fsdbinary
// parity: sde:res:/staticdata/marketgroups.fsdbinary, sde:res:/staticdata/schematics.fsdbinary, sde:res:/staticdata/stationoperations.fsdbinary
// parity: sde:res:/staticdata/stationservices.fsdbinary, sde:res:/staticdata/dynamicitemattributes.fsdbinary, sde:res:/staticdata/spacecomponentsbytype.fsdbinary
// parity: sde:res:/staticdata/dogmaattributes.fsdbinary, sde:res:/staticdata/dogmaeffects.fsdbinary
fn test_t2_sde_fsdbinary_raw_client_verification_sweep() {
    let db_path = SdeRegistry::default_db_path();
    let sde = SdeRegistry::open(&db_path).expect("real V24.01 SDE database must open");
    let sde_uri = format!("file:{}?immutable=1", db_path.display());
    let sde_conn = Connection::open_with_flags(
        &sde_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open sde_conn");

    let home = std::env::var("HOME").expect("HOME environment variable must be set");
    let resfiles_dir =
        PathBuf::from(&home).join("Library/Application Support/EVE Online/SharedCache/ResFiles");
    assert!(
        resfiles_dir.is_dir(),
        "V24.01 client SharedCache/ResFiles directory must exist at {:?}",
        resfiles_dir
    );

    let mut attested: Vec<&'static str> = Vec::new();

    // 1. categories.fsdbinary (48 records, stride 40)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/categories.fsdbinary",
            40,
        );
        assert_eq!(recs.len(), 48);
        assert_eq!(sde.categories.len(), 48);
        for rec in recs {
            let cid = read_u64_le(&raw, rec) as u32;
            let rec_cid = read_u32_le(&raw, rec + 16);
            let name_id = read_u32_le(&raw, rec + 20);
            let published = raw[rec + 28] != 0;
            assert_eq!(cid, rec_cid);
            assert!(name_id > 0);
            let cat = sde
                .categories
                .get(&cid)
                .expect("category in sde.categories");
            assert_eq!(cat.category_id, cid);
            assert!(!cat.name.is_empty());
            assert_eq!(cat.published, published);
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
        attested.push("sde:res:/staticdata/categories.fsdbinary");
    }

    // 2. groups.fsdbinary (1,611 records, stride 32)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/groups.fsdbinary",
            32,
        );
        assert_eq!(recs.len(), 1_611);
        assert_eq!(sde.groups.len(), 1_611);
        for rec in recs {
            let gid = read_u32_le(&raw, rec);
            let cid = read_u32_le(&raw, rec + 4);
            let rec_gid = read_u32_le(&raw, rec + 8);
            let name_id = read_u32_le(&raw, rec + 12);
            let anchorable = raw[rec + 20] != 0;
            let anchored = raw[rec + 21] != 0;
            let fittable_non_singleton = raw[rec + 22] != 0;
            let published = raw[rec + 23] != 0;
            let use_base_price = raw[rec + 24] != 0;
            assert_eq!(gid, rec_gid);
            assert!(name_id > 0);
            assert!(sde.categories.contains_key(&cid));
            let grp = sde.groups.get(&gid).expect("group in sde.groups");
            assert_eq!(grp.group_id, gid);
            assert_eq!(grp.category_id, cid);
            assert!(!grp.name.is_empty());
            assert_eq!(grp.anchorable, anchorable);
            assert_eq!(grp.anchored, anchored);
            assert_eq!(grp.fittable_non_singleton, fittable_non_singleton);
            assert_eq!(grp.use_base_price, use_base_price);
            assert_eq!(grp.published, published);
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
        attested.push("sde:res:/staticdata/groups.fsdbinary");
    }

    // 3. dogmaattributes.fsdbinary (2,871 records, stride 80)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/dogmaattributes.fsdbinary",
            80,
        );
        assert_eq!(recs.len(), 2_871);
        assert_eq!(sde.attributes.len(), 2_871);
        for rec in recs {
            let aid = read_u64_le(&raw, rec) as u16;
            let name_rel = read_u64_le(&raw, rec + 16);
            let rec_aid = read_u32_le(&raw, rec + 24) as u16;
            let def_val = f64::from(read_f32_le(&raw, rec + 40));
            let high_is_good = raw[rec + 73] != 0;
            let published = raw[rec + 74] != 0;
            let stackable = raw[rec + 75] != 0;
            let name = read_cfsd_str(&raw, name_rel);
            assert_eq!(aid, rec_aid);
            let attr = sde
                .attributes
                .get(&aid)
                .expect("attribute in sde.attributes");
            assert_eq!(attr.attribute_id, aid);
            assert_eq!(attr.name, name);
            assert_eq!(attr.default_value, def_val);
            assert_eq!(attr.high_is_good, high_is_good);
            assert_eq!(attr.stackable, stackable);
            assert_eq!(attr.published, published);
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
        attested.push("sde:res:/staticdata/dogmaattributes.fsdbinary");
    }

    // 4. dogmaeffects.fsdbinary (3,427 records, stride 112)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/dogmaeffects.fsdbinary",
            112,
        );
        assert_eq!(recs.len(), 3_427);
        assert_eq!(sde.effects.len(), 3_427);
        let mut total_modifiers = 0usize;
        let mut effect_stopper_count = 0usize;
        for rec in recs {
            let eid = read_u64_le(&raw, rec) as u16;
            let name_rel = read_u64_le(&raw, rec + 8);
            let guid_rel = read_u64_le(&raw, rec + 16);
            let mod_rel = read_u64_le(&raw, rec + 24);
            let effect_category = read_u32_le(&raw, rec + 60) as u16;
            let rec_eid = read_u32_le(&raw, rec + 64) as u16;
            let disallow_auto_repeat = raw[rec + 100] != 0;
            let electronic_chance = raw[rec + 101] != 0;
            let is_assistance = raw[rec + 102] != 0;
            let is_offensive = raw[rec + 103] != 0;
            let is_warp_safe = raw[rec + 104] != 0;
            let propulsion_chance = raw[rec + 105] != 0;
            let published = raw[rec + 106] != 0;
            let range_chance = raw[rec + 107] != 0;
            let mask = read_u32_le(&raw, rec + 108);
            assert_eq!(eid, rec_eid);

            let name = read_cfsd_str(&raw, name_rel);
            let guid = ((mask & 128) != 0).then(|| read_cfsd_str(&raw, guid_rel));
            let mut expected_mods: Vec<EffectModifierSpec> = Vec::new();
            if (mask & 512) != 0 && mod_rel != 0 {
                let m_abs = 32 + mod_rel as usize;
                let mcnt = read_u64_le(&raw, m_abs - 8) as usize;
                for mi in 0..mcnt {
                    let mrec = m_abs + mi * 48;
                    let dom_rel = read_u64_le(&raw, mrec);
                    let func_rel = read_u64_le(&raw, mrec + 8);
                    let meff = read_i32_le(&raw, mrec + 16);
                    let mgrp = read_i32_le(&raw, mrec + 20);
                    let mmod_attr = read_i32_le(&raw, mrec + 24);
                    let mming_attr = read_i32_le(&raw, mrec + 28);
                    let mop = read_i32_le(&raw, mrec + 32);
                    let mskill = read_i32_le(&raw, mrec + 36);
                    let mmask = read_u32_le(&raw, mrec + 40);

                    let func = read_cfsd_str(&raw, func_rel);
                    let domain = read_cfsd_str(&raw, dom_rel);
                    let effect_id = ((mmask & 1) != 0).then_some(meff as u16);
                    let group_id = ((mmask & 2) != 0).then_some(mgrp as u32);
                    let skill_type_id = ((mmask & 32) != 0).then_some(mskill as u32);
                    let operator = if (mmask & 16) != 0 { mop } else { 0 };
                    if func == "EffectStopper" {
                        effect_stopper_count += 1;
                        expected_mods.push(EffectModifierSpec {
                            domain,
                            func,
                            modified_attribute_id: if (mmask & 4) != 0 {
                                mmod_attr as u16
                            } else {
                                0
                            },
                            modifying_attribute_id: if (mmask & 8) != 0 {
                                mming_attr as u16
                            } else {
                                0
                            },
                            operator,
                            group_id,
                            skill_type_id,
                            effect_id,
                        });
                    } else if (mmask & 4) != 0
                        && (mmask & 8) != 0
                        && mmod_attr >= 0
                        && mming_attr >= 0
                    {
                        expected_mods.push(EffectModifierSpec {
                            domain,
                            func,
                            modified_attribute_id: mmod_attr as u16,
                            modifying_attribute_id: mming_attr as u16,
                            operator,
                            group_id,
                            skill_type_id,
                            effect_id,
                        });
                    }
                }
            }
            total_modifiers += expected_mods.len();

            let eff = sde.effects.get(&eid).expect("effect in sde.effects");
            assert_eq!(eff.effect_id, eid);
            assert_eq!(eff.name, name);
            assert_eq!(eff.effect_category, effect_category);
            assert_eq!(eff.is_offensive, is_offensive);
            assert_eq!(eff.is_assistance, is_assistance);
            assert_eq!(eff.is_warp_safe, is_warp_safe);
            assert_eq!(eff.disallow_auto_repeat, disallow_auto_repeat);
            assert_eq!(eff.electronic_chance, electronic_chance);
            assert_eq!(eff.propulsion_chance, propulsion_chance);
            assert_eq!(eff.range_chance, range_chance);
            assert_eq!(eff.guid, guid);
            assert_eq!(eff.published, published);
            assert_eq!(eff.modifiers.len(), expected_mods.len());
            for (actual_m, exp_m) in eff.modifiers.iter().zip(expected_mods.iter()) {
                assert_eq!(actual_m.domain, exp_m.domain);
                assert_eq!(actual_m.func, exp_m.func);
                assert_eq!(actual_m.modified_attribute_id, exp_m.modified_attribute_id);
                assert_eq!(
                    actual_m.modifying_attribute_id,
                    exp_m.modifying_attribute_id
                );
                assert_eq!(actual_m.operator, exp_m.operator);
                assert_eq!(actual_m.group_id, exp_m.group_id);
                assert_eq!(actual_m.skill_type_id, exp_m.skill_type_id);
                assert_eq!(actual_m.effect_id, exp_m.effect_id);
            }
        }
        assert_eq!(total_modifiers, 5_178);
        assert_eq!(effect_stopper_count, 10);
        attested.push("sde:res:/staticdata/dogmaeffects.fsdbinary");
    }

    // 5. types.fsdbinary (53,288 records, stride 152)
    {
        let (types_raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/types.fsdbinary",
            152,
        );
        assert_eq!(recs.len(), 53_288);
        assert_eq!(sde.types.len(), 53_288);
        assert_eq!(sde.wrecks.len(), 5_703);
        for rec in recs {
            let tid = read_u64_le(&types_raw, rec) as u32;
            let bp = read_f64_le(&types_raw, rec + 8);
            let cap = read_f64_le(&types_raw, rec + 16);
            let mass = read_f64_le(&types_raw, rec + 32);
            let psize = read_i64_le(&types_raw, rec + 40).max(1) as u64;
            let rad = read_f64_le(&types_raw, rec + 48);
            let vol = read_f64_le(&types_raw, rec + 56);
            let desc_id = read_i32_le(&types_raw, rec + 68) as u32;
            let fac_id = read_i32_le(&types_raw, rec + 72) as u32;
            let graphic_id = read_i32_le(&types_raw, rec + 80) as u32;
            let group_id = read_i32_le(&types_raw, rec + 84) as u32;
            let icon_id = read_i32_le(&types_raw, rec + 88) as u32;
            let mkt_grp_id = read_i32_le(&types_raw, rec + 96) as u32;
            let meta_grp_id = read_i32_le(&types_raw, rec + 100) as u32;
            let meta_lvl = i64::from(read_i32_le(&types_raw, rec + 104));
            let race_id = read_i32_le(&types_raw, rec + 116) as u32;
            let sound_id = read_i32_le(&types_raw, rec + 120) as u32;
            let tech_lvl = read_i32_le(&types_raw, rec + 124) as u32;
            let rec_tid = read_i32_le(&types_raw, rec + 128) as u32;
            let var_parent_tid = read_i32_le(&types_raw, rec + 136) as u32;
            let wreck_tid = read_i32_le(&types_raw, rec + 140) as u32;
            let published = types_raw[rec + 145] != 0;
            let mask = read_u32_le(&types_raw, rec + 148);

            assert_eq!(rec_tid, tid);
            let inv = sde
                .types
                .get(&tid)
                .expect("type from types.fsdbinary must exist in sde.types");
            assert_eq!(inv.type_id, tid);
            assert_eq!(inv.group_id, group_id);
            assert_eq!(inv.category_id, sde.groups[&group_id].category_id);
            assert!(!inv.name.is_empty());
            assert_eq!(
                inv.base_price,
                if (mask & (1 << 0)) != 0 { bp } else { 0.0 }
            );
            assert_eq!(inv.capacity, if (mask & (1 << 1)) != 0 { cap } else { 0.0 });
            assert_eq!(inv.mass, if (mask & (1 << 11)) != 0 { mass } else { 0.0 });
            assert_eq!(inv.portion_size, psize);
            assert_eq!(inv.radius, if (mask & (1 << 19)) != 0 { rad } else { 0.0 });
            assert_eq!(inv.volume, if (mask & (1 << 23)) != 0 { vol } else { 0.0 });
            assert_eq!(inv.published, published);
            assert_eq!(
                inv.description_id,
                ((mask & (1 << 3)) != 0).then_some(desc_id)
            );
            assert_eq!(inv.faction_id, ((mask & (1 << 5)) != 0).then_some(fac_id));
            assert_eq!(
                inv.graphic_id,
                ((mask & (1 << 6)) != 0).then_some(graphic_id)
            );
            assert_eq!(inv.icon_id, ((mask & (1 << 7)) != 0).then_some(icon_id));
            assert_eq!(
                inv.market_group_id,
                ((mask & (1 << 10)) != 0).then_some(mkt_grp_id)
            );
            assert_eq!(
                inv.meta_group_id,
                ((mask & (1 << 12)) != 0).then_some(meta_grp_id)
            );
            assert_eq!(
                inv.meta_level,
                ((mask & (1 << 13)) != 0).then_some(meta_lvl)
            );
            assert_eq!(inv.race_id, ((mask & (1 << 18)) != 0).then_some(race_id));
            assert_eq!(inv.sound_id, ((mask & (1 << 20)) != 0).then_some(sound_id));
            assert_eq!(
                inv.tech_level,
                ((mask & (1 << 21)) != 0).then_some(tech_lvl)
            );
            assert_eq!(
                inv.variation_parent_type_id,
                ((mask & (1 << 22)) != 0).then_some(var_parent_tid)
            );
            assert_eq!(
                sde.wrecks.get(&tid).copied(),
                ((mask & (1 << 24)) != 0).then_some(wreck_tid)
            );
            if let Some(parent_id) = inv.variation_parent_type_id {
                assert!(sde.variations_of(parent_id).contains(&inv.type_id));
            }
        }
        let raven = &sde.types[&638];
        assert_eq!(raven.name, "Raven");
        assert_eq!(raven.group_id, 27);
        assert_eq!(raven.category_id, 6);
        assert_eq!(raven.default_attributes[&attr_ids::MAX_VELOCITY], 113.0);
        assert_eq!(
            raven.default_attributes[&attr_ids::SHIELD_CAPACITY],
            7_700.0
        );
        assert_eq!(raven.default_attributes[&attr_ids::ARMOR_HP], 6_380.0);
        assert_eq!(raven.default_attributes[&attr_ids::HP_STRUCT], 7_040.0);
        assert_eq!(raven.default_attributes[&attr_ids::RIG_SIZE], 3.0);
        attested.push("sde:res:/staticdata/types.fsdbinary");
    }

    // 6. typematerials.fsdbinary (9,866 records, stride 32)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/typematerials.fsdbinary",
            32,
        );
        assert_eq!(recs.len(), 9_866);
        assert_eq!(sde.type_materials.len(), 9_866);
        assert!(!sde.type_materials.contains_key(&34));
        let mut non_empty = 0usize;
        for rec in recs {
            let tid = read_u64_le(&raw, rec) as u32;
            let vec_rel = read_u64_le(&raw, rec + 8);
            let mut expected = Vec::new();
            if vec_rel != 0 {
                let voff = 32 + vec_rel as usize;
                let vcnt = read_u64_le(&raw, voff - 8) as usize;
                for j in 0..vcnt {
                    let mtid = read_u32_le(&raw, voff + j * 8);
                    let qty = u64::from(read_u32_le(&raw, voff + j * 8 + 4));
                    expected.push(TypeMaterialEntry {
                        material_type_id: mtid,
                        quantity: qty,
                    });
                }
            }
            if !expected.is_empty() {
                non_empty += 1;
            }
            let actual = sde
                .type_materials
                .get(&tid)
                .expect("type_materials entry in sde");
            assert_eq!(actual, &expected);
        }
        assert_eq!(non_empty, 9_556);
        attested.push("sde:res:/staticdata/typematerials.fsdbinary");
    }

    // 7. compressibletypes.fsdbinary (212 records, stride 8)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/compressibletypes.fsdbinary",
            8,
        );
        assert_eq!(recs.len(), 212);
        assert_eq!(sde.compressible_types.len(), 212);
        for rec in recs {
            let raw_tid = read_u32_le(&raw, rec);
            let comp_tid = read_u32_le(&raw, rec + 4);
            assert_eq!(
                sde.compressible_types.get(&raw_tid).copied(),
                Some(comp_tid)
            );
            let raw_t = sde.types.get(&raw_tid).expect("raw compressible type");
            let comp_t = sde.types.get(&comp_tid).expect("compressed target type");
            assert!(comp_t.volume < raw_t.volume);
        }
        assert_eq!(sde.compressible_types[&1230], 62516);
        assert_eq!(sde.compressible_types[&1228], 62520);
        assert_eq!(sde.compressible_types[&1224], 62524);
        assert_eq!(sde.compressible_types[&18], 62528);
        attested.push("sde:res:/staticdata/compressibletypes.fsdbinary");
    }

    // 8. typelist.fsdbinary (469 records, stride 88)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/typelist.fsdbinary",
            88,
        );
        assert_eq!(recs.len(), 469);
        assert_eq!(sde.type_lists.len(), 469);
        let sorted_vec = |rel: u64| -> Vec<u32> {
            let mut v = read_cfsd_u32_vec(&raw, rel);
            v.sort_unstable();
            v
        };
        for rec in recs {
            let tlid = read_u64_le(&raw, rec) as u32;
            let exc_cat = sorted_vec(read_u64_le(&raw, rec + 16));
            let exc_grp = sorted_vec(read_u64_le(&raw, rec + 24));
            let exc_typ = sorted_vec(read_u64_le(&raw, rec + 32));
            let inc_cat = sorted_vec(read_u64_le(&raw, rec + 40));
            let inc_grp = sorted_vec(read_u64_le(&raw, rec + 48));
            let inc_typ = sorted_vec(read_u64_le(&raw, rec + 56));
            let name = read_cfsd_str(&raw, read_u64_le(&raw, rec + 64));
            let tl = sde
                .type_lists
                .get(&tlid)
                .expect("type_list in sde.type_lists");
            assert_eq!(tl.type_list_id, tlid);
            assert_eq!(tl.name, name);
            assert_eq!(tl.included_category_ids, inc_cat);
            assert_eq!(tl.included_group_ids, inc_grp);
            assert_eq!(tl.included_type_ids, inc_typ);
            assert_eq!(tl.excluded_category_ids, exc_cat);
            assert_eq!(tl.excluded_group_ids, exc_grp);
            assert_eq!(tl.excluded_type_ids, exc_typ);
        }
        attested.push("sde:res:/staticdata/typelist.fsdbinary");
    }

    // 9. races.fsdbinary (11 records, stride 48)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/races.fsdbinary",
            48,
        );
        assert_eq!(recs.len(), 11);
        assert_eq!(sde.races.len(), 11);
        for rec in recs {
            let rid = read_u64_le(&raw, rec) as u32;
            let sk_rel = read_u64_le(&raw, rec + 8);
            let sk_cnt = read_u64_le(&raw, rec + 16) as usize;
            let name_id = read_u32_le(&raw, rec + 32);
            let ship_tid = read_u32_le(&raw, rec + 36);
            let mask = read_u32_le(&raw, rec + 40);
            assert!(name_id > 0);
            let mut expected_skills = HashMap::new();
            if sk_rel != 0 && sk_cnt > 0 {
                let sk_abs = 32 + sk_rel as usize;
                let nbuckets = read_u64_le(&raw, sk_abs - 8) as usize;
                for b in 0..nbuckets {
                    let boff = read_u64_le(&raw, sk_abs + b * 8) as usize;
                    if boff == 0 {
                        continue;
                    }
                    let b_abs = 32 + boff;
                    let n = read_u64_le(&raw, b_abs - 8) as usize;
                    for i in 0..n {
                        let k = read_u32_le(&raw, b_abs + i * 8);
                        let v = read_u32_le(&raw, b_abs + i * 8 + 4) as u8;
                        expected_skills.insert(k, v);
                    }
                }
            }
            assert_eq!(expected_skills.len(), sk_cnt);
            let race = sde.races.get(&rid).expect("race in sde.races");
            assert_eq!(race.race_id, rid);
            assert!(!race.name.is_empty());
            assert_eq!(race.corvette_type_id, ((mask & 4) != 0).then_some(ship_tid));
            assert_eq!(race.starter_skills, expected_skills);
        }
        assert_eq!(sde.races[&1].corvette_type_id, Some(601));
        assert_eq!(sde.races[&2].corvette_type_id, Some(588));
        assert_eq!(sde.races[&4].corvette_type_id, Some(596));
        assert_eq!(sde.races[&8].corvette_type_id, Some(606));
        assert_eq!(sde.races[&16].corvette_type_id, None);
        attested.push("sde:res:/staticdata/races.fsdbinary");
    }

    // 10. bloodlines.fsdbinary (18 records, stride 48)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/bloodlines.fsdbinary",
            48,
        );
        assert_eq!(recs.len(), 18);
        assert_eq!(sde.bloodlines.len(), 18);
        for rec in recs {
            let bid = read_u32_le(&raw, rec);
            let charisma = i64::from(read_i32_le(&raw, rec + 4));
            let corp_id = i64::from(read_i32_le(&raw, rec + 8));
            let intel = i64::from(read_i32_le(&raw, rec + 20));
            let mem = i64::from(read_i32_le(&raw, rec + 24));
            let name_id = read_u32_le(&raw, rec + 28);
            let perc = i64::from(read_i32_le(&raw, rec + 32));
            let race_id = read_u32_le(&raw, rec + 36);
            let will = i64::from(read_i32_le(&raw, rec + 40));
            assert!(name_id > 0);
            let bl = sde
                .bloodlines
                .get(&bid)
                .expect("bloodline in sde.bloodlines");
            assert_eq!(bl.bloodline_id, bid);
            assert_eq!(bl.race_id, race_id);
            assert_eq!(bl.corporation_id, corp_id);
            assert_eq!(bl.charisma, charisma);
            assert_eq!(bl.intelligence, intel);
            assert_eq!(bl.memory, mem);
            assert_eq!(bl.perception, perc);
            assert_eq!(bl.willpower, will);
            assert_eq!(bl.ship_type_id, sde.races[&race_id].corvette_type_id);
        }
        attested.push("sde:res:/staticdata/bloodlines.fsdbinary");
    }

    // 11. ancestries.fsdbinary (43 records, stride 56)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/ancestries.fsdbinary",
            56,
        );
        assert_eq!(recs.len(), 43);
        assert_eq!(sde.ancestries.len(), 43);
        for rec in recs {
            let aid = read_u64_le(&raw, rec) as u32;
            let short_desc_rel = read_u64_le(&raw, rec + 8);
            let bid = read_u32_le(&raw, rec + 16);
            let charisma = i64::from(read_i32_le(&raw, rec + 20));
            let intel = i64::from(read_i32_le(&raw, rec + 32));
            let mem = i64::from(read_i32_le(&raw, rec + 36));
            let name_id = read_u32_le(&raw, rec + 40);
            let perc = i64::from(read_i32_le(&raw, rec + 44));
            let will = i64::from(read_i32_le(&raw, rec + 48));
            let mask = read_u32_le(&raw, rec + 52);
            assert!(name_id > 0);
            let sdesc = if (mask & 8) != 0 {
                read_cfsd_str(&raw, short_desc_rel)
            } else {
                String::new()
            };
            let anc = sde
                .ancestries
                .get(&aid)
                .expect("ancestry in sde.ancestries");
            assert_eq!(anc.ancestry_id, aid);
            assert_eq!(anc.bloodline_id, bid);
            assert_eq!(anc.charisma, charisma);
            assert_eq!(anc.intelligence, intel);
            assert_eq!(anc.memory, mem);
            assert_eq!(anc.perception, perc);
            assert_eq!(anc.willpower, will);
            assert_eq!(anc.short_description, sdesc);
        }
        attested.push("sde:res:/staticdata/ancestries.fsdbinary");
    }

    // 12. schools.fsdbinary (23 records, stride 64)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/schools.fsdbinary",
            64,
        );
        assert_eq!(recs.len(), 23);
        assert_eq!(sde.schools.len(), 23);
        let mut db_stmt = sde_conn
            .prepare("SELECT school_id, career_id, starting_stations_json, career_agents_json FROM sde_schools")
            .expect("prepare sde_schools query");
        let db_schools: HashMap<u32, (u32, Vec<u32>, Vec<u32>)> = db_stmt
            .query_map([], |r| {
                let sid = r.get::<_, i64>(0)? as u32;
                let cid = r.get::<_, i64>(1)? as u32;
                let stas: Vec<u32> = serde_json::from_str(&r.get::<_, String>(2)?).unwrap();
                let ags: Vec<u32> = serde_json::from_str(&r.get::<_, String>(3)?).unwrap();
                Ok((sid, (cid, stas, ags)))
            })
            .expect("query sde_schools")
            .map(|r| r.expect("valid school row"))
            .collect();
        for rec in recs {
            let sid = read_u64_le(&raw, rec) as u32;
            let agents_rel = read_u64_le(&raw, rec + 8);
            let stations_rel = read_u64_le(&raw, rec + 16);
            let career_id = read_u32_le(&raw, rec + 24);
            let corp_id = i64::from(read_i32_le(&raw, rec + 32));
            let name_id = read_u32_le(&raw, rec + 44);
            let race_id = read_u32_le(&raw, rec + 48);
            let mask = read_u32_le(&raw, rec + 60);
            assert!(name_id > 0);
            let agents = if (mask & 1) != 0 {
                read_cfsd_u32_vec(&raw, agents_rel)
            } else {
                Vec::new()
            };
            let stations = if (mask & 64) != 0 {
                read_cfsd_u32_vec(&raw, stations_rel)
            } else {
                Vec::new()
            };
            let start_sta = stations.first().copied().map(i64::from).unwrap_or(60003760);
            let sch = sde.schools.get(&sid).expect("school in sde.schools");
            assert_eq!(sch.school_id, sid);
            assert_eq!(sch.race_id, race_id);
            assert_eq!(sch.corporation_id, corp_id);
            assert_eq!(sch.station_id, start_sta);
            assert_eq!(
                sch.solar_system_id,
                sde.stations[&start_sta].solar_system_id
            );
            assert_eq!(db_schools[&sid], (career_id, stations, agents));
        }
        attested.push("sde:res:/staticdata/schools.fsdbinary");
    }

    // 13. factions.fsdbinary (27 records, stride 80)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/factions.fsdbinary",
            80,
        );
        assert_eq!(recs.len(), 27);
        assert_eq!(sde.factions.len(), 27);
        for rec in recs {
            let fid = read_u64_le(&raw, rec) as u32;
            let corp_id = i64::from(read_i32_le(&raw, rec + 32));
            let militia_id = i64::from(read_i32_le(&raw, rec + 44));
            let name_id = read_u32_le(&raw, rec + 48);
            let size_factor = f64::from(read_f32_le(&raw, rec + 60));
            let sys_id = read_u32_le(&raw, rec + 64);
            let mask = read_u32_le(&raw, rec + 72);
            assert!(name_id > 0);
            let fac = sde.factions.get(&fid).expect("faction in sde.factions");
            assert_eq!(fac.faction_id, fid);
            assert!(!fac.name.is_empty());
            assert_eq!(fac.corporation_id, ((mask & 1) != 0).then_some(corp_id));
            assert_eq!(
                fac.militia_corporation_id,
                ((mask & 8) != 0).then_some(militia_id)
            );
            assert_eq!(fac.solar_system_id, Some(sys_id));
            assert_eq!(fac.size_factor, size_factor);
        }
        assert_eq!(sde.factions[&500001].name, "Caldari State");
        assert_eq!(sde.factions[&500002].name, "Minmatar Republic");
        assert_eq!(sde.factions[&500003].name, "Amarr Empire");
        assert_eq!(sde.factions[&500004].name, "Gallente Federation");
        attested.push("sde:res:/staticdata/factions.fsdbinary");
    }

    // 14. npccorporations.fsdbinary (283 records, stride 224)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/npccorporations.fsdbinary",
            224,
        );
        assert_eq!(recs.len(), 283);
        assert_eq!(sde.npc_corporations.len(), 283);
        for rec in recs {
            let cid = read_i64_le(&raw, rec);
            let ticker = read_cfsd_str(&raw, read_u64_le(&raw, rec + 128));
            let fac_id = read_u32_le(&raw, rec + 160);
            let name_id = read_u32_le(&raw, rec + 188);
            let sys_id = read_u32_le(&raw, rec + 204);
            let sta_id = i64::from(read_i32_le(&raw, rec + 208));
            let tax_rate = f64::from(read_f32_le(&raw, rec + 212));
            assert!(name_id > 0);
            let corp = sde
                .npc_corporations
                .get(&cid)
                .expect("corp in sde.npc_corporations");
            assert_eq!(corp.corporation_id, cid);
            assert!(!corp.name.is_empty());
            assert_eq!(corp.ticker_name, ticker);
            assert_eq!(corp.faction_id, (fac_id != 0).then_some(fac_id));
            assert_eq!(corp.solar_system_id, (sys_id != 0).then_some(sys_id));
            assert_eq!(corp.station_id, (sta_id != 0).then_some(sta_id));
            assert_eq!(corp.tax_rate, tax_rate);
        }
        assert_eq!(sde.npc_corporations[&1000035].name, "Caldari Navy");
        assert_eq!(sde.npc_corporations[&1000035].ticker_name, "CN");
        assert_eq!(sde.npc_corporations[&1000125].name, "CONCORD");
        assert_eq!(sde.npc_corporations[&1000125].ticker_name, "CONCO");
        attested.push("sde:res:/staticdata/npccorporations.fsdbinary");
    }

    // 15. marketgroups.fsdbinary (2,114 records, stride 28)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/marketgroups.fsdbinary",
            28,
        );
        assert_eq!(recs.len(), 2_114);
        assert_eq!(sde.market_groups.len(), 2_114);
        for rec in recs {
            let mgid = read_u32_le(&raw, rec);
            let icon_id = read_u32_le(&raw, rec + 8);
            let name_id = read_u32_le(&raw, rec + 12);
            let parent_id = read_u32_le(&raw, rec + 16);
            let has_types = raw[rec + 20] != 0;
            let mask = read_u32_le(&raw, rec + 24);
            assert!(name_id > 0);
            let mg = sde
                .market_groups
                .get(&mgid)
                .expect("market_group in sde.market_groups");
            assert_eq!(mg.market_group_id, mgid);
            assert!(!mg.name.is_empty());
            assert_eq!(mg.parent_group_id, ((mask & 4) != 0).then_some(parent_id));
            assert_eq!(mg.has_types, has_types);
            assert_eq!(mg.icon_id, ((mask & 2) != 0).then_some(icon_id));
        }
        assert_eq!(sde.market_groups[&4].name, "Ships");
        assert_eq!(sde.market_groups[&9].name, "Ship Equipment");
        attested.push("sde:res:/staticdata/marketgroups.fsdbinary");
    }

    // 16. schematics.fsdbinary (68 records, stride 40)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/schematics.fsdbinary",
            40,
        );
        assert_eq!(recs.len(), 68);
        assert_eq!(sde.planet_schematics.len(), 68);
        for rec in recs {
            let scid = read_u64_le(&raw, rec) as u32;
            let pins_rel = read_u64_le(&raw, rec + 8);
            let types_rel = read_u64_le(&raw, rec + 16);
            let types_cnt = read_u64_le(&raw, rec + 24) as usize;
            let cycle_time = u64::from(read_u32_le(&raw, rec + 32));
            let name_id = read_u32_le(&raw, rec + 36);
            assert!(name_id > 0);
            let pins = read_cfsd_u32_vec(&raw, pins_rel);
            let t_abs = 32 + types_rel as usize;
            let tbuckets = read_u64_le(&raw, t_abs - 8) as usize;
            let mut expected_types: HashMap<u32, (u64, bool)> = HashMap::new();
            for tb in 0..tbuckets {
                let tboff = read_u64_le(&raw, t_abs + tb * 8) as usize;
                if tboff == 0 {
                    continue;
                }
                let tb_abs = 32 + tboff;
                let tn = read_u64_le(&raw, tb_abs - 8) as usize;
                for ti in 0..tn {
                    let trec = tb_abs + ti * 12;
                    let tk = read_u32_le(&raw, trec);
                    let tqty = u64::from(read_u32_le(&raw, trec + 4));
                    let tis_in = raw[trec + 8] != 0;
                    expected_types.insert(tk, (tqty, tis_in));
                }
            }
            assert_eq!(expected_types.len(), types_cnt);
            let ps = sde
                .planet_schematics
                .get(&scid)
                .expect("schematic in sde.planet_schematics");
            assert_eq!(ps.schematic_id, scid);
            assert!(!ps.name.is_empty());
            assert_eq!(ps.cycle_time, cycle_time);
            assert_eq!(ps.pins, pins);
            let actual_types: HashMap<u32, (u64, bool)> = ps
                .types
                .iter()
                .map(
                    |PlanetSchematicTypeEntry {
                         type_id,
                         quantity,
                         is_input,
                     }| { (*type_id, (*quantity, *is_input)) },
                )
                .collect();
            assert_eq!(actual_types, expected_types);
        }
        assert_eq!(sde.planet_schematics[&65].name, "Superconductors");
        assert_eq!(sde.planet_schematics[&65].cycle_time, 3_600);
        attested.push("sde:res:/staticdata/schematics.fsdbinary");
    }

    // 17. stationoperations.fsdbinary (69 records, stride 88)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/stationoperations.fsdbinary",
            88,
        );
        assert_eq!(recs.len(), 69);
        assert_eq!(sde.station_operations.len(), 69);
        for rec in recs {
            let oid = read_u64_le(&raw, rec) as u32;
            let svcs = read_cfsd_u32_vec(&raw, read_u64_le(&raw, rec + 24));
            let act_id = read_u32_le(&raw, rec + 48);
            let name_id = read_u32_le(&raw, rec + 72);
            assert!(name_id > 0);
            let op = sde
                .station_operations
                .get(&oid)
                .expect("operation in sde.station_operations");
            assert_eq!(op.operation_id, oid);
            assert!(!op.name.is_empty());
            assert_eq!(op.activity_id, Some(act_id));
            assert_eq!(op.services, svcs);
        }
        attested.push("sde:res:/staticdata/stationoperations.fsdbinary");
    }

    // 18. stationservices.fsdbinary (27 records, stride 16)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/stationservices.fsdbinary",
            16,
        );
        assert_eq!(recs.len(), 27);
        assert_eq!(sde.station_services.len(), 27);
        for rec in recs {
            let sid = read_u32_le(&raw, rec);
            let name_id = read_u32_le(&raw, rec + 8);
            assert!(name_id > 0);
            let svc = sde
                .station_services
                .get(&sid)
                .expect("service in sde.station_services");
            assert_eq!(svc.service_id, sid);
            assert!(!svc.name.is_empty());
        }
        assert_eq!(sde.station_services[&1].name, "Bounty Missions");
        assert_eq!(sde.station_services[&2].name, "Assassination Missions");
        assert_eq!(sde.station_services[&3].name, "Courier Missions");
        attested.push("sde:res:/staticdata/stationservices.fsdbinary");
    }

    // 19. dynamicitemattributes.fsdbinary (441 records, stride 32)
    {
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/dynamicitemattributes.fsdbinary",
            32,
        );
        assert_eq!(recs.len(), 441);
        assert_eq!(sde.dynamic_item_attributes.len(), 441);
        for rec in recs {
            let mid = read_u64_le(&raw, rec) as u32;
            let attrs_rel = read_u64_le(&raw, rec + 8);
            let attrs_cnt = read_u64_le(&raw, rec + 16) as usize;
            let io_rel = read_u64_le(&raw, rec + 24);
            let a_abs = 32 + attrs_rel as usize;
            let abuckets = read_u64_le(&raw, a_abs - 8) as usize;
            let mut aranges: HashMap<u16, [f32; 2]> = HashMap::new();
            for ab in 0..abuckets {
                let aboff = read_u64_le(&raw, a_abs + ab * 8) as usize;
                if aboff == 0 {
                    continue;
                }
                let ab_abs = 32 + aboff;
                let an = read_u64_le(&raw, ab_abs - 8) as usize;
                for ai in 0..an {
                    let arec = ab_abs + ai * 20;
                    let aid = read_u32_le(&raw, arec) as u16;
                    let amax = read_f32_le(&raw, arec + 4);
                    let amin = read_f32_le(&raw, arec + 8);
                    aranges.insert(aid, [amin, amax]);
                }
            }
            assert_eq!(aranges.len(), attrs_cnt);

            let io_abs = 32 + io_rel as usize;
            let io_cnt = read_u64_le(&raw, io_abs - 8) as usize;
            let mut first_res_tid = None;
            let mut app_types = Vec::new();
            for ii in 0..io_cnt {
                let irec = io_abs + ii * 16;
                let app_rel = read_u64_le(&raw, irec);
                let res_tid = read_u32_le(&raw, irec + 8);
                if first_res_tid.is_none() {
                    first_res_tid = Some(res_tid);
                }
                app_types.extend(read_cfsd_u32_vec(&raw, app_rel));
            }
            let muta = sde
                .dynamic_item_attributes
                .get(&mid)
                .expect("mutaplasmid in sde.dynamic_item_attributes");
            assert_eq!(muta.mutaplasmid_type_id, mid);
            assert_eq!(muta.resulting_type_id, first_res_tid.unwrap_or(mid));
            assert_eq!(muta.applicable_type_ids, app_types);
            assert_eq!(muta.attribute_ranges.len(), aranges.len());
            for (&aid, &[amin, amax]) in &aranges {
                let actual = muta.attribute_ranges[&aid];
                assert_eq!([actual[0] as f32, actual[1] as f32], [amin, amax]);
            }
        }
        attested.push("sde:res:/staticdata/dynamicitemattributes.fsdbinary");
    }

    // 20. spacecomponentsbytype.fsdbinary (11,238 records, stride 1832)
    {
        const BIT_TO_COMP: &[(u32, &str)] = &[
            (0, "activate"),
            (1, "additionalLoot"),
            (2, "alignmentBasedToll"),
            (3, "appliedProximityEffects"),
            (4, "autoLooter"),
            (5, "autoMoonMiner"),
            (6, "autoTractorBeam"),
            (7, "autoWarpScrambler"),
            (8, "behavior"),
            (10, "bookmark"),
            (11, "boombox"),
            (13, "breachPointAsteroid"),
            (14, "builtLandingPad"),
            (15, "cameraLimitation"),
            (16, "cargoBay"),
            (17, "companionModel"),
            (18, "cynoBeaconGenerator"),
            (19, "cynoInhibitor"),
            (20, "deathzoneDamageGenerator"),
            (21, "deathzoneImmunityGenerator"),
            (22, "decay"),
            (23, "decloakEmitter"),
            (24, "deploy"),
            (25, "deployBlocker"),
            (26, "destinyServerBallClass"),
            (27, "dogmatic"),
            (28, "dungeonFracture"),
            (29, "dynamicEntityName"),
            (30, "enforceCriminalFlag"),
            (31, "entityBounty"),
            (32, "entityStandings"),
            (33, "entosisCommandNode"),
            (34, "entosisLootTarget"),
            (35, "entosisSovereigntyHub"),
            (36, "essBracket"),
            (37, "essLink"),
            (38, "essRegistration"),
            (39, "fighterSquadron"),
            (40, "filamentSpoolup"),
            (41, "fitModulesByOwner"),
            (42, "fitting"),
            (43, "fwCapturePoint"),
            (44, "fwObjective"),
            (45, "fwScoreboard"),
            (46, "genericMultiEffect"),
            (47, "hackingSecurityState"),
            (48, "hostileBaiter"),
            (49, "ignoreFittingRestrictions"),
            (50, "itemTrader"),
            (51, "jumpPolarization"),
            (52, "linkWithShip"),
            (53, "mercenaryDen"),
            (54, "microJumpDriver"),
            (55, "mineable"),
            (56, "npcWarpBeacon"),
            (57, "npcpilot"),
            (58, "orbitalSkyhook"),
            (59, "phaseStabilizer"),
            (60, "phaseable"),
            (61, "physics"),
            (62, "pilotPresenceTracker"),
            (63, "proximityDisplacement"),
            (64, "proximityLock"),
            (65, "proximityNotifier"),
            (66, "proximityPersistAnchoredContainers"),
            (67, "proximitySensor"),
            (68, "proximityTrap"),
            (69, "pvpMatchResult"),
            (70, "reactiveSpawnComponent"),
            (71, "reinforce"),
            (72, "reorientor"),
            (74, "scanBlocker"),
            (75, "scoop"),
            (76, "shipGroup"),
            (77, "shipcasterLauncher"),
            (78, "siphon"),
            (79, "skybox"),
            (80, "skyhookReagentSilo"),
            (81, "sofOwnerMapper"),
            (84, "starterShipcaster"),
            (85, "stellarHarvester"),
            (86, "storeSlimItemFieldInItemSettings"),
            (87, "structureJumpBridge"),
            (88, "systemDbuffEmitter"),
            (89, "systemWideEffects"),
            (91, "targeting"),
            (92, "towGameGoalpost"),
            (93, "towGameObjective"),
            (94, "towGameOperator"),
            (95, "turboShield"),
            (96, "underConstruction"),
            (97, "visualProximityEffect"),
            (98, "warpDisruption"),
        ];
        let (raw, recs) = load_and_verify_cfsd_records(
            &resfiles_dir,
            &sde,
            "res:/staticdata/spacecomponentsbytype.fsdbinary",
            1832,
        );
        assert_eq!(recs.len(), 11_238);
        assert_eq!(sde.space_components_by_type.len(), 11_238);
        let mut non_empty = 0usize;
        for rec in recs {
            let tid = read_u64_le(&raw, rec) as u32;
            let m0 = u128::from(read_u64_le(&raw, rec + 1816));
            let m1 = u128::from(read_u64_le(&raw, rec + 1824));
            let mask128 = m0 | (m1 << 64);
            let expected: Vec<String> = BIT_TO_COMP
                .iter()
                .filter_map(|&(bit, name)| {
                    ((mask128 & (1u128 << bit)) != 0).then(|| name.to_string())
                })
                .collect();
            assert_eq!(expected.len() as u32, mask128.count_ones());
            if !expected.is_empty() {
                non_empty += 1;
            }
            let actual = sde
                .space_components_by_type
                .get(&tid)
                .expect("type in sde.space_components_by_type");
            assert_eq!(actual, &expected);
        }
        assert_eq!(non_empty, 11_180);
        assert!(sde.space_components_by_type[&35832].contains(&"autoWarpScrambler".to_string()));
        attested.push("sde:res:/staticdata/spacecomponentsbytype.fsdbinary");
    }

    // Domain checks for Agents, Dungeons, Missions, Epic Arcs, and Node Graphs (.fsdbinary tables)
    for ag in sde.agents.values() {
        assert!(!ag.name.is_empty());
        assert!(sde.npc_corporations.contains_key(&ag.corporation_id));
        assert!((1..=5).contains(&ag.level));
    }
    assert_eq!(sde.agents[&3008416].name, "Antaken Kamola");
    assert_eq!(sde.agents[&3008416].corporation_id, 1000002);

    assert_eq!(
        sde.dungeons.len(),
        sde.resources["res:/staticdata/dungeons.fsdbinary"].record_count
    );
    for d in sde.dungeons.values() {
        assert!(!d.name.is_empty());
    }
    assert_eq!(sde.dungeons[&43].name, "Pith Merchant Depot");

    assert_eq!(
        sde.missions.len(),
        sde.resources["res:/staticdata/missions.fsdbinary"].record_count
    );
    for m in sde.missions.values() {
        assert!(!m.title.is_empty());
    }
    assert_eq!(sde.missions[&875].title, "The Guristas Spies");

    assert_eq!(
        sde.epic_arcs.len(),
        sde.resources["res:/staticdata/epicarcs.fsdbinary"].record_count
    );
    for ea in sde.epic_arcs.values() {
        assert!(!ea.name.is_empty());
        assert!(ea.missions_count > 0);
    }
    assert_eq!(sde.epic_arcs[&29].name, "The Blood-Stained Stars");
    assert_eq!(sde.epic_arcs[&40].name, "Penumbra");
    assert_eq!(sde.epic_arcs[&48].name, "Right to Rule");
    assert_eq!(sde.epic_arcs[&52].name, "Syndication");
    assert_eq!(sde.epic_arcs[&53].name, "Wildfire");

    assert_eq!(
        sde.node_graphs.len(),
        sde.resources["res:/staticdata/nodegraphs.fsdbinary"].record_count
    );
    for ng in sde.node_graphs.values() {
        assert!(!ng.name.is_empty());
        assert!(ng.node_count > 0);
    }

    assert_eq!(attested.len(), 20);
    eve_testkit::attest(&attested);
}
