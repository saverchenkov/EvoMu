//! Unit and mutation-killing tests for `static_localization_loaders`.

use super::*;

#[test]
fn normalize_lang_and_decode_pickle_raw_unicode_edge_cases() {
    assert_eq!(normalize_localization_fsd_lang("en"), Some("en-us"));
    assert_eq!(normalize_localization_fsd_lang("EN-US"), Some("en-us"));
    assert_eq!(normalize_localization_fsd_lang("en-GB,en;q=0.9"), Some("en-us"));
    assert_eq!(normalize_localization_fsd_lang("de-DE;q=0.8"), Some("de"));
    assert_eq!(normalize_localization_fsd_lang("es"), Some("es"));
    assert_eq!(normalize_localization_fsd_lang("fr-FR"), Some("fr"));
    assert_eq!(normalize_localization_fsd_lang("it"), Some("it"));
    assert_eq!(normalize_localization_fsd_lang("ja"), Some("ja"));
    assert_eq!(normalize_localization_fsd_lang("ko"), Some("ko"));
    assert_eq!(normalize_localization_fsd_lang("ru"), Some("ru"));
    assert_eq!(normalize_localization_fsd_lang("zh-CN"), Some("zh"));
    assert_eq!(normalize_localization_fsd_lang("pl"), None);
    assert_eq!(normalize_localization_fsd_lang(""), None);

    // decode_pickle_raw_unicode:
    // 1. Plain ASCII + \uXXXX + \UXXXXXXXX
    assert_eq!(
        decode_pickle_raw_unicode(b"Caf\\u00e9 \\U0001f680"),
        "Café 🚀"
    );
    // 2. Even backslashes (\\u0041) are literal backslashes, not unicode escape
    assert_eq!(decode_pickle_raw_unicode(b"\\\\u0041"), "\\\\u0041");
    // 3. Odd backslashes (\\\u0041) emit (bs_count - 1) backslashes + decoded char
    assert_eq!(decode_pickle_raw_unicode(b"\\\\\\u0041"), "\\\\A");
    // 4. Invalid hex after \u falls back to literal backslashes
    assert_eq!(decode_pickle_raw_unicode(b"\\uZZZZ"), "\\uZZZZ");
}

#[test]
fn parse_localization_lang_and_main_pickle_bytes() {
    let raw_lang = b"(dp1\nI100\n(VHello\\u0021\nsI200\n(VWorld\nsI300\n(VExtra\ns.";
    let all = parse_localization_fsd_lang_bytes(raw_lang, None);
    assert_eq!(all.len(), 3);
    assert_eq!(all[&100], "Hello!");
    assert_eq!(all[&200], "World");
    assert_eq!(all[&300], "Extra");

    let subset_ids = HashSet::from([200u32]);
    let sub = parse_localization_fsd_lang_bytes(raw_lang, Some(&subset_ids));
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[&200], "World");

    // Main pickle protocol-0 parser covering (dp, (lp, S'...', S"...", I..., p..., g..., a, s, .
    let raw_main = b"(dp0\r\n\
S'languages'\r\n\
(lp1\r\n\
S'en-us'\r\n\
p2\r\n\
aS\"de\"\r\n\
asS'labels'\r\n\
(dp3\r\n\
I42\r\n\
(d\r\n\
S'FullPath'\r\n\
S'UI/Common'\r\n\
sS'label'\r\n\
g2\r\n\
sS'messageID'\r\n\
I42\r\n\
sss.";
    let bundle = parse_localization_fsd_main_bytes(raw_main).unwrap();
    assert_eq!(bundle.languages, vec!["en-us".to_string(), "de".to_string()]);
    assert_eq!(
        bundle.labels.get(&42),
        Some(&LocalizationLabelEntry {
            message_id: 42,
            full_path: "UI/Common".to_string(),
            label: "en-us".to_string(),
        })
    );

    // Error paths on malformed main pickle bytes
    assert!(parse_localization_fsd_main_bytes(b"Ibad\n.").is_err());
    assert!(parse_localization_fsd_main_bytes(b"S'unterminated\n.").is_err());
    assert!(parse_localization_fsd_main_bytes(b"gbad\n.").is_err());
    assert!(parse_localization_fsd_main_bytes(b"g60\n.").is_err());
    assert!(parse_localization_fsd_main_bytes(b"(l\n.").is_err());
}

