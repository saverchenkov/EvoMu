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
