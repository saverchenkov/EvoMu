//! Independent raw client-file verification of the flat `.fsdbinary` catalogue resources that
//! `SdeRegistry::load_cfsd_table` exposes to server logic (`T-2`).
//!
//! Each file is read from `SharedCache/ResFiles/` with its byte size and md5 pinned to the parity
//! ledger, its records are located by walking the `cfsd` hash buckets, and every field of every
//! record is decoded here with this test's own offsets and compared with the server-side table.
//! Spot values are the client's own `*Loader.so` output for those records, and ID fields are
//! cross-checked against the SDE domain tables they reference.

#![forbid(unsafe_code)]

mod raw_client;

use eve_sde::{CfsdValue, SdeRegistry};
use raw_client::*;
use std::collections::BTreeMap;

/// Test-side field spec: `(client name, kind, offset, presence bit or 0 when mandatory)`.
/// Kinds: `s` string, `u` u32, `q` u64, `b` u8 flag, `l` list of u32.
type Spec = &'static [(&'static str, char, usize, u32)];

struct Res {
    path: &'static str,
    md5: &'static str,
    bytes: u64,
    key8: bool,
    stride: usize,
    mask_at: Option<usize>,
    spec: Spec,
    count: usize,
}

/// Decodes every record of `r` from the raw client file, checks the header/bucket count and the
/// importer's `sde_resources` row, and asserts the server table equals the independent decode.
fn verify(sde: &SdeRegistry, r: &Res) -> BTreeMap<i64, BTreeMap<String, CfsdValue>> {
    let info = sde
        .resources
        .get(r.path)
        .unwrap_or_else(|| panic!("{} not indexed", r.path));
    assert!(
        info.cdn_relative_path.ends_with(r.md5),
        "{} cdn path",
        r.path
    );
    let f = load_cfsd(&info.cdn_relative_path, r.md5, r.bytes, r.stride);
    assert_eq!(f.header_count, r.count, "{} header count", r.path);
    assert_eq!(
        info.record_count, r.count,
        "{} importer record_count",
        r.path
    );
    let mut client = BTreeMap::new();
    for &rec in &f.records {
        let key = if r.key8 {
            u64_at(&f.raw, rec) as i64
        } else {
            i64::from(u32_at(&f.raw, rec))
        };
        let mask = r.mask_at.map_or(0, |m| u32_at(&f.raw, rec + m));
        let mut fields = BTreeMap::new();
        for &(name, kind, off, bit) in r.spec {
            if bit != 0 && mask & bit == 0 {
                continue;
            }
            let p = rec + off;
            let v = match kind {
                's' => CfsdValue::Str(cfsd_str(&f.raw, u64_at(&f.raw, p)).unwrap_or_default()),
                'u' => CfsdValue::Int(i64::from(u32_at(&f.raw, p))),
                'q' => CfsdValue::Int(u64_at(&f.raw, p) as i64),
                'b' => CfsdValue::Int(i64::from(f.raw[p] != 0)),
                'l' => {
                    let (at, n) = cfsd_list(&f.raw, u64_at(&f.raw, p));
                    CfsdValue::IntList(
                        (0..n)
                            .map(|j| i64::from(u32_at(&f.raw, at + j * 4)))
                            .collect(),
                    )
                }
                other => panic!("spec kind {other}"),
            };
            fields.insert(name.to_string(), v);
        }
        assert!(
            client.insert(key, fields).is_none(),
            "{} duplicate key {key}",
            r.path
        );
    }
    assert_eq!(client.len(), r.count, "{} distinct keys", r.path);
    let spot: i64 = info.spot.parse().expect("integer spot key");
    assert!(
        client.contains_key(&spot),
        "{} importer spot {spot} not a client key",
        r.path
    );
    let server = sde.load_cfsd_table(r.path).expect("server cfsd table");
    assert_eq!(server.len(), r.count, "{} server rows", r.path);
    for (key, fields) in &client {
        let row = &server[key];
        assert_eq!(row.key, *key);
        assert_eq!(&row.fields, fields, "{} record {key}", r.path);
    }
    client
}