#[test]
fn sde_registry_localization_loaders_and_fallbacks() {
    let mut sde = SdeRegistry::default();
    let main_path = std::env::temp_dir().join(format!("evomu_loc_main_{}.pickle", std::process::id()));
    let en_path = std::env::temp_dir().join(format!("evomu_loc_en_{}.pickle", std::process::id()));
    std::fs::write(
        &main_path,
        b"(dp0\nS'languages'\n(l\nS'en-us'\nasS'labels'\n(d\ns.",
    )
    .unwrap();
    std::fs::write(&en_path, b"(dp1\nI500\n(VRifter\ns.").unwrap();

    sde.resources.insert(
        "res:/localizationfsd/localization_fsd_main.pickle".to_string(),
        SdeResourceInfo {
            res_path: "res:/localizationfsd/localization_fsd_main.pickle".to_string(),
            cdn_relative_path: main_path.display().to_string(),
            md5: String::new(),
            byte_size: 32,
            format: String::new(),
            record_count: 1,
            spot: String::new(),
        },
    );
    sde.resources.insert(
        "res:/localizationfsd/localization_fsd_en-us.pickle".to_string(),
        SdeResourceInfo {
            res_path: "res:/localizationfsd/localization_fsd_en-us.pickle".to_string(),
            cdn_relative_path: en_path.display().to_string(),
            md5: String::new(),
            byte_size: 32,
            format: String::new(),
            record_count: 1,
            spot: String::new(),
        },
    );

    let main_bundle = sde.load_localization_main().unwrap();
    assert_eq!(main_bundle.languages, vec!["en-us"]);

    assert!(sde.load_all_localized_messages("invalid-lang").is_err());
    assert!(sde.load_localized_messages("invalid-lang", &HashSet::new()).is_err());
    assert!(sde
        .load_localized_messages_en_us(&HashSet::new())
        .unwrap()
        .is_empty());

    let en_all = sde.load_all_localized_messages_en_us().unwrap();
    assert_eq!(en_all.get(&500).map(String::as_str), Some("Rifter"));

    let en_sub = sde
        .load_localized_messages_en_us(&HashSet::from([500u32]))
        .unwrap();
    assert_eq!(en_sub.get(&500).map(String::as_str), Some("Rifter"));

    // Test localized_message cache + localized_type_description fallback via self.types[tid].description_id
    let it_path = std::env::temp_dir().join(format!("evomu_loc_it_{}.pickle", std::process::id()));
    std::fs::write(&it_path, b"(dp1\nI500\n(VDescrizione\ns.").unwrap();
    sde.resources.insert(
        "res:/localizationfsd/localization_fsd_it.pickle".to_string(),
        SdeResourceInfo {
            res_path: "res:/localizationfsd/localization_fsd_it.pickle".to_string(),
            cdn_relative_path: it_path.display().to_string(),
            md5: String::new(),
            byte_size: 32,
            format: String::new(),
            record_count: 1,
            spot: String::new(),
        },
    );
    sde.types.insert(
        587,
        InvType {
            type_id: 587,
            group_id: 25,
            category_id: 6,
            name: "Rifter".to_string(),
            mass: 1.0,
            volume: 1.0,
            capacity: 1.0,
            radius: 1.0,
            base_price: 1.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: Some(500),
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::new(),
        },
    );
    assert_eq!(sde.localized_message("invalid-lang", 500), None);
    assert_eq!(
        sde.localized_message("it", 500).as_deref(),
        Some("Descrizione")
    );
    assert_eq!(
        sde.localized_type_description("it", 587).as_deref(),
        Some("Descrizione")
    );
    assert_eq!(sde.localized_type_name("it", 999999), None);
    assert_eq!(sde.localized_group_name("it", 999999), None);
    assert_eq!(sde.localized_category_name("it", 999999), None);
    assert_eq!(sde.localized_market_group_name("it", 999999), None);
    assert_eq!(sde.localized_market_group_description("it", 999999), None);

    let _ = std::fs::remove_file(main_path);
    let _ = std::fs::remove_file(en_path);
    let _ = std::fs::remove_file(it_path);
}

