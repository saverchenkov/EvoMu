//! Extra unit and mutation-killing tests for `services::fleet_social::fleet_social_corp_tail`
//! covering `bloodline_default_npc_corp` fallback, every role/title/cooldown branch of
//! `member_kick_blocker` (including exact 86_400s boundary), all row/Rowset/Dict payload
//! shapes and non-CEO `ROLE_DIRECTOR` protections in `handle_update_members`,
//! `emit_corp_role_session_changed`, `handle_kick_out_members` session-change & role guards,
//! downtime stale auto-kick cleanup, and `InsertVoteCase` via `voteManager.InsertVoteCase`.

use crate::services::fleet_social::fleet_social_corp_tail::*;
use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_corps::with_corp_gov;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use eve_db::corp_roles;
use eve_db::snapshot_ext::{CorpMemberRoleAssignment, CorpTitleEntry};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn bloodline_default_npc_corp_and_member_kick_blocker_all_branches_and_cooldown_boundary() {
    let ctx = test_ctx_with_clock(1_800_050_000);
    let _ = seed_player_corps(&ctx);
    let corp_id = 98_000_001i64;
    let target_id = 90_000_002i64;

    // 1. bloodline_default_npc_corp: unknown character falls back to 1_000_006
    ctx.db.with_snapshot(|snap| {
        assert_eq!(bloodline_default_npc_corp(&ctx, snap, 999_999_999), 1_000_006);
        assert!(bloodline_default_npc_corp(&ctx, snap, target_id) >= 1_000_000);
        // Unknown corporation returns None in member_kick_blocker
        assert_eq!(member_kick_blocker(snap, 999_999_999, target_id, 1_800_050_000), None);
    });

    // Clear all roles/titles on target_id so baseline is None
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(target_id, 0);
        snap.corp_role_assignments.remove(&(corp_id, target_id));
        if let Some(g) = snap.corp_governance.get_mut(&corp_id) {
            g.member_titles.remove(&target_id);
            g.role_drop_timestamps.remove(&target_id);
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_800_050_000), None);
    });

    // 2. Test each of the 8 role fields in CorpMemberRoleAssignment individually
    let setters: &[fn(&mut CorpMemberRoleAssignment)] = &[
        |ra| ra.roles = 1,
        |ra| ra.grantable_roles = 1,
        |ra| ra.roles_at_hq = 1,
        |ra| ra.grantable_roles_at_hq = 1,
        |ra| ra.roles_at_base = 1,
        |ra| ra.grantable_roles_at_base = 1,
        |ra| ra.roles_at_other = 1,
        |ra| ra.grantable_roles_at_other = 1,
    ];
    for setter in setters {
        ctx.db.with_mut_snapshot(|snap| {
            let mut ra = CorpMemberRoleAssignment {
                character_id: target_id,
                ..Default::default()
            };
            setter(&mut ra);
            snap.corp_role_assignments.insert((corp_id, target_id), ra);
        });
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                member_kick_blocker(snap, corp_id, target_id, 1_800_050_000),
                Some("MemberHasRoles")
            );
        });
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.remove(&(corp_id, target_id));
    });

    // 3. Pending role grants (pending_role_grants, pending_title_role_grants, pending_scoped_role_grants)
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().pending_role_grants.insert(target_id, (1, 1_900_000_000));
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_800_050_000), Some("MemberHasRoles"));
    });
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.pending_role_grants.clear();
        st.pending_title_role_grants.insert(target_id, (1, 1_900_000_000));
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_800_050_000), Some("MemberHasRoles"));
    });
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.pending_title_role_grants.clear();
        st.pending_scoped_role_grants.insert(target_id, (1, 0, 0, 1_900_000_000));
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_800_050_000), Some("MemberHasRoles"));
    });
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().pending_scoped_role_grants.clear();
    });

    // 4. snap.corp_titles entries (roles, grantable_roles, roles_at_hq, roles_at_base, roles_at_other)
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.titles.clear();
        st.member_titles.insert(target_id, 2); // bit 1 -> title_id = 2
    });
    let title_setters: &[fn(&mut CorpTitleEntry)] = &[
        |t| t.roles = vec!["Accountant".into()],
        |t| t.grantable_roles = vec!["Accountant".into()],
        |t| t.roles_at_hq = vec!["Hangar_Take_1".into()],
        |t| t.roles_at_base = vec!["Hangar_Take_1".into()],
        |t| t.roles_at_other = vec!["Hangar_Take_1".into()],
    ];
    for t_set in title_setters {
        ctx.db.with_mut_snapshot(|snap| {
            let mut te = CorpTitleEntry {
                title_id: 2,
                name: "Officer".into(),
                ..Default::default()
            };
            t_set(&mut te);
            snap.corp_titles.insert(corp_id, vec![te]);
        });
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                member_kick_blocker(snap, corp_id, target_id, 1_800_050_000),
                Some("MemberHasRoleGrantingTitle")
            );
        });
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_titles.remove(&corp_id);
        snap.corp_governance.get_mut(&corp_id).unwrap().member_titles.clear();
    });

    // 5. Role drop 24h (86_400s) cooldown exact boundary:
    //    drop_ts = 1_000_000 -> at 1_086_399 returns Some("RoleDropCooldownActive"), at 1_086_400 returns None
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().role_drop_timestamps.insert(target_id, 1_000_000);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            member_kick_blocker(snap, corp_id, target_id, 1_086_399),
            Some("RoleDropCooldownActive")
        );
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_086_400), None);
    });
}

