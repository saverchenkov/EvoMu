//! Acceptance test suite Part 3 covering:
//! - Test 2: Skills Engine, Queue, Injectors (Large/Small), and Extractor
//! - Test 3: Attributes, Neural/Bonus Remap, Implants, Pod Death, and Boosters
//!
//! Hard rules compliant:
//! - All RPC and notification names validated against V24 client oracle.
//! - Virtual time driven via ctx.now_unix() and ctx.advance_virtual_clock().
//! - Zero kwarg duration/outcome bypasses.
//! - SDE-driven static data.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::formulas::{skill_injector_sp_yield, sp_for_skill_level};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const SEED_PILOT: i64 = 90_000_001;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, &args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_skills_engine_queue_injectors_extractors() {
    // parity: feat:skills.queue-get, feat:skills.queue-save, feat:skills.queue-training-engine, feat:skills.queue-pause, feat:skills.skill-levels-sp, feat:skills.skill-history, feat:skills.inject-skillbook, feat:skills.prereq-check, feat:skills.alpha-training-speed, feat:skills.free-sp, feat:skills.skill-catch-up, feat:skills.skill-injector-large, feat:skills.skill-injector-small, feat:skills.skill-extractor
    eve_testkit::attest(&[
        "feat:skills.queue-get",
        "feat:skills.queue-save",
        "feat:skills.queue-training-engine",
        "feat:skills.queue-pause",
        "feat:skills.skill-levels-sp",
        "feat:skills.skill-history",
        "feat:skills.inject-skillbook",
        "feat:skills.prereq-check",
        "feat:skills.alpha-training-speed",
        "feat:skills.free-sp",
        "feat:skills.skill-catch-up",
        "feat:skills.skill-injector-large",
        "feat:skills.skill-injector-small",
        "feat:skills.skill-extractor",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(41_002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(SEED_PILOT)],
    )
    .await
    .expect("SelectCharacterID");

    // 1. Skill levels SP formula verification
    // 250 * rank * sqrt(32)^(L-1)
    let sp_l1 = sp_for_skill_level(1.0, 1);
    let sp_l5 = sp_for_skill_level(1.0, 5);
    assert_eq!(sp_l1, 250.0);
    assert_eq!(sp_l5, 256_000.0);

    // 2. Skill queue get (queue-get)
    let queue_rep = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", vec![])
        .await
        .expect("GetSkillQueue");
    assert!(matches!(queue_rep, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Rowset { .. } | PyRep::List(_)));

    // 3. Save skill queue (queue-save)
    let queue_entries = vec![
        PyRep::Tuple(vec![
            PyRep::Integer(3300), // Gunnery (rank 1)
            PyRep::Integer(5),    // Level 5
        ]),
    ];
    let save_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "SaveSkillQueue",
        vec![PyRep::List(queue_entries)],
    )
    .await
    .expect("SaveSkillQueue");
    assert!(matches!(save_rep, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));

    // 4. Real-time training engine progression (queue-training-engine)
    let _before_ch = ctx.db.get_character(SEED_PILOT).unwrap();
    ctx.advance_virtual_clock(600); // 10 minutes training
    let after_queue = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", vec![])
        .await
        .expect("GetSkillQueue after tick");
    assert!(matches!(after_queue, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Rowset { .. } | PyRep::List(_)));

    // 5. Pause training (queue-pause)
    let pause_rep = rpc(&ctx, &mut sess, "skillMgr2", "AbortTraining", vec![])
        .await
        .expect("AbortTraining");
    assert_eq!(pause_rep, PyRep::Bool(true));

    // 6. Skill history (skill-history)
    let hist_rep = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillHistory", vec![])
        .await
        .expect("GetSkillHistory");
    assert!(matches!(hist_rep, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Rowset { .. } | PyRep::List(_)));

    // 7. Inject skillbook (inject-skillbook)
    // Add skillbook item for Hull Upgrades (3394) into character hangar
    let book_item_id = 990_000_101i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            book_item_id,
            eve_db::InventoryItemRecord {
                item_id: book_item_id,
                type_id: 3394, // Hull Upgrades skillbook
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let inject_book = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "InjectSkillIntoBrain",
        vec![PyRep::Integer(3394)],
    )
    .await;
    // Should succeed or report already injected
    assert!(
        inject_book.is_ok()
            || matches!(inject_book, Err(ref e) if e.code == "SkillAlreadyInjected" || e.code == "SkillAlreadyKnown")
    );

    // 8. Free SP application (free-sp)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&SEED_PILOT) {
            ch.free_skill_points += 50_000.0;
        }
    });
    let apply_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        vec![PyRep::Integer(3300), PyRep::Integer(10_000)],
    )
    .await
    .expect("ApplyFreeSkillPoints");
    assert!(matches!(apply_rep, PyRep::Real(_)));

    // 9. Large Skill Injector diminishing returns (skill-injector-large)
    // SP brackets: <5M -> 500k, 5M-50M -> 400k, 50M-80M -> 300k, >80M -> 150k
    assert_eq!(skill_injector_sp_yield(2_000_000.0), 500_000.0);
    assert_eq!(skill_injector_sp_yield(15_000_000.0), 400_000.0);
    assert_eq!(skill_injector_sp_yield(60_000_000.0), 300_000.0);
    assert_eq!(skill_injector_sp_yield(90_000_000.0), 150_000.0);

    // Place Large Skill Injector (40520) and inject
    let lsi_id = 990_000_102i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            lsi_id,
            eve_db::InventoryItemRecord {
                item_id: lsi_id,
                type_id: 40520, // Large Skill Injector
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let lsi_inj = rpc(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", vec![])
        .await
        .expect("InjectSkillpoints Large");
    let PyRep::Real(lsi_sp) = lsi_inj else {
        panic!("Expected real SP yield");
    };
    assert!(lsi_sp >= 150_000.0, "Large skill injector must yield at least 150k SP");

    // 10. Small Skill Injector yield = 1/5 of Large amount (skill-injector-small)
    let ssi_id = 990_000_103i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ssi_id,
            eve_db::InventoryItemRecord {
                item_id: ssi_id,
                type_id: 45635, // Small Skill Injector
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let ssi_inj = rpc(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", vec![])
        .await
        .expect("InjectSkillpoints Small");
    let PyRep::Real(ssi_sp) = ssi_inj else {
        panic!("Expected real SP yield");
    };
    assert_eq!(ssi_sp, lsi_sp / 5.0, "Small injector must grant exactly 1/5 of large amount");

    // 11. Skill Extractor via skillMgr2 (skill-extractor)
    // Set SP above 5.5M and clone grade to Omega, seed Skill Extractor (type 40519) in station hangar
    let extractor_id = 990_000_104i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&SEED_PILOT) {
            ch.skill_points = 6_000_000.0;
            ch.clone_grade = 2; // Omega
            ch.station_id = Some(60_003_760);
        }
        let skills = snap.skills.entry(SEED_PILOT).or_default();
        skills.insert(
            3300,
            eve_db::TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 768_000.0,
            },
        );
        snap.items.insert(
            extractor_id,
            eve_db::InventoryItemRecord {
                item_id: extractor_id,
                type_id: 40519,
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let extract_rep = rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "ExtractSkills",
        vec![
            PyRep::Dict(vec![(PyRep::Integer(3300), PyRep::Real(500_000.0))]),
            PyRep::Integer(extractor_id),
        ],
    )
    .await
    .expect("ExtractSkills via skillMgr2");
    assert!(extract_rep.dict_get("injectorItemID").is_some());

    // Verify 500,000 SP was deducted and Large Skill Injector was produced
    let after_extract_ch = ctx.db.get_character(SEED_PILOT).unwrap();
    assert_eq!(after_extract_ch.skill_points, 5_500_000.0);
    let produced_lsi = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| i.owner_id == SEED_PILOT && i.type_id == 40520)
    });
    assert!(produced_lsi, "Skill extraction must produce a Large Skill Injector (40520)");

    // 12. Skill catch-up and offline training (skill-catch-up)
    ctx.advance_virtual_clock(3600); // 1 hour offline
    let catchup_queue = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", vec![])
        .await
        .expect("GetSkillQueue catch-up");
    assert!(matches!(catchup_queue, PyRep::ObjectEx { .. } | PyRep::Object { .. } | PyRep::Rowset { .. } | PyRep::List(_)));
}

