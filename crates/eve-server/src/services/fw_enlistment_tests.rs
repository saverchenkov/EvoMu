//! Unit tests for `fw_enlistment` (`fwCharacterEnlistmentMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{corp_roles, CorporationRecord};
use eve_proto::PyRep;

#[test]
fn fw_enlistment_direct_join_leave_cooldown_and_corp_allowed_factions() {
    assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);

    let ctx = test_ctx();
    assert!(is_militia_faction(&ctx, 500_001));
    assert!(is_militia_faction(&ctx, 500_004));
    assert!(!is_militia_faction(&ctx, 999_999));

    let cid = 90_000_001_i64;
    let player_corp = 98_000_001_i64;
    let mut sess = auth_session(1, cid, player_corp, 30_000_142);

    let other_svc = "other";
    let unknown_method = "Unknown";
    assert!(handle(&ctx, &mut sess, other_svc, "GetMyEnlistment", &[]).is_none());
    assert!(handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", unknown_method, &[]).is_none());

    // Initial state: not enlisted
    assert_eq!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "GetMyEnlistment", &[])
            .unwrap()
            .unwrap(),
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None])
    );

    // Ensure player_corp exists with cid as CEO and configure corp allowed militia factions
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = player_corp;
            ch.war_faction_id = None;
        }
        snap.corporations.insert(
            player_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = player_corp;
                __corp.ceo_id = cid;
                __corp },
        );
    });

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "GetCorpAllowedEnlistmentFactions",
            &[PyRep::Integer(player_corp)],
            )
            .unwrap()
            .unwrap(),
        PyRep::List(Vec::new())
    );

    let set_allowed = handle(
        &ctx,
        &mut sess,
        "fwCharacterEnlistmentMgr",
        "SetMyCorpAllowedEnlistmentFactions",
        &[PyRep::List(vec![
            PyRep::Integer(500_004),
            PyRep::Integer(500_001),
            PyRep::Integer(500_001),
        ])],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        set_allowed,
        PyRep::List(vec![PyRep::Integer(500_001), PyRep::Integer(500_004)])
    );

    // Non-director cannot set allowed factions
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&player_corp).unwrap().ceo_id = 90_000_002;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::List(vec![PyRep::Integer(500_001)])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&player_corp)
            .unwrap()
            .member_roles
            .insert(cid, corp_roles::ROLE_DIRECTOR);
    });

    // Enlistment blocked by corp allowed list (500_002 not in [500_001, 500_004])
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

    // Negative standing blocks enlistment
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&cid)
            .unwrap()
            .standings
            .insert(500_001, -0.5);
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
        "FwStandingTooLow"
    );

    // Valid direct enlistment
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&cid)
            .unwrap()
            .standings
            .insert(500_001, 1.0);
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
        .unwrap(),
        PyRep::None
    );
    assert_eq!(sess.war_faction_id, Some(500_001));
    assert_eq!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "GetMyEnlistment", &[])
            .unwrap()
            .unwrap(),
        PyRep::Tuple(vec![
            PyRep::Integer(500_001),
            PyRep::Integer(500_001),
            PyRep::None,
        ])
    );

    // Duplicate enlistment rejected
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
        "FwAlreadyEnlisted"
    );

    // Leave militia -> sets 24h cooldown
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
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "RemoveMyDirectEnlistment",
            &[],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FwNotEnlisted"
    );

    let cd_rep = handle(
        &ctx,
        &mut sess,
        "fwCharacterEnlistmentMgr",
        "GetMyDirectEnlistmentCooldownTimestamp",
        &[],
    )
    .unwrap()
    .unwrap();
    assert!(cd_rep.as_i64().unwrap() > 0);
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
        "FwEnlistmentCooldown"
    );

    // Expire cooldown and verify it clears
    let _ = ctx.db.acquire_online_lock(1, 701, Some(cid), true, ctx.now_unix());
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_store(snap);
        st.cooldown_until.insert(cid, 1);
        save_store(snap, &st);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "GetMyDirectEnlistmentCooldownTimestamp",
            &[],
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    // Expired cooldown clears and emits OnEnlistmentCooldownCleared
    let drained = ctx.drain_notifications_for_char(cid);
    assert!(drained.iter().any(|n| n.event == "OnEnlistmentCooldownCleared"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_enlistment_structures_npc_corp_migration_corp_sync_and_purge() {
    use eve_db::UpwellStructureRecord;
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;

    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let cid2 = 90_000_002_i64;
    let player_corp = 98_000_010_i64;
    let sys_id = 30_000_142_u32;
    let online_struct = 1_000_000_801_i64;
    let uncored_struct = 1_000_000_802_i64;
    let anchoring_struct = 1_000_000_803_i64;
    let now = ctx.now_unix();

    // Bring cid & cid2 online so notifications can be drained
    ctx.db
        .acquire_online_lock(1, 501, Some(cid), true, now)
        .expect("online 1");
    ctx.db
        .acquire_online_lock(2, 502, Some(cid2), true, now)
        .expect("online 2");

    let mut sess = auth_session(1, cid, 1_000_006, sys_id);

    // 1. Structure docking checks & invalid args
    assert_eq!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[])
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
            &[PyRep::Integer(999_999)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidWarFaction"
    );

    // Undocked -> MustBeDocked
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(100_000_001);
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

    // Missing structure -> MustBeDocked
    sess.structure_id = Some(9_999_999);
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

    // Uncored & Anchoring structures -> MustBeDocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            online_struct,
            UpwellStructureRecord {
                structure_id: online_struct,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            uncored_struct,
            UpwellStructureRecord {
                structure_id: uncored_struct,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                ..Default::default()
            },
        );
        snap.structures.insert(
            anchoring_struct,
            UpwellStructureRecord {
                structure_id: anchoring_struct,
                solar_system_id: sys_id,
                state: "Anchoring".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
    });
    for bad_sid in [uncored_struct, anchoring_struct] {
        sess.structure_id = Some(bad_sid);
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
    }

    // 2. NPC-corp character enlisting while docked in operational cored structure migrates to militia corp and back on removal
    sess.structure_id = Some(online_struct);
    let expected_militia_corp = ctx
        .sde
        .factions
        .get(&500_001)
        .and_then(|f| f.militia_corporation_id)
        .map(i64::from);
    assert!(handle(
        &ctx,
        &mut sess,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500_001)],
    )
    .unwrap()
    .is_ok());
    if let Some(mc) = expected_militia_corp {
        assert_eq!(sess.corporation_id, Some(mc));
    }
    assert!(handle(
        &ctx,
        &mut sess,
        "fwCharacterEnlistmentMgr",
        "RemoveMyDirectEnlistment",
        &[],
    )
    .unwrap()
    .is_ok());
    assert_eq!(sess.war_faction_id, None);
    assert_ne!(sess.corporation_id, expected_militia_corp);

    // 3. NPC corp cannot SetMyCorpAllowedEnlistmentFactions
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::List(vec![PyRep::Integer(500_001)])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // 4. GetCorpAllowedEnlistmentFactions without args when session.corporation_id is None -> CorporationNotFound
    let mut no_corp_sess = sess.clone();
    no_corp_sess.corporation_id = None;
    assert_eq!(
        handle(
            &ctx,
            &mut no_corp_sess,
            "fwCharacterEnlistmentMgr",
            "GetCorpAllowedEnlistmentFactions",
            &[],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );

    // 5. Player corp + apply_corp_militia_faction_in_snap + sync_corp_militia_members_after_change + purge
    let ship2_id = ctx.db.with_snapshot(|s| s.characters[&cid2].active_ship_id);
    let ship2_tid = ctx.db.with_snapshot(|s| s.items.get(&ship2_id).map(|i| i.type_id).unwrap_or(606));
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(Ball {
            id: ship2_id,
            type_id: ship2_tid,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            player_corp,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = player_corp; __corp.ceo_id = cid; __corp.ensure_wallet_divisions(); __corp },
        );
        for &c in &[cid, cid2] {
            if let Some(ch) = snap.characters.get_mut(&c) {
                ch.corporation_id = player_corp;
                ch.war_faction_id = None;
            }
        }
        snap.fw_runtime.direct_enlisted_chars.insert(cid2, 500_002);
        let mut st = load_store(snap);
        st.cooldown_until.remove(&cid);
        st.corp_allowed.insert(player_corp, vec![500_001, 500_002]);
        save_store(snap, &st);
        record_corp_join_cooldown_test_helper(snap, player_corp, now + 3600);
    });
    sess.corporation_id = Some(player_corp);

    // Corp cooldown helpers
    ctx.db.with_snapshot(|snap| {
        assert!(is_corp_leave_cooldown_active_in_snap(snap, player_corp, now));
        assert!(!is_corp_leave_cooldown_active_in_snap(snap, player_corp, now + 3600));
    });

    // apply_corp_militia_faction_in_snap(Some(500_001)) retains only 500_001 in corp_allowed, clears cid2's conflicting 500_002
    let changes = ctx.db.with_mut_snapshot(|snap| {
        apply_corp_militia_faction_in_snap(snap, player_corp, Some(500_001), now)
    });
    assert_eq!(changes.len(), 2);
    sync_corp_militia_members_after_change(&ctx, &mut sess, Some(500_001), &changes).await;
    assert_eq!(sess.war_faction_id, Some(500_001));
    assert_eq!(take_forced_war_faction_sync(&ctx, cid), None); // already consumed for session's char
    assert_eq!(take_forced_war_faction_sync(&ctx, cid2), Some(Some(500_001)));
    assert_eq!(take_forced_war_faction_sync(&ctx, cid2), None);
    let _ = ctx.drain_notifications_for_char(cid);
    let _ = ctx.drain_notifications_for_char(cid2);

    // Setting corp allowed factions to include opposing 500_002 while corp is in 500_001 fails
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::Tuple(vec![PyRep::Integer(500_002)])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FwFactionNotAllowedByCorp"
    );
    // Invalid faction list types & unknown faction
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
            &[PyRep::List(vec![PyRep::String("bad".into())])],
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

    // GetCorpAllowedEnlistmentFactions with no args returns [500_001]
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

    // Clear corp war faction -> resets members & fw_character_stats
    let clear_changes = ctx.db.with_mut_snapshot(|snap| {
        apply_corp_militia_faction_in_snap(snap, player_corp, None, now)
    });
    sync_corp_militia_members_after_change(&ctx, &mut sess, None, &clear_changes).await;
    assert_eq!(sess.war_faction_id, None);

    // Direct enlist cid2 into 500_002, then CEO sets allowed factions to [] -> purges cid2 & emits corp/char notifications
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.direct_enlisted_chars.insert(cid2, 500_002);
        snap.characters.get_mut(&cid2).unwrap().war_faction_id = Some(500_002);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fwCharacterEnlistmentMgr",
            "SetMyCorpAllowedEnlistmentFactions",
            &[PyRep::None],
        )
        .unwrap()
        .unwrap(),
        PyRep::List(vec![])
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fw_runtime.direct_enlisted_chars.contains_key(&cid2));
        assert_eq!(snap.characters[&cid2].war_faction_id, None);
    });
    assert!(ctx
        .drain_notifications_for_char(cid2)
        .iter()
        .any(|n| n.event == "OnEnlistmentStateUpdated"));
    assert!(ctx
        .drain_notifications_for_char(cid)
        .iter()
        .any(|n| n.event == "OnCorpAllowedEnlistmentFactionsSet"));
}

