//! Independent raw client-file verification of the `.fsdbinary` resources that feed the server's
//! dogma, mission, epic-arc, dungeon and node-graph tables (`T-2`).
//!
//! Each file is parsed by this test from the raw `cfsd` bytes in `SharedCache/ResFiles/` (byte
//! size and md5 pinned to the parity ledger, records located by walking the hash buckets), and
//! every decoded record is compared field by field with what `SdeRegistry` exposes to server
//! logic. Display names are checked through the English labels decoded from the raw
//! `localization_fsd_en-us.pickle` by this test's own unpickler.

#![forbid(unsafe_code)]

mod raw_client;

use eve_sde::SdeRegistry;
use raw_client::*;
use std::collections::HashMap;

/// (res path, ledger md5, ledger byte size) for every client file this test reads.
const TYPEDOGMA: (&str, &str, u64) = (
    "res:/staticdata/typedogma.fsdbinary",
    "e8b313eff5dcd9b701f711832ba872ac",
    12_335_088,
);
const MISSIONS: (&str, &str, u64) = (
    "res:/staticdata/missions.fsdbinary",
    "7b6587a6e1129765bc41041699a83ca8",
    1_288_248,
);
const EPICARCS: (&str, &str, u64) = (
    "res:/staticdata/epicarcs.fsdbinary",
    "36ee60637670b50969fa2fd7bd190180",
    16_692,
);
const DUNGEONS: (&str, &str, u64) = (
    "res:/staticdata/dungeons.fsdbinary",
    "2e5bc11b9de86e15181fca48e43b69ba",
    52_395_840,
);
const NODEGRAPHS: (&str, &str, u64) = (
    "res:/staticdata/nodegraphs.fsdbinary",
    "a59acb0775423a99b82cb8a3f8e697a6",
    8_254_048,
);
const EN_US: (&str, &str, u64) = (
    "res:/localizationfsd/localization_fsd_en-us.pickle",
    "7a753c9da2eeca4dcc7cf0c6655aaae0",
    40_753_078,
);

fn load(sde: &SdeRegistry, res: (&str, &str, u64), stride: usize) -> CfsdFile {
    let info = sde
        .resources
        .get(res.0)
        .unwrap_or_else(|| panic!("{} missing from sde.resources", res.0));
    assert!(info.cdn_relative_path.ends_with(res.1));
    load_cfsd(&info.cdn_relative_path, res.1, res.2, stride)
}

fn load_en_us_labels(sde: &SdeRegistry) -> HashMap<u32, String> {
    let info = &sde.resources[EN_US.0];
    let raw = std::fs::read(resfiles_dir().join(&info.cdn_relative_path)).expect("en-us pickle");
    assert_eq!(raw.len() as u64, EN_US.2);
    assert_eq!(md5_hex(&raw), EN_US.1);
    assert_eq!(detect_format(&raw, false), "Pickle (protocol 0)");
    en_us_labels(&raw)
}

/// Importer naming rule: the en-us label of the record's name message, or the importer's
/// `"<Kind> <id>"` fallback when the client has no English text for that message (DERIVED).
fn expected_name(labels: &HashMap<u32, String>, name_id: u32, kind: &str, id: u64) -> String {
    labels
        .get(&name_id)
        .cloned()
        .unwrap_or_else(|| format!("{kind} {id}"))
}

