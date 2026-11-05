//! Unit and mutation-killing tests for `static_domain_loaders`.

use super::*;

fn build_fsd_dict_raw(entries: &[(u32, Vec<u8>)], item_size: usize) -> Vec<u8> {
    let mut values_buf = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for &(k, ref val) in entries {
        offsets.push((k, values_buf.len() as u32));
        values_buf.extend_from_slice(val);
    }
    let mut footer = Vec::new();
    footer.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (k, off) in offsets {
        footer.extend_from_slice(&k.to_le_bytes());
        footer.extend_from_slice(&off.to_le_bytes());
        footer.resize(footer.len() + (item_size - 8), 0);
    }
    let footer_size = footer.len() as u32;
    let body_len = (values_buf.len() + footer.len() + 4) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&values_buf);
    out.extend_from_slice(&footer);
    out.extend_from_slice(&footer_size.to_le_bytes());
    out
}

fn with_zero_schema(dict_bytes: Vec<u8>) -> Vec<u8> {
    let mut out = 0u32.to_le_bytes().to_vec();
    out.extend_from_slice(&dict_bytes);
    out
}

fn fsd_string_bytes(s: &str) -> Vec<u8> {
    let mut out = (s.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(s.as_bytes());
    out
}

fn register_temp_res(sde: &mut SdeRegistry, res_path: &str, name: &str, bytes: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("evomu_sdl_{}_{}", std::process::id(), name));
    std::fs::write(&path, bytes).unwrap();
    sde.resources.insert(
        res_path.to_string(),
        SdeResourceInfo {
            res_path: res_path.to_string(),
            cdn_relative_path: path.display().to_string(),
            md5: String::new(),
            byte_size: bytes.len() as u64,
            format: String::new(),
            record_count: 1,
            spot: String::new(),
        },
    );
    path
}

