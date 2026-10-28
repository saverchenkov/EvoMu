//! Character progression, training queues, jump clones, GM commands, neural remap and inventory action integration tests.


use crate::common;

use eve_db::{
    CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    NeuralAttributes, SkillQueueEntry, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::{sp_for_skill_level, sp_per_minute};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

pub(crate) fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code");
    bundle.access_token
}

#[tokio::test]
async fn test_ch1_skill_training_queue_engine() {
    // parity: rpc:charUnboundMgr.SelectCharacterID
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(919);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // 1. SP/min calculation formula verification: SP/min = (Primary + 0.5 * Secondary) * multiplier
    // Perception = 27, Willpower = 21:
    // Omega (1.0x): 27.0 + 0.5 * 21.0 = 37.5 SP/min = 0.625 SP/sec
    // Alpha (0.5x): 0.5 * 37.5 = 18.75 SP/min = 0.3125 SP/sec
    let omega_rate = sp_per_minute(27.0, 21.0, 1.0);
    assert!((omega_rate - 37.5).abs() < 1e-6);
    let alpha_rate = sp_per_minute(27.0, 21.0, 0.5);
    assert!((alpha_rate - 18.75).abs() < 1e-6);

    // 2. Real-time tick advance & Omega 2x vs Alpha 1x rate
    let omega_char_id = 91_000_101;
    let alpha_char_id = 91_000_102;

    ctx.db.with_mut_snapshot(|snap| {
        // Create an Omega character (clone_grade = 2) belonging to demo account (user_id = 1)
        snap.characters.insert(
            omega_char_id,
            CharacterRecord {
                character_id: omega_char_id,
                user_id: 1,
                name: "Omega Tester".into(),
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
                active_ship_id: 991_000_101,
                balance: IskCents::from_isk(100_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 2, // Omega
                skill_points: 0.0,
                activity_status: None,
            free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 0,
                fleet_id: None, autopilot_waypoints: Vec::new(),
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

        // Create an Alpha character (clone_grade = 1)
        snap.characters.insert(
            alpha_char_id,
            CharacterRecord {
                character_id: alpha_char_id,
                user_id: 2,
                name: "Alpha Tester".into(),
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
                active_ship_id: 991_000_102,
                balance: IskCents::from_isk(100_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 1, // Alpha
                skill_points: 0.0,
                activity_status: None,
            free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 0,
                fleet_id: None, autopilot_waypoints: Vec::new(),
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

        // Skill 3300 (Gunnery): rank 1, primary perception(167), secondary willpower(168).
        // For 20 Per / 20 Wil:
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

    // Select omega character so session is active and character is online
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

    // Advance 60 seconds of virtual time
    ctx.advance_skill_queues(60.0);

    ctx.db.with_snapshot(|snap| {
        let omega_q = &snap.skill_queues[&omega_char_id][0];
        let alpha_q = &snap.skill_queues[&alpha_char_id][0];
        // Omega gained 30.0 SP (60 * 0.5)
        assert!((omega_q.start_sp - 30.0).abs() < 1e-4);
        // Alpha gained 15.0 SP (60 * 0.25)
        assert!((alpha_q.start_sp - 15.0).abs() < 1e-4);
        // Omega 2x multiplier verified
        assert!((omega_q.start_sp - 2.0 * alpha_q.start_sp).abs() < 1e-4);
    });

    // 3. Skill level completion event (notify_skill_trained) & auto-advancing queue
    ctx.drain_notifications_for_char(omega_char_id);
    ctx.db.with_mut_snapshot(|snap| {
        // Queue two skills:
        // Skill 1: Gunnery (3300) lvl 1, 30.0 -> 250.0 (needs 220 SP = 440 seconds)
        // Skill 2: Small Hybrid Turret (3301) lvl 1, 0.0 -> 250.0 (needs 250 SP = 500 seconds)
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

    // Advance 540.0 seconds (440s to finish skill 3300 + 100s for skill 3301)
    ctx.advance_skill_queues(540.0);

    ctx.db.with_snapshot(|snap| {
        // Skill 3300 completed and removed from queue
        let q = &snap.skill_queues[&omega_char_id];
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].type_id, 3301);
        // Remaining 100 seconds applied to Skill 3301 (100 * 0.5 = 50 SP)
        assert!((q[0].start_sp - 50.0).abs() < 1e-4);

        // Character trained skills map has skill 3300 at level 1 with 250 SP
        let trained = &snap.skills[&omega_char_id];
        assert_eq!(trained.get(&3300).unwrap().level, 1);
        assert_eq!(trained.get(&3300).unwrap().skill_points, 250.0);
    });

    // Completion notification OnSkillTrained emitted
    let notifs = ctx.drain_notifications_for_char(omega_char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnSkillTrained"),
        "Expected OnSkillTrained notification"
    );

    // 4. Alpha 5M SP cap
    let alpha_cap_id = 91_000_103;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            alpha_cap_id,
            CharacterRecord {
                character_id: alpha_cap_id,
                user_id: 3,
                name: "Alpha Cap Tester".into(),
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
                active_ship_id: 991_000_103,
                balance: IskCents::from_isk(100_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 1, // Alpha
                skill_points: 4_999_960.0, // 40 SP away from 5,000,000 SP cap
                activity_status: None,
            free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 0,
                fleet_id: None, autopilot_waypoints: Vec::new(),
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

    // Advance 10,000 seconds (which would normally grant 2,500 SP)
    ctx.advance_skill_queues(10_000.0);

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&alpha_cap_id];
        // Total SP is strictly capped at 5,000,000 SP
        assert_eq!(ch.skill_points, 5_000_000.0);
        let q = &snap.skill_queues[&alpha_cap_id][0];
        // Only 40.0 SP was granted
        assert_eq!(q.start_sp, 40.0);
    });

    // Advance further: no more SP granted to Alpha character at or beyond 5M
    ctx.advance_skill_queues(1_000.0);
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&alpha_cap_id];
        assert_eq!(ch.skill_points, 5_000_000.0);
        let q = &snap.skill_queues[&alpha_cap_id][0];
        assert_eq!(q.start_sp, 40.0);
    });
}

#[tokio::test]
async fn test_ch2_character_creation_and_starter_assets() {
    // parity: rpc:charUnboundMgr.CreateCharacterWithDoll
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(920);

    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // Call charUnboundMgr.CreateCharacterWithDoll with V24 7-argument signature:
    // (charactername, bloodlineID=1 Deteis, genderID=1 Male, ancestryID=11 Scientists, charInfo, portraitInfo, schoolID=17 STI)
    let new_char_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Caldari Cadet".into()),
                PyRep::Integer(1),  // bloodlineID = 1 (Deteis -> Caldari race 1)
                PyRep::Integer(1),  // genderID = 1 (Male)
                PyRep::Integer(11), // ancestryID = 11 (Scientists)
                PyRep::String("doll:deteis-cadet".into()),
                PyRep::String("portrait:deteis-cadet".into()),
                PyRep::Integer(17), // schoolID = 17 (Science and Trade Institute)
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateCharacterWithDoll");

    let new_char_id = new_char_rep.as_i64().expect("character ID i64");
    assert!(new_char_id > 0);

    ctx.db.with_snapshot(|snap| {
        let ch = snap
            .characters
            .get(&new_char_id)
            .expect("character record exists");
        assert_eq!(ch.race_id, 1);
        assert_eq!(ch.bloodline_id, 1);

        // Starting wallet: 5,000 ISK
        assert_eq!(ch.balance, IskCents::from_isk(5_000));
        assert_eq!(ch.balance.to_wire_f64(), 5_000.0);

        // Starter skills populated in snap.skills with calculated SP
        let char_skills = snap
            .skills
            .get(&new_char_id)
            .expect("starter skills map exists");
        assert!(
            !char_skills.is_empty(),
            "Starter skills should be populated for race"
        );
        assert!(
            char_skills.len() > 10,
            "Caldari race should have dozens of starter skills"
        );
        for (&skill_id, record) in char_skills {
            assert!(record.level > 0);
            assert!(record.skill_points > 0.0);
            let expected_sp = sp_for_skill_level(
                ctx.sde
                    .types
                    .get(&skill_id)
                    .and_then(|t| {
                        t.default_attributes
                            .get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT)
                            .copied()
                    })
                    .unwrap_or(1.0),
                record.level,
            );
            assert!((record.skill_points - expected_sp).abs() < 1e-3);
        }
        assert!(
            ch.skill_points > 100_000.0,
            "Total starter SP should be populated"
        );

        // Rookie ship (corvette, type 601 for Caldari) in home station hangar
        let home_station = ch.station_id.expect("home station id");
        let rookie_ship = snap.items.values().find(|i| {
            i.owner_id == new_char_id
                && i.location_id == home_station
                && i.flag == InventoryFlag::Hangar
                && i.type_id == 601
        });
        assert!(
            rookie_ship.is_some(),
            "Starting rookie corvette must be present in station hangar"
        );
        let ship_item = rookie_ship.unwrap();
        assert!(ship_item.singleton, "Rookie ship must be a singleton item");
        assert_eq!(ship_item.quantity, 1);
    });
}

#[tokio::test]
async fn test_ch3_jump_clones_implants_and_pod_death() {
    // parity: rpc:charUnboundMgr.SelectCharacterID, rpc:jumpCloneSvc.CloneJump, rpc:jumpCloneSvc.InstallCloneInStation
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(921);

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

    // Dock pilot at station with clone bay (60003760), give 1,000,000 ISK and Infomorph Psychology (24242)
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(60003760);
        ch.balance = IskCents::from_isk(1_000_000);
        snap.skills.entry(char_id).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 2,
                skill_points: 1414.0,
            },
        );
        snap.active_implants.insert(char_id, vec![9899]); // Snake Alpha
    });

    // 1. Install clone in station with 900,000 ISK fee
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
    let PyRep::KeyVal(kv) = install_rep else {
        panic!("Expected KeyVal from InstallCloneInStation");
    };
    let jc_id = kv.get("jumpCloneID").unwrap().as_i64().unwrap();

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        // 900,000 ISK deducted from starting 1,000,000 ISK
        assert_eq!(ch.balance, IskCents::from_isk(100_000));
        let clones = &snap.jump_clones[&char_id];
        let c = clones.iter().find(|c| c.jump_clone_id == jc_id).unwrap();
        assert_eq!(c.location_id, 60003760);
    });

    // 2. Clone jump pod swap: active_implants swapped with jump clone implants
    // Relocate jump clone to another station (60008494) with implant 9941 (Halo Alpha)
    let pod_id = 99_000_400;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(pod_id, InventoryItemRecord { item_id: pod_id, type_id: 670, owner_id: char_id, location_id: 60003760, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None, insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None });
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.active_ship_id = pod_id;
        ch.balance.0 += 1_000_000_00;
        let clones = snap.jump_clones.get_mut(&char_id).unwrap();
        let c = clones.iter_mut().find(|c| c.jump_clone_id == jc_id).unwrap();
        c.location_id = 60008494;
        c.implants = vec![9941];
    });
    sess.ship_id = Some(pod_id);

    ctx.advance_secs(12);
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
        jump_kv.get("activeImplants"),
        Some(&PyRep::List(vec![PyRep::Integer(9941)]))
    );

    ctx.db.with_snapshot(|snap| {
        // Active implants now has 9941
        assert_eq!(snap.active_implants.get(&char_id), Some(&vec![9941]));
        // The clone left behind at 60003760 now has 9899
        let left_behind = snap.jump_clones[&char_id]
            .iter()
            .find(|c| c.jump_clone_id == jc_id)
            .unwrap();
        assert_eq!(left_behind.location_id, 60003760);
        assert_eq!(left_behind.implants, vec![9899]);
    });

    // 24h cooldown active (advance past 10s session-change timer)
    ctx.advance_secs(12);
    let cooldown_err = ctx
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
    assert_eq!(cooldown_err.unwrap_err().code, "CloneJumpCooldownActive");

    // 3. Implant loss on pod death
    let normal_ship_id = 991_000_150;
    let capsule_id = 991_000_151;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            normal_ship_id,
            InventoryItemRecord {
                item_id: normal_ship_id,
                type_id: 587, // Rifter (Category 6, Group 25, ship)
                owner_id: char_id,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
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
        snap.items.insert(
            capsule_id,
            InventoryItemRecord {
                item_id: capsule_id,
                type_id: 670, // Capsule (pod)
                owner_id: char_id,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
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
    });

    // Ship loss (type 587): active implants preserved
    let _ = ctx.db.respawn_character_at_home_station(char_id, normal_ship_id, &ctx.sde);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_implants.get(&char_id), Some(&vec![9941]));
    });

    // Pod death (type 670): active implants wiped
    let _ = ctx.db.respawn_character_at_home_station(char_id, capsule_id, &ctx.sde);
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.active_implants.get(&char_id).map(|v| v.is_empty()).unwrap_or(true),
            "Active implants must be wiped on pod death"
        );
    });
}

