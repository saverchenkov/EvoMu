//! Additional integration tests for batch 41 `B41-P1-01..04` remediations:
//! - `feat:pve.insurgency-outpost-raid`: deadspace warp block, gate-admitted occupancy, T3D/T3C, warzone militia
//! - `feat:pve.insurgency-laundering`: probabilistic `MechanicRng` Stage >= 3 named roamer promotion
//! - `feat:pve.proving-grounds-queue-and-rewards`: party size, boss, <= 30 km range, cargo module, jump-clone implant

#![forbid(unsafe_code)]

use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay_closure::{
    lock_target_via_rpc, register_online_pilot, rpc_call,
};
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_combat_ephemeral, with_mut_combat_ephemeral};
use eve_server::services::insurgency_complex_ops::{
    is_named_insurgency_roamer, load_ops_state, OutpostRaidSize,
};
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, FORECASTING_DURATION_SECS,
};
use eve_server::services::pvp_filament_mgr::{get_pvp_filament_match_spec, load_pvp_store};
use glam::DVec3;

fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("__server_now_unix".into(), cur.to_string());
    });
}

fn fit_small_railgun(ctx: &ServerContext, char_id: i64, ship_id: i64, gun_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun_id,
            InventoryItemRecord {
                item_id: gun_id,
                type_id: 10_678,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            gun_id + 1,
            InventoryItemRecord {
                item_id: gun_id + 1,
                type_id: 222,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 200,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(gun_id, true);
    });
}

async fn kill_npc_with_turret_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    gun_id: i64,
    npc_id: i64,
) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let npc_pos = sys_h
        .get_ball_snapshot(npc_id)
        .await
        .unwrap()
        .expect("NPC ball")
        .1
        .position;
    let _ = sys_h
        .set_ball_position(ship_id, npc_pos + DVec3::new(1_200.0, 0.0, 0.0))
        .await;
    let mut cs = sys_h.get_combat_state(npc_id).await.unwrap().unwrap();
    (
        cs.shield_hp,
        cs.armor_hp,
        cs.structure_hp,
        cs.shield_recharge_time_ms,
    ) = (0.0, 0.0, 1.0, 1.0e15);
    let _ = sys_h.set_combat_state(npc_id, cs).await;
    let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
    lock_target_via_rpc(ctx, sess, sys_id, ship_id, npc_id).await;
    rpc_call(
        ctx,
        sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(npc_id),
        ],
    )
    .await
    .expect("dogmaIM.Activate on NPC");
    for _ in 0..8 {
        sys_h.step_manual(1.0).await.unwrap();
        if sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none() {
            break;
        }
    }
    assert!(sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none());
    let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
}