fn record_corp_join_cooldown_test_helper(
    snap: &mut eve_db::WorldStateSnapshot,
    corp_id: i64,
    until_unix: u64,
) {
    record_corp_leave_cooldown_in_snap(snap, corp_id, until_unix);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_enlistment_docking_npc_corp_purge_and_ballpark_sync() {
    use eve_db::UpwellStructureRecord;

    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let cid2 = 90_000_002_i64;
    let npc_corp = 1_000_125_i64;
    let player_corp = 98_000_055_i64;
    let sys_id = 30_000_142_u32;
    let struct_id = 1_055_000_001_i64;

    // Seed ship for ballpark slim update + structure + second char in player corp
    let ship_id = 100_555_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = npc_corp;
            ch.station_id = None;
            ch.structure_id = None;
            ch.war_faction_id = None;
        }
        let mut ch2 = snap.characters.get(&cid).cloned().unwrap();
        ch2.character_id = cid2;
        ch2.name = "Purged Pilot".into();
        ch2.corporation_id = player_corp;
        ch2.active_ship_id = ship_id;
        ch2.station_id = Some(60_003_760);
        snap.characters.insert(cid2, ch2);

        let mut corp = CorporationRecord::default();
        corp.corporation_id = player_corp;
        corp.name = "Enlist Corp".into();
        corp.ticker = "ENLC".into();
        corp.ceo_id = cid2;
        corp.members.insert(cid2);
        corp.member_roles.insert(cid2, corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(player_corp, corp);

        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: cid2,
                location_id: sys_id as i64,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: player_corp,
                solar_system_id: sys_id,
                name: "Enlist Fortizar".into(),
                state: "Unanchored".into(),
                quantum_core_installed: false,
                ..Default::default()
            },
        );
    });
    // Spawn ship into system ballpark so `find_ball_system_sync` hits lines 225-232 & 683-690
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    {
        let mut bp = sys_h.ballpark.write().unwrap();
        let ball = eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        bp.add_ball(ball);
    }

    let mut sess = auth_session(1, cid, npc_corp, sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    // 1. Undocked direct enlistment -> MustBeDocked
    assert_eq!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
            .unwrap()
            .unwrap_err()
            .code,
        "MustBeDocked"
    );
    // Docked at non-existent structure -> MustBeDocked
    sess.ship_id = None;
    sess.structure_id = Some(9_999_999);
    assert_eq!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
            .unwrap()
            .unwrap_err()
            .code,
        "MustBeDocked"
    );
    // Docked at unanchored / un-cored structure -> MustBeDocked
    sess.structure_id = Some(struct_id);
    assert_eq!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
            .unwrap()
            .unwrap_err()
            .code,
        "MustBeDocked"
    );
    // Online & cored structure -> succeeds & moves NPC-corp pilot into militia corp, then RemoveMyDirectEnlistment restores NPC corp
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&struct_id) {
            st.state = "ShieldVulnerable".into();
            st.quantum_core_installed = true;
        }
    });
    assert!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
            .unwrap()
            .is_ok()
    );
    assert!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "RemoveMyDirectEnlistment", &[])
            .unwrap()
            .is_ok()
    );

    // 2. NPC corp calling SetMyCorpAllowedEnlistmentFactions -> CorpRoleDenied
    assert_eq!(
        handle(&ctx, &mut sess, "fwCharacterEnlistmentMgr", "SetMyCorpAllowedEnlistmentFactions", &[PyRep::List(vec![PyRep::Integer(500_001)])])
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // 3. Player corp member in opposing faction -> FwFactionNotAllowedByCorp, then direct enlist & purge via SetMyCorpAllowedEnlistmentFactions + sync_corp_enlistment_members
    let mut sess2 = auth_session(2, cid2, player_corp, sys_id);
    assert!(
        handle(&ctx, &mut sess2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
            .unwrap()
            .is_ok()
    );
    // Set allowed to [500_001] -> purges cid2 from 500_004 and updates ship slim item in ballpark!
    assert!(
        handle(&ctx, &mut sess2, "fwCharacterEnlistmentMgr", "SetMyCorpAllowedEnlistmentFactions", &[PyRep::List(vec![PyRep::Integer(500_001)])])
            .unwrap()
            .is_ok()
    );
    // Set corp_war_factions[player_corp] = 500_001, then test trying to join 500_004 -> FwFactionNotAllowedByCorp
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.corp_war_factions.insert(player_corp, 500_001);
    });
    assert_eq!(
        handle(&ctx, &mut sess2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
            .unwrap()
            .unwrap_err()
            .code,
        "FwFactionNotAllowedByCorp"
    );
    assert_eq!(
        handle(&ctx, &mut sess2, "fwCharacterEnlistmentMgr", "SetMyCorpAllowedEnlistmentFactions", &[PyRep::List(vec![PyRep::Integer(500_004)])])
            .unwrap()
            .unwrap_err()
            .code,
        "FwFactionNotAllowedByCorp"
    );
    // Call async sync_corp_militia_members_after_change
    sync_corp_militia_members_after_change(
        &ctx,
        &mut sess2,
        Some(500_001),
        &[(cid2, None, Some(500_001), None, ship_id, 587)],
    )
    .await;
    tokio::task::yield_now().await;
}
