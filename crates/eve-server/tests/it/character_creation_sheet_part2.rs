//! Integration tests for Character Sheet & Notes (`CH-4`).

use crate::common;
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_ch4_character_sheet_biography_and_notes() {
    // parity: feat:character.show-info-public, feat:character.biography, feat:character.employment-history, feat:character.owner-notes, feat:character.character-sheet, rpc:charMgr.AddOwnerNote, rpc:charMgr.EditOwnerNote, rpc:charMgr.GetCharacterDescription, rpc:charMgr.GetOwnerNote, rpc:charMgr.GetOwnerNoteLabels, rpc:charMgr.GetPrivateInfo, rpc:charMgr.GetPublicInfo3, rpc:charMgr.RemoveOwnerNote, rpc:charMgr.SetCharacterDescription, rpc:charUnboundMgr.SelectCharacterID, rpc:corporationSvc.GetEmployementRecordAndCharacterTransfers
    eve_testkit::attest(&["feat:character.show-info-public", "feat:character.biography", "feat:character.employment-history", "feat:character.owner-notes", "feat:character.character-sheet"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
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

    // 1. Show info public (feat:character.show-info-public) via charMgr.GetPublicInfo3
    for method in ["GetPublicInfo3"] {
        let pub_info = ctx
            .dispatch_rpc(
                &mut sess,
                "charMgr",
                None,
                method,
                &[PyRep::Integer(char_id)],
                &kw,
                None,
            )
            .await
            .expect(method);
        let PyRep::KeyVal(pub_kv) = pub_info else {
            panic!("Expected KeyVal from {method}");
        };
        assert_eq!(pub_kv.get("characterID").unwrap().as_i64(), Some(char_id));
        assert_eq!(
            pub_kv.get("characterName"),
            Some(&PyRep::String("Aura Capsuleer".into()))
        );
        assert_eq!(
            pub_kv.get("corporationID").and_then(|v| v.as_i64()),
            Some(1_000_170)
        );
        assert_eq!(pub_kv.get("allianceID"), Some(&PyRep::None));
        assert_eq!(pub_kv.get("raceID").and_then(|v| v.as_i64()), Some(2));
        assert_eq!(pub_kv.get("bloodlineID").and_then(|v| v.as_i64()), Some(3));
        assert_eq!(pub_kv.get("ancestryID").and_then(|v| v.as_i64()), Some(19));
        assert_eq!(pub_kv.get("schoolID").and_then(|v| v.as_i64()), Some(14));
        assert_eq!(pub_kv.get("securityStatus").and_then(|v| v.as_f64()), Some(0.5));
        assert!(
            pub_kv.get("createDateTime").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
            "{method} must return birthday (createDateTime)"
        );
        assert!(
            pub_kv.get("portraitInfo").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
            "{method} must return portraitInfo"
        );
    }

    // Verify allianceID is populated when character belongs to an alliance
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.alliance_id = Some(99_000_001);
        }
    });
    let pub_with_alliance = ctx
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
        .expect("GetPublicInfo3 with alliance");
    if let PyRep::KeyVal(kv) = pub_with_alliance {
        assert_eq!(kv.get("allianceID").and_then(|v| v.as_i64()), Some(99_000_001));
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.alliance_id = None;
        }
    });

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
    // Populate implants, jump clones, standings, kill rights, medals, and decorations so all 9 sections are verified non-empty
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_035, 5.0);
        }
        snap.active_implants.entry(char_id).or_default().push(9899); // Ocular Filter - Basic
        snap.jump_clones.entry(char_id).or_default().push(eve_db::JumpCloneRecord {
            jump_clone_id: 77_001,
            character_id: char_id,
            location_id: 60_003_760,
            implants: vec![9899],
        });
        snap.kill_rights.insert(
            88_001,
            eve_db::KillRightRecord {
                kill_right_id: 88_001,
                owner_id: char_id,
                target_id: 90_000_002,
                activation_fee: IskCents::from_isk(10_000_000),
                is_active_suspect: false,
            },
        );
        snap.corporations.insert(
            1_000_170,
            eve_db::CorporationRecord {
                corporation_id: 1_000_170,
                name: "Republic University".into(),
                ticker: "RU".into(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.11,
                master_wallet: IskCents(0),
                members: std::collections::HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, u64::MAX)]),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(60_003_760),
            },
        );
    });
    let medal_res = ctx
        .dispatch_rpc(
            &mut sess,
            "corporationSvc",
            None,
            "CreateMedal",
            &[
                PyRep::String("Valorous Service".into()),
                PyRep::String("For fleet valor".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateMedal");
    let medal_id = match medal_res {
        PyRep::KeyVal(ref kv) => kv.get("medalID").and_then(|v| v.as_i64()).unwrap_or(1),
        _ => 1,
    };
    ctx.dispatch_rpc(
        &mut sess,
        "corporationSvc",
        None,
        "GiveMedalToCharacters",
        &[
            PyRep::Integer(medal_id),
            PyRep::Integer(char_id),
            PyRep::String("Fleet valor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("GiveMedalToCharacters");

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
    for section in [
        "skills",
        "implants",
        "jumpClones",
        "standings",
        "killRights",
        "medals",
        "decorations",
    ] {
        let Some(PyRep::List(items)) = priv_kv.get(section) else {
            panic!("GetPrivateInfo must return List for {section}");
        };
        assert!(!items.is_empty(), "GetPrivateInfo.{section} must be non-empty");
    }

    let char_sheet = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
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
        Some(995_000_000.0)
    );
    for section in [
        "skills",
        "implants",
        "jumpClones",
        "standings",
        "killRights",
        "medals",
        "decorations",
    ] {
        let Some(PyRep::List(items)) = sheet_kv.get(section) else {
            panic!("GetCharacterInfo must return List for {section}");
        };
        assert!(!items.is_empty(), "GetCharacterInfo.{section} must be non-empty");
    }
    let PyRep::KeyVal(attr_kv) = sheet_kv.get("attributes").expect("attributes KeyVal") else {
        panic!("Expected KeyVal for attributes");
    };
    assert_eq!(attr_kv.get("intelligence").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(attr_kv.get("memory").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(attr_kv.get("perception").and_then(|v| v.as_i64()), Some(23)); // 20 base + 3 Ocular Filter - Basic (9899)
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
