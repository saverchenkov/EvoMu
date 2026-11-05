//! Live Sovereignty and Faction Warfare capture flows with zero direct snapshot seeding.
//!
//! Every state transition is driven by MachoNet RPCs (`structureDeployment.Anchor`, `dogmaIM.Activate`,
//! `keeper.ActivateAccelerationGate`), GM slash commands for items/skills/travel, live space combat,
//! and cluster ticks. Snapshot reads are used only for assertions.

#![forbid(unsafe_code)]

use crate::esi_fw_sov_incursions_live::{
    arm_ship_autocannons, create_corp_and_alliance, lock_and_destroy_target, lock_target, rpc, select_gm_pilot, slash,
};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::sov_gameplay::{is_claimable_nullsec_system, ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_NODE_BASE_CAPTURE_SECS};
use eve_server::services::sov_services::{calculate_adm, TYPE_SOVEREIGNTY_HUB};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const TYPE_ENTOSIS_LINK_I: u32 = 34_593;
const TYPE_STRONTIUM: u32 = 16_275;

fn claimable_null_system(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .filter(|s| is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .min()
        .expect("claimable null-sec system")
}

fn sov_holder(ctx: &ServerContext, sys_id: u32) -> Option<i64> {
    ctx.db.with_snapshot(|s| s.system_states.get(&sys_id).and_then(|st| st.sov_alliance_id))
}

fn hub_state(ctx: &ServerContext, hub_id: i64) -> String {
    ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone())
}

fn entosis_progress(ctx: &ServerContext, target_id: i64) -> Option<f64> {
    ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress.get(&target_id).map(|e| e.3))
}

fn hub_nodes(ctx: &ServerContext, hub_id: i64) -> Vec<(i64, u32)> {
    ctx.db.with_snapshot(|s| {
        let mut v: Vec<(i64, u32)> = s
            .sov_runtime
            .command_nodes
            .iter()
            .filter(|(_, &(hid, _, _))| hid == hub_id)
            .map(|(&nid, &(_, nsys, _))| (nid, nsys))
            .collect();
        v.sort_unstable();
        v
    })
}

fn events(ctx: &ServerContext, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id).into_iter().map(|n| (n.event, n.args)).collect()
}

async fn give_item(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i32) -> i64 {
    slash(ctx, sess, &format!("/giveitem {type_id} {qty}")).await.dict_get("itemID").and_then(|v| v.as_i64()).expect("itemID")
}

/// Trains the Entosis Link I prerequisites and fits it into `flag` while docked; returns the module item ID.
async fn fit_entosis_link(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, flag: InventoryFlag) -> i64 {
    // Entosis Links need Omega-only skill levels: grant the account Omega time through GM tooling.
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    slash(ctx, sess, &format!("/setomega {uid} {}", ctx.now_unix() + 90 * 86_400)).await;
    let mut pending = vec![TYPE_ENTOSIS_LINK_I];
    let mut trained = std::collections::HashSet::new();
    while let Some(tid) = pending.pop() {
        for attr_id in [182_u16, 183, 184, 1285, 1289, 1290] {
            if let Some(&sk) = ctx.sde.types.get(&tid).and_then(|t| t.default_attributes.get(&attr_id)) {
                let sk = sk as u32;
                if sk > 0 && trained.insert(sk) {
                    slash(ctx, sess, &format!("/giveskill {sk} 5")).await;
                    pending.push(sk);
                }
            }
        }
    }
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let module = give_item(ctx, sess, TYPE_ENTOSIS_LINK_I, 1).await;
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(module), PyRep::Integer(ship_id), PyRep::Integer(flag as i64)])
        .await
        .expect("fit Entosis Link I");
    module
}

async fn entosis(ctx: &ServerContext, sess: &mut SessionState, module: i64, target: i64) -> Result<PyRep, eve_proto::BridgeRpcError> {
    rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(module), PyRep::String("default".into()), PyRep::Integer(target)]).await
}

async fn stop_module(ctx: &ServerContext, sess: &mut SessionState, module: i64) {
    rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(module), PyRep::String("default".into())])
        .await
        .expect("dogmaIM.Deactivate");
}

async fn advance_and_tick(ctx: &ServerContext, secs: u64) {
    ctx.advance_secs(secs);
    tick_background_cluster(ctx).await;
}

