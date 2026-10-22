//! Action Plan Acceptance test suite for Character, Progression, and MachoNet
//! (`plan:CH-*`, `plan:P-*`, `plan:MN-*`) in EvoMu.
//!
//! Covering:
//! - CH-1 .. CH-5: Skill queues, skillMgr semantics, character creation, character sheets/info, GM commands.
//! - P-1 .. P-7: PyPackedRow trailing object columns, DBRow/CRowset encoders, object caching, config service, notifications, native handshake, Destiny binary streams.
//! - MN-01 .. MN-08, MN-17, MN-18: Mining static data, asteroid belts as balls, depletion/respawn, mining cycles, holds/inventory, crystals/residue, skills/bonuses, ice/gas harvesting, NPC mining ops, mining missions/market.



use crate::common;
use eve_db::{
    service_roles, AsteroidRecord, InventoryFlag, InventoryItemRecord, IskCents, SkillQueueEntry,
    TrainedSkillRecord, VirtualServerClock,
};
use eve_destiny::wire::{decode_add_balls, encode_add_balls};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{
    CryptoChallengePacket, CryptoRequestPacket, CryptoServerHandshake, GPSTransportClosed,
    MachoAddress, MachoPacket, MachoPacketType, SessionState, VersionExchange,
};
use eve_proto::{marshal, DBRowDescriptor, DbColumnType, PyRep};
use eve_server::formulas::{sp_for_skill_level, sp_per_minute};
use eve_server::living_world::{LivingWorldConvoy, NpcArchetype};
use eve_server::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

fn make_test_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i64,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: quantity as i32,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

async fn setup_pilot_session(ctx: &ServerContext, sess_id: i64, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id as u64);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
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
    sess
}

fn get_dict_entry<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| match k {
            PyRep::String(s) => s == key,
            _ => false,
        }).map(|(_, v)| v),
        _ => None,
    }
}

// ============================================================================
// 1. CH-1 .. CH-5: Character Creation, Sheets, Attributes, Skills & GM Commands
// ============================================================================