#[test]
fn update_members_covers_row_shapes_director_toggles_base_removal_and_session_notify() {
    let ctx = test_ctx_with_clock(1_800_060_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001i64;
    let target_id = 90_000_002i64;

    // 1. TargetNotInCorp when target is not a member of corp_id
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut ceo_sess,
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(90_000_999)),
                ("roles", PyRep::Integer(0)),
            ])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotInCorp"
    );

    // 2. Non-CEO Director cannot toggle ROLE_DIRECTOR in any of the 8 role/grantable/scoped masks
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(target_id, corp_roles::ROLE_DIRECTOR);
    });
    for field in [
        "grantableRoles",
        "rolesAtHQ",
        "grantableRolesAtHQ",
        "rolesAtBase",
        "grantableRolesAtBase",
        "rolesAtOther",
        "grantableRolesAtOther",
    ] {
        assert_eq!(
            handle_update_members(
                &ctx,
                &mut mem_sess,
                "UpdateMember",
                &[PyRep::keyval([
                    ("characterID", PyRep::Integer(90_000_001)),
                    (field, PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64)),
                ])],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CrpAccessDenied",
            "non-CEO director should not be able to toggle ROLE_DIRECTOR in {field}"
        );
    }

    // 3. 14-element named row format, numeric row with bool offset at index 3, and Rowset format
    let delayed_bit = 1u64 << corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK.trailing_zeros();
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id)
            .or_default()
            .titles
            .insert(2, ("HangarOfficer".into(), delayed_bit));
    });
    let row_14 = vec![
        PyRep::Integer(target_id),
        PyRep::String("Pilot".into()),
        PyRep::Integer(4),                     // divisionID
        PyRep::Integer(5),                     // squadronID
        PyRep::Integer(corp_roles::ROLE_AUDITOR as i64),
        PyRep::Integer(corp_roles::ROLE_DIPLOMAT as i64),
        PyRep::Integer(delayed_bit as i64),    // rolesAtHQ (delayed hangar)
        PyRep::Integer(1),                     // grantableRolesAtHQ
        PyRep::Integer(delayed_bit as i64),    // rolesAtBase (delayed hangar)
        PyRep::Integer(1),                     // grantableRolesAtBase
        PyRep::Integer(delayed_bit as i64),    // rolesAtOther (delayed hangar)
        PyRep::Integer(1),                     // grantableRolesAtOther
        PyRep::Integer(60_003_760),            // baseID > 0 -> inserts member_bases
        PyRep::Integer(2),                     // titleMask = 2 (title #2 has delayed hangar role)
    ];
    let r14 = handle_update_members(&ctx, &mut ceo_sess, "UpdateMember", &[PyRep::List(row_14)])
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(kv14) = r14 else { panic!() };
    assert_eq!(kv14.get("delayed24h"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let g = snap.corp_governance.get(&corp_id).unwrap();
        assert_eq!(g.member_bases.get(&target_id), Some(&60_003_760));
        assert_eq!(g.member_divisions.get(&target_id), Some(&(4, 5)));
        assert!(g.pending_title_role_grants.contains_key(&target_id));
        assert!(g.pending_scoped_role_grants.contains_key(&target_id));
    });

    // Numeric row with Bool at index 3 and baseID = 0 -> removes member_bases entry
    let row_numeric_bool = vec![
        PyRep::Integer(target_id),
        PyRep::Integer(0),         // new_roles
        PyRep::Integer(0),         // title_mask
        PyRep::Bool(false),        // bool offset!
        PyRep::Integer(0),         // grantable_roles
        PyRep::Integer(0),         // roles_at_hq
        PyRep::Integer(0),         // grantable_roles_at_hq
        PyRep::Integer(0),         // roles_at_base
        PyRep::Integer(0),         // grantable_roles_at_base
        PyRep::Integer(0),         // roles_at_other
        PyRep::Integer(0),         // grantable_roles_at_other
        PyRep::Integer(0),         // base_id == 0 -> removes from member_bases
    ];
    let r_rowset = handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMembers",
        &[PyRep::rowset(vec!["charID"], vec![row_numeric_bool])],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv_rs) = r_rowset else { panic!() };
    assert_eq!(kv_rs.get("updatedCount").and_then(|v| v.as_i64()), Some(1));
    ctx.db.with_snapshot(|snap| {
        let g = snap.corp_governance.get(&corp_id).unwrap();
        assert!(!g.member_bases.contains_key(&target_id));
        assert!(!g.pending_title_role_grants.contains_key(&target_id));
        assert!(!g.pending_scoped_role_grants.contains_key(&target_id));
    });

    // Dict with "memberID" and "role" keys updating CEO's own session
    let r_self = handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("memberID", PyRep::Integer(90_000_001)),
            ("role", PyRep::Integer(corp_roles::ROLE_AUDITOR as i64)),
        ])],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv_self) = r_self else { panic!() };
    assert_eq!(kv_self.get("characterID").and_then(|v| v.as_i64()), Some(90_000_001));
    assert_eq!(ceo_sess.corp_role, corp_roles::ROLE_AUDITOR);

    // 4. Direct emit_corp_role_session_changed no-op when nothing changed vs emits OnSessionChanged when changed
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&target_id).unwrap().user_id = 1);
    ctx.db.acquire_online_lock(1, 2, Some(target_id), true, ctx.now_unix()).unwrap();
    let _ = ctx.drain_notifications_for_char(target_id);
    emit_corp_role_session_changed(&ctx, corp_id, target_id, 1, 1, 2, 2, 3, 3, 4, 4);
    assert!(ctx.drain_notifications_for_char(target_id).is_empty());
    emit_corp_role_session_changed(&ctx, corp_id, target_id, 1, 2, 2, 4, 3, 6, 4, 8);
    let notifs = ctx.drain_notifications_for_char(target_id);
    assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));
}

