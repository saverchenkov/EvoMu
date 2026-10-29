//! End-to-end V24.01 integration tests closing NPC spawns, belt rats, Clone Soldiers,
//! Mordu's Legion Special Warfare Units, Faction/Null-Sec Officers, and Drifters.
//!
//! SDE Data Gap Note (intentionally left open without parity tags):
//! `data/evomu_sde.sqlite` `sde_dungeons` stores only `(dungeon_id, name, archetype_id, faction_id, difficulty)`
//! without multi-room acceleration gate graphs or per-wave trigger scripts for World Ark Observatories,
//! Pirate Forward Operating Bases, or Insurgency Ice Heist / Mining Ambush / Outpost Raid / Laundering sites.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    combat_npc_encounters::flush_npc_bounty_tick, with_combat_runtime,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use std::collections::HashMap;

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

pub(crate) async fn bind_beyonce(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32) -> u64 {
    let PyRep::Tuple(tup) = rpc(
        ctx,
        sess,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(sys_id as i64), PyRep::None],
    )
    .await
    .expect("MachoBindObject beyonce") else {
        panic!("expected Tuple from MachoBindObject");
    };
    tup[0].as_i64().expect("bound_id") as u64
}

pub(crate) fn find_multi_hit_seed(ctx: &ServerContext, prob: f64, count: usize) -> u64 {
    (1u64..500_000)
        .find(|&s| {
            ctx.set_mechanic_rng_seed(Some(s));
            (0..count).all(|_| ctx.mechanic_rng.roll(prob))
        })
        .expect("deterministic seed hitting consecutive rolls")
}

fn grant_combat_skills(ctx: &ServerContext, char_id: i64, type_ids: &[u32]) {
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for &sid in &[
            3300u32, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3413, 3418, 3426, 20327, 21667,
            41404, 54794, 54826, 54829, 55033,
        ] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 1_280_000.0 });
        }
        for &tid in type_ids {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for sa in [
                    eve_sde::attr_ids::REQUIRED_SKILL_1,
                    eve_sde::attr_ids::REQUIRED_SKILL_2,
                    eve_sde::attr_ids::REQUIRED_SKILL_3,
                ] {
                    if let Some(&req_f) = t.default_attributes.get(&sa) {
                        let rsid = req_f as u32;
                        if rsid > 0 {
                            sk.insert(rsid, TrainedSkillRecord { type_id: rsid, level: 5, skill_points: 1_280_000.0 });
                        }
                    }
                }
            }
        }
    });
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, qty: i32) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: qty,
        singleton: qty == 1,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

