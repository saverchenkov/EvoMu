//! Integration tests for GH #1088 GM soak, deadwarp, junk purge, wire session dump,
//! starter skill reset (`/backintime`), and SDE wreck spawning (`/spawnwreck`) commands.

#![forbid(unsafe_code)]

use crate::common::{
    gm_pilot_in_space, pin_clock, slash_cmd, teardown_guard, wrap_wire_harness,
    SessionBorrowWireTransport,
};
use eve_db::{InventoryFlag, InventoryItemRecord, MobileDeployableRecord, SkillQueueEntry};
use eve_destiny::{Ball, DVec3, DestinyEvent, MovementMode, SlimItemMeta};
use eve_machonet::service_roles::ROLE_LOGIN;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::gm_soak_commands::combat_ephemeral_test_helpers::get_gm_deadwarp;
use eve_server::services::mobile_deployables::{write_deployable_state_snap, GROUP_MOBILE_DEPOT};
use eve_server::services::space_destiny::record_session_change_timer;
use eve_server::sp6_lifecycle::{
    record_wreck_or_container_meta_in_snap, step_active_systems_physics_tick,
};
use eve_testkit::client_harness::ClientHarness;
use std::collections::HashMap;

const JITA_SYS: u32 = 30_000_142;
const AU_IN_METERS: f64 = 149_597_870_700.0;