async fn anchor_hub(ctx: &ServerContext, sess: &mut SessionState, hub_item: i64) -> Result<PyRep, eve_proto::BridgeRpcError> {
    rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Live Sov Hub".into())]).await
}

#[tokio::test]
async fn test_sov_hub_anchor_from_cargo_claims_system_and_rejects_invalid_space() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut s_def = select_gm_pilot(&ctx, 74_001, "other", "other-secret", PILOT_2).await;
    let mut s_rival = select_gm_pilot(&ctx, 74_002, "demo", "demo-secret", PILOT_1).await;
    let (def_corp, Some(def_ally)) = create_corp_and_alliance(&ctx, &mut s_def, "Hub Wardens", "HUBW", Some(("Hub Warden Pact", "HWPT"))).await
    else {
        unreachable!()
    };
    let (_, Some(rival_ally)) = create_corp_and_alliance(&ctx, &mut s_rival, "Rival Claim", "RVCL", Some(("Rival Claim Bloc", "RVCB"))).await
    else {
        unreachable!()
    };

    let null_sys = claimable_null_system(&ctx);
    let pick = |pred: &dyn Fn(&eve_sde::SolarSystem) -> bool| {
        ctx.sde.solar_systems.values().filter(|s| pred(s)).map(|s| s.solar_system_id).min().expect("SDE system")
    };
    let high_sec = pick(&|s| s.security_status >= 0.5 && s.solar_system_id < 31_000_000);
    let low_sec = pick(&|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000);
    let npc_null = pick(&|s| {
        s.security_status <= 0.0 && s.solar_system_id < 31_000_000 && !is_claimable_nullsec_system(&ctx.sde, s.solar_system_id)
    });

    // 1. Negative: the Sovereignty Hub in cargo cannot be anchored in high-sec, low-sec, or NPC null-sec.
    slash(&ctx, &mut s_def, &format!("/tr {high_sec}")).await;
    let hub_item = give_item(&ctx, &mut s_def, TYPE_SOVEREIGNTY_HUB, 1).await;
    for sys in [high_sec, low_sec, npc_null] {
        slash(&ctx, &mut s_def, &format!("/tr {sys}")).await;
        let err = anchor_hub(&ctx, &mut s_def, hub_item).await.expect_err("anchoring outside claimable null-sec must fail");
        assert_eq!(err.code, "CannotAnchorSovHubOutsideNullSec", "system {sys}");
    }
    let def_ship = ctx.db.with_snapshot(|s| s.characters[&PILOT_2].active_ship_id);
    assert!(
        ctx.db.with_snapshot(|s| s.items.get(&hub_item).is_some_and(|i| i.location_id == def_ship && i.flag == InventoryFlag::Cargo)),
        "rejected anchors must leave the hub in cargo"
    );

    // 2. Anchor from cargo in unclaimed null-sec: hub consumed, Onlining timer, no claim until onlining completes.
    slash(&ctx, &mut s_def, &format!("/tr {null_sys}")).await;
    slash(&ctx, &mut s_rival, &format!("/tr {null_sys}")).await;
    let _ = events(&ctx, PILOT_1);
    let rep = anchor_hub(&ctx, &mut s_def, hub_item).await.expect("Anchor Sovereignty Hub from cargo");
    let hub_id = rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID");
    assert_eq!(rep.dict_get("state").and_then(|v| v.as_str().map(str::to_string)).as_deref(), Some("Onlining"));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&hub_item)), "anchoring consumes the cargo item");
    advance_and_tick(&ctx, 600).await;
    assert_eq!(sov_holder(&ctx, null_sys), None, "no sovereignty before the onlining timer completes");

    // 3. Onlining completes -> alliance claim + OnSovereigntyChanged / OnSolarSystemSovStructuresUpdated broadcast.
    advance_and_tick(&ctx, 305).await;
    assert_eq!(sov_holder(&ctx, null_sys), Some(def_ally));
    let observed = events(&ctx, PILOT_1);
    assert!(
        observed.iter().any(|(e, a)| e == "OnSovereigntyChanged"
            && a.first().and_then(PyRep::as_i64) == Some(i64::from(null_sys))
            && a.get(1).and_then(PyRep::as_i64) == Some(def_ally)),
        "claim must broadcast OnSovereigntyChanged(sys, alliance): {:?}",
        observed.iter().map(|(e, _)| e).collect::<Vec<_>>()
    );
    assert!(observed.iter().any(|(e, a)| e == "OnSolarSystemSovStructuresUpdated" && a.first().and_then(PyRep::as_i64) == Some(i64::from(null_sys))));
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].owner_corp_id), def_corp);

    // 4. Negative: a rival alliance cannot anchor in the claimed system; the holder cannot anchor a second hub.
    let rival_item = give_item(&ctx, &mut s_rival, TYPE_SOVEREIGNTY_HUB, 1).await;
    assert_eq!(anchor_hub(&ctx, &mut s_rival, rival_item).await.unwrap_err().code, "SovereigntyHeldByAnotherAlliance");
    let second_item = give_item(&ctx, &mut s_def, TYPE_SOVEREIGNTY_HUB, 1).await;
    assert_eq!(anchor_hub(&ctx, &mut s_def, second_item).await.unwrap_err().code, "SovHubAlreadyPresent");
    assert_ne!(sov_holder(&ctx, null_sys), Some(rival_ally));
}

