//! Integration tests for FW LP rewards from complexes & PvP kills scaled by Uprising system status
//! (`1.5x` Frontline / `1.0x` Command / `0.01x` Rearguard), defensive vs offensive plexing,
//! and real Tranquility militia LP store redemption (`feat:fw.fw-lp`).

#![forbid(unsafe_code)]

use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

use super::common;
use super::features_sovereignty_upwell_rpcs::{rpc, setup_pilot};

const PILOT_1: i64 = 90_000_001;
const STATE_PROTECTORATE_CORP: i64 = 1_000_180;

fn fit_combat_frigate(ctx: &ServerContext, char_id: i64, ship_id: i64, base_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 587; // Rifter (T1 Frigate allowed in Novice FW complexes)
        }
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3300u32, 3303, 3306, 3310, 3311, 3312, 3413, 3426, 3456] {
            sk.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        for (i, flg) in [
            eve_db::InventoryFlag::HiSlot0,
            eve_db::InventoryFlag::HiSlot1,
            eve_db::InventoryFlag::HiSlot2,
        ]
        .into_iter()
        .enumerate()
        {
            let gid = base_id + (i as i64) * 2;
            snap.items.insert(
                gid,
                eve_db::InventoryItemRecord {
                    item_id: gid,
                    type_id: 484, // 125mm Gatling AutoCannon I
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                gid + 1,
                eve_db::InventoryItemRecord {
                    item_id: gid + 1,
                    type_id: 185, // EMP S
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 2_000,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gid, true);
            snap.module_active_charges.insert(gid, 185);
        }
        for (i, flg) in [
            eve_db::InventoryFlag::LoSlot0,
            eve_db::InventoryFlag::LoSlot1,
            eve_db::InventoryFlag::LoSlot2,
        ]
        .into_iter()
        .enumerate()
        {
            let mid = base_id + 20 + (i as i64);
            snap.items.insert(
                mid,
                eve_db::InventoryItemRecord {
                    item_id: mid,
                    type_id: 519, // Gyrostabilizer II
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
}

async fn kill_target_live(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    gun_base_id: i64,
    target_id: i64,
) {
    crate::sovereignty_equinox_gameplay::lock_target_via_rpc(
        ctx, sess, sys_id, ship_id, target_id,
    )
    .await;
    for _ in 0..40 {
        if ctx
            .systems
            .get_or_spawn(sys_id)
            .get_ball_snapshot(target_id)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            return;
        }
        for i in 0..3 {
            let _ = rpc(
                ctx,
                sess,
                "dogmaIM",
                "Activate",
                &[
                    PyRep::Integer(gun_base_id + i * 2),
                    PyRep::String("targetAttack".into()),
                    PyRep::Integer(target_id),
                ],
            )
            .await;
        }
        crate::sovereignty_equinox_gameplay::step_weapon_ticks(ctx, sys_id).await;
        ctx.advance_secs(5);
    }
    assert!(
        ctx.systems
            .get_or_spawn(sys_id)
            .get_ball_snapshot(target_id)
            .await
            .ok()
            .flatten()
            .is_none(),
        "target {target_id} in system {sys_id} must be destroyed via live dogmaIM.Activate ticks"
    );
}

async fn capture_novice_complex_live(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    gun_base_id: i64,
    sys_id: u32,
) -> i64 {
    let lp_before = ctx.db.with_snapshot(|s| {
        s.characters[&char_id]
            .loyalty_points
            .get(&STATE_PROTECTORATE_CORP)
            .copied()
            .unwrap_or(0)
    });
    crate::sovereignty_equinox_gameplay::set_pilot_location(ctx, sess, sys_id, None);
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().structure_id = None;
        s.items.get_mut(&ship_id).unwrap().location_id = i64::from(sys_id);
    });
    crate::sovereignty_equinox_gameplay::place_ship_ball(
        ctx,
        sys_id,
        char_id,
        ship_id,
        587,
        DVec3::new(500_000.0, 0.0, 0.0),
    )
    .await;
    eve_server::sp6_lifecycle::tick_background_cluster(ctx).await;

    let (novice_gate, gate_pos) = ctx.db.with_snapshot(|s| {
        let (&gid, _) = s
            .fw_runtime
            .fw_complexes
            .iter()
            .find(|(_, (csys, tier, _, _, done))| *csys == sys_id && tier == "Novice" && !*done)
            .expect("spawned Novice FW complex");
        let pos = s.item_positions.get(&gid).copied().unwrap_or([0.0, 0.0, 0.0]);
        (gid, DVec3::new(pos[0], pos[1], pos[2]))
    });
    crate::sovereignty_equinox_gameplay::place_ship_ball(
        ctx,
        sys_id,
        char_id,
        ship_id,
        587,
        gate_pos + DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    rpc(
        ctx,
        sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(novice_gate)],
    )
    .await
    .expect("ActivateAccelerationGate into Novice pocket");

    let occ_fac = ctx
        .db
        .with_snapshot(|s| s.fw_runtime.fw_complexes.get(&novice_gate).map(|c| c.2));
    if occ_fac != sess.war_faction_id {
        let guard_id = novice_gate + 500_000_000;
        kill_target_live(ctx, sess, sys_id, ship_id, gun_base_id, guard_id).await;
    }

    eve_server::sp6_lifecycle::tick_background_cluster(ctx).await;
    ctx.advance_secs(900);
    eve_server::sp6_lifecycle::tick_background_cluster(ctx).await;
    assert!(
        ctx.db.with_snapshot(|s| {
            s.fw_runtime.fw_complexes.get(&novice_gate).is_none_or(|c| c.4)
                && s.fw_runtime
                    .complex_tier_cooldowns
                    .contains_key(&format!("{sys_id}:Novice"))
        }),
        "Novice FW complex {novice_gate} in system {sys_id} must be marked completed"
    );

    let lp_after = ctx.db.with_snapshot(|s| {
        s.characters[&char_id]
            .loyalty_points
            .get(&STATE_PROTECTORATE_CORP)
            .copied()
            .unwrap_or(0)
    });
    lp_after - lp_before
}

#[tokio::test]
async fn test_fw_lp_complexes_kills_status_scaling_and_militia_store() {
    // parity: feat:fw.fw-lp
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (enemy_sys, front_sys, cmd_sys, rear_sys) =
        crate::fw_warzone_insurgency::setup_warzone_chain(&ctx);

    let mut sess_cal = setup_pilot(&ctx, PILOT_1).await;
    let cal_ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_1).expect("PILOT_1");
        ch.war_faction_id = Some(500_001); // Caldari militia -> State Protectorate (1000180)
        ch.loyalty_points.remove(&STATE_PROTECTORATE_CORP);
        ch.balance = IskCents::from_isk(50_000_000);
        snap.fw_runtime.direct_enlisted_chars.insert(PILOT_1, 500_001);
        ch.active_ship_id
    });
    sess_cal.war_faction_id = Some(500_001);
    let cal_guns = 995_100i64;
    fit_combat_frigate(&ctx, PILOT_1, cal_ship_id, cal_guns);
    let _ = rpc(&ctx, &mut sess_cal, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await;

    // 1. Defensive plexing in friendly Caldari-occupied systems:
    //    - 0% contested ("stable") Frontline system pays 0 LP!
    //    - Contested Command system (1.0x) pays full status-scaled offensive LP = 10,000 LP!
    let def_stable_lp =
        capture_novice_complex_live(&ctx, &mut sess_cal, PILOT_1, cal_ship_id, cal_guns, front_sys)
            .await;
    assert_eq!(def_stable_lp, 0, "defensive plexing in a 0% contested system must pay 0 LP");

    eve_server::services::fw_services::add_victory_points(&ctx, cmd_sys, 1_500); // 1,500 / 3,000 = 50% (Contested)
    let def_contested_lp =
        capture_novice_complex_live(&ctx, &mut sess_cal, PILOT_1, cal_ship_id, cal_guns, cmd_sys)
            .await;
    assert_eq!(
        def_contested_lp, 10_000,
        "defensive plexing in Contested Command system must pay full status-scaled offensive LP = 10,000 LP"
    );

    // 2. Live PvP ship + empty Capsule destruction in Frontline system (`front_sys`, 1.5x, 0 VP):
    //    - 2 Caldari militia pilots (`PILOT_1` + wingman `90_000_003`) engage Gallente pilot `90_000_002` in a fitted Tristan (593).
    //    - Verify wiki `3.25 * (hull - insurance + fit + cargo) / 10,000` LP split 50/50, 0 VP pushed on defender kill,
    //      and 0 LP / 0 VP paid when destroying the ejected empty Capsule (670)!
    let gal_cid = 90_000_002i64;
    let gal_ship = 100_995_002i64;
    let wing_cid = 90_000_003i64;
    let wing_ship = 100_995_003i64;
    let mut sess_gal = crate::sovereignty_equinox_gameplay::register_online_pilot(
        &ctx, 39_102, "gal_fw", "gal-secret", gal_cid, 98_000_102, None,
        front_sys, gal_ship, 593, DVec3::new(50_000.0, 0.0, 0.0), 0,
    ).await;
    let mut sess_wing = crate::sovereignty_equinox_gameplay::register_online_pilot(
        &ctx, 39_103, "wing_fw", "wing-secret", wing_cid, 98_000_103, None,
        front_sys, wing_ship, 587, DVec3::new(50_800.0, 0.0, 0.0), 0,
    ).await;
    sess_gal.war_faction_id = Some(500_004);
    sess_wing.war_faction_id = Some(500_001);
    let _ = rpc(&ctx, &mut sess_wing, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await;
    let wing_guns = 995_200i64;
    fit_combat_frigate(&ctx, wing_cid, wing_ship, wing_guns);

    let custom_ins_payout = 100_000.0;
    ctx.db.with_mut_snapshot(|snap| {
        let gch = snap.characters.get_mut(&gal_cid).unwrap();
        gch.war_faction_id = Some(500_004);
        snap.fw_runtime.direct_enlisted_chars.insert(gal_cid, 500_004);
        let wch = snap.characters.get_mut(&wing_cid).unwrap();
        wch.war_faction_id = Some(500_001);
        wch.loyalty_points.remove(&STATE_PROTECTORATE_CORP);
        snap.fw_runtime.direct_enlisted_chars.insert(wing_cid, 500_001);
        if let Some(sh) = snap.items.get_mut(&gal_ship) {
            sh.insured_payout_isk = custom_ins_payout;
        }
        snap.items.insert(
            995_301,
            eve_db::InventoryItemRecord {
                item_id: 995_301,
                type_id: 2_048, // Damage Control II (fitted)
                owner_id: gal_cid,
                location_id: gal_ship,
                flag: eve_db::InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            995_302,
            eve_db::InventoryItemRecord {
                item_id: 995_302,
                type_id: 25_887, // Datacore - Caldari Starship Engineering x20 in cargo
                owner_id: gal_cid,
                location_id: gal_ship,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 20,
                ..Default::default()
            },
        );
    });
    let expected_net_isk = ctx.sde.types.get(&593).map(|t| t.base_price.max(0.0)).unwrap_or(0.0)
        + ctx.sde.types.get(&2_048).map(|t| t.base_price.max(0.0)).unwrap_or(0.0)
        + ctx.sde.types.get(&25_887).map(|t| t.base_price.max(0.0)).unwrap_or(0.0) * 20.0
        - custom_ins_payout;
    let expected_base_kill_lp =
        eve_server::services::fw_services::compute_fw_kill_base_lp(expected_net_isk);
    let expected_status_kill_lp = eve_server::services::fw_services::calculate_fw_lp_reward(
        expected_base_kill_lp,
        eve_server::services::fw_services::STATUS_FRONTLINE,
    );
    let expected_each_share = ((expected_status_kill_lp as f64) / 2.0).round() as i64;
    assert!(expected_each_share > 0, "fitted Tristan + cargo net loss must yield positive FW LP");

    crate::sovereignty_equinox_gameplay::set_pilot_location(&ctx, &mut sess_cal, front_sys, None);
    crate::sovereignty_equinox_gameplay::place_ship_ball(
        &ctx, front_sys, PILOT_1, cal_ship_id, 587, DVec3::new(50_600.0, 0.0, 0.0),
    ).await;
    let p1_lp_pre_kill = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].loyalty_points.get(&STATE_PROTECTORATE_CORP).copied().unwrap_or(0));

    // Wingman lands a single turret hit, then PILOT_1 destroys the enemy Tristan via live combat ticks
    crate::sovereignty_equinox_gameplay::lock_target_via_rpc(&ctx, &mut sess_wing, front_sys, wing_ship, gal_ship).await;
    rpc(&ctx, &mut sess_wing, "dogmaIM", "Activate", &[
        PyRep::Integer(wing_guns), PyRep::String("targetAttack".into()), PyRep::Integer(gal_ship),
    ]).await.expect("wingman turret activate");
    ctx.systems.get_or_spawn(front_sys).step_manual(1.0).await.unwrap();
    let _ = rpc(&ctx, &mut sess_wing, "dogmaIM", "Deactivate", &[
        PyRep::Integer(wing_guns), PyRep::String("targetAttack".into()),
    ]).await;

    kill_target_live(&ctx, &mut sess_cal, front_sys, cal_ship_id, cal_guns, gal_ship).await;

    let (p1_kill_gain, wing_kill_gain, front_vp_after_kill, pod_id) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&PILOT_1].loyalty_points.get(&STATE_PROTECTORATE_CORP).copied().unwrap_or(0) - p1_lp_pre_kill,
            s.characters[&wing_cid].loyalty_points.get(&STATE_PROTECTORATE_CORP).copied().unwrap_or(0),
            s.fw_system_occupations[&front_sys].victory_points,
            s.characters[&gal_cid].active_ship_id,
        )
    });
    assert_eq!((p1_kill_gain, wing_kill_gain), (expected_each_share, expected_each_share), "PvP kill LP must match 3.25/10000 net-loss formula split equally across both militia attackers");
    assert_eq!(front_vp_after_kill, 0, "defender PvP kill in own system must NOT push attacker contested VP");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&pod_id].type_id), 670, "destroyed enemy pilot ejected into Capsule 670");

    // Destroying the ejected empty Capsule (670) via live combat pays 0 LP and 0 VP!
    let p1_lp_pre_pod = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].loyalty_points.get(&STATE_PROTECTORATE_CORP).copied().unwrap_or(0));
    kill_target_live(&ctx, &mut sess_cal, front_sys, cal_ship_id, cal_guns, pod_id).await;
    let (p1_pod_gain, front_vp_after_pod) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&PILOT_1].loyalty_points.get(&STATE_PROTECTORATE_CORP).copied().unwrap_or(0) - p1_lp_pre_pod,
            s.fw_system_occupations[&front_sys].victory_points,
        )
    });
    assert_eq!((p1_pod_gain, front_vp_after_pod), (0, 0), "empty Capsule (670) kill must award 0 LP and 0 VP");

    // 3. Offensive Novice complex captures across enemy-occupied Frontline (1.5x -> 15,000), Command (1.0x -> 10,000), Rearguard (0.01x -> 100)
    let _ = ctx.systems.get_or_spawn(front_sys).remove_ball(wing_ship).await;
    ctx.advance_secs(1_900); // expire 30-min complex respawn cooldown
    ctx.db.with_mut_snapshot(|s| {
        s.fw_runtime.fw_complexes.clear();
        s.fw_runtime.complex_guards_cleared.clear();
        s.fw_runtime.battlefield_instances.clear();
        s.fw_runtime.system_advantage.clear();
        for (&sys, o) in s.fw_system_occupations.iter_mut() {
            let fac = if sys == enemy_sys { 500_001 } else { 500_004 };
            o.owner_faction_id = fac;
            o.occupier_faction_id = fac;
            o.victory_points = 0;
        }
    });
    let mut offensive_payouts = Vec::new();
    for sys_id in [front_sys, cmd_sys, rear_sys] {
        offensive_payouts.push(
            capture_novice_complex_live(&ctx, &mut sess_cal, PILOT_1, cal_ship_id, cal_guns, sys_id).await,
        );
    }
    assert_eq!(
        offensive_payouts,
        vec![15_000, 10_000, 100],
        "Offensive Novice complex LP must scale 1.5x Frontline (15000), 1.0x Command (10000), 0.01x Rearguard (100)"
    );

    // 4. Spend earned militia LP in real Tranquility State Protectorate (1000180) LP store (NO synthetic seeding!)
    let militia_station = ctx
        .sde
        .stations
        .values()
        .find(|st| st.owner_corporation_id == STATE_PROTECTORATE_CORP)
        .map(|st| st.station_id)
        .expect("State Protectorate SDE station");
    sess_cal.station_id = Some(militia_station);
    sess_cal.structure_id = None;
    sess_gal.station_id = Some(militia_station);
    sess_gal.structure_id = None;
    sess_gal.corp_role = u64::MAX;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_1).unwrap().station_id = Some(militia_station);
        let gch = s.characters.get_mut(&gal_cid).unwrap();
        gch.station_id = Some(militia_station);
        gch.balance = IskCents::from_isk(50_000_000);
        gch.standings.clear(); // 0.0 default raw standing — opposing militia gate must still reject!
        gch.loyalty_points.insert(STATE_PROTECTORATE_CORP, 50_000);
        if let Some(gcorp) = s.corporations.get_mut(&98_000_102) {
            gcorp.master_wallet = IskCents::from_isk(50_000_000);
            if let Some(d0) = gcorp.wallet_divisions.first_mut() {
                d0.balance = gcorp.master_wallet;
            }
        }
    });

    let offers_rep = rpc(
        &ctx,
        &mut sess_cal,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(STATE_PROTECTORATE_CORP)],
    )
    .await
    .expect("GetAvailableOffersFromCorp real militia catalog");
    let PyRep::List(ref offers) = offers_rep else {
        panic!("expected List from militia LP store");
    };
    assert!(offers.len() >= 300, "real ESI State Protectorate LP catalog must expose >=300 offers, got {}", offers.len());
    let chosen_oid = offers
        .iter()
        .find_map(|o| {
            let oid = o.dict_get("offerID")?.as_i64()?;
            let lpc = o.dict_get("lpCost")?.as_i64()?;
            let akc = o.dict_get("akCost").and_then(|v| v.as_i64()).unwrap_or(0);
            let req_empty = match o.dict_get("reqItems") {
                Some(PyRep::List(l)) => l.is_empty(),
                Some(PyRep::Rowset { rows, .. }) => rows.is_empty(),
                _ => false,
            };
            (lpc > 0 && lpc <= 1_000 && akc == 0 && req_empty).then_some(oid)
        })
        .expect("real no-tag State Protectorate offer in ESI catalog");

    // Negative check 1: opposing Gallente militia pilot (`500_004`) with 0.0 raw standing is rejected on both character & corp purchases
    for method in ["TakeOfferForCharacter", "TakeOfferForCorporation"] {
        let opp_err = rpc(
            &ctx,
            &mut sess_gal,
            "LPStoreMgr",
            method,
            &[
                PyRep::Integer(STATE_PROTECTORATE_CORP),
                PyRep::Integer(chosen_oid),
                PyRep::Integer(1),
            ],
        )
        .await
        .expect_err("opposing militia pilot must be rejected from enemy militia LP store");
        assert_eq!(opp_err.code, "InsufficientStanding");
    }

    // Negative check 2: excessive quantity exceeding earned LP fails
    assert!(
        rpc(
            &ctx,
            &mut sess_cal,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(STATE_PROTECTORATE_CORP),
                PyRep::Integer(chosen_oid),
                PyRep::Integer(1_000_000),
            ],
        )
        .await
        .is_err(),
        "spending more militia LP than earned must fail"
    );

    let total_earned_lp = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].loyalty_points[&STATE_PROTECTORATE_CORP]);
    let isk_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance);
    rpc(
        &ctx,
        &mut sess_cal,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(STATE_PROTECTORATE_CORP),
            PyRep::Integer(chosen_oid),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("TakeOfferForCharacter spending earned FW militia LP on real ESI offer");

    let (lp_remaining, isk_after) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&PILOT_1];
        (ch.loyalty_points[&STATE_PROTECTORATE_CORP], ch.balance)
    });
    assert!(lp_remaining < total_earned_lp, "militia LP must be deducted");
    assert!(isk_after < isk_before, "militia offer ISK cost must be deducted");

    // Verify SQLite persistence of remaining militia LP balance
    let ctx_re =
        crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b7_fw_lp_store");
    let reloaded_lp = ctx_re.db.with_snapshot(|s| {
        s.characters[&PILOT_1]
            .loyalty_points
            .get(&STATE_PROTECTORATE_CORP)
            .copied()
            .unwrap_or(0)
    });
    assert_eq!(reloaded_lp, lp_remaining);
}