#[tokio::test]
async fn gm_soak_logi_ring_heals_cycles_specialfx_and_dbuffs() {
    // parity: feat:admin.slash-dispatcher, feat:admin.gm-qa-tools, feat:admin.gm-spawn, rpc:slash.SlashCmd
    eve_testkit::attest(&[
        "feat:admin.slash-dispatcher",
        "feat:admin.gm-qa-tools",
        "feat:admin.gm-spawn",
        "rpc:slash.SlashCmd",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let mut pilot = gm_pilot_in_space(&ctx, 51_001, 90_108_801, 587, JITA_SYS).await;
    let sys = ctx.systems.get_or_spawn(JITA_SYS);

    let ship_id = pilot.ship_id;
    let (damaged_shield, damaged_armor, ship_pos) = sys
        .test_with_ballpark_mut_async(move |bp| {
            let ship_pos = bp.balls.get(&ship_id).expect("ship ball").position;
            let cs = bp
                .combat_states
                .get_mut(&ship_id)
                .expect("ship combat state");
            cs.shield_hp = (cs.max_shield_hp * 0.25).max(1.0);
            cs.armor_hp = (cs.max_armor_hp * 0.25).max(1.0);
            (cs.shield_hp, cs.armor_hp, ship_pos)
        })
        .await;
    let _ = ctx.drain_notifications_for_char(pilot.char_id);

    let spawned = slash_cmd(&ctx, &mut pilot.sess, "/soak logi 3").await;
    assert_eq!(spawned.dict_get("active").and_then(PyRep::as_bool), Some(true));
    assert_eq!(spawned.dict_get("npcCount").and_then(PyRep::as_i64), Some(3));
    let npc_ids: Vec<i64> = match spawned.dict_get("npcIDs") {
        Some(PyRep::List(items)) => items.iter().filter_map(PyRep::as_i64).collect(),
        other => panic!("expected npcIDs list, got {other:?}"),
    };
    assert_eq!(npc_ids.len(), 3);

    {
        let npc_ids_check = npc_ids.clone();
        let horiz_dists: Vec<f64> = sys
            .test_with_ballpark_async(move |bp| {
                npc_ids_check
                    .into_iter()
                    .map(|nid| {
                        let b = bp.balls.get(&nid).expect("logi npc ball present");
                        DVec3::new(b.position.x - ship_pos.x, 0.0, b.position.z - ship_pos.z).length()
                    })
                    .collect()
            })
            .await;
        for horiz in horiz_dists {
            assert!(
                (horiz - 12_000.0).abs() < 2.0,
                "expected logi cruiser on 12 km ring, got horizontal distance {horiz}"
            );
        }
    }

    let mut saw_destiny_dbuffs = false;
    for _ in 0..4 {
        ctx.advance_secs(1);
        step_active_systems_physics_tick(&ctx, 1).await;
        let saw_tick_dbuffs = sys
            .test_with_ballpark_async(move |bp| {
                bp.pending_events.iter().any(|ev| {
                    matches!(
                        ev,
                        DestinyEvent::OnDbuffUpdated { ball_id, dbuffs }
                            if *ball_id == ship_id
                                && dbuffs
                                    .iter()
                                    .any(|&(id, val, _)| id == 10 && (val - 15.0).abs() < 1e-9)
                                && dbuffs
                                    .iter()
                                    .any(|&(id, val, _)| id == 13 && (val - 15.0).abs() < 1e-9)
                    )
                })
            })
            .await;
        if saw_tick_dbuffs {
            saw_destiny_dbuffs = true;
        }
    }
    assert!(
        saw_destiny_dbuffs,
        "expected Destiny OnDbuffUpdated with command burst dbuffs 10 and 13"
    );

    let (healed_shield, healed_armor) = sys
        .test_with_ballpark_async(move |bp| {
            let cs = bp
                .combat_states
                .get(&ship_id)
                .expect("ship combat state after soak");
            (cs.shield_hp, cs.armor_hp)
        })
        .await;
    assert!(
        healed_shield > damaged_shield,
        "expected remote shield transfer heal: before={damaged_shield}, after={healed_shield}"
    );
    assert!(
        healed_armor > damaged_armor,
        "expected remote armor repair heal: before={damaged_armor}, after={healed_armor}"
    );

    {
        let npc_ids_fx = npc_ids.clone();
        let fx_checks = sys
            .test_with_ballpark_async(move |bp| {
                let mut out = Vec::new();
                for &nid in &npc_ids_fx {
                    for (guid, expected_tid) in [
                        ("effects.ShieldTransfer", 3_596_u32),
                        ("effects.RemoteArmourRepair", 11_357_u32),
                    ] {
                        let Some(ev) = bp.pending_events.iter().find(|ev| {
                            matches!(
                                ev,
                                DestinyEvent::OnSpecialFX {
                                    ball_id,
                                    module_id: Some(mod_id),
                                    guid: ev_guid,
                                    is_active: true,
                                } if *ball_id == nid
                                    && *mod_id != ship_id
                                    && *mod_id != nid
                                    && ev_guid == guid
                            )
                        }) else {
                            return Err(format!(
                                "expected active Destiny OnSpecialFX {guid} from npc {nid}: {:?}",
                                bp.pending_events
                            ));
                        };
                        let DestinyEvent::OnSpecialFX {
                            module_id: Some(mod_id),
                            ..
                        } = ev
                        else {
                            unreachable!();
                        };
                        let Some(meta) = bp
                            .special_fx_meta
                            .get(&(nid, Some(*mod_id), guid.to_string()))
                            .cloned()
                        else {
                            return Err(format!("expected special_fx_meta for ({nid}, {mod_id}, {guid})"));
                        };
                        let encoded =
                            eve_destiny::wire::encode_destiny_event(bp.current_tick as u32, ev, Some(bp));
                        out.push((nid, guid, expected_tid, *mod_id, meta, encoded));
                    }
                }
                Ok(out)
            })
            .await
            .unwrap_or_else(|msg| panic!("{msg}"));

        for (nid, guid, expected_tid, mod_id, meta, encoded) in fx_checks {
            assert_eq!(meta.target_id, Some(ship_id));
            assert_eq!(meta.module_type_id, Some(expected_tid));
            assert!(!meta.is_offensive);
            assert_eq!(meta.duration_ms, Some(2_000.0));
            assert_eq!(meta.repeat, Some(0));

            let PyRep::Tuple(stamp_and_action) = &encoded else {
                panic!("expected (stamp, action) OnSpecialFX tuple: {encoded:?}");
            };
            assert_eq!(stamp_and_action.len(), 2);
            let PyRep::Tuple(outer) = &stamp_and_action[1] else {
                panic!("expected outer OnSpecialFX action tuple: {:?}", stamp_and_action[1]);
            };
            assert_eq!(outer[0].as_str(), Some("OnSpecialFX"));
            let PyRep::Tuple(args) = &outer[1] else {
                panic!("expected 14-arg OnSpecialFX tuple: {:?}", outer[1]);
            };
            assert_eq!(args.len(), 14);
            assert_eq!(args[0].as_i64(), Some(nid));
            assert_eq!(args[1].as_i64(), Some(mod_id));
            assert_ne!(args[1].as_i64(), Some(ship_id));
            assert_eq!(args[2].as_i64(), Some(i64::from(expected_tid)));
            assert_eq!(args[3].as_i64(), Some(ship_id));
            assert_eq!(args[5].as_str(), Some(guid));
            assert_eq!(args[6].as_bool(), Some(false));
            assert_eq!(args[7].as_i64(), Some(1));
            assert_eq!(args[8].as_i64(), Some(1));
            assert_eq!(args[9].as_f64(), Some(2_000.0));
            assert_eq!(args[10].as_i64(), Some(0));
        }
    }

    let notifs = ctx.drain_notifications_for_char(pilot.char_id);
    assert!(
        !notifs.iter().any(|n| n.event == "OnSpecialFX"),
        "expected no duplicate MachoNet OnSpecialFX notification when Destiny handles it: {notifs:?}"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnDbuffUpdated" && n.args == vec![PyRep::Integer(pilot.ship_id)]
        }),
        "expected typed OnDbuffUpdated notification for ship {}: {notifs:?}",
        pilot.ship_id
    );

    let status = slash_cmd(&ctx, &mut pilot.sess, "/soak logi status").await;
    assert_eq!(status.dict_get("active").and_then(PyRep::as_bool), Some(true));
    assert!(
        status
            .dict_get("cycleCount")
            .and_then(PyRep::as_i64)
            .unwrap_or(0)
            >= 1
    );
    assert!(
        status
            .dict_get("totalShieldRepaired")
            .and_then(PyRep::as_f64)
            .unwrap_or(0.0)
            > 0.0
    );
    assert!(
        status
            .dict_get("totalArmorRepaired")
            .and_then(PyRep::as_f64)
            .unwrap_or(0.0)
            > 0.0
    );

    let stopped = slash_cmd(&ctx, &mut pilot.sess, "/soak logi stop").await;
    assert_eq!(stopped.dict_get("active").and_then(PyRep::as_bool), Some(false));
    assert_eq!(
        stopped.dict_get("stoppedCount").and_then(PyRep::as_i64),
        Some(3)
    );
    {
        let npc_ids_stop = npc_ids.clone();
        let (npc_present, pending_events, pruned_fx, fx_meta_dbg) = sys
            .test_with_ballpark_async(move |bp| {
                let npc_present: Vec<(i64, bool)> = npc_ids_stop
                    .iter()
                    .map(|&nid| (nid, bp.balls.contains_key(&nid)))
                    .collect();
                let pruned_fx = !bp
                    .special_fx_meta
                    .keys()
                    .any(|(bid, _, _)| npc_ids_stop.contains(bid));
                let fx_meta_dbg = if pruned_fx {
                    String::new()
                } else {
                    format!("{:?}", bp.special_fx_meta)
                };
                (npc_present, bp.pending_events.clone(), pruned_fx, fx_meta_dbg)
            })
            .await;
        for (nid, still_present) in npc_present {
            assert!(!still_present, "logi npc {nid} should despawn on stop");
            for guid in ["effects.ShieldTransfer", "effects.RemoteArmourRepair"] {
                assert!(
                    pending_events.iter().any(|ev| matches!(
                        ev,
                        DestinyEvent::OnSpecialFX {
                            ball_id,
                            module_id: Some(mod_id),
                            guid: ev_guid,
                            is_active: false,
                        } if *ball_id == nid && *mod_id != ship_id && ev_guid == guid
                    )),
                    "expected stop Destiny OnSpecialFX {guid} for npc {nid}: {:?}",
                    pending_events
                );
            }
        }
        assert!(pruned_fx, "expected special_fx_meta pruned on stop: {fx_meta_dbg}");
        assert!(
            pending_events.iter().any(|ev| matches!(
                ev,
                DestinyEvent::OnDbuffUpdated { ball_id, dbuffs }
                    if *ball_id == ship_id && dbuffs.is_empty()
            )),
            "expected Destiny OnDbuffUpdated clear on /soak logi stop: {:?}",
            pending_events
        );
    }
    let stop_notifs = ctx.drain_notifications_for_char(pilot.char_id);
    assert!(
        !stop_notifs.iter().any(|n| n.event == "OnSpecialFX"),
        "expected no duplicate MachoNet OnSpecialFX notification on stop: {stop_notifs:?}"
    );
    assert!(
        stop_notifs.iter().any(|n| {
            n.event == "OnDbuffUpdated" && n.args == vec![PyRep::Integer(pilot.ship_id)]
        }),
        "expected typed OnDbuffUpdated notification on /soak logi stop: {stop_notifs:?}"
    );
    let after_stop = slash_cmd(&ctx, &mut pilot.sess, "/soak status").await;
    assert_eq!(
        after_stop.dict_get("active").and_then(PyRep::as_bool),
        Some(false)
    );
}

