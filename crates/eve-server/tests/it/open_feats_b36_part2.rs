//! Integration tests for Batch 36 open `feat:pve.*` parity rows (part 2).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_open_feats_b36::{
    SLEEPER_SENTRY_RESTLESS, SLEEPER_SENTRY_VIGILANT, SLEEPER_SENTRY_WAKEFUL,
};
use glam::DVec3;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

#[tokio::test]
async fn test_b36_ess_grid_restrictions_main_autopayout_and_reserve_tiers() {
    // parity: feat:pve.ess-grid-warp-mwd-restrictions, feat:pve.ess-main-bank-autopayout, feat:pve.ess-reserve-bank-key-tiers
    eve_testkit::attest(&[
        "feat:pve.ess-grid-warp-mwd-restrictions",
        "feat:pve.ess-main-bank-autopayout",
        "feat:pve.ess-reserve-bank-key-tiers",
    ]);
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= -0.1 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759); // Null-sec
    let pilot_a = 100_001_i64;
    let pilot_b = 100_002_i64;
    let ship_a = 960_001_i64;
    let ess_id = 820_000_000 + sys_id as i64;
    let kw = HashMap::new();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ess_id, type_id: 32882, position: DVec3::ZERO,
                velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1_000_000.0,
                radius: 500.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_a, type_id: 587, position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0,
                radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;

    // 1. Inside 75 km ESS intrusion bubble: warp, MWD, MJD, cloak, cyno all blocked
    for action in ["warp", "mwd", "mjd", "cloak", "cyno"] {
        let err = eve_server::services::pve_open_feats_b36::check_ess_intrusion_bubble_restriction(
            &ctx, sys_id, ship_a, action,
        )
        .await
        .expect_err("ESS 75km bubble must block action");
        assert!(err.code.starts_with("EssIntrusionBubble"));
    }

    // 2. ESS Main Bank 2h45m autopayout (persisted in system_states, deferred while hacked, proportional 2:1 when unlinked)
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().expect("base char");
        let mut ca = base.clone();
        ca.character_id = pilot_a;
        ca.station_id = None;
        ca.solar_system_id = sys_id;
        ca.active_ship_id = ship_a;
        ca.balance = IskCents(0);
        let mut cb = base;
        cb.character_id = pilot_b;
        cb.balance = IskCents(0);
        snap.characters.insert(pilot_a, ca);
        snap.characters.insert(pilot_b, cb);
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.ess_main_bank = IskCents(300_000);
        st.ess_reserve_bank = IskCents(1_500_000);
        st.ess_linked_character_id = Some(pilot_a);
        eve_server::services::pve_open_feats_b36::record_ess_main_bank_contribution(st, pilot_a, 200_000, now);
        eve_server::services::pve_open_feats_b36::record_ess_main_bank_contribution(st, pilot_b, 100_000, now);
    });

    ctx.db.clock().advance_secs(9_900); // 2h45m
    // While hacked, autopayout must NOT fire
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_ess_main_bank_autopayout(&ctx, ctx.now_unix()), 0);

    // Unlink hacker and trigger autopayout -> 200,000 to pilot_a, 100,000 to pilot_b
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sys_id) {
            st.ess_linked_character_id = None;
        }
    });
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_ess_main_bank_autopayout(&ctx, ctx.now_unix()), 1);
    let (bal_a, bal_b, main_rem, journal_a, journal_b) = ctx.db.with_snapshot(|s| {
        (
            s.characters.get(&pilot_a).map(|c| c.balance.0).unwrap_or(0),
            s.characters.get(&pilot_b).map(|c| c.balance.0).unwrap_or(0),
            s.system_states.get(&sys_id).map(|st| st.ess_main_bank.0).unwrap_or(-1),
            s.wallet_journal.iter().any(|j| j.character_id == pilot_a && j.entry_type == "ESSMainBankPayout" && j.amount.0 == 200_000),
            s.wallet_journal.iter().any(|j| j.character_id == pilot_b && j.entry_type == "ESSMainBankPayout" && j.amount.0 == 100_000),
        )
    });
    assert_eq!(bal_a, 200_000);
    assert_eq!(bal_b, 100_000);
    assert_eq!(main_rem, 0);
    assert!(journal_a, "pilot_a must receive ESSMainBankPayout journal entry");
    assert!(journal_b, "pilot_b must receive ESSMainBankPayout journal entry");

    // 3. ESS Reserve Bank key tiers (15-min vs 45-min quadrant key)
    let mut sess_a = SessionState::new(3603);
    sess_a.user_id = Some(1);
    sess_a.character_id = Some(pilot_a);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.station_id = None;
    sess_a.ship_id = Some(ship_a);

    // Test all 4 quadrant 15m and 45m reserve keys: 3 quadrants fail, 1 quadrant matches sys_id
    let keys_15m: [u32; 4] = [57714, 57716, 57717, 57720];
    let keys_45m: [u32; 4] = [57715, 57719, 57718, 57721];

    let mut matched_15m = false;
    let mut wrong_quad_rejected = false;
    let mut last_err = None;
    for (idx, &tid) in keys_15m.iter().enumerate() {
        let item_id = 960_010 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id, type_id: tid, owner_id: pilot_a,
                    location_id: ship_a, flag: InventoryFlag::Cargo,
                    quantity: 1, singleton: false, ..Default::default()
                },
            );
        });
        match ctx
            .dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(item_id)], &kw, None)
            .await
        {
            Ok(res_15) => {
                let kv_15 = kv_map(&res_15);
                assert_eq!(kv_15.get("unlocked").and_then(|v| v.as_bool()), Some(true));
                assert_eq!(kv_15.get("durationMinutes").and_then(|v| v.as_i64()), Some(15));
                let (consumed, pulses_tot, pulses_rem) = ctx.db.with_snapshot(|s| {
                    let p = eve_server::services::space_destiny::get_ess_reserve_pulses(s, sys_id);
                    (!s.items.contains_key(&item_id), p.pulses_total, p.pulses_remaining)
                });
                assert!(consumed, "15m key must be consumed");
                assert_eq!(pulses_tot, 15);
                assert_eq!(pulses_rem, 15);
                let notifs = sess_a.drain_notifications();
                assert!(notifs.iter().any(|(name, _)| name == "OnReserveBankUnlock"), "Must emit OnReserveBankUnlock");
                matched_15m = true;
            }
            Err(e) => {
                wrong_quad_rejected = true;
                last_err = Some(e);
            }
        }
    }
    assert!(wrong_quad_rejected, "Wrong-quadrant ESS reserve key must be rejected");
    assert!(matched_15m, "Matching-quadrant 15m ESS reserve key must unlock 15 pulses: {last_err:?}");

    let mut matched_45m = false;
    for (idx, &tid) in keys_45m.iter().enumerate() {
        let item_id = 960_020 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id, type_id: tid, owner_id: pilot_a,
                    location_id: ship_a, flag: InventoryFlag::Cargo,
                    quantity: 1, singleton: false, ..Default::default()
                },
            );
        });
        if let Ok(res_45) = ctx
            .dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(item_id)], &kw, None)
            .await
        {
            let kv_45 = kv_map(&res_45);
            assert_eq!(kv_45.get("unlocked").and_then(|v| v.as_bool()), Some(true));
            assert_eq!(kv_45.get("durationMinutes").and_then(|v| v.as_i64()), Some(45));
            let (consumed, pulses_tot, pulses_rem) = ctx.db.with_snapshot(|s| {
                let p = eve_server::services::space_destiny::get_ess_reserve_pulses(s, sys_id);
                (!s.items.contains_key(&item_id), p.pulses_total, p.pulses_remaining)
            });
            assert!(consumed, "45m key must be consumed");
            assert_eq!(pulses_tot, 45);
            assert_eq!(pulses_rem, 45);
            matched_45m = true;
        }
    }
    assert!(matched_45m, "Matching-quadrant 45m ESS reserve key must unlock 45 pulses");
}

