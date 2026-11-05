#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{IskCents, JumpCloneRecord, TrainedSkillRecord};

#[tokio::test]
async fn jump_clone_names_and_extract_skills_mutations() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);

    let clone_id = 777_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.entry(90_000_001).or_default().push(JumpCloneRecord {
            jump_clone_id: clone_id,
            character_id: 90_000_001,
            location_id: 60_003_760,
            implants: vec![],
        });
    });

    assert!(set_jump_clone_name(&ctx, &sess, &[]).is_err());
    assert!(set_jump_clone_name(
        &ctx,
        &sess,
        &[PyRep::Integer(clone_id), PyRep::String("x".repeat(101))]
    )
    .is_err());
    assert!(set_jump_clone_name(
        &ctx,
        &sess,
        &[PyRep::Integer(999_999), PyRep::String("My Clone".into())]
    )
    .is_err());

    assert_eq!(
        set_jump_clone_name(
            &ctx,
            &sess,
            &[PyRep::Integer(clone_id), PyRep::String("  Combat Clone  ".into())]
        )
        .unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(name_of(snap, clone_id), "Combat Clone");
    });

    // Clear name
    assert_eq!(
        set_jump_clone_name(&ctx, &sess, &[PyRep::Integer(clone_id), PyRep::String("".into())]).unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(name_of(snap, clone_id), "");
    });

    // ValidateInstallJumpClone
    let mut undocked = sess.clone();
    undocked.station_id = None;
    undocked.structure_id = None;
    assert_eq!(
        validate_install_jump_clone(&ctx, &undocked, &[]).unwrap_err().code,
        "MustBeDocked"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = IskCents(100);
        }
    });
    assert_eq!(
        validate_install_jump_clone(&ctx, &sess, &[]).unwrap_err().code,
        "NotEnoughMoney"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = IskCents::from_f64_isk(10_000_000.0);
        }
        snap.skills.entry(90_000_001).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    assert_eq!(
        validate_install_jump_clone(&ctx, &sess, &[]).unwrap(),
        PyRep::Bool(true)
    );

    // GetShipCloneState
    assert!(get_ship_clone_state(&ctx, &sess, &[PyRep::Integer(999_888_777)]).is_err());
    let ship_id = ctx.db.with_snapshot(|s| s.characters.get(&90_000_001).map(|c| c.active_ship_id).unwrap_or(1));
    sess.ship_id = Some(ship_id);
    assert!(get_ship_clone_state(&ctx, &sess, &[PyRep::Integer(ship_id)]).is_ok());

    // Doll & attribute helpers
    assert!(!is_valid_doll_or_portrait_payload(&PyRep::None));
    assert!(!is_valid_doll_or_portrait_payload(&PyRep::String("doll_info_placeholder".into())));
    assert!(is_valid_doll_or_portrait_payload(&PyRep::String("doll:valid=1".into())));
    assert_eq!(
        serialize_doll_pyrep(&PyRep::Tuple(vec![PyRep::Integer(1), PyRep::String("a".into())])),
        "[1,a]"
    );

    ctx.db.with_snapshot(|snap| {
        let attrs = effective_character_attributes(&ctx.sde, snap, 90_000_001);
        assert!(attrs.0 > 0.0 && attrs.1 > 0.0);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 164), attrs.0);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 165), attrs.1);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 166), attrs.2);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 167), attrs.3);
        assert_eq!(lookup_effective_attr_from_tuple(attrs, 168), attrs.4);
    });

    assert!(alpha_skill_cap_for_character(&ctx.sde, 1, 3300) <= 5);
    let _ = skill_queue_rowset(&ctx, 90_000_001);
    emit_server_skills_changed(&ctx, 90_000_001, &[24242], Some("Test"));
    emit_server_skills_removed(&ctx, 90_000_001, &[(24242, 4)]);
    emit_station_jump_clone_invalidated(&ctx, 90_000_001, 60_003_760, true);
    emit_ship_clone_offered(&ctx, 90_000_001, 90_000_002, ship_id);
    emit_ship_clone_canceled(&ctx, 90_000_001, Some(90_000_002));
    emit_ship_clone_accepted(&ctx, 90_000_001, 90_000_002, ship_id);

    // ExtractSkills validation
    assert_eq!(
        extract_skills(&ctx, &sess, &[]).unwrap_err().code,
        "MissingSkillExtractor"
    );
    assert_eq!(
        extract_skills(
            &ctx,
            &sess,
            &[
                PyRep::Dict(vec![(PyRep::Integer(24242), PyRep::Real(500_000.0))]),
                PyRep::Integer(12345),
            ],
        )
        .unwrap_err()
        .code,
        "SkillCannotBeExtracted"
    );

    // handle_qa_move_me_to
    assert!(handle_qa_move_me_to(&ctx, &mut sess, 90_000_001, &[PyRep::Integer(999_999_999)])
        .await
        .is_err());
    let moved = handle_qa_move_me_to(&ctx, &mut sess, 90_000_001, &[PyRep::Integer(30000142)])
        .await
        .unwrap();
    assert_eq!(moved.dict_get("solarSystemID"), Some(&PyRep::Integer(30000142)));
}
