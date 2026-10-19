//! Integration tests for Character & Progression:
//! CH-1: Skill training queue engine
//! CH-2: Skill handling
//! CH-3: Character creation
//! CH-4: Character sheet
//! CH-5: GM command dispatcher
//! S-4: Jump clones
// parity: plan:CH-1, plan:CH-2, plan:CH-3, plan:CH-4, plan:CH-5, plan:S-4
// parity: feat:skills.queue-training-engine, feat:skills.queue-pause, feat:skills.skill-levels-sp, feat:skills.alpha-skill-caps, feat:skills.alpha-training-speed, feat:skills.queue-get, feat:skills.queue-save, feat:skills.skill-history, feat:skills.inject-skillbook, feat:skills.prereq-check, feat:character.list-characters, feat:character.select-character, feat:character.create-character, feat:character.show-info-public, feat:character.biography, feat:character.employment-history, feat:character.owner-notes, feat:character.character-sheet, feat:admin.slash-dispatcher, feat:admin.gm-giveskill, feat:admin.gm-giveisk, feat:admin.gm-createitem, feat:admin.gm-tr, feat:clones.jump-clone-install, feat:clones.jump-clone-jump, feat:clones.jump-clone-destroy

#![forbid(unsafe_code)]

mod common;

use eve_db::{
    CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    SkillQueueEntry, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::{clone_jump_cooldown_seconds, sp_for_skill_level, sp_per_minute};
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_ch1_skill_training_engine_and_caps() {
    // parity: plan:CH-1
    // parity: feat:skills.queue-training-engine, feat:skills.queue-pause, feat:skills.skill-levels-sp, feat:skills.alpha-skill-caps, feat:skills.alpha-training-speed
    let ctx = ServerContext::new();
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
    assert!(
        pause_notifs.iter().any(|n| n.event == "OnSkillQueuePaused"),
        "OnSkillQueuePaused notification expected"
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

    ctx.advance_skill_queues(100.0);
    let sp_resumed = ctx.db.with_snapshot(|s| s.skill_queues[&omega_char_id][0].start_sp);
    assert!(sp_resumed > sp_after, "Resumed queue advances SP on tick");
}

#[tokio::test]
async fn test_ch2_skill_handling_and_prerequisites() {
    // parity: plan:CH-2
    // parity: feat:skills.queue-get, feat:skills.queue-save, feat:skills.skill-history, feat:skills.inject-skillbook, feat:skills.prereq-check
    let ctx = ServerContext::new();
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
async fn test_ch3_character_creation_and_starter_assets() {
    // parity: plan:CH-3
    // parity: feat:character.list-characters, feat:character.select-character, feat:character.create-character
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
    // parity: plan:CH-4
    // parity: feat:character.show-info-public, feat:character.biography, feat:character.employment-history, feat:character.owner-notes, feat:character.character-sheet
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
async fn test_ch5_gm_command_dispatcher() {
    // parity: plan:CH-5
    // parity: feat:admin.slash-dispatcher, feat:admin.gm-giveskill, feat:admin.gm-giveisk, feat:admin.gm-createitem, feat:admin.gm-tr
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Regular player session rejected on slash commands (feat:admin.slash-dispatcher)
    let mut player_sess = SessionState::new(935);
    common::login_pilot(&ctx, &mut player_sess, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut player_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID player");

    let slash_err = ctx
        .dispatch_rpc(
            &mut player_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 1000".into())],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        slash_err.unwrap_err().code,
        "InsufficientRole",
        "Non-GM player session must be rejected on slash commands"
    );

    // 2. GM session authorized (role = ROLE_ADMIN | ROLE_GMS)
    let mut gm_sess = SessionState::new(936);
    common::login_pilot(&ctx, &mut gm_sess, "demo", "demo-secret").await;
    gm_sess.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMS;

    let gm_char_id = 90_000_001;
    ctx.dispatch_rpc(
        &mut gm_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(gm_char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID gm");

    // 3. /giveisk <amount> (feat:admin.gm-giveisk)
    let isk_before = ctx.db.with_snapshot(|s| s.characters[&gm_char_id].balance.to_wire_f64());
    let giveisk_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 500000".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveisk 500000");
    let PyRep::KeyVal(isk_kv) = giveisk_rep else {
        panic!("Expected KeyVal from /giveisk");
    };
    let new_bal = isk_kv.get("newBalanceISK").unwrap().as_f64().unwrap();
    assert_eq!(new_bal, isk_before + 500_000.0);

    // 4. /giveskill <id> <lvl> (feat:admin.gm-giveskill)
    let giveskill_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveskill 3300 4".into())],
            &kw,
            None,
        )
        .await
        .expect("/giveskill 3300 4");
    let PyRep::KeyVal(skill_kv) = giveskill_rep else {
        panic!("Expected KeyVal from /giveskill");
    };
    assert_eq!(skill_kv.get("level").unwrap().as_i64(), Some(4));
    assert_eq!(skill_kv.get("skillID").unwrap().as_i64(), Some(3300));
    ctx.db.with_snapshot(|s| {
        let rec = s.skills[&gm_char_id].get(&3300).expect("Trained skill 3300");
        assert_eq!(rec.level, 4);
    });

    // 5. /createitem <type_id> <qty> (feat:admin.gm-createitem)
    let createitem_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/createitem 601 2".into())],
            &kw,
            None,
        )
        .await
        .expect("/createitem 601 2");
    let PyRep::KeyVal(item_kv) = createitem_rep else {
        panic!("Expected KeyVal from /createitem");
    };
    let item_id = item_kv.get("itemID").unwrap().as_i64().unwrap();
    ctx.db.with_snapshot(|s| {
        let it = s.items.get(&item_id).expect("Created item");
        assert_eq!(it.type_id, 601);
        assert_eq!(it.owner_id, gm_char_id);
    });

    // 6. /tr <system_id> (feat:admin.gm-tr)
    // Teleport from Jita (30000142) to Amamake (30002537)
    let tr_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/tr 30002537".into())],
            &kw,
            None,
        )
        .await
        .expect("/tr 30002537");
    let PyRep::KeyVal(tr_kv) = tr_rep else {
        panic!("Expected KeyVal from /tr");
    };
    assert_eq!(
        tr_kv.get("solarSystemID").unwrap().as_i64(),
        Some(30002537)
    );
    assert_eq!(gm_sess.solar_system_id, Some(30002537));

    // 7. /heal
    let heal_rep = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/heal".into())],
            &kw,
            None,
        )
        .await
        .expect("/heal");
    let PyRep::KeyVal(heal_kv) = heal_rep else {
        panic!("Expected KeyVal from /heal");
    };
    assert_eq!(heal_kv.get("healed").unwrap().as_bool(), Some(true));
}

#[tokio::test]
async fn test_s4_jump_clones_implants_and_cooldown() {
    // parity: plan:S-4
    // parity: feat:clones.jump-clone-install, feat:clones.jump-clone-jump, feat:clones.jump-clone-destroy
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

    // Station with clone bay (Jita 4-4, 60003760), Infomorph Psychology (24242), and Infomorph Synchronizing (33399)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_003_760);
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

    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());

    // Install jump clone in NPC station (feat:clones.jump-clone-install)
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

    // Verify MaxJumpClonesReached when Infomorph Psychology I (1 slot) is full
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