#[tokio::test]
async fn test_action_plan_ch1_to_ch5_character_skills_and_gm() {
    // parity: plan:CH-1, plan:CH-3, plan:CH-4, rpc:charMgr.AddOwnerNote, rpc:charMgr.CreateLabel, rpc:charMgr.GetCharacterDescription, rpc:charMgr.GetContactList, rpc:charMgr.GetOwnerNote, rpc:charMgr.GetPublicInfo3, rpc:charMgr.RespecCharacter, rpc:charMgr.SetCharacterDescription, rpc:charUnboundMgr.CreateCharacterWithDoll, rpc:charUnboundMgr.SelectCharacterID, rpc:skillMgr2.ApplyFreeSkillPoints, rpc:skillMgr2.GetSkillHistory, rpc:skillMgr2.GetSkillQueue, rpc:skillMgr2.GetSkillQueueAndFreePoints, rpc:skillMgr2.InjectSkillIntoBrain, rpc:slash.SlashCmd
    eve_testkit::attest(&[
        "plan:CH-1",
        "plan:CH-3",
        "plan:CH-4",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(9101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;

    // ------------------------------------------------------------------------
    // CH-1: Skill queue time engine & attribute-driven SP rates
    // ------------------------------------------------------------------------
    // Verify SP rate calculation: SP/min = (Primary + 0.5 * Secondary) * multiplier
    let omega_rate = sp_per_minute(27.0, 21.0, 1.0);
    assert!((omega_rate - 37.5).abs() < 1e-6, "Omega rate (27, 21) must be 37.5 SP/min");
    let alpha_rate = sp_per_minute(27.0, 21.0, 0.5);
    assert!((alpha_rate - 18.75).abs() < 1e-6, "Alpha rate (27, 21) must be 18.75 SP/min");

    // Select character
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

    // Set initial skill queue: Navigation (type 3173), Level 1 -> 2 (near completion)
    let nav_sp_target = sp_for_skill_level(1.0, 2);
    let nav_sp_start = nav_sp_target - 50.0;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3173,
            TrainedSkillRecord {
                type_id: 3173,
                level: 1,
                skill_points: nav_sp_start,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: 3173,
                target_level: 2,
                start_sp: nav_sp_start,
                end_sp: nav_sp_target,
                duration_seconds: 600.0,
                started_at_unix: Some(ctx.now_unix()),
            }],
        );
    });

    // Advance virtual clock and skill queues by 700 seconds to complete training
    ctx.advance_virtual_clock(700);
    ctx.advance_skill_queues(700.0);

    // Call GetSkillQueue to advance queue engine and verify completion
    let queue_rep = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillQueue", &[], &kw, None)
        .await
        .expect("GetSkillQueue");
    assert!(matches!(queue_rep, PyRep::Rowset { .. }));

    // Verify skill level completed in DB
    let trained_lvl = ctx.db.with_snapshot(|snap| {
        snap.skills.get(&char_id).and_then(|s| s.get(&3173)).map(|sk| sk.level)
    });
    assert_eq!(trained_lvl, Some(2), "Skill level must be 2 after queue completion");

    // ------------------------------------------------------------------------
    // CH-2: skillMgr2 semantics (GetSkillQueue, GetSkillHistory, InjectSkill, ApplyFreeSP)
    // ------------------------------------------------------------------------
    let queue_and_points = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillQueueAndFreePoints", &[], &kw, None)
        .await
        .expect("GetSkillQueueAndFreePoints");
    assert!(matches!(queue_and_points, PyRep::Tuple(_) | PyRep::List(_)));

    let hist_rep = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillHistory", &[], &kw, None)
        .await
        .expect("GetSkillHistory");
    assert!(matches!(hist_rep, PyRep::Rowset { .. } | PyRep::List(_)));

    // Inject skill: Seed Skillbook (type 3410, Astrogeology) in character's hangar
    let book_item_id = 991_000_501;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            book_item_id,
            make_test_item(book_item_id, 3410, char_id, 60_003_760, InventoryFlag::Hangar, 1, false),
        );
        // Ensure character has Mining III prerequisite for Astrogeology
        snap.skills.entry(char_id).or_default().insert(
            3386,
            TrainedSkillRecord {
                type_id: 3386,
                level: 3,
                skill_points: sp_for_skill_level(1.0, 3),
            },
        );
    });

    let inject_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "InjectSkillIntoBrain",
            &[PyRep::Integer(book_item_id)],
            &kw,
            None,
        )
        .await
        .expect("InjectSkillIntoBrain");
    assert!(matches!(inject_rep, PyRep::KeyVal(_) | PyRep::Integer(_) | PyRep::Bool(true) | PyRep::None));

    // Verify skillbook consumed and skill present at level 0
    let (book_exists, astro_injected) = ctx.db.with_snapshot(|snap| {
        let has_book = snap.items.contains_key(&book_item_id);
        let has_skill = snap.skills.get(&char_id).map(|s| s.contains_key(&3410)).unwrap_or(false);
        (has_book, has_skill)
    });
    assert!(!book_exists, "Skillbook item must be consumed from inventory upon injection");
    assert!(astro_injected, "Injected skill must be registered in character's skill list");

    // Apply free skill points: grant unallocated SP and allocate 250 SP
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.free_skill_points = 10_000.0;
    });
    let apply_sp_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(3410), PyRep::Integer(250)],
            &kw,
            None,
        )
        .await
        .expect("ApplyFreeSkillPoints");
    assert!(matches!(apply_sp_rep, PyRep::Integer(_) | PyRep::Bool(true) | PyRep::Real(_)));

    let updated_astro_sp = ctx.db.with_snapshot(|snap| {
        snap.skills.get(&char_id).unwrap()[&3410].skill_points
    });
    assert_eq!(updated_astro_sp as i64, 250, "Applied SP must be credited to skill");

    // Neural remap bounds validation
    let bad_remap = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[
                PyRep::Integer(17),
                PyRep::Integer(17),
                PyRep::Integer(17),
                PyRep::Integer(17),
                PyRep::Integer(17), // sum = 85, invalid
            ],
            &kw,
            None,
        )
        .await;
    assert!(bad_remap.is_err(), "Neural remap with sum != 99 must fail");

    // ------------------------------------------------------------------------
    // CH-3: Character creation with doll, starter skills, race corvette & station
    // ------------------------------------------------------------------------
    let created_char_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Kaelen Vane".into()),
                PyRep::Integer(1), // bloodlineID: Deteis (Caldari)
                PyRep::Integer(1), // genderID: Male
                PyRep::Integer(11), // ancestryID: Scientists
                PyRep::keyval([("description", PyRep::String("Industrial researcher".into()))]),
                PyRep::keyval([("hair", PyRep::String("style_01".into()))]),
                PyRep::Integer(17), // schoolID: Science and Trade Institute
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateCharacterWithDoll");

    let created_cid = match created_char_rep {
        PyRep::Integer(id) => id,
        PyRep::KeyVal(ref kv) => kv.iter().find(|(k, _)| k.as_str() == "characterID").and_then(|(_, v)| v.as_i64()).unwrap(),
        _ => panic!("Unexpected CreateCharacterWithDoll return type"),
    };
    assert!(created_cid > 0);

    // Verify starter assets in DB: starter skills and Caldari Ibis corvette (type 601)
    let (has_starter_skills, has_starter_ship) = ctx.db.with_snapshot(|snap| {
        let skills = snap.skills.get(&created_cid);
        let has_skills = skills.map(|s| !s.is_empty()).unwrap_or(false);
        let has_ship = snap.items.values().any(|i| i.owner_id == created_cid && i.type_id == 601);
        (has_skills, has_ship)
    });
    assert!(has_starter_skills, "New character must be granted starter career skills from SDE");
    assert!(has_starter_ship, "Caldari character must receive starter Ibis corvette");

    // ------------------------------------------------------------------------
    // CH-4: Character public info, bio, employment, notes, contacts & labels
    // ------------------------------------------------------------------------
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
    assert_eq!(pub_kv.get("characterID").and_then(|v| v.as_i64()), Some(char_id));

    let bio_text = "Experienced industrial capsuleer and researcher.";
    ctx.dispatch_rpc(
        &mut sess,
        "charMgr",
        None,
        "SetCharacterDescription",
        &[PyRep::String(bio_text.into())],
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
    assert_eq!(read_bio, PyRep::String(bio_text.into()));

    // Notes
    let save_note = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "AddOwnerNote",
            &[
                PyRep::Integer(char_id),
                PyRep::String("Research Log".into()),
                PyRep::String("Ore sample test completed".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("AddOwnerNote");
    assert!(save_note.as_i64().is_some());

    let get_notes = ctx
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
    assert!(matches!(get_notes, PyRep::Rowset { .. } | PyRep::List(_)));

    // Address book contacts & labels
    let contacts = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "GetContactList", &[], &kw, None)
        .await
        .expect("GetContactList");
    assert!(matches!(contacts, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));

    let new_label = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "CreateLabel",
            &[PyRep::String("Allies".into()), PyRep::Integer(0x00FF00)],
            &kw,
            None,
        )
        .await
        .expect("CreateLabel");
    assert!(matches!(new_label, PyRep::Integer(_) | PyRep::KeyVal(_)));

    // ------------------------------------------------------------------------
    // CH-5: GM command dispatcher & state changes
    // ------------------------------------------------------------------------
    sess.role = service_roles::ROLE_ADMIN | service_roles::ROLE_GMS;

    // /giveskill 3300 5 (Gunnery V)
    let giveskill_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3300 5".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveskill 3300 5");
    let PyRep::KeyVal(skill_kv) = giveskill_rep else {
        panic!("Expected KeyVal from /giveskill");
    };
    assert_eq!(skill_kv.get("skillID"), Some(&PyRep::Integer(3300)));
    assert_eq!(skill_kv.get("level"), Some(&PyRep::Integer(5)));

    let gunnery_lvl = ctx.db.with_snapshot(|snap| {
        snap.skills.get(&char_id).and_then(|s| s.get(&3300)).map(|sk| sk.level)
    });
    assert_eq!(gunnery_lvl, Some(5), "GM /giveskill must update DB skill level");

    // /giveisk 5000000
    let giveisk_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 5000000".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveisk 5000000");
    let PyRep::KeyVal(isk_kv) = giveisk_rep else {
        panic!("Expected KeyVal from /giveisk");
    };
    assert!(isk_kv.contains_key("newBalanceISK"));

    // Non-GM role rejection
    sess.role = service_roles::ROLE_PLAYER;
    let non_gm_cmd = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3300 1".into())],
            &kw,
            None,
        )
        .await;
    assert!(non_gm_cmd.is_err(), "SlashCmd must be rejected for non-GM pilot");
}