#[tokio::test]
async fn test_b41_outpost_raid_deadspace_warp_gate_occupancy_t3_and_warzone_faction() {
    // parity: feat:pve.insurgency-outpost-raid
    eve_testkit::attest(&["feat:pve.insurgency-outpost-raid"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let mut s_pir = register_online_pilot(
        &ctx, 89_501, "b41p2_pir", "pw", 91_500_001, 98_500_001, None, front_sys, 101_500_001, 587,
        DVec3::ZERO, 0,
    )
    .await;
    let mut s_pir_bypass = register_online_pilot(
        &ctx, 89_502, "b41p2_byp", "pw", 91_500_002, 98_500_002, None, front_sys, 101_500_002, 587,
        DVec3::ZERO, 0,
    )
    .await;
    let mut s_amarr = register_online_pilot(
        &ctx, 89_503, "b41p2_ama", "pw", 91_500_003, 98_500_003, None, front_sys, 101_500_003, 587,
        DVec3::ZERO, 0,
    )
    .await;
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc_call(&ctx, &mut s_pir_bypass, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc_call(&ctx, &mut s_amarr, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_003)]).await.unwrap();
    fit_small_railgun(&ctx, 91_500_001, 101_500_001, 250_001);
    fit_small_railgun(&ctx, 91_500_002, 101_500_002, 250_011);

    let _ = rpc_call(&ctx, &mut s_pir, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let gate_for = |sz: OutpostRaidSize| 1_893_100_000 + i64::from(front_sys) * 10 + sz.slot_index();
    let (g_small, g_med, g_mod, g_ladv) = (
        gate_for(OutpostRaidSize::Small),
        gate_for(OutpostRaidSize::Medium),
        gate_for(OutpostRaidSize::Moderate),
        gate_for(OutpostRaidSize::LargeAdv),
    );
    let sys_h = ctx.systems.get_or_spawn(front_sys);

    // 1. Direct warp from outside into Outpost Raid pocket is blocked (`NaturalPhenomenaDisruptingWarp`).
    let _ = sys_h.set_ball_position(101_500_002, OutpostRaidSize::Small.pocket_position()).await;
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(599, FleetRecord { fleet_id: 599, boss_id: 91_500_001, members: vec![91_500_001, 91_500_002], ..Default::default() });
        s.characters.get_mut(&91_500_001).unwrap().fleet_id = Some(599);
        s.characters.get_mut(&91_500_002).unwrap().fleet_id = Some(599);
    });
    s_pir.fleet_id = Some(599);
    let warp_err = rpc_call(
        &ctx,
        &mut s_pir,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("char".into()), PyRep::Integer(91_500_002), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(warp_err.code, "NaturalPhenomenaDisruptingWarp");
    let _ = sys_h.set_ball_position(101_500_002, DVec3::ZERO).await;

    // 2. Cross-warzone Empire militia (Amarr 500_003 in Guristas 500_010 campaign) is rejected at gate.
    let _ = sys_h.set_ball_position(101_500_003, OutpostRaidSize::Small.gate_position()).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_amarr, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)])
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );

    // 3. T3 Tactical Destroyer (34828 Jackdaw, group 1305): rejected at Small, allowed at Medium.
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_500_001).unwrap().type_id = 34_828);
    let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Small.gate_position()).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)])
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedForComplex"
    );
    let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Medium.gate_position()).await;
    rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_med)])
        .await
        .expect("T3D allowed in Medium Outpost Raid");

    // 4. T3 Strategic Cruiser (29984 Tengu, group 963): rejected at Medium & Moderate, allowed at LargeAdv.
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_500_001).unwrap().type_id = 29_984);
    let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Medium.gate_position()).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_med)])
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedForComplex"
    );
    let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Moderate.gate_position()).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_mod)])
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedForComplex"
    );
    let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::LargeAdv.gate_position()).await;
    rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_ladv)])
        .await
        .expect("T3C allowed in LargeAdv Outpost Raid");

    // 5. Gate-admitted occupancy & live WCS re-validation:
    // Admit s_pir via gate in Frigate (587) to spawn & clear Small defender, then move s_pir out while
    // placing s_pir_bypass (who never took the gate) inside Small pocket -> capture progress stays 0.0.
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_500_001).unwrap().type_id = 587);
    let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Small.gate_position()).await;
    rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)])
        .await
        .expect("enter Small raid in Frigate");
    let mordus_small = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].mordus_defender_ball_id.unwrap());
    kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, 101_500_001, 250_001, mordus_small).await;
    let _ = sys_h.set_ball_position(101_500_001, DVec3::ZERO).await;
    let _ = sys_h.set_ball_position(101_500_002, OutpostRaidSize::Small.pocket_position()).await;
    fast_forward_secs(&ctx, 120);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].weighted_elapsed_secs),
        0.0,
        "Pilot who bypassed acceleration gate cannot capture Outpost Raid"
    );

    // Return gate-admitted s_pir to Small pocket with WCS fitted -> capture progress stays 0.0.
    let _ = sys_h.set_ball_position(101_500_002, DVec3::ZERO).await;
    let _ = sys_h.set_ball_position(101_500_001, OutpostRaidSize::Small.pocket_position()).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            250_099,
            InventoryItemRecord {
                item_id: 250_099,
                type_id: 10_998,
                owner_id: 91_500_001,
                location_id: 101_500_001,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    fast_forward_secs(&ctx, 120);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].weighted_elapsed_secs),
        0.0,
        "Pilot with WCS fitted mid-pocket cannot progress Outpost Raid capture"
    );
}

