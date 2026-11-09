//! Independent raw client-file verification of all 9 `res:/localizationfsd/localization_fsd_*.pickle`
//! V24.01 resources (`main`, `en-us`, `de`, `es`, `fr`, `ja`, `ko`, `ru`, `zh`).

#![forbid(unsafe_code)]

mod raw_client;

use eve_sde::SdeRegistry;
use raw_client::{md5_hex, resfiles_dir, unpickle, PyVal};
use std::collections::HashSet;

fn verify_language_pickle(
    sde: &SdeRegistry,
    lang: &str,
    rel_path: &str,
    expected_size: usize,
    expected_md5: &str,
    expected_records: usize,
) {
    let raw_path = resfiles_dir().join(rel_path);
    let raw_bytes = std::fs::read(&raw_path)
        .unwrap_or_else(|e| panic!("failed to read raw client file {raw_path:?}: {e}"));
    assert_eq!(
        raw_bytes.len(),
        expected_size,
        "localization_fsd_{lang}.pickle byte size must match V24.01 ledger"
    );
    assert_eq!(
        md5_hex(&raw_bytes),
        expected_md5,
        "localization_fsd_{lang}.pickle MD5 must match V24.01 ledger"
    );

    let top = unpickle(&raw_bytes);
    let PyVal::Tuple(top_tuple) = top else {
        panic!("localization_fsd_{lang}.pickle top-level object must be a 2-tuple");
    };
    assert_eq!(top_tuple.first().and_then(PyVal::as_str), Some(lang));
    let Some(PyVal::Dict(entries_rc)) = top_tuple.get(1) else {
        panic!("localization_fsd_{lang}.pickle second tuple element must be message dict");
    };
    let entries = entries_rc.borrow();
    assert_eq!(
        entries.len(),
        expected_records,
        "raw client localization_fsd_{lang}.pickle must contain exact expected record count"
    );

    let loaded = sde
        .load_all_localized_messages(lang)
        .unwrap_or_else(|e| panic!("load_all_localized_messages({lang}) failed: {e}"));
    assert_eq!(
        loaded.len(),
        entries.len(),
        "SdeRegistry({lang}) must expose all {} records without dropping any",
        entries.len()
    );

    for (k, v) in entries.iter() {
        let msg_id = k.as_int().expect("integer messageID") as u32;
        let PyVal::Tuple(parts) = v else {
            panic!("message entry {msg_id} in {lang} must be a tuple");
        };
        let raw_text = parts
            .first()
            .and_then(PyVal::as_str)
            .expect("tuple[0] must be message text");
        let sde_text = loaded
            .get(&msg_id)
            .unwrap_or_else(|| panic!("SdeRegistry({lang}) missing messageID {msg_id}"));
        assert_eq!(
            sde_text, raw_text,
            "localized text mismatch in {lang} for messageID {msg_id}"
        );
    }

    assert!(
        sde.localized_type_name(lang, 587).is_some(),
        "Rifter (587) must resolve localized name in {lang}"
    );
    assert!(
        sde.localized_group_name(lang, 25).is_some(),
        "Frigate group (25) must resolve localized name in {lang}"
    );
    assert!(
        sde.localized_category_name(lang, 6).is_some(),
        "Ship category (6) must resolve localized name in {lang}"
    );
    assert!(
        sde.localized_market_group_name(lang, 4).is_some(),
        "Frigates market group (4) must resolve localized name in {lang}"
    );
}

// parity: sde:res:/localizationfsd/localization_fsd_main.pickle, sde:res:/localizationfsd/localization_fsd_en-us.pickle
#[test]
fn test_sde_localization_fsd_main_and_en_us_full_verification() {
    eve_testkit::attest(&[
        "sde:res:/localizationfsd/localization_fsd_main.pickle",
        "sde:res:/localizationfsd/localization_fsd_en-us.pickle",
    ]);

    let sde = SdeRegistry::open(&SdeRegistry::default_db_path())
        .expect("SdeRegistry::open must succeed");

    // 1. Verify localization_fsd_main.pickle against raw client file (3,344,947 bytes, 31,872 labels)
    let main_path = resfiles_dir().join("79/79a473897ae5f3b0_ee8726670ad947c75d7048f30acc9b35");
    let main_bytes = std::fs::read(&main_path).expect("read localization_fsd_main.pickle");
    assert_eq!(main_bytes.len(), 3_344_947);
    assert_eq!(md5_hex(&main_bytes), "ee8726670ad947c75d7048f30acc9b35");

    let raw_main = unpickle(&main_bytes);
    let PyVal::Dict(main_dict_rc) = raw_main else {
        panic!("main pickle root must be dict");
    };
    let dict_find = |entries: &[(PyVal, PyVal)], key: &str| -> Option<PyVal> {
        entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v.clone())
    };
    let main_dict = main_dict_rc.borrow();
    let Some(PyVal::List(langs_rc)) = dict_find(&main_dict, "languages") else {
        panic!("main pickle must have 'languages' list");
    };
    let raw_langs: Vec<String> = langs_rc
        .borrow()
        .iter()
        .map(|v| v.as_str().expect("lang str").to_string())
        .collect();
    assert_eq!(
        raw_langs,
        vec!["ru", "fr", "de", "zh", "en-us", "ko", "it", "ja", "es"]
    );
    let Some(PyVal::Dict(labels_rc)) = dict_find(&main_dict, "labels") else {
        panic!("main pickle must have 'labels' dict");
    };
    let raw_labels = labels_rc.borrow();
    assert_eq!(raw_labels.len(), 31_872);

    let bundle = sde.load_localization_main().expect("load_localization_main");
    assert_eq!(bundle.languages, raw_langs);
    assert_eq!(bundle.labels.len(), 31_872);
    for (k, v) in raw_labels.iter() {
        let mid = k.as_int().expect("int messageID") as u32;
        let PyVal::Dict(f_rc) = v else {
            panic!("label {mid} must be dict");
        };
        let f = f_rc.borrow();
        let fp = dict_find(&f, "FullPath").as_ref().and_then(PyVal::as_str).unwrap().to_string();
        let lbl = dict_find(&f, "label").as_ref().and_then(PyVal::as_str).unwrap().to_string();
        let f_mid = dict_find(&f, "messageID").as_ref().and_then(PyVal::as_int).unwrap() as u32;
        let got = bundle.labels.get(&mid).unwrap_or_else(|| panic!("missing label {mid}"));
        assert_eq!(got.full_path, fp);
        assert_eq!(got.label, lbl);
        assert_eq!(got.message_id, f_mid);
    }

    // 2. Verify localization_fsd_en-us.pickle (40,753,078 bytes, 308,657 records)
    verify_language_pickle(
        &sde,
        "en-us",
        "2c/2c3038b3c38e91a1_7a753c9da2eeca4dcc7cf0c6655aaae0",
        40_753_078,
        "7a753c9da2eeca4dcc7cf0c6655aaae0",
        308_657,
    );

    let spot_ids: HashSet<u32> = [1, 524_288].into_iter().collect();
    let subset = sde.load_localized_messages_en_us(&spot_ids).expect("subset loader");
    assert_eq!(subset.get(&1).map(String::as_str), Some("Passive"));
    assert_eq!(
        sde.localized_message_en_us(524_288).as_deref(),
        Some("Tormentor Ironblood Serenity Only SKIN (30 Days)")
    );
    assert!(sde.load_achievements().expect("load_achievements").values().any(|a| !a.name.is_empty()));
    assert!(!sde.load_market_group_descriptions().expect("mg_descs").is_empty());
}