pub(crate) fn fit_combat_ship(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_tid: u32,
    loc_id: i64,
    turret_tid: u32,
    ammo_tid: u32,
    base_mod_id: i64,
) -> [i64; 3] {
    let dmg_mod_tid = if turret_tid == 54754 { 54975u32 } else { 519u32 };
    grant_combat_skills(ctx, char_id, &[ship_tid, turret_tid, ammo_tid, dmg_mod_tid, 3504]);
    let guns = [base_mod_id, base_mod_id + 1, base_mod_id + 2];
    let hi_flags = [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2];
    let med_flags = [InventoryFlag::MedSlot0, InventoryFlag::MedSlot1, InventoryFlag::MedSlot2];
    let lo_flags = [InventoryFlag::LoSlot0, InventoryFlag::LoSlot1, InventoryFlag::LoSlot2];
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(ship_id, make_item(ship_id, ship_tid, char_id, loc_id, InventoryFlag::Hangar, 1));
        for i in 0..3 {
            snap.items.insert(guns[i], make_item(guns[i], turret_tid, char_id, ship_id, hi_flags[i], 1));
            snap.items.insert(base_mod_id + 10 + i as i64, make_item(base_mod_id + 10 + i as i64, ammo_tid, char_id, ship_id, hi_flags[i], 5_000));
            let dmg_id = base_mod_id + 20 + i as i64;
            snap.items.insert(dmg_id, make_item(dmg_id, dmg_mod_tid, char_id, ship_id, lo_flags[i], 1));
            snap.module_online_states.insert(guns[i], true);
            snap.module_online_states.insert(dmg_id, true);
            if ship_tid == 54733 {
                let cap_id = base_mod_id + 30 + i as i64;
                snap.items.insert(cap_id, make_item(cap_id, 3504, char_id, ship_id, med_flags[i], 1));
                snap.module_online_states.insert(cap_id, true);
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    guns
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn setup_docked_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    secret: &str,
    char_id: i64,
    sys_id: u32,
    station_id: i64,
    ship_id: i64,
    ship_tid: u32,
    turret_tid: u32,
    ammo_tid: u32,
    base_mod_id: i64,
) -> (SessionState, [i64; 3]) {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String(secret.into())]).await.unwrap();
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.unwrap();
    let guns = fit_combat_ship(ctx, char_id, ship_id, ship_tid, station_id, turret_tid, ammo_tid, base_mod_id);
    let (const_id, reg_id) = ctx.sde.solar_systems.get(&sys_id).map(|s| (s.constellation_id, s.region_id)).unwrap_or((0, 0));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.constellation_id = const_id;
            ch.region_id = reg_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
    });
    ctx.sync_session(&mut sess);
    (sess, guns)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn setup_space_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    secret: &str,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_tid: u32,
    turret_tid: u32,
    ammo_tid: u32,
    base_mod_id: i64,
    pos: DVec3,
) -> (SessionState, [i64; 3]) {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String(secret.into())]).await.unwrap();
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.unwrap();
    let guns = fit_combat_ship(ctx, char_id, ship_id, ship_tid, sys_id as i64, turret_tid, ammo_tid, base_mod_id);
    let (const_id, reg_id) = ctx.sde.solar_systems.get(&sys_id).map(|s| (s.constellation_id, s.region_id)).unwrap_or((0, 0));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.constellation_id = const_id;
            ch.region_id = reg_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
    });
    ctx.sync_session(&mut sess);
    let handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_tid);
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ship_tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 180.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
    (sess, guns)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn warp_lock_and_destroy_via_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    beyonce_id: u64,
    sys_id: u32,
    ship_id: i64,
    guns: &[i64; 3],
    target_id: i64,
    warp_dest_id: Option<i64>,
    max_volleys: usize,
) {
    let handle = ctx.systems.get_or_spawn(sys_id);
    if handle.get_ball_snapshot(target_id).await.unwrap().is_none() {
        return;
    }
    if let Some(dest_id) = warp_dest_id {
        let _ = ctx
            .dispatch_rpc(
                sess,
                "beyonce",
                Some(beyonce_id),
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(dest_id), PyRep::Real(0.0)],
                &HashMap::new(),
                None,
            )
            .await;
        for _ in 0..35 {
            let _ = handle.step_manual(1.0).await;
            if handle.get_ball_snapshot(ship_id).await.ok().flatten().is_some_and(|(_, b)| !matches!(b.mode, MovementMode::Warp { .. })) {
                break;
            }
        }
    }
    if let Ok(Some((_, tb))) = handle.get_ball_snapshot(target_id).await {
        let sb_pos = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
        if sb_pos.distance(tb.position) > 35_000.0 {
            let _ = handle.set_ball_position(ship_id, tb.position + DVec3::new(5_000.0, 0.0, 0.0)).await;
        }
    } else {
        return;
    }
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await.expect("AddTarget");
    for _ in 0..45 {
        if handle.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) {
            break;
        }
        let _ = handle.step_manual(1.0).await;
    }
    for _ in 0..max_volleys {
        for &gid in guns {
            if handle.get_ball_snapshot(target_id).await.unwrap().is_none() {
                return;
            }
            rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::Integer(target_id)]).await.expect("dogmaIM.Activate");
        }
        if handle.get_ball_snapshot(target_id).await.unwrap().is_none() {
            return;
        }
        if handle.get_combat_state(ship_id).await.ok().flatten().is_some_and(|cs| cs.capacitor < 300.0) {
            let _ = handle.step_manual(25.0).await;
        }
    }
    assert!(
        handle.get_ball_snapshot(target_id).await.unwrap().is_none(),
        "Target {target_id} in system {sys_id} must be destroyed via dogmaIM.Activate"
    );
}