#[tokio::test]
async fn test_b36_insurgency_heist_and_fob() {
    // parity: feat:pve.insurgency-ice-heist, feat:pve.pirate-fob-forward-operating-base
    eve_testkit::attest(&[
        "feat:pve.insurgency-ice-heist",
        "feat:pve.pirate-fob-forward-operating-base",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(3604);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(30_002_537);

    let cid = 900_001_i64;
    eve_server::services::insurgency_services::init_insurgency_campaign(
        &ctx, cid, 500_011, 500_002, 30_002_537, vec![30_002_537],
    );
    let _ = eve_server::services::insurgency_services::add_system_corruption(&ctx, cid, 30_002_537, 600);
    let heist_id = eve_server::services::insurgency_services::spawn_ice_heist(&ctx, 30_002_537);
    assert!(heist_id > 0);

    let heists_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dungeonInstanceCacheMgr",
            None,
            "GetPirateInsurgencyIceHeistInstances",
            &[PyRep::Integer(30_002_537)],
            &kw,
            None,
        )
        .await
        .expect("GetPirateInsurgencyIceHeistInstances");
    let PyRep::List(heists) = heists_rep else {
        panic!("Expected List of ice heists");
    };
    assert_eq!(heists.len(), 1);
    let h_kv = kv_map(&heists[0]);
    assert_eq!(h_kv.get("instanceID").and_then(|v| v.as_i64()), Some(heist_id));
    assert_eq!(h_kv.get("solarSystemID").and_then(|v| v.as_i64()), Some(30_002_537));
    assert_eq!(h_kv.get("completed").and_then(|v| v.as_bool()), Some(false));

    let fob_id = eve_server::services::fw_gameplay::fw_gameplay_insurgency::ensure_campaign_pirate_fob(&ctx, cid)
        .await
        .expect("Pirate FOB spawned");
    assert!(fob_id > 0);
    let fob_in_db = ctx.db.with_snapshot(|s| {
        s.structures
            .get(&fob_id)
            .is_some_and(|st| st.solar_system_id == 30_002_537 && st.state == "ShieldVulnerable")
    });
    assert!(fob_in_db, "Pirate FOB structure must be persisted in FOB system");
    let sys_handle = ctx.systems.get_or_spawn(30_002_537);
    assert!(sys_handle.get_ball_snapshot(fob_id).await.unwrap().is_some(), "Pirate FOB ball must exist in space");
}

#[tokio::test]
async fn test_b36_sleeper_sentries_and_wh_downtime_despawn() {
    // parity: feat:pve.sleeper-sentry-towers, feat:pve.wormhole-site-downtime-despawn
    eve_testkit::attest(&[
        "feat:pve.sleeper-sentry-towers",
        "feat:pve.wormhole-site-downtime-despawn",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let wh_sys = 31_000_005_u32;

    // 1. Sleeper Sentry Towers (30460 Vigilant C1, 30461 Wakeful C3, 30462 Restless C5)
    //    with 50 m signature radius, 250 km optimal range, and EM/Thermal damage
    for (wh_class, expected_tid) in [
        (1_u8, SLEEPER_SENTRY_VIGILANT),
        (3_u8, SLEEPER_SENTRY_WAKEFUL),
        (5_u8, SLEEPER_SENTRY_RESTLESS),
    ] {
        let spawned = eve_server::services::pve_exploration_sites::spawn_sleeper_wave_in_system(
            &ctx, wh_sys, wh_class, 1, DVec3::ZERO,
        )
        .await;
        let sentry_bid = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            spawned.iter().copied().find(|bid| {
                rt.npc_entities.get(bid).is_some_and(|e| {
                    e.type_id == expected_tid
                        && (e.optimal_range - 250_000.0).abs() < 1.0
                        && e.damage_vector.em > 0.0
                        && e.damage_vector.thermal > 0.0
                        && e.damage_vector.kinetic == 0.0
                        && e.damage_vector.explosive == 0.0
                })
            })
        })
        .expect("Sleeper Sentry Tower with 250km optimal range and EM/Thermal damage");
        let sys_handle = ctx.systems.get_or_spawn(wh_sys);
        let cs = sys_handle
            .get_combat_state(sentry_bid)
            .await
            .unwrap()
            .expect("Sentry combat state");
        let expected_sig = ctx
            .sde
            .types
            .get(&expected_tid)
            .and_then(|t| t.default_attributes.get(&552).copied())
            .unwrap_or(50.0);
        assert!((cs.signature_radius - expected_sig).abs() < 1e-3, "Expected {expected_sig}m signature radius");
    }

    // 2. W-space site downtime despawn: 3rd downtime if completed, 4th if unfinished
    let base_site = eve_server::services::pve_exploration::PveSiteInstance {
        site_id: "970001".into(),
        dungeon_id: 1,
        dungeon_name: "WH Site".into(),
        solar_system_id: wh_sys,
        site_category: "WormholeCombat".into(),
        ded_rating: None,
        wormhole_class: Some(3),
        position: [0.0, 0.0, 0.0],
        probe_required: false,
        signal_strength: 100.0,
        current_room: 1,
        total_rooms: 1,
        current_wave: 1,
        total_waves: 1,
        active_npc_ball_ids: Vec::new(), hackable_can_ids: Vec::new(), npc_guards_spawned: true,
        ghost_timer_started_at: None, ghost_detonate_after_sec: 0, ghost_detonated: false,
        sleeper_rift_stabilized: false, sleeper_sentry_disabled: false, sleeper_gas_cloud_active: false,
        cleared: true, activated: true, downtimes_since_activation: 0,
        created_unix: 0, lifetime_seconds: 86_400, expedition_instance_id: None, gate_ball_id: None, activated_at_downtime: None, dread_escalation_triggers: 0, carrier_escalation_triggers: 0, active_escalation_capitals: Vec::new(), is_shattered_resource_site: false, delayed_wave_triggered: false, relay_spawned: false,
    };
    let mut unfinished_site = base_site.clone();
    unfinished_site.site_id = "970002".into();
    unfinished_site.cleared = false;
    eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(wh_sys, vec![base_site, unfinished_site]);
    });

    // Downtime 1 & 2: both survive
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_wormhole_sites_downtime(&ctx), 0);
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_wormhole_sites_downtime(&ctx), 0);
    // Downtime 3: completed site despawns, unfinished survives
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_wormhole_sites_downtime(&ctx), 1);
    // Downtime 4: unfinished site despawns
    assert_eq!(eve_server::services::pve_open_feats_b36::tick_wormhole_sites_downtime(&ctx), 1);
}