// parity: sde:res:/localizationfsd/localization_fsd_de.pickle, sde:res:/localizationfsd/localization_fsd_es.pickle, sde:res:/localizationfsd/localization_fsd_fr.pickle
#[test]
fn test_sde_localization_fsd_west_eu_locales_full_verification() {
    eve_testkit::attest(&[
        "sde:res:/localizationfsd/localization_fsd_de.pickle",
        "sde:res:/localizationfsd/localization_fsd_es.pickle",
        "sde:res:/localizationfsd/localization_fsd_fr.pickle",
    ]);

    let sde = SdeRegistry::open(&SdeRegistry::default_db_path())
        .expect("SdeRegistry::open must succeed");

    verify_language_pickle(
        &sde,
        "de",
        "f7/f781e479bccfd00e_9bfa3342a109a5a30b7523af734daee5",
        44_737_417,
        "9bfa3342a109a5a30b7523af734daee5",
        305_231,
    );
    assert_eq!(sde.localized_message("de", 1).as_deref(), Some("Passiv"));

    verify_language_pickle(
        &sde,
        "es",
        "68/68766cf9c7eb3001_07b96ab4740f9b89347549f432a09228",
        44_059_431,
        "07b96ab4740f9b89347549f432a09228",
        305_229,
    );

    verify_language_pickle(
        &sde,
        "fr",
        "4a/4a0f5fe70b667169_eac42d7449d6083400e4da35ee998a03",
        46_429_727,
        "eac42d7449d6083400e4da35ee998a03",
        305_234,
    );
}

// parity: sde:res:/localizationfsd/localization_fsd_ja.pickle, sde:res:/localizationfsd/localization_fsd_ko.pickle, sde:res:/localizationfsd/localization_fsd_ru.pickle, sde:res:/localizationfsd/localization_fsd_zh.pickle
#[test]
fn test_sde_localization_fsd_cjk_ru_locales_full_verification() {
    eve_testkit::attest(&[
        "sde:res:/localizationfsd/localization_fsd_ja.pickle",
        "sde:res:/localizationfsd/localization_fsd_ko.pickle",
        "sde:res:/localizationfsd/localization_fsd_ru.pickle",
        "sde:res:/localizationfsd/localization_fsd_zh.pickle",
    ]);

    let sde = SdeRegistry::open(&SdeRegistry::default_db_path())
        .expect("SdeRegistry::open must succeed");

    verify_language_pickle(
        &sde,
        "ja",
        "82/82afc4ce93a0dc14_f401a04fc2313a2090e836d726245bec",
        86_414_823,
        "f401a04fc2313a2090e836d726245bec",
        305_234,
    );
    verify_language_pickle(
        &sde,
        "ko",
        "f5/f5467fdb71de5d33_dc47cdaba73e5d3d58c2ba748fabe71a",
        71_125_744,
        "dc47cdaba73e5d3d58c2ba748fabe71a",
        305_231,
    );
    verify_language_pickle(
        &sde,
        "ru",
        "4c/4c56e191aaa0e618_baede863986025489fe8970f0ab38ee5",
        153_500_626,
        "baede863986025489fe8970f0ab38ee5",
        305_234,
    );
    verify_language_pickle(
        &sde,
        "zh",
        "87/87bfc3b6eab5e5d3_468af73100512587c6eab7ea5a1528ca",
        61_334_886,
        "468af73100512587c6eab7ea5a1528ca",
        305_235,
    );
}
