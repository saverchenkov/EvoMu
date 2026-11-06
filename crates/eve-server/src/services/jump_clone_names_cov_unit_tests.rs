//! Focused unit tests for uncovered branches in `jump_clone_names`
//! (`set_jump_clone_name`, `validate_install_jump_clone`, `get_ship_clone_state`,
//! and `extract_skills` validation, prerequisite checking, skill queue pruning, and injector minting).

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::types::{SkillQueueEntry, TrainedSkillRecord};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn full_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

#[test]
fn clone_names_install_validation_and_ship_clone_state_paths() {
    let ctx = ServerContext::with_sde_and_repository(
        full_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_930_001i64;
    let ship_id = 96_930_500i64;
    let station_id = 60_003_760i64;
    let jc_id = 96_930_100i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 9501;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.balance = IskCents::from_f64_isk(500_000.0); // < 900,000 ISK
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 671,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.jump_clones.insert(
            char_id,
            vec![JumpCloneRecord {
                jump_clone_id: jc_id,
                character_id: char_id,
                location_id: ship_id,
                implants: vec![],
            }],
        );
    });

    let sess = SessionState {
        user_id: Some(9501),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // SetJumpCloneName updates stored clone name
    set_jump_clone_name(&ctx, &sess, &[PyRep::Integer(jc_id), PyRep::String("Wormhole Pod".into())])
        .expect("set name");
    ctx.db.with_snapshot(|s| assert_eq!(name_of(s, jc_id), "Wormhole Pod"));

    // ValidateInstallJumpClone: insufficient ISK -> NotEnoughMoney
    let err = validate_install_jump_clone(&ctx, &sess, &[]).expect_err("insufficient funds");
    assert_eq!(err.code, "NotEnoughMoney");

    // Grant 5M ISK -> limit reached (0 Infomorph Psychology skill levels vs 1 existing clone) -> JumpCloneLimitReached
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_f64_isk(5_000_000.0);
    });
    let err = validate_install_jump_clone(&ctx, &sess, &[]).expect_err("clone limit reached");
    assert_eq!(err.code, "JumpCloneLimitReached");

    // Train Infomorph Psychology 2 -> ValidateInstallJumpClone succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 2,
                skill_points: 45_255.0,
            },
        );
    });
    assert_eq!(validate_install_jump_clone(&ctx, &sess, &[]).unwrap(), PyRep::Bool(true));

    // GetShipCloneState returns rowset containing the clone installed on ship_id
    let ship_state = get_ship_clone_state(&ctx, &sess, &[PyRep::Integer(ship_id)]).expect("ship clone state");
    assert!(matches!(ship_state, PyRep::Rowset { ref rows, .. } if !rows.is_empty()));
}

#[test]
fn extract_skills_rules_prerequisites_queue_and_injector_creation() {
    let ctx = ServerContext::with_sde_and_repository(
        full_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000)));

    let char_id = 96_930_010i64;
    let station_id = 60_003_760i64;
    let extractor_id = 96_930_401i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 9510;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.clone_grade = 1; // Alpha first
        ch.skill_points = 5_200_000.0; // Below 5.5M floor first
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            extractor_id,
            InventoryItemRecord {
                item_id: extractor_id,
                type_id: 40519, // Skill Extractor
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(3327, TrainedSkillRecord { type_id: 3327, level: 5, skill_points: 256_000.0 });
        skills.insert(3331, TrainedSkillRecord { type_id: 3331, level: 5, skill_points: 512_000.0 });
        skills.insert(3300, TrainedSkillRecord { type_id: 3300, level: 5, skill_points: 256_000.0 });

        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: 3301,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                started_at_unix: Some(1_700_000_000),
                duration_seconds: 60.0,
            }],
        );
    });

    let sess = SessionState {
        user_id: Some(9510),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };

    // 1. Non-extractable skill 24242 (Infomorph Psychology) rejected
    let err = extract_skills(
        &ctx,
        &sess,
        &[
            PyRep::Dict(vec![(PyRep::Integer(24242), PyRep::Real(500_000.0))]),
            PyRep::Integer(extractor_id),
        ],
    )
    .expect_err("non-extractable skill");
    assert_eq!(err.code, "SkillCannotBeExtracted");

    // 2. Alpha account rejected
    let err = extract_skills(
        &ctx,
        &sess,
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3331), PyRep::Real(250_000.0)),
                (PyRep::Integer(3300), PyRep::Real(250_000.0)),
            ]),
            PyRep::Integer(extractor_id),
        ],
    )
    .expect_err("Omega required");
    assert_eq!(err.code, "OmegaCloneRequired");

    // 3. Upgrade to Omega, test SP floor (< 5.5M) rejected
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
    });
    let err = extract_skills(
        &ctx,
        &sess,
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3331), PyRep::Real(250_000.0)),
                (PyRep::Integer(3300), PyRep::Real(250_000.0)),
            ]),
            PyRep::Integer(extractor_id),
        ],
    )
    .expect_err("5.5M SP floor required");
    assert_eq!(err.code, "InsufficientSkillPoints");

    // 4. Set SP = 8,000,000; extracting Spaceship Command (3327) below L1 while Minmatar Frigate (3331) requires it fails
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().skill_points = 8_000_000.0;
    });
    let err = extract_skills(
        &ctx,
        &sess,
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3327), PyRep::Real(256_000.0)),
                (PyRep::Integer(3331), PyRep::Real(244_000.0)),
            ]),
            PyRep::Integer(extractor_id),
        ],
    )
    .expect_err("prerequisite required");
    assert_eq!(err.code, "SkillPrerequisiteRequired");

    // 5. Valid 500,000 SP extraction from 3331 (250k) + 3300 (250k) succeeds, consumes 1 extractor, mints Large Skill Injector (40520)
    let res = extract_skills(
        &ctx,
        &sess,
        &[
            PyRep::Dict(vec![
                (PyRep::Integer(3331), PyRep::Real(250_000.0)),
                (PyRep::Integer(3300), PyRep::Real(250_000.0)),
            ]),
            PyRep::Integer(extractor_id),
        ],
    )
    .expect("valid extraction succeeds");
    assert!(matches!(res, PyRep::KeyVal(_)));
    ctx.db.with_snapshot(|snap| {
        assert!((snap.characters[&char_id].skill_points - 7_500_000.0).abs() < 1.0);
        assert_eq!(snap.items[&extractor_id].quantity, 1);
        assert!(snap.items.values().any(|i| i.owner_id == char_id && i.type_id == 40520 && i.location_id == station_id));
    });
    let _ = HashMap::<String, PyRep>::new();
}
