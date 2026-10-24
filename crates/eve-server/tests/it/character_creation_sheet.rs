//! Integration tests for Character Creation (`CH-3`), Character Sheet & Notes (`CH-4`), and Jump Clones (`S-4`).


use crate::common;

use eve_db::{InventoryFlag, IskCents, SkillQueueEntry};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::sp_for_skill_level;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_ch3_character_creation_and_starter_assets() {
    // parity: feat:character.list-characters, feat:character.select-character, feat:character.create-character, feat:character.starter-skills, feat:character.school-corp, feat:character.random-name, feat:character.name-validation, rpc:charUnboundMgr.CreateCharacterWithDoll, rpc:charUnboundMgr.GetCharacterInfo, rpc:charUnboundMgr.GetValidRandomName, rpc:charUnboundMgr.SelectCharacterID, rpc:charUnboundMgr.ValidateNameEx, rpc:invbroker.SetLabel
    eve_testkit::attest(&["feat:character.list-characters", "feat:character.select-character", "feat:character.create-character", "feat:character.starter-skills", "feat:character.school-corp", "feat:character.random-name", "feat:character.name-validation"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
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
