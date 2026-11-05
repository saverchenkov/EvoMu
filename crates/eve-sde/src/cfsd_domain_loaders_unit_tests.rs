//! Unit and mutation-killing tests for `cfsd_domain_loaders`.

use super::*;

struct CfsdBuilder {
    buf: Vec<u8>,
}

impl CfsdBuilder {
    fn new(records: &[Vec<u8>]) -> Self {
        let mut buf = vec![0u8; 72];
        //cnt at 40, buckets = 2 at 48 (bucket 0 empty = 0, bucket 1 populated)
        buf[40..48].copy_from_slice(&(records.len() as u64).to_le_bytes());
        buf[48..56].copy_from_slice(&2u64.to_le_bytes());
        buf[56..64].copy_from_slice(&0u64.to_le_bytes());
        // Allocate bucket 1 at end of header: 8-byte count prefix + records
        let count_pos = buf.len();
        buf.extend_from_slice(&(records.len() as u64).to_le_bytes());
        let abs = buf.len();
        let boff = (abs - 32) as u64;
        buf[64..72].copy_from_slice(&boff.to_le_bytes());
        let _ = count_pos;
        for r in records {
            buf.extend_from_slice(r);
        }
        Self { buf }
    }

    fn alloc_str(&mut self, s: &str) -> u64 {
        self.buf
            .extend_from_slice(&(s.len() as u64).to_le_bytes());
        let abs = self.buf.len();
        self.buf.extend_from_slice(s.as_bytes());
        (abs - 32) as u64
    }