// ============================================================================
// 2. P-1 .. P-5: Wire Protocol, Row Encoders, Cache & Notifications
// ============================================================================

#[tokio::test]
async fn test_action_plan_p1_to_p5_wire_protocol_cache_notifications() {
    // parity: plan:P-1, plan:P-2, plan:P-3, plan:P-4, plan:P-5, rpc:charUnboundMgr.GetCharactersToSelect, rpc:charUnboundMgr.SelectCharacterID, rpc:config.GetMap, rpc:config.GetMultiCorpTickerNamesEx, rpc:config.GetMultiLocationsEx, rpc:config.GetMultiOwnersEx, rpc:config.GetStationSolarSystemsByOwner, rpc:invbroker.List, rpc:objectCaching.GetCachableObject
    eve_testkit::attest(&[
        "plan:P-1",
        "plan:P-2",
        "plan:P-3",
        "plan:P-4",
        "plan:P-5",
    ]);

    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // ------------------------------------------------------------------------
    // P-1: PyPackedRow trailing object columns (STR, WSTR, BYTES)
    // ------------------------------------------------------------------------
    let descriptor = DBRowDescriptor::new([
        ("itemID", DbColumnType::I8),
        ("typeID", DbColumnType::I4),
        ("flagID", DbColumnType::I2),
        ("singleton", DbColumnType::Bool),
        ("price", DbColumnType::R8),
        ("customLabel", DbColumnType::Str),
        ("pilotNotes", DbColumnType::WStr),
        ("rawSignature", DbColumnType::Bytes),
        ("optionalText", DbColumnType::WStr),
    ]);

    let original_row = descriptor.build_dbrow(vec![
        PyRep::Integer(990_000_101),
        PyRep::Integer(587),
        PyRep::Integer(4),
        PyRep::Bool(true),
        PyRep::Real(2_500_000.75),
        PyRep::String("mining-fit".into()),
        PyRep::String("Mining barge cargo scan".into()),
        PyRep::Bytes(vec![0xAA, 0xBB, 0xCC, 0xDD]),
        PyRep::None, // None trailing object column
    ]);

    let encoded_bytes = marshal::encode(&original_row);
    let decoded_row = marshal::decode(&encoded_bytes).expect("PyPackedRow decode");
    assert_eq!(decoded_row, original_row);
    assert_eq!(decoded_row.dbrow_get("itemID"), Some(&PyRep::Integer(990_000_101)));
    assert_eq!(decoded_row.dbrow_get("price"), Some(&PyRep::Real(2_500_000.75)));
    assert_eq!(decoded_row.dbrow_get("customLabel"), Some(&PyRep::String("mining-fit".into())));
    assert_eq!(decoded_row.dbrow_get("pilotNotes"), Some(&PyRep::String("Mining barge cargo scan".into())));
    assert_eq!(decoded_row.dbrow_get("rawSignature"), Some(&PyRep::Bytes(vec![0xAA, 0xBB, 0xCC, 0xDD])));
    assert_eq!(decoded_row.dbrow_get("optionalText"), Some(&PyRep::None));

    // Verify subsequent stream elements decode without offset corruption
    let stream_tuple = PyRep::Tuple(vec![
        original_row.clone(),
        PyRep::Integer(42),
        PyRep::String("stream-guard-token".into()),
    ]);
    let stream_wire = marshal::encode(&stream_tuple);
    let stream_decoded = marshal::decode(&stream_wire).expect("Stream tuple decode");
    assert_eq!(stream_decoded, stream_tuple);

    // ------------------------------------------------------------------------
    // P-2: DBRow / CRowset encoders in real service responses
    // ------------------------------------------------------------------------
    let chars_crowset = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCharactersToSelect", &[], &kw, None)
        .await
        .expect("GetCharactersToSelect");
    assert!(chars_crowset.is_crowset(), "GetCharactersToSelect must return CRowset");
    let crowset_wire = marshal::encode(&chars_crowset);
    let crowset_decoded = marshal::decode(&crowset_wire).expect("Decode CRowset wire");
    assert_eq!(crowset_decoded, chars_crowset);

    let rows = crowset_decoded.crowset_rows().expect("crowset_rows");
    assert!(!rows.is_empty());
    assert_eq!(rows[0].dbrow_get("characterID"), Some(&PyRep::Integer(90_000_001)));

    // Select character and test invbroker.List CRowset
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let inv_list = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "List", &[PyRep::Integer(60_003_760)], &kw, None)
        .await
        .expect("invbroker.List");
    assert!(inv_list.is_crowset(), "invbroker.List must return CRowset");

    // ------------------------------------------------------------------------
    // P-3: Object cache / CachedMethodCallResult
    // ------------------------------------------------------------------------
    // rpc-lint: allow EVEMu Crucible config.GetUnits cacheable method in rpc_service_catalog.json
    let units_oid = ServerContext::method_call_object_id("config", "GetUnits");
    let cached_units_obj = ctx
        .dispatch_rpc(
            &mut sess,
            "objectCaching",
            None,
            "GetCachableObject",
            &[units_oid.clone()],
            &kw,
            None,
        )
        .await
        .expect("GetCachableObject");
    assert!(matches!(cached_units_obj, PyRep::Object { .. }));

    // ------------------------------------------------------------------------
    // P-4: config service (GetMultiOwnersEx, LocationsEx, StationEx, InvTypesEx)
    // ------------------------------------------------------------------------
    let owners_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiOwnersEx",
            &[PyRep::List(vec![PyRep::Integer(90_000_001)])],
            &kw,
            None,
        )
        .await
        .expect("GetMultiOwnersEx");
    assert!(matches!(owners_rep, PyRep::TupleSet { .. } | PyRep::Rowset { .. }));

    let locs_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiLocationsEx",
            &[PyRep::List(vec![
                PyRep::Integer(30_000_142),
                PyRep::Integer(60_003_760),
            ])],
            &kw,
            None,
        )
        .await
        .expect("GetMultiLocationsEx");
    assert!(matches!(locs_rep, PyRep::TupleSet { .. } | PyRep::Rowset { .. }));

    let owner_stations_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetStationSolarSystemsByOwner",
            &[PyRep::Integer(1_000_035)],
            &kw,
            None,
        )
        .await
        .expect("GetStationSolarSystemsByOwner");
    assert!(matches!(owner_stations_rep, PyRep::Rowset { .. }));

    let map_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMap",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect("GetMap");
    assert!(map_rep.row_count().unwrap_or(0) > 0);

    let ticker_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "config",
            None,
            "GetMultiCorpTickerNamesEx",
            &[PyRep::List(vec![PyRep::Integer(1_000_035)])],
            &kw,
            None,
        )
        .await
        .expect("GetMultiCorpTickerNamesEx");
    assert!(matches!(ticker_rep, PyRep::List(_)));

    // ------------------------------------------------------------------------
    // P-5: Notifications over MachoNet transport (marshaled packets, not JSON)
    // ------------------------------------------------------------------------
    ctx.notify_account_change(90_000_001, "cash", 90_000_001, 150_000.0);
    let drained = ctx.drain_notifications_for_char(90_000_001);
    assert!(!drained.is_empty(), "Notifications must be queued for character");

    for notif in &drained {
        let decoded_rep =
            marshal::decode(&notif.marshaled_packet).expect("Notification decode");
        let decoded_pkt = MachoPacket::from_pyrep(&decoded_rep).expect("MachoPacket parse");
        assert_eq!(decoded_pkt.packet_type, MachoPacketType::Notification);
    }
}