#[tokio::test]
async fn test_b41_insurgency_stage3_roamer_probabilistic_rng_promotion() {
    // parity: feat:pve.insurgency-laundering
    eve_testkit::attest(&["feat:pve.insurgency-laundering"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let mut s_emp = register_online_pilot(
        &ctx, 89_520, "b41p2_rng", "pw", 91_520_001, 98_520_001, None, front_sys, 101_520_001, 621,
        DVec3::ZERO, 0,
    )
    .await;
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp_id = ctx.db.with_snapshot(|s| {
        *s.insurgency_runtime.campaigns.iter().find(|(_, c)| c.pirate_faction_id == 500_010).unwrap().0
    });
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    assert_eq!(add_system_corruption(&ctx, camp_id, front_sys, 600).1, 3);
    assert_eq!(add_system_suppression(&ctx, camp_id, front_sys, 600).1, 3);

    ctx.mechanic_rng.set_seed(Some(42));
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let (mut named_pir, mut named_enf) = (0_usize, 0_usize);
    for _ in 0..20 {
        fast_forward_secs(&ctx, 2);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let (pir_ball, pir_tid, enf_ball, enf_tid) = with_combat_ephemeral(&ctx, |rt| {
            let p = rt.npc_entities.values().find(|n| n.solar_system_id == front_sys && n.archetype == "InsurgencyPirateRoamer").unwrap();
            let e = rt.npc_entities.values().find(|n| n.solar_system_id == front_sys && n.archetype == "InsurgencyEnforcer").unwrap();
            (p.ball_id, p.type_id, e.ball_id, e.type_id)
        });
        if is_named_insurgency_roamer(pir_tid) {
            named_pir += 1;
        }
        if is_named_insurgency_roamer(enf_tid) {
            named_enf += 1;
        }
        for (bid, btid) in [(pir_ball, pir_tid), (enf_ball, enf_tid)] {
            let _ = sys_h.remove_ball(bid).await;
            with_mut_combat_ephemeral(&ctx, |rt| {
                rt.npc_entities.remove(&bid);
            });
            eve_server::services::insurgency_complex_ops::on_insurgency_npc_destroyed(&ctx, front_sys, bid, btid, 0, 0).await;
        }
    }
    assert!(
        (1..20).contains(&named_pir) && (1..20).contains(&named_enf),
        "Stage >= 3 named roamer promotion must be probabilistic via MechanicRng (~35%), got pir={named_pir}/20 enf={named_enf}/20"
    );
}

#[tokio::test]
async fn test_b41_proving_grounds_party_size_boss_range_cargo_implant_and_atomic_ticket() {
    // parity: feat:pve.proving-grounds-queue-and-rewards
    eve_testkit::attest(&["feat:pve.proving-grounds-queue-and-rewards"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let jita = 30_000_142_u32;
    let fil_2v2 = 72_984_u32; // 2v2 Battleships Proving Filament
    let (fil_1v1, ship_1v1) = ctx
        .sde
        .types
        .values()
        .find_map(|t| {
            let sp = get_pvp_filament_match_spec(&ctx.sde, t.type_id);
            if t.group_id != 4050 || t.name.contains("Expired") || sp.fleet_size != 1 {
                return None;
            }
            let lid = sp.ship_restrictions_list_id?;
            let tl = ctx.sde.type_lists.get(&lid)?;
            ctx.sde.types.values().find(|st| tl.contains(st)).map(|st| (t.type_id, st.type_id))
        })
        .expect("1v1 Proving Grounds filament in SDE");
    let spec_2v2 = get_pvp_filament_match_spec(&ctx.sde, fil_2v2);
    let high_meta_mod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 7 && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec_2v2.max_meta_level))
        .unwrap()
        .type_id;
    let high_meta_imp_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 20 && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec_2v2.max_meta_level))
        .unwrap()
        .type_id;

    let mut sessions = Vec::new();
    let mut ids = Vec::new();
    for i in 0..4_i64 {
        let (cid, sid, fid) = (91_530_001 + i, 101_530_001 + i, 885_301 + i);
        let s = register_online_pilot(
            &ctx,
            89_531 + i as u64,
            &format!("b41p2_pg_{i}"),
            "pw",
            cid,
            98_530_001 + i,
            None,
            jita,
            sid,
            638,
            DVec3::new(40_000_000.0 + (i as f64) * 2_000.0, 0.0, 0.0),
            0,
        )
        .await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                fid,
                InventoryItemRecord {
                    item_id: fid,
                    type_id: fil_2v2,
                    owner_id: cid,
                    location_id: sid,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                fid + 100,
                InventoryItemRecord {
                    item_id: fid + 100,
                    type_id: fil_1v1,
                    owner_id: cid,
                    location_id: sid,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        ids.push((cid, sid, fid));
        sessions.push(s);
    }

    // 1. Solo pilot activating 2v2 filament -> FleetTooSmallForFilament.
    assert_eq!(
        rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)])
            .await
            .unwrap_err()
            .code,
        "FleetTooSmallForFilament"
    );

    // 2. 2-person fleet: non-boss -> NotFleetBoss; 1v1 filament -> FleetTooLargeForFilament; > 30 km -> FleetMemberOutOfRange.
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(601, FleetRecord { fleet_id: 601, boss_id: ids[0].0, members: vec![ids[0].0, ids[1].0], ..Default::default() });
        s.fleets.insert(602, FleetRecord { fleet_id: 602, boss_id: ids[2].0, members: vec![ids[2].0, ids[3].0], ..Default::default() });
    });
    assert_eq!(
        rpc_call(&ctx, &mut sessions[1], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[1].2), PyRep::Integer(fil_2v2 as i64)])
            .await
            .unwrap_err()
            .code,
        "NotFleetBoss"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&ids[0].1).unwrap().type_id = ship_1v1;
        s.items.get_mut(&ids[1].1).unwrap().type_id = ship_1v1;
    });
    assert_eq!(
        rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2 + 100), PyRep::Integer(fil_1v1 as i64)])
            .await
            .unwrap_err()
            .code,
        "FleetTooLargeForFilament"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&ids[0].1).unwrap().type_id = 638;
        s.items.get_mut(&ids[1].1).unwrap().type_id = 638;
    });

    let jita_h = ctx.systems.get_or_spawn(jita);
    let _ = jita_h.set_ball_position(ids[1].1, DVec3::new(40_035_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)])
            .await
            .unwrap_err()
            .code,
        "FleetMemberOutOfRange"
    );
    let _ = jita_h.set_ball_position(ids[1].1, DVec3::new(40_002_000.0, 0.0, 0.0)).await;

    // 3. High-meta module carried in Cargo -> InvalidModuleTierForFilament.
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            885_501,
            InventoryItemRecord {
                item_id: 885_501,
                type_id: high_meta_mod_tid,
                owner_id: ids[1].0,
                location_id: ids[1].1,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)])
            .await
            .unwrap_err()
            .code,
        "InvalidModuleTierForFilament"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&885_501);
        // Active-body implant item (location_id == pilot_id) -> rejected.
        s.items.insert(
            885_502,
            InventoryItemRecord {
                item_id: 885_502,
                type_id: high_meta_imp_tid,
                owner_id: ids[1].0,
                location_id: ids[1].0,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)])
            .await
            .unwrap_err()
            .code,
        "InvalidImplantsForFilament"
    );

    // 4. Jump-clone implant (location_id == clone_id != pilot_id, owner_id == pilot_id) is ALLOWED & atomic 2v2 ticket match!
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&885_502).unwrap().location_id = 777_000_001; // stored jump clone ID
    });
    assert_eq!(
        rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_2v2 as i64)]).await.unwrap(),
        PyRep::Integer(30)
    );
    assert_eq!(
        rpc_call(&ctx, &mut sessions[2], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[2].2), PyRep::Integer(fil_2v2 as i64)]).await.unwrap(),
        PyRep::Integer(5)
    );
    ctx.db.with_snapshot(|s| {
        let st = load_pvp_store(s);
        assert!(st.queue.is_empty(), "whole-fleet tickets consumed atomically");
        let m = st.active_matches.values().next().unwrap();
        assert_eq!(m.team_a.len() + m.team_b.len(), 4);
        assert!(s.items.contains_key(&885_502), "jump-clone implant preserved");
    });
}