#[tokio::test]
async fn test_belt_rats_clone_soldier_mordu_and_officer_spawns() {
    // parity: feat:npc.belt-rats, feat:npc.clone-soldier-spawns, feat:npc.officer-npc
    eve_testkit::attest(&[
        "feat:npc.belt-rats",
        "feat:npc.clone-soldier-spawns",
        "feat:npc.officer-npc",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. Beltless High-Sec (`30_000_142` Jita): undocked pilot + cluster tick -> 0 belt rats, 0 Clone Soldiers, 0 Officers
    let (mut jita_sess, _) = setup_docked_pilot(&ctx, 7101, "demo", "demo-secret", 90_000_001, 30_000_142, 60_003_760, 910_001, 639, 2961, 21894, 920_000).await;
    rpc(&ctx, &mut jita_sess, "stationSvc", "Undock", &[]).await.expect("Undock Jita");
    tick_background_cluster(&ctx).await;
    let jita_rats = ctx.npc_spawns.read().unwrap().get(&30_000_142).map(|m| m.entities.len()).unwrap_or(0);
    assert_eq!(jita_rats, 0, "Beltless Jita must spawn 0 belt rats");
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == 30_000_142 && matches!(n.archetype.as_str(), "CloneSoldier" | "Officer" | "MorduCommander"))));

    // 2. High-Sec Belt System (`30_000_001` Tanoo, `0.858` sec): spawns frigate/destroyer belt rats, 0 Clone Soldiers, 0 Officers
    let tanoo_stn = ctx.sde.solar_systems[&30_000_001].stations[0].station_id;
    let (mut tanoo_sess, _) = setup_docked_pilot(&ctx, 7102, "other", "other-secret", 90_000_002, 30_000_001, tanoo_stn, 910_002, 639, 2961, 21894, 920_100).await;
    rpc(&ctx, &mut tanoo_sess, "stationSvc", "Undock", &[]).await.expect("Undock Tanoo");
    tick_background_cluster(&ctx).await;
    let tanoo_groups: Vec<u32> = ctx.npc_spawns.read().unwrap()[&30_000_001].entities.values().map(|e| e.spawn_group_id).collect();
    assert!(!tanoo_groups.is_empty(), "High-sec system with belts must spawn regional belt rats");
    for sg in &tanoo_groups {
        assert!(ctx.sde.npc_spawn_groups[sg].ship_class <= 2, "High-sec belt rats must be Frigate/Destroyer class (<= 2)");
    }
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == 30_000_001 && matches!(n.archetype.as_str(), "CloneSoldier" | "MorduCommander"))));

    // 3. Low-Sec Belt System (`30_000_012` Asabona, `0.323` sec): Clone Soldier + Mordu's Legion + Faction Commander
    let low_sys = 30_000_012u32;
    let low_stn = ctx.sde.solar_systems[&low_sys].stations[0].station_id;
    let low_belt_id = ctx.sde.celestials_in_system(low_sys).into_iter().find(|c| c.group_id == 9).unwrap().celestial_id as i64;
    let (mut low_sess, low_guns) = setup_docked_pilot(&ctx, 7103, "demo", "demo-secret", 90_000_001, low_sys, low_stn, 910_003, 54733, 54754, 54783, 920_200).await;
    rpc(&ctx, &mut low_sess, "stationSvc", "Undock", &[]).await.expect("Undock Asabona");
    let low_beyonce = bind_beyonce(&ctx, &mut low_sess, low_sys).await;

    let hit_seed = find_multi_hit_seed(&ctx, 0.08, 4);
    ctx.set_mechanic_rng_seed(Some(hit_seed));
    tick_background_cluster(&ctx).await;

    let (cs_bid, mordu_bid, cmd_bid) = with_combat_runtime(&ctx, |rt| {
        let cs = rt.npc_entities.values().find(|n| n.solar_system_id == low_sys && n.archetype == "CloneSoldier").expect("CloneSoldier spawned").ball_id;
        let md = rt.npc_entities.values().find(|n| n.solar_system_id == low_sys && n.archetype == "MorduCommander" && n.faction_id == 500_018).expect("Mordu Special Warfare Unit spawned").ball_id;
        let cm = rt.npc_entities.values().find(|n| n.solar_system_id == low_sys && n.archetype == "Officer").expect("Faction Commander spawned").ball_id;
        (cs, md, cm)
    });

    warp_lock_and_destroy_via_rpc(&ctx, &mut low_sess, low_beyonce, low_sys, 910_003, &low_guns, cs_bid, Some(low_belt_id), 15).await;
    warp_lock_and_destroy_via_rpc(&ctx, &mut low_sess, low_beyonce, low_sys, 910_003, &low_guns, mordu_bid, None, 15).await;
    warp_lock_and_destroy_via_rpc(&ctx, &mut low_sess, low_beyonce, low_sys, 910_003, &low_guns, cmd_bid, None, 20).await;

    ctx.db.with_snapshot(|snap| {
        let low_wrecks: Vec<i64> = snap.items.values().filter(|i| i.location_id == low_sys as i64 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186)).map(|i| i.item_id).collect();
        let wreck_loot: Vec<&InventoryItemRecord> = snap.items.values().filter(|i| low_wrecks.contains(&i.location_id)).collect();
        assert!(wreck_loot.iter().any(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 1206)), "Clone Soldier wreck must drop SDE Security Tag (group 1206)");
        assert!(wreck_loot.iter().any(|i| matches!(i.type_id, 33817 | 33819 | 33821) && i.is_blueprint_copy && i.blueprint_runs_remaining == Some(1)), "Mordu Commander wreck must drop 1-run Garmur/Orthrus/Barghest BPC");
        assert!(wreck_loot.iter().any(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.meta_group_id == Some(4))), "Faction Commander wreck must drop SDE Faction module (meta_group 4)");
    });

    tick_background_cluster(&ctx).await;
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == low_sys && n.archetype == "CloneSoldier")), "Clone Soldier must respect 20m respawn cooldown");
    let (paid_isk, _, _) = flush_npc_bounty_tick(&ctx, low_sys, 90_000_001, 0.0);
    assert!(paid_isk > 0.0, "20m bounty tick must pay pilot ISK for low-sec NPC kills");
    ctx.advance_secs(1_205);
    tick_background_cluster(&ctx).await;
    assert!(with_combat_runtime(&ctx, |rt| rt.npc_entities.values().any(|n| n.solar_system_id == low_sys && n.archetype == "CloneSoldier")), "Clone Soldier must respawn after 20m cooldown");

    // 4. True Null-Sec (`30_001_269` H-PA29, `-0.241` sec): Named Guristas Officer (`group 564`) + `meta_group 5` Officer module drop
    let null_sys = 30_001_269u32;
    let null_belt = ctx.sde.celestials_in_system(null_sys).into_iter().find(|c| c.group_id == 9).unwrap();
    let (mut null_sess, null_guns) = setup_space_pilot(&ctx, 7104, "other", "other-secret", 90_000_002, null_sys, 910_004, 54733, 54754, 54783, 920_300, null_belt.position + DVec3::new(4_000.0, 0.0, 0.0)).await;
    let null_beyonce = bind_beyonce(&ctx, &mut null_sess, null_sys).await;
    // Seed 1_603_003 yields 6 consecutive draws < 0.10, guaranteeing the null-sec Officer spawn roll (0.10) hits
    let officer_seed = 1_603_003u64;
    ctx.set_mechanic_rng_seed(Some(officer_seed));
    tick_background_cluster(&ctx).await;

    let (off_bid, off_tid) = with_combat_runtime(&ctx, |rt| {
        let off = rt.npc_entities.values().find(|n| n.solar_system_id == null_sys && ctx.sde.types.get(&n.type_id).is_some_and(|t| t.group_id == 564)).expect("Named Guristas Officer (group 564) spawned in null-sec");
        (off.ball_id, off.type_id)
    });
    assert!(ctx.sde.types[&off_tid].default_attributes.get(&481).copied().unwrap_or(0.0) >= 1_000_000.0);
    warp_lock_and_destroy_via_rpc(&ctx, &mut null_sess, null_beyonce, null_sys, 910_004, &null_guns, off_bid, Some(null_belt.celestial_id as i64), 35).await;

    let has_officer_mod = ctx.db.with_snapshot(|snap| {
        let wrecks: Vec<i64> = snap.items.values().filter(|i| i.location_id == null_sys as i64 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186)).map(|i| i.item_id).collect();
        snap.items.values().any(|i| wrecks.contains(&i.location_id) && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.meta_group_id == Some(5)))
    });
    assert!(has_officer_mod, "Destroyed Null-Sec Officer must drop authentic SDE Officer module (meta_group 5)");
}

