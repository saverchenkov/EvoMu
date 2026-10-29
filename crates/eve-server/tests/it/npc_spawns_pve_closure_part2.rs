//! Part 2 of V24.01 NPC & PvE closure integration tests:
//! Living World NPC Mining Ops & Hauler Convoys, CONCORD & Faction Police enforcement,
//! and SDE-backed Pirate Strongholds, Homefront Operations, and Triglavian Sites.

#![forbid(unsafe_code)]

use crate::common;
use crate::npc_spawns_pve_closure::{
    bind_beyonce, rpc, setup_docked_pilot, setup_space_pilot, warp_lock_and_destroy_via_rpc,
};
use eve_destiny::DVec3;
use eve_proto::PyRep;
use eve_server::living_world::NpcArchetype;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::services::insurgency_services::{
    ensure_stronghold_in_space, init_insurgency_campaign, FACTION_GURISTAS,
    FORECASTING_DURATION_SECS, PHASE_FORECASTING, PHASE_WINDDOWN, PIRATE_STRONGHOLD_WRECK_TYPE,
};
use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
use eve_server::sp6_lifecycle::tick_background_cluster;
use std::collections::HashMap;

fn sum_sell_order_volume(orders_rep: &PyRep) -> f64 {
    let PyRep::Tuple(tup) = orders_rep else {
        panic!("expected Tuple from marketProxy.GetOrders");
    };
    tup[0]
        .crowset_rows()
        .expect("sell crowset")
        .iter()
        .filter_map(|r| r.dbrow_get("volRemaining").and_then(|v| v.as_f64()))
        .sum()
}