fn s(v: &str) -> CfsdValue {
    CfsdValue::Str(v.to_string())
}
fn i(v: i64) -> CfsdValue {
    CfsdValue::Int(v)
}
fn rec(pairs: &[(&str, CfsdValue)]) -> BTreeMap<String, CfsdValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}
fn int_of(m: &BTreeMap<String, CfsdValue>, k: &str) -> i64 {
    match m.get(k) {
        Some(CfsdValue::Int(v)) => *v,
        other => panic!("{k}: {other:?}"),
    }
}

#[test]
fn test_t2_sde_fsdbinary_flat_corporation_character_and_economy_tables() {
    // parity: sde:res:/staticdata/agenttypes.fsdbinary, sde:res:/staticdata/billtypes.fsdbinary, sde:res:/staticdata/careerpaths.fsdbinary
    // parity: sde:res:/staticdata/corporationroles.fsdbinary, sde:res:/staticdata/corporationrolegroups.fsdbinary
    // parity: sde:res:/staticdata/currencybuckets.fsdbinary, sde:res:/staticdata/dogmaattributecategories.fsdbinary
    // parity: sde:res:/staticdata/essreservekeys.fsdbinary, sde:res:/staticdata/petitionevents.fsdbinary
    // parity: sde:res:/staticdata/recruitmentgroups.fsdbinary, sde:res:/staticdata/recruitmenttypes.fsdbinary
    // parity: sde:res:/staticdata/schoolmap.fsdbinary, sde:res:/staticdata/pvpfilamenteventdates.fsdbinary
    let sde = SdeRegistry::open(&SdeRegistry::default_db_path()).expect("real V24.01 SDE");
    let p = |n: &str| format!("res:/staticdata/{n}.fsdbinary");
    let t = |path, md5, bytes, key8, stride, mask_at, spec, count| Res {
        path,
        md5,
        bytes,
        key8,
        stride,
        mask_at,
        spec,
        count,
    };

    let agent_types = verify(
        &sde,
        &t(
            "res:/staticdata/agenttypes.fsdbinary",
            "a3243825687248bbb08bb54eef96ac3b",
            674,
            true,
            16,
            None,
            &[("value", 's', 8, 0)],
            13,
        ),
    );
    assert_eq!(agent_types[&1], rec(&[("value", s("NonAgent"))]));
    assert_eq!(agent_types[&13], rec(&[("value", s("HeraldryAgent"))]));
    // Every agent's agent_type_id in the SDE agent table names a client agent type.
    assert!(sde
        .agents
        .values()
        .all(|a| agent_types.contains_key(&i64::from(a.agent_type_id))));

    let bills = verify(
        &sde,
        &t(
            "res:/staticdata/billtypes.fsdbinary",
            "06976d1b42e23e6b5c64e6ebf597c143",
            612,
            true,
            32,
            Some(28),
            &[
                ("billTypeName", 's', 8, 0),
                ("description", 's', 16, 1),
                ("nameID", 'u', 24, 0),
            ],
            7,
        ),
    );
    assert_eq!(
        bills[&1],
        rec(&[("billTypeName", s("Market Fine")), ("nameID", i(77_950))])
    );
    assert_eq!(
        bills[&7],
        rec(&[
            ("billTypeName", s("Infrastructure Hub Bill")),
            ("description", s("Bill for infrastructure hub")),
            ("nameID", i(309_671))
        ])
    );
    assert_eq!(
        bills
            .values()
            .filter(|r| r.contains_key("description"))
            .count(),
        2
    );

    let careers = verify(
        &sde,
        &t(
            "res:/staticdata/careerpaths.fsdbinary",
            "0ff6705b8c800e74f1bf39b4f67b44b2",
            283,
            true,
            24,
            None,
            &[
                ("internalName", 's', 8, 0),
                ("descriptionID", 'u', 16, 0),
                ("nameID", 'u', 20, 0),
            ],
            4,
        ),
    );
    assert_eq!(
        careers[&4],
        rec(&[
            ("internalName", s("Explorer")),
            ("descriptionID", i(582_517)),
            ("nameID", i(582_516))
        ])
    );
    assert_eq!(
        careers[&7],
        rec(&[
            ("internalName", s("Soldier of Fortune")),
            ("descriptionID", i(582_523)),
            ("nameID", i(582_522))
        ])
    );

    let roles = verify(
        &sde,
        &t(
            "res:/staticdata/corporationroles.fsdbinary",
            "3c34d6ff5143781317e7546646283cd9",
            3_855,
            true,
            32,
            Some(24),
            &[
                ("roleName", 's', 8, 0),
                ("descriptionID", 'u', 16, 1),
                ("nameID", 'u', 20, 0),
            ],
            55,
        ),
    );
    assert_eq!(
        roles[&0],
        rec(&[
            ("roleName", s("roleDirector")),
            ("descriptionID", i(60_126)),
            ("nameID", i(60_174))
        ])
    );
    assert_eq!(
        roles[&62],
        rec(&[
            ("roleName", s("roleSkillPlanManager")),
            ("descriptionID", i(589_064)),
            ("nameID", i(589_065))
        ])
    );

    let role_groups = verify(
        &sde,
        &t(
            "res:/staticdata/corporationrolegroups.fsdbinary",
            "4f47dc0d3e4adfcdc0c07a5c4e3d41a4",
            1_025,
            true,
            48,
            None,
            &[
                ("appliesTo", 's', 8, 0),
                ("appliesToGrantable", 's', 16, 0),
                ("roleGroupName", 's', 24, 0),
                ("roleMask", 'q', 32, 0),
                ("roleGroupNameID", 'u', 40, 0),
                ("isDivisional", 'b', 44, 0),
                ("isLocational", 'b', 45, 0),
            ],
            9,
        ),
    );
    assert_eq!(
        role_groups[&1],
        rec(&[
            ("appliesTo", s("roles")),
            ("appliesToGrantable", s("grantableRoles")),
            ("roleGroupName", s("General")),
            ("roleMask", i(6_895_013_280_067_096_961)),
            ("roleGroupNameID", i(60_117)),
            ("isDivisional", i(0)),
            ("isLocational", i(0))
        ])
    );
    assert_eq!(
        (
            int_of(&role_groups[&9], "isDivisional"),
            int_of(&role_groups[&9], "isLocational")
        ),
        (1, 1)
    );
    // "General" includes roleDirector (bit 0 of the role mask; corporationroles key 0).
    assert_eq!(int_of(&role_groups[&1], "roleMask") & 1, 1);

    let buckets = verify(
        &sde,
        &t(
            "res:/staticdata/currencybuckets.fsdbinary",
            "426cbf2cb7674b462ecb240b66339437",
            392,
            true,
            24,
            None,
            &[
                ("currency", 's', 8, 0),
                ("capacity", 'u', 16, 0),
                ("redeemTypeID", 'u', 20, 0),
            ],
            11,
        ),
    );
    assert_eq!(
        buckets[&1],
        rec(&[
            ("currency", s("skillPoints")),
            ("capacity", i(25_000)),
            ("redeemTypeID", i(49_756))
        ])
    );
    assert_eq!(
        buckets[&16],
        rec(&[
            ("currency", s("skillPoints")),
            ("capacity", i(125_000)),
            ("redeemTypeID", i(63_825))
        ])
    );
    assert!(buckets
        .values()
        .all(|b| sde.types.contains_key(&(int_of(b, "redeemTypeID") as u32))));

    let attr_cats = verify(
        &sde,
        &t(
            "res:/staticdata/dogmaattributecategories.fsdbinary",
            "649a2244e72466ed803f094402ba67b6",
            3_945,
            true,
            32,
            Some(24),
            &[("description", 's', 8, 1), ("name", 's', 16, 0)],
            37,
        ),
    );
    assert_eq!(
        attr_cats[&1],
        rec(&[
            ("description", s("Fitting capabilities of a ship")),
            ("name", s("Fitting"))
        ])
    );
    assert_eq!(attr_cats[&52], rec(&[("name", s("Heat"))]));

    let ess = verify(
        &sde,
        &t(
            "res:/staticdata/essreservekeys.fsdbinary",
            "549e8c81732d6bcac475a498adc5d06b",
            216,
            false,
            16,
            None,
            &[
                ("keyTypeID", 'u', 4, 0),
                ("locationListID", 'u', 8, 0),
                ("totalPulses", 'u', 12, 0),
            ],
            8,
        ),
    );
    assert_eq!(
        ess[&1],
        rec(&[
            ("keyTypeID", i(57_714)),
            ("locationListID", i(4_609)),
            ("totalPulses", i(15))
        ])
    );
    assert_eq!(
        ess[&8],
        rec(&[
            ("keyTypeID", i(57_721)),
            ("locationListID", i(4_611)),
            ("totalPulses", i(45))
        ])
    );
    assert!(ess
        .values()
        .all(|k| sde.types.contains_key(&(int_of(k, "keyTypeID") as u32))));

    let petitions = verify(
        &sde,
        &t(
            "res:/staticdata/petitionevents.fsdbinary",
            "12079f90d48a50e446ec8bce01474931",
            1_353,
            true,
            32,
            Some(28),
            &[("eventText", 's', 16, 0), ("eventTextID", 'u', 24, 2)],
            21,
        ),
    );
    assert_eq!(
        petitions[&1],
        rec(&[("eventText", s("Created")), ("eventTextID", i(62_191))])
    );
    assert_eq!(petitions[&21], rec(&[("eventText", s("Unflag as bug"))]));

    let rgroups = verify(
        &sde,
        &t(
            "res:/staticdata/recruitmentgroups.fsdbinary",
            "a5cb6cfcb010f29410d145df72ebaab4",
            232,
            false,
            16,
            Some(12),
            &[("descriptionID", 'u', 4, 1), ("nameID", 'u', 8, 0)],
            8,
        ),
    );
    assert_eq!(
        rgroups[&6],
        rec(&[("descriptionID", i(59_656)), ("nameID", i(59_662))])
    );
    assert_eq!(
        rgroups[&80],
        rec(&[("descriptionID", i(589_050)), ("nameID", i(589_051))])
    );
    let rtypes = verify(
        &sde,
        &t(
            "res:/staticdata/recruitmenttypes.fsdbinary",
            "a3442baee9c5b611914259b0ec7c7b76",
            1_148,
            false,
            20,
            Some(16),
            &[
                ("descriptionID", 'u', 4, 1),
                ("nameID", 'u', 8, 0),
                ("recruitmentGroupID", 'u', 12, 0),
            ],
            45,
        ),
    );
    assert_eq!(
        rtypes[&0],
        rec(&[
            ("descriptionID", i(233_779)),
            ("nameID", i(233_764)),
            ("recruitmentGroupID", i(80))
        ])
    );
    assert_eq!(
        rtypes[&44],
        rec(&[("nameID", i(588_609)), ("recruitmentGroupID", i(11))])
    );
    assert!(rtypes
        .values()
        .all(|r| rgroups.contains_key(&int_of(r, "recruitmentGroupID"))));

    let schools = verify(
        &sde,
        &t(
            "res:/staticdata/schoolmap.fsdbinary",
            "c62442b52beeb36dd7edfc26118358e3",
            248,
            false,
            12,
            None,
            &[("schoolID", 'u', 4, 0), ("solarSystemID", 'u', 8, 0)],
            12,
        ),
    );
    assert_eq!(
        schools[&1],
        rec(&[("schoolID", i(11)), ("solarSystemID", i(30_003_489))])
    );
    assert_eq!(
        schools[&12],
        rec(&[("schoolID", i(22)), ("solarSystemID", i(30_005_305))])
    );
    for m in schools.values() {
        assert!(sde.schools.contains_key(&(int_of(m, "schoolID") as u32)));
        assert!(sde
            .solar_systems
            .contains_key(&(int_of(m, "solarSystemID") as u32)));
    }

    let pvp = verify(
        &sde,
        &t(
            "res:/staticdata/pvpfilamenteventdates.fsdbinary",
            "c0697866a62786a0bc03770e1ebb84b2",
            10_715,
            true,
            40,
            None,
            &[
                ("endDate", 's', 8, 0),
                ("region", 's', 16, 0),
                ("startDate", 's', 24, 0),
                ("matchTypeID", 'u', 32, 0),
                ("QA", 'b', 36, 0),
            ],
            118,
        ),
    );
    assert_eq!(
        pvp[&1],
        rec(&[
            ("QA", i(1)),
            ("endDate", s("2020-07-13")),
            ("matchTypeID", i(1)),
            ("region", s("ccp")),
            ("startDate", s("2020-06-30"))
        ])
    );
    assert_eq!(
        pvp[&171],
        rec(&[
            ("QA", i(0)),
            ("endDate", s("2023-01-16")),
            ("matchTypeID", i(129)),
            ("region", s("ccp")),
            ("startDate", s("2023-01-14"))
        ])
    );
    assert_eq!(pvp.values().filter(|r| int_of(r, "QA") == 1).count(), 5);
    assert!(SdeRegistry::cfsd_flat_layout(&p("schoolmap")).is_some());

    eve_testkit::attest(&[
        "sde:res:/staticdata/agenttypes.fsdbinary",
        "sde:res:/staticdata/billtypes.fsdbinary",
        "sde:res:/staticdata/careerpaths.fsdbinary",
        "sde:res:/staticdata/corporationroles.fsdbinary",
        "sde:res:/staticdata/corporationrolegroups.fsdbinary",
        "sde:res:/staticdata/currencybuckets.fsdbinary",
        "sde:res:/staticdata/dogmaattributecategories.fsdbinary",
        "sde:res:/staticdata/essreservekeys.fsdbinary",
        "sde:res:/staticdata/petitionevents.fsdbinary",
        "sde:res:/staticdata/recruitmentgroups.fsdbinary",
        "sde:res:/staticdata/recruitmenttypes.fsdbinary",
        "sde:res:/staticdata/schoolmap.fsdbinary",
        "sde:res:/staticdata/pvpfilamenteventdates.fsdbinary",
    ]);
}