#[test]
fn test_t2_sde_fsdbinary_dogma_and_content_raw_client_verification() {
    // parity: sde:res:/staticdata/typedogma.fsdbinary, sde:res:/staticdata/missions.fsdbinary
    // parity: sde:res:/staticdata/epicarcs.fsdbinary, sde:res:/staticdata/dungeons.fsdbinary
    // parity: sde:res:/staticdata/nodegraphs.fsdbinary
    let sde = SdeRegistry::open(&SdeRegistry::default_db_path()).expect("real V24.01 SDE");
    let labels = load_en_us_labels(&sde);
    assert_eq!(labels[&524_288], "Tormentor Ironblood Serenity Only SKIN (30 Days)");

    // typedogma: key u64, dogmaAttributes list rel u64, dogmaEffects list rel u64 (stride 24).
    // Attribute entries are 16 bytes (value f64, attributeID u32); effect entries are 8 bytes
    // (effectID u32, isDefault u8).
    let td = load(&sde, TYPEDOGMA, 24);
    assert_eq!(td.records.len(), 27_160);
    let mut in_typedogma = std::collections::HashSet::new();
    let mut attr_total = 0usize;
    let mut with_default = 0usize;
    let mut ulp_off = 0usize;
    for &rec in &td.records {
        let tid = u64_at(&td.raw, rec) as u32;
        let (a_pos, a_cnt) = cfsd_list(&td.raw, u64_at(&td.raw, rec + 8));
        let attrs: HashMap<u16, f64> = (0..a_cnt)
            .map(|j| {
                let e = a_pos + j * 16;
                (u32_at(&td.raw, e + 8) as u16, f64_at(&td.raw, e))
            })
            .collect();
        assert_eq!(attrs.len(), a_cnt, "duplicate attribute in type {tid}");
        let (e_pos, e_cnt) = cfsd_list(&td.raw, u64_at(&td.raw, rec + 16));
        let effects: Vec<(u16, bool)> = (0..e_cnt)
            .map(|j| {
                let e = e_pos + j * 8;
                (u32_at(&td.raw, e) as u16, td.raw[e + 4] != 0)
            })
            .collect();
        let default_effect = effects.iter().find(|e| e.1).map(|e| e.0);
        let inv = sde.types.get(&tid).expect("typedogma type in sde.types");
        assert_eq!(inv.default_attributes.len(), attrs.len(), "attrs of {tid}");
        for (aid, &client_value) in &attrs {
            let server_value = inv.default_attributes[aid];
            // `attributes_json` goes through serde_json's default (not correctly rounded) float
            // parser, which can land one ulp away from the client's f64; allow exactly that.
            let ulps = (server_value.to_bits() as i64 - client_value.to_bits() as i64).abs();
            assert!(ulps <= 1, "type {tid} attr {aid}: {server_value} vs {client_value}");
            ulp_off += usize::from(ulps == 1);
        }
        let ids: Vec<u16> = effects.iter().map(|e| e.0).collect();
        assert_eq!(inv.effect_ids, ids, "effects of type {tid}");
        assert_eq!(inv.default_effect_id, default_effect, "default effect {tid}");
        attr_total += a_cnt;
        with_default += usize::from(default_effect.is_some());
        in_typedogma.insert(tid);
    }
    assert_eq!(with_default, 8_755);
    assert_eq!(attr_total, 648_726);
    assert_eq!(ulp_off, 5, "float drift must stay at the known 5 values");
    let without: Vec<_> = sde
        .types
        .values()
        .filter(|t| !in_typedogma.contains(&t.type_id))
        .collect();
    assert_eq!(without.len(), sde.types.len() - 27_160);
    for t in without {
        assert!(t.default_attributes.is_empty() && t.effect_ids.is_empty());
        assert_eq!(t.default_effect_id, None);
    }
    let raven = &sde.types[&638];
    assert_eq!(raven.effect_ids, vec![891, 892, 1885, 1886, 5618, 8377]);
    assert_eq!(raven.default_attributes.len(), 90);
    assert_eq!(raven.default_attributes[&37], 113.0);
    assert_eq!(raven.default_attributes[&5796], 2_147_483_647_000.0);

    // missions: stride 224; contentTemplate string rel @48, fixedLpRewardAlpha i32 @124,
    // fixedLpRewardOmega i32 @128, nameID u32 @196, hasStandingRewards u8 @212, optional-field
    // mask u32 @216 (bit 4 = courierMission present).
    let mi = load(&sde, MISSIONS, 224);
    assert_eq!(mi.records.len(), 2_892);
    assert_eq!(sde.missions.len(), mi.records.len());
    let mut couriers = 0usize;
    let mut decoded_875 = None;
    for &rec in &mi.records {
        let r = &mi.raw;
        let mid = u64_at(r, rec);
        let is_courier = u32_at(r, rec + 216) & (1 << 4) != 0;
        let lp_alpha = i64::from(i32_at(r, rec + 124));
        let lp_omega = i64::from(i32_at(r, rec + 128));
        let standing = r[rec + 212] != 0;
        let template = cfsd_str(r, u64_at(r, rec + 48));
        let title = expected_name(&labels, u32_at(r, rec + 196), "Mission", mid);
        let m = sde.missions.get(&(mid as u32)).expect("mission in sde");
        assert_eq!(m.title, title, "mission {mid} title");
        assert_eq!(m.is_courier, is_courier, "mission {mid} courier");
        assert_eq!((m.lp_alpha, m.lp_omega), (lp_alpha, lp_omega));
        assert_eq!(m.has_standing_rewards, standing);
        assert_eq!(m.content_template, template, "mission {mid} template");
        couriers += usize::from(is_courier);
        if mid == 875 {
            decoded_875 = Some((title, template, standing));
        }
    }
    assert_eq!(couriers, 1_155);
    let (title, template, standing) = decoded_875.expect("mission 875 in client file");
    assert_eq!(title, "The Guristas Spies");
    assert_eq!(
        template.as_deref(),
        Some("agent.missionTemplatizedContent_BasicKillMission")
    );
    assert!(standing);

    // epicarcs: stride 56; missions dict rel @16 + entry count @24, arcRestartInterval u32 @32,
    // epicArcNameID u32 @36, factionID u32 @40, mask u32 @48 (bit 1 factionID, bit 2 restart).
    let ea = load(&sde, EPICARCS, 56);
    assert_eq!(ea.records.len(), 21);
    assert_eq!(sde.epic_arcs.len(), ea.records.len());
    let mut decoded_29 = None;
    for &rec in &ea.records {
        let r = &ea.raw;
        let id = u64_at(r, rec);
        let mask = u32_at(r, rec + 48);
        let missions = cfsd_dict_len(r, u64_at(r, rec + 16));
        assert_eq!(missions as u64, u64_at(r, rec + 24), "arc {id} mission dict");
        let faction = (mask & 2 != 0).then(|| u32_at(r, rec + 40));
        let restart = (mask & 4 != 0).then(|| u32_at(r, rec + 32));
        let name = expected_name(&labels, u32_at(r, rec + 36), "Epic Arc", id);
        let arc = sde.epic_arcs.get(&(id as u32)).expect("epic arc in sde");
        assert_eq!(arc.name, name);
        assert_eq!(arc.faction_id, faction, "arc {id} faction");
        assert_eq!(arc.restart_interval, restart, "arc {id} restart");
        assert_eq!(arc.missions_count as usize, missions, "arc {id} missions");
        if id == 29 {
            decoded_29 = Some((name, faction, restart, missions));
        }
    }
    assert_eq!(
        decoded_29,
        Some((
            "The Blood-Stained Stars".to_string(),
            Some(500_016),
            Some(129_600),
            64
        ))
    );

    // dungeons: stride 120; archetypeID u32 @64, difficulty u32 @72, dungeonNameID u32 @80,
    // factionID u32 @92, mask u32 @112 (bit 4 difficulty, bit 7 factionID, bit 13 archetypeID).
    let du = load(&sde, DUNGEONS, 120);
    assert_eq!(du.records.len(), 5_516);
    assert_eq!(sde.dungeons.len(), du.records.len());
    let mut unlabelled = 0usize;
    let mut decoded_43 = None;
    for &rec in &du.records {
        let r = &du.raw;
        let id = u64_at(r, rec);
        let mask = u32_at(r, rec + 112);
        let archetype = (mask & (1 << 13) != 0).then(|| u32_at(r, rec + 64));
        let difficulty = (mask & (1 << 4) != 0).then(|| u32_at(r, rec + 72));
        let faction = (mask & (1 << 7) != 0).then(|| u32_at(r, rec + 92));
        let name_id = u32_at(r, rec + 80);
        unlabelled += usize::from(!labels.contains_key(&name_id));
        let name = expected_name(&labels, name_id, "Dungeon", id);
        let d = sde.dungeons.get(&(id as u32)).expect("dungeon in sde");
        assert_eq!(d.name, name, "dungeon {id} name");
        assert_eq!(d.archetype_id, archetype, "dungeon {id} archetype");
        assert_eq!(d.difficulty, difficulty, "dungeon {id} difficulty");
        assert_eq!(d.faction_id, faction, "dungeon {id} faction");
        if id == 43 {
            decoded_43 = Some((name, archetype, faction, difficulty));
        }
    }
    assert_eq!(unlabelled, 5);
    assert_eq!(
        decoded_43,
        Some((
            "Pith Merchant Depot".to_string(),
            Some(24),
            Some(500_010),
            Some(2)
        ))
    );

    // nodegraphs: stride 72; name string rel @16, nodes dict rel @24 + entry count @32, tags
    // list rel @56 (u64 string offsets), parentGraph u32 @64, mask u32 @68 (bit 1 parentGraph).
    let ng = load(&sde, NODEGRAPHS, 72);
    assert_eq!(ng.records.len(), 584);
    assert_eq!(sde.node_graphs.len(), ng.records.len());
    let mut decoded_1 = None;
    for &rec in &ng.records {
        let r = &ng.raw;
        let id = u64_at(r, rec);
        let name = cfsd_str(r, u64_at(r, rec + 16)).unwrap_or_default();
        let nodes = cfsd_dict_len(r, u64_at(r, rec + 24));
        assert_eq!(nodes as u64, u64_at(r, rec + 32), "graph {id} node dict");
        let (t_pos, t_cnt) = cfsd_list(r, u64_at(r, rec + 56));
        let tags: Vec<String> = (0..t_cnt)
            .map(|j| cfsd_str(r, u64_at(r, t_pos + j * 8)).expect("tag string"))
            .collect();
        let parent = (u32_at(r, rec + 68) & 2 != 0).then(|| u32_at(r, rec + 64));
        let g = sde.node_graphs.get(&(id as u32)).expect("node graph in sde");
        assert_eq!(g.name, name, "graph {id} name");
        assert_eq!(g.node_count as usize, nodes, "graph {id} nodes");
        assert_eq!(g.tags, tags, "graph {id} tags");
        assert_eq!(g.parent_graph_id, parent, "graph {id} parent");
        if id == 1 {
            decoded_1 = Some((name, nodes, tags, parent));
        }
    }
    assert_eq!(
        decoded_1,
        Some((
            "ScratchPad C-1".to_string(),
            2,
            vec!["client".to_string(), "scratchpad".to_string()],
            None
        ))
    );

    eve_testkit::attest(&[
        "sde:res:/staticdata/typedogma.fsdbinary",
        "sde:res:/staticdata/missions.fsdbinary",
        "sde:res:/staticdata/epicarcs.fsdbinary",
        "sde:res:/staticdata/dungeons.fsdbinary",
        "sde:res:/staticdata/nodegraphs.fsdbinary",
    ]);
}