#[tokio::test]
async fn kick_out_guards_downtime_stale_cleanup_and_insert_vote_case_edge_cases() {
    let ctx = test_ctx_with_clock(1_800_070_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001i64;
    let ceo_id = 90_000_001i64;
    let mem_id = 90_000_002i64;

    // 1. SessionChangeInProgress on caller and on target
    let now = ctx.now_unix();
    crate::services::space_destiny::record_session_change_timer(&ctx, ceo_id, now, 30_000_142, 0, "jump");
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(mem_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
    ctx.advance_secs(15);

    let now2 = ctx.now_unix();
    crate::services::space_destiny::record_session_change_timer(&ctx, mem_id, now2, 30_000_142, 0, "dock");
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(mem_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
    ctx.advance_secs(15);

    // 2. Non-director / non-personnel-manager cannot kick another member -> CorpRoleDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(mem_id, 0);
    });
    assert_eq!(
        handle_kick_out_members(&ctx, &mut mem_sess, &[PyRep::Integer(ceo_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // 3. NotCorpMember and CeoCannotLeave while other members remain
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(90_000_999)])
            .unwrap()
            .unwrap_err()
            .code,
        "NotCorpMember"
    );
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(ceo_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "CeoCannotLeave"
    );

    // 4. execute_pending_auto_kicks_on_downtime cleans up stale queued member who already left
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().pending_auto_kicks.insert(90_000_999, ctx.now_unix());
    });
    assert_eq!(execute_pending_auto_kicks_on_downtime(&ctx), 0);
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap
                .corp_governance
                .get(&corp_id)
                .unwrap()
                .pending_auto_kicks
                .contains_key(&90_000_999)
        );
    });

    // 5. InsertVoteCase via `voteManager.InsertVoteCase`:
    //    - Leading corp_id argument with non-existent corp -> CorporationNotFound
    //    - CandidateNotMember for integer option, tuple option, and default option
    //    - Share creation (vote_type == 5) bounds: <= 0 and > 10_000_000_000 -> InvalidArgument
    //    - Rowset / String options
    let kw = HashMap::new();
    let err_no_corp = crate::services::fleet_social::handle(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::Integer(999_999_999),
            PyRep::String("Election".into()),
            PyRep::String("Desc".into()),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_corp.code, "CorporationNotFound");

    // CandidateNotMember (integer option)
    let err_cand_int = crate::services::fleet_social::handle(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::String("Election".into()),
            PyRep::String("Desc".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(90_000_999)]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cand_int.code, "CandidateNotMember");

    // CandidateNotMember (tuple option `[1, "Outlander", 90_000_999]`)
    let err_cand_tup = crate::services::fleet_social::handle(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::String("Election".into()),
            PyRep::String("Desc".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::String("Outlander".into()),
                PyRep::Integer(90_000_999),
            ])]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cand_tup.code, "CandidateNotMember");

    // Share creation (vote_type == 5) bounds: 0 and 10_000_000_001 -> InvalidArgument
    for bad_shares in [0i64, 10_000_000_001i64] {
        let err_sh = crate::services::fleet_social::handle(
            &ctx,
            &mut ceo_sess,
            "voteManager",
            "InsertVoteCase",
            &[
                PyRep::String("Shares".into()),
                PyRep::String("Bounds".into()),
                PyRep::Integer(5),
                PyRep::Integer(bad_shares),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_sh.code, "InvalidArgument");
    }

    // Valid general vote (vote_type == 4) with String options and leading corp_id
    let ok_vote = crate::services::fleet_social::handle(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("General Poll".into()),
            PyRep::String("Pick one".into()),
            PyRep::Integer(4),
            PyRep::List(vec![PyRep::String("Option Alpha".into()), PyRep::String("Option Beta".into())]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(v_kv) = ok_vote else { panic!() };
    assert_eq!(v_kv.get("voteType").and_then(|v| v.as_i64()), Some(4));
}

#[tokio::test]
async fn mutants_cov_title_roles_member_update_release_kicks_and_vote_director() {
    use crate::router::BoundObjectEntry;
    use crate::services::fleet_social::fleet_social_corp_tail::{
        execute_pending_auto_kicks_for_docked_char, handle_kick_out_members,
        handle_update_members, title_mask_grants_roles,
    };
    use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
    use crate::services::fleet_social::fleet_social_corps::with_corp_gov;
    use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
    use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
    use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};

    let now_unix = 1_800_030_000u64;
    let ctx = test_ctx_with_clock(now_unix);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001i64;
    let ceo_id = 90_000_001i64;
    let mem_id = 90_000_002i64;
    let third_id = 90_000_003i64;
    let kick_docked_id = 90_000_004i64;
    let zero_ship_leave_id = 90_000_005i64;
    let self_leave_id = 90_000_006i64;
    let st_id = 10_000_930_001i64;
    let mem_ship_id = 90_930_001i64;
    let kick_ship_id = 90_930_002i64;
    let self_leave_ship_id = 90_930_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().unwrap();
        for &(cid, ship, docked) in &[
            (mem_id, mem_ship_id, false),
            (third_id, 0, true),
            (kick_docked_id, kick_ship_id, true),
            (zero_ship_leave_id, 0, true),
            (self_leave_id, self_leave_ship_id, true),
        ] {
            if !snap.characters.contains_key(&cid) {
                let mut ch = base_ch.clone();
                ch.character_id = cid;
                ch.name = format!("Char-{cid}");
                ch.corporation_id = corp_id;
                ch.solar_system_id = 30_000_142;
                ch.station_id = if docked { Some(60_003_760) } else { None };
                ch.structure_id = None;
                ch.active_ship_id = ship;
                ch.bloodline_id = 1;
                ch.race_id = 1;
                snap.characters.insert(cid, ch);
            }
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
                ch.active_ship_id = ship;
                if docked {
                    ch.station_id = Some(60_003_760);
                    ch.structure_id = None;
                }
            }
            let corp = snap.corporations.get_mut(&corp_id).unwrap();
            corp.members.insert(cid);
            if ship > 0 {
                snap.items.insert(
                    ship,
                    InventoryItemRecord {
                        item_id: ship,
                        type_id: 587,
                        owner_id: cid,
                        location_id: if docked { 60_003_760 } else { 30_000_142 },
                        flag: if docked { InventoryFlag::Hangar } else { InventoryFlag::None },
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            }
        }
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Dead-Structure".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: "Destroyed".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 0.0,
                armor_hp: 0.0,
                hull_hp: 0.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    // 1. title_mask_grants_roles: zero mask, non-role title, and bit-mapped role titles
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.titles.clear();
        st.titles.insert(3, ("EmptyTitle3".into(), 0));
        st.titles.insert(1 << 20, ("Trader20".into(), corp_roles::ROLE_TRADER));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!title_mask_grants_roles(snap, corp_id, 0));
        assert!(!title_mask_grants_roles(snap, corp_id, 4));
        assert!(title_mask_grants_roles(snap, corp_id, 1 << 20));
    });
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().titles.insert(3, ("Acct3".into(), corp_roles::ROLE_ACCOUNTANT));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(title_mask_grants_roles(snap, corp_id, 4));
        assert!(!title_mask_grants_roles(snap, corp_id, 8));
    });

    // 2. handle_update_members: plain member denied; non-CEO director allowed to grant non-director role
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut mem_sess,
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(third_id)),
                ("roles", PyRep::Integer(corp_roles::ROLE_AUDITOR as i64)),
            ])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(mem_id, corp_roles::ROLE_DIRECTOR);
    });
    assert!(handle_update_members(
        &ctx,
        &mut mem_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(third_id)),
            ("roles", PyRep::Integer(corp_roles::ROLE_AUDITOR as i64)),
        ])],
    )
    .unwrap()
    .is_ok());

    // 3. handle_update_members corp_member_titles bit mapping (matched in g.titles, unmatched low bits, >=16 fallback, empty clear)
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.titles.clear();
        st.titles.insert(1 << 20, ("T20".into(), 0));
        st.titles.insert(1 << 21, ("T21".into(), 0));
    });
    handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(third_id)),
            ("titleMask", PyRep::Integer((1 << 20) as i64)),
        ])],
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corp_member_titles.get(&(corp_id, third_id)), Some(&vec![1 << 20]));
    });

    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().titles.clear();
    });
    handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(third_id)),
            ("titleMask", PyRep::Integer(4)),
        ])],
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corp_member_titles.get(&(corp_id, third_id)), Some(&vec![3, 4]));
    });

    handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(third_id)),
            ("titleMask", PyRep::Integer((1 << 22) as i64)),
        ])],
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corp_member_titles.get(&(corp_id, third_id)), Some(&vec![1 << 22]));
    });

    handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(third_id)),
            ("titleMask", PyRep::Integer(0)),
        ])],
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corp_member_titles.get(&(corp_id, third_id)), Some(&Vec::new()));
    });

    // 4. handle_update_members releases unauthorized structure controller with restored_ship > 0 updating caller session.ship_id
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(st_id, mem_id);
        ext.previous_ship_by_structure.insert(st_id, mem_ship_id);
        save_ext_state(snap, &ext);
    });
    mem_sess.ship_id = Some(st_id);
    handle_update_members(
        &ctx,
        &mut mem_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(mem_id)),
            ("roles", PyRep::Integer((corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_AUDITOR) as i64)),
        ])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(mem_sess.ship_id, Some(mem_ship_id));

    // 5. handle_insert_vote_case allows non-CEO director for voteType=4
    let dir_vote = crate::services::fleet_social::handle(
        &ctx,
        &mut mem_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::String("DirSanctionVote".into()),
            PyRep::String("Sanction check".into()),
            PyRep::Integer(4),
            PyRep::Integer(1),
        ],
        &std::collections::HashMap::new(),
    )
    .await
    .unwrap()
    .expect("non-CEO director InsertVoteCase succeeds");
    assert_eq!(dir_vote.dict_get("voteType").and_then(PyRep::as_i64), Some(4));

    // 6. handle_kick_out_members self-leave: act_ship > 0 assigns session.ship_id; act_ship == 0 leaves existing ship_id untouched
    let mut self_sess = ceo_sess.clone();
    self_sess.session_id = 910_000;
    self_sess.user_id = Some(self_leave_id as u64);
    self_sess.character_id = Some(self_leave_id);
    self_sess.corporation_id = Some(corp_id);
    self_sess.corp_role = 0;
    self_sess.station_id = Some(60_003_760);
    self_sess.structure_id = None;
    self_sess.ship_id = None;
    assert!(handle_kick_out_members(&ctx, &mut self_sess, &[PyRep::Integer(self_leave_id)]).unwrap().is_ok());
    assert_eq!(self_sess.ship_id, Some(self_leave_ship_id));
    let _ = ceo_id;

    let mut zero_sess = ceo_sess.clone();
    zero_sess.session_id = 910_001;
    zero_sess.user_id = Some(zero_ship_leave_id as u64);
    zero_sess.character_id = Some(zero_ship_leave_id);
    zero_sess.corporation_id = Some(corp_id);
    zero_sess.corp_role = 0;
    zero_sess.station_id = Some(60_003_760);
    zero_sess.structure_id = None;
    zero_sess.ship_id = Some(999_777);
    assert!(handle_kick_out_members(&ctx, &mut zero_sess, &[PyRep::Integer(zero_ship_leave_id)]).unwrap().is_ok());
    assert_eq!(zero_sess.ship_id, Some(999_777));

    // 7. execute_pending_auto_kicks_for_docked_char updates ship_id when act_ship > 0, moves to npc corp, and invalidates bound objects
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().pending_auto_kicks.insert(kick_docked_id, now_unix - 10);
    });
    let mut kick_sess = ceo_sess.clone();
    kick_sess.session_id = 910_002;
    kick_sess.user_id = Some(kick_docked_id as u64);
    kick_sess.character_id = Some(kick_docked_id);
    kick_sess.corporation_id = Some(corp_id);
    kick_sess.corp_role = 0;
    kick_sess.station_id = Some(60_003_760);
    kick_sess.structure_id = None;
    kick_sess.ship_id = None;
    let bound_id = 880_001u64;
    ctx.bound_objects.write().unwrap().insert(
        bound_id,
        BoundObjectEntry {
            bound_id,
            session_id: kick_sess.session_id,
            user_id: kick_docked_id as u64,
            character_id: kick_docked_id,
            service: "corpRegistry".into(),
            object_id: corp_id,
            oid_str: format!("N=1:{bound_id}"),
            ref_id: 1,
        },
    );
    execute_pending_auto_kicks_for_docked_char(&ctx, &mut kick_sess, kick_docked_id);
    assert_eq!(kick_sess.ship_id, Some(kick_ship_id));
    assert_ne!(kick_sess.corporation_id, Some(corp_id));
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&bound_id));
}
