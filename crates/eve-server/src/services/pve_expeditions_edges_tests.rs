//! Edge-case behavioral unit tests for `pve_expeditions`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::cache_notifications::PendingMachoNotification;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::pve_exploration::with_pve_exploration_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, ServerClock, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

fn make_char(cid: i64, sys_id: u32, station_id: Option<i64>) -> CharacterRecord {
    CharacterRecord {
        activity_status: None,
        character_id: cid,
        user_id: cid as u64,
        name: format!("ExpPilot_{cid}"),
        corporation_id: 98_000_001,
        alliance_id: None,
        war_faction_id: None,
        station_id,
        structure_id: None,
        solar_system_id: sys_id,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: 0,
        balance: eve_db::IskCents(0),
        plex_balance: 0,
        loyalty_points: Default::default(),
        standings: Default::default(),
        security_status: 0.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 11,
        neural: eve_db::NeuralAttributes::default(),
        skill_points: 0.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

fn make_exp_ctx() -> (ServerContext, Arc<VirtualServerClock>) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_785_000_000));
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(test_ctx().sde.clone(), db);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escalation_create_advance_notifications_expiry_and_fallbacks() {
    let (ctx, clock) = make_exp_ctx();
    let char_id = 90_800_001_i64;
    let uid = char_id as u64;
    let origin_sys = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("exp_{uid}"),
            AccountRecord {
                user_id: uid,
                username: format!("exp_{uid}"),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.characters.insert(char_id, make_char(char_id, origin_sys, None));
    });
    ctx.db
        .acquire_online_lock(uid, uid + 1000, Some(char_id), true, ctx.now_unix())
        .unwrap();

    // Seed character at step destinations and an extra docked/remote char to verify notification filtering
    let first = create_escalation_expedition(&ctx, char_id, origin_sys, 391_783, "Angel Hideout", [0.0; 3])
        .await
        .expect("create escalation expedition");

    // Bookmark & signature & notification created
    let sig1 = expedition_sig_id(first.instance_id, first.step_index);
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .bookmarks
            .values()
            .any(|b| b.owner_id == char_id && b.item_id == Some(first.instance_id)));
        assert!(snap
            .cosmic_signatures
            .get(&first.destination_solar_system_id)
            .is_some_and(|l| l.iter().any(|s| s.signature_id == sig1 && !s.collapsed)));
    });
    assert!(!ctx.drain_notifications_for_char(char_id).is_empty());

    // Advance step 1 -> 2, updating bookmark & collapsing sig1 & emitting sig2
    let step2 = advance_expedition_step(&ctx, char_id, first.instance_id)
        .await
        .unwrap()
        .expect("step 2");
    assert_eq!(step2.step_index, 2);
    let sig2 = expedition_sig_id(step2.instance_id, step2.step_index);
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .cosmic_signatures
            .get(&first.destination_solar_system_id)
            .is_some_and(|l| l.iter().any(|s| s.signature_id == sig1 && s.collapsed)));
        assert!(snap
            .cosmic_signatures
            .get(&step2.destination_solar_system_id)
            .is_some_and(|l| l.iter().any(|s| s.signature_id == sig2 && !s.collapsed)));
        let bm = snap
            .bookmarks
            .values()
            .find(|b| b.owner_id == char_id && b.item_id == Some(first.instance_id))
            .unwrap();
        assert!(bm.label.contains("[Step 2/"));
    });

    // Advance to final completion -> drops loot 19419 & removes bookmark
    let mut cur = step2;
    while cur.step_index < cur.total_steps {
        cur = advance_expedition_step(&ctx, char_id, cur.instance_id)
            .await
            .unwrap()
            .unwrap();
    }
    let final_sys = cur.destination_solar_system_id;
    let done = advance_expedition_step(&ctx, char_id, cur.instance_id)
        .await
        .unwrap()
        .expect("completion entry");
    assert!(done.journal_text.contains("fully completed"));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap
            .bookmarks
            .values()
            .any(|b| b.owner_id == char_id && b.item_id == Some(first.instance_id)));
        assert!(snap.items.values().any(|it| {
            it.type_id == 19419
                && (it.location_id == i64::from(final_sys)
                    || snap
                        .items
                        .get(&it.location_id)
                        .is_some_and(|c| c.location_id == i64::from(final_sys)))
        }));
    });
    // Non-existent instance after completion -> Ok(None)
    assert!(advance_expedition_step(&ctx, char_id, first.instance_id)
        .await
        .unwrap()
        .is_none());

    // Expiry pruning via get_my_escalating_path_details_rowset collapses sig & notifies
    let exp_entry = create_escalation_expedition(&ctx, char_id, origin_sys, 391_783, "Expire Test", [0.0; 3])
        .await
        .unwrap();
    let exp_sig = expedition_sig_id(exp_entry.instance_id, exp_entry.step_index);
    let _ = ctx.drain_notifications_for_char(char_id);
    clock.advance_secs(86_405);
    let empty_rowset = get_my_escalating_path_details_rowset(&ctx, char_id);
    assert!(matches!(empty_rowset, PyRep::Rowset { .. }));
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnEscalatingPathChange"));
    assert!(ctx.db.with_snapshot(|snap| {
        !snap
            .bookmarks
            .values()
            .any(|b| b.owner_id == char_id && b.item_id == Some(exp_entry.instance_id))
            && snap
                .cosmic_signatures
                .get(&exp_entry.destination_solar_system_id)
                .is_some_and(|l| l.iter().any(|s| s.signature_id == exp_sig && s.collapsed))
    }));

    // Empty rowset on untouched char + unknown dungeon fallback kv
    assert!(matches!(get_my_escalating_path_details_rowset(&ctx, 99_999_111), PyRep::Rowset { .. }));
    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.entry(char_id).or_default().push(EscalatingPathEntry {
            instance_id: 99_001,
            character_id: char_id,
            source_dungeon_id: 999_999_991,
            expedition_dungeon_id: 999_999_992,
            expedition_name: "UnknownFallback".into(),
            destination_solar_system_id: origin_sys,
            step_index: 1,
            total_steps: 2,
            journal_text: "Text".into(),
            created_at_sec: clock.now_unix_secs(),
            expires_at_sec: clock.now_unix_secs() + 1_000,
            dungeons_entered: vec![999_999_991],
            journal_entry_template_id: 0,
        });
    });
    let fallback_rowset = get_my_escalating_path_details_rowset(&ctx, char_id);
    assert!(matches!(fallback_rowset, PyRep::Rowset { .. }));

    // emit_expedition_destruction_notifications filtering & 512 queue cap
    let c_space = 90_800_050_i64;
    let c_docked = 90_800_051_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(c_space, make_char(c_space, origin_sys, None));
        snap.characters.insert(c_docked, make_char(c_docked, origin_sys, Some(60_003_760)));
    });
    let (dummy_pkt, _) = ServerContext::build_macho_notification_packet(
        c_space,
        Some(c_space as u64),
        "Dummy",
        "charid",
        vec![],
    );
    let pending: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::from([(
        c_space,
        vec![
            PendingMachoNotification { seq_id: 0,
                recipient_char_id: c_space,
                event: "Dummy".into(),
                id_type: "charid".into(),
                args: vec![],
                packet: dummy_pkt,
                marshaled_packet: vec![],
                created_filetime: 0,
            };
            512
        ],
    )]));
    let (tx, _) = broadcast::channel(16);
    let adv_sample = EscalatingPathEntry {
        instance_id: 88_001,
        character_id: c_space,
        source_dungeon_id: 391_783,
        expedition_dungeon_id: 391_784,
        expedition_name: "Test".into(),
        destination_solar_system_id: origin_sys,
        step_index: 2,
        total_steps: 3,
        journal_text: "".into(),
        created_at_sec: 100,
        expires_at_sec: 1000,
        dungeons_entered: vec![391_783],
        journal_entry_template_id: 1,
    };
    emit_expedition_destruction_notifications(
        &ctx.db,
        &pending,
        &tx,
        Some((origin_sys, 391_783)),
        None,
        Some(&adv_sample),
    );
    let q_guard = pending.read().unwrap();
    assert_eq!(q_guard[&c_space].len(), 512);
    assert!(!q_guard.contains_key(&c_docked));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_advance_pve_site_room_and_enter_combat_anomaly_edges() {
    let (ctx, clock) = make_exp_ctx();
    let char_id = 90_800_100_i64;
    let ship_id = 1_000_800_100_i64;
    let sys_id = 30_000_142_u32;

    // Seed ship item and site with sleeper cache (unstabilized) & DED site
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sleeper_did = 491_000_u32;
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "SLEEPER-UNSTAB".into(),
            dungeon_id: sleeper_did,
            dungeon_name: "Limited Sleeper Cache".into(),
            solar_system_id: sys_id,
            site_category: "SleeperCache".into(),
            position: [1_000.0, 0.0, 0.0],
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 1,
            sleeper_rift_stabilized: false,
            sleeper_sentry_disabled: true,
            lifetime_seconds: 0, // tests fallback 86_400
            created_unix: clock.now_unix_secs(),
            ..Default::default()
        });
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "EXP-SITE-BY-DID".into(),
            dungeon_id: 391_785,
            dungeon_name: "Escalation Site".into(),
            solar_system_id: sys_id,
            site_category: "Combat".into(),
            position: [500.0, 0.0, 0.0],
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 2,
            lifetime_seconds: 0,
            created_unix: clock.now_unix_secs(),
            expedition_instance_id: None,
            ..Default::default()
        });
        reg.escalations_by_char.entry(char_id).or_default().push(EscalatingPathEntry {
            instance_id: 55_001,
            character_id: char_id,
            source_dungeon_id: 391_783,
            expedition_dungeon_id: 391_785,
            expedition_name: "Esc".into(),
            destination_solar_system_id: sys_id,
            step_index: 1,
            total_steps: 2,
            journal_text: "".into(),
            created_at_sec: clock.now_unix_secs(),
            expires_at_sec: clock.now_unix_secs() + 10_000,
            dungeons_entered: vec![391_783],
            journal_entry_template_id: 0,
        });
    });

    // 1. ship_id == None -> NotInSpace
    let mut sess_no_ship = auth_session(1, char_id, 98_000_001, sys_id);
    sess_no_ship.ship_id = None;
    assert_eq!(
        validate_and_advance_pve_site_room(&ctx, &mut sess_no_ship, sys_id, "SLEEPER-UNSTAB")
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );

    // 2. Spawn ship near sleeper site (< 2500m), unstabilized SleeperCache -> validate_sleeper_cache_passage Err
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(1_100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let mut sess = auth_session(1, char_id, 98_000_001, sys_id);
    sess.ship_id = Some(ship_id);
    assert!(
        validate_and_advance_pve_site_room(&ctx, &mut sess, sys_id, "SLEEPER-UNSTAB")
            .await
            .is_err()
    );

    // 3. Unknown site_id delegates and errors cleanly
    assert!(
        validate_and_advance_pve_site_room(&ctx, &mut sess, sys_id, "NO_SUCH_SITE")
            .await
            .is_err()
    );

    // 4. Advance by dungeon_id string: unauthenticated session is rejected, authenticated session advances wave
    let mut no_char_sess = SessionState::default();
    no_char_sess.solar_system_id = Some(sys_id);
    no_char_sess.ship_id = Some(ship_id);
    assert!(
        validate_and_advance_pve_site_room(&ctx, &mut no_char_sess, sys_id, "391785")
            .await
            .is_err()
    );
    let adv_ok = validate_and_advance_pve_site_room(&ctx, &mut sess, sys_id, "391785")
        .await
        .unwrap();
    assert_eq!(adv_ok.dict_get("currentWave"), Some(&PyRep::Integer(2)));

    // 5. enter_combat_anomaly_site matches (dest_sys, expedition_dungeon_id) when expedition_instance_id == None
    let entered = enter_combat_anomaly_site(&ctx, char_id, sys_id, "EXP-SITE-BY-DID").unwrap();
    assert_eq!(entered.dict_get("dungeonID"), Some(&PyRep::Integer(391_785)));
    with_pve_exploration_reg(&ctx, |reg| {
        assert!(reg.escalations_by_char[&char_id][0]
            .dungeons_entered
            .contains(&391_785));
    });
}