#[tokio::test]
async fn test_drifters_overshield_lux_kontos_doomsday_and_wreck() {
    // parity: feat:npc.drifters
    eve_testkit::attest(&["feat:npc.drifters"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let wh_sys = 31_000_007u32;
    let anchor = ctx.sde.celestials_in_system(wh_sys).first().map(|c| c.position).unwrap_or(DVec3::ZERO) + DVec3::new(15_000.0, 0.0, 0.0);
    let (mut sess, guns) = setup_space_pilot(&ctx, 7201, "demo", "demo-secret", 90_000_001, wh_sys, 910_101, 54733, 54754, 54783, 921_000, anchor + DVec3::new(6_000.0, 0.0, 0.0)).await;
    let beyonce_id = bind_beyonce(&ctx, &mut sess, wh_sys).await;

    tick_background_cluster(&ctx).await;

    let (cruiser_bid, bs_bid) = with_combat_runtime(&ctx, |rt| {
        let c = rt.npc_entities.values().find(|n| n.solar_system_id == wh_sys && n.archetype == "Drifter" && ctx.sde.types[&n.type_id].group_id == 1956).expect("Drifter Cruiser spawned");
        let b = rt.npc_entities.values().find(|n| n.solar_system_id == wh_sys && n.archetype == "Drifter" && ctx.sde.types[&n.type_id].group_id == 1310).expect("Drifter Battleship spawned");
        assert_eq!(c.drifter_overshield_hp, 0.0);
        assert_eq!(b.drifter_overshield_hp, 4_000.0);
        assert!(!b.drifter_doomsday_fired);
        (c.ball_id, b.ball_id)
    });

    let handle = ctx.systems.get_or_spawn(wh_sys);
    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(bs_bid)]).await.expect("Lock Drifter BS");
    for _ in 0..35 {
        if handle.get_target_locks(910_101).await.unwrap().iter().any(|l| l.target_id == bs_bid && l.locked) {
            break;
        }
        let _ = handle.step_manual(1.0).await;
    }

    // Fire 1 Vorton volley via dogmaIM.Activate to break the 4,000 HP overshield while leaving the Drifter BS alive
    for &gid in &guns {
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::Integer(bs_bid)]).await.expect("Activate gun on Drifter BS");
    }
    let ship_hp_before_dd = {
        let cs = handle.get_combat_state(910_101).await.unwrap().unwrap();
        cs.shield_hp + cs.armor_hp + cs.structure_hp
    };

    tick_background_cluster(&ctx).await;
    let (ov_after, dd_fired, cruiser_dd) = with_combat_runtime(&ctx, |rt| {
        let b = &rt.npc_entities[&bs_bid];
        let c = &rt.npc_entities[&cruiser_bid];
        (b.drifter_overshield_hp, b.drifter_doomsday_fired, c.drifter_doomsday_fired)
    });
    assert_eq!(ov_after, 0.0, "Drifter overshield must deplete to 0.0 under weapon fire");
    assert!(dd_fired, "Drifter Battleship must fire Lux Kontos Doomsday upon overshield depletion");
    assert!(!cruiser_dd, "Drifter Cruiser must never fire Lux Kontos Doomsday");

    let ship_hp_after_dd = {
        let cs = handle.get_combat_state(910_101).await.unwrap().unwrap();
        cs.shield_hp + cs.armor_hp + cs.structure_hp
    };
    assert!(ship_hp_before_dd - ship_hp_after_dd >= 4_500.0, "Lux Kontos Doomsday must deal massive omni damage to target tank");

    warp_lock_and_destroy_via_rpc(&ctx, &mut sess, beyonce_id, wh_sys, 910_101, &guns, bs_bid, None, 35).await;
    let has_drifter_wreck = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| {
            i.location_id == wh_sys as i64
                && (matches!(i.type_id, 34768 | 37531 | 56741)
                    || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186 && t.name.contains("Drifter")))
        })
    });
    assert!(has_drifter_wreck, "Destroyed Drifter Battleship must spawn SDE Drifter Wreck in space");
}