#[tokio::test]
async fn test_sov_adm_scaled_entosis_counter_regen_command_nodes_and_loss() {
    // parity: feat:sovereignty.sov-entosis
    // parity: feat:sovereignty.sov-command-nodes
    eve_testkit::attest(&[
        "feat:sovereignty.sov-entosis",
        "feat:sovereignty.sov-command-nodes",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut s_def = select_gm_pilot(&ctx, 75_001, "other", "other-secret", PILOT_2).await;
    let mut s_atk = select_gm_pilot(&ctx, 75_002, "demo", "demo-secret", PILOT_1).await;
    let (_, Some(def_ally)) = create_corp_and_alliance(&ctx, &mut s_def, "Home Guard", "HMGD", Some(("Home Guard Alliance", "HGAL"))).await
    else {
        unreachable!()
    };
    let _ = create_corp_and_alliance(&ctx, &mut s_atk, "Siege Works", "SGWK", Some(("Siege Works Coalition", "SGWC"))).await;
    let (def_ship, mut def_guns) = arm_ship_autocannons(&ctx, &mut s_def, PILOT_2).await;
    // The Rifter has 3 high slots: swap one autocannon back to the hangar for the defender's Entosis Link.
    let spare_gun = def_guns.pop().expect("third autocannon");
    let station = s_def.station_id.expect("docked defender");
    rpc(&ctx, &mut s_def, "invbroker", "Add", &[PyRep::Integer(spare_gun), PyRep::Integer(station), PyRep::Integer(InventoryFlag::Hangar as i64)])
        .await
        .expect("unfit autocannon");
    let def_link = fit_entosis_link(&ctx, &mut s_def, PILOT_2, InventoryFlag::HiSlot2).await;
    let atk_link = fit_entosis_link(&ctx, &mut s_atk, PILOT_1, InventoryFlag::HiSlot1).await;
    let atk_ship = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);

    // 1. Defender anchors + onlines a hub and sets a 12:00 UTC vulnerability window.
    let null_sys = claimable_null_system(&ctx);
    slash(&ctx, &mut s_def, &format!("/tr {null_sys}")).await;
    let hub_item = give_item(&ctx, &mut s_def, TYPE_SOVEREIGNTY_HUB, 1).await;
    let hub_id = anchor_hub(&ctx, &mut s_def, hub_item).await.unwrap().dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    advance_and_tick(&ctx, 905).await;
    assert_eq!(sov_holder(&ctx, null_sys), Some(def_ally));
    rpc(&ctx, &mut s_def, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(12), PyRep::Integer(3)])
        .await
        .expect("UpdateReinforceTimesFromClient");
    give_item(&ctx, &mut s_def, TYPE_STRONTIUM, 50).await;

    // 2. ADM from live indices: 7 days held (strategic 1) + NPC kills by live weapon fire (military 1) -> ADM 2.0.
    ctx.systems.get_or_spawn(null_sys).set_ball_position(def_ship, DVec3::new(500_000.0, 0.0, 0.0)).await;
    advance_and_tick(&ctx, 7 * 86_400 + 60).await;
    let npc_type = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 11 && t.default_attributes.get(&9).copied().is_some_and(|hp| hp > 10.0 && hp < 400.0))
        .map(|t| t.type_id)
        .min()
        .expect("SDE frigate NPC");
    for _ in 0..5 {
        let npc = slash(&ctx, &mut s_def, &format!("/spawnnpc {npc_type}")).await.dict_get("npcID").and_then(|v| v.as_i64()).unwrap();
        lock_and_destroy_target(&ctx, &mut s_def, null_sys, def_ship, &def_guns, npc).await;
    }
    let adm = ctx.db.with_snapshot(|s| {
        let st = &s.system_states[&null_sys];
        calculate_adm(st.military_index, st.industrial_index, st.strategic_index)
    });
    assert!((adm - 2.0).abs() < 1e-9, "live indices must yield ADM 2.0, got {adm}");
    let hub_required = ENTOSIS_HUB_BASE_CAPTURE_SECS * adm;

    // 3. Attacker links inside the window: 300s warm-up + 620s capture would reinforce at ADM 1.0 but not at ADM 2.0.
    let cur = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 12 * 3_600 - cur) % 86_400);
    slash(&ctx, &mut s_atk, &format!("/tr {null_sys}")).await;
    give_item(&ctx, &mut s_atk, TYPE_STRONTIUM, 200).await;
    lock_target(&ctx, &mut s_atk, null_sys, atk_ship, hub_id, 2_500.0).await;
    let first = entosis(&ctx, &mut s_atk, atk_link, hub_id).await.expect("attacker entosis on hub");
    assert_eq!(first.dict_get("isWarmupCycle").and_then(|v| v.as_bool()), Some(true));
    advance_and_tick(&ctx, 300 + 620).await;
    assert_eq!(hub_state(&ctx, hub_id), "ShieldVulnerable", "ADM 2.0 doubles the 600s hub capture time");
    let p0 = entosis_progress(&ctx, hub_id).expect("partial capture progress");
    assert!((p0 - 620.0).abs() < 1.0, "620s accrued after warm-up, got {p0}");

    // 4. Defender counter-entosis: both linking pauses progress; defender alone counts it back (unaffected by ADM).
    lock_target(&ctx, &mut s_def, null_sys, def_ship, hub_id, 3_000.0).await;
    entosis(&ctx, &mut s_def, def_link, hub_id).await.expect("hub-holding defender may counter-entosis a contested hub");
    advance_and_tick(&ctx, 300).await;
    assert!((entosis_progress(&ctx, hub_id).unwrap() - p0).abs() < 1.0, "both sides linking pauses capture");
    stop_module(&ctx, &mut s_atk, atk_link).await;
    advance_and_tick(&ctx, 200).await;
    let p1 = entosis_progress(&ctx, hub_id).unwrap();
    assert!((p1 - (p0 - 200.0)).abs() < 1.0, "defender-only link counts capture back 1:1, got {p1}");
    stop_module(&ctx, &mut s_def, def_link).await;

    // 5. Unattended partial capture regenerates; a full hub regenerates within 50 minutes.
    advance_and_tick(&ctx, 600).await;
    let p2 = entosis_progress(&ctx, hub_id).unwrap();
    assert!((p2 - (p1 - 600.0 * hub_required / 3_000.0)).abs() < 1.0, "passive regen at full-bar/50min, got {p2}");
    advance_and_tick(&ctx, 3_000).await;
    assert_eq!(entosis_progress(&ctx, hub_id), None, "partial capture fully regenerated within 50 minutes");

    // 6. Uncontested ADM-scaled capture reinforces the hub; nodes only appear after the 48h reinforcement timer.
    let _ = events(&ctx, PILOT_2);
    entosis(&ctx, &mut s_atk, atk_link, hub_id).await.expect("attacker re-links hub");
    advance_and_tick(&ctx, 300 + hub_required as u64 + 30).await;
    assert_eq!(hub_state(&ctx, hub_id), "HullReinforced");
    assert!(events(&ctx, PILOT_2).iter().any(|(e, _)| e == "OnSolarSystemSovStructuresUpdated"));
    assert!(hub_nodes(&ctx, hub_id).is_empty(), "no command nodes at reinforcement start");
    advance_and_tick(&ctx, 24 * 3_600).await;
    assert!(hub_nodes(&ctx, hub_id).is_empty(), "no command nodes 24h into reinforcement");
    advance_and_tick(&ctx, 24 * 3_600 + 60).await;
    assert_eq!(hub_nodes(&ctx, hub_id).len(), 4, "4 command nodes spawn once the 48h timer expires in-window");

    // 7. Attacker captures nodes; unattended nodes regenerate toward the defender; attacker win destroys the hub.
    let node_wait = 300 + (ENTOSIS_NODE_BASE_CAPTURE_SECS * 6.0) as u64 + 30;
    let score = |ctx: &ServerContext| ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores.get(&hub_id).map(|v| v[0].1));
    let mut seen: Vec<(String, Vec<PyRep>)> = events(&ctx, PILOT_2);
    let mut cur_sys = null_sys;
    for round in 0..16 {
        let Some(&(nid, nsys)) = hub_nodes(&ctx, hub_id).first() else { break };
        if nsys != cur_sys {
            slash(&ctx, &mut s_atk, &format!("/tr {nsys}")).await;
            cur_sys = nsys;
        }
        lock_target(&ctx, &mut s_atk, nsys, atk_ship, nid, 2_500.0).await;
        entosis(&ctx, &mut s_atk, atk_link, nid).await.expect("attacker entosis on command node");
        advance_and_tick(&ctx, node_wait).await;
        seen.extend(events(&ctx, PILOT_2));
        if round == 0 {
            let after_one = score(&ctx).expect("campaign score");
            assert!((after_one - 0.53).abs() < 1e-6, "one attacker node capture moves 0.60 -> 0.53, got {after_one}");
            advance_and_tick(&ctx, 1_200).await;
            let regen = score(&ctx).unwrap();
            assert!((regen - (after_one + 1_200.0 / 5_880.0)).abs() < 1e-3, "unattended nodes regen over 98 min, got {regen}");
        }
    }
    assert_eq!(hub_state(&ctx, hub_id), "Destroyed", "attacker node win destroys the hub");
    assert_eq!(sov_holder(&ctx, null_sys), None, "sovereignty is lost with the hub");
    assert!(
        seen.iter().any(|(e, a)| e == "OnSovereigntyChanged" && a.first().and_then(PyRep::as_i64) == Some(i64::from(null_sys)) && matches!(a.get(1), Some(PyRep::None))),
        "loss must broadcast OnSovereigntyChanged(sys, None)"
    );
    assert!(seen.iter().any(|(e, a)| e == "OnSolarSystemSovStructuresUpdated" && a.first().and_then(PyRep::as_i64) == Some(i64::from(null_sys))));
}

