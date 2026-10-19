//! Character progression, training queues, jump clones, GM commands, neural remap and inventory action integration tests.

#![forbid(unsafe_code)]

mod common;

use eve_db::{
    CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    NeuralAttributes, SavedFittingRecord, SkillQueueEntry, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::formulas::{sp_for_skill_level, sp_per_minute};
use eve_server::router::ServerContext;
use eve_server::{dna_to_fitting, fitting_to_dna, saved_fitting_to_dna};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
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
    // parity: plan:CH-1
    let ctx = ServerContext::new();
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
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
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
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
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
                },
                SkillQueueEntry {
                    type_id: 3301,
                    target_level: 1,
                    start_sp: 0.0,
                    end_sp: 250.0,
                    duration_seconds: 500.0,
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
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
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
    // parity: plan:CH-2
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(920);

    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

    // Call charUnboundMgr.CreateCharacterWithDoll with Caldari pilot
    // (race=1 Caldari, bloodline=1 Deteis, ancestry=11 Scientists, school=17 Science and Trade Institute)
    let new_char_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Caldari Cadet".into()),
                PyRep::Integer(1),  // Caldari
                PyRep::Integer(1),  // Deteis
                PyRep::Integer(11), // Scientists
                PyRep::Integer(17), // Science and Trade Institute
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
    // parity: plan:CH-3
    let ctx = ServerContext::new();
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
    ctx.db.with_mut_snapshot(|snap| {
        let clones = snap.jump_clones.get_mut(&char_id).unwrap();
        let c = clones.iter_mut().find(|c| c.jump_clone_id == jc_id).unwrap();
        c.location_id = 60008494;
        c.implants = vec![9941];
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

    // 24h cooldown active
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

#[tokio::test]
async fn test_ch4_slash_gm_commands() {
    // parity: plan:CH-4
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
    // parity: plan:CH-5
    let ctx = Arc::new(ServerContext::new());
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
            "charUnboundMgr",
            None,
            "ApplyNeuralRemap",
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
            "charUnboundMgr",
            None,
            "ApplyNeuralRemap",
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
            "charUnboundMgr",
            None,
            "ApplyNeuralRemap",
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
            "charUnboundMgr",
            None,
            "ApplyNeuralRemap",
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
        .expect("ApplyNeuralRemap");
    let PyRep::KeyVal(rm_kv) = remap_rep else {
        panic!("Expected KeyVal from ApplyNeuralRemap");
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
            "charUnboundMgr",
            None,
            "ApplyNeuralRemap",
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
            "charUnboundMgr",
            None,
            "ApplyNeuralRemap",
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
        .expect("ApplyNeuralRemap with bonus remap");
    let PyRep::KeyVal(b_kv) = bonus_remap else {
        panic!("Expected KeyVal from ApplyNeuralRemap");
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
    });

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

#[tokio::test]
async fn test_inv1_repackage_item() {
    // parity: feat:ships.repackage
    let ctx = ServerContext::new();
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

    // Error case: cannot repackage active ship
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

    // Error case: cannot repackage while undocked
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
    // parity: feat:inventory.split-merge, feat:inventory.trash-items
    let ctx = ServerContext::new();
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

    // 1. Stack items: merge item B into item A
    let stack_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invBroker",
            None,
            "StackItems",
            &[PyRep::Integer(item_a_id), PyRep::Integer(item_b_id)],
            &kw,
            None,
        )
        .await
        .expect("StackItems");
    let PyRep::KeyVal(stack_kv) = stack_rep else {
        panic!("Expected KeyVal from StackItems");
    };
    assert_eq!(stack_kv.get("quantity"), Some(&PyRep::Integer(50)));

    ctx.db.with_snapshot(|snap| {
        let item_a = &snap.items[&item_a_id];
        assert_eq!(item_a.quantity, 50);
        assert!(!snap.items.contains_key(&item_b_id), "Source stack must be deleted");
    });

    // 2. Split stack: split 15 out of item A
    let split_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invBroker",
            None,
            "SplitStack",
            &[PyRep::Integer(item_a_id), PyRep::Integer(15)],
            &kw,
            None,
        )
        .await
        .expect("SplitStack");
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

    // 3. Delete/Destroy item
    let destroy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invBroker",
            None,
            "DestroyItem",
            &[PyRep::Integer(new_item_id)],
            &kw,
            None,
        )
        .await
        .expect("DestroyItem");
    let PyRep::KeyVal(des_kv) = destroy_rep else {
        panic!("Expected KeyVal from DestroyItem");
    };
    assert_eq!(des_kv.get("deleted"), Some(&PyRep::Bool(true)));

    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&new_item_id),
            "Destroyed item must be removed from inventory"
        );
    });
}

#[test]
fn test_inv3_dna_fitting_string_encoder_decoder() {
    // parity: feat:fitting.fittings-save
    // Standard format: <ship_type_id>:<type_id_1>;<qty_1>:<type_id_2>;<qty_2>:...::
    let ship_type_id = 587; // Rifter
    let modules = vec![
        (3300, 2),  // Small Projectile Turret x2
        (34, 100),  // Tritanium x100
        (3300, 1),  // Additional Small Projectile Turret x1 (should aggregate to 3)
    ];

    // Encode
    let dna = fitting_to_dna(ship_type_id, &modules);
    assert_eq!(dna, "587:3300;3:34;100::");

    // Decode
    let (decoded_ship, decoded_items) = dna_to_fitting(&dna).expect("decode dna");
    assert_eq!(decoded_ship, 587);
    assert_eq!(decoded_items, vec![(3300, 3), (34, 100)]);

    // Round-trip
    let re_encoded = fitting_to_dna(decoded_ship, &decoded_items);
    assert_eq!(re_encoded, dna);

    // SavedFittingRecord encoder
    let saved_fit = SavedFittingRecord {
        fitting_id: 1,
        owner_id: 90_000_001,
        name: "Test Fit".into(),
        description: "DNA test fitting".into(),
        ship_type_id: 587,
        items: vec![(3300, InventoryFlag::HiSlot0, 3), (34, InventoryFlag::Cargo, 100)],
    };
    let saved_dna = saved_fitting_to_dna(&saved_fit);
    assert_eq!(saved_dna, "587:3300;3:34;100::");

    // Robust parsing: single item without count defaults to 1
    let (s_id, items) = dna_to_fitting("587:3300::").expect("decode without count");
    assert_eq!(s_id, 587);
    assert_eq!(items, vec![(3300, 1)]);

    // Error handling
    assert!(dna_to_fitting("").is_err(), "Empty string should error");
    assert!(dna_to_fitting("not_a_number:3300;1::").is_err(), "Invalid ship type ID should error");
}