#[test]
fn test_t2_sde_fsdbinary_flat_cosmetics_tags_and_onboarding_tables() {
    // parity: sde:res:/staticdata/ship_cosmetic_slot_categories.fsdbinary, sde:res:/staticdata/ship_cosmetic_slot_names.fsdbinary
    // parity: sde:res:/staticdata/ship_cosmetic_slots.fsdbinary, sde:res:/staticdata/ship_skin_design_component_categories.fsdbinary
    // parity: sde:res:/staticdata/ship_skin_design_component_rarities.fsdbinary, sde:res:/staticdata/ship_skin_design_components_post_process.fsdbinary
    // parity: sde:res:/staticdata/shipcosmeticlicenses.fsdbinary, sde:res:/staticdata/tags.fsdbinary
    // parity: sde:res:/staticdata/checkpoints.fsdbinary, sde:res:/staticdata/conversationagents.fsdbinary
    // parity: sde:res:/staticdata/paints.fsdbinary, sde:res:/staticdata/achievementevents.fsdbinary
    let sde = SdeRegistry::open(&SdeRegistry::default_db_path()).expect("real V24.01 SDE");
    let t = |path, md5, bytes, key8, stride, mask_at, spec, count| Res {
        path,
        md5,
        bytes,
        key8,
        stride,
        mask_at,
        spec,
        count,
    };
    const NAME: Spec = &[("name", 's', 8, 0)];

    let slot_cats = verify(
        &sde,
        &t(
            "res:/staticdata/ship_cosmetic_slot_categories.fsdbinary",
            "06b1f143d96f53fe3eb51bc58025d372",
            214,
            true,
            16,
            None,
            NAME,
            3,
        ),
    );
    assert_eq!(slot_cats[&1], rec(&[("name", s("Material_slot"))]));
    assert_eq!(slot_cats[&3], rec(&[("name", s("Pattern_material_slot"))]));
    let slot_names = verify(
        &sde,
        &t(
            "res:/staticdata/ship_cosmetic_slot_names.fsdbinary",
            "20f10cae0dfd2f2920c16ef5b902ed14",
            450,
            true,
            16,
            None,
            NAME,
            8,
        ),
    );
    assert_eq!(slot_names[&1], rec(&[("name", s("primary_nanocoating"))]));
    assert_eq!(
        slot_names[&8],
        rec(&[("name", s("secondary_pattern_material"))])
    );
    let comp_cats = verify(
        &sde,
        &t(
            "res:/staticdata/ship_skin_design_component_categories.fsdbinary",
            "1055d62195fb427acbedd365428f0bea",
            193,
            true,
            16,
            None,
            NAME,
            3,
        ),
    );
    assert_eq!(comp_cats[&1], rec(&[("name", s("Material"))]));
    assert_eq!(comp_cats[&3], rec(&[("name", s("Metallic"))]));

    let slots = verify(
        &sde,
        &t(
            "res:/staticdata/ship_cosmetic_slots.fsdbinary",
            "2977421e1cbe42f593cdbd285fc8e51a",
            464,
            true,
            32,
            None,
            &[
                ("allowed_design_component_categories", 'l', 8, 0),
                ("category", 'u', 16, 0),
                ("description", 'u', 20, 0),
                ("internal_name", 'u', 24, 0),
                ("name", 'u', 28, 0),
            ],
            8,
        ),
    );
    assert_eq!(
        slots[&1],
        rec(&[
            (
                "allowed_design_component_categories",
                CfsdValue::IntList(vec![1, 3])
            ),
            ("category", i(1)),
            ("description", i(709_822)),
            ("internal_name", i(1)),
            ("name", i(709_823))
        ])
    );
    assert_eq!(
        slots[&8],
        rec(&[
            (
                "allowed_design_component_categories",
                CfsdValue::IntList(vec![1, 3])
            ),
            ("category", i(3)),
            ("description", i(726_302)),
            ("internal_name", i(8)),
            ("name", i(726_303))
        ])
    );
    for slot in slots.values() {
        assert!(slot_cats.contains_key(&int_of(slot, "category")));
        assert!(slot_names.contains_key(&int_of(slot, "internal_name")));
        let Some(CfsdValue::IntList(allowed)) = slot.get("allowed_design_component_categories")
        else {
            panic!()
        };
        assert!(allowed.iter().all(|c| comp_cats.contains_key(c)));
    }

    let rarities = verify(
        &sde,
        &t(
            "res:/staticdata/ship_skin_design_component_rarities.fsdbinary",
            "a8ac4a65f53e612d5cc481627705a310",
            160,
            false,
            12,
            None,
            &[("name", 'u', 4, 0), ("rank", 'u', 8, 0)],
            6,
        ),
    );
    assert_eq!(rarities[&1], rec(&[("name", i(709_470)), ("rank", i(1))]));
    assert_eq!(rarities[&6], rec(&[("name", i(709_475)), ("rank", i(6))]));

    let post = verify(
        &sde,
        &t(
            "res:/staticdata/ship_skin_design_components_post_process.fsdbinary",
            "af4c8de790665114cfe09c5e7e739fd6",
            23_848,
            true,
            16,
            None,
            &[("value", 's', 8, 0)],
            613,
        ),
    );
    assert_eq!(post[&53], rec(&[("value", s("#181643"))]));
    assert_eq!(post[&2132], rec(&[("value", s("#4c0601"))]));
    assert!(post.values().all(
        |r| matches!(r.get("value"), Some(CfsdValue::Str(c)) if c.len() == 7 && c.starts_with('#'))
    ));

    let licenses = verify(
        &sde,
        &t(
            "res:/staticdata/shipcosmeticlicenses.fsdbinary",
            "200f37bbeeb02381f51623c298c9e76b",
            20_800,
            false,
            20,
            None,
            &[
                ("cosmeticType", 'u', 4, 0),
                ("shipCosmeticLicenseTypeID", 'u', 8, 0),
                ("shipTypeID", 'u', 12, 0),
                ("slotGroup", 'u', 16, 0),
            ],
            830,
        ),
    );
    assert_eq!(
        licenses[&6],
        rec(&[
            ("cosmeticType", i(1)),
            ("shipCosmeticLicenseTypeID", i(72_964)),
            ("shipTypeID", i(587)),
            ("slotGroup", i(1))
        ])
    );
    assert_eq!(
        licenses[&894],
        rec(&[
            ("cosmeticType", i(2)),
            ("shipCosmeticLicenseTypeID", i(95_745)),
            ("shipTypeID", i(95_741)),
            ("slotGroup", i(1))
        ])
    );
    assert_eq!(sde.types[&587].name, "Rifter");
    let hulls = licenses
        .values()
        .filter(|l| {
            sde.types
                .get(&(int_of(l, "shipTypeID") as u32))
                .is_some_and(|t| t.category_id == 6)
        })
        .count();
    assert_eq!(
        hulls,
        licenses.len(),
        "every licensed shipTypeID is an SDE ship hull"
    );

    let tags = verify(
        &sde,
        &t(
            "res:/staticdata/tags.fsdbinary",
            "b5ca5b45d06fd39a67d6b33e64fb1b72",
            4_097,
            true,
            40,
            Some(32),
            &[
                ("internalName", 's', 8, 0),
                ("texturePath", 's', 16, 2),
                ("titleLabelID", 'u', 28, 4),
            ],
            49,
        ),
    );
    assert_eq!(
        tags[&7],
        rec(&[
            ("internalName", s("Mining tag")),
            (
                "texturePath",
                s("res:/UI/Texture/classes/Treatments/tagIcons/mining.png")
            ),
            ("titleLabelID", i(577_105))
        ])
    );
    assert_eq!(
        tags[&59],
        rec(&[
            ("internalName", s("Skills tag")),
            ("titleLabelID", i(584_598))
        ])
    );
    assert_eq!(
        tags.values()
            .filter(|r| r.contains_key("texturePath"))
            .count(),
        10
    );

    let checkpoints = verify(
        &sde,
        &t(
            "res:/staticdata/checkpoints.fsdbinary",
            "84035ab7cfc9db93650dd21c9552425b",
            9_910,
            true,
            40,
            Some(32),
            &[
                ("description", 's', 8, 1),
                ("featureID", 's', 16, 0),
                ("name", 's', 24, 0),
            ],
            78,
        ),
    );
    assert_eq!(
        checkpoints[&1],
        rec(&[
            ("description", s("Exists for development purpose")),
            ("featureID", s("air_npe")),
            ("name", s("Testing Checkpoint"))
        ])
    );
    assert_eq!(
        checkpoints[&106],
        rec(&[
            ("featureID", s("nes_intro")),
            ("name", s("[MAP Onboarding] 6 - Onboarding Finished"))
        ])
    );
    assert_eq!(
        checkpoints
            .values()
            .filter(|r| r.contains_key("description"))
            .count(),
        41
    );

    let conv = verify(
        &sde,
        &t(
            "res:/staticdata/conversationagents.fsdbinary",
            "5de53d90687359c83347cedbd673d133",
            9_083,
            true,
            72,
            Some(68),
            &[
                ("imagePath", 's', 8, 0),
                ("loopVideoPath", 's', 32, 4),
                ("preloadCachePath", 's', 48, 16),
                ("scenePath", 's', 56, 32),
                ("name", 'u', 64, 0),
            ],
            44,
        ),
    );
    assert_eq!(
        conv[&2086],
        rec(&[
            (
                "imagePath",
                s("res:/ui/texture/classes/conversationui/conversationagents/EmperprHeideran.png")
            ),
            ("name", i(1_046_176))
        ])
    );
    assert_eq!(
        conv[&5]["preloadCachePath"],
        s("res:/Animation_Gstate/Scenes/Templates/aura_curve_cache_list.yaml")
    );
    assert_eq!(int_of(&conv[&5], "name"), 519_116);
    let present = |k: &str| conv.values().filter(|r| r.contains_key(k)).count();
    assert_eq!(
        (
            present("loopVideoPath"),
            present("preloadCachePath"),
            present("scenePath")
        ),
        (2, 6, 18)
    );

    let paints = verify(
        &sde,
        &t(
            "res:/staticdata/paints.fsdbinary",
            "41730cd3dadc242118433cf52f51fbf4",
            10_531,
            true,
            48,
            None,
            &[
                ("finish", 's', 8, 0),
                ("hueCategory", 's', 16, 0),
                ("materialName", 's', 24, 0),
                ("rarity", 's', 32, 0),
                ("paintNameID", 'u', 40, 0),
            ],
            96,
        ),
    );
    assert_eq!(
        paints[&1],
        rec(&[
            ("finish", s("coated")),
            ("hueCategory", s("azure")),
            (
                "materialName",
                s("cosm_azure_vibrant_bright_coated_200_96_50")
            ),
            ("paintNameID", i(664_397)),
            ("rarity", s("uncommon"))
        ])
    );
    assert_eq!(
        paints[&99],
        rec(&[
            ("finish", s("satin")),
            ("hueCategory", s("yellow")),
            (
                "materialName",
                s("cosm_yellow_vibrant_bright_satin_60_100_100")
            ),
            ("paintNameID", i(664_492)),
            ("rarity", s("rare"))
        ])
    );

    let events = verify(
        &sde,
        &t(
            "res:/staticdata/achievementevents.fsdbinary",
            "f11edfc6911b45a762ffc40a2e9dec08",
            19_677,
            true,
            40,
            Some(32),
            &[
                ("description", 's', 8, 0),
                ("inputParameters", 'l', 16, 1),
                ("name", 's', 24, 0),
            ],
            131,
        ),
    );
    assert_eq!(
        events[&1],
        rec(&[
            ("description", s("An enemy NPC ship just got killed")),
            ("inputParameters", CfsdValue::IntList(vec![5, 15, 1, 2])),
            ("name", s("destroyHostileNPC"))
        ])
    );
    assert_eq!(
        events[&191],
        rec(&[
            ("description", s("Player mined X volume of ore")),
            ("name", s("volumeMined"))
        ])
    );
    assert_eq!(
        events
            .values()
            .filter(|r| r.contains_key("inputParameters"))
            .count(),
        61
    );

    eve_testkit::attest(&[
        "sde:res:/staticdata/ship_cosmetic_slot_categories.fsdbinary",
        "sde:res:/staticdata/ship_cosmetic_slot_names.fsdbinary",
        "sde:res:/staticdata/ship_cosmetic_slots.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_component_categories.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_component_rarities.fsdbinary",
        "sde:res:/staticdata/ship_skin_design_components_post_process.fsdbinary",
        "sde:res:/staticdata/shipcosmeticlicenses.fsdbinary",
        "sde:res:/staticdata/tags.fsdbinary",
        "sde:res:/staticdata/checkpoints.fsdbinary",
        "sde:res:/staticdata/conversationagents.fsdbinary",
        "sde:res:/staticdata/paints.fsdbinary",
        "sde:res:/staticdata/achievementevents.fsdbinary",
    ]);
}
