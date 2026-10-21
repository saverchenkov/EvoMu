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
const REQ_SKILLS_FOR_TYPES: (&str, &str, u64) = (
    "res:/staticdata/requiredskillsfortypes.fsdbinary",
    "58d4d940882dbe8af6531268679d87e4",
    863_720,
);
const REQ_SKILLS_INDEX: (&str, &str, u64) = (
    "res:/staticdata/requiredskillsindex.fsdbinary",
    "d3d5a70e4b51d03b2bab6ae17857c6d0",
    266_136,
);
const EN_US: (&str, &str, u64) = (
    "res:/localizationfsd/localization_fsd_en-us.pickle",
    "7a753c9da2eeca4dcc7cf0c6655aaae0",
    40_753_078,
);

/// Walks a nested `cfsd` hash dict whose entries are 24-byte `(key u64, inner_dict_rel u64,
/// inner_dict_len u64)` triples.
fn cfsd_dict_of_dicts(buf: &[u8], rel: u64, expected_len: usize) -> Vec<(u64, u64, usize)> {
    let abs = cfsd_abs(rel);
    let buckets = u64_at(buf, abs - 8) as usize;
    let mut out = Vec::with_capacity(expected_len);
    for b in 0..buckets {
        let boff = u64_at(buf, abs + b * 8);
        if boff == 0 {
            continue;
        }
        let babs = cfsd_abs(boff);
        let bn = u64_at(buf, babs - 8) as usize;
        for i in 0..bn {
            let e = babs + i * 24;
            out.push((u64_at(buf, e), u64_at(buf, e + 8), u64_at(buf, e + 16) as usize));
        }
    }
    assert_eq!(out.len(), expected_len, "cfsd nested dict count");
    out
}

