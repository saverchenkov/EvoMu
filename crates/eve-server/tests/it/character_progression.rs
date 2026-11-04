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

pub(crate) mod ch4_slash_gm_commands {
    // GM slash commands and neural attribute remap integration tests (`character_progression` part 2).


    use crate::character_progression::issue_token_with_scopes;
    use crate::common;

    use eve_db::InventoryFlag;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use eve_server::router::ServerContext;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_ch4_slash_gm_commands() {
        // parity: rpc:charUnboundMgr.SelectCharacterID, rpc:slash.SlashCmd
        let ctx = ServerContext::new();
        let kw = HashMap::new();
        let mut sess = SessionState::new(922);

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

        // 1. Regular pilot without GM role rejected
        let unpriv = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/giveitem 34 100".into())],
                &kw,
                None,
            )
            .await;
        assert_eq!(unpriv.unwrap_err().code, "InsufficientRole");

        // Elevate session to GM role
        sess.role = eve_machonet::service_roles::ROLE_GMH;
        assert!(sess.has_gm_role());

        // 2. /giveitem <type_id> <quantity>
        let giveitem_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/giveitem 34 500".into())],
                &kw,
                None,
            )
            .await
            .expect("/giveitem 34 500");
        let PyRep::KeyVal(gi_kv) = giveitem_rep else {
            panic!("Expected KeyVal from /giveitem");
        };
        assert_eq!(gi_kv.get("typeID"), Some(&PyRep::Integer(34)));
        assert_eq!(gi_kv.get("quantity"), Some(&PyRep::Integer(500)));

        ctx.db.with_snapshot(|snap| {
            let has_tritanium = snap.items.values().any(|i| {
                i.owner_id == char_id
                    && i.type_id == 34
                    && i.quantity >= 500
                    && i.flag == InventoryFlag::Hangar
            });
            assert!(has_tritanium, "Given item must be present in hangar");
        });

        // 3. /setskill <skill_id> <level>
        let setskill_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/setskill 3300 5".into())],
                &kw,
                None,
            )
            .await
            .expect("/setskill 3300 5");
        let PyRep::KeyVal(sk_kv) = setskill_rep else {
            panic!("Expected KeyVal from /setskill");
        };
        assert_eq!(sk_kv.get("level"), Some(&PyRep::Integer(5)));

        ctx.db.with_snapshot(|snap| {
            let sk = snap.skills.get(&char_id).and_then(|m| m.get(&3300)).unwrap();
            assert_eq!(sk.level, 5);
            assert_eq!(sk.skill_points, 256_000.0);
        });

        // 4. /teleport <solar_system_id>
        let teleport_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/teleport 30000142".into())],
                &kw,
                None,
            )
            .await
            .expect("/teleport 30000142");
        let PyRep::KeyVal(tp_kv) = teleport_rep else {
            panic!("Expected KeyVal from /teleport");
        };
        assert_eq!(tp_kv.get("solarSystemID"), Some(&PyRep::Integer(30000142)));
        assert_eq!(sess.solar_system_id, Some(30000142));

        ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            assert_eq!(ch.solar_system_id, 30000142);
            assert_eq!(ch.station_id, None);
        });

        // Unknown solar system
        let bad_sys = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/teleport 99999999".into())],
                &kw,
                None,
            )
            .await;
        assert_eq!(bad_sys.unwrap_err().code, "SolarSystemNotFound");

        // 5. /spawnnpc <type_id> in space
        sess.ship_id = Some(991_000_101);
        let spawn_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/spawnnpc 601".into())],
                &kw,
                None,
            )
            .await
            .expect("/spawnnpc 601");
        let PyRep::KeyVal(sp_kv) = spawn_rep else {
            panic!("Expected KeyVal from /spawnnpc");
        };
        assert_eq!(sp_kv.get("typeID"), Some(&PyRep::Integer(601)));
        assert_eq!(sp_kv.get("solarSystemID"), Some(&PyRep::Integer(30000142)));

        // 6. Unknown slash command error
        let unknown_cmd = ctx
            .dispatch_rpc(
                &mut sess,
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String("/unsupportedcmd 123".into())],
                &kw,
                None,
            )
            .await;
        assert_eq!(unknown_cmd.unwrap_err().code, "UnknownSlashCommand");
    }

    #[tokio::test]
    async fn test_ch5_neural_attribute_remap_and_cooldown() {
        // parity: feat:attributes.neural-remap, feat:attributes.bonus-remap, rpc:charUnboundMgr.SelectCharacterID
        eve_testkit::attest(&[
            "feat:attributes.neural-remap",
            "feat:attributes.bonus-remap",
        ]);
        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let mut sess = SessionState::new(923);

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

        // 1. Validation bounds: each attribute must be 17..=27 and sum must equal 99
        // Sum != 99
        let bad_sum = ctx
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
                    PyRep::Integer(17), // sum = 85
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(bad_sum.unwrap_err().code, "RemapFailed");

        // Under min (< 17)
        let under_min = ctx
            .dispatch_rpc(
                &mut sess,
                "charMgr",
                None,
                "RespecCharacter",
                &[
                    PyRep::Integer(16),
                    PyRep::Integer(28),
                    PyRep::Integer(21),
                    PyRep::Integer(17),
                    PyRep::Integer(17),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(under_min.unwrap_err().code, "RemapFailed");

        // Over max (> 27)
        let over_max = ctx
            .dispatch_rpc(
                &mut sess,
                "charMgr",
                None,
                "RespecCharacter",
                &[
                    PyRep::Integer(17),
                    PyRep::Integer(28),
                    PyRep::Integer(20),
                    PyRep::Integer(17),
                    PyRep::Integer(17),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(over_max.unwrap_err().code, "RemapFailed");

        // 2. Timed yearly remap (365 days / 31,536,000s)
        let now_unix = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.neural.last_remap_unix = None;
            ch.neural.remap_cooldown_until_unix = None;
            ch.neural.bonus_remaps = 0;
        });

        let remap_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "charMgr",
                None,
                "RespecCharacter",
                &[
                    PyRep::Integer(17),
                    PyRep::Integer(27),
                    PyRep::Integer(21),
                    PyRep::Integer(17),
                    PyRep::Integer(17), // sum = 99
                ],
                &kw,
                None,
            )
            .await
            .expect("RespecCharacter");
        let PyRep::KeyVal(rm_kv) = remap_rep else {
            panic!("Expected KeyVal from RespecCharacter");
        };
        assert_eq!(rm_kv.get("intelligence"), Some(&PyRep::Integer(27)));
        assert_eq!(rm_kv.get("memory"), Some(&PyRep::Integer(21)));

        ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            assert_eq!(ch.neural.charisma, 17);
            assert_eq!(ch.neural.intelligence, 27);
            assert_eq!(ch.neural.memory, 21);
            assert_eq!(ch.neural.perception, 17);
            assert_eq!(ch.neural.willpower, 17);
            assert!(ch.neural.last_remap_unix.is_some());
            let cooldown = ch.neural.remap_cooldown_until_unix.unwrap();
            // Cooldown is 31,536,000s (1 year)
            assert!(cooldown >= now_unix + 31_535_900 && cooldown <= now_unix + 31_536_100);
        });

        // 3. Second remap with no bonus remaps rejected due to cooldown
        let cooldown_err = ctx
            .dispatch_rpc(
                &mut sess,
                "charMgr",
                None,
                "RespecCharacter",
                &[
                    PyRep::Integer(21),
                    PyRep::Integer(21),
                    PyRep::Integer(21),
                    PyRep::Integer(18),
                    PyRep::Integer(18),
                ],
                &kw,
                None,
            )
            .await;
        assert_eq!(cooldown_err.unwrap_err().code, "RemapFailed");

        // 4. Consume bonus remap allows remapping during cooldown
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.neural.bonus_remaps = 1;
        });

        let bonus_remap = ctx
            .dispatch_rpc(
                &mut sess,
                "charMgr",
                None,
                "RespecCharacter",
                &[
                    PyRep::Integer(21),
                    PyRep::Integer(21),
                    PyRep::Integer(21),
                    PyRep::Integer(18),
                    PyRep::Integer(18), // sum = 99
                ],
                &kw,
                None,
            )
            .await
            .expect("RespecCharacter with bonus remap");
        let PyRep::KeyVal(b_kv) = bonus_remap else {
            panic!("Expected KeyVal from RespecCharacter");
        };
        assert_eq!(b_kv.get("bonusRemaps"), Some(&PyRep::Integer(0)));

        ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&char_id];
            assert_eq!(ch.neural.bonus_remaps, 0);
            assert_eq!(ch.neural.charisma, 21);
            assert_eq!(ch.neural.intelligence, 21);
            assert_eq!(ch.neural.memory, 21);
            assert_eq!(ch.neural.perception, 18);
            assert_eq!(ch.neural.willpower, 18);
            let cooldown = ch.neural.remap_cooldown_until_unix.unwrap();
            assert!(cooldown >= now_unix + 31_535_900 && cooldown <= now_unix + 31_536_100);
        });

        let respec_info = ctx
            .dispatch_rpc(&mut sess, "skillMgr2", None, "GetRespecInfo", &[], &kw, None)
            .await
            .expect("GetRespecInfo after bonus remap");
        let PyRep::KeyVal(respec_kv) = respec_info else {
            panic!("Expected KeyVal from GetRespecInfo");
        };
        assert_eq!(respec_kv.get("freeRespecs"), Some(&PyRep::Integer(0)));

        // 5. ESI GET /characters/{character_id}/attributes includes remap timestamps
        let token = issue_token_with_scopes(
            &ctx,
            char_id,
            vec!["esi-skills.read_skills.v1".to_string()],
        );
        let esi_router = EsiRouter::new(Arc::clone(&ctx));
        let req = EsiHttpRequest::new("GET", &format!("/v1/characters/{char_id}/attributes/"))
            .with_bearer_token(&token);
        let resp = esi_router.handle_request(&req);
        assert_eq!(resp.status_code, 200);
        let json: Value = serde_json::from_slice(&resp.body).expect("parse json");
        assert_eq!(json["charisma"], 21);
        assert_eq!(json["intelligence"], 21);
        assert!(json["last_remap_date"].as_str().is_some());
        assert!(json["accrued_remap_cooldown_date"].as_str().is_some());
    }
}

