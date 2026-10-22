//! Integration tests for Character & Progression:
//! CH-1: Skill training queue engine
//! CH-2: Skill handling


use crate::common;

use eve_db::{
    CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    SkillQueueEntry, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::{sp_for_skill_level, sp_per_minute};
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_ch1_skill_training_engine_and_caps() {
    // parity: feat:skills.queue-training-engine, feat:skills.queue-pause, feat:skills.skill-levels-sp, feat:skills.alpha-skill-caps, feat:skills.alpha-training-speed, rpc:charUnboundMgr.GetCharacterInfo, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:skills.queue-training-engine", "feat:skills.queue-pause", "feat:skills.skill-levels-sp", "feat:skills.alpha-skill-caps", "feat:skills.alpha-training-speed"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(931);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // 1. Skill Levels SP: SP(L, R) = 250 * R * (sqrt(32))^(L - 1)
    // Rank 1: L1=250, L2=1415, L3=8000, L4=45255, L5=256000
    assert_eq!(sp_for_skill_level(1.0, 1), 250.0);
    assert_eq!(sp_for_skill_level(1.0, 2), 1415.0);
    assert_eq!(sp_for_skill_level(1.0, 3), 8000.0);
    assert_eq!(sp_for_skill_level(1.0, 4), 45255.0);
    assert_eq!(sp_for_skill_level(1.0, 5), 256000.0);

    // Rank 2: L1=500, L5=512000
    assert_eq!(sp_for_skill_level(2.0, 1), 500.0);
    assert_eq!(sp_for_skill_level(2.0, 5), 512000.0);

    // 2. Real-time SP tick calculation (SP/min = Primary + 0.5 * Secondary)
    // Primary = 27, Secondary = 21:
    // Omega (1.0x): 27 + 0.5 * 21 = 37.5 SP/min = 0.625 SP/sec
    // Alpha (0.5x): 0.5 * 37.5 = 18.75 SP/min = 0.3125 SP/sec (Omega 2x training speed)
    let omega_sp_min = sp_per_minute(27.0, 21.0, 1.0);
    assert!((omega_sp_min - 37.5).abs() < 1e-6);
    let alpha_sp_min = sp_per_minute(27.0, 21.0, 0.5);
    assert!((alpha_sp_min - 18.75).abs() < 1e-6);

    let omega_char_id = 92_000_101;
    let alpha_char_id = 92_000_102;

    ctx.db.with_mut_snapshot(|snap| {
        // Omega character (clone_grade = 2)
        snap.characters.insert(
            omega_char_id,
            CharacterRecord {
                character_id: omega_char_id,
                user_id: 1,
                name: "Omega Queue Pilot".into(),
                corporation_id: 1000001,
                alliance_id: None,
                war_faction_id: None,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 11,
                school_id: 17,
                solar_system_id: 30000142,
                constellation_id: 20000020,
                region_id: 10000002,
                station_id: Some(60003760),
                structure_id: None,
                active_ship_id: 992_000_101,
                balance: IskCents::from_isk(100_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 2, // Omega
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1704067200,
                version: 1,
                system_epoch: 1,
                neural: NeuralAttributes {
                    charisma: 20,
                    intelligence: 20,
                    memory: 20,
                    perception: 20,
                    willpower: 20,
                    bonus_remaps: 1,
                    last_remap_unix: None,
                    remap_cooldown_until_unix: None,
                },
                home_station_id: 60003760,
            },
        );

        // Alpha character (clone_grade = 1)
        snap.characters.insert(
            alpha_char_id,
            CharacterRecord {
                character_id: alpha_char_id,
                user_id: 2,
                name: "Alpha Queue Pilot".into(),
                corporation_id: 1000001,
                alliance_id: None,
                war_faction_id: None,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 11,
                school_id: 17,
                solar_system_id: 30000142,
                constellation_id: 20000020,
                region_id: 10000002,
                station_id: Some(60003760),
                structure_id: None,
                active_ship_id: 992_000_102,
                balance: IskCents::from_isk(100_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 1, // Alpha
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1704067200,
                version: 1,
                system_epoch: 1,
                neural: NeuralAttributes {
                    charisma: 20,
                    intelligence: 20,
                    memory: 20,
                    perception: 20,
                    willpower: 20,
                    bonus_remaps: 1,
                    last_remap_unix: None,
                    remap_cooldown_until_unix: None,
                },
                home_station_id: 60003760,
            },
        );

        // Skill 3300 (Gunnery): rank 1, Primary Perception(20), Secondary Willpower(20)
        // Omega: 20 + 10 = 30 SP/min = 0.5 SP/sec
        // Alpha: 0.5 * 30 = 15 SP/min = 0.25 SP/sec
        snap.skill_queues.insert(
            omega_char_id,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 500.0,
                started_at_unix: None,
            }],
        );
        snap.skill_queues.insert(
            alpha_char_id,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 1000.0,
                started_at_unix: None,
            }],
        );
    });

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(omega_char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID omega");

    // Advance 60 seconds virtual time
    ctx.advance_skill_queues(60.0);

    ctx.db.with_snapshot(|snap| {
        let omega_q = &snap.skill_queues[&omega_char_id][0];
        let alpha_q = &snap.skill_queues[&alpha_char_id][0];
        // Omega gained 30 SP (60 * 0.5)
        assert!((omega_q.start_sp - 30.0).abs() < 1e-4);
        // Alpha gained 15 SP (60 * 0.25)
        assert!((alpha_q.start_sp - 15.0).abs() < 1e-4);
        // Omega rate is 2x Alpha rate
        assert!((omega_q.start_sp - 2.0 * alpha_q.start_sp).abs() < 1e-4);
    });

    // 3. Skill level completion event & auto-advance next skill in queue
    ctx.drain_notifications_for_char(omega_char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skill_queues.insert(
            omega_char_id,
            vec![
                SkillQueueEntry {
                    type_id: 3300,
                    target_level: 1,
                    start_sp: 30.0,
                    end_sp: 250.0,
                    duration_seconds: 440.0,
                    started_at_unix: None,
                },
                SkillQueueEntry {
                    type_id: 3301,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 500.0,
                    started_at_unix: None,
                },
            ],
        );
    });

    // Advance 540 seconds (440s to finish 3300 + 100s for 3301)
    ctx.advance_skill_queues(540.0);

    ctx.db.with_snapshot(|snap| {
        let q = &snap.skill_queues[&omega_char_id];
        assert_eq!(q.len(), 1, "First skill should auto-advance upon completion");
        assert_eq!(q[0].type_id, 3301);
        assert!((q[0].start_sp - 50.0).abs() < 1e-4, "Excess time rolls over to next skill");

        let trained = &snap.skills[&omega_char_id];
        assert_eq!(trained.get(&3300).unwrap().level, 1);
        assert_eq!(trained.get(&3300).unwrap().skill_points, 250.0);
    });

    let notifs = ctx.drain_notifications_for_char(omega_char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnSkillTrained"),
        "OnSkillTrained completion event must be emitted"
    );

    // 4. Alpha 5M SP cap
    let alpha_cap_id = 92_000_103;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            alpha_cap_id,
            CharacterRecord {
                character_id: alpha_cap_id,
                user_id: 3,
                name: "Alpha Cap Pilot".into(),
                corporation_id: 1000001,
                alliance_id: None,
                war_faction_id: None,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 11,
                school_id: 17,
                solar_system_id: 30000142,
                constellation_id: 20000020,
                region_id: 10000002,
                station_id: Some(60003760),
                structure_id: None,
                active_ship_id: 992_000_103,
                balance: IskCents::from_isk(100_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 1, // Alpha
                skill_points: 4_999_960.0, // 40 SP remaining until 5M
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1704067200,
                version: 1,
                system_epoch: 1,
                neural: NeuralAttributes {
                    charisma: 20,
                    intelligence: 20,
                    memory: 20,
                    perception: 20,
                    willpower: 20,
                    bonus_remaps: 0,
                    last_remap_unix: None,
                    remap_cooldown_until_unix: None,
                },
                home_station_id: 60003760,
            },
        );
        snap.skill_queues.insert(
            alpha_cap_id,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 5,
                start_sp: 0.0,
                end_sp: 256_000.0,
                duration_seconds: 1_000_000.0,
                started_at_unix: None,
            }],
        );
    });

    // Advance 10,000 seconds
    ctx.advance_skill_queues(10_000.0);

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&alpha_cap_id];
        assert_eq!(ch.skill_points, 5_000_000.0, "SP strictly capped at 5M for Alpha");
        let q = &snap.skill_queues[&alpha_cap_id][0];
        assert_eq!(q.start_sp, 40.0, "Only SP up to 5M cap granted");
    });

    // 4b. Per-skill Alpha level cap from clonegrades.fsdbinary & inactive SP when downgraded (feat:skills.alpha-skill-caps)
    // Caldari Alpha (clone_grade_id=1) has Anchoring (11584) capped at 1 and Cloaking (11579) capped at 0.
    // Give alpha_cap_id trained Anchoring IV, Cloaking IV (from prior Omega state), and Gunnery IV, reset SP to 1,000,000 so 5M cap doesn't trigger first.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&alpha_cap_id) {
            ch.skill_points = 1_000_000.0;
            ch.user_id = 1;
        }
        let skills = snap.skills.entry(alpha_cap_id).or_default();
        skills.insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        skills.insert(
            11584,
            TrainedSkillRecord {
                type_id: 11584,
                level: 4,
                skill_points: 135_765.0,
            },
        );
        skills.insert(
            11579,
            TrainedSkillRecord {
                type_id: 11579,
                level: 4,
                skill_points: 271_530.0,
            },
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(alpha_cap_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID alpha_cap_id");

    // Attempt to queue Science V (3402, no prerequisites, Alpha cap 4 in clonegrades.fsdbinary) -> AlphaCloneSkillLevelCapExceeded
    let alpha_lvl_err = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(3402),
                PyRep::Integer(5),
            ])])],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        alpha_lvl_err.unwrap_err().code,
        "AlphaCloneSkillLevelCapExceeded",
        "Alpha clone must not be able to queue skills above clonegrades.fsdbinary per-skill level cap"
    );

    // Verify inactive SP: GetCharacterInfo reports skillLevel=4, activeSkillLevel=1 for Anchoring (11584)
    // and skillLevel=4, activeSkillLevel=0 for Cloaking (11579) on Alpha clone
    let alpha_info = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "GetCharacterInfo",
            &[PyRep::Integer(alpha_cap_id)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterInfo alpha");
    let PyRep::KeyVal(alpha_kv) = alpha_info else {
        panic!("Expected KeyVal from GetCharacterInfo");
    };
    let PyRep::List(alpha_skills) = alpha_kv.get("skills").expect("skills list") else {
        panic!("Expected skills List");
    };
    let anchoring_entry = alpha_skills
        .iter()
        .find_map(|s| match s {
            PyRep::KeyVal(kv) if kv.get("typeID").and_then(|v| v.as_i64()) == Some(11584) => Some(kv),
            _ => None,
        })
        .expect("Anchoring skill entry in character sheet");
    assert_eq!(anchoring_entry.get("skillLevel").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(
        anchoring_entry.get("activeSkillLevel").and_then(|v| v.as_i64()),
        Some(1),
        "Anchoring trained to IV must have activeSkillLevel=1 on Alpha clone"
    );
    let cloaking_entry = alpha_skills
        .iter()
        .find_map(|s| match s {
            PyRep::KeyVal(kv) if kv.get("typeID").and_then(|v| v.as_i64()) == Some(11579) => Some(kv),
            _ => None,
        })
        .expect("Cloaking skill entry in character sheet");
    assert_eq!(cloaking_entry.get("skillLevel").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(
        cloaking_entry.get("activeSkillLevel").and_then(|v| v.as_i64()),
        Some(0),
        "Omega-only skill trained to IV must have activeSkillLevel=0 (inactive SP) on Alpha clone"
    );

    // Verify that Dogma applies the clamped skill level for Alpha clones (feat:skills.alpha-skill-caps)
    // Sharpshooter (3311) trained to V (+25% optimal range). Caldari/Minmatar Alpha clone cap is IV (+20%).
    let test_pilot_id = 90_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(test_pilot_id).or_default().insert(
            3311,
            TrainedSkillRecord {
                type_id: 3311,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&test_pilot_id) {
            ch.clone_grade = 2; // Omega
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&100_000_001);
    let omega_fit = ctx.get_or_rebuild_ship_fitting(test_pilot_id, 100_000_001).expect("omega fit");
    let omega_ctx = omega_fit.evaluated_context(&ctx.sde);
    let omega_range = omega_ctx
        .ship_located_items
        .iter()
        .find(|i| i.item_id == 100_000_005)
        .expect("fitted 200mm autocannon")
        .get_attribute(54, &ctx.sde);

    // Downgrade to Alpha (clone_grade = 0)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&test_pilot_id) {
            ch.clone_grade = 0; // Alpha
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&100_000_001);
    let alpha_fit = ctx.get_or_rebuild_ship_fitting(test_pilot_id, 100_000_001).expect("alpha fit");
    let alpha_ctx = alpha_fit.evaluated_context(&ctx.sde);
    let alpha_range = alpha_ctx
        .ship_located_items
        .iter()
        .find(|i| i.item_id == 100_000_005)
        .expect("fitted 200mm autocannon")
        .get_attribute(54, &ctx.sde);

    let base_range = ctx.sde.types[&2889].default_attributes[&54];
    assert!(
        (omega_range - base_range * 1.25).abs() < 1e-3,
        "Omega clone with Sharpshooter V must get 25% optimal range bonus: expected {}, got {}",
        base_range * 1.25,
        omega_range
    );
    assert!(
        (alpha_range - base_range * 1.20).abs() < 1e-3,
        "Alpha clone with Sharpshooter V must get Alpha-capped level 4 (20%) optimal range bonus: expected {}, got {}",
        base_range * 1.20,
        alpha_range
    );
    assert!(
        omega_range > alpha_range,
        "Dogma must clamp skill bonuses for Alpha clones"
    );

    // Restore Omega for subsequent tests
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&test_pilot_id) {
            ch.clone_grade = 2;
        }
    });

    // 5. Skill queue pause & resume (feat:skills.queue-pause)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skill_queues.insert(
            omega_char_id,
            vec![SkillQueueEntry {
                type_id: 3302,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 500.0,
                started_at_unix: None,
            }],
        );
    });
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(omega_char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID omega");

    ctx.drain_notifications_for_char(omega_char_id);
    let pause_res = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "AbortTraining",
            &[],
            &kw,
            None,
        )
        .await
        .expect("AbortTraining");
    assert_eq!(pause_res, PyRep::Bool(true));

    let pause_notifs = ctx.drain_notifications_for_char(omega_char_id);
    // The server sends `OnSkillQueuePausedServer()` (no args); the V24 client's
    // `skillQueueSvc.OnSkillQueuePausedServer` derives the client-local `OnSkillQueuePaused`.
    let paused: Vec<_> = pause_notifs
        .iter()
        .filter(|n| n.event == "OnSkillQueuePausedServer")
        .collect();
    assert_eq!(paused.len(), 1, "exactly one OnSkillQueuePausedServer");
    assert!(paused[0].args.is_empty(), "OnSkillQueuePausedServer takes no arguments");
    assert_eq!(paused[0].id_type, "charid");
    assert!(
        pause_notifs.iter().all(|n| n.event != "OnSkillQueuePaused"),
        "client-local OnSkillQueuePaused must not be sent by the server"
    );

    let sp_before = ctx.db.with_snapshot(|s| s.skill_queues[&omega_char_id][0].start_sp);
    ctx.advance_skill_queues(100.0);
    let sp_after = ctx.db.with_snapshot(|s| s.skill_queues[&omega_char_id][0].start_sp);
    assert_eq!(sp_before, sp_after, "Paused queue must not train or advance SP");

    let resume_res = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(3302),
                PyRep::Integer(1),
            ])])],
            &kw,
            None,
        )
        .await
        .expect("SaveSkillQueue");
    assert!(matches!(resume_res, PyRep::KeyVal(_)));
    // `OnNewSkillQueueSaved(newQueue)` carries the saved queue (same rowset as GetSkillQueue);
    // the V24 client's `skillQueueSvc.OnNewSkillQueueSaved` derives `OnSkillQueueChanged`.
    let save_notifs = ctx.drain_notifications_for_char(omega_char_id);
    let saved: Vec<_> = save_notifs
        .iter()
        .filter(|n| n.event == "OnNewSkillQueueSaved")
        .collect();
    assert_eq!(saved.len(), 1, "exactly one OnNewSkillQueueSaved");
    assert_eq!(saved[0].id_type, "charid");
    let queue_now = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillQueue", &[], &kw, None)
        .await
        .expect("GetSkillQueue");
    assert_eq!(saved[0].args, vec![queue_now], "newQueue must equal the saved skill queue");
    assert!(save_notifs.iter().all(|n| n.event != "OnSkillQueueChanged"));

    ctx.advance_skill_queues(100.0);
    let sp_resumed = ctx.db.with_snapshot(|s| s.skill_queues[&omega_char_id][0].start_sp);
    assert!(sp_resumed > sp_after, "Resumed queue advances SP on tick");
}