/// Walks a nested `cfsd` hash dict whose entries are 16-byte `(key u64, list_rel u64)` pairs where
/// each list holds `u32` elements.
fn cfsd_dict_of_u32_lists(buf: &[u8], rel: u64, expected_len: usize) -> HashMap<u32, Vec<u32>> {
    let abs = cfsd_abs(rel);
    let buckets = u64_at(buf, abs - 8) as usize;
    let mut out = HashMap::with_capacity(expected_len);
    for b in 0..buckets {
        let boff = u64_at(buf, abs + b * 8);
        if boff == 0 {
            continue;
        }
        let babs = cfsd_abs(boff);
        let bn = u64_at(buf, babs - 8) as usize;
        for i in 0..bn {
            let e = babs + i * 16;
            let k = u64_at(buf, e) as u32;
            let (l_pos, l_cnt) = cfsd_list(buf, u64_at(buf, e + 8));
            let mut tids: Vec<u32> = (0..l_cnt).map(|j| u32_at(buf, l_pos + j * 4)).collect();
            tids.sort_unstable();
            out.insert(k, tids);
        }
    }
    assert_eq!(out.len(), expected_len, "cfsd list-dict count");
    out
}

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
    // parity: sde:res:/staticdata/requiredskillsfortypes.fsdbinary, sde:res:/staticdata/requiredskillsindex.fsdbinary
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

    // requiredskillsfortypes: stride 24 (type_id u64, skills_dict rel u64, skills_dict len u64).
    // Inner dict entries are 8-byte (skill_type_id u32, level u32) pairs.
    let rft = load(&sde, REQ_SKILLS_FOR_TYPES, 24);
    assert_eq!(rft.records.len(), 9_573);
    let mut total_skill_reqs = 0usize;
    for &rec in &rft.records {
        let r = &rft.raw;
        let tid = u64_at(r, rec) as u32;
        let d_rel = u64_at(r, rec + 8);
        let d_len = u64_at(r, rec + 16) as usize;
        let abs = cfsd_abs(d_rel);
        let buckets = u64_at(r, abs - 8) as usize;
        let mut client_skills: HashMap<u32, u8> = HashMap::with_capacity(d_len);
        for b in 0..buckets {
            let boff = u64_at(r, abs + b * 8);
            if boff == 0 {
                continue;
            }
            let babs = cfsd_abs(boff);
            let bn = u64_at(r, babs - 8) as usize;
            for j in 0..bn {
                let e = babs + j * 8;
                client_skills.insert(u32_at(r, e), u32_at(r, e + 4) as u8);
            }
        }
        assert_eq!(client_skills.len(), d_len, "type {tid} skill dict len");
        total_skill_reqs += d_len;
        assert_eq!(
            sde.required_skills_for_type(tid),
            client_skills,
            "required skills for type {tid}"
        );
    }
    assert_eq!(total_skill_reqs, 12_432);
    assert_eq!(
        sde.required_skills_for_type(638),
        HashMap::from([(3338, 1)])
    );

    // requiredskillsindex: stride 24 (skill_type_id u64, level_dict rel u64, level_dict len u64).
    // Nested dicts map level -> root_market_group_id -> meta_group_id -> sorted Vec<type_id>.
    let rsi = load(&sde, REQ_SKILLS_INDEX, 24);
    assert_eq!(rsi.records.len(), 421);
    let server_index = sde.required_skills_index();
    assert_eq!(server_index.len(), rsi.records.len());
    let mut indexed_type_entries = 0usize;
    for &rec in &rsi.records {
        let r = &rsi.raw;
        let sk_id = u64_at(r, rec) as u32;
        let lvl_entries = cfsd_dict_of_dicts(r, u64_at(r, rec + 8), u64_at(r, rec + 16) as usize);
        let mut client_lvl_map: HashMap<u8, HashMap<u32, HashMap<u32, Vec<u32>>>> =
            HashMap::with_capacity(lvl_entries.len());
        for (lvl, mg_rel, mg_len) in lvl_entries {
            let mg_entries = cfsd_dict_of_dicts(r, mg_rel, mg_len);
            let mut client_mg_map: HashMap<u32, HashMap<u32, Vec<u32>>> =
                HashMap::with_capacity(mg_entries.len());
            for (rmg, meta_rel, meta_len) in mg_entries {
                let meta_map = cfsd_dict_of_u32_lists(r, meta_rel, meta_len);
                indexed_type_entries += meta_map.values().map(Vec::len).sum::<usize>();
                client_mg_map.insert(rmg as u32, meta_map);
            }
            client_lvl_map.insert(lvl as u8, client_mg_map);
        }
        assert_eq!(
            server_index.get(&sk_id),
            Some(&client_lvl_map),
            "requiredskillsindex mismatch for skill {sk_id}"
        );
    }
    assert_eq!(indexed_type_entries, 16_970);

    eve_testkit::attest(&[
        "sde:res:/staticdata/typedogma.fsdbinary",
        "sde:res:/staticdata/missions.fsdbinary",
        "sde:res:/staticdata/epicarcs.fsdbinary",
        "sde:res:/staticdata/dungeons.fsdbinary",
        "sde:res:/staticdata/nodegraphs.fsdbinary",
        "sde:res:/staticdata/requiredskillsfortypes.fsdbinary",
        "sde:res:/staticdata/requiredskillsindex.fsdbinary",
    ]);
}

