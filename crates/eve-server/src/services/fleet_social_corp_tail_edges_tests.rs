//! Edge-case tests for `fleet_social_corp_tail` covering auth guards, title-only role drops,
//! pending grant preservation, bound-object/docked session notifications, structure controller
//! release, kick blockers, and vote case option formats.

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_corps::with_corp_gov;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use eve_db::corp_roles;
use eve_db::snapshot_ext::CorpTitleEntry;
use eve_db::UpwellStructureRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;

#[test]
fn update_members_edge_payloads_title_role_drop_pending_preservation_and_controller_release() {
    let ctx = test_ctx_with_clock(1_800_080_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001_i64;
    let ceo_id = 90_000_001_i64;
    let mem_id = 90_000_002_i64;

    // 1. Unauthenticated session returns error on update_members and kick_out_members
    let mut unauth = SessionState::new(1);
    assert!(handle_update_members(&ctx, &mut unauth, "UpdateMember", &[])
        .unwrap()
        .is_err());
    assert!(handle_kick_out_members(&ctx, &mut unauth, &[])
        .unwrap()
        .is_err());

    // 2. Non-numeric / unsupported args -> InvalidArgument
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut ceo_sess,
            "UpdateMembers",
            &[PyRep::String("bad".into())]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // 3. CorporationNotFound when session corp doesn't exist
    let mut ghost_sess = ceo_sess.clone();
    ghost_sess.corporation_id = Some(98_999_999);
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut ghost_sess,
            "UpdateMember",
            &[PyRep::keyval([("characterID", PyRep::Integer(mem_id))])]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );

    // 4. Title-only role drop detection (old_had_title_roles && !new_had_title_roles) via snap.corp_titles
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.titles.clear();
        st.member_titles.insert(mem_id, 1); // bit 0 -> title_id 1
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(mem_id, 0);
        snap.corp_role_assignments.remove(&(corp_id, mem_id));
        snap.corp_titles.insert(
            corp_id,
            vec![CorpTitleEntry {
                title_id: 1,
                name: "Auditor".into(),
                roles: vec!["Auditor".into()],
                ..Default::default()
            }],
        );
    });
    assert!(handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::Dict(vec![
            (PyRep::String("characterID".into()), PyRep::Integer(mem_id)),
            (PyRep::String("titleMask".into()), PyRep::Integer(0)),
        ])]
    )
    .unwrap()
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        assert!(snap.corp_governance[&corp_id]
            .role_drop_timestamps
            .contains_key(&mem_id));
    });

    // 5. Re-submitting pending delayed roles & pending delayed title mask before 24h elapses
    //    preserves original ready_at, plus tm > 0 above bit 15 hits fallback tids push
    let delayed_bit = 1u64 << corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK.trailing_zeros();
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.titles.insert(21, ("Vault".into(), delayed_bit));
    });
    assert!(handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(mem_id)),
            ("roles", PyRep::Integer(delayed_bit as i64)),
            ("rolesAtHQ", PyRep::Integer(delayed_bit as i64)),
            ("titleMask", PyRep::Integer(1i64 << 20)),
            ("divisionID", PyRep::Integer(3)),
        ])]
    )
    .unwrap()
    .is_ok());
    ctx.advance_secs(60);
    // Re-submit identical pending masks -> preserves ready_at (newly == 0, still_pending != 0)
    assert!(handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(mem_id)),
            ("roles", PyRep::Integer(delayed_bit as i64)),
            ("rolesAtHQ", PyRep::Integer(delayed_bit as i64)),
            ("titleMask", PyRep::Integer(1i64 << 20)),
            ("squadronID", PyRep::Integer(7)),
        ])]
    )
    .unwrap()
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corp_member_titles[&(corp_id, mem_id)], vec![21]);
        assert_eq!(snap.corp_governance[&corp_id].member_divisions[&mem_id], (3, 7));
    });

    // 6. Numeric row without Bool at index 3 (offset = 0)
    let numeric_no_bool = vec![
        PyRep::Integer(mem_id),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
    ];
    assert!(handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMembers",
        &[PyRep::Tuple(vec![PyRep::List(numeric_no_bool)])]
    )
    .unwrap()
    .is_ok());

    // 7. Releasing an unauthorized structure controller when updating roles on ceo_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_000_000_880_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_880_001,
                type_id: 35832,
                owner_corp_id: 98_999_998, // different corp -> unauthorized
                solar_system_id: 30_000_142,
                name: "Test Astrahus".into(),
                position: [0.0; 3],
                state: "Destroyed".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.active_ship_id = 0;
            ch.solar_system_id = 0;
            ch.station_id = Some(60_003_760);
        }
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(1_000_000_880_001, ceo_id);
        ext.previous_ship_by_structure.insert(1_000_000_880_001, 990_001);
        save_ext_state(snap, &ext);
    });
    assert!(handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(ceo_id)),
            ("roles", PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64)),
        ])]
    )
    .unwrap()
    .is_ok());
    assert!(ceo_sess.ship_id.is_some());
    assert_eq!(
        ceo_sess.ship_id,
        Some(ctx.db.with_snapshot(|s| s.characters[&ceo_id].active_ship_id))
    );

    // 8. emit_corp_role_session_changed with docked char & active_ship_id == 0 & bound_objects fallback
    let _ = ctx.bind_object(&mut mem_sess, "beyonce", 30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&mem_id) {
            ch.solar_system_id = 0;
            ch.active_ship_id = 0;
            ch.station_id = Some(60_003_760);
        }
    });
    emit_corp_role_session_changed(&ctx, corp_id, mem_id, 0, 1, 0, 0, 0, 0, 0, 0);
}