    fn write_u32(&mut self, abs_pos: usize, v: u32) {
        self.buf[abs_pos..abs_pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn write_i32(&mut self, abs_pos: usize, v: i32) {
        self.buf[abs_pos..abs_pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn write_u64(&mut self, abs_pos: usize, v: u64) {
        self.buf[abs_pos..abs_pos + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn write_i64(&mut self, abs_pos: usize, v: i64) {
        self.buf[abs_pos..abs_pos + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn write_f32(&mut self, abs_pos: usize, v: f32) {
        self.buf[abs_pos..abs_pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn write_f64(&mut self, abs_pos: usize, v: f64) {
        self.buf[abs_pos..abs_pos + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn rec_offset(idx: usize, stride: usize) -> usize {
        80 + idx * stride
    }
}

fn register_cfsd_resource(
    sde: &mut SdeRegistry,
    res_path: &str,
    tag: &str,
    bytes: &[u8],
    record_count: usize,
) -> PathBuf {
    let file_path = std::env::temp_dir().join(format!(
        "evomu_cfsd_dom_{}_{}.fsdbinary",
        tag,
        std::process::id()
    ));
    std::fs::write(&file_path, bytes).expect("write temp cfsd file");
    sde.resources.insert(
        res_path.to_string(),
        SdeResourceInfo {
            res_path: res_path.to_string(),
            cdn_relative_path: file_path.display().to_string(),
            md5: String::new(),
            byte_size: bytes.len() as u64,
            format: "cfsd".to_string(),
            record_count,
            spot: String::new(),
        },
    );
    file_path
}

#[test]
fn agents_npc_characters_corp_activities_divisions_and_contraband() {
    let mut sde = SdeRegistry::default();
    let mut files = Vec::new();

    // 1. agentsinspace.fsdbinary (stride 20)
    let mut b_ais = CfsdBuilder::new(&[vec![0u8; 20]]);
    let r0 = CfsdBuilder::rec_offset(0, 20);
    b_ais.write_u32(r0, 3_019_480);
    b_ais.write_u32(r0 + 4, 1234);
    b_ais.write_u32(r0 + 8, 30_000_142);
    b_ais.write_u32(r0 + 12, 55);
    b_ais.write_u32(r0 + 16, 17_703);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/agentsinspace.fsdbinary",
        "ais",
        &b_ais.buf,
        1,
    ));
    let ais = sde.load_agents_in_space().expect("load_agents_in_space");
    assert_eq!(
        ais.get(&3_019_480),
        Some(&AgentInSpaceInfo {
            agent_id: 3_019_480,
            dungeon_id: 1234,
            solar_system_id: 30_000_142,
            spawn_point_id: 55,
            type_id: 17_703,
        })
    );

    // 2. npccharacters.fsdbinary (stride 136): 3 records testing all mask bits & locator logic
    let mut b_npc = CfsdBuilder::new(&[vec![0u8; 136], vec![0u8; 136], vec![0u8; 136]]);
    let n0 = CfsdBuilder::rec_offset(0, 136);
    b_npc.write_u64(n0, 3_008_416);
    b_npc.write_u32(n0 + 16, 24); // agent_division_id
    b_npc.write_u32(n0 + 20, 4); // agent_level
    b_npc.write_u32(n0 + 24, 2); // agent_type_id
    b_npc.buf[n0 + 28] = 1; // is_locator
    b_npc.write_u32(n0 + 64, 11); // ancestry_id (mask & 2)
    b_npc.write_u32(n0 + 68, 5); // bloodline_id
    b_npc.write_u32(n0 + 80, 1_000_035); // corporation_id
    b_npc.write_u32(n0 + 92, 700_001); // name_id
    b_npc.write_u32(n0 + 100, 1); // race_id (mask & 128)
    b_npc.write_u32(n0 + 112, 60_003_760); // station_id (mask & 256)
    b_npc.write_u32(n0 + 128, 1 | 2 | 128 | 256);

    let n1 = CfsdBuilder::rec_offset(1, 136);
    b_npc.write_u64(n1, 3_008_417);
    b_npc.buf[n1 + 28] = 1; // locator byte non-zero, but is_agent == false -> is_locator must be false
    b_npc.write_u32(n1 + 68, 6);
    b_npc.write_u32(n1 + 80, 1_000_036);
    b_npc.write_u32(n1 + 92, 700_002);
    b_npc.write_u32(n1 + 128, 0);

    let n2 = CfsdBuilder::rec_offset(2, 136);
    b_npc.write_u64(n2, 3_008_418);
    b_npc.write_u32(n2 + 16, 22);
    b_npc.write_u32(n2 + 20, 1);
    b_npc.write_u32(n2 + 24, 3);
    b_npc.buf[n2 + 28] = 0; // agent with locator == false
    b_npc.write_u32(n2 + 128, 1);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/npccharacters.fsdbinary",
        "npc",
        &b_npc.buf,
        3,
    ));
    let npcs = sde.load_npc_characters().expect("load_npc_characters");
    assert_eq!(
        npcs[&3_008_416],
        NpcCharacterRecord {
            character_id: 3_008_416,
            corporation_id: 1_000_035,
            race_id: Some(1),
            bloodline_id: 5,
            ancestry_id: Some(11),
            station_id: Some(60_003_760),
            name_id: 700_001,
            is_agent: true,
            agent_level: Some(4),
            agent_division_id: Some(24),
            agent_type_id: Some(2),
            is_locator: true,
        }
    );
    assert!(!npcs[&3_008_417].is_agent);
    assert!(!npcs[&3_008_417].is_locator);
    assert_eq!(npcs[&3_008_417].race_id, None);
    assert_eq!(npcs[&3_008_417].ancestry_id, None);
    assert_eq!(npcs[&3_008_417].station_id, None);
    assert_eq!(npcs[&3_008_417].agent_level, None);
    assert!(npcs[&3_008_418].is_agent);
    assert!(!npcs[&3_008_418].is_locator);

    // 3. corporationactivities.fsdbinary (stride 8)
    let mut b_ca = CfsdBuilder::new(&[vec![0u8; 8]]);
    let c0 = CfsdBuilder::rec_offset(0, 8);
    b_ca.write_u32(c0, 10);
    b_ca.write_u32(c0 + 4, 8888);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/corporationactivities.fsdbinary",
        "ca",
        &b_ca.buf,
        1,
    ));
    let cas = sde
        .load_corporation_activities()
        .expect("load_corporation_activities");
    assert_eq!(
        cas[&10],
        CorporationActivityInfo {
            activity_id: 10,
            name_id: 8888
        }
    );

    // 4. npccorporationdivisions.fsdbinary (stride 40)
    let mut b_div = CfsdBuilder::new(&[vec![0u8; 40], vec![0u8; 40]]);
    let d_desc = b_div.alloc_str("Security Desc");
    let d_int = b_div.alloc_str("SECURITY");
    let d0 = CfsdBuilder::rec_offset(0, 40);
    b_div.write_u64(d0, 24);
    b_div.write_u64(d0 + 8, d_desc);
    b_div.write_u64(d0 + 16, d_int);
    b_div.write_u32(d0 + 24, 101);
    b_div.write_u32(d0 + 28, 102);
    b_div.write_u32(d0 + 32, 103);
    b_div.write_u32(d0 + 36, 1 | 2);

    let d1 = CfsdBuilder::rec_offset(1, 40);
    b_div.write_u64(d1, 25);
    b_div.write_u64(d1 + 16, 0); // rel == 0 -> empty string
    b_div.write_u32(d1 + 28, 202);
    b_div.write_u32(d1 + 32, 203);
    b_div.write_u32(d1 + 36, 0);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/npccorporationdivisions.fsdbinary",
        "div",
        &b_div.buf,
        2,
    ));
    let divs = sde
        .load_npc_corporation_divisions()
        .expect("load_npc_corporation_divisions");
    assert_eq!(
        divs[&24],
        NpcCorporationDivisionInfo {
            division_id: 24,
            internal_name: "SECURITY".to_string(),
            description: Some("Security Desc".to_string()),
            name_id: 103,
            description_id: Some(101),
            leader_type_name_id: 102,
        }
    );
    assert_eq!(divs[&25].description, None);
    assert_eq!(divs[&25].description_id, None);
    assert_eq!(divs[&25].internal_name, "");

    // 5. contrabandtypes.fsdbinary (stride 24)
    let mut b_cb = CfsdBuilder::new(&[vec![0u8; 24]]);
    // Append inner dict: 2 buckets (bucket 0 = 0, bucket 1 -> 1 entry of 20 bytes)
    b_cb.buf.extend_from_slice(&2u64.to_le_bytes());
    let dict_abs = b_cb.buf.len();
    let d_rel = (dict_abs - 32) as u64;
    b_cb.buf.extend_from_slice(&0u64.to_le_bytes()); // bucket 0 empty
    b_cb.buf.extend_from_slice(&0u64.to_le_bytes()); // placeholder for bucket 1
    b_cb.buf.extend_from_slice(&1u64.to_le_bytes()); // bn = 1
    let babs = b_cb.buf.len();
    let boff = (babs - 32) as u64;
    b_cb.write_u64(dict_abs + 8, boff);
    b_cb.buf.extend_from_slice(&[0u8; 20]);
    b_cb.write_u32(babs, 500_001);
    b_cb.write_f32(babs + 4, 0.9);
    b_cb.write_f32(babs + 8, 0.5);
    b_cb.write_f32(babs + 12, 2.5);
    b_cb.write_f32(babs + 16, 0.25);

    let cb0 = CfsdBuilder::rec_offset(0, 24);
    b_cb.write_u64(cb0, 3721);
    b_cb.write_u64(cb0 + 8, d_rel);
    b_cb.write_u64(cb0 + 16, 1);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/contrabandtypes.fsdbinary",
        "cb",
        &b_cb.buf,
        1,
    ));
    let cbs = sde.load_contraband_types().expect("load_contraband_types");
    assert_eq!(
        cbs[&3721][&500_001],
        ContrabandFactionRule {
            faction_id: 500_001,
            attack_min_sec: 0.9,
            confiscate_min_sec: 0.5,
            fine_by_value: 2.5,
            standing_loss: 0.25,
        }
    );

    for f in files {
        let _ = std::fs::remove_file(f);
    }
}

#[test]
fn units_accounting_metagroups_graphics_icons_sounds_and_raw_types() {
    let mut sde = SdeRegistry::default();
    let mut files = Vec::new();

    // 6. dogmaunits.fsdbinary (stride 32)
    let mut b_du = CfsdBuilder::new(&[vec![0u8; 32], vec![0u8; 32]]);
    let s_m = b_du.alloc_str("Length");
    let u0 = CfsdBuilder::rec_offset(0, 32);
    b_du.write_u64(u0, 1);
    b_du.write_u64(u0 + 8, s_m);
    b_du.write_u32(u0 + 16, 10);
    b_du.write_u32(u0 + 20, 20);
    b_du.write_u32(u0 + 24, 1 | 2);
    let u1 = CfsdBuilder::rec_offset(1, 32);
    b_du.write_u64(u1, 2);
    b_du.write_u32(u1 + 24, 0);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/dogmaunits.fsdbinary",
        "du",
        &b_du.buf,
        2,
    ));
    let units = sde.load_dogma_units().expect("load_dogma_units");
    assert_eq!(
        units[&1],
        DogmaUnitInfo {
            unit_id: 1,
            name: "Length".to_string(),
            display_name_id: Some(20),
            description_id: Some(10),
        }
    );
    assert_eq!(units[&2].display_name_id, None);
    assert_eq!(units[&2].description_id, None);