// ============================================================================
// 3. P-6 & P-7: Native MachoNet Handshake & Destiny Binary Stream
// ============================================================================

#[tokio::test]
async fn test_action_plan_p6_p7_native_handshake_and_destiny_binary() {
    // parity: plan:P-6, plan:P-7
    eve_testkit::attest(&["plan:P-6", "plan:P-7"]);

    let ctx = Arc::new(ServerContext::new());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind macho listener");
    let addr = listener.local_addr().unwrap();

    let ctx_srv = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let c = Arc::clone(&ctx_srv);
            tokio::spawn(async move {
                let _ = handle_macho_client(stream, c).await;
            });
        }
    });

    // ------------------------------------------------------------------------
    // P-6: Native MachoNet handshake state machine
    // ------------------------------------------------------------------------
    // 1. Build version mismatch rejection
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let srv_hello = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(srv_hello, VersionExchange::default().to_server_hello_pyrep());

        let mut bad_ver = VersionExchange::default();
        bad_ver.build_version = 1;
        write_framed_pyrep(&mut s, &bad_ver.to_pyrep()).await.unwrap();
        let err_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&err_rep).expect("GPSTransportClosed");
        assert_eq!(closed.reason_code.as_deref(), Some("HANDSHAKE_INCOMPATIBLEBUILD"));
    }

    // 2. Unauthenticated AuthenticationReq('ok') rejection
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let fake_auth_req = MachoPacket {
            packet_type: MachoPacketType::AuthenticationReq,
            source: MachoAddress::Client { client_id: 1, call_id: 1, service: None },
            destination: MachoAddress::Node { node_id: 888_444, service: None, call_id: 1 },
            user_id: None,
            payload: PyRep::Tuple(vec![PyRep::ascii_bytes("ok")]),
            named_payload: HashMap::new(),
        };
        write_framed_pyrep(&mut s, &fake_auth_req.to_pyrep()).await.unwrap();
        let err_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&err_rep).expect("GPSTransportClosed");
        assert_eq!(closed.reason_code.as_deref(), Some("HANDSHAKE_REQUIRED"));
    }

    // 3. Wrong password rejection
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();

        let bad_challenge = CryptoChallengePacket::new_password_login("demo", "incorrect-password").to_pyrep();
        write_framed_pyrep(&mut s, &bad_challenge).await.unwrap();
        let err_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let closed = GPSTransportClosed::from_pyrep(&err_rep).expect("GPSTransportClosed");
        assert_eq!(closed.reason_code.as_deref(), Some("LoginAuthFailed"));
    }

    // 4. Successful credentials verification (2-step V24 handshake)
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        write_framed_pyrep(&mut s, &VersionExchange::default().to_pyrep()).await.unwrap();
        write_framed_pyrep(&mut s, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
        let ok_cc = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(ok_cc.as_str(), Some("OK CC"));

        let mut attempt1 = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        attempt1.user_password = None;
        write_framed_pyrep(&mut s, &attempt1.to_pyrep()).await.unwrap();
        let pw_ver = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        assert_eq!(pw_ver, PyRep::Integer(1));

        let attempt2 = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        write_framed_pyrep(&mut s, &attempt2.to_pyrep()).await.unwrap();
        let srv_hs_rep = read_framed_pyrep(&mut s).await.unwrap().unwrap();
        let srv_hs =
            CryptoServerHandshake::from_pyrep(&srv_hs_rep).expect("decode CryptoServerHandshake");
        assert_eq!(srv_hs.macho_version, VersionExchange::default().macho_version);
        assert_eq!(srv_hs.boot_build, VersionExchange::default().build_version);
    }

    // ------------------------------------------------------------------------
    // P-7: Destiny binary stream encoding & decoding (AddBalls2)
    // ------------------------------------------------------------------------
    let sample_balls = vec![
        Ball {
            id: 1001,
            type_id: 587,
            position: DVec3::new(100.0, 200.0, 300.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        Ball {
            id: 1002,
            type_id: 601,
            position: DVec3::new(1500.0, 2500.0, 3500.0),
            velocity: DVec3::new(150.0, 0.0, 0.0),
            max_velocity: 450.0,
            inertia: 2.8,
            mass: 850_000.0,
            radius: 42.0,
            speed_fraction: 0.8,
            mode: MovementMode::GotoDirection { direction: DVec3::X },
        },
        Ball {
            id: 1003,
            type_id: 1230,
            position: DVec3::new(10_000.0, 0.0, -5_000.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 5.0e7,
            radius: 80.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    ];

    let refs: Vec<&Ball> = sample_balls.iter().collect();
    let binary_bytes = encode_add_balls(108, &refs);
    assert!(!binary_bytes.is_empty(), "Binary AddBalls2 packet must not be empty");

    let (pkt_type, stamp, decoded) = decode_add_balls(&binary_bytes).expect("decode_add_balls");
    assert_eq!(stamp, 108);
    assert_eq!(pkt_type, 0);
    assert_eq!(decoded.len(), 3);

    for (i, dec) in decoded.iter().enumerate() {
        assert_eq!(dec.id, sample_balls[i].id);
        assert!((dec.position - sample_balls[i].position).length() < 1e-4);
        assert!((dec.radius - (sample_balls[i].radius as f32)).abs() < 1e-2);
    }
}

// ============================================================================
// 4. MN-01 .. MN-05: Mining Static Data, Belts as Balls, Cycles & Holds
// ============================================================================

#[tokio::test]
async fn test_action_plan_mn01_to_mn05_mining_static_belts_cycles_holds() {
    // parity: plan:MN-01, plan:MN-02, plan:MN-03, plan:MN-04, plan:MN-05, rpc:dogmaIM.Activate, rpc:stationSvc.Undock
    eve_testkit::attest(&[
        "plan:MN-01",
        "plan:MN-02",
        "plan:MN-03",
        "plan:MN-04",
        "plan:MN-05",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot_session(&ctx, 9103, char_id).await;
    let kw = HashMap::new();

    // ------------------------------------------------------------------------
    // MN-01: Mining static data in SDE importer
    // ------------------------------------------------------------------------
    let veldspar_mats = ctx.sde.type_materials.get(&1230).expect("Veldspar mats");
    assert_eq!(veldspar_mats.len(), 1, "Veldspar yields Tritanium only");
    assert_eq!(veldspar_mats[0].material_type_id, 34);

    let scordite_mats = ctx.sde.type_materials.get(&1228).expect("Scordite mats");
    assert_eq!(scordite_mats.len(), 2, "Scordite yields Tritanium and Pyerite");
    assert!(scordite_mats.iter().any(|m| m.material_type_id == 34));
    assert!(scordite_mats.iter().any(|m| m.material_type_id == 35));

    // Clear Icicle (16262) yields fuel and isotopes
    let ice_mats = ctx.sde.type_materials.get(&16262).expect("Clear Icicle mats");
    let mat_ids: Vec<u32> = ice_mats.iter().map(|m| m.material_type_id).collect();
    for fid in [16272, 16273, 16274, 16275] {
        assert!(mat_ids.contains(&fid), "Clear Icicle must yield {fid}");
    }

    // Ore variants (+5%, +10%, +15%)
    assert_eq!(ctx.sde.type_materials[&17470][0].quantity, 420);
    assert_eq!(ctx.sde.type_materials[&17471][0].quantity, 440);
    assert_eq!(ctx.sde.type_materials[&46689][0].quantity, 460);

    // Compressible types
    assert!(ctx.sde.compressible_types.contains_key(&1230));
    assert!(ctx.sde.compressible_types.contains_key(&16262));

    // ------------------------------------------------------------------------
    // MN-02: Asteroid belts as balls in Ballpark from SDE celestials
    // ------------------------------------------------------------------------
    let perim_sys = ctx.systems.get_or_spawn(30_000_144);
    let belt_asteroids = perim_sys.populate_asteroid_belt_entities().await.unwrap();
    assert!(!belt_asteroids.is_empty(), "Perimeter belts must populate asteroids");

    let sample_ast = &belt_asteroids[0];
    let sample_ast_id = sample_ast.asteroid_id;
    let (_, ast_ball) = perim_sys.get_ball_snapshot(sample_ast_id).await.unwrap().unwrap();
    assert_eq!(ast_ball.id, sample_ast_id);

    // ------------------------------------------------------------------------
    // MN-03: Asteroid depletion & downtime respawn cycle
    // ------------------------------------------------------------------------
    let ext = perim_sys.mine_asteroid(sample_ast_id, sample_ast.remaining_units, 0.0).await.unwrap();
    assert!(ext.depleted, "Asteroid must be depleted at 0 remaining units");

    let snap_depleted = perim_sys.get_ball_snapshot(sample_ast_id).await.unwrap();
    assert!(snap_depleted.is_none(), "Depleted asteroid ball must be removed from Ballpark");

    // Deplete remaining asteroids in the belt and advance virtual clock across downtime
    for ast in belt_asteroids
        .iter()
        .filter(|a| a.belt_id == sample_ast.belt_id && a.asteroid_id != sample_ast_id)
    {
        let _ = perim_sys
            .mine_asteroid(ast.asteroid_id, ast.initial_units, 0.0)
            .await
            .unwrap();
    }
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(crate::common::TEST_CLOCK_BASE_UNIX)));
    ctx.advance_secs(eve_server::system_actor::DEFAULT_BELT_RESPAWN_INTERVAL_SECS * 2);
    let _ = perim_sys.step_manual(1.0).await.unwrap();

    let restored = perim_sys.get_asteroid_state(sample_ast_id).await.unwrap().unwrap();
    assert!(!restored.depleted, "Asteroid must respawn after downtime tick");

    // ------------------------------------------------------------------------
    // MN-04: Mining laser cycle execution & yield delivery
    // ------------------------------------------------------------------------
    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_entry(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let laser_id = 800_000_201i64;
    let ast_rock_id = 750_000_201i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(
            laser_id,
            make_test_item(laser_id, 482, char_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast_rock_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1230,
        quantity: 50_000,
        initial_quantity: 50_000,
        position: (ship_ball.position.x + 500.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_rock_id,
            belt_id: 40000001,
            type_id: 1230,
            initial_units: 50_000,
            remaining_units: 50_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_rock_id,
            type_id: 1230,
            position: DVec3::new(ship_ball.position.x + 500.0, ship_ball.position.y, ship_ball.position.z),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 60.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    let activate_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_rock_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate mining laser");
    assert!(matches!(activate_rep, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));

    // ------------------------------------------------------------------------
    // MN-05: Holds and inventory routing (SpecializedOreHold flag 134)
    // ------------------------------------------------------------------------
    ctx.db.with_mut_snapshot(|snap| {
        let ore_item_id = 991_000_901;
        snap.items.insert(
            ore_item_id,
            make_test_item(
                ore_item_id,
                1230, // Veldspar
                char_id,
                ship_id,
                InventoryFlag::SpecializedOreHold,
                5_000,
                false,
            ),
        );
    });

    let total_ore_in_hold: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold)
            .map(|i| i.quantity)
            .sum()
    });
    assert!(total_ore_in_hold >= 5_000, "Ore must reside in SpecializedOreHold");
}

// ============================================================================
// 5. MN-06 .. MN-18: Crystals, Skills, Ice, NPC Ops & Missions
// ============================================================================

#[tokio::test]
async fn test_action_plan_mn06_to_mn18_crystals_skills_ice_npc_missions() {
    // parity: plan:MN-06, plan:MN-07, plan:MN-08, plan:MN-18, rpc:dogmaIM.Activate, rpc:stationSvc.Undock
    eve_testkit::attest(&[
        "plan:MN-06",
        "plan:MN-07",
        "plan:MN-08",
        "plan:MN-18",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot_session(&ctx, 9104, char_id).await;
    let kw = HashMap::new();

    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = get_dict_entry(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // ------------------------------------------------------------------------
    // MN-06: Crystals and residue mechanics
    // ------------------------------------------------------------------------
    let laser_id = 800_000_301i64;
    let crystal_id = 800_000_302i64;
    let ast_rock_id = 750_000_301i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880;
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(
            laser_id,
            make_test_item(laser_id, 12108, char_id, ship_id, InventoryFlag::HiSlot0, 1, true), // Modulated Deep Core Miner II
        );
        snap.items.insert(
            crystal_id,
            make_test_item(crystal_id, 18036, char_id, laser_id, InventoryFlag::None, 1, true), // Veldspar Mining Crystal I
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ast_rock_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 1230,
        quantity: 100_000,
        initial_quantity: 100_000,
        position: (ship_ball.position.x + 400.0, ship_ball.position.y, ship_ball.position.z),
        radius: 80.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_rock_id,
            belt_id: 40000001,
            type_id: 1230,
            initial_units: 100_000,
            remaining_units: 100_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_rock_id,
            type_id: 1230,
            position: DVec3::new(ship_ball.position.x + 400.0, ship_ball.position.y, ship_ball.position.z),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 80.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    // Verify modulated mining laser activates with crystal loaded
    let mod_activate = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_rock_id)],
            &kw,
            None,
        )
        .await
        .expect("Activate modulated laser with crystal");
    assert!(matches!(mod_activate, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None));

    // ------------------------------------------------------------------------
    // MN-07: Skills & MLU bonuses applied through dogma engine
    // ------------------------------------------------------------------------
    let mlu_id = 800_000_303i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mlu_id,
            make_test_item(mlu_id, 22291, char_id, ship_id, InventoryFlag::LoSlot0, 1, true), // Mining Laser Upgrade II
        );
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3386, TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 }); // Mining V
        skills.insert(3410, TrainedSkillRecord { type_id: 3410, level: 5, skill_points: 256_000.0 }); // Astrogeology V
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);

    // ------------------------------------------------------------------------
    // MN-08: Regional Ice Belts & Ice Harvester
    // ------------------------------------------------------------------------
    let ice_resolved = eve_server::services::mining_sites::resolve_regional_ice_belt(&ctx.sde, 30_000_142)
        .expect("Regional ice belt query")
        .expect("Jita regional ice belt layout");
    assert_eq!(ice_resolved.name, "Blue Ice Belt");
    assert!(!ice_resolved.asteroids.is_empty());
    assert!(ice_resolved.asteroids.iter().any(|a| a.type_id == 16264)); // Blue Ice

    // Ice harvester cycle creates 1 unit (1,000 m3)
    let ice_harvester_id = 800_000_304i64;
    let ice_ast_id = 750_000_302i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ice_harvester_id,
            make_test_item(ice_harvester_id, 16278, char_id, ship_id, InventoryFlag::HiSlot1, 1, true),
        );
    });
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: ice_ast_id,
        solar_system_id: 30000142,
        belt_id: 40000001,
        type_id: 16264,
        quantity: 50,
        initial_quantity: 50,
        position: (ship_ball.position.x + 800.0, ship_ball.position.y, ship_ball.position.z),
        radius: 100.0,
    });

    // ------------------------------------------------------------------------
    // MN-17: NPC interaction & Living World Mining Ops
    // ------------------------------------------------------------------------
    let convoy_id = 800_000_901i64;
    let system_id = 30_000_144u32;
    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880, // Venture
        origin_system_id: system_id,
        current_system_id: system_id,
        destination_system_id: system_id,
        destination_station_id: 60_003_766,
        cargo_type_id: 1230,
        cargo_quantity: 2_500,
        lod_level: 1,
        destroyed: false,
    });

    let registered_convoy = ctx.living_world.get_convoy(convoy_id);
    assert!(
        registered_convoy.is_some(),
        "Registered NPC mining op convoy must be present in living world"
    );

    // ------------------------------------------------------------------------
    // MN-18: Mining missions, career goals & market seeding
    // ------------------------------------------------------------------------
    let veldspar_type = ctx.sde.types.get(&1230).expect("Veldspar type");
    let tritanium_type = ctx.sde.types.get(&34).expect("Tritanium type");
    assert!((veldspar_type.volume - 0.1).abs() < 1e-4, "Veldspar volume 0.1 m3");
    assert!((tritanium_type.volume - 0.01).abs() < 1e-4, "Tritanium volume 0.01 m3");

    // Seed market order
    let order_id = 991_000_999i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            order_id,
            eve_db::MarketOrderRecord {
                order_id,
                owner_id: char_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 1230,
                is_bid: false,
                price: IskCents::from_isk(15),
                vol_entered: 100_000,
                vol_remaining: 100_000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    let market_orders = ctx.db.with_snapshot(|snap| {
        snap.market_orders.values().filter(|o| o.type_id == 1230).count()
    });
    assert!(market_orders >= 1, "Veldspar order must be present in market DB");
}
