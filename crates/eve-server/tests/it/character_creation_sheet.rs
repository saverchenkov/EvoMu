//! Integration tests for Character Creation (`CH-3`), Character Sheet & Notes (`CH-4`), and Jump Clones (`S-4`).


use crate::common;

use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, SkillQueueEntry, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::{clone_jump_cooldown_seconds, sp_for_skill_level};
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_ch3_character_creation_and_starter_assets() {
    // parity: feat:character.list-characters, feat:character.select-character, feat:character.create-character, feat:character.starter-skills, feat:character.school-corp, feat:character.random-name, feat:character.name-validation
    eve_testkit::attest(&["feat:character.list-characters", "feat:character.select-character", "feat:character.create-character", "feat:character.starter-skills", "feat:character.school-corp", "feat:character.random-name", "feat:character.name-validation"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(933);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // 1. Random name generation per bloodline (feat:character.random-name)
    let rand_name = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetValidRandomName",
            &[PyRep::Integer(1)], // Deteis bloodline
            &kw,
            None,
        )
        .await
        .expect("GetValidRandomName Deteis");
    let PyRep::String(ref generated_name) = rand_name else {
        panic!("Expected string name from GetValidRandomName");
    };
    assert!((3..=37).contains(&generated_name.len()));
    assert!(
        generated_name.contains(' '),
        "Generated character name should include first and last name from bloodline pool"
    );

    let amarr_rand_name = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetValidRandomName",
            &[PyRep::Integer(5)], // Amarr bloodline
            &kw,
            None,
        )
        .await
        .expect("GetValidRandomName Amarr");
    let PyRep::String(ref amarr_name) = amarr_rand_name else {
        panic!("Expected string name from GetValidRandomName Amarr");
    };
    assert_ne!(generated_name, amarr_name, "Different bloodlines must draw from distinct name pools");

    // 2. Name validation: character set, banned words, length, duplicate (feat:character.name-validation)
    let valid_check = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "ValidateNameEx",
            &[PyRep::String("Valid Capsuleer-Name".into())],
            &kw,
            None,
        )
        .await
        .expect("ValidateNameEx valid");
    assert_eq!(valid_check, PyRep::Bool(true));

    let short_check = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "ValidateNameEx",
            &[PyRep::String("AB".into())],
            &kw,
            None,
        )
        .await
        .expect("ValidateNameEx short");
    assert_eq!(short_check, PyRep::Bool(false));

    // Invalid punctuation / character set check
    let bad_chars_check = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "ValidateNameEx",
            &[PyRep::String("Bad_Name!@#".into())],
            &kw,
            None,
        )
        .await
        .expect("ValidateNameEx invalid chars");
    assert_eq!(bad_chars_check, PyRep::Bool(false));

    // Banned / reserved word check
    let banned_word_check = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "ValidateNameEx",
            &[PyRep::String("CCP Dev Tester".into())],
            &kw,
            None,
        )
        .await
        .expect("ValidateNameEx banned word");
    assert_eq!(banned_word_check, PyRep::Bool(false));

    // Duplicate check
    let dup_check = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "ValidateNameEx",
            &[PyRep::String("Aura Capsuleer".into())],
            &kw,
            None,
        )
        .await
        .expect("ValidateNameEx duplicate");
    assert_eq!(dup_check, PyRep::Bool(false));

    // Reject invalid doll info (None, empty dict, non-structured scalar placeholder) (feat:character.create-character)
    let invalid_doll_err1 = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Invalid Doll Pilot".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(11),
                PyRep::None,
                PyRep::None,
                PyRep::Integer(17),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_doll_err1.unwrap_err().code, "InvalidDollInfo");

    let invalid_doll_err2 = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Invalid Doll Pilot".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(11),
                PyRep::String("doll_info_placeholder".into()),
                PyRep::String("portrait_info_placeholder".into()),
                PyRep::Integer(17),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_doll_err2.unwrap_err().code, "InvalidDollInfo");

    let invalid_doll_err3 = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Invalid Doll Pilot".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(11),
                PyRep::Dict(vec![]),
                PyRep::Dict(vec![]),
                PyRep::Integer(17),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_doll_err3.unwrap_err().code, "InvalidDollInfo");

    // 3. CreateCharacterWithDoll with V24 7-argument layout and structured doll descriptor (feat:character.create-character)
    // (charactername, bloodlineID, genderID, ancestryID, charInfo, portraitInfo, schoolID)
    // Deteis bloodline=1 (Caldari race=1), gender=1, Scientists ancestry=11, STI school=17
    let valid_doll = PyRep::keyval([
        ("costume", PyRep::Integer(301)),
        ("gender", PyRep::Integer(1)),
        ("hair", PyRep::Integer(101)),
        ("skin", PyRep::Integer(201)),
    ]);
    let valid_portrait = PyRep::keyval([
        ("background", PyRep::Integer(42)),
        ("camera", PyRep::Integer(1)),
        ("pose", PyRep::Integer(7)),
    ]);
    let new_char_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Caldari Vanguard".into()),
                PyRep::Integer(1),  // bloodlineID = 1 (Deteis -> Caldari race 1)
                PyRep::Integer(1),  // genderID = 1 (Male)
                PyRep::Integer(11), // ancestryID = 11 (Scientists)
                valid_doll,
                valid_portrait,
                PyRep::Integer(17), // schoolID = 17 (Science and Trade Institute)
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateCharacterWithDoll V24 7-arg");
    let new_char_id = new_char_rep.as_i64().expect("character ID i64");

    let mut home_st = 0;
    let mut ship_id = 0;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get(&new_char_id).expect("character record");
        assert_eq!(ch.race_id, 1);
        assert_eq!(ch.bloodline_id, 1);
        assert_eq!(ch.gender, 1);
        assert_eq!(ch.ancestry_id, 11);
        assert_eq!(ch.school_id, 17);
        // 5,000 ISK starting wallet
        assert_eq!(ch.balance, IskCents::from_isk(5_000));
        assert_eq!(ch.balance.to_wire_f64(), 5_000.0);

        // Doll & portrait metadata persisted
        let (doll_dna, portrait_meta) = snap
            .character_doll_info
            .get(&new_char_id)
            .expect("character_doll_info persisted");
        assert_eq!(doll_dna, "costume=301&gender=1&hair=101&skin=201");
        assert_eq!(portrait_meta, "background=42&camera=1&pose=7");

        // Starter skills populated
        let skills = &snap.skills[&new_char_id];
        assert!(skills.len() > 10, "Starter skills populated from bloodline/race");
        for (&skill_id, rec) in skills {
            assert!(rec.level > 0);
            assert!(rec.skill_points > 0.0);
            let rank = ctx
                .sde
                .types
                .get(&skill_id)
                .and_then(|t| t.default_attributes.get(&275).copied())
                .unwrap_or(1.0);
            let expected_sp = sp_for_skill_level(rank, rec.level);
            assert!((rec.skill_points - expected_sp).abs() < 1e-3);
        }

        // Starting rookie ship (corvette type 601 for Caldari) in home station hangar
        home_st = ch.station_id.expect("home station");
        let corvette = snap.items.values().find(|i| {
            i.owner_id == new_char_id
                && i.location_id == home_st
                && i.flag == InventoryFlag::Hangar
                && i.type_id == 601
        });
        assert!(corvette.is_some(), "Starting rookie corvette must exist in hangar");
        let ship = corvette.unwrap();
        assert!(ship.singleton);
        assert_eq!(ship.quantity, 1);
        ship_id = ship.item_id;

        snap.skill_queues.insert(
            new_char_id,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 2,
                start_sp: 250.0,
                end_sp: 1415.0,
                duration_seconds: 1200.0,
                started_at_unix: None,
            }],
        );
    });

    // Select character and set custom ship name via invbroker.SetLabel (feat:character.list-characters)
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(new_char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "SetLabel",
        &[PyRep::Integer(ship_id), PyRep::String("Vanguard Ibis".into())],
        &kw,
        None,
    )
    .await
    .expect("SetLabel for active ship");

    // 4. List characters & character selection summary (feat:character.list-characters)
    let char_list = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharactersToSelect",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharactersToSelect");
    assert!(char_list.is_crowset());
    let select_rows = char_list
        .crowset_rows()
        .expect("Expected CRowset from GetCharactersToSelect");
    let created_row = select_rows
        .iter()
        .find_map(|r| match r {
            PyRep::DBRow { columns, .. } => {
                if columns.iter().any(|(k, v)| k == "characterID" && v == &PyRep::Integer(new_char_id)) {
                    Some(columns)
                } else {
                    None
                }
            }
            _ => None,
        })
        .expect("Created character in GetCharactersToSelect");
    assert_eq!(
        created_row
            .iter()
            .find(|(k, _)| k == "characterName")
            .map(|(_, v)| v),
        Some(&PyRep::String("Caldari Vanguard".into())),
        "GetCharactersToSelect must return exact characterName"
    );
    assert_eq!(
        created_row
            .iter()
            .find(|(k, _)| k == "stationID")
            .map(|(_, v)| v),
        Some(&PyRep::Integer(home_st)),
        "GetCharactersToSelect must return exact stationID"
    );
    assert_eq!(
        created_row
            .iter()
            .find(|(k, _)| k == "balance")
            .map(|(_, v)| v),
        Some(&PyRep::Real(5_000.0)),
        "GetCharactersToSelect must return exact wallet balance"
    );
    assert_eq!(
        created_row
            .iter()
            .find(|(k, _)| k == "shipTypeID")
            .map(|(_, v)| v),
        Some(&PyRep::Integer(601)),
        "GetCharactersToSelect must return exact shipTypeID (Ibis)"
    );
    assert_eq!(
        created_row
            .iter()
            .find(|(k, _)| k == "shipName")
            .map(|(_, v)| v),
        Some(&PyRep::String("Vanguard Ibis".into())),
        "GetCharactersToSelect must return custom ship name set via SetLabel"
    );

    let select_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[PyRep::Integer(new_char_id)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterInfo");
    let PyRep::KeyVal(sel_info_kv) = select_info else {
        panic!("Expected KeyVal from GetCharacterInfo");
    };
    assert_eq!(
        sel_info_kv.get("shipName"),
        Some(&PyRep::String("Vanguard Ibis".into()))
    );
    assert_eq!(
        sel_info_kv.get("paperdollState"),
        Some(&PyRep::String("costume=301&gender=1&hair=101&skin=201".into()))
    );
    assert_eq!(
        sel_info_kv.get("portraitInfo"),
        Some(&PyRep::String("background=42&camera=1&pose=7".into()))
    );
    assert_eq!(
        sel_info_kv.get("trainingSkillTypeID"),
        Some(&PyRep::Integer(3300))
    );
    assert_eq!(
        sel_info_kv.get("trainingToLevel"),
        Some(&PyRep::Integer(2))
    );

    // 5. Select character (feat:character.select-character)
    let select_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(new_char_id)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID");
    let PyRep::KeyVal(select_kv) = select_rep else {
        panic!("Expected KeyVal from SelectCharacterID");
    };
    assert_eq!(
        select_kv.get("characterID").unwrap().as_i64(),
        Some(new_char_id)
    );
    assert_eq!(sess.character_id, Some(new_char_id));
}