#[tokio::test]
async fn test_ch2_skill_handling_and_prerequisites() {
    // parity: feat:skills.queue-get, feat:skills.queue-save, feat:skills.skill-history, feat:skills.inject-skillbook, feat:skills.prereq-check, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:skills.queue-get", "feat:skills.queue-save", "feat:skills.skill-history", "feat:skills.inject-skillbook", "feat:skills.prereq-check"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(932);
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

    // 1. GetSkillQueue (feat:skills.queue-get)
    let q_res = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkillQueue", &[], &kw, None)
        .await
        .expect("GetSkillQueue");
    let PyRep::Rowset { columns, .. } = q_res else {
        panic!("Expected Rowset from GetSkillQueue");
    };
    assert_eq!(
        columns,
        vec![
            "trainingTypeID",
            "trainingToLevel",
            "trainingStartSP",
            "trainingDestinationSP",
            "durationSeconds",
        ]
    );

    // 2. SaveSkillQueue: Queue skills with prerequisite checks (feat:skills.queue-save, feat:skills.prereq-check)
    // Small Hybrid Turret (type 3301) requires Gunnery (type 3300) level 1.
    // Try to queue Small Hybrid Turret without Gunnery:
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().remove(&3300);
        snap.skills.entry(char_id).or_default().remove(&3301);
        snap.skill_queues.insert(char_id, Vec::new());
    });

    let prereq_fail = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(3301),
                PyRep::Integer(1),
            ])])],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        prereq_fail.unwrap_err().code,
        "SkillPrerequisiteNotMet",
        "Must enforce prerequisites on SaveSkillQueue"
    );

    // Queue Gunnery 1 first, then Small Hybrid Turret 1 in same queue save:
    let save_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(3300), PyRep::Integer(1)]),
                PyRep::Tuple(vec![PyRep::Integer(3301), PyRep::Integer(1)]),
            ])],
            &kw,
            None,
        )
        .await
        .expect("SaveSkillQueue with satisfied in-queue prerequisite");
    assert!(matches!(save_ok, PyRep::KeyVal(_)));

    ctx.db.with_snapshot(|snap| {
        let q = &snap.skill_queues[&char_id];
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].type_id, 3300);
        assert_eq!(q[1].type_id, 3301);
    });

    // Save empty queue clears and pauses queue
    let empty_save = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![])],
            &kw,
            None,
        )
        .await
        .expect("SaveSkillQueue empty");
    assert_eq!(empty_save, PyRep::List(vec![]));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.skill_queues[&char_id].is_empty());
        assert!(snap.paused_skill_queues.contains(&char_id));
    });

    // 3. Inject skillbook without prerequisites trained (feat:skills.inject-skillbook, feat:skills.prereq-check)
    // Per wiki:Skills_and_learning, any skillbook can be injected without having trained its prerequisites,
    // whereas training/queueing requires the prerequisite skills.
    // Add skillbook item for Small Projectile Turret (type 3302, requires Gunnery 3300 level 1) to hangar
    let book_item_id = 992_000_201;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            book_item_id,
            InventoryItemRecord {
                item_id: book_item_id,
                type_id: 3302,
                owner_id: char_id,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.skills.entry(char_id).or_default().remove(&3300);
        snap.skills.entry(char_id).or_default().remove(&3301);
        snap.skills.entry(char_id).or_default().remove(&3302);
    });

    // Inject succeeds even though prerequisite (Gunnery I) is not yet trained:
    // consumes item and adds skill at level 0 (0 SP)
    let inject_ok = ctx
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
        .expect("InjectSkillIntoBrain succeeds without prerequisites trained");
    assert!(matches!(inject_ok, PyRep::KeyVal(_)));

    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&book_item_id),
            "Skillbook item must be consumed"
        );
        let s = snap.skills[&char_id].get(&3302).expect("Injected skill");
        assert_eq!(s.level, 0);
        assert_eq!(s.skill_points, 0.0);
    });

    // Attempting to queue the injected skill (3302) without its prerequisite (3300 level 1) must fail
    let queue_unprereq_err = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(3302),
                PyRep::Integer(1),
            ])])],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        queue_unprereq_err.unwrap_err().code,
        "SkillPrerequisiteNotMet",
        "Training/queueing an injected skill still requires prerequisites"
    );

    // Complete a skill to trigger eventTypeID=37 (Skill completion)
    ctx.dispatch_rpc(
        &mut sess,
        "skillMgr2",
        None,
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3300),
            PyRep::Integer(1),
        ])])],
        &kw,
        None,
    )
    .await
    .expect("SaveSkillQueue Gunnery I");
    // Advance queue to completion (250 SP completes in <1000s)
    ctx.advance_skill_queues(1000.0);

    // Queue Small Hybrid Turret (3302) now that Gunnery I prerequisite is completed
    ctx.dispatch_rpc(
        &mut sess,
        "skillMgr2",
        None,
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3302),
            PyRep::Integer(1),
        ])])],
        &kw,
        None,
    )
    .await
    .expect("SaveSkillQueue Small Hybrid Turret I");

    // Clear skill queue to trigger eventTypeID=38 (Skill training cancelled / removal)
    ctx.dispatch_rpc(
        &mut sess,
        "skillMgr2",
        None,
        "SaveSkillQueue",
        &[PyRep::List(vec![])],
        &kw,
        None,
    )
    .await
    .expect("SaveSkillQueue clear queue to trigger event 38");

    // 4. GetSkillHistory (feat:skills.skill-history)
    // Verify GetSkillHistory records event log rows with valid filetime logDate:
    // eventTypeID=36 (SkillEventSkillInjected), eventTypeID=37 (Skill completed), eventTypeID=38 (Skill cancelled)
    let history_res = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "GetSkillHistory",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetSkillHistory");
    let PyRep::Rowset {
        columns: hist_cols,
        rows: hist_rows,
    } = history_res
    else {
        panic!("Expected Rowset from GetSkillHistory");
    };
    assert_eq!(
        hist_cols,
        vec![
            "logDate",
            "eventTypeID",
            "skillTypeID",
            "skillLevel",
            "absolutePoints"
        ]
    );

    // Assert event 36 (Skill Injected for 3302)
    assert!(
        hist_rows.iter().any(|r| {
            r.get(1) == Some(&PyRep::Integer(36))
                && r.get(2) == Some(&PyRep::Integer(3302))
                && r.get(3) == Some(&PyRep::Integer(0))
        }),
        "GetSkillHistory must include injected skill event row (eventTypeID=36, skillTypeID=3302)"
    );

    // Assert event 37 (Skill Completed for 3300 level 1)
    assert!(
        hist_rows.iter().any(|r| {
            r.get(1) == Some(&PyRep::Integer(37))
                && r.get(2) == Some(&PyRep::Integer(3300))
                && r.get(3) == Some(&PyRep::Integer(1))
        }),
        "GetSkillHistory must include completed skill event row (eventTypeID=37, skillTypeID=3300)"
    );

    // Assert event 38 (Skill Cancelled / Removal for 3302)
    assert!(
        hist_rows.iter().any(|r| {
            r.get(1) == Some(&PyRep::Integer(38))
                && r.get(2) == Some(&PyRep::Integer(3302))
        }),
        "GetSkillHistory must include cancelled skill event row (eventTypeID=38, skillTypeID=3302)"
    );

    // Assert valid Windows/BlueOS FILETIME logDate (> 116444736000000000, i.e. past year 1970)
    for r in &hist_rows {
        let ft = r.first().and_then(|v| v.as_i64()).expect("logDate i64");
        assert!(
            ft > 116_444_736_000_000_000,
            "logDate {ft} must be a valid BlueOS FILETIME timestamp"
        );
    }

    // Second injection of already known skill fails
    let already_injected = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "InjectSkillIntoBrain",
            &[PyRep::Integer(3302)],
            &kw,
            None,
        )
        .await;
    assert_eq!(already_injected.unwrap_err().code, "SkillAlreadyInjected");
}