    // 7. accountingentrytypes.fsdbinary (stride 40)
    let mut b_ae = CfsdBuilder::new(&[vec![0u8; 40], vec![0u8; 40]]);
    let s_jm = b_ae.alloc_str("Bounty Prize");
    let s_nm = b_ae.alloc_str("bounty_prizes");
    let a0 = CfsdBuilder::rec_offset(0, 40);
    b_ae.write_u64(a0, 85);
    b_ae.write_u64(a0 + 8, s_jm);
    b_ae.write_u64(a0 + 16, s_nm);
    b_ae.write_u32(a0 + 24, 301);
    b_ae.write_u32(a0 + 28, 302);
    b_ae.write_u32(a0 + 32, 303);
    b_ae.write_u32(a0 + 36, 1);
    let a1 = CfsdBuilder::rec_offset(1, 40);
    b_ae.write_u64(a1, 86);
    b_ae.write_u32(a1 + 36, 0);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/accountingentrytypes.fsdbinary",
        "ae",
        &b_ae.buf,
        2,
    ));
    let aets = sde
        .load_accounting_entry_types()
        .expect("load_accounting_entry_types");
    assert_eq!(
        aets[&85],
        AccountingEntryTypeInfo {
            entry_type_id: 85,
            name: "bounty_prizes".to_string(),
            journal_message: "Bounty Prize".to_string(),
            name_id: 303,
            journal_message_id: 302,
            description_id: Some(301),
        }
    );
    assert_eq!(aets[&86].description_id, None);

    // 8. accountingkeys.fsdbinary (stride 48)
    let mut b_ak = CfsdBuilder::new(&[vec![0u8; 48], vec![0u8; 48]]);
    let s_cur = b_ak.alloc_str("ISK");
    let s_kd = b_ak.alloc_str("Master");
    let s_kn = b_ak.alloc_str("cash");
    let s_kt = b_ak.alloc_str("R");
    let k0 = CfsdBuilder::rec_offset(0, 48);
    b_ak.write_u64(k0, 1000);
    b_ak.write_u64(k0 + 8, s_cur);
    b_ak.write_u64(k0 + 16, s_kd);
    b_ak.write_u64(k0 + 24, s_kn);
    b_ak.write_u64(k0 + 32, s_kt);
    b_ak.write_u32(k0 + 40, 401);
    b_ak.write_u32(k0 + 44, 1);
    let k1 = CfsdBuilder::rec_offset(1, 48);
    b_ak.write_u64(k1, 1001);
    b_ak.write_u32(k1 + 44, 0);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/accountingkeys.fsdbinary",
        "ak",
        &b_ak.buf,
        2,
    ));
    let aks = sde.load_accounting_keys().expect("load_accounting_keys");
    assert_eq!(
        aks[&1000],
        AccountingKeyInfo {
            key_id: 1000,
            key_name: "cash".to_string(),
            key_type: "R".to_string(),
            currency: Some("ISK".to_string()),
            description: Some("Master".to_string()),
            name_id: 401,
        }
    );
    assert_eq!(aks[&1001].currency, None);
    assert_eq!(aks[&1001].description, None);

    // 9. metagroups.fsdbinary (stride 48)
    let mut b_mg = CfsdBuilder::new(&[vec![0u8; 48], vec![0u8; 48]]);
    let s_mgd = b_mg.alloc_str("Tech II");
    let m0 = CfsdBuilder::rec_offset(0, 48);
    b_mg.write_u64(m0, 2);
    b_mg.write_u64(m0 + 8, s_mgd);
    b_mg.write_f32(m0 + 16, 0.1);
    b_mg.write_f32(m0 + 20, 0.2);
    b_mg.write_f32(m0 + 24, 0.3);
    b_mg.write_f32(m0 + 28, 1.0);
    b_mg.write_u32(m0 + 32, 501);
    b_mg.write_u32(m0 + 36, 502);
    b_mg.write_u32(m0 + 40, 503);
    b_mg.write_u32(m0 + 44, 1 | 2 | 4 | 8);
    let m1 = CfsdBuilder::rec_offset(1, 48);
    b_mg.write_u64(m1, 1);
    b_mg.write_u32(m1 + 40, 500);
    b_mg.write_u32(m1 + 44, 0);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/metagroups.fsdbinary",
        "mg",
        &b_mg.buf,
        2,
    ));
    let mgs = sde.load_meta_groups().expect("load_meta_groups");
    assert_eq!(
        mgs[&2],
        MetaGroupInfo {
            meta_group_id: 2,
            name_id: 503,
            description_id: Some(501),
            icon_id: Some(502),
            color: Some([0.1, 0.2, 0.3, 1.0]),
            description: Some("Tech II".to_string()),
        }
    );
    assert_eq!(mgs[&1].color, None);
    assert_eq!(mgs[&1].description, None);
    assert_eq!(mgs[&1].description_id, None);
    assert_eq!(mgs[&1].icon_id, None);

    // 10. graphicids.fsdbinary (stride 160), 11. iconids (32), 12. soundids (16)
    let mut b_gr = CfsdBuilder::new(&[vec![0u8; 160]]);
    let s_gf = b_gr.alloc_str("res:/gf");
    let s_if = b_gr.alloc_str("res:/if");
    let s_sf = b_gr.alloc_str("minmatar");
    let s_sh = b_gr.alloc_str("mf1");
    let s_sr = b_gr.alloc_str("minmatar_race");
    let g0 = CfsdBuilder::rec_offset(0, 160);
    b_gr.write_u64(g0, 46);
    b_gr.write_u64(g0 + 40, s_gf);
    b_gr.write_u64(g0 + 48, s_if);
    b_gr.write_u64(g0 + 64, s_sf);
    b_gr.write_u64(g0 + 72, s_sh);
    b_gr.write_u64(g0 + 88, s_sr);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/graphicids.fsdbinary",
        "gr",
        &b_gr.buf,
        1,
    ));
    let grs = sde.load_graphic_ids().expect("load_graphic_ids");
    assert_eq!(
        grs[&46],
        GraphicIdInfo {
            graphic_id: 46,
            graphic_file: Some("res:/gf".to_string()),
            icon_folder: Some("res:/if".to_string()),
            sof_faction_name: Some("minmatar".to_string()),
            sof_hull_name: Some("mf1".to_string()),
            sof_race_name: Some("minmatar_race".to_string()),
        }
    );

    let mut b_ic = CfsdBuilder::new(&[vec![0u8; 32]]);
    let s_icf = b_ic.alloc_str("res:/ui/icon.png");
    let s_icd = b_ic.alloc_str("Icon Desc");
    let ic0 = CfsdBuilder::rec_offset(0, 32);
    b_ic.write_u64(ic0, 22);
    b_ic.write_u64(ic0 + 8, s_icf);
    b_ic.write_u64(ic0 + 16, s_icd);
    b_ic.buf[ic0 + 24] = 1;
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/iconids.fsdbinary",
        "ic",
        &b_ic.buf,
        1,
    ));
    let ics = sde.load_icon_ids().expect("load_icon_ids");
    assert_eq!(
        ics[&22],
        IconIdInfo {
            icon_id: 22,
            icon_file: "res:/ui/icon.png".to_string(),
            description: "Icon Desc".to_string(),
            obsolete: true,
        }
    );

    let mut b_snd = CfsdBuilder::new(&[vec![0u8; 16]]);
    let s_ww = b_snd.alloc_str("ship_rifter_play");
    let snd0 = CfsdBuilder::rec_offset(0, 16);
    b_snd.write_u64(snd0, 20078);
    b_snd.write_u64(snd0 + 8, s_ww);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/soundids.fsdbinary",
        "snd",
        &b_snd.buf,
        1,
    ));
    let snds = sde.load_sound_ids().expect("load_sound_ids");
    assert_eq!(
        snds[&20078],
        SoundIdInfo {
            sound_id: 20078,
            wwise_event: "ship_rifter_play".to_string(),
        }
    );

    // 13. types.fsdbinary (stride 152): 2 records (all mask bits on vs all mask bits off)
    let mut b_ty = CfsdBuilder::new(&[vec![0u8; 152], vec![0u8; 152]]);
    let t0 = CfsdBuilder::rec_offset(0, 152);
    b_ty.write_u64(t0, 587);
    b_ty.write_f64(t0 + 8, 250_000.0); // base_price (bit 0)
    b_ty.write_f64(t0 + 16, 140.0); // capacity (bit 1)
    b_ty.write_f64(t0 + 32, 1_067_000.0); // mass (bit 11)
    b_ty.write_i64(t0 + 40, 2); // portion_size
    b_ty.write_f64(t0 + 48, 35.0); // radius (bit 19)
    b_ty.write_f64(t0 + 56, 27_289.0); // volume (bit 23)
    b_ty.write_i32(t0 + 68, 93841); // description_id (bit 3)
    b_ty.write_i32(t0 + 72, 500_002); // faction_id (bit 5)
    b_ty.write_i32(t0 + 80, 46); // graphic_id (bit 6)
    b_ty.write_i32(t0 + 84, 25); // group_id
    b_ty.write_i32(t0 + 88, 21); // icon_id (bit 7)
    b_ty.write_i32(t0 + 96, 64); // market_group_id (bit 10)
    b_ty.write_i32(t0 + 100, 1); // meta_group_id (bit 12)
    b_ty.write_i32(t0 + 104, 0); // meta_level (bit 13)
    b_ty.write_i32(t0 + 116, 2); // race_id (bit 18)
    b_ty.write_i32(t0 + 120, 20078); // sound_id (bit 20)
    b_ty.write_i32(t0 + 124, 1); // tech_level (bit 21)
    b_ty.write_u32(t0 + 132, 88888); // type_name_id
    b_ty.write_i32(t0 + 136, 586); // variation_parent_type_id (bit 22)
    b_ty.write_i32(t0 + 140, 26_553); // wreck_type_id (bit 24)
    b_ty.buf[t0 + 145] = 1; // published
    let all_type_bits = (1 << 0)
        | (1 << 1)
        | (1 << 3)
        | (1 << 5)
        | (1 << 6)
        | (1 << 7)
        | (1 << 10)
        | (1 << 11)
        | (1 << 12)
        | (1 << 13)
        | (1 << 18)
        | (1 << 19)
        | (1 << 20)
        | (1 << 21)
        | (1 << 22)
        | (1 << 23)
        | (1 << 24);
    b_ty.write_u32(t0 + 148, all_type_bits);

    let t1 = CfsdBuilder::rec_offset(1, 152);
    b_ty.write_u64(t1, 588);
    b_ty.write_i64(t1 + 40, 0); // clamped to 1
    b_ty.write_i32(t1 + 84, 25);
    b_ty.write_u32(t1 + 148, 0);
    files.push(register_cfsd_resource(
        &mut sde,
        "res:/staticdata/types.fsdbinary",
        "types",
        &b_ty.buf,
        2,
    ));
    let (raw_len, raw_types) = sde
        .load_raw_types_fsdbinary()
        .expect("load_raw_types_fsdbinary");
    assert_eq!(raw_len, b_ty.buf.len());
    assert_eq!(
        raw_types[&587],
        RawCfsdTypeRecord {
            type_id: 587,
            group_id: 25,
            base_price: 250_000.0,
            capacity: 140.0,
            mass: 1_067_000.0,
            portion_size: 2,
            radius: 35.0,
            volume: 27_289.0,
            published: true,
            description_id: Some(93841),
            faction_id: Some(500_002),
            graphic_id: Some(46),
            icon_id: Some(21),
            market_group_id: Some(64),
            meta_group_id: Some(1),
            meta_level: Some(0),
            race_id: Some(2),
            sound_id: Some(20078),
            tech_level: Some(1),
            variation_parent_type_id: Some(586),
            wreck_type_id: Some(26_553),
            type_name_id: 88888,
        }
    );
    assert_eq!(raw_types[&588].portion_size, 1);
    assert_eq!(raw_types[&588].base_price, 0.0);
    assert_eq!(raw_types[&588].capacity, 0.0);
    assert_eq!(raw_types[&588].mass, 0.0);
    assert_eq!(raw_types[&588].radius, 0.0);
    assert_eq!(raw_types[&588].volume, 0.0);
    assert!(!raw_types[&588].published);
    assert_eq!(raw_types[&588].description_id, None);
    assert_eq!(raw_types[&588].wreck_type_id, None);

    for f in files {
        let _ = std::fs::remove_file(f);
    }
}