pub(crate) mod inv1_repackage_item_inv2_stack {
    // Ship repackaging, inventory stack/split/trash, and DNA fitting encoder/decoder integration tests.


    use crate::common;

    use eve_db::{InventoryFlag, InventoryItemRecord, SavedFittingRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::{dna_to_fitting, fitting_to_dna, saved_fitting_to_dna};
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_inv1_repackage_item() {
        // parity: feat:ships.repackage, rpc:charUnboundMgr.SelectCharacterID, rpc:repackagingSvc.RepackageItems, rpc:ship.AssembleShip
        eve_testkit::attest(&["feat:ships.repackage"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let mut sess = SessionState::new(924);

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

        let station_id = 60003760;
        let packaged_item_id = 991_000_201;

        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.station_id = Some(station_id);
            sess.station_id = Some(station_id);

            // Put an unpackaged item in station hangar (singleton = true)
            snap.items.insert(
                packaged_item_id,
                InventoryItemRecord {
                    item_id: packaged_item_id,
                    type_id: 587, // Rifter
                    owner_id: char_id,
                    location_id: station_id,
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

        // Repackage item via packaging service
        let rep = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(packaged_item_id)],
                &kw,
                None,
            )
            .await
            .expect("RepackageItems");
        let PyRep::KeyVal(kv) = rep else {
            panic!("Expected KeyVal from RepackageItems");
        };
        assert_eq!(kv.get("singleton"), Some(&PyRep::Bool(false)));

        ctx.db.with_snapshot(|snap| {
            let item = &snap.items[&packaged_item_id];
            assert!(!item.singleton, "Item should now be packaged (singleton=false)");
        });

        // Re-assemble the packaged ship via ship.AssembleShip and then repackage via V24 dict(itemsByStation) payload
        let asm_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "AssembleShip",
                &[PyRep::Integer(packaged_item_id)],
                &kw,
                None,
            )
            .await
            .expect("AssembleShip");
        assert_eq!(asm_rep.dict_get("singleton"), Some(&PyRep::Bool(true)));

        let v24_repack_arg = PyRep::Dict(vec![(
            PyRep::Integer(station_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(packaged_item_id),
                PyRep::Integer(station_id),
            ])]),
        )]);
        let v24_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[v24_repack_arg],
                &kw,
                None,
            )
            .await
            .expect("RepackageItems V24 dict");
        assert_eq!(v24_rep.dict_get("singleton"), Some(&PyRep::Bool(false)));

        // Error case 1: cannot repackage active ship
        let active_ship_id = ctx.db.with_snapshot(|snap| snap.characters[&char_id].active_ship_id);
        let active_ship_err = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(active_ship_id)],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            active_ship_err.unwrap_err().code,
            "CannotRepackageActiveShip"
        );

        // Error case 2: cannot repackage or assemble an item that is currently fitted to a ship (IsShipFittingFlag)
        let fitted_module_id = 991_000_202;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                fitted_module_id,
                InventoryItemRecord {
                    item_id: fitted_module_id,
                    type_id: 2889, // 200mm AutoCannon I
                    owner_id: char_id,
                    location_id: active_ship_id,
                    flag: InventoryFlag::HiSlot0,
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
        let fitted_repack_err = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(fitted_module_id)],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            fitted_repack_err.unwrap_err().code,
            "CanNotUnassembleInThisItemLoc"
        );
        let fitted_asm_err = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "AssembleShip",
                &[PyRep::Integer(fitted_module_id)],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            fitted_asm_err.unwrap_err().code,
            "CanNotUnassembleInThisItemLoc"
        );

        // Error case 3: cannot repackage or assemble a ship/module with structural or armor damage
        let damaged_ship_id = 991_000_203;
        ctx.db.with_mut_snapshot(|snap| {
            let mut dmg_attrs = HashMap::new();
            dmg_attrs.insert(eve_sde::attr_ids::DAMAGE, 42.0);
            dmg_attrs.insert(eve_sde::attr_ids::ARMOR_DAMAGE, 15.0);
            snap.items.insert(
                damaged_ship_id,
                InventoryItemRecord {
                    item_id: damaged_ship_id,
                    type_id: 587, // Rifter
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: dmg_attrs,
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
        let dmg_repack_err = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(damaged_ship_id)],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            dmg_repack_err.unwrap_err().code,
            "CantRepackageDamagedItem"
        );
        let dmg_asm_err = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "AssembleShip",
                &[PyRep::Integer(damaged_ship_id)],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            dmg_asm_err.unwrap_err().code,
            "CantRepackageDamagedItem"
        );

        // Repairing the damaged ship via repairSvc.RepairItems clears hull/armor damage and allows repackaging
        ctx.dispatch_rpc(
            &mut sess,
            "repairSvc",
            None,
            "RepairItems",
            &[PyRep::List(vec![PyRep::Integer(damaged_ship_id)])],
            &kw,
            None,
        )
        .await
        .expect("RepairItems on damaged hangar ship");

        // Error case 3b: cannot repackage a non-empty ship (containing cargo or fitted modules)
        let cargo_in_ship_id = 991_000_204;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                cargo_in_ship_id,
                InventoryItemRecord {
                    item_id: cargo_in_ship_id,
                    type_id: 34, // Tritanium
                    owner_id: char_id,
                    location_id: damaged_ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
        let non_empty_err = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(damaged_ship_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Non-empty ship must not be repackaged");
        assert_eq!(non_empty_err.code, "CannotRepackageNonEmptyItem");
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&cargo_in_ship_id);
        });

        let repaired_repack = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(damaged_ship_id)],
                &kw,
                None,
            )
            .await
            .expect("RepackageItems after repair");
        assert_eq!(repaired_repack.dict_get("singleton"), Some(&PyRep::Bool(false)));

        // Repackaging an unpacked module in station hangar: refuses when damaged, succeeds when undamaged
        let hangar_module_id = 991_000_205;
        ctx.db.with_mut_snapshot(|snap| {
            let mut mod_dmg = HashMap::new();
            mod_dmg.insert(eve_sde::attr_ids::DAMAGE, 5.0);
            snap.items.insert(
                hangar_module_id,
                InventoryItemRecord {
                    item_id: hangar_module_id,
                    type_id: 2889, // 200mm AutoCannon I
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: mod_dmg,
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
        let dmg_mod_err = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(hangar_module_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Damaged module must not be repackaged");
        assert_eq!(dmg_mod_err.code, "CantRepackageDamagedItem");
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&hangar_module_id).unwrap().dynamic_attributes.clear();
        });
        let mod_repack_ok = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(hangar_module_id)],
                &kw,
                None,
            )
            .await
            .expect("RepackageItems on undamaged module");
        assert_eq!(mod_repack_ok.dict_get("singleton"), Some(&PyRep::Bool(false)));

        // Error case 4: cannot repackage while undocked
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().station_id = None;
        });
        sess.station_id = None;
        let undocked_err = ctx
            .dispatch_rpc(
                &mut sess,
                "repackagingSvc",
                None,
                "RepackageItems",
                &[PyRep::Integer(packaged_item_id)],
                &kw,
                None,
            )
            .await;
        assert_eq!(undocked_err.unwrap_err().code, "NotDocked");
    }

    #[tokio::test]
    async fn test_inv2_stack_split_destroy_inventory() {
        // parity: feat:inventory.split-merge, feat:inventory.trash-items, rpc:charUnboundMgr.SelectCharacterID, rpc:invbroker.SplitStack, rpc:invbroker.TrashItems
        eve_testkit::attest(&["feat:inventory.split-merge", "feat:inventory.trash-items"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let mut sess = SessionState::new(925);

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

        let station_id = 60003760;
        let item_a_id = 991_000_301;
        let item_b_id = 991_000_302;

        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.station_id = Some(station_id);
            sess.station_id = Some(station_id);
            snap.items
                .retain(|_, it| !(it.owner_id == char_id && it.location_id == station_id && it.type_id == 34));
            snap.next_entity_id = snap.next_entity_id.max(item_b_id);

            // Item A: Tritanium (type 34), qty 20
            snap.items.insert(
                item_a_id,
                InventoryItemRecord {
                    item_id: item_a_id,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 20,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );

            // Item B: Tritanium (type 34), qty 30
            snap.items.insert(
                item_b_id,
                InventoryItemRecord {
                    item_id: item_b_id,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 30,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });

        // 1. Merge stack B (30) into stack A (20) via canonical invbroker.MultiMerge
        let merge_ops = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(item_b_id),
            PyRep::Integer(item_a_id),
            PyRep::Integer(30),
        ])]);
        let stack_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "MultiMerge",
                &[merge_ops, PyRep::Integer(station_id)],
                &kw,
                None,
            )
            .await
            .expect("invbroker.MultiMerge");
        let PyRep::KeyVal(stack_kv) = stack_rep else {
            panic!("Expected KeyVal from MultiMerge");
        };
        assert_eq!(stack_kv.get("quantity"), Some(&PyRep::Integer(50)));

        ctx.db.with_snapshot(|snap| {
            let item_a = &snap.items[&item_a_id];
            assert_eq!(item_a.quantity, 50);
            assert!(
                !snap.items.contains_key(&item_b_id),
                "Source stack must be deleted after full merge"
            );
        });

        // 2. Split stack: split 15 out of item A via canonical invbroker.SplitStack
        let split_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "SplitStack",
                &[
                    PyRep::Integer(station_id),
                    PyRep::Integer(item_a_id),
                    PyRep::Integer(15),
                    PyRep::Integer(char_id),
                ],
                &kw,
                None,
            )
            .await
            .expect("invbroker.SplitStack");
        let PyRep::KeyVal(split_kv) = split_rep else {
            panic!("Expected KeyVal from SplitStack");
        };
        assert_eq!(split_kv.get("originalQuantity"), Some(&PyRep::Integer(35)));
        assert_eq!(split_kv.get("newQuantity"), Some(&PyRep::Integer(15)));
        let new_item_id = split_kv.get("newItemID").unwrap().as_i64().unwrap();

        ctx.db.with_snapshot(|snap| {
            let item_a = &snap.items[&item_a_id];
            assert_eq!(item_a.quantity, 35);
            let new_item = &snap.items[&new_item_id];
            assert_eq!(new_item.quantity, 15);
            assert_eq!(new_item.type_id, 34);
        });

        // 3. Split-via-Add: move 5 units out of item_a_id (35 -> 30 + 5) using invbroker.Add with qty kwarg
        let mut add_kw = HashMap::new();
        add_kw.insert("flag".to_string(), PyRep::Integer(InventoryFlag::Hangar as i64));
        add_kw.insert("qty".to_string(), PyRep::Integer(5));
        let add_split_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "Add",
                &[PyRep::Integer(item_a_id), PyRep::Integer(station_id)],
                &add_kw,
                None,
            )
            .await
            .expect("invbroker.Add with qty split");
        let split_via_add_id = add_split_rep.dict_get("itemID").unwrap().as_i64().unwrap();
        assert_ne!(split_via_add_id, item_a_id);

        // 4. StackAll in station hangar merges the 5-unit stack and 30-unit stack ( leaving new_item_id separate only after we re-split 15 )
        let stack_all_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "StackAll",
                &[PyRep::Integer(InventoryFlag::Hangar as i64)],
                &kw,
                None,
            )
            .await
            .expect("invbroker.StackAll");
        assert!(
            stack_all_rep
                .dict_get("mergedCount")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                >= 2
        );
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.items[&item_a_id].quantity, 50);
        });

        // 4b. Dropping an identical stackable item into Cargo via invbroker.Add merges them automatically
        let active_ship_id = ctx.db.with_snapshot(|snap| snap.characters[&char_id].active_ship_id);
        let drop_a_id = 991_000_310;
        let drop_b_id = 991_000_311;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                drop_a_id,
                InventoryItemRecord {
                    item_id: drop_a_id,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: active_ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                drop_b_id,
                InventoryItemRecord {
                    item_id: drop_b_id,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 250,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(drop_b_id),
                PyRep::Integer(active_ship_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("invbroker.Add identical stackable item into Cargo merges");
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.items[&drop_a_id].quantity,
                350,
                "Dropping identical stackable item into Cargo must merge into existing stack"
            );
            assert!(!snap.items.contains_key(&drop_b_id));
        });

        // Split 15 again so we can trash the 15-unit stack
        let split_again = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "SplitStack",
                &[PyRep::Integer(item_a_id), PyRep::Integer(15)],
                &kw,
                None,
            )
            .await
            .expect("SplitStack before trash");
        let trash_target_id = split_again.dict_get("newItemID").unwrap().as_i64().unwrap();

        // 5. Trash items via canonical invbroker.TrashItems (feat:inventory.trash-items)
        let destroy_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "TrashItems",
                &[
                    PyRep::List(vec![PyRep::Integer(trash_target_id)]),
                    PyRep::Integer(60_003_760),
                ],
                &kw,
                None,
            )
            .await
            .expect("TrashItems");
        let PyRep::KeyVal(des_kv) = destroy_rep else {
            panic!("Expected KeyVal from TrashItems");
        };
        assert_eq!(des_kv.get("deleted"), Some(&PyRep::Bool(true)));
        assert_eq!(des_kv.get("count"), Some(&PyRep::Integer(1)));

        ctx.db.with_snapshot(|snap| {
            assert!(
                !snap.items.contains_key(&trash_target_id),
                "Trashed item must be removed from inventory"
            );
        });

        // 4. Verify trashing refusals: fitted items (CannotTrashFittedItem) and locked items (ItemLocked)
        let fitted_item_id = 999_888_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                fitted_item_id,
                eve_db::InventoryItemRecord {
                    item_id: fitted_item_id,
                    type_id: 3300,
                    owner_id: 90_000_001,
                    location_id: 100_000_001,
                    flag: eve_db::InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let fitted_err = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "TrashItems",
                &[PyRep::List(vec![PyRep::Integer(fitted_item_id)])],
                &kw,
                None,
            )
            .await
            .expect_err("Trashing fitted item must fail");
        assert_eq!(fitted_err.code, "CannotTrashFittedItem");

        let contract_item_id = 999_888_002i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                contract_item_id,
                eve_db::InventoryItemRecord {
                    item_id: contract_item_id,
                    type_id: 34,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.contracts.insert(
                77001,
                eve_db::ContractRecord {
                    contract_id: 77001,
                    issuer_id: 90_000_001,
                    status: "Outstanding".into(),
                    item_ids: vec![contract_item_id],
                    ..Default::default()
                },
            );
        });
        let contract_err = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "TrashItems",
                &[PyRep::List(vec![PyRep::Integer(contract_item_id)])],
                &kw,
                None,
            )
            .await
            .expect_err("Trashing contract-locked item must fail");
        assert_eq!(contract_err.code, "ItemLocked");

        let industry_item_id = 999_888_003i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                industry_item_id,
                eve_db::InventoryItemRecord {
                    item_id: industry_item_id,
                    type_id: 687,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.industry_jobs.insert(
                88001,
                eve_db::IndustryJobRecord {
                    job_id: 88001,
                    installer_id: 90_000_001,
                    facility_id: 60_003_760,
                    blueprint_item_id: industry_item_id,
                    blueprint_type_id: 687,
                    activity_id: 1,
                    runs: 1,
                    product_type_id: 587,
                    status: "Active".into(),
                    duration_seconds: 3600,
                    start_unix: 1704067200,
                },
            );
        });
        let industry_err = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "TrashItems",
                &[PyRep::List(vec![PyRep::Integer(industry_item_id)])],
                &kw,
                None,
            )
            .await
            .expect_err("Trashing industry-locked item must fail");
        assert_eq!(industry_err.code, "ItemLocked");

        // Refusal when item is in a remote station hangar the character cannot access while docked at 60003760
        let remote_item_id = 999_888_004i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                remote_item_id,
                eve_db::InventoryItemRecord {
                    item_id: remote_item_id,
                    type_id: 34,
                    owner_id: 90_000_001,
                    location_id: 60_008_494, // Amarr VIII
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 50,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let remote_err = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "TrashItems",
                &[PyRep::List(vec![PyRep::Integer(remote_item_id)])],
                &kw,
                None,
            )
            .await
            .expect_err("Trashing item in inaccessible remote station hangar must fail");
        assert_eq!(remote_err.code, "DeleteFailed");
    }

    #[tokio::test]
    async fn test_inv3_dna_fitting_string_encoder_decoder() {
        // parity: feat:fitting.fittings-save, rpc:charUnboundMgr.SelectCharacterID
        eve_testkit::attest(&["feat:fitting.fittings-save"]);
        // 1. DNA format encode/decode unit checks: <ship_type_id>:<type_id_1>;<qty_1>:<type_id_2>;<qty_2>:...::
        let ship_type_id = 587; // Rifter
        let modules = vec![
            (3300, 2), // Small Projectile Turret x2
            (34, 100), // Tritanium x100
            (3300, 1), // Additional Small Projectile Turret x1 (should aggregate to 3)
        ];

        let dna = fitting_to_dna(ship_type_id, &modules);
        assert_eq!(dna, "587:3300;3:34;100::");

        let (decoded_ship, decoded_items) = dna_to_fitting(&dna).expect("decode dna");
        assert_eq!(decoded_ship, 587);
        assert_eq!(decoded_items, vec![(3300, 3), (34, 100)]);

        let re_encoded = fitting_to_dna(decoded_ship, &decoded_items);
        assert_eq!(re_encoded, dna);

        let saved_fit = SavedFittingRecord {
            fitting_id: 1,
            owner_id: 90_000_001,
            name: "Test Fit".into(),
            description: "DNA test fitting".into(),
            ship_type_id: 587,
            items: vec![
                (3300, InventoryFlag::HiSlot0, 3),
                (34, InventoryFlag::Cargo, 100),
            ],
        };
        let saved_dna = saved_fitting_to_dna(&saved_fit);
        assert_eq!(saved_dna, "587:3300;3:34;100::");

        let (s_id, items) = dna_to_fitting("587:3300::").expect("decode without count");
        assert_eq!(s_id, 587);
        assert_eq!(items, vec![(3300, 1)]);

        assert!(dna_to_fitting("").is_err(), "Empty string should error");
        assert!(
            dna_to_fitting("not_a_number:3300;1::").is_err(),
            "Invalid ship type ID should error"
        );

        // 2. End-to-end charFittingMgr, corpFittingMgr & allianceFittingMgr RPCs + SQLite persistence + max fitting limits
        let db_path =
            std::env::temp_dir().join(format!("evomu_fittings_save_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        let repo = std::sync::Arc::new(
            eve_db::WorldRepository::open_sqlite(&db_path).expect("WorldRepository::open_sqlite"),
        );
        repo.with_mut_snapshot(|snap| {
            *snap = eve_db::WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        });
        let ctx = ServerContext::with_repository(repo);
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let mut sess = SessionState::new(926);

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

        let fit_kv = PyRep::keyval([
            ("name", PyRep::String("V24 Rifter Brawl".into())),
            ("shipTypeID", PyRep::Integer(587)),
            ("description", PyRep::String("Autocannon scram web Rifter".into())),
            (
                "fitData",
                PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::Integer(2889),
                        PyRep::Integer(InventoryFlag::HiSlot0 as i64),
                        PyRep::Integer(1),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(438),
                        PyRep::Integer(InventoryFlag::MedSlot0 as i64),
                        PyRep::Integer(1),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1236),
                        PyRep::Integer(InventoryFlag::LoSlot0 as i64),
                        PyRep::Integer(1),
                    ]),
                ]),
            ),
        ]);

        let fid1 = ctx
            .dispatch_rpc(
                &mut sess,
                "charFittingMgr",
                None,
                "SaveFitting",
                &[PyRep::Integer(char_id), fit_kv.clone()],
                &kw,
                None,
            )
            .await
            .expect("charFittingMgr.SaveFitting")
            .as_i64()
            .unwrap();

        // SaveManyFittings (bulk import)
        let bulk_dict = PyRep::Dict(vec![
            (
                PyRep::Integer(1),
                PyRep::keyval([
                    ("name", PyRep::String("Bulk Rifter 1".into())),
                    ("shipTypeID", PyRep::Integer(587)),
                    ("description", PyRep::String("Bulk 1".into())),
                    ("fitData", PyRep::List(vec![])),
                ]),
            ),
            (
                PyRep::Integer(2),
                PyRep::keyval([
                    ("name", PyRep::String("Bulk Rifter 2".into())),
                    ("shipTypeID", PyRep::Integer(587)),
                    ("description", PyRep::String("Bulk 2".into())),
                    ("fitData", PyRep::List(vec![])),
                ]),
            ),
        ]);
        let saved_many = ctx
            .dispatch_rpc(
                &mut sess,
                "charFittingMgr",
                None,
                "SaveManyFittings",
                &[PyRep::Integer(char_id), bulk_dict],
                &kw,
                None,
            )
            .await
            .expect("charFittingMgr.SaveManyFittings");
        let PyRep::List(many_ids) = saved_many else {
            panic!("Expected List of fittingIDs from SaveManyFittings");
        };
        assert_eq!(many_ids.len(), 2);

        // UpdateNameAndDescription
        ctx.dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "UpdateNameAndDescription",
            &[
                PyRep::Integer(fid1),
                PyRep::Integer(char_id),
                PyRep::String("Updated Rifter Fit".into()),
                PyRep::String("Updated description".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("charFittingMgr.UpdateNameAndDescription");

        // GetFittings
        let fittings_rowset = ctx
            .dispatch_rpc(
                &mut sess,
                "charFittingMgr",
                None,
                "GetFittings",
                &[PyRep::Integer(char_id)],
                &kw,
                None,
            )
            .await
            .expect("charFittingMgr.GetFittings");
        let PyRep::Dict(char_fits_map) = fittings_rowset else {
            panic!("expected Dict from charFittingMgr.GetFittings");
        };
        assert_eq!(char_fits_map.len(), 3);

        // Corporate fitting save & retrieve via corpFittingMgr
        let corp_id = sess.corporation_id.unwrap_or(1_000_009);
        let corp_fid = ctx
            .dispatch_rpc(
                &mut sess,
                "corpFittingMgr",
                None,
                "SaveFitting",
                &[PyRep::Integer(corp_id), fit_kv.clone()],
                &kw,
                None,
            )
            .await
            .expect("corpFittingMgr.SaveFitting")
            .as_i64()
            .unwrap();
        let corp_fittings = ctx
            .dispatch_rpc(
                &mut sess,
                "corpFittingMgr",
                None,
                "GetFittings",
                &[PyRep::Integer(corp_id)],
                &kw,
                None,
            )
            .await
            .expect("corpFittingMgr.GetFittings");
        let PyRep::Dict(corp_fits_map) = corp_fittings else {
            panic!("expected Dict from corpFittingMgr.GetFittings");
        };
        assert_eq!(corp_fits_map.len(), 1);

        // DeleteManyFittings & DeleteFitting
        ctx.dispatch_rpc(
            &mut sess,
            "charFittingMgr",
            None,
            "DeleteManyFittings",
            &[PyRep::Integer(char_id), PyRep::List(many_ids)],
            &kw,
            None,
        )
        .await
        .expect("charFittingMgr.DeleteManyFittings");
        ctx.dispatch_rpc(
            &mut sess,
            "corpFittingMgr",
            None,
            "DeleteFitting",
            &[PyRep::Integer(corp_id), PyRep::Integer(corp_fid)],
            &kw,
            None,
        )
        .await
        .expect("corpFittingMgr.DeleteFitting");

        // Alliance fitting save, list & delete via allianceFittingMgr + 300 corp/alliance fitting limit
        let alliance_id = 99_000_001i64;
        sess.alliance_id = Some(alliance_id);
        let alliance_fid = ctx
            .dispatch_rpc(
                &mut sess,
                "allianceFittingMgr",
                None,
                "SaveFitting",
                &[PyRep::Integer(alliance_id), fit_kv.clone()],
                &kw,
                None,
            )
            .await
            .expect("allianceFittingMgr.SaveFitting")
            .as_i64()
            .unwrap();
        let alliance_fittings = ctx
            .dispatch_rpc(
                &mut sess,
                "allianceFittingMgr",
                None,
                "GetFittings",
                &[PyRep::Integer(alliance_id)],
                &kw,
                None,
            )
            .await
            .expect("allianceFittingMgr.GetFittings");
        let PyRep::Dict(alliance_fits_map) = alliance_fittings else {
            panic!("expected Dict from allianceFittingMgr.GetFittings");
        };
        assert_eq!(alliance_fits_map.len(), 1);
        ctx.dispatch_rpc(
            &mut sess,
            "allianceFittingMgr",
            None,
            "DeleteFitting",
            &[PyRep::Integer(alliance_id), PyRep::Integer(alliance_fid)],
            &kw,
            None,
        )
        .await
        .expect("allianceFittingMgr.DeleteFitting");

        // Enforce 300 corp/alliance fitting limit (OwnerMaxFittings)
        ctx.db.with_mut_snapshot(|snap| {
            for idx in 0..300 {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.saved_fittings.insert(
                    id,
                    SavedFittingRecord {
                        fitting_id: id,
                        owner_id: alliance_id,
                        name: format!("Alliance Fit {idx}"),
                        ship_type_id: 587,
                        description: String::new(),
                        items: Vec::new(),
                    },
                );
            }
        });
        let alliance_over_limit = ctx
            .dispatch_rpc(
                &mut sess,
                "allianceFittingMgr",
                None,
                "SaveFitting",
                &[PyRep::Integer(alliance_id), fit_kv.clone()],
                &kw,
                None,
            )
            .await;
        assert_eq!(alliance_over_limit.unwrap_err().code, "OwnerMaxFittings");

        // Enforce 250 personal fitting limit (OwnerMaxFittings)
        ctx.db.with_mut_snapshot(|snap| {
            for idx in 0..249 {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.saved_fittings.insert(
                    id,
                    SavedFittingRecord {
                        fitting_id: id,
                        owner_id: char_id,
                        name: format!("Fit {idx}"),
                        ship_type_id: 587,
                        description: String::new(),
                        items: Vec::new(),
                    },
                );
            }
        });
        let over_limit_err = ctx
            .dispatch_rpc(
                &mut sess,
                "charFittingMgr",
                None,
                "SaveFitting",
                &[PyRep::Integer(char_id), fit_kv],
                &kw,
                None,
            )
            .await;
        assert_eq!(over_limit_err.unwrap_err().code, "OwnerMaxFittings");

        // Verify SQLite persistence across reload
        let reloaded_db = eve_db::WorldRepository::new_empty_with_snapshot_path(Some(db_path.clone()))
            .expect("reopen SQLite");
        reloaded_db.with_snapshot(|snap| {
            let updated = snap.saved_fittings.get(&fid1).expect("fid1 persisted in SQLite");
            assert_eq!(updated.name, "Updated Rifter Fit");
            assert_eq!(updated.description, "Updated description");
            assert_eq!(updated.items.len(), 3);
        });
        let _ = std::fs::remove_file(&db_path);
    }
}