#[tokio::test]
async fn gm_deadwarp_round_trip_and_stop() {
    // parity: feat:admin.slash-dispatcher, feat:admin.gm-qa-tools, rpc:slash.SlashCmd
    eve_testkit::attest(&[
        "feat:admin.slash-dispatcher",
        "feat:admin.gm-qa-tools",
        "rpc:slash.SlashCmd",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let mut pilot = gm_pilot_in_space(&ctx, 51_002, 90_108_802, 587, JITA_SYS).await;
    let sys = ctx.systems.get_or_spawn(JITA_SYS);

    let origin = sys
        .get_ball_snapshot(pilot.ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball")
        .1
        .position;

    let started = slash_cmd(&ctx, &mut pilot.sess, "/deadwarp").await;
    assert_eq!(started.dict_get("active").and_then(PyRep::as_bool), Some(true));
    assert_eq!(
        started.dict_get("phase").and_then(PyRep::as_str),
        Some("Outbound")
    );
    let anchor_vec: Vec<f64> = match started.dict_get("anchorPos") {
        Some(PyRep::List(coords)) => coords.iter().filter_map(PyRep::as_f64).collect(),
        other => panic!("expected anchorPos list, got {other:?}"),
    };
    assert_eq!(anchor_vec.len(), 3);
    let anchor = DVec3::new(anchor_vec[0], anchor_vec[1], anchor_vec[2]);
    let anchor_dist_au = anchor.distance(origin) / AU_IN_METERS;
    assert!(
        (anchor_dist_au - 2.0).abs() < 1e-4,
        "expected ~2 AU deadwarp anchor, got {anchor_dist_au} AU"
    );

    for _ in 0..3 {
        ctx.advance_secs(1);
        step_active_systems_physics_tick(&ctx, 1).await;
    }
    let mid_status = slash_cmd(&ctx, &mut pilot.sess, "/deadwarp status").await;
    assert_eq!(
        mid_status.dict_get("active").and_then(PyRep::as_bool),
        Some(true)
    );

    let mut saw_returning = false;
    let mut completed = false;
    for _ in 0..240 {
        ctx.advance_secs(1);
        step_active_systems_physics_tick(&ctx, 1).await;
        let state_opt = get_gm_deadwarp(&ctx, pilot.ship_id);
        match state_opt {
            Some(st) => {
                if st.phase == eve_server::services::gm_soak_commands::DeadwarpPhase::Returning {
                    saw_returning = true;
                }
            }
            None => {
                completed = true;
                break;
            }
        }
    }

    assert!(completed, "/deadwarp must complete round trip within 240 virtual ticks");
    assert!(saw_returning, "/deadwarp must enter Returning phase after anchor");

    let final_pos = sys
        .get_ball_snapshot(pilot.ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball")
        .1
        .position;
    assert!(
        final_pos.distance(origin) < 5_000.0,
        "expected return warp near origin, distance was {} m",
        final_pos.distance(origin)
    );

    let done_status = slash_cmd(&ctx, &mut pilot.sess, "/deadwarp status").await;
    assert_eq!(
        done_status.dict_get("active").and_then(PyRep::as_bool),
        Some(false)
    );
    assert_eq!(
        done_status.dict_get("phase").and_then(PyRep::as_str),
        Some("Idle")
    );

    // Start another deadwarp and cancel mid-flight with `/deadwarp stop`.
    let second = slash_cmd(&ctx, &mut pilot.sess, "/deadwarp").await;
    assert_eq!(second.dict_get("active").and_then(PyRep::as_bool), Some(true));
    let cancelled = slash_cmd(&ctx, &mut pilot.sess, "/deadwarp stop").await;
    assert_eq!(
        cancelled.dict_get("active").and_then(PyRep::as_bool),
        Some(false)
    );
    assert_eq!(
        cancelled.dict_get("stopped").and_then(PyRep::as_bool),
        Some(true)
    );
    let stopped_ball = sys
        .get_ball_snapshot(pilot.ship_id)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball")
        .1;
    assert_eq!(stopped_ball.mode, MovementMode::Stop);
}

#[tokio::test]
async fn gm_sysjunkclear_backintime_spawnwreck_session_and_role_gating() {
    // parity: feat:admin.slash-dispatcher, feat:admin.gm-qa-tools, feat:admin.gm-spawn, feat:admin.gm-giveskill, feat:admin.gm-role-gating, rpc:slash.SlashCmd
    eve_testkit::attest(&[
        "feat:admin.slash-dispatcher",
        "feat:admin.gm-qa-tools",
        "feat:admin.gm-spawn",
        "feat:admin.gm-giveskill",
        "feat:admin.gm-role-gating",
        "rpc:slash.SlashCmd",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let mut pilot = gm_pilot_in_space(&ctx, 51_003, 90_108_803, 587, JITA_SYS).await;
    let sys = ctx.systems.get_or_spawn(JITA_SYS);

    // Subtest A: `/spawnwreck` + `/sysjunkclear`
    let wrecked = slash_cmd(&ctx, &mut pilot.sess, "/spawnwreck 587 2").await;
    assert_eq!(wrecked.dict_get("count").and_then(PyRep::as_i64), Some(2));
    assert!(
        wrecked
            .dict_get("lootItemCount")
            .and_then(PyRep::as_i64)
            .unwrap_or(0)
            > 0,
        "expected deterministic SDE-derived loot inside spawned wrecks: {wrecked:?}"
    );
    let wreck_ids: Vec<i64> = match wrecked.dict_get("wreckIDs") {
        Some(PyRep::List(ids)) => ids.iter().filter_map(PyRep::as_i64).collect(),
        other => panic!("expected wreckIDs list, got {other:?}"),
    };
    assert_eq!(wreck_ids.len(), 2);
    ctx.db.with_snapshot(|snap| {
        for &wid in &wreck_ids {
            assert!(snap.items.contains_key(&wid), "wreck item {wid} missing");
            let has_loot = snap.items.values().any(|it| it.location_id == wid);
            assert!(has_loot, "expected loot items inside wreck {wid}");
        }
    });

    let now = ctx.now_unix();
    let (jetcan_id, expired_dep_id, live_dep_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let jid = snap.next_entity_id;
        snap.next_entity_id += 1;
        let edid = snap.next_entity_id;
        snap.next_entity_id += 1;
        let ldid = snap.next_entity_id;

        snap.items.insert(
            jid,
            InventoryItemRecord {
                item_id: jid,
                type_id: 23,
                owner_id: pilot.char_id,
                location_id: i64::from(JITA_SYS),
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(jid, [12_000.0, 0.0, 0.0]);
        record_wreck_or_container_meta_in_snap(snap, jid, now, false);

        for (did, pos, expires_at) in [
            (edid, [14_000.0, 0.0, 0.0], now.saturating_sub(10)),
            (ldid, [18_000.0, 0.0, 0.0], now.saturating_add(3_600)),
        ] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: 33_474,
                    owner_id: pilot.char_id,
                    location_id: i64::from(JITA_SYS),
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(did, pos);
            write_deployable_state_snap(snap, |st| {
                st.deployables.insert(
                    did,
                    MobileDeployableRecord {
                        item_id: did,
                        type_id: 33_474,
                        group_id: GROUP_MOBILE_DEPOT,
                        owner_id: pilot.char_id,
                        solar_system_id: JITA_SYS,
                        anchor_start_unix: now.saturating_sub(1_000),
                        anchor_duration_secs: 60,
                        anchor_completed: true,
                        is_unanchored: false,
                        expires_at_unix: expires_at,
                        last_interaction_unix: now,
                        reinforced_until_unix: None,
                        last_ping_unix: None,
                        position: pos,
                        deployed_fleet_id: None,
                    },
                );
            });
        }
        (jid, edid, ldid)
    });

    for (id, tid, gid, pos) in [
        (jetcan_id, 23_u32, 12_u32, DVec3::new(12_000.0, 0.0, 0.0)),
        (expired_dep_id, 33_474, GROUP_MOBILE_DEPOT, DVec3::new(14_000.0, 0.0, 0.0)),
        (live_dep_id, 33_474, GROUP_MOBILE_DEPOT, DVec3::new(18_000.0, 0.0, 0.0)),
    ] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 10_000.0,
                    radius: 250.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(tid),
                SlimItemMeta {
                    group_id: gid,
                    category_id: 22,
                    owner_id: pilot.char_id,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    ..Default::default()
                },
            )
            .await;
    }

    let cleared = slash_cmd(&ctx, &mut pilot.sess, "/sysjunkclear").await;
    assert_eq!(cleared.dict_get("wrecksPurged").and_then(PyRep::as_i64), Some(2));
    assert_eq!(cleared.dict_get("jetcansPurged").and_then(PyRep::as_i64), Some(1));
    assert_eq!(
        cleared
            .dict_get("expiredDeployablesPurged")
            .and_then(PyRep::as_i64),
        Some(1)
    );
    assert_eq!(cleared.dict_get("totalPurged").and_then(PyRep::as_i64), Some(4));

    ctx.db.with_snapshot(|snap| {
        for &wid in &wreck_ids {
            assert!(!snap.items.contains_key(&wid));
        }
        assert!(!snap.items.contains_key(&jetcan_id));
        assert!(!snap.items.contains_key(&expired_dep_id));
        assert!(
            snap.items.contains_key(&live_dep_id),
            "non-expired deployable must survive /sysjunkclear"
        );
    });
    let (has_jetcan, has_expired_dep, has_live_dep) = sys
        .test_with_ballpark_async(move |bp| {
            (
                bp.balls.contains_key(&jetcan_id),
                bp.balls.contains_key(&expired_dep_id),
                bp.balls.contains_key(&live_dep_id),
            )
        })
        .await;
    assert!(!has_jetcan);
    assert!(!has_expired_dep);
    assert!(has_live_dep);

    // Subtest B: `/backintime`
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot.char_id) {
            ch.race_id = 1;
            ch.free_skill_points = 12_345.0;
        }
        snap.skill_queues.insert(
            pilot.char_id,
            vec![SkillQueueEntry {
                type_id: 20_533,
                target_level: 2,
                start_sp: 0.0,
                end_sp: 35_000.0,
                duration_seconds: 1_200.0,
                started_at_unix: Some(now),
            }],
        );
    });
    let _ = slash_cmd(&ctx, &mut pilot.sess, "/giveskill 20533 5").await;
    let _ = ctx.drain_notifications_for_char(pilot.char_id);

    let reset = slash_cmd(&ctx, &mut pilot.sess, "/backintime").await;
    assert_eq!(reset.dict_get("raceID").and_then(PyRep::as_i64), Some(1));
    assert_eq!(reset.dict_get("queueLength").and_then(PyRep::as_i64), Some(0));
    assert_eq!(reset.dict_get("freeSkillPoints").and_then(PyRep::as_f64), Some(0.0));
    assert!(
        reset
            .dict_get("removedSkillCount")
            .and_then(PyRep::as_i64)
            .unwrap_or(0)
            >= 1
    );

    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&pilot.char_id).expect("char");
        assert_eq!(ch.free_skill_points, 0.0);
        let skills = snap.skills.get(&pilot.char_id).expect("skills");
        assert!(
            !skills.contains_key(&20_533),
            "Capital Ships extra skill must be removed by /backintime"
        );
        let race_starter = &ctx.sde.races.get(&1).expect("Caldari race").starter_skills;
        for (&tid, &lvl) in race_starter {
            if lvl > 0 {
                assert_eq!(
                    skills.get(&tid).map(|s| s.level),
                    Some(lvl),
                    "starter skill {tid} mismatch"
                );
            }
        }
        assert!(
            snap.skill_queues
                .get(&pilot.char_id)
                .is_none_or(|q| q.is_empty()),
            "skill queue must be empty after /backintime"
        );
    });

    let back_notifs = ctx.drain_notifications_for_char(pilot.char_id);
    assert!(back_notifs.iter().any(|n| n.event == "OnServerSkillsChanged"));
    assert!(back_notifs.iter().any(|n| n.event == "OnNewSkillQueueSaved"));
    assert!(back_notifs.iter().any(|n| n.event == "OnFreeSkillPointsChanged"));

    // Subtest C: `/session` on persistent `ClientWireSession`
    record_session_change_timer(&ctx, pilot.char_id, ctx.now_unix(), JITA_SYS, 1, "test_dump");
    let mut wire_harness = wrap_wire_harness(&ctx, pilot.sess.clone());
    let kw = HashMap::new();
    wire_harness
        .call_ok("beyonce", Some(pilot.beyonce), "UpdateStateRequest", &[], &kw)
        .await;
    let dump = wire_harness
        .call_ok(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/session".into())],
            &kw,
        )
        .await;
    let sess_dict = dump.dict_get("sessionDict").expect("sessionDict present");
    assert_eq!(
        sess_dict.dict_get("charid").and_then(PyRep::as_i64),
        Some(pilot.char_id)
    );
    assert_eq!(
        sess_dict.dict_get("solarsystemid").and_then(PyRep::as_i64),
        Some(i64::from(JITA_SYS))
    );
    let wire_state = dump.dict_get("wireState").expect("wireState present");
    assert_eq!(
        wire_state.dict_get("hasWireSession").and_then(PyRep::as_bool),
        Some(true)
    );
    assert_eq!(
        wire_state.dict_get("materialized").and_then(PyRep::as_bool),
        Some(true)
    );
    assert_eq!(
        wire_state.dict_get("destinyState").and_then(PyRep::as_str),
        Some("Live")
    );
    assert!(
        wire_state
            .dict_get("materializedSetSize")
            .and_then(PyRep::as_i64)
            .unwrap_or(0)
            > 0
    );
    assert_eq!(
        dump.dict_get("hasPendingSessionChange")
            .and_then(PyRep::as_bool),
        Some(true)
    );
    let pending = dump
        .dict_get("pendingSessionChange")
        .expect("pendingSessionChange present");
    assert_eq!(
        pending.dict_get("reason").and_then(PyRep::as_str),
        Some("test_dump")
    );
    let ledger_count = dump
        .dict_get("ledgerCount")
        .and_then(PyRep::as_i64)
        .unwrap_or(0);
    let stamp_count = dump
        .dict_get("destinyStampCount")
        .and_then(PyRep::as_i64)
        .unwrap_or(0);
    let rpc_count = dump
        .dict_get("rpcHistoryCount")
        .and_then(PyRep::as_i64)
        .unwrap_or(0);
    assert!((1..=50).contains(&ledger_count));
    assert!((1..=50).contains(&stamp_count));
    assert!((1..=50).contains(&rpc_count));

    // Advance past session change timer before Subtest D and teardown.
    ctx.advance_secs(35);

    // Subtest D: `ROLE_LOGIN` gating + `slash.SlashCmd("/")` command list
    let cmd_list_err = wire_harness
        .call(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/".into())],
            &kw,
        )
        .await
        .expect_err("slash '/' returns SlashError command list");
    for expected in [
        "'backintime'",
        "'deadwarp'",
        "'session'",
        "'soak'",
        "'spawnwreck'",
        "'sysjunkclear'",
    ] {
        assert!(
            cmd_list_err.message.contains(expected),
            "expected {expected} in SlashCmd('/') list: {}",
            cmd_list_err.message
        );
    }

    let mut non_gm_sess = pilot.sess.clone();
    non_gm_sess.role = ROLE_LOGIN;
    let mut non_gm = ClientHarness::new(SessionBorrowWireTransport {
        ctx: &ctx,
        session: &mut non_gm_sess,
        preserve_char_notifications: false,
    });
    for forbidden in [
        "/soak logi 1",
        "/deadwarp",
        "/sysjunkclear",
        "/session",
        "/backintime",
        "/spawnwreck 587",
    ] {
        let err = non_gm
            .call(
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(forbidden.into())],
                &kw,
            )
            .await
            .expect_err("ROLE_LOGIN must be denied GM soak/repro command");
        assert_eq!(err.code, "InsufficientRole");
    }
}