#[test]
fn pickle_malformed_escapes_memo_limits_and_market_group_description_filters() {
    // decode_pickle_raw_unicode: replacement char on invalid codepoint, truncated escape, trailing backslash
    assert_eq!(decode_pickle_raw_unicode(b"\\Uffffffff"), "\u{fffd}");
    assert_eq!(decode_pickle_raw_unicode(b"\\u12"), "\\u12");
    assert_eq!(decode_pickle_raw_unicode(b"abc\\"), "abc\\");
    assert_eq!(decode_pickle_raw_unicode(b"abc\\\\"), "abc\\\\");

    // parse_localization_fsd_lang_bytes: sI prefix, invalid utf8 id, non-numeric id, non-(V line, filter early stop
    let raw_lang = b"sI10\n(VTen\nIbad\n(VSkip\nI\xff\n(VBadUtf8\nsI20\nVNotParenV\nsI30\n(VThirty\nsI40\n(VForty\n";
    let all = parse_localization_fsd_lang_bytes(raw_lang, None);
    assert_eq!(all.len(), 3);
    assert_eq!(all[&10], "Ten");
    assert_eq!(all[&30], "Thirty");
    assert_eq!(all[&40], "Forty");
    let sub = parse_localization_fsd_lang_bytes(raw_lang, Some(&HashSet::from([10u32, 30])));
    assert_eq!(sub.len(), 2);

    // parse_localization_fsd_main_bytes: empty -> err, memo > 64 ignored, unknown keys/types skipped
    assert!(parse_localization_fsd_main_bytes(b"").is_err());
    let main_edge = b"(dp0\n\
I999\nS'ignored_non_str_key'\ns\
S'languages'\n\
(lp65\n\
I1\naS'fr'\na\
sS'labels'\n\
(dp10\n\
S'not_an_int'\n(d\ns\
I7\nS'not_a_dict'\ns\
I8\n(dp66\nS'unknown'\nS'x'\nsS'FullPath'\nS'A/B'\nsS'label'\nS'lbl'\ns\
ss.";
    let bundle = parse_localization_fsd_main_bytes(main_edge).unwrap();
    assert_eq!(bundle.languages, vec!["fr".to_string()]);
    assert_eq!(bundle.labels[&8].message_id, 8);
    assert_eq!(bundle.labels[&8].full_path, "A/B");
    assert_eq!(bundle.labels[&8].label, "lbl");

    // Unindexed resource -> InvalidSchema; indexed missing file -> MissingDatabase
    let mut sde = SdeRegistry::default();
    assert!(matches!(
        sde.load_all_localized_messages("en"),
        Err(SdeError::InvalidSchema(_))
    ));
    assert!(matches!(
        sde.load_localized_messages("de", &HashSet::from([1u32])),
        Err(SdeError::InvalidSchema(_))
    ));
    sde.resources.insert(
        "res:/localizationfsd/localization_fsd_de.pickle".to_string(),
        SdeResourceInfo {
            res_path: "res:/localizationfsd/localization_fsd_de.pickle".to_string(),
            cdn_relative_path: "/nonexistent/evomu_missing_de.pickle".to_string(),
            md5: String::new(),
            byte_size: 0,
            format: String::new(),
            record_count: 0,
            spot: String::new(),
        },
    );
    assert!(matches!(
        sde.load_localized_messages("de", &HashSet::from([1u32])),
        Err(SdeError::MissingDatabase(_))
    ));

    // load_market_group_descriptions + localized_type_name from CFSD blob
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("evomu_loc_edge_{pid}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Build minimal CFSD blob using the 72-byte CFSD header expected by `cfsd_records`:
    // header: [40..48] record_count, [48..56] bucket_count=2, [56..64] bucket0=0, [64..72] bucket1=boff,
    // followed at 72..80 by [u64 count] and then the fixed-stride records at offset 80.
    let build_cfsd = |recs: &[&[u8]]| -> Vec<u8> {
        let mut buf = vec![0u8; 72];
        buf[40..48].copy_from_slice(&(recs.len() as u64).to_le_bytes());
        buf[48..56].copy_from_slice(&2u64.to_le_bytes());
        buf[56..64].copy_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&(recs.len() as u64).to_le_bytes());
        let boff = (buf.len() - 32) as u64;
        buf[64..72].copy_from_slice(&boff.to_le_bytes());
        for r in recs {
            buf.extend_from_slice(r);
        }
        buf
    };

    // marketgroups.fsdbinary (stride 28): mg1 (mask=1, desc=101 -> "Good Desc"), mg2 (mask=1, desc=102 -> "" filtered!), mg3 (mask=0 -> skipped)
    let mut mg1 = [0u8; 28];
    mg1[0..4].copy_from_slice(&1u32.to_le_bytes());
    mg1[4..8].copy_from_slice(&101u32.to_le_bytes());
    mg1[12..16].copy_from_slice(&103u32.to_le_bytes());
    mg1[24..28].copy_from_slice(&1u32.to_le_bytes());

    let mut mg2 = [0u8; 28];
    mg2[0..4].copy_from_slice(&2u32.to_le_bytes());
    mg2[4..8].copy_from_slice(&102u32.to_le_bytes());
    mg2[24..28].copy_from_slice(&1u32.to_le_bytes());

    let mut mg3 = [0u8; 28];
    mg3[0..4].copy_from_slice(&3u32.to_le_bytes());
    mg3[4..8].copy_from_slice(&101u32.to_le_bytes());
    mg3[24..28].copy_from_slice(&0u32.to_le_bytes());

    // types.fsdbinary (stride 152): type 587 with description_id at offset 68 (mask bit 1<<3 = 8 at offset 148) and type_name_id at offset 132
    let mut t1 = [0u8; 152];
    t1[0..8].copy_from_slice(&587u64.to_le_bytes());
    t1[68..72].copy_from_slice(&101u32.to_le_bytes()); // description_id
    t1[132..136].copy_from_slice(&103u32.to_le_bytes()); // type_name_id
    t1[148..152].copy_from_slice(&8u32.to_le_bytes()); // has description_id bit (1 << 3)

    let mg_path = dir.join("mg.bin");
    let ty_path = dir.join("types.bin");
    let en_path = dir.join("en.pickle");
    std::fs::write(&mg_path, build_cfsd(&[&mg1, &mg2, &mg3])).unwrap();
    std::fs::write(&ty_path, build_cfsd(&[&t1])).unwrap();
    std::fs::write(&en_path, b"(dp0\nI101\n(VGood Desc\nsI102\n(V\nsI103\n(VRifter\ns.").unwrap();

    for (res, path) in [
        ("res:/staticdata/marketgroups.fsdbinary", &mg_path),
        ("res:/staticdata/types.fsdbinary", &ty_path),
        ("res:/localizationfsd/localization_fsd_en-us.pickle", &en_path),
    ] {
        sde.resources.insert(
            res.to_string(),
            SdeResourceInfo {
                res_path: res.to_string(),
                cdn_relative_path: path.display().to_string(),
                md5: String::new(),
                byte_size: 128,
                format: String::new(),
                record_count: 1,
                spot: String::new(),
            },
        );
    }

    let descs = sde.load_market_group_descriptions().unwrap();
    assert_eq!(descs.get(&1).map(String::as_str), Some("Good Desc"));
    assert!(!descs.contains_key(&2));
    assert!(!descs.contains_key(&3));

    assert_eq!(sde.localized_type_name("en", 587).as_deref(), Some("Rifter"));
    assert_eq!(
        sde.localized_type_description("en", 587).as_deref(),
        Some("Good Desc")
    );

    let _ = std::fs::remove_dir_all(&dir);
}