const CALDARI: i64 = 500_001;
const GALLENTE: i64 = 500_004;

fn fw_occupation(ctx: &ServerContext, sys_id: u32) -> (i64, i64, i64) {
    ctx.db.with_snapshot(|s| {
        let o = &s.fw_system_occupations[&sys_id];
        (o.occupier_faction_id, o.victory_points, o.victory_points_threshold)
    })
}

fn open_complexes(ctx: &ServerContext, sys_id: u32) -> Vec<(i64, String, DVec3)> {
    ctx.db.with_snapshot(|s| {
        let mut v: Vec<(i64, String, DVec3)> = s
            .fw_runtime
            .fw_complexes
            .iter()
            .filter(|(_, (csys, _, _, _, done))| *csys == sys_id && !*done)
            .filter_map(|(&g, (_, tier, _, _, _))| Some((g, tier.clone(), DVec3::from_array(*s.item_positions.get(&g)?))))
            .collect();
        // Highest-value complexes first (wiki VP table), then by gate ID for determinism.
        v.sort_by_key(|c| (-eve_server::services::fw_gameplay::complex_victory_points(&c.1), c.0));
        v
    })
}

fn open_complex(ctx: &ServerContext, sys_id: u32) -> Option<(i64, String, DVec3)> {
    open_complexes(ctx, sys_id).into_iter().min_by_key(|c| c.0)
}