#[tokio::test]
async fn test_ch4_character_sheet_biography_and_notes() {
    // parity: feat:character.show-info-public, feat:character.biography, feat:character.employment-history, feat:character.owner-notes, feat:character.character-sheet
    eve_testkit::attest(&["feat:character.show-info-public", "feat:character.biography", "feat:character.employment-history", "feat:character.owner-notes", "feat:character.character-sheet"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(934);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Show info public (feat:character.show-info-public)
    let pub_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetPublicInfo3",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("GetPublicInfo3");
    let PyRep::KeyVal(pub_kv) = pub_info else {
        panic!("Expected KeyVal from GetPublicInfo3");
    };
    assert_eq!(
        pub_kv.get("characterID").unwrap().as_i64(),
        Some(char_id)
    );
    assert_eq!(
        pub_kv.get("characterName"),
        Some(&PyRep::String("Aura Capsuleer".into()))
    );
    assert_eq!(
        pub_kv.get("corporationID").and_then(|v| v.as_i64()),
        Some(1_000_170)
    );
    assert_eq!(
        pub_kv.get("allianceID"),
        Some(&PyRep::None)
    );
    assert_eq!(
        pub_kv.get("raceID").and_then(|v| v.as_i64()),
        Some(2)
    );
    assert_eq!(
        pub_kv.get("bloodlineID").and_then(|v| v.as_i64()),
        Some(3)
    );
    assert_eq!(
        pub_kv.get("ancestryID").and_then(|v| v.as_i64()),
        Some(19)
    );
    assert_eq!(
        pub_kv.get("schoolID").and_then(|v| v.as_i64()),
        Some(14)
    );
    assert_eq!(
        pub_kv.get("securityStatus").and_then(|v| v.as_f64()),
        Some(0.5)
    );

    // 2. Biography (feat:character.biography)
    let test_bio = "Veteran Capsuleer operating in Lonetrek and The Citadel.";
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "SetCharacterDescription",
        &[PyRep::String(test_bio.into())],
        &kw,
        None,
    )
    .await
    .expect("SetCharacterDescription");

    let read_bio = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetCharacterDescription",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterDescription");
    assert_eq!(read_bio, PyRep::String(test_bio.into()));

    // 3. Employment history (feat:character.employment-history)
    let emp_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "corporationSvc",
            None,
            "GetEmployementRecordAndCharacterTransfers",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("GetEmployementRecordAndCharacterTransfers");
    let PyRep::Rowset {
        columns: emp_cols,
        rows: emp_rows,
    } = emp_rep
    else {
        panic!("Expected Rowset from GetEmployementRecordAndCharacterTransfers");
    };
    assert_eq!(emp_cols, vec!["recordID", "corporationID", "startDate"]);
    assert!(!emp_rows.is_empty());
    assert_eq!(
        emp_rows[0].get(1).and_then(|v| v.as_i64()),
        Some(1_000_170),
        "Employment history row must match pilot corporation"
    );

    // 4. Owner notes (feat:character.owner-notes)
    let add_note_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "AddOwnerNote",
            &[
                PyRep::Integer(char_id),
                PyRep::String("Travel Log".into()),
                PyRep::String("Bookmarked safe spots in Jita".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("AddOwnerNote");
    let note_id = add_note_rep.as_i64().expect("note_id i64");

    let get_notes_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetOwnerNote",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("GetOwnerNote");
    let PyRep::Rowset {
        rows: note_rows, ..
    } = get_notes_rep
    else {
        panic!("Expected Rowset from GetOwnerNote");
    };
    assert_eq!(note_rows.len(), 1);
    assert_eq!(note_rows[0][0], PyRep::Integer(note_id));
    assert_eq!(note_rows[0][2], PyRep::String("Travel Log".into()));

    let edit_note = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "EditOwnerNote",
            &[
                PyRep::Integer(note_id),
                PyRep::String("Travel Log".into()),
                PyRep::String("Updated bookmarks".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("EditOwnerNote");
    assert_eq!(edit_note, PyRep::Bool(true));

    let labels = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetOwnerNoteLabels",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetOwnerNoteLabels");
    assert_eq!(labels, PyRep::List(vec![PyRep::String("Travel Log".into())]));

    let remove_note = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RemoveOwnerNote",
            &[PyRep::Integer(note_id)],
            &kw,
            None,
        )
        .await
        .expect("RemoveOwnerNote");
    assert_eq!(remove_note, PyRep::Bool(true));

    // 5. Character sheet & Private Info & Neural Remap (feat:character.character-sheet)
    let priv_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetPrivateInfo",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("GetPrivateInfo");
    let PyRep::KeyVal(priv_kv) = priv_info else {
        panic!("Expected KeyVal from GetPrivateInfo");
    };
    assert_eq!(priv_kv.get("characterID").unwrap().as_i64(), Some(char_id));
    assert!(priv_kv.contains_key("balance"));
    assert!(priv_kv.contains_key("skillPoints"));
    assert!(priv_kv.contains_key("freeSkillPoints"));
    assert!(priv_kv.contains_key("cloneStationID"));

    let char_sheet = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "GetCharacterInfo",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterInfo");
    let PyRep::KeyVal(sheet_kv) = char_sheet else {
        panic!("Expected KeyVal from GetCharacterInfo");
    };
    assert_eq!(sheet_kv.get("characterID").unwrap().as_i64(), Some(char_id));
    assert_eq!(
        sheet_kv.get("characterName"),
        Some(&PyRep::String("Aura Capsuleer".into()))
    );
    assert_eq!(
        sheet_kv.get("corporationID").and_then(|v| v.as_i64()),
        Some(1_000_170)
    );
    assert_eq!(
        sheet_kv.get("raceID").and_then(|v| v.as_i64()),
        Some(2)
    );
    assert_eq!(
        sheet_kv.get("bloodlineID").and_then(|v| v.as_i64()),
        Some(3)
    );
    assert_eq!(
        sheet_kv.get("ancestryID").and_then(|v| v.as_i64()),
        Some(19)
    );
    assert_eq!(
        sheet_kv.get("securityStatus").and_then(|v| v.as_f64()),
        Some(0.5)
    );
    assert_eq!(
        sheet_kv.get("balance").and_then(|v| v.as_f64()),
        Some(1_000_000_000.0)
    );
    let PyRep::KeyVal(attr_kv) = sheet_kv.get("attributes").expect("attributes KeyVal") else {
        panic!("Expected KeyVal for attributes");
    };
    assert_eq!(attr_kv.get("intelligence").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(attr_kv.get("memory").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(attr_kv.get("perception").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(attr_kv.get("willpower").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(attr_kv.get("charisma").and_then(|v| v.as_i64()), Some(19));

    // Neural remap: 99 total, 17..=27 per attribute
    // Valid: 27 Perception, 21 Willpower, 17 Charisma, 17 Intelligence, 17 Memory = 99
    let remap_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[
                PyRep::Integer(17), // Charisma
                PyRep::Integer(17), // Intelligence
                PyRep::Integer(17), // Memory
                PyRep::Integer(27), // Perception
                PyRep::Integer(21), // Willpower
            ],
            &kw,
            None,
        )
        .await
        .expect("RespecCharacter valid");
    let PyRep::KeyVal(remap_kv) = remap_ok else {
        panic!("Expected KeyVal from RespecCharacter");
    };
    assert_eq!(remap_kv.get("perception").unwrap().as_i64(), Some(27));
    assert_eq!(remap_kv.get("willpower").unwrap().as_i64(), Some(21));

    // Remap failure: sum != 99
    let bad_sum = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[
                PyRep::Integer(20),
                PyRep::Integer(20),
                PyRep::Integer(20),
                PyRep::Integer(20),
                PyRep::Integer(20), // sum = 100
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_sum.unwrap_err().code, "RemapFailed");
}

#[tokio::test]
async fn test_s4_jump_clones_implants_and_cooldown() {
    // parity: plan:S-4, feat:clones.jump-clone-install, feat:clones.jump-clone-jump, feat:clones.jump-clone-destroy
    eve_testkit::attest(&["plan:S-4", "feat:clones.jump-clone-install", "feat:clones.jump-clone-jump", "feat:clones.jump-clone-destroy"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(937);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    let char_id = 90_000_001;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Verify jump clone install fee: 900,000 ISK
    let price_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "GetPriceForClone",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetPriceForClone");
    assert_eq!(price_rep, PyRep::Real(900_000.0));

    // Untrained Infomorph Psychology (24242, 0 slots) rejected on InstallCloneInStation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
        }
        if let Some(skills) = snap.skills.get_mut(&char_id) {
            skills.remove(&24242);
            skills.remove(&33407);
            skills.remove(&73910);
            skills.remove(&55025);
        }
    });
    let untrained_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        untrained_err.unwrap_err().code,
        "InfomorphPsychologyRequired",
        "Installing a jump clone without Infomorph Psychology (24242) must fail"
    );

    // Station without clone bay rejected
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_000_361); // Station without clone bay
        }
    });
    let no_bay_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(no_bay_err.unwrap_err().code, "StationHasNoCloneBay");

    // Station with clone bay (Jita 4-4, 60003760, owner 1000035 Caldari Navy), Infomorph Psychology I (24242), and Infomorph Synchronizing I (33399)
    // Negative standing (< 0.0) with station owner corporation must be rejected with InsufficientStanding
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
            ch.standings.insert(1_000_035, -1.5);
        }
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 1,
                skill_points: 250.0,
            },
        );
        skills.insert(
            33399,
            TrainedSkillRecord {
                type_id: 33399,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    let low_standing_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        low_standing_err.unwrap_err().code,
        "InsufficientStanding",
        "NPC station jump clone install must enforce standing threshold with station owner"
    );

    // Restore standing with station owner to 0.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_035, 0.0);
        }
    });

    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());

    // Install 1st jump clone in NPC station (feat:clones.jump-clone-install)
    let install_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await
        .expect("InstallCloneInStation");
    let PyRep::KeyVal(install_kv) = install_rep else {
        panic!("Expected KeyVal from InstallCloneInStation");
    };
    let jc_id = install_kv.get("jumpCloneID").unwrap().as_i64().unwrap();

    // Verify 900,000 ISK fee deducted
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert_eq!(bal_before - bal_after, 900_000.0);

    // Dock at a 2nd NPC station with clone bay (Perimeter II - Moon 1 - Caldari Navy Assembly Plant, 60003754):
    // with Infomorph Psychology I (1 slot) already used, 2nd station install must fail with MaxJumpClonesReached
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_754);
        }
    });
    sess.station_id = Some(60_003_754);
    let second_station_limit_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        second_station_limit_err.unwrap_err().code,
        "MaxJumpClonesReached",
        "2nd station install with Infomorph Psychology I (1 slot) must fail with MaxJumpClonesReached"
    );

    // Train Infomorph Psychology (24242) to level 2 (2 slots) and verify 2nd station install succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 2,
                skill_points: 1415.0,
            },
        );
    });
    let second_station_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await
        .expect("2nd InstallCloneInStation after training Infomorph Psychology II");
    let PyRep::KeyVal(second_station_kv) = second_station_rep else {
        panic!("Expected KeyVal from 2nd InstallCloneInStation");
    };
    assert_eq!(
        second_station_kv.get("locationID").and_then(|v| v.as_i64()),
        Some(60_003_754)
    );
    let bal_after_second = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert_eq!(bal_after - bal_after_second, 900_000.0);

    // Verify MaxJumpClonesReached when Infomorph Psychology II (2 slots) is full on structure install
    let clone_struct_id = 1_030_000_099_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            clone_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: clone_struct_id,
                type_id: 35832,
                owner_corp_id: 1_000_001,
                solar_system_id: 30_000_142,
                name: "Jita Clone Bay Astrahus".into(),
                position: [1000.0, 2000.0, 3000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 5000.0,
                armor_hp: 5000.0,
                hull_hp: 5000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(clone_struct_id);
        }
    });
    sess.station_id = None;
    sess.structure_id = Some(clone_struct_id);

    // Bare Astrahus lacking Standup Cloning Center I service module must fail with NoCloneBayService (feat:clones.jump-clone-install)
    let bare_struct_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStructure",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        bare_struct_err.unwrap_err().code,
        "NoCloneBayService",
        "Bare Upwell structure lacking Standup Cloning Center module must be rejected with NoCloneBayService"
    );

    // Fit Standup Cloning Center I (type_id 35894) in ServiceSlot0 and set online
    let clone_bay_item_id = 100_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            clone_bay_item_id,
            InventoryItemRecord {
                item_id: clone_bay_item_id,
                type_id: 35894,
                owner_id: 1_000_001,
                location_id: clone_struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(clone_bay_item_id, true);
    });

    let max_clones_err = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStructure",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        max_clones_err.unwrap_err().code,
        "MaxJumpClonesReached",
        "Must enforce max jump clone slot limit from Infomorph Psychology"
    );

    // Grant Advanced Infomorph Psychology I (33407) to unlock a 2nd jump clone slot, then install in Upwell structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            33407,
            TrainedSkillRecord {
                type_id: 33407,
                level: 1,
                skill_points: 750.0,
            },
        );
    });
    let struct_install_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStructure",
            &[],
            &kw,
            None,
        )
        .await
        .expect("InstallCloneInStructure with Advanced Infomorph Psychology I");
    let PyRep::KeyVal(struct_jc_kv) = struct_install_rep else {
        panic!("Expected KeyVal from InstallCloneInStructure");
    };
    assert_eq!(
        struct_jc_kv.get("locationID").and_then(|v| v.as_i64()),
        Some(clone_struct_id)
    );

    // Return pilot to Jita 4-4 (60003760) for clone jump test
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
        }
    });
    sess.station_id = Some(60_003_760);
    sess.structure_id = None;

    // 2. Clone Jump & Implant swap (feat:clones.jump-clone-jump)
    // Setup destination clone at another station (60008494) with implant 9941
    // Character active implant is 9899
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.insert(char_id, vec![9899]);
        if let Some(clones) = snap.jump_clones.get_mut(&char_id) {
            if let Some(c) = clones.iter_mut().find(|c| c.jump_clone_id == jc_id) {
                c.location_id = 60_008_494;
                c.implants = vec![9941];
            }
        }
    });

    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await
        .expect("CloneJump");
    let PyRep::KeyVal(jump_kv) = jump_rep else {
        panic!("Expected KeyVal from CloneJump");
    };
    assert_eq!(
        jump_kv.get("locationID").unwrap().as_i64(),
        Some(60_008_494)
    );

    // Verify cooldown timer is 23h (24h - 1h * Infomorph Sync 1)
    let expected_cooldown = clone_jump_cooldown_seconds(1);
    assert_eq!(expected_cooldown, 23 * 3600);
    assert_eq!(
        jump_kv.get("cooldownSeconds").unwrap().as_i64(),
        Some(expected_cooldown as i64)
    );

    // Verify implant swap: active implants now [9941], left behind clone has [9899]
    ctx.db.with_snapshot(|snap| {
        let active = &snap.active_implants[&char_id];
        assert_eq!(active, &vec![9941], "Active implants must be swapped");

        let left_behind = snap.jump_clones[&char_id]
            .iter()
            .find(|c| c.jump_clone_id == jc_id)
            .expect("Left-behind jump clone");
        assert_eq!(left_behind.location_id, 60_003_760);
        assert_eq!(
            left_behind.implants,
            vec![9899],
            "Previous active implants stored in left-behind clone"
        );
    });

    // 3. Attempting clone jump before 24h cooldown expires must fail
    let early_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "CloneJump",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(early_jump.unwrap_err().code, "CloneJumpCooldownActive");

    // 4. Destroy installed clone (feat:clones.jump-clone-destroy)
    let destroy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "DestroyInstalledClone",
            &[PyRep::Integer(jc_id)],
            &kw,
            None,
        )
        .await
        .expect("DestroyInstalledClone");
    assert_eq!(destroy_rep, PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        let clones = &snap.jump_clones[&char_id];
        assert!(
            clones.iter().all(|c| c.jump_clone_id != jc_id),
            "Destroyed jump clone must be removed"
        );
    });
}