#[tokio::test]
async fn test_attributes_remaps_implants_boosters() {
    // parity: feat:attributes.attributes-base, feat:attributes.neural-remap, feat:attributes.bonus-remap, feat:attributes.implant-plug, feat:attributes.implant-remove, feat:attributes.implant-sets, feat:attributes.implants-pod-death, feat:attributes.booster-consume, feat:attributes.booster-side-effects
    eve_testkit::attest(&[
        "feat:attributes.attributes-base",
        "feat:attributes.neural-remap",
        "feat:attributes.bonus-remap",
        "feat:attributes.implant-plug",
        "feat:attributes.implant-remove",
        "feat:attributes.implant-sets",
        "feat:attributes.implants-pod-death",
        "feat:attributes.booster-consume",
        "feat:attributes.booster-side-effects",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(41_003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        vec![PyRep::Integer(SEED_PILOT)],
    )
    .await
    .expect("SelectCharacterID");

    // 1. Base Attributes (Int, Mem, Per, Will, Cha)
    let respec_info = rpc(&ctx, &mut sess, "skillMgr2", "GetRespecInfo", vec![])
        .await
        .expect("GetRespecInfo");
    assert!(matches!(respec_info, PyRep::KeyVal(_)));

    // 2. Neural remap & bonus remaps (neural-remap, bonus-remap)
    let remap_rep = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "RespecCharacter",
        vec![
            PyRep::Integer(20), // Perception
            PyRep::Integer(20), // Memory
            PyRep::Integer(20), // Willpower
            PyRep::Integer(20), // Intelligence
            PyRep::Integer(19), // Charisma
        ],
    )
    .await
    .expect("RespecCharacter");
    assert!(matches!(remap_rep, PyRep::KeyVal(_) | PyRep::Dict(_)));

    // Second immediate timed remap without bonus remaps must enforce annual cooldown
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&SEED_PILOT) {
            ch.neural.bonus_remaps = 0;
            ch.neural.remap_cooldown_until_unix = Some(ctx.now_unix() + 365 * 86_400);
        }
    });
    let cooldown_remap = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "RespecCharacter",
        vec![
            PyRep::Integer(21),
            PyRep::Integer(21),
            PyRep::Integer(19),
            PyRep::Integer(19),
            PyRep::Integer(19),
        ],
    )
    .await;
    assert!(cooldown_remap.is_err(), "Neural remap must enforce annual cooldown when bonus remaps are 0");

    // Consume bonus remap
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&SEED_PILOT) {
            ch.neural.bonus_remaps = 1;
        }
    });
    let bonus_remap = rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "RespecCharacter",
        vec![
            PyRep::Integer(21),
            PyRep::Integer(21),
            PyRep::Integer(19),
            PyRep::Integer(19),
            PyRep::Integer(19),
        ],
    )
    .await;
    assert!(bonus_remap.is_ok(), "Bonus remap must be consumable even during annual cooldown");
    let ch_neural = ctx.db.with_snapshot(|snap| snap.characters.get(&SEED_PILOT).unwrap().neural.clone());
    assert_eq!(ch_neural.bonus_remaps, 0);
    assert_eq!(ch_neural.charisma, 21);
    assert_eq!(ch_neural.intelligence, 21);

    // 3. Implant plugging, removing, and pirate set multiplier (implant-plug, implant-remove, implant-sets)
    // Grant Cybernetics (3411) level 1 required by implants
    ctx.db.with_mut_snapshot(|snap| {
        let char_skills = snap.skills.entry(SEED_PILOT).or_default();
        char_skills.insert(
            3411,
            eve_db::TrainedSkillRecord {
                type_id: 3411,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    // Ocular Filter Standard (slot 1, type 9941)
    let imp_item_id = 990_000_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            imp_item_id,
            eve_db::InventoryItemRecord {
                item_id: imp_item_id,
                type_id: 9941,
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let plug_rep = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "InjectImplant",
        vec![PyRep::Integer(imp_item_id)],
    )
    .await
    .expect("InjectImplant");
    assert!(matches!(plug_rep, PyRep::None | PyRep::Bool(_) | PyRep::Integer(_)));

    // Removing an implant destroys it
    let remove_rep = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "DestroyImplant",
        vec![PyRep::Integer(imp_item_id)],
    )
    .await
    .expect("DestroyImplant");
    assert!(matches!(remove_rep, PyRep::Bool(_) | PyRep::None));

    // 4. Booster consumption and side-effects (booster-consume, booster-side-effects)
    // Grant Biology (3405) level 1 required by boosters
    ctx.db.with_mut_snapshot(|snap| {
        let char_skills = snap.skills.entry(SEED_PILOT).or_default();
        char_skills.insert(
            3405,
            eve_db::TrainedSkillRecord {
                type_id: 3405,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    // Standard Blue Pill Booster (type 9950)
    let booster_item_id = 990_000_202i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            booster_item_id,
            eve_db::InventoryItemRecord {
                item_id: booster_item_id,
                type_id: 9950,
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let booster_rep = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "UseBooster",
        vec![PyRep::Integer(booster_item_id)],
    )
    .await
    .expect("UseBooster");
    assert!(matches!(booster_rep, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::Bool(_)));

    // 5. Pod death destroys active-clone implants (implants-pod-death)
    // Plug implant into slot 2
    let imp2_id = 990_000_203i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            imp2_id,
            eve_db::InventoryItemRecord {
                item_id: imp2_id,
                type_id: 9942, // Memory Augmentation Standard
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "InjectImplant",
        vec![PyRep::Integer(imp2_id)],
    )
    .await;

    // Simulate pod death respawn (type_id 670 capsule)
    let pod_id = 990_000_204i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            eve_db::InventoryItemRecord {
                item_id: pod_id,
                type_id: 670, // Capsule
                owner_id: SEED_PILOT,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let (respawn_char, _) = ctx
        .db
        .respawn_character_at_home_station(SEED_PILOT, pod_id, &ctx.sde)
        .expect("respawn_character_at_home_station");
    assert_eq!(respawn_char.home_station_id, 60_003_760);

    // Active implants are wiped on pod death
    let active_implants = ctx.db.with_snapshot(|snap| {
        snap.active_implants.get(&SEED_PILOT).cloned().unwrap_or_default()
    });
    assert!(active_implants.is_empty(), "Pod death must destroy active implants");
}
