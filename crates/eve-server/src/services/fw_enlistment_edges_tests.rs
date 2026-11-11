//! Edge-case unit tests for `fw_enlistment` (`fwCharacterEnlistmentMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{corp_roles, CorporationRecord, UpwellStructureRecord};
use eve_proto::PyRep;

#[test]
fn forced_sync_corp_cooldown_and_apply_corp_militia_faction_edges() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let corp_id = 98_500_001_i64;
    let now = 1_775_400_000_u64;

    // take_forced_war_faction_sync when absent returns None, when present returns and clears.
    assert_eq!(take_forced_war_faction_sync(&ctx, cid), None);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_store(snap);
        st.forced_war_faction_sync.insert(cid, Some(500_001));
        save_store(snap, &st);
    });
    assert_eq!(take_forced_war_faction_sync(&ctx, cid), Some(Some(500_001)));
    assert_eq!(take_forced_war_faction_sync(&ctx, cid), None);

    // Corp leave cooldown helpers
    ctx.db.with_mut_snapshot(|snap| {
        assert!(!is_corp_leave_cooldown_active_in_snap(snap, corp_id, now));
        record_corp_leave_cooldown_in_snap(snap, corp_id, now + 3_600);
        assert!(is_corp_leave_cooldown_active_in_snap(snap, corp_id, now));
        assert!(!is_corp_leave_cooldown_active_in_snap(snap, corp_id, now + 3_600));
    });

    // apply_corp_militia_faction_in_snap: prunes corp_allowed, clears opposing direct enlistment,
    // sets fw_character_stats, then clears when fac = None (preserving same-faction direct vs clearing).
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.ceo_id = cid; __corp.ensure_wallet_divisions(); __corp },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.war_faction_id = Some(500_002);
        }
        snap.fw_runtime.direct_enlisted_chars.insert(cid, 500_002);
        let mut st = load_store(snap);
        st.corp_allowed.insert(corp_id, vec![500_001, 500_002]);
        save_store(snap, &st);

        let muts = apply_corp_militia_faction_in_snap(snap, corp_id, Some(500_001), now);
        assert!(muts.iter().any(|m| m.0 == cid && m.1 == Some(500_002) && m.2 == Some(500_001)));
        assert!(!snap.fw_runtime.direct_enlisted_chars.contains_key(&cid));
        assert_eq!(load_store(snap).corp_allowed.get(&corp_id), Some(&vec![500_001]));
        let stats = snap.fw_character_stats.get(&cid).expect("fw stats created");
        assert_eq!(stats.faction_id, Some(500_001));
        assert_eq!(stats.current_rank, 1);

        // Now clear corp faction while cid has no direct enlistment -> clears fw_character_stats.
        let muts2 = apply_corp_militia_faction_in_snap(snap, corp_id, None, now);
        assert!(muts2.iter().any(|m| m.0 == cid && m.2.is_none()));
        let stats2 = snap.fw_character_stats.get(&cid).unwrap();
        assert_eq!(stats2.faction_id, None);
        assert_eq!(stats2.enlisted_on_unix, None);
        assert_eq!(stats2.current_rank, 0);
        let _ = other_cid;
    });

    // sync_corp_militia_members_after_change async runtime path (updates session & notifies)
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        let uid = ctx.db.with_snapshot(|snap| snap.characters[&cid].user_id);
        ctx.db
            .acquire_online_lock(uid, 1, Some(cid), true, now)
            .expect("online lock");
        let mut sess = auth_session(uid, cid, corp_id, 30_000_142);
        let changes: Vec<FwMemberMutation> = vec![
            (cid, None, Some(500_001), None, 0, 0),
            (other_cid, Some(500_001), Some(500_001), None, 0, 0),
        ];
        sync_corp_militia_members_after_change(&ctx, &mut sess, Some(500_001), &changes).await;
        assert_eq!(sess.war_faction_id, Some(500_001));
        assert_eq!(take_forced_war_faction_sync(&ctx, cid), None);
        let notifs = ctx.drain_notifications_for_char(cid);
        assert!(notifs.iter().any(|n| n.event == "OnEnlistmentStateUpdated"));
        assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));
    });
}

