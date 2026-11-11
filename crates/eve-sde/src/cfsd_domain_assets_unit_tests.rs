//! Semantic unit tests split from `cfsd_domain_loaders_unit_tests`.

use super::*;

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
    b_ty.write_i32(t0 + 92, 8); // isis_group_id (bit 9)
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
        | (1 << 9)
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
            isis_group_id: Some(8),
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
    assert_eq!(raw_types[&588].isis_group_id, None);
    assert_eq!(raw_types[&588].wreck_type_id, None);

    for f in files {
        let _ = std::fs::remove_file(f);
    }
}