#[test]
fn sqlite_static_industry_infobubbles_and_fighter_abilities_loaders() {
    let mut sde = SdeRegistry::default();
    let mut temp_files = Vec::new();

    let write_sqlite = |sde: &mut SdeRegistry, res: &str, tag: &str, rows: &[(&str, &str)]| -> PathBuf {
        let p = std::env::temp_dir().join(format!("evomu_sdl_sq_{}_{}.sqlite", std::process::id(), tag));
        let _ = std::fs::remove_file(&p);
        let conn = Connection::open(&p).unwrap();
        conn.execute("CREATE TABLE cache (key TEXT, value TEXT)", []).unwrap();
        for &(k, v) in rows {
            conn.execute("INSERT INTO cache VALUES (?1, ?2)", [k, v]).unwrap();
        }
        sde.resources.insert(
            res.to_string(),
            SdeResourceInfo {
                res_path: res.to_string(),
                cdn_relative_path: p.display().to_string(),
                md5: String::new(),
                byte_size: 1024,
                format: String::new(),
                record_count: rows.len(),
                spot: String::new(),
            },
        );
        p
    };

    temp_files.push(write_sqlite(
        &mut sde,
        "res:/staticdata/industry_activities.static",
        "act",
        &[(
            "1",
            r#"{"activityID":1,"activityName":"Manufacturing","description":"Build"}"#,
        )],
    ));
    temp_files.push(write_sqlite(
        &mut sde,
        "res:/staticdata/industry_activity_modifier_sources.static",
        "modsrc",
        &[(
            "3000",
            r#"{"manufacturing":{"material":[{"dogmaAttributeID":100,"filterID":5}]}}"#,
        )],
    ));
    temp_files.push(write_sqlite(
        &mut sde,
        "res:/staticdata/industry_activity_target_filters.static",
        "flt",
        &[("5", r#"{"name":"Ships","categoryIDs":[6],"groupIDs":[25]}"#)],
    ));
    temp_files.push(write_sqlite(
        &mut sde,
        "res:/staticdata/industry_assembly_lines.static",
        "asm",
        &[(
            "10",
            r#"{"id":10,"name":"Line","activity":1,"base_time_multiplier":0.8,"base_material_multiplier":0.9,"base_cost_multiplier":1.0}"#,
        )],
    ));
    temp_files.push(write_sqlite(
        &mut sde,
        "res:/staticdata/industry_installation_types.static",
        "inst",
        &[("200", r#"{"assembly_lines":[{"assemblyLine":10},{"assemblyLine":11}]}"#)],
    ));
    temp_files.push(write_sqlite(
        &mut sde,
        "res:/staticdata/infobubbles.static",
        "info",
        &[("shipTraits", r#"{"587":{"bonuses":[]}}"#)],
    ));
    temp_files.push(write_sqlite(
        &mut sde,
        "res:/staticdata/fighterabilities.static",
        "fa",
        &[(
            "7",
            r#"{"targetMode":"pointTargeted","disallowInHighSec":true,"disallowInLowSec":false,"iconID":11,"displayNameID":22,"tooltipTextID":33,"turretGraphicID":44}"#,
        )],
    ));

    let acts = sde.load_industry_activities().unwrap();
    assert_eq!(acts[&1].activity_name, "Manufacturing");
    let srcs = sde.load_industry_activity_modifier_sources().unwrap();
    assert_eq!(srcs[&3000]["manufacturing"]["material"][0].dogma_attribute_id, 100);
    let flts = sde.load_industry_activity_target_filters().unwrap();
    assert_eq!(flts[&5].category_ids, vec![6]);
    let lines = sde.load_industry_assembly_lines().unwrap();
    assert!((lines[&10].base_time_multiplier - 0.8).abs() < 1e-9);
    let insts = sde.load_industry_installation_types().unwrap();
    assert_eq!(insts[&200], vec![10, 11]);
    let bubbles = sde.load_info_bubbles().unwrap();
    assert!(bubbles.contains_key("shipTraits"));
    let (fsize, fas) = sde.load_raw_fighter_abilities_static().unwrap();
    assert!(fsize > 0);
    assert!(fas[&7].disallow_in_high_sec);
    assert!(!fas[&7].disallow_in_low_sec);
    assert_eq!(fas[&7].ability_name_id, Some(22));

    for p in temp_files {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn asteroid_pickle_opcodes_and_error_branches() {
    let mut sde = SdeRegistry::default();
    // Valid pickle testing 0x80, '}', '(', 'K', 'M', 'J', 'q', 'r', 0x85, 0x86, 0x87, 't', 'u', '.'
    let mut valid = Vec::new();
    valid.extend_from_slice(&[0x80, 2, b'}', b'q', 0, b'r', 1, 0, 0, 0, b'(']);
    // entry 1: key K(10), val TUPLE1(K(30))
    valid.extend_from_slice(&[b'K', 10, b'K', 30, 0x85]);
    // entry 2: key M(300), val TUPLE2(K(20), K(10))
    valid.extend_from_slice(&[b'M', 44, 1, b'K', 20, b'K', 10, 0x86]);
    // entry 3: key J(30000142), val TUPLE3(K(9), K(8), K(7))
    valid.push(b'J');
    valid.extend_from_slice(&30000142u32.to_le_bytes());
    valid.extend_from_slice(&[b'K', 9, b'K', 8, b'K', 7, 0x87]);
    // entry 4: key K(50), val '(' K(4) K(3) 't'
    valid.extend_from_slice(&[b'K', 50, b'(', b'K', 4, b'K', 3, b't']);
    valid.extend_from_slice(&[b'u', b'.']);

    let p = register_temp_res(
        &mut sde,
        "res:/staticdata/asteroidtypesbysolarsystemid.pickle",
        "ast.pickle",
        &valid,
    );
    let parsed = sde.load_asteroid_types_by_solar_system().unwrap();
    assert_eq!(parsed[&10], vec![30]);
    assert_eq!(parsed[&300], vec![10, 20]);
    assert_eq!(parsed[&30000142], vec![7, 8, 9]);
    assert_eq!(parsed[&50], vec![3, 4]);

    // Error branches: TUPLE1 on empty, TUPLE2 on 1 int, TUPLE3 on 2 ints, TUPLE with Dict, SETITEMS bad key/val, unknown opcode
    for bad in [
        vec![0x85],
        vec![b'K', 1, 0x86],
        vec![b'K', 1, b'K', 2, 0x87],
        vec![b'(', b'}', b't'],
        vec![b'(', b'K', 1, 0x85, b'u'],
        vec![b'(', b'K', 1, b'K', 2, b'u'],
        vec![0xff],
    ] {
        std::fs::write(&p, &bad).unwrap();
        assert!(sde.load_asteroid_types_by_solar_system().is_err());
    }
    let _ = std::fs::remove_file(p);
}

#[test]
fn fsd_binary_domain_loaders_all_tables() {
    let mut sde = SdeRegistry::default();
    let mut files = Vec::new();

    // 1. wrecks.static: outer item_size 8; value has 12-byte header (pos+8 = 0) followed by inner dict
    let inner_wreck = build_fsd_dict_raw(&[(1, 26511u32.to_le_bytes().to_vec())], 8);
    let mut wreck_val = vec![0u8; 12];
    wreck_val.extend_from_slice(&inner_wreck);
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/wrecks.static",
        "wrecks",
        &with_zero_schema(build_fsd_dict_raw(&[(25, wreck_val)], 8)),
    ));
    assert_eq!(sde.load_wrecks_by_group_and_race().unwrap()[&25][&1], 26511);

    // 2. asteroidcountbytypeidbeltid.static
    let inner_belt = build_fsd_dict_raw(&[(1230, 15u32.to_le_bytes().to_vec())], 8);
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/asteroidcountbytypeidbeltid.static",
        "belts",
        &with_zero_schema(build_fsd_dict_raw(&[(40001, inner_belt)], 12)),
    ));
    assert_eq!(sde.load_asteroid_counts_by_belt().unwrap()[&40001][&1230], 15);

    // 3. bracketsbycategory / group / type & brackets.static
    for (res, tag) in [
        ("res:/staticdata/bracketsbycategory.static", "bcat"),
        ("res:/staticdata/bracketsbygroup.static", "bgrp"),
        ("res:/staticdata/bracketsbytype.static", "btyp"),
    ] {
        files.push(register_temp_res(
            &mut sde,
            res,
            tag,
            &with_zero_schema(build_fsd_dict_raw(&[(6, 99u32.to_le_bytes().to_vec())], 12)),
        ));
    }
    assert_eq!(sde.load_brackets_by_category().unwrap()[&6], 99);
    assert_eq!(sde.load_brackets_by_group().unwrap()[&6], 99);
    assert_eq!(sde.load_brackets_by_type().unwrap()[&6], 99);

    // brackets.static entry with all 6 attrs present (mask = 15)
    let mut b_data = Vec::new();
    let mut b_offs = Vec::new();
    for chunk in [
        fsd_string_bytes("ShipBracket"),
        fsd_string_bytes("res:/tex.png"),
        fsd_string_bytes("res:/empty.png"),
        10.0f32.to_le_bytes().to_vec(),
        1000.0f32.to_le_bytes().to_vec(),
        (-5i32).to_le_bytes().to_vec(),
    ] {
        b_offs.push(b_data.len() as u32);
        b_data.extend_from_slice(&chunk);
    }
    let mut b_val = 15u64.to_le_bytes().to_vec();
    for o in b_offs {
        b_val.extend_from_slice(&o.to_le_bytes());
    }
    b_val.extend_from_slice(&b_data);
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/brackets.static",
        "brackets",
        &with_zero_schema(build_fsd_dict_raw(&[(1, b_val)], 12)),
    ));
    let br = sde.load_brackets().unwrap();
    assert_eq!(br[&1].name, "ShipBracket");
    assert_eq!(br[&1].icon_offset, Some(-5));

    // 4. landmarks, empireselection, empiretechnologies, eventmessagetypes, eventtypes, podguide, podguidegroups, selectiongroups, triggertypes, dusticons, skilltags, dialogs, secondary_suns
    let mut lm = Vec::new();
    lm.extend_from_slice(&10u32.to_le_bytes());
    lm.extend_from_slice(&20u32.to_le_bytes());
    for f in [1.0f64, 2.0, 3.0] {
        lm.extend_from_slice(&f.to_le_bytes());
    }
    lm.push(7u8);
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/landmarks.static",
        "lm",
        &with_zero_schema(build_fsd_dict_raw(&[(5, lm)], 8)),
    ));
    assert_eq!(sde.load_landmarks().unwrap()[&5].landmark_type, 7);

    let mut et = Vec::new();
    et.extend_from_slice(&100u32.to_le_bytes());
    et.extend_from_slice(&2u32.to_le_bytes());
    et.push(1u8);
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/empiretechnologies.static",
        "et",
        &with_zero_schema(build_fsd_dict_raw(&[(1, et)], 8)),
    ));
    assert!(sde.load_empire_technologies().unwrap()[&1].is_displayed);

    let mut emt = vec![0u8; 12];
    emt.extend_from_slice(&fsd_string_bytes("EvtMsg"));
    for (res, tag) in [
        ("res:/staticdata/eventmessagetypes.static", "emt"),
        ("res:/staticdata/eventtypes.static", "evtt"),
        ("res:/staticdata/dusticons.static", "dust"),
    ] {
        let isz = if tag == "dust" { 12 } else { 8 };
        files.push(register_temp_res(
            &mut sde,
            res,
            tag,
            &with_zero_schema(build_fsd_dict_raw(&[(1, emt.clone())], isz)),
        ));
    }
    assert_eq!(sde.load_event_message_types().unwrap()[&1], "EvtMsg");
    assert_eq!(sde.load_event_types().unwrap()[&1], "EvtMsg");
    assert_eq!(sde.load_dust_icons().unwrap()[&1], "EvtMsg");

    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/skilltags.static",
        "skt",
        &with_zero_schema(build_fsd_dict_raw(&[(3, 42u32.to_le_bytes().to_vec())], 8)),
    ));
    assert_eq!(sde.load_skill_tags().unwrap()[&3], 42);

    let mut pg = vec![0u8; 37];
    pg[24] = 2;
    pg.extend_from_slice(&fsd_string_bytes("res:/pg.png"));
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/podguide.static",
        "pg",
        &with_zero_schema(build_fsd_dict_raw(&[(1, pg)], 8)),
    ));
    assert_eq!(sde.load_pod_guide().unwrap()[&1].image_size, 2);

    let mut pgg = vec![0u8; 13];
    pgg[8] = 3;
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/podguidegroups.static",
        "pgg",
        &with_zero_schema(build_fsd_dict_raw(&[(1, pgg)], 8)),
    ));
    assert_eq!(sde.load_pod_guide_groups().unwrap()[&1].group_type, 3);

    let mut sg = vec![0u8; 28];
    sg.extend_from_slice(&fsd_string_bytes("Hangar"));
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/selectiongroups.static",
        "sg",
        &with_zero_schema(build_fsd_dict_raw(&[(1, sg)], 8)),
    ));
    assert_eq!(sde.load_selection_groups().unwrap()[&1].selection_group_name, "Hangar");

    let mut tt = vec![1u8; 14];
    tt.extend_from_slice(&fsd_string_bytes("Attacked"));
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/triggertypes.static",
        "tt",
        &with_zero_schema(build_fsd_dict_raw(&[(1, tt)], 8)),
    ));
    assert!(sde.load_trigger_types().unwrap()[&1].allow_on_guard_object);

    // Secondary suns
    let mut ss_val = vec![0u8; 98];
    ss_val.extend_from_slice(&4u64.to_le_bytes()); // mask = 4 -> n_present = 3, sec_idx = 1
    ss_val.extend_from_slice(&0u32.to_le_bytes());
    ss_val.extend_from_slice(&0u32.to_le_bytes()); // sec_idx = 1 offset = 0
    ss_val.extend_from_slice(&0u32.to_le_bytes());
    ss_val.extend_from_slice(&9001u32.to_le_bytes()); // item_id
    ss_val.extend_from_slice(&45041u32.to_le_bytes()); // type_id
    ss_val.extend_from_slice(&30844u32.to_le_bytes()); // effect_beacon_type_id
    for f in [10.0f64, 20.0, 30.0] {
        ss_val.extend_from_slice(&f.to_le_bytes());
    }
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/solarsystemcontent.static",
        "ssc",
        &with_zero_schema(build_fsd_dict_raw(&[(31000001, ss_val)], 12)),
    ));
    let suns = sde.load_secondary_suns().unwrap();
    assert_eq!(suns[&31000001].effect_beacon_type_id, 30844);

    for p in files {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn certificates_achievements_empire_selection_and_dialogs_loaders() {
    let mut sde = SdeRegistry::default();
    let mut files = Vec::new();

    // 1. certificates.static
    let sk_levels: Vec<u8> = [1u32, 2, 3, 4, 5]
        .into_iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let st_dict = build_fsd_dict_raw(&[(3300, sk_levels)], 8);
    let mut rec_list = 1u32.to_le_bytes().to_vec();
    rec_list.extend_from_slice(&587u32.to_le_bytes());
    let mut cert_val = Vec::new();
    cert_val.extend_from_slice(&100u32.to_le_bytes()); // name_id
    cert_val.extend_from_slice(&200u32.to_le_bytes()); // description_id
    cert_val.extend_from_slice(&255u32.to_le_bytes()); // group_id
    cert_val.extend_from_slice(&1u64.to_le_bytes()); // mask = 1 (has_recommended)
    cert_val.extend_from_slice(&0u32.to_le_bytes()); // st_pos rel offset = 0
    cert_val.extend_from_slice(&(st_dict.len() as u32).to_le_bytes()); // rec_pos rel offset
    cert_val.extend_from_slice(&st_dict);
    cert_val.extend_from_slice(&rec_list);
    let c_dict = build_fsd_dict_raw(&[(10, cert_val)], 12);

    let mut lvl_vec = 1u32.to_le_bytes().to_vec();
    lvl_vec.extend_from_slice(&10u32.to_le_bytes());
    let ship_lvl_dict = build_fsd_dict_raw(&[(0, lvl_vec)], 8);
    let m_dict = build_fsd_dict_raw(&[(587, ship_lvl_dict)], 8);

    let mut cert_file = vec![0u8; 4 + 16]; // sch_len=0, body=4, body+8=c_off(0), body+12=m_off
    let m_off = c_dict.len() as u32;
    cert_file[16..20].copy_from_slice(&m_off.to_le_bytes());
    cert_file.extend_from_slice(&c_dict);
    cert_file.extend_from_slice(&m_dict);
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/certificates.static",
        "certs",
        &cert_file,
    ));
    let cb = sde.load_certificates_and_masteries().unwrap();
    assert_eq!(cb.certificates[&10].recommended_for, vec![587]);
    assert_eq!(cb.masteries_by_ship_type[&587], vec![vec![10]]);

    // 2. achievements.static (bitfield = 0x0f: name_id, desc_id, notif_id, simpleConditions)
    let mut cond_val = vec![0u8; 20];
    cond_val[0..4].copy_from_slice(&5u32.to_le_bytes());
    cond_val[16..20].copy_from_slice(&99u32.to_le_bytes());
    let sc_dict = build_fsd_dict_raw(&[(1, cond_val)], 8);
    let mut ach_val = Vec::new();
    ach_val.extend_from_slice(&77u32.to_le_bytes()); // achievement_id
    ach_val.push(1); // is_client
    ach_val.push(1); // is_enabled
    ach_val.extend_from_slice(&0x0fu32.to_le_bytes()); // bitfield
    ach_val.extend_from_slice(&0u32.to_le_bytes()); // pad to 14
    for off in [0u32, 4, 8, 12] {
        ach_val.extend_from_slice(&off.to_le_bytes());
    }
    for id in [1001u32, 1002, 1003] {
        ach_val.extend_from_slice(&id.to_le_bytes());
    }
    ach_val.extend_from_slice(&sc_dict);
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/achievements.static",
        "ach",
        &with_zero_schema(build_fsd_dict_raw(&[(77, ach_val)], 8)),
    ));
    let achs = sde.load_achievements().unwrap();
    assert_eq!(achs[&77].target_value, 5);
    assert_eq!(achs[&77].conditions[0].statistic_event_id, 99);

    // 3. dialogs.schema & dialogs.static
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/dialogs.schema",
        "dlg.sch",
        b"schema",
    ));
    let mut dlg_raw = vec![0u8; 4]; // 4-byte header before vpos=4
    dlg_raw.extend_from_slice(&42u32.to_le_bytes()); // dialog_id at vpos=4
    dlg_raw.push(3u8); // dialog_type at vpos+4
    let footer_pos = dlg_raw.len();
    let mut dlg_footer = Vec::new();
    dlg_footer.extend_from_slice(&1u32.to_le_bytes()); // cnt = 1
    dlg_footer.extend_from_slice(&8u32.to_le_bytes()); // item rel offset = 8 (right after cnt + 1 offset)
    dlg_footer.extend_from_slice(&0u32.to_le_bytes()); // val_off = 0 -> vpos = 4
    dlg_footer.extend_from_slice(&[0u8; 12]); // pad to item + 16
    dlg_footer.extend_from_slice(&fsd_string_bytes("WelcomeDlg"));
    let f_sz = dlg_footer.len() as u32;
    dlg_raw.extend_from_slice(&dlg_footer);
    dlg_raw.extend_from_slice(&f_sz.to_le_bytes());
    let _ = footer_pos;
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/dialogs.static",
        "dlg.st",
        &dlg_raw,
    ));
    let dlgs = sde.load_dialogs().unwrap();
    assert_eq!(dlgs["WelcomeDlg"].dialog_id, 42);
    assert_eq!(dlgs["WelcomeDlg"].dialog_type, 3);

    // 4. empireselection.static (5-byte footer items with race_id mapping 1->4, 2->1, 3->8, 4->2, 9->9)
    let mut emp_val = vec![0u8; 32];
    emp_val[0..4].copy_from_slice(&777u32.to_le_bytes()); // quote_id
    let s1 = fsd_string_bytes("logo.png");
    let s2 = fsd_string_bytes("named.png");
    let s3 = fsd_string_bytes("seal.png");
    let off1 = 0u32;
    let off2 = s1.len() as u32;
    let off3 = (s1.len() + s2.len()) as u32;
    emp_val[20..24].copy_from_slice(&off1.to_le_bytes());
    emp_val[24..28].copy_from_slice(&off2.to_le_bytes());
    emp_val[28..32].copy_from_slice(&off3.to_le_bytes());
    emp_val.extend_from_slice(&s1);
    emp_val.extend_from_slice(&s2);
    emp_val.extend_from_slice(&s3);

    let mut emp_footer = 5u32.to_le_bytes().to_vec();
    for code in [1u8, 2, 3, 4, 9] {
        emp_footer.push(code);
        emp_footer.extend_from_slice(&0u32.to_le_bytes()); // offset 0 -> all point to emp_val
    }
    let emp_fsz = emp_footer.len() as u32;
    let emp_body_len = (emp_val.len() + emp_footer.len() + 4) as u32;
    let mut emp_raw = 0u32.to_le_bytes().to_vec(); // sch_len = 0
    emp_raw.extend_from_slice(&emp_body_len.to_le_bytes());
    emp_raw.extend_from_slice(&emp_val);
    emp_raw.extend_from_slice(&emp_footer);
    emp_raw.extend_from_slice(&emp_fsz.to_le_bytes());
    files.push(register_temp_res(
        &mut sde,
        "res:/staticdata/empireselection.static",
        "empsel",
        &emp_raw,
    ));
    let emps = sde.load_empire_selection().unwrap();
    for rid in [1u32, 2, 4, 8, 9] {
        assert_eq!(emps[&rid].quote_id, 777);
        assert_eq!(emps[&rid].logo_path, "logo.png");
        assert_eq!(emps[&rid].named_logo_path, "named.png");
        assert_eq!(emps[&rid].seal_path, "seal.png");
    }

    for p in files {
        let _ = std::fs::remove_file(p);
    }
}