#[tokio::test]
async fn test_r66_p1_3_nine_maps_durability_restart_and_rollback() {
    let pid = std::process::id();
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sqlite_path = std::env::temp_dir().join(format!("evomu_server_r66_{}_{}.sqlite", pid, now_nanos));
    let _ = std::fs::remove_file(&sqlite_path);

    let repo = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("open_sqlite DB");

    repo.with_mut_snapshot(|snap| {
        snap.character_last_jump_unix.insert(90_000_001, 1_700_000_100);
        snap.character_cspa_charges.insert(90_000_001, IskCents(250_00));
        snap.mail_timestamps.insert(888_001, 1_700_000_200);
        snap.character_login_stats.insert(
            90_000_001,
            eve_db::CharacterLoginStats {
                last_login_unix: 1_700_000_300,
                last_logout_unix: 1_700_000_400,
                logins_count: 5,
            },
        );
        snap.item_custom_names.insert(100_000_001, "My Battle Rifter".to_string());
        snap.item_positions.insert(100_000_001, [1234.5, 6789.0, -4321.25]);
        snap.structure_vulnerability_windows.insert(970_001, (1_700_000_500, 1_700_004_100));
        snap.skill_history.insert(
            90_000_001,
            vec![eve_db::SkillHistoryEntry {
                log_date_ft: 133_000_000_000,
                event_type_id: 37,
                skill_type_id: 3300,
                skill_level: 5,
                absolute_points: 256_000.0,
            }],
        );
        snap.character_doll_info.insert(
            90_000_001,
            ("dna:caldari:male:123".to_string(), "meta:portrait:v2".to_string()),
        );
    });

    // Rollback test
    let res: Result<(), eve_db::DbError> = repo.try_with_mut_snapshot(|snap| {
        snap.character_last_jump_unix.insert(90_000_001, 9_999_999);
        snap.character_last_jump_unix.insert(90_000_002, 8_888_888);
        snap.character_cspa_charges.insert(90_000_001, IskCents(999));
        snap.mail_timestamps.insert(888_001, 9_999_999);
        snap.character_login_stats.get_mut(&90_000_001).unwrap().logins_count = 999;
        snap.item_custom_names.insert(100_000_001, "Corrupted Name".to_string());
        snap.item_positions.insert(100_000_001, [0.0, 0.0, 0.0]);
        snap.structure_vulnerability_windows.insert(970_001, (0, 0));
        snap.skill_history.get_mut(&90_000_001).unwrap().clear();
        snap.character_doll_info.insert(
            90_000_001,
            ("corrupted".to_string(), "corrupted".to_string()),
        );
        Err(eve_db::DbError::ValidationError("trigger rollback".into()))
    });
    assert!(res.is_err());

    // Verify rollback restored committed state
    repo.with_snapshot(|snap| {
        assert_eq!(snap.character_last_jump_unix.get(&90_000_001), Some(&1_700_000_100));
        assert!(!snap.character_last_jump_unix.contains_key(&90_000_002));
        assert_eq!(snap.character_cspa_charges.get(&90_000_001), Some(&IskCents(250_00)));
        assert_eq!(snap.mail_timestamps.get(&888_001), Some(&1_700_000_200));
        let stats = snap.character_login_stats.get(&90_000_001).unwrap();
        assert_eq!(stats.logins_count, 5);
        assert_eq!(snap.item_custom_names.get(&100_000_001).unwrap(), "My Battle Rifter");
        assert_eq!(snap.item_positions.get(&100_000_001), Some(&[1234.5, 6789.0, -4321.25]));
        assert_eq!(
            snap.structure_vulnerability_windows.get(&970_001),
            Some(&(1_700_000_500, 1_700_004_100))
        );
        assert_eq!(snap.skill_history.get(&90_000_001).unwrap().len(), 1);
        assert_eq!(
            snap.character_doll_info.get(&90_000_001).unwrap(),
            &("dna:caldari:male:123".to_string(), "meta:portrait:v2".to_string())
        );
    });

    drop(repo);

    // Reopen from SQLite and verify restart survival
    let restarted = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("reopen SQLite DB");
    restarted.with_snapshot(|snap| {
        assert_eq!(snap.character_last_jump_unix.get(&90_000_001), Some(&1_700_000_100));
        assert!(!snap.character_last_jump_unix.contains_key(&90_000_002));
        assert_eq!(snap.character_cspa_charges.get(&90_000_001), Some(&IskCents(250_00)));
        assert_eq!(snap.mail_timestamps.get(&888_001), Some(&1_700_000_200));
        let stats = snap.character_login_stats.get(&90_000_001).unwrap();
        assert_eq!(stats.logins_count, 5);
        assert_eq!(stats.last_login_unix, 1_700_000_300);
        assert_eq!(stats.last_logout_unix, 1_700_000_400);
        assert_eq!(snap.item_custom_names.get(&100_000_001).unwrap(), "My Battle Rifter");
        assert_eq!(snap.item_positions.get(&100_000_001), Some(&[1234.5, 6789.0, -4321.25]));
        assert_eq!(
            snap.structure_vulnerability_windows.get(&970_001),
            Some(&(1_700_000_500, 1_700_004_100))
        );
        let hist = snap.skill_history.get(&90_000_001).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].skill_type_id, 3300);
        assert_eq!(hist[0].skill_level, 5);
        assert_eq!(
            snap.character_doll_info.get(&90_000_001).unwrap(),
            &("dna:caldari:male:123".to_string(), "meta:portrait:v2".to_string())
        );
    });

    let _ = std::fs::remove_file(&sqlite_path);
    let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-shm"));
}