fn complex_progress(ctx: &ServerContext, gate: i64) -> Option<f64> {
    ctx.db.with_snapshot(|s| s.fw_runtime.fw_complexes.get(&gate).map(|c| c.3))
}

async fn enter_complex(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship: i64, gate: i64, gate_pos: DVec3) {
    ctx.systems.get_or_spawn(sys_id).set_ball_position(ship, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    rpc(ctx, sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate)]).await.expect("ActivateAccelerationGate");
}

#[tokio::test]
async fn test_fw_live_plexing_to_vulnerable_ihub_kill_and_downtime_occupier_flip() {
    // parity: feat:fw.system-capture, notify:OnCaptureChanged
    // parity: notify:OnFacWarBunkerVulnerable, notify:OnVictoryPointStateUpdated
    eve_testkit::attest(&[
        "feat:fw.system-capture",
        "notify:OnCaptureChanged",
        "notify:OnFacWarBunkerVulnerable",
        "notify:OnVictoryPointStateUpdated",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut s_atk = select_gm_pilot(&ctx, 76_001, "demo", "demo-secret", PILOT_1).await;
    let _ = create_corp_and_alliance(&ctx, &mut s_atk, "State Siege Wing", "SSGW", None).await;
    rpc(&ctx, &mut s_atk, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(CALDARI)]).await.expect("JoinFactionAsCorporation");
    let (ship, guns) = arm_ship_autocannons(&ctx, &mut s_atk, PILOT_1).await;
    let target = ctx
        .db
        .with_snapshot(|snap| {
            snap.fw_system_occupations
                .values()
                .filter(|o| o.occupier_faction_id == GALLENTE && o.victory_points == 0)
                .map(|o| o.solar_system_id)
                .filter(|&s| eve_server::services::fw_services::compute_system_frontline_status(&ctx, snap, s) == eve_server::services::fw_services::STATUS_FRONTLINE)
                .min()
        })
        .expect("uncontested Gallente frontline system");
    slash(&ctx, &mut s_atk, &format!("/tr {target}")).await;
    tick_background_cluster(&ctx).await;
    let sys = ctx.systems.get_or_spawn(target);
    let ihub = ctx.db.with_snapshot(|s| {
        s.structures
            .values()
            .find(|st| st.solar_system_id == target && st.type_id == eve_server::services::fw_gameplay::TYPE_FW_IHUB && st.state != "Destroyed")
            .map(|st| st.structure_id)
            .expect("live FW Infrastructure Hub")
    });

    // 1. I-Hub is invulnerable to the attacking militia until the system is 100% contested.
    lock_target(&ctx, &mut s_atk, target, ship, ihub, 1_000.0).await;
    let err = rpc(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(guns[0]), PyRep::String("targetAttack".into()), PyRep::Integer(ihub)])
        .await
        .expect_err("I-Hub must be invulnerable before 100% contested");
    assert_eq!(err.code, "IHubInvulnerable");

    // 2. First complex: gate -> live guard blocks the timer -> guard killed in combat -> capture timer -> VP.
    let (gate, tier, gate_pos) = open_complex(&ctx, target).expect("spawned FW complex");
    let _ = events(&ctx, PILOT_1);
    enter_complex(&ctx, &mut s_atk, target, ship, gate, gate_pos).await;
    advance_and_tick(&ctx, 120).await;
    assert_eq!(complex_progress(&ctx, gate), Some(0.0), "capture timer must not run while the complex guard lives");
    lock_and_destroy_target(&ctx, &mut s_atk, target, ship, &guns, gate + 500_000_000).await;
    let hold = eve_server::services::fw_gameplay::complex_base_capture_duration_seconds(&tier);
    sys.set_ball_position(ship, eve_server::services::fw_gameplay::pocket_pos_for_complex(gate)).await;
    advance_and_tick(&ctx, hold / 2).await;
    let half = complex_progress(&ctx, gate).expect("complex still running");
    assert!(half > 0.0 && half < 100.0, "capture timer advances only after the guard dies, got {half}");
    advance_and_tick(&ctx, hold / 2 + 5).await;
    let (_, vp1, _threshold) = fw_occupation(&ctx, target);
    assert_eq!(vp1, eve_server::services::fw_gameplay::complex_victory_points(&tier), "frontline capture awards the tier's VP");
    let first = events(&ctx, PILOT_1);
    assert!(first.iter().any(|(e, a)| e == "OnCaptureChanged" && a.first().and_then(PyRep::as_i64) == Some(gate) && a.get(1).and_then(PyRep::as_i64) == Some(CALDARI)));
    assert!(first.iter().any(|(e, a)| e == "OnVictoryPointStateUpdated"
        && a.get(1).and_then(|d| d.dict_get("victoryPoints")).and_then(|v| v.as_i64()) == Some(vp1)));
    assert!(!first.iter().any(|(e, _)| e == "OnFacWarBunkerVulnerable"), "not vulnerable below 100% contested");

    // 3. Advance victory points to near threshold, then capture the final complex via authentic live
    // weapon fire on the guard: the authentic complex completion crosses 100% contested,
    // triggering OnFacWarBunkerVulnerable and unlocking the I-Hub for attack.
    let (_, cur_vp, threshold) = fw_occupation(&ctx, target);
    let (final_gate, final_tier, final_gate_pos) = open_complex(&ctx, target).expect("spawned final FW complex");
    let final_vp = eve_server::services::fw_gameplay::complex_victory_points(&final_tier);
    let vp_needed = (threshold - cur_vp).saturating_sub(final_vp);
    if vp_needed > 0 {
        slash(&ctx, &mut s_atk, &format!("/fwcapture {target} {vp_needed}")).await;
    }
    let _ = events(&ctx, PILOT_1);
    enter_complex(&ctx, &mut s_atk, target, ship, final_gate, final_gate_pos).await;
    lock_and_destroy_target(&ctx, &mut s_atk, target, ship, &guns, final_gate + 500_000_000).await;
    let final_hold = eve_server::services::fw_gameplay::complex_base_capture_duration_seconds(&final_tier);
    sys.set_ball_position(ship, eve_server::services::fw_gameplay::pocket_pos_for_complex(final_gate)).await;
    advance_and_tick(&ctx, final_hold + 5).await;

    let (_, vp_final, _) = fw_occupation(&ctx, target);
    assert!(vp_final >= threshold, "system must reach 100% contested (vp={vp_final}, thresh={threshold})");
    let bunker_vulnerable = events(&ctx, PILOT_1).iter().any(|(e, a)| {
        e == "OnFacWarBunkerVulnerable" && a.first().and_then(PyRep::as_i64) == Some(GALLENTE)
    });
    assert!(bunker_vulnerable, "reaching 100% contested must emit OnFacWarBunkerVulnerable with occupier factionID");
    assert_eq!(fw_occupation(&ctx, target).0, GALLENTE, "100% contested alone does not flip occupancy");

    // 4. Destroy the now-vulnerable I-Hub with live weapons fire; occupancy is only queued for downtime.
    for &g in &guns {
        slash(&ctx, &mut s_atk, &format!("/setattr {g} 64 250.0")).await;
    }
    lock_target(&ctx, &mut s_atk, target, ship, ihub, 1_000.0).await;
    for _ in 0..100 {
        if ctx.db.with_snapshot(|s| s.structures[&ihub].state == "Destroyed") {
            break;
        }
        for &g in &guns {
            let _ = rpc(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(g), PyRep::String("targetAttack".into()), PyRep::Integer(ihub)]).await;
        }
        for _ in 0..3 {
            sys.step_manual(1.0).await.unwrap();
        }
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&ihub].state.clone()), "Destroyed", "I-Hub destroyed by weapons fire");
    assert_eq!(fw_occupation(&ctx, target).0, GALLENTE, "occupier unchanged until the next downtime");
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_runtime.pending_downtime_captures.get(&target).copied()), Some(CALDARI));

    // 5. Daily downtime flips the system to the attacker and resets contested state.
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    let until_dt = (86_400 + dt_sec - ctx.now_unix() % 86_400) % 86_400;
    if until_dt > 10 {
        ctx.advance_secs(until_dt - 10);
        assert_eq!(fw_occupation(&ctx, target).0, GALLENTE, "still Gallente just before downtime");
    }
    let _ = events(&ctx, PILOT_1);
    ctx.advance_secs(20);
    let (occ, vp_after, _) = fw_occupation(&ctx, target);
    assert_eq!((occ, vp_after), (CALDARI, 0), "occupier flips to the attacker at downtime");
    assert!(events(&ctx, PILOT_1).iter().any(|(e, _)| e == "OnWarzoneOccupationStateUpdated"));
}