#[tokio::test]
async fn test_fw_offensive_lp_zero_when_vulnerable() {
    // parity: feat:fw.fw-lp
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (enemy_sys, _front_sys, _cmd_sys, _rear_sys) =
        crate::fw_warzone_insurgency::setup_warzone_chain(&ctx);

    let mut sess_cal = setup_pilot(&ctx, PILOT_1).await;
    let cal_ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_1).expect("PILOT_1");
        ch.war_faction_id = Some(500_001); // Caldari militia -> State Protectorate (1000180)
        ch.loyalty_points.remove(&STATE_PROTECTORATE_CORP);
        ch.balance = IskCents::from_isk(50_000_000);
        snap.fw_runtime.direct_enlisted_chars.insert(PILOT_1, 500_001);
        let occ = snap
            .fw_system_occupations
            .get_mut(&enemy_sys)
            .expect("enemy_sys occupation");
        occ.victory_points = occ.victory_points_threshold;
        snap.fw_runtime
            .system_victory_points
            .insert(enemy_sys, occ.victory_points_threshold);
        ch.active_ship_id
    });
    sess_cal.war_faction_id = Some(500_001);
    let cal_guns = 995_100i64;
    fit_combat_frigate(&ctx, PILOT_1, cal_ship_id, cal_guns);
    let _ = rpc(&ctx, &mut sess_cal, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await;

    let off_vuln_lp =
        capture_novice_complex_live(&ctx, &mut sess_cal, PILOT_1, cal_ship_id, cal_guns, enemy_sys)
            .await;
    assert_eq!(
        off_vuln_lp, 0,
        "offensive plexing in a Vulnerable system (victory_points >= threshold) must pay 0 LP"
    );
}