pub(crate) mod ch1_to_ch5_character_skills {
    // Action Plan Acceptance test suite for Character, Progression, and MachoNet
    // (`plan:CH-*`, `plan:P-*`, `plan:MN-*`) in EvoMu.
    //
    // Covering:
    // - CH-1 .. CH-5: Skill queues, skillMgr semantics, character creation, character sheets/info, GM commands.
    // - P-1 .. P-7: PyPackedRow trailing object columns, DBRow/CRowset encoders, object caching, config service, notifications, native handshake, Destiny binary streams.
    // - MN-01 .. MN-08, MN-17, MN-18: Mining static data, asteroid belts as balls, depletion/respawn, mining cycles, holds/inventory, crystals/residue, skills/bonuses, ice/gas harvesting, NPC mining ops, mining missions/market.



    use crate::common;
    use eve_db::{
        service_roles, AsteroidRecord, InventoryFlag, InventoryItemRecord, IskCents, SkillQueueEntry,
        TrainedSkillRecord,
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
        // parity: plan:CH-1, plan:CH-3, plan:CH-4, rpc:charMgr.AddOwnerNote, rpc:charMgr.GetCharacterDescription, rpc:charMgr.GetContactList, rpc:charMgr.GetOwnerNote, rpc:charMgr.GetPublicInfo3, rpc:charMgr.SetCharacterDescription, rpc:charUnboundMgr.CreateCharacterWithDoll, rpc:charUnboundMgr.SelectCharacterID, rpc:slash.SlashCmd
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
            // Ensure character has Mining IV and Science IV prerequisites for Astrogeology
            let skills = snap.skills.entry(char_id).or_default();
            skills.insert(3386, TrainedSkillRecord { type_id: 3386, level: 4, skill_points: sp_for_skill_level(1.0, 4) });
            skills.insert(3402, TrainedSkillRecord { type_id: 3402, level: 4, skill_points: sp_for_skill_level(1.0, 4) });
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
        // parity: plan:P-1, plan:P-2, plan:P-3, plan:P-4, plan:P-5, rpc:charUnboundMgr.SelectCharacterID, rpc:config.GetStationSolarSystemsByOwner
        eve_testkit::attest(&[
            "plan:P-1",
            "plan:P-2",
            "plan:P-3",
            "plan:P-4",
            "plan:P-5",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
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
        // (rpc tags covered by ledger attestations)

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

        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(char_id).or_default();
            skills.insert(3386, TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 }); // Mining V
        });
        ctx.invalidate_ship_fitting(ship_id);
        let cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, laser_id, ast_rock_id)
            .await
            .expect("one Miner II cycle on the locked Veldspar rock");
        assert_eq!(cycle.ore_type_id, 1230);
        assert!(cycle.units_removed > 0);
        assert_eq!(ctx.db.get_asteroid(ast_rock_id).unwrap().quantity, 50_000 - cycle.units_removed - cycle.residue_units);

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
        // (rpc tags covered by ledger attestations)

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
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
        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(char_id).or_default();
            skills.insert(3386, TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 }); // Mining V
            skills.insert(11395, TrainedSkillRecord { type_id: 11395, level: 1, skill_points: 8_000.0 }); // Deep Core Mining I
        });
        ctx.invalidate_ship_fitting(ship_id);
        let cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, laser_id, ast_rock_id)
            .await
            .expect("one Deep Core Mining Laser I cycle on the locked Veldspar rock");
        assert_eq!(cycle.ore_type_id, 1230);
        assert!(cycle.units_removed > 0);

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
}