const TYPES_RES: (&str, &str, u64) = (
    "res:/staticdata/types.fsdbinary",
    "c0ee9c292aa113737029c9ec63503233",
    9_169_272,
);
const AGENTS_IN_SPACE: (&str, &str, u64) = (
    "res:/staticdata/agentsinspace.fsdbinary",
    "588bd7a75698b8d164b7dddbc5e50818",
    11_576,
);
const NPC_CHARACTERS: (&str, &str, u64) = (
    "res:/staticdata/npccharacters.fsdbinary",
    "9a5efb6a18648808dc9b1ccffd8259ac",
    1_873_264,
);
const CORP_ACTIVITIES: (&str, &str, u64) = (
    "res:/staticdata/corporationactivities.fsdbinary",
    "5c40c3ba3f7875dc257a1eda6ab43721",
    296,
);
const NPC_CORP_DIVISIONS: (&str, &str, u64) = (
    "res:/staticdata/npccorporationdivisions.fsdbinary",
    "93187a5122247235d7f5b0e0d9c28719",
    1_075,
);
const CONTRABAND_TYPES: (&str, &str, u64) = (
    "res:/staticdata/contrabandtypes.fsdbinary",
    "73e624f59b01d5230a5ecd00fe1669a9",
    1_680,
);
const DOGMA_UNITS: (&str, &str, u64) = (
    "res:/staticdata/dogmaunits.fsdbinary",
    "bc428339035ee07dfebf376f4cd34ba9",
    3_769,
);
const ACCOUNTING_ENTRY_TYPES: (&str, &str, u64) = (
    "res:/staticdata/accountingentrytypes.fsdbinary",
    "5ae04315ca2fbf678278fc0f47ac0c05",
    27_051,
);
const ACCOUNTING_KEYS: (&str, &str, u64) = (
    "res:/staticdata/accountingkeys.fsdbinary",
    "6195face9aa40f4c3d253ddf5df0bcc8",
    2_016,
);
const META_GROUPS: (&str, &str, u64) = (
    "res:/staticdata/metagroups.fsdbinary",
    "20d30e22d51debd9e46795d1da61a248",
    984,
);
const GRAPHIC_IDS: (&str, &str, u64) = (
    "res:/staticdata/graphicids.fsdbinary",
    "016ae914e50fa2839fe1082616d91ec4",
    1_535_928,
);
const ICON_IDS: (&str, &str, u64) = (
    "res:/staticdata/iconids.fsdbinary",
    "52ac1339c7a1cd0270e8df2d4d9020a4",
    546_952,
);
const SOUND_IDS: (&str, &str, u64) = (
    "res:/staticdata/soundids.fsdbinary",
    "ee04e8241837c12838d952c3e5169494",
    101_640,
);