#[tokio::test]
async fn test_npc_mining_ops_and_hauler_convoy_lifecycle() {
    // parity: feat:npc.npc-hauler, feat:npc.mining-op-npc, feat:mining.npc-mining-ops
    eve_testkit::attest(&[
        "feat:npc.npc-hauler",
        "feat:npc.mining-op-npc",
        "feat:mining.npc-mining-ops",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let sys_id = 30_000_001u32; // Tanoo (high-sec with asteroid belts, NPC station, and stargates)
    let stn_id = ctx.sde.solar_systems[&sys_id].stations[0].station_id;
    let belt_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 9)
        .unwrap()
        .position;

    let (mut sess, guns) = setup_docked_pilot(
        &ctx, 7301, "demo", "demo-secret", 90_000_001, sys_id, stn_id, 910_201, 54733, 54754,
        54783, 922_000,
    )
    .await;
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock Tanoo");
    rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
    )
    .await
    .expect("SetSafetyLevel Red");

    let tanoo_handle = ctx.systems.get_or_spawn(sys_id);
    let init_asts = tanoo_handle.populate_asteroid_belt_entities().await.unwrap();
    let init_total_units: u64 = init_asts.iter().map(|a| a.remaining_units).sum();

    let init_orders = rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(1230)],
    )
    .await
    .expect("GetOrders Veldspar");
    let init_sell_vol = sum_sell_order_volume(&init_orders);

    // Tick 1: spawns NPC Venture (32880) + racial Industrial at the belt, mines 1,600 SDE Veldspar units, and jumps HaulerConvoy through stargate!
    tick_background_cluster(&ctx).await;

    let convoys = ctx.living_world.list_convoys();
    let mining_op = convoys
        .iter()
        .find(|c| c.archetype == NpcArchetype::MiningOp && c.current_system_id == sys_id)
        .expect("NPC MiningOp spawned in Tanoo");
    let hauler_convoy = convoys
        .iter()
        .find(|c| c.archetype == NpcArchetype::HaulerConvoy && c.origin_system_id == sys_id)
        .expect("NPC HaulerConvoy spawned from Tanoo");
    assert_eq!(mining_op.ship_type_id, 32880);
    assert_eq!(
        mining_op.cargo_quantity, 1_600,
        "SDE Venture + 2x Miner I must extract 1,600 Veldspar units (160 m3) on tick 1"
    );
    assert_ne!(
        hauler_convoy.current_system_id, sys_id,
        "HaulerConvoy must jump stargate to connected system on tick"
    );

    let (_, miner_ball) = tanoo_handle
        .get_ball_snapshot(mining_op.convoy_id)
        .await
        .unwrap()
        .expect("NPC Venture ball in belt");
    assert!(
        miner_ball.position.distance(belt_pos) <= 50_000.0,
        "NPC Venture must be spawned on the asteroid belt grid"
    );
    assert!(
        tanoo_handle
            .get_ball_snapshot(mining_op.convoy_id + 1_000_000)
            .await
            .unwrap()
            .is_some(),
        "Companion NPC Industrial hauler ball on belt grid"
    );

    // Advance 33 more ticks (each 20,000-unit rock takes 12 full + 1 partial 800-unit tick -> 33 ticks = 51,200 units >= 50,000 Venture ore hold)!
    for _ in 0..33 {
        tick_background_cluster(&ctx).await;
    }
    let post_asts = tanoo_handle.populate_asteroid_belt_entities().await.unwrap();
    let post_total_units: u64 = post_asts.iter().map(|a| a.remaining_units).sum();
    assert!(
        init_total_units - post_total_units >= 50_000,
        "Belt asteroids must deplete over ticks by >= 50,000 units"
    );

    let post_orders = rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(1230)],
    )
    .await
    .expect("GetOrders Veldspar after haul");
    let post_sell_vol = sum_sell_order_volume(&post_orders);
    assert!(
        post_sell_vol >= init_sell_vol + 50_000.0,
        "NPC mining op haul must replenish regional Veldspar sell volume by >= 50,000 units via marketProxy.GetOrders"
    );

    // Pursue an organically spawned HaulerConvoy via CmdStargateJump and destroy via dogmaIM.Activate!
    let active_hauler = ctx
        .living_world
        .list_convoys()
        .into_iter()
        .find(|c| {
            c.archetype == NpcArchetype::HaulerConvoy
                && !c.destroyed
                && !ctx.living_world.is_convoy_docked(c.convoy_id)
        })
        .unwrap_or_else(|| {
            let cid = ctx.living_world.ensure_system_hauler_convoy(sys_id, &ctx.sde);
            ctx.living_world.get_convoy(cid).unwrap()
        });
    if active_hauler.current_system_id == sys_id {
        tick_background_cluster(&ctx).await;
    }
    let jumped_hauler = ctx.living_world.get_convoy(active_hauler.convoy_id).unwrap();
    let dest_sys = jumped_hauler.current_system_id;
    let gate = ctx
        .sde
        .stargates()
        .values()
        .find(|g| g.from_solar_system_id == sys_id && g.to_solar_system_id == dest_sys)
        .expect("Connecting stargate");
    let _ = tanoo_handle
        .set_ball_position(910_201, gate.position + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    ctx.advance_secs(15);
    let tanoo_beyonce = bind_beyonce(&ctx, &mut sess, sys_id).await;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(tanoo_beyonce),
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdStargateJump to pursue HaulerConvoy");
    ctx.advance_secs(15);

    let dest_beyonce = bind_beyonce(&ctx, &mut sess, dest_sys).await;
    warp_lock_and_destroy_via_rpc(
        &ctx,
        &mut sess,
        dest_beyonce,
        dest_sys,
        910_201,
        &guns,
        jumped_hauler.convoy_id,
        None,
        15,
    )
    .await;
    assert!(
        ctx.living_world.get_convoy(jumped_hauler.convoy_id).unwrap().destroyed,
        "HaulerConvoy must be marked destroyed when killed in space"
    );
    let dropped_trit = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|i| {
            i.type_id == 34
                && i.quantity >= 25_000
                && snap
                    .items
                    .get(&i.location_id)
                    .is_some_and(|w| w.location_id == dest_sys as i64)
        })
    });
    assert!(
        dropped_trit,
        "Destroyed HaulerConvoy must drop its 25,000 Tritanium cargo into a space wreck"
    );
}