#[tokio::test]
async fn test_ch1_ch2_effective_attributes_catchup_and_2arg_free_sp() {
    // parity: feat:skills.skill-catch-up, feat:attributes.attributes-base, feat:skills.free-sp, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:skills.skill-catch-up", "feat:attributes.attributes-base", "feat:skills.free-sp"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(940);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;

    // 1. Plug in an attribute implant (+4 Perception: Ocular Filter - Standard 9899 or 9941) and a Cerebral Accelerator booster (+6 all attributes)
    let booster_item_id = 990_000_801_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.neural = NeuralAttributes {
                charisma: 19,
                intelligence: 20,
                memory: 20,
                perception: 20,
                willpower: 20,
                bonus_remaps: 2,
                last_remap_unix: None,
                remap_cooldown_until_unix: None,
            };
            ch.free_skill_points = 50_000.0;
        }
        snap.active_implants.insert(char_id, vec![9899]);
        let mut booster_attrs = HashMap::new();
        booster_attrs.insert(175, 6.0);
        booster_attrs.insert(176, 6.0);
        booster_attrs.insert(177, 6.0);
        booster_attrs.insert(178, 6.0);
        booster_attrs.insert(179, 6.0);
        snap.items.insert(
            booster_item_id,
            InventoryItemRecord {
                item_id: booster_item_id,
                type_id: 45998,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: booster_attrs,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

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

    // Verify GetSkillQueueAndFreePoints returns queue and unallocated free SP
    let q_and_free_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "GetSkillQueueAndFreePoints",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetSkillQueueAndFreePoints");
    let PyRep::Tuple(q_and_free_tup) = q_and_free_rep else {
        panic!("Expected Tuple from GetSkillQueueAndFreePoints");
    };
    assert_eq!(q_and_free_tup.get(1).and_then(|v| v.as_f64()), Some(50_000.0));

    // Verify GetCharacterInfo reports effective attributes (base + implant + booster)
    let info_rep = ctx
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
    let PyRep::KeyVal(info_kv) = info_rep else {
        panic!("Expected KeyVal from GetCharacterInfo");
    };
    let PyRep::KeyVal(attrs_kv) = info_kv.get("attributes").expect("attributes") else {
        panic!("Expected attributes KeyVal");
    };
    assert_eq!(
        attrs_kv.get("baseCharisma").and_then(|v| v.as_i64()),
        Some(19)
    );
    assert_eq!(
        attrs_kv.get("charisma").and_then(|v| v.as_i64()),
        Some(25),
        "Effective charisma must include +6 cerebral accelerator bonus"
    );

    // 2. Offline skill catch-up on login / SelectCharacterID (feat:skills.skill-catch-up)
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.paused_skill_queues.remove(&char_id);
        snap.skills.entry(char_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![
                SkillQueueEntry {
                    type_id: 3300, // Gunnery I (250 SP)
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 500.0,
                    started_at_unix: Some(now_unix.saturating_sub(1_200)),
                },
                SkillQueueEntry {
                    type_id: 3300, // Gunnery II (1415 SP)
                    target_level: 2,
                    start_sp: 250.0,
                    end_sp: 1415.0,
                    duration_seconds: 2330.0,
                    started_at_unix: None,
                },
            ],
        );
    });

    // Trigger login catch-up via SelectCharacterID
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
    .expect("SelectCharacterID catch-up");

    ctx.db.with_snapshot(|snap| {
        let g = snap.skills[&char_id].get(&3300).expect("Gunnery trained");
        assert!(
            g.level >= 1 && g.skill_points > 250.0,
            "Offline catch-up must complete Gunnery I and advance into Gunnery II"
        );
        let q = &snap.skill_queues[&char_id];
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].target_level, 2);
        assert!(q[0].start_sp > 250.0);
    });

    // 3. V24 2-arg ApplyFreeSkillPoints(skill_type_id, points) + GetRespecInfo (feat:skills.free-sp)
    let apply_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(3300), PyRep::Real(1_500.0)],
            &kw,
            None,
        )
        .await
        .expect("ApplyFreeSkillPoints(3300, 1500.0)");
    let new_gunnery_sp = apply_rep.as_f64().expect("Real new skill SP");
    assert!(
        new_gunnery_sp >= 1_415.0,
        "Applying 1,500 free SP to Gunnery must reach at least level II (1,415 SP)"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.skills[&char_id][&3300].level, 2);
        assert!(snap.skill_queues[&char_id].is_empty());
    });

    let respec_rep = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetRespecInfo", &[], &kw, None)
        .await
        .expect("GetRespecInfo");
    let PyRep::KeyVal(respec_kv) = respec_rep else {
        panic!("Expected KeyVal from GetRespecInfo");
    };
    assert_eq!(
        respec_kv.get("freeRespecs").and_then(|v| v.as_i64()),
        Some(2)
    );
}