#[test]
fn test_t2_sde_fsdbinary_characters_economy_and_assets_verification() {
    // parity: sde:res:/staticdata/types.fsdbinary, sde:res:/staticdata/fighterabilities.static
    // parity: sde:res:/staticdata/agentsinspace.fsdbinary, sde:res:/staticdata/npccharacters.fsdbinary
    // parity: sde:res:/staticdata/corporationactivities.fsdbinary, sde:res:/staticdata/npccorporationdivisions.fsdbinary
    // parity: sde:res:/staticdata/contrabandtypes.fsdbinary, sde:res:/staticdata/dogmaunits.fsdbinary
    // parity: sde:res:/staticdata/accountingentrytypes.fsdbinary, sde:res:/staticdata/accountingkeys.fsdbinary
    // parity: sde:res:/staticdata/metagroups.fsdbinary, sde:res:/staticdata/graphicids.fsdbinary
    // parity: sde:res:/staticdata/iconids.fsdbinary, sde:res:/staticdata/soundids.fsdbinary
    let sde = SdeRegistry::open(&SdeRegistry::default_db_path()).expect("real V24.01 SDE");
    let labels = load_en_us_labels(&sde);

    // 1. types.fsdbinary localized typeNameID (@132) and descriptionID (@68) verification across all 53,288 types
    let tf = load(&sde, TYPES_RES, 152);
    assert_eq!(tf.records.len(), 53_288);
    let mut resolved_descriptions = 0usize;
    for &rec in &tf.records {
        let tid = u64_at(&tf.raw, rec) as u32;
        let desc_id = u32_at(&tf.raw, rec + 68);
        let name_id = u32_at(&tf.raw, rec + 132);
        let mask = u32_at(&tf.raw, rec + 148);
        let inv = sde.types.get(&tid).expect("type in sde.types");
        assert_eq!(
            inv.name,
            expected_name(&labels, name_id, "Type", u64::from(tid)),
            "type {tid} localized name"
        );
        assert_eq!(inv.description_id, ((mask & 8) != 0).then_some(desc_id));
        if let Some(did) = inv.description_id {
            if labels.contains_key(&did) {
                resolved_descriptions += 1;
            }
        }
    }
    assert_eq!(resolved_descriptions, 34_652);

    // 2. fighterabilities.static joined with fighterabilitiesbytype.static via SdeRegistry::fighter_ability_slots_with_info
    assert_eq!(sde.fighter_abilities.len(), 36);
    assert_eq!(sde.fighter_abilities_by_type.len(), 94);
    let mut total_fighter_slots = 0usize;
    for (&ftid, raw_slots) in &sde.fighter_abilities_by_type {
        let joined = sde.fighter_ability_slots_with_info(ftid);
        assert_eq!(joined.len(), raw_slots.len(), "fighter {ftid} slot join");
        for (slot, info) in joined {
            total_fighter_slots += 1;
            assert_eq!(slot.ability_id, info.ability_id);
            assert!(
                matches!(
                    info.target_mode.as_str(),
                    "untargeted" | "itemTargeted" | "pointTargeted"
                ),
                "unexpected fighter target_mode {}",
                info.target_mode
            );
            if let Some(dnid) = info.display_name_id {
                assert!(labels.contains_key(&dnid), "fighter ability display_name_id {dnid}");
            }
            if let Some(ttid) = info.tooltip_text_id {
                assert!(labels.contains_key(&ttid), "fighter ability tooltip_text_id {ttid}");
            }
        }
    }
    assert!(total_fighter_slots >= 170);

    // 3. agentsinspace.fsdbinary (360 records, stride 20)
    let ais_raw = load(&sde, AGENTS_IN_SPACE, 20);
    let ais = sde.load_agents_in_space().expect("load_agents_in_space");
    assert_eq!(ais_raw.records.len(), 360);
    assert_eq!(ais.len(), 360);
    for &rec in &ais_raw.records {
        let r = &ais_raw.raw;
        let aid = u32_at(r, rec);
        let entry = ais.get(&aid).expect("agent in space");
        assert_eq!(entry.dungeon_id, u32_at(r, rec + 4));
        assert_eq!(entry.solar_system_id, u32_at(r, rec + 8));
        assert_eq!(entry.spawn_point_id, u32_at(r, rec + 12));
        assert_eq!(entry.type_id, u32_at(r, rec + 16));
        let ag = sde.agents.get(&i64::from(aid)).expect("agent in sde.agents");
        assert_eq!(ag.solar_system_id, Some(entry.solar_system_id));
        assert!(sde.solar_systems.contains_key(&entry.solar_system_id));
        assert!(sde.types.contains_key(&entry.type_id));
    }
    assert_eq!(ais[&3_018_870].solar_system_id, 30_002_548);
    assert_eq!(ais[&3_018_870].dungeon_id, 1_063);
    assert_eq!(ais[&3_018_870].type_id, 26_176);

    // 4. npccharacters.fsdbinary (11,424 records, stride 136; 10,972 agents in sde.agents)
    let nc_raw = load(&sde, NPC_CHARACTERS, 136);
    let nc = sde.load_npc_characters().expect("load_npc_characters");
    assert_eq!(nc_raw.records.len(), 11_424);
    assert_eq!(nc.len(), 11_424);
    let mut agent_count = 0usize;
    for &rec in &nc_raw.records {
        let r = &nc_raw.raw;
        let cid = u64_at(r, rec) as i64;
        let ch = nc.get(&cid).expect("npc character");
        assert!(sde.npc_corporations.contains_key(&ch.corporation_id));
        assert!(sde.bloodlines.contains_key(&ch.bloodline_id));
        if let Some(rid) = ch.race_id {
            assert!(sde.races.contains_key(&rid));
        }
        if let Some(aid) = ch.ancestry_id {
            assert!(sde.ancestries.contains_key(&aid));
        }
        if ch.is_agent {
            agent_count += 1;
            let ag = sde.agents.get(&cid).expect("agent in sde.agents");
            assert_eq!(ag.corporation_id, ch.corporation_id);
            assert_eq!(ag.division_id, ch.agent_division_id.unwrap());
            assert_eq!(ag.level, ch.agent_level.unwrap() as u8);
            assert_eq!(ag.agent_type_id, ch.agent_type_id.unwrap());
            assert_eq!(ag.is_locator, ch.is_locator);
            assert_eq!(
                ag.name,
                expected_name(&labels, ch.name_id, "Agent", cid as u64)
            );
        }
    }
    assert_eq!(agent_count, sde.agents.len());
    assert_eq!(agent_count, 10_972);

    // 5. corporationactivities.fsdbinary (20 records, stride 8)
    let ca_raw = load(&sde, CORP_ACTIVITIES, 8);
    let ca = sde
        .load_corporation_activities()
        .expect("load_corporation_activities");
    assert_eq!(ca_raw.records.len(), 20);
    assert_eq!(ca.len(), 20);
    for &rec in &ca_raw.records {
        let id = u32_at(&ca_raw.raw, rec);
        let nid = u32_at(&ca_raw.raw, rec + 4);
        assert_eq!(ca[&id].name_id, nid);
        assert!(labels.contains_key(&nid));
    }
    for op in sde.station_operations.values() {
        if let Some(act_id) = op.activity_id {
            assert!(ca.contains_key(&act_id));
        }
    }

    // 6. npccorporationdivisions.fsdbinary (10 records, stride 40)
    let ncd_raw = load(&sde, NPC_CORP_DIVISIONS, 40);
    let ncd = sde
        .load_npc_corporation_divisions()
        .expect("load_npc_corporation_divisions");
    assert_eq!(ncd_raw.records.len(), 10);
    assert_eq!(ncd.len(), 10);
    for div in ncd.values() {
        assert!(!div.internal_name.is_empty());
        assert!(labels.contains_key(&div.name_id));
        assert!(labels.contains_key(&div.leader_type_name_id));
    }
    assert_eq!(labels[&ncd[&24].name_id], "Security");

    // 7. contrabandtypes.fsdbinary (8 records, stride 24)
    let cb_raw = load(&sde, CONTRABAND_TYPES, 24);
    let cb = sde.load_contraband_types().expect("load_contraband_types");
    assert_eq!(cb_raw.records.len(), 8);
    assert_eq!(cb.len(), 8);
    let mut total_faction_rules = 0usize;
    for (&tid, rules) in &cb {
        assert!(sde.types.contains_key(&tid));
        assert!(!rules.is_empty());
        for (&fid, rule) in rules {
            total_faction_rules += 1;
            assert!(sde.factions.contains_key(&fid));
            assert!(rule.fine_by_value >= 0.0);
        }
    }
    assert!(total_faction_rules >= 40);
    assert!(cb.contains_key(&3721)); // Slaves

    // 8. dogmaunits.fsdbinary (60 records, stride 32)
    let du_raw = load(&sde, DOGMA_UNITS, 32);
    let du = sde.load_dogma_units().expect("load_dogma_units");
    assert_eq!(du_raw.records.len(), 60);
    assert_eq!(du.len(), 60);
    for u in du.values() {
        assert!(!u.name.is_empty());
        if let Some(dnid) = u.display_name_id {
            assert!(labels.contains_key(&dnid));
        }
    }
    assert_eq!(du[&1].name, "Length");
    assert_eq!(du[&101].name, "Milliseconds");

    // 9. accountingentrytypes.fsdbinary (218 records, stride 40)
    let aet_raw = load(&sde, ACCOUNTING_ENTRY_TYPES, 40);
    let aet = sde
        .load_accounting_entry_types()
        .expect("load_accounting_entry_types");
    assert_eq!(aet_raw.records.len(), 218);
    assert_eq!(aet.len(), 218);
    for e in aet.values() {
        assert!(!e.name.is_empty());
        assert!(labels.contains_key(&e.name_id));
    }
    assert_eq!(aet[&10].name, "Player Donation");
    assert_eq!(aet[&85].name, "Bounty Prizes");
    assert!(labels.contains_key(&aet[&10].journal_message_id));

    // 10. accountingkeys.fsdbinary (19 records, stride 48)
    let ak_raw = load(&sde, ACCOUNTING_KEYS, 48);
    let ak = sde.load_accounting_keys().expect("load_accounting_keys");
    assert_eq!(ak_raw.records.len(), 19);
    assert_eq!(ak.len(), 19);
    for k in ak.values() {
        assert!(!k.key_name.is_empty());
        assert!(!k.key_type.is_empty());
    }
    assert_eq!(ak[&1000].key_name, "cash");
    assert_eq!(ak[&1000].currency.as_deref(), Some("ISK"));
    assert!(labels.contains_key(&ak[&1000].name_id));

    // 11. metagroups.fsdbinary (13 records, stride 48)
    let mg_raw = load(&sde, META_GROUPS, 48);
    let mg = sde.load_meta_groups().expect("load_meta_groups");
    assert_eq!(mg_raw.records.len(), 13);
    assert_eq!(mg.len(), 13);
    for m in mg.values() {
        assert!(labels.contains_key(&m.name_id));
    }
    for inv in sde.types.values() {
        if let Some(mg_id) = inv.meta_group_id {
            assert!(mg.contains_key(&mg_id), "type {} meta_group_id {mg_id}", inv.type_id);
        }
    }
    assert_eq!(labels[&mg[&1].name_id], "Tech I");
    assert_eq!(labels[&mg[&2].name_id], "Tech II");
    assert_eq!(labels[&mg[&14].name_id], "Tech III");

    // 12. graphicids.fsdbinary (6,159 records, stride 160), iconids.fsdbinary (4,804 records, stride 32), soundids.fsdbinary (1,470 records, stride 16)
    let gid_raw = load(&sde, GRAPHIC_IDS, 160);
    let gids = sde.load_graphic_ids().expect("load_graphic_ids");
    assert_eq!(gid_raw.records.len(), 6_159);
    assert_eq!(gids.len(), 6_159);

    let iid_raw = load(&sde, ICON_IDS, 32);
    let iids = sde.load_icon_ids().expect("load_icon_ids");
    assert_eq!(iid_raw.records.len(), 4_804);
    assert_eq!(iids.len(), 4_804);
    for ic in iids.values() {
        assert!(!ic.icon_file.is_empty());
    }

    let sid_raw = load(&sde, SOUND_IDS, 16);
    let sids = sde.load_sound_ids().expect("load_sound_ids");
    assert_eq!(sid_raw.records.len(), 1_470);
    assert_eq!(sids.len(), 1_470);

    let mut resolved_type_graphics = 0usize;
    let mut resolved_type_icons = 0usize;
    let mut resolved_type_sounds = 0usize;
    for inv in sde.types.values() {
        if let Some(gid) = inv.graphic_id {
            if gids.contains_key(&gid) {
                resolved_type_graphics += 1;
            }
        }
        if let Some(iid) = inv.icon_id {
            if iids.contains_key(&iid) {
                resolved_type_icons += 1;
            }
        }
        if let Some(sid) = inv.sound_id {
            if sids.contains_key(&sid) {
                resolved_type_sounds += 1;
            }
        }
    }
    assert_eq!(resolved_type_graphics, 18_816);
    assert_eq!(resolved_type_icons, 23_088);
    assert_eq!(resolved_type_sounds, 5_091);

    eve_testkit::attest(&[
        "sde:res:/staticdata/types.fsdbinary",
        "sde:res:/staticdata/fighterabilities.static",
        "sde:res:/staticdata/agentsinspace.fsdbinary",
        "sde:res:/staticdata/npccharacters.fsdbinary",
        "sde:res:/staticdata/corporationactivities.fsdbinary",
        "sde:res:/staticdata/npccorporationdivisions.fsdbinary",
        "sde:res:/staticdata/contrabandtypes.fsdbinary",
        "sde:res:/staticdata/dogmaunits.fsdbinary",
        "sde:res:/staticdata/accountingentrytypes.fsdbinary",
        "sde:res:/staticdata/accountingkeys.fsdbinary",
        "sde:res:/staticdata/metagroups.fsdbinary",
        "sde:res:/staticdata/graphicids.fsdbinary",
        "sde:res:/staticdata/iconids.fsdbinary",
        "sde:res:/staticdata/soundids.fsdbinary",
    ]);
}