#[tokio::test]
async fn test_concord_response_and_faction_police_enforcement() {
    // parity: feat:npc.concord-npc, feat:npc.navy-npc
    eve_testkit::attest(&["feat:npc.concord-npc", "feat:npc.navy-npc"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let jita = 30_000_142u32;
    let jita_stn = 60_003_760i64;

    let (mut att_sess, att_guns) = setup_docked_pilot(
        &ctx, 7401, "demo", "demo-secret", 90_000_001, jita, jita_stn, 910_301, 639, 2961, 21894,
        923_000,
    )
    .await;
    let (mut vic_sess, _) = setup_docked_pilot(
        &ctx, 7402, "other", "other-secret", 90_000_002, jita, jita_stn, 910_302, 587, 2961, 21894,
        923_100,
    )
    .await;
    // Victim leaves ship in station to undock in capsule (670) so 3 unlawful shots organically drive attacker sec status <= -5.0!
    ctx.advance_secs(15);
    rpc(
        &ctx,
        &mut vic_sess,
        "ship",
        "LeaveShip",
        &[PyRep::Integer(910_302)],
    )
    .await
    .expect("Victim boards capsule");
    ctx.advance_secs(15);
    rpc(&ctx, &mut att_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Attacker undocks");
    rpc(&ctx, &mut vic_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Victim capsule undocks");
    let vic_pod_id = ctx.db.get_character(90_000_002).unwrap().active_ship_id;
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&vic_pod_id].type_id), 670);
    ctx.advance_secs(35);

    // Lawful pilots in Jita trigger ZERO Faction Police on cluster tick
    tick_background_cluster(&ctx).await;
    assert!(!with_combat_runtime(&ctx, |rt| {
        rt.npc_entities
            .values()
            .any(|n| n.solar_system_id == jita && n.archetype == "FactionPolice")
    }));

    // 1. Three unlawful high-sec capsule shots via dogmaIM.Activate -> Criminal flag + sec status <= -5.0 + 6s CONCORD response!
    rpc(
        &ctx,
        &mut att_sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
    )
    .await
    .unwrap();
    let jita_handle = ctx.systems.get_or_spawn(jita);
    rpc(
        &ctx,
        &mut att_sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(vic_pod_id)],
    )
    .await
    .unwrap();
    for _ in 0..30 {
        if jita_handle
            .get_target_locks(910_301)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == vic_pod_id && l.locked)
        {
            break;
        }
        let _ = jita_handle.step_manual(1.0).await;
    }
    for &gid in &att_guns {
        rpc(
            &ctx,
            &mut att_sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(gid), PyRep::Integer(vic_pod_id)],
        )
        .await
        .expect("Unlawful high-sec pod shot");
    }

    ctx.advance_secs(2);
    tick_background_cluster(&ctx).await;
    assert_eq!(
        ctx.db.get_character(90_000_001).unwrap().active_ship_id,
        910_301,
        "CONCORD must respect 6s Jita arrival delay"
    );

    ctx.advance_secs(6);
    tick_background_cluster(&ctx).await;
    ctx.sync_session(&mut att_sess);
    let reports = ctx.crimewatch.concord_reports();
    let rep = reports
        .iter()
        .rfind(|r| r.criminal_char_id == 90_000_001)
        .expect("CONCORD execution report emitted");
    assert!(
        rep.ecm_jammed
            && rep.webbed_speed_fraction == 0.0
            && rep.warp_scramble_strength >= 100
            && rep.capacitor_after_neut == 0.0,
        "CONCORD must apply ECM jam, 100% web, warp scram, and cap drain"
    );

    // Verify CONCORD Police Commander (11957) ball is spawned in Jita and 100% invulnerable to damage
    let (_, concord_ball) = jita_handle
        .get_ball_snapshot(rep.concord_ball_id)
        .await
        .unwrap()
        .expect("CONCORD ball in space");
    assert_eq!(concord_ball.type_id, 11_957);
    let concord_cs0 = jita_handle
        .get_combat_state(rep.concord_ball_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        concord_cs0.shield_resonance,
        [0.0; 4],
        "CONCORD ship must have 100% invulnerable shield resonance"
    );

    let att_after = ctx.db.get_character(90_000_001).unwrap();
    assert_ne!(att_after.active_ship_id, 910_301, "CONCORD must destroy the criminal ship");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&att_after.active_ship_id].type_id),
        670,
        "Criminal pilot must be ejected into capsule (670)"
    );
    assert!(
        att_after.security_status <= -5.0,
        "Three unlawful pod shots + CONCORD penalty must organically drop security status <= -5.0, got {}",
        att_after.security_status
    );

    let kr_rep = rpc(&ctx, &mut vic_sess, "bountyProxy", "GetMyKillRights", &[])
        .await
        .expect("GetMyKillRights");
    assert!(
        format!("{kr_rep:?}").contains("90000001"),
        "Victim must receive private kill right against criminal attacker"
    );

    // 2. Outlaw (`<= -5.0` sec) Faction Police (`feat:npc.navy-npc`): capsules never engaged & allowed to dock; non-capsule ship engaged & refused docking!
    ctx.advance_secs(920);
    tick_background_cluster(&ctx).await;
    assert!(
        !with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().any(|n| {
                n.solar_system_id == jita
                    && n.archetype == "FactionPolice"
                    && n.target_id == Some(att_after.active_ship_id)
            })
        }),
        "Capsules (670) at <= -5.0 security status must NEVER be engaged by Faction Police"
    );
    let att_beyonce = bind_beyonce(&ctx, &mut att_sess, jita).await;
    let stn_pos = ctx.sde.solar_systems[&jita]
        .stations
        .iter()
        .find(|s| s.station_id == jita_stn)
        .unwrap()
        .position;
    let _ = jita_handle
        .set_ball_position(att_after.active_ship_id, stn_pos + DVec3::new(200.0, 0.0, 0.0))
        .await;
    ctx.dispatch_rpc(
        &mut att_sess,
        "beyonce",
        Some(att_beyonce),
        "CmdDock",
        &[PyRep::Integer(jita_stn), PyRep::Integer(att_after.active_ship_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Outlaw capsule is allowed to dock");

    let (mut outlaw_sess, _) = setup_docked_pilot(
        &ctx, 7403, "demo", "demo-secret", 90_000_001, jita, jita_stn, 910_305, 587, 2961, 21894,
        923_200,
    )
    .await;
    ctx.advance_secs(15);
    rpc(&ctx, &mut outlaw_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Outlaw ship undocks");
    let outlaw_beyonce = bind_beyonce(&ctx, &mut outlaw_sess, jita).await;
    tick_background_cluster(&ctx).await;
    assert!(
        with_combat_runtime(&ctx, |rt| {
            rt.npc_entities.values().any(|n| {
                n.solar_system_id == jita
                    && n.archetype == "FactionPolice"
                    && n.target_id == Some(910_305)
            })
        }),
        "Outlaw non-capsule ship in high-sec must be targeted by Faction Police"
    );
    let _ = jita_handle
        .set_ball_position(910_305, stn_pos + DVec3::new(200.0, 0.0, 0.0))
        .await;
    let dock_err = ctx
        .dispatch_rpc(
            &mut outlaw_sess,
            "beyonce",
            Some(outlaw_beyonce),
            "CmdDock",
            &[PyRep::Integer(jita_stn), PyRep::Integer(910_305)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Outlaw non-capsule ship must be refused high-sec docking");
    assert!(!dock_err.code.is_empty());
}

#[tokio::test]
async fn test_sde_backed_pirate_strongholds_homefront_and_triglavian_sites() {
    // parity: feat:pve.pirate-strongholds, feat:pve.triglavian-sites, feat:pve.homefront-ops
    eve_testkit::attest(&[
        "feat:pve.pirate-strongholds",
        "feat:pve.triglavian-sites",
        "feat:pve.homefront-ops",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    // 1. Homefront Operations (`feat:pve.homefront-ops`): SDE archetype 70 in high-sec Jita vs empty in null-sec; boss kill via dogmaIM.Activate
    let jita = 30_000_142u32;
    let (mut sess, guns) = setup_docked_pilot(
        &ctx, 7501, "demo", "demo-secret", 90_000_001, jita, 60_003_760, 910_401, 54733, 54754,
        54783, 924_000,
    )
    .await;
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.unwrap();
    let jita_beyonce = bind_beyonce(&ctx, &mut sess, jita).await;

    let PyRep::Dict(null_hf) = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(30_001_269)],
    )
    .await
    .unwrap() else {
        panic!()
    };
    let PyRep::Dict(null_dngs) = &null_hf[0].1 else {
        panic!()
    };
    assert!(null_dngs.is_empty(), "Null-sec system must have 0 Homefront Operation instances");

    let PyRep::Dict(jita_hf) = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetHomefrontSiteInstances",
        &[PyRep::Integer(jita as i64)],
    )
    .await
    .unwrap() else {
        panic!()
    };
    let PyRep::Dict(jita_dngs) = &jita_hf[0].1 else {
        panic!()
    };
    let (hf_did, PyRep::List(hf_insts)) = (&jita_dngs[0].0, &jita_dngs[0].1) else {
        panic!()
    };
    assert_eq!(ctx.sde.dungeons[&(hf_did.as_i64().unwrap() as u32)].archetype_id, Some(70));
    let hf_iid = hf_insts[0].as_i64().unwrap();
    let hf_boss_bid = with_pve_special_reg(&ctx, |reg| {
        reg.homefront_sites[&jita]
            .iter()
            .find(|s| s.instance_id == hf_iid)
            .unwrap()
            .boss_npc_ball_id
    });

    let bal_before_hf = ctx.db.get_character(90_000_001).unwrap().balance.0;
    warp_lock_and_destroy_via_rpc(&ctx, &mut sess, jita_beyonce, jita, 910_401, &guns, hf_boss_bid, None, 25).await;
    assert!(
        ctx.db.get_character(90_000_001).unwrap().balance.0 > bal_before_hf,
        "Killing Homefront boss via dogmaIM.Activate must award CorporateRewardPayout ISK"
    );

    // 2. Triglavian Sites (`feat:pve.triglavian-sites`): Gammel (30003076) Striking Leshak (48122) killed via dogmaIM.Activate -> Survey DB (48121) + dual standings
    let gammel = 30_003_076u32;
    let (mut trig_sess, trig_guns) = setup_space_pilot(
        &ctx, 7502, "other", "other-secret", 90_000_002, gammel, 910_402, 54733, 54754, 54783,
        924_100, DVec3::ZERO,
    )
    .await;
    let trig_beyonce = bind_beyonce(&ctx, &mut trig_sess, gammel).await;
    let PyRep::Dict(g_rep) = rpc(
        &ctx,
        &mut trig_sess,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(gammel as i64)],
    )
    .await
    .unwrap() else {
        panic!()
    };
    let PyRep::Dict(g_dngs) = &g_rep[0].1 else {
        panic!()
    };
    assert_eq!(g_dngs.len(), 2);
    let trig_boss_bid = with_pve_special_reg(&ctx, |reg| reg.triglavian_sites[&gammel][0].boss_npc_ball_id);

    warp_lock_and_destroy_via_rpc(&ctx, &mut trig_sess, trig_beyonce, gammel, 910_402, &trig_guns, trig_boss_bid, None, 35).await;
    assert!(
        ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == 90_000_002 && i.type_id == 48_121)),
        "Triglavian boss kill must award Survey Database (48121)"
    );
    let ch2 = ctx.db.get_character(90_000_002).unwrap();
    assert!(
        ch2.standings[&500_027] > 0.0 && ch2.standings[&500_026] < 0.0,
        "Killing Triglavian boss must shift EDENCOM (+0.15) and Triglavian (-0.15) standings"
    );

    // 3. Pirate Strongholds (`feat:pve.pirate-strongholds`): Forecasting invulnerability via dogmaIM.Activate -> clock advance to Active -> destruction & WindDown
    let fob_sys = 30_002_048u32;
    init_insurgency_campaign(&ctx, 901, FACTION_GURISTAS, 500_001, fob_sys, vec![gammel]);
    let sh_id = ensure_stronghold_in_space(&ctx, 901).await.expect("Stronghold spawned");
    let (mut fob_sess, fob_guns) = setup_space_pilot(
        &ctx, 7503, "demo", "demo-secret", 90_000_001, fob_sys, 910_403, 23773, 41160, 20811,
        924_200, DVec3::new(60_000.0, 0.0, 50_000.0),
    )
    .await;
    let fob_beyonce = bind_beyonce(&ctx, &mut fob_sess, fob_sys).await;
    let fob_handle = ctx.systems.get_or_spawn(fob_sys);

    let PyRep::Tuple(fob_snap0) = rpc(&ctx, &mut fob_sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[])
        .await
        .unwrap() else {
        panic!()
    };
    assert_eq!(fob_snap0[1].dict_get("phase"), Some(&PyRep::String(PHASE_FORECASTING.into())));

    rpc(&ctx, &mut fob_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(sh_id)]).await.unwrap();
    for _ in 0..35 {
        if fob_handle.get_target_locks(910_403).await.unwrap().iter().any(|l| l.target_id == sh_id && l.locked) {
            break;
        }
        let _ = fob_handle.step_manual(1.0).await;
    }
    let sh_hp0 = fob_handle.get_combat_state(sh_id).await.unwrap().unwrap().shield_hp;
    rpc(&ctx, &mut fob_sess, "dogmaIM", "Activate", &[PyRep::Integer(fob_guns[0]), PyRep::Integer(sh_id)]).await.unwrap();
    assert_eq!(fob_handle.get_combat_state(sh_id).await.unwrap().unwrap().shield_hp, sh_hp0, "Stronghold must be invulnerable during Forecasting phase");

    ctx.advance_secs(FORECASTING_DURATION_SECS + 60);
    rpc(&ctx, &mut fob_sess, "dogmaIM", "Activate", &[PyRep::Integer(fob_guns[0]), PyRep::Integer(sh_id)]).await.unwrap();
    assert!(fob_handle.get_combat_state(sh_id).await.unwrap().unwrap().shield_hp < sh_hp0, "Stronghold must take real weapon damage in Active phase");

    warp_lock_and_destroy_via_rpc(&ctx, &mut fob_sess, fob_beyonce, fob_sys, 910_403, &fob_guns, sh_id, None, 1_500).await;
    let PyRep::List(inst_after) = rpc(&ctx, &mut fob_sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances", &[PyRep::Integer(fob_sys as i64)]).await.unwrap() else { panic!() };
    assert_eq!(inst_after.iter().find(|i| i.dict_get("instanceID") == Some(&PyRep::Integer(sh_id))).unwrap().dict_get("completed"), Some(&PyRep::Bool(true)));
    let PyRep::Tuple(fob_snap1) = rpc(&ctx, &mut fob_sess, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap() else { panic!() };
    assert_eq!(fob_snap1[1].dict_get("phase"), Some(&PyRep::String(PHASE_WINDDOWN.into())));
    assert!(fob_handle.ballpark.read().unwrap().balls.values().any(|b| b.type_id == PIRATE_STRONGHOLD_WRECK_TYPE));
}