#[test]
fn kick_out_edge_cases_personnel_manager_must_be_docked_alliance_war_and_vote_rowset() {
    let ctx = test_ctx_with_clock(1_800_090_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001_i64;
    let ceo_id = 90_000_001_i64;
    let mem_id = 90_000_002_i64;

    // 1. CorporationNotFound on kick
    let mut bad_corp = ceo_sess.clone();
    bad_corp.corporation_id = Some(98_777_777);
    assert_eq!(
        handle_kick_out_members(&ctx, &mut bad_corp, &[PyRep::Integer(mem_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "CorporationNotFound"
    );

    // 2. MustBeDocked when undocked member tries to resign
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&mem_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(mem_id, 0);
    });
    mem_sess.station_id = None;
    mem_sess.structure_id = None;
    mem_sess.ship_id = Some(555_001);
    assert_eq!(
        handle_kick_out_members(&ctx, &mut mem_sess, &[PyRep::List(vec![])])
            .unwrap()
            .unwrap_err()
            .code,
        "MustBeDocked"
    );

    // 3. member_kick_blocker prevents kicking member who still has roles
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(mem_id, corp_roles::ROLE_PERSONNEL_MANAGER);
        if let Some(ch) = snap.characters.get_mut(&mem_id) {
            ch.station_id = Some(60_003_760);
        }
    });
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Tuple(vec![PyRep::Integer(mem_id)])])
            .unwrap()
            .unwrap_err()
            .code,
        "MemberHasRoles"
    );

    // 4. execute_pending_auto_kicks_for_docked_char early returns (missing char, undocked, no pending)
    //    and structure-docked execution where session is NOT that char
    execute_pending_auto_kicks_for_docked_char(&ctx, &mut ceo_sess, 999_888_777);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&mem_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    execute_pending_auto_kicks_for_docked_char(&ctx, &mut ceo_sess, mem_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&mem_id) {
            ch.structure_id = Some(1_000_000_111_001);
        }
    });
    execute_pending_auto_kicks_for_docked_char(&ctx, &mut ceo_sess, mem_id);
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id)
            .or_default()
            .pending_auto_kicks
            .insert(mem_id, ctx.now_unix());
    });
    execute_pending_auto_kicks_for_docked_char(&ctx, &mut ceo_sess, mem_id);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.corporations[&corp_id].members.contains(&mem_id));
    });

    // 5. Alliance war prevents solo CEO closure
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().alliance_id = Some(99_000_001);
        snap.wars.insert(
            777_001,
            eve_db::WarRecord {
                war_id: 777_001,
                aggressor_corp_id: 99_000_001,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: 1_800_000_000,
                active: true,
            },
        );
    });
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[])
            .unwrap()
            .unwrap_err()
            .code,
        "ActiveWarsPreventClosure"
    );

    // 6. handle_insert_vote_case edge cases:
    //    - 0 shares non-director on vote_type 0 -> CorpRoleDenied
    //    - Rowset options, tuple [text, param] options, empty options for vote_type 0 & non-0,
    //      and empty options CandidateNotMember when target_id not in corp
    assert_eq!(
        handle_insert_vote_case(
            &ctx,
            corp_id,
            mem_id, // no longer in corp
            ctx.now_unix(),
            &[PyRep::String("T".into())]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().members.insert(mem_id);
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(mem_id, 0);
    });
    assert_eq!(
        handle_insert_vote_case(
            &ctx,
            corp_id,
            mem_id,
            ctx.now_unix(),
            &[PyRep::String("CEO".into()), PyRep::String("D".into()), PyRep::Integer(0)]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    // Empty options with target_id not in corp -> CandidateNotMember
    assert_eq!(
        handle_insert_vote_case(
            &ctx,
            corp_id,
            ceo_id,
            ctx.now_unix(),
            &[
                PyRep::String("CEO".into()),
                PyRep::String("D".into()),
                PyRep::Integer(0),
                PyRep::Integer(999_999_111),
            ]
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CandidateNotMember"
    );
    // Empty options with valid member -> auto-populates Vote for <name> / Reject
    assert!(handle_insert_vote_case(
        &ctx,
        corp_id,
        ceo_id,
        ctx.now_unix(),
        &[
            PyRep::String("CEO".into()),
            PyRep::String("D".into()),
            PyRep::Integer(0),
            PyRep::Integer(mem_id),
        ]
    )
    .unwrap()
    .is_ok());
    // Empty options with vote_type != 0 -> auto-populates Yes / Accept & No / Reject
    assert!(handle_insert_vote_case(
        &ctx,
        corp_id,
        ceo_id,
        ctx.now_unix(),
        &[
            PyRep::String("General".into()),
            PyRep::String("D".into()),
            PyRep::Integer(1),
        ]
    )
    .unwrap()
    .is_ok());
    // Rowset options
    assert!(handle_insert_vote_case(
        &ctx,
        corp_id,
        ceo_id,
        ctx.now_unix(),
        &[
            PyRep::String("Rowset".into()),
            PyRep::String("D".into()),
            PyRep::Integer(4),
            PyRep::rowset(
                vec!["opt", "param"],
                vec![vec![PyRep::String("Choice A".into()), PyRep::Integer(1)]]
            ),
        ]
    )
    .unwrap()
    .is_ok());
}