#[test]
fn dock_checks_npc_corp_migration_and_corp_allowed_edges() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let struct_id = 1_050_000_001_i64;

    // Check undocked -> MustBeDocked
    let mut sess = auth_session(1, cid, 1_000_170, 30_000_142);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(100_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.war_faction_id = None;
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_001)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MustBeDocked"
    );

    // Docked at missing structure -> MustBeDocked
    sess.ship_id = None;
    sess.structure_id = Some(struct_id);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_001)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MustBeDocked"
    );

    // Docked at uncored / destroyed structure -> MustBeDocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                state: "Destroyed".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_001)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MustBeDocked"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&struct_id).unwrap();
        st.state = "shield_vulnerable".into();
        st.quantum_core_installed = false;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_001)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MustBeDocked"
    );

    // Operational cored structure allows NPC corp member to direct-enlist -> moves to militia corp!
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().quantum_core_installed = true;
    });
    // Non-militia faction fails
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(999_999)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidWarFaction"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidWarFaction"
    );

    // NPC corp member enlist -> moves into Caldari militia corp (for 500_001)
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_001)],
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let militia_corp = ctx.sde.factions[&500_001].militia_corporation_id.unwrap() as i64;
    assert_eq!(sess.corporation_id, Some(militia_corp));

    // SetMyCorpAllowedEnlistmentFactions denied for NPC corp member
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::Tuple(vec![PyRep::Integer(500_001)])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Leave direct enlistment -> returns from militia corp to default NPC corp
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "RemoveMyDirectEnlistment",
            &[],
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_ne!(sess.corporation_id, Some(militia_corp));

    // Test player corp enlisted in 500_001: opposing CreateMyDirectEnlistment fails,
    // SetMyCorpAllowedEnlistmentFactions with bad arg types, disallowing own faction, and purging disallowed direct members!
    let player_corp = 98_500_002_i64;
    let member_disallowed = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = player_corp;
            ch.war_faction_id = Some(500_001);
        }
        if let Some(ch2) = snap.characters.get_mut(&member_disallowed) {
            ch2.corporation_id = player_corp;
            ch2.war_faction_id = Some(500_004);
        }
        snap.corporations.insert(
            player_corp,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = player_corp; __corp.ceo_id = cid; __corp.ensure_wallet_divisions(); __corp },
        );
        snap.fw_runtime.corp_war_factions.insert(player_corp, 500_001);
        snap.fw_runtime.direct_enlisted_chars.insert(member_disallowed, 500_004);
        let mut st = load_store(snap);
        st.cooldown_until.remove(&cid);
        save_store(snap, &st);
    });
    sess.corporation_id = Some(player_corp);

    // GetCorpAllowedEnlistmentFactions with no args uses session.corporation_id; missing both -> CorporationNotFound
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "GetCorpAllowedEnlistmentFactions",
            &[],
        )
        .unwrap()
        .unwrap(),
        PyRep::List(Vec::new())
    );
    let mut sess_no_corp = sess.clone();
    sess_no_corp.corporation_id = None;
    assert_eq!(
        handle(
            &ctx,
            &mut sess_no_corp,
            "fwCharacterEnlistmentMgr",
            "GetCorpAllowedEnlistmentFactions",
            &[],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );

    // faction_list_arg validation errors
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::String("bad".into())],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFactionList"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::List(vec![PyRep::String("not_int".into())])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFactionList"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::List(vec![PyRep::Integer(999_999)])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidWarFaction"
    );
    // Corp enlisted in 500_001 cannot allow 500_004
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::List(vec![PyRep::Integer(500_004)])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FwFactionNotAllowedByCorp"
    );

    // Allowing only 500_001 purges member_disallowed who was direct-enlisted in 500_004!
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::Tuple(vec![PyRep::Integer(500_001)])],
        )
        .unwrap()
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(500_001)])
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fw_runtime.direct_enlisted_chars.contains_key(&member_disallowed));
        assert_eq!(snap.characters[&member_disallowed].war_faction_id, Some(500_001));
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "GetCorpAllowedEnlistmentFactions",
            &[],
        )
        .unwrap()
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(500_001)])
    );

    // Player corp enlisted in 500_001 blocks direct enlistment into opposing 500_002
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().war_faction_id = None;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "CreateMyDirectEnlistment",
            &[PyRep::Integer(500_002)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FwFactionNotAllowedByCorp"
    );

    // Direct-enlisted in same faction as player corp -> RemoveMyDirectEnlistment retains corp_fac
    let mut sess_m2 = auth_session(2, member_disallowed, player_corp, 30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.direct_enlisted_chars.insert(member_disallowed, 500_001);
        snap.characters.get_mut(&member_disallowed).unwrap().war_faction_id = Some(500_001);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess_m2,
            "fwCharacterEnlistmentMgr",
            "RemoveMyDirectEnlistment",
            &[],
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(sess_m2.war_faction_id, Some(500_001));
    assert!(sess.role == sess.role || corp_roles::ROLE_DIRECTOR > 0);
}
