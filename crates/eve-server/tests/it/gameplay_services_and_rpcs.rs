//! Shared helpers for open `feat:` and `rpc:` parity tests (Batch 7).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

use crate::common;

const STATION_ID: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

pub(crate) async fn setup_pilot_for_user(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    let uid = sess.user_id.unwrap_or(1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.station_id = Some(STATION_ID);
            ch.structure_id = None;
            ch.solar_system_id = JITA_SYS;
            ch.home_station_id = STATION_ID;
        }
    });
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess.station_id = Some(STATION_ID);
    sess.structure_id = None;
    sess.solar_system_id = Some(JITA_SYS);
    eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
    sess
}

pub(crate) async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    setup_pilot_for_user(ctx, char_id as u64, "demo", "demo-secret", char_id).await
}

pub(crate) mod fw_lp_complexes_kills_status {
    // Integration tests for FW LP rewards from complexes & PvP kills scaled by Uprising system status
    // (`1.5x` Frontline / `1.0x` Command / `0.01x` Rearguard), defensive vs offensive plexing,
    // and real Tranquility militia LP store redemption (`feat:fw.fw-lp`).


    use eve_db::IskCents;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;

    use crate::common;
    use crate::gameplay_services_and_rpcs::{rpc, setup_pilot};

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
        for round in 0..24 {
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
            if round == 0 || target_id < 500_000_000 {
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
            }
            if target_id >= 500_000_000 {
                crate::sovereignty_equinox_gameplay::step_weapon_ticks(ctx, sys_id).await;
            }
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
        eve_server::session_cluster_lifecycle::tick_background_cluster(ctx).await;

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

        eve_server::session_cluster_lifecycle::tick_background_cluster(ctx).await;
        ctx.advance_secs(900);
        eve_server::session_cluster_lifecycle::tick_background_cluster(ctx).await;
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
            crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite(&ctx, "b7_fw_lp_store");
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
}

pub(crate) mod cosmetics_skin_apply_in_space {
    // Integration tests for open `feat:` parity rows (Batch 7, Part 2).
    //
    // Covers:
    // - Ship SKIN application in space with observer slim/cosmetic updates & SQLite persistence (`feat:cosmetics.skin-apply`)
    // - Structure directory listing accessible structures with online services & ACL gating (`feat:structures.structure-directory`)


    use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;

    use crate::gameplay_services_and_rpcs::{rpc, setup_pilot, setup_pilot_for_user};

    const PILOT_1: i64 = 90_000_001;
    const PILOT_2: i64 = 90_000_002;
    const STATION_ID: i64 = 60_003_760;
    const JITA_SYS: u32 = 30_000_142;

    #[tokio::test]
    async fn test_cosmetics_skin_apply_in_space_broadcast_and_persistence() {
        // parity: feat:cosmetics.skin-apply
        let ctx = ServerContext::new();
        let mut sess1 = setup_pilot(&ctx, PILOT_1).await;
        let _sess2 = setup_pilot_for_user(&ctx, 77_002, "other", "other-secret", PILOT_2).await;

        let active_ship_id = ctx.db.with_mut_snapshot(|snap| {
            let ship_id = snap.characters[&PILOT_1].active_ship_id;
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.type_id = 587; // Rifter
            }
            ship_id
        });

        // Find a real SDE permanent SKIN license item for Rifter (type 587)
        let (license_type_id, resolved_skin_id) = ctx
            .sde
            .skin_licenses
            .iter()
            .find_map(|(&lic_tid, lic)| {
                let skin = ctx.sde.skins.get(&lic.skin_id)?;
                let ty = ctx.sde.types.get(&lic_tid)?;
                if ty.category_id == 91 && skin.types.contains(&587) {
                    Some((lic_tid, lic.skin_id))
                } else {
                    None
                }
            })
            .expect("SDE Rifter SKIN license");

        // Find a valid SDE SKIN for another hull (not Rifter) to test hull-mismatch rejection
        let wrong_hull_skin_id = ctx
            .sde
            .skins
            .values()
            .find(|sk| !sk.types.is_empty() && !sk.types.contains(&587))
            .expect("non-Rifter SDE skin")
            .skin_id;

        // Negative: unlicensed SKIN rejected before license consumption
        let unlic_err = rpc(
            &ctx,
            &mut sess1,
            "shipCosmeticsMgr",
            "ApplySkinToShip",
            &[
                PyRep::Integer(active_ship_id),
                PyRep::Integer(i64::from(resolved_skin_id)),
            ],
        )
        .await
        .expect_err("unlicensed SKIN must fail");
        assert_eq!(unlic_err.code, "SkinNotLicensed");

        // Negative: wrong hull SKIN rejected
        let hull_err = rpc(
            &ctx,
            &mut sess1,
            "shipCosmeticsMgr",
            "ApplySkinToShip",
            &[
                PyRep::Integer(active_ship_id),
                PyRep::Integer(i64::from(wrong_hull_skin_id)),
            ],
        )
        .await
        .expect_err("incompatible hull SKIN must fail");
        assert_eq!(hull_err.code, "InvalidShipTypeForSkin");

        // Seed real SKIN license item in hangar and consume via ActivateSkinLicense
        let lic_item_id = 990_701_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                lic_item_id,
                InventoryItemRecord {
                    item_id: lic_item_id,
                    type_id: license_type_id,
                    owner_id: PILOT_1,
                    location_id: STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: Default::default(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
        rpc(
            &ctx,
            &mut sess1,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(i64::from(license_type_id))],
        )
        .await
        .expect("ActivateSkinLicense");
        assert!(
            ctx.db.with_snapshot(|s| !s.items.contains_key(&lic_item_id)),
            "consumed SKIN license item must be removed from hangar"
        );

        // Put both PILOT_1 (in active Rifter) and observer PILOT_2 into space in Jita
        ctx.db.with_mut_snapshot(|snap| {
            for cid in [PILOT_1, PILOT_2] {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.solar_system_id = JITA_SYS;
                    ch.station_id = None;
                    ch.structure_id = None;
                }
            }
        });
        sess1.station_id = None;
        let sys_h = ctx.systems.get_or_spawn(JITA_SYS);
        let _ = sys_h
            .spawn_ball(crate::sovereignty_equinox_gameplay::make_ball(
                active_ship_id,
                587,
                DVec3::new(10_000.0, 0.0, 0.0),
            ))
            .await;

        let _ = ctx.drain_notifications_for_char(PILOT_1);
        let _ = ctx.drain_notifications_for_char(PILOT_2);

        // Apply licensed SKIN to active ship in space
        let apply_rep = rpc(
            &ctx,
            &mut sess1,
            "shipCosmeticsMgr",
            "ApplySkinToShip",
            &[
                PyRep::Integer(active_ship_id),
                PyRep::Integer(i64::from(license_type_id)),
            ],
        )
        .await
        .expect("ApplySkinToShip");
        assert_eq!(
            apply_rep.dict_get("shipID"),
            Some(&PyRep::Integer(active_ship_id))
        );
        assert_eq!(
            apply_rep.dict_get("skinID"),
            Some(&PyRep::Integer(i64::from(resolved_skin_id)))
        );

        // Observer PILOT_2 already in the solar system receives slim item update + cosmetic change
        let observer_notifs = ctx.drain_notifications_for_char(PILOT_2);
        let slim_notif = observer_notifs
            .iter()
            .find(|n| n.event == "OnSlimItemChange")
            .expect("observer PILOT_2 must receive OnSlimItemChange");
        assert_eq!(
            slim_notif
                .args
                .get(1)
                .and_then(|s| s.dict_get("skinID"))
                .and_then(|v| v.as_i64()),
            Some(i64::from(resolved_skin_id)),
            "newSlim.skinID must carry the applied SDE skinID"
        );
        assert!(
            observer_notifs
                .iter()
                .any(|n| n.event == "OnShipCosmeticsChanged"),
            "observer PILOT_2 must receive OnShipCosmeticsChanged"
        );

        // Late-arriving grid observer syncing Ballpark state via beyonce.UpdateStateRequest sees skinID in SetState slims
        let late_observer_cid = 90_000_003i64;
        let late_ship_id = 100_999_003i64;
        let mut sess_late = crate::sovereignty_equinox_gameplay::register_online_pilot(
            &ctx,
            39_903,
            "demo3",
            "demo3-secret",
            late_observer_cid,
            98_000_093,
            None,
            JITA_SYS,
            late_ship_id,
            587,
            DVec3::new(10_500.0, 0.0, 0.0),
            0,
        )
        .await;
        let _ = ctx.drain_notifications_for_char(late_observer_cid);
        let beyonce_oid = ctx
            .bind_object(&mut sess_late, "beyonce", i64::from(JITA_SYS))
            .expect("bind beyonce");
        ctx.dispatch_rpc(
            &mut sess_late,
            "beyonce",
            Some(beyonce_oid),
            "UpdateStateRequest",
            &[],
            &std::collections::HashMap::new(),
            None,
        )
        .await
        .expect("beyonce.UpdateStateRequest for late-arriving grid observer");
        let (_, _, late_set_state_rep) = sys_h
            .drain_observer_updates(late_ship_id)
            .await
            .expect("drain SetState for late observer");
        let late_destiny_dbg = format!("{late_set_state_rep:?}");
        assert!(
            late_destiny_dbg.contains("\"SetState\"")
                && late_destiny_dbg.contains(&format!(
                    "(String(\"skinID\"), Integer({resolved_skin_id}))"
                )),
            "late-arriving observer SetState slim item must include skinID={resolved_skin_id}, got: {late_destiny_dbg}"
        );

        // Re-spawning the skinned ship on grid emits AddBalls2 with skinID in DoDestinyUpdate
        let _ = sys_h.remove_ball(active_ship_id).await;
        let _ = sys_h.drain_observer_updates(late_ship_id).await;
        let _ = sys_h
            .spawn_ball_with_meta(
                crate::sovereignty_equinox_gameplay::make_ball(
                    active_ship_id,
                    587,
                    DVec3::new(10_000.0, 0.0, 0.0),
                ),
                ctx.build_ship_combat_state(PILOT_1, active_ship_id, 587),
                ctx.build_ship_slim_meta(PILOT_1, 587),
            )
            .await;
        let (_, _, addballs2_rep) = sys_h
            .drain_observer_updates(late_ship_id)
            .await
            .expect("drain AddBalls2 for late observer");
        let addballs2_dbg = format!("{addballs2_rep:?}");
        assert!(
            addballs2_dbg.contains("\"AddBalls2\"")
                && addballs2_dbg.contains(&format!(
                    "(String(\"skinID\"), Integer({resolved_skin_id}))"
                )),
            "AddBalls2 slim item must include skinID={resolved_skin_id}, got: {addballs2_dbg}"
        );

        // Verify canonical SDE skin_id persists across SQLite restart AND populates build_ship_slim_meta
        let ctx_re =
            crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite(&ctx, "b7_skin_apply");
        let persisted_skin = ctx_re
            .db
            .with_snapshot(|s| s.applied_ship_skins.get(&active_ship_id).copied());
        assert_eq!(persisted_skin, Some(resolved_skin_id));
        let reloaded_meta = ctx_re.build_ship_slim_meta(PILOT_1, 587);
        assert_eq!(
            reloaded_meta.skin_id,
            Some(resolved_skin_id),
            "build_ship_slim_meta after SQLite reload must populate canonical skin_id"
        );
    }

    #[tokio::test]
    async fn test_structure_directory_accessible_services_and_acl() {
        // parity: feat:structures.structure-directory
        let ctx = ServerContext::new();
        let mut unauth = SessionState::new(997);
        assert!(
            rpc(&ctx, &mut unauth, "structureDirectory", "GetStructures", &[])
                .await
                .is_err(),
            "unauthenticated structureDirectory.GetStructures must fail"
        );

        let mut sess1 = setup_pilot(&ctx, PILOT_1).await;
        let mut sess2 = setup_pilot_for_user(&ctx, 77_012, "other", "other-secret", PILOT_2).await;
        let p3_uid = ctx
            .db
            .provision_player_account("b7_blocked", "b7-blocked-secret");
        let p3_char = ctx
            .db
            .create_character(p3_uid, "Batch7 Blocked", 1, 1, 11, 17, &ctx.sde)
            .expect("create blocked pilot")
            .character_id;
        let mut sess3 =
            setup_pilot_for_user(&ctx, 77_013, "b7_blocked", "b7-blocked-secret", p3_char).await;
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&p3_char) {
                ch.solar_system_id = 30_000_144; // Perimeter (not in Jita)
                ch.station_id = None;
            }
        });
        sess3.solar_system_id = Some(30_000_144);
        sess3.station_id = None;

        // Create owning player corporation for PILOT_1
        let corp_rep = rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "AddCorporation",
            &[
                PyRep::String("Batch7 Upwell Holdings".into()),
                PyRep::String("B7UH".into()),
                PyRep::String("Structure directory corp".into()),
                PyRep::Real(0.05),
            ],
        )
        .await
        .expect("AddCorporation");
        let owner_corp = corp_rep
            .dict_get("corporationID")
            .and_then(|v| v.as_i64())
            .or_else(|| corp_rep.as_i64())
            .expect("owner_corp");
        sess1.corporation_id = Some(owner_corp);

        // Deploy an operational Raitaru (35825) with online Cloning (35894 -> 10) & Manufacturing (35878 -> 22) services
        let structure_id = 1_045_000_777_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                structure_id,
                UpwellStructureRecord {
                    structure_id,
                    type_id: 35_825,
                    owner_corp_id: owner_corp,
                    solar_system_id: JITA_SYS,
                    name: "Jita - B7 Industrial Complex".to_string(),
                    position: [150_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".to_string(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            for (iid, tid, flag) in [
                (990_800_001i64, 35_894u32, InventoryFlag::ServiceSlot0),
                (990_800_002i64, 35_878u32, InventoryFlag::ServiceSlot1),
            ] {
                snap.items.insert(
                    iid,
                    InventoryItemRecord {
                        item_id: iid,
                        type_id: tid,
                        owner_id: owner_corp,
                        location_id: structure_id,
                        flag,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: Default::default(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                snap.module_online_states.insert(iid, true);
            }
        });

        // Configure ACL profile: PILOT_2 allowed, p3_char blocked
        eve_server::services::station_structure::structure_upwell_ops::configure_structure_access_profile(
            &ctx,
            &sess1,
            structure_id,
            Some(false),
            &[(10, 0.01), (22, 0.02)],
            &[],
            &[p3_char],
            &[PILOT_2],
        )
        .expect("configure_structure_access_profile");

        for sess in [&mut sess1, &mut sess2] {
            let dir_rep = rpc(&ctx, sess, "structureDirectory", "GetStructures", &[])
                .await
                .expect("GetStructures");
            let PyRep::Dict(entries) = dir_rep else {
                panic!("expected Dict from structureDirectory.GetStructures");
            };
            let entry = entries
                .iter()
                .find_map(|(k, v)| (k.as_i64() == Some(structure_id)).then_some(v))
                .expect("accessible structure must appear in GetStructures");
            let Some(PyRep::Dict(svcs)) = entry.dict_get("services") else {
                panic!("expected services Dict on structure directory entry");
            };
            assert!(svcs.contains(&(PyRep::Integer(10), PyRep::Integer(1))));
            assert!(svcs.contains(&(PyRep::Integer(22), PyRep::Integer(1))));
        }

        // Blocked pilot PILOT_3 does not see structure in GetStructures and is denied GetStructureInfo
        let blocked_dir = rpc(&ctx, &mut sess3, "structureDirectory", "GetStructures", &[])
            .await
            .expect("GetStructures blocked pilot");
        let PyRep::Dict(blocked_entries) = blocked_dir else {
            panic!("expected Dict");
        };
        assert!(
            !blocked_entries
                .iter()
                .any(|(k, _)| k.as_i64() == Some(structure_id)),
            "ACL-blocked pilot must not see restricted structure in GetStructures"
        );
        let info_err = rpc(
            &ctx,
            &mut sess3,
            "structureDirectory",
            "GetStructureInfo",
            &[PyRep::Integer(structure_id)],
        )
        .await
        .expect_err("ACL-blocked remote pilot must fail GetStructureInfo");
        assert_eq!(info_err.code, "StructureAccessDenied");

        // Verify SQLite persistence of structure directory & online services
        let ctx_re =
            crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite(&ctx, "b7_struct_dir");
        let mut sess2_re =
            setup_pilot_for_user(&ctx_re, 77_022, "other", "other-secret", PILOT_2).await;
        let dir_re = rpc(&ctx_re, &mut sess2_re, "structureDirectory", "GetStructures", &[])
            .await
            .expect("GetStructures after reload");
        let PyRep::Dict(re_entries) = dir_re else {
            panic!("expected Dict after reload");
        };
        assert!(re_entries
            .iter()
            .any(|(k, _)| k.as_i64() == Some(structure_id)));
    }
}

pub(crate) mod alliance_dictatorial_declare_executor_support {
    // Integration tests for open `rpc:` parity rows in social, notification, alliance, corp, and war domains (Batch 7).


    use std::collections::HashMap;

    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;

    use crate::common;

    const PILOT_1: i64 = 90_000_001;
    const PILOT_2: i64 = 90_000_002;
    const STATION_ID: i64 = 60_003_760;

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
            .await
    }

    async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(char_id as u64);
        common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        let uid = sess.user_id.unwrap_or(1);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.user_id = uid;
                ch.balance = eve_db::IskCents(500_000_000_00);
                ch.station_id = Some(STATION_ID);
            }
        });
        rpc(
            ctx,
            &mut sess,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
        )
        .await
        .expect("SelectCharacterID");

        sess.station_id = Some(STATION_ID);
        sess.structure_id = None;
        eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
        sess
    }

    #[tokio::test]
    async fn test_alliance_dictatorial_declare_executor_support_and_persistence() {
        const EXPECTED_MISSING_ERR: &str = "AllianceNotFound";
        let ctx = ServerContext::new();
        let mut unauth_sess = SessionState::new(995);
        assert!(
            rpc(&ctx, &mut unauth_sess, "allianceRegistry", "GetAlliance", &[])
                .await
                .is_err()
        );

        let mut sess = setup_pilot(&ctx, PILOT_1).await;

        // Negative case: non-existent ID raises AllianceNotFound
        let err = rpc(&ctx, &mut sess, "allianceRegistry", "GetAlliance", &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err();
        assert_eq!(err.code, EXPECTED_MISSING_ERR);

        // Create a player corp and an alliance
        let corp_rep = rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "AddCorporation",
            &[
                PyRep::String("Batch7 Vanguard".into()),
                PyRep::String("B7VG".into()),
                PyRep::String("Initial corp desc".into()),
                PyRep::Real(0.05),
            ],
        )
        .await
        .expect("AddCorporation");
        let corp_id = corp_rep
            .dict_get("corporationID")
            .and_then(|v| v.as_i64())
            .or_else(|| corp_rep.as_i64())
            .expect("corp_id");
        sess.corporation_id = Some(corp_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                corp.master_wallet = eve_db::IskCents(2_000_000_000_00);
                if let Some(div) = corp.wallet_divisions.first_mut() {
                    div.balance = eve_db::IskCents(2_000_000_000_00);
                }
            }
        });

        let ally_rep = rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "CreateAlliance",
            &[
                PyRep::String("Batch7 Coalition".into()),
                PyRep::String("B7CL".into()),
                PyRep::String("Initial alliance desc".into()),
                PyRep::String("https://initial.example".into()),
            ],
        )
        .await
        .expect("CreateAlliance");
        let ally_id = ally_rep
            .dict_get("allianceID")
            .and_then(|v| v.as_i64())
            .or_else(|| ally_rep.as_i64())
            .expect("ally_id");
        sess.alliance_id = Some(ally_id);

        let init_info = rpc(
            &ctx,
            &mut sess,
            "allianceRegistry",
            "GetAlliance",
            &[PyRep::Integer(ally_id)],
        )
        .await
        .expect("initial GetAlliance");
        assert_eq!(init_info.dict_get("dictatorial"), Some(&PyRep::Bool(false)));

        // Mutate alliance description, url, allowWar, and dictatorial=true via UpdateAlliance
        rpc(
            &ctx,
            &mut sess,
            "allianceRegistry",
            "UpdateAlliance",
            &[
                PyRep::String("Updated Coalition Charter".into()),
                PyRep::String("https://coalition.example".into()),
                PyRep::Bool(true),
                PyRep::Bool(true),
            ],
        )
        .await
        .expect("UpdateAlliance");

        // Add a second member corporation and verify DeclareExecutorSupport is blocked while dictatorial=true
        let mut sess_member = setup_pilot(&ctx, 90_000_002).await;
        let corp2_rep = rpc(
            &ctx,
            &mut sess_member,
            "corpRegistry",
            "AddCorporation",
            &[
                PyRep::String("Batch7 Wing".into()),
                PyRep::String("B7WG".into()),
                PyRep::String("Second member corp".into()),
                PyRep::Real(0.05),
            ],
        )
        .await
        .expect("AddCorporation 2");
        let corp2_id = corp2_rep
            .dict_get("corporationID")
            .and_then(|v| v.as_i64())
            .or_else(|| corp2_rep.as_i64())
            .expect("corp2_id");
        sess_member.corporation_id = Some(corp2_id);
        sess_member.alliance_id = Some(ally_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c2) = snap.corporations.get_mut(&corp2_id) {
                c2.alliance_id = Some(ally_id);
            }
            if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
                ch2.alliance_id = Some(ally_id);
            }
            if let Some(al) = snap.alliances.get_mut(&ally_id) {
                al.member_corp_ids.insert(corp2_id);
            }
        });

        let vote_err = rpc(
            &ctx,
            &mut sess_member,
            "allianceRegistry",
            "DeclareExecutorSupport",
            &[PyRep::Integer(corp2_id)],
        )
        .await
        .expect_err("DeclareExecutorSupport must fail in a dictatorial alliance");
        assert_eq!(vote_err.code, "AllianceIsDictatorial");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.alliances[&ally_id].executor_corp_id),
            corp_id,
            "executorCorpID must remain unchanged when alliance is dictatorial"
        );

        // Reload from SQLite and verify alliance description, url, and dictatorial=true persist via GetAlliance
        let ctx_reloaded =
            crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite(&ctx, "b7_alliance_attrs");
        let mut sess_re = setup_pilot(&ctx_reloaded, PILOT_1).await;
        let info_re = rpc(
            &ctx_reloaded,
            &mut sess_re,
            "allianceRegistry",
            "GetAlliance",
            &[PyRep::Integer(ally_id)],
        )
        .await
        .expect("GetAlliance after reload");
        assert_eq!(
            info_re.dict_get("description"),
            Some(&PyRep::String("Updated Coalition Charter".into()))
        );
        assert_eq!(
            info_re.dict_get("url"),
            Some(&PyRep::String("https://coalition.example".into()))
        );
        assert_eq!(info_re.dict_get("dictatorial"), Some(&PyRep::Bool(true)));
    }

    #[tokio::test]
    async fn test_war_registry_bind_and_unbind() {
        // parity: rpc:warRegistry.Bind
        // parity: rpc:warRegistry.Unbind
        let ctx = ServerContext::new();
        let mut unauth_sess = SessionState::new(998);
        for (svc, meth) in [("warRegistry", "Bind"), ("warRegistry", "Unbind")] {
            assert!(
                rpc(&ctx, &mut unauth_sess, svc, meth, &[]).await.is_err(),
                "unauthenticated {svc}.{meth} must fail"
            );
        }

        let mut sess = setup_pilot(&ctx, PILOT_1).await;
        let mut sess_other = crate::gameplay_services_and_rpcs::setup_pilot_for_user(
            &ctx,
            77_092,
            "other",
            "other-secret",
            PILOT_2,
        )
        .await;

        // Bind & Unbind warRegistry, verify wrong-session Unbind fails, and Bind warStatisticMgr
        let war_bind = rpc(&ctx, &mut sess, "warRegistry", "Bind", &[])
            .await
            .expect("warRegistry.Bind");
        let war_bound_id = war_bind.as_i64().expect("warRegistry bound_id");
        assert!(war_bound_id > 0);
        assert!(
            rpc(
                &ctx,
                &mut sess_other,
                "warRegistry",
                "Unbind",
                &[PyRep::Integer(war_bound_id)],
            )
            .await
            .is_err(),
            "another session must not unbind PILOT_1's warRegistry bound object"
        );
        assert_eq!(
            rpc(
                &ctx,
                &mut sess,
                "warRegistry",
                "Unbind",
                &[PyRep::Integer(war_bound_id)],
            )
            .await
            .expect("warRegistry.Unbind"),
            PyRep::None
        );
        assert!(!ctx
            .bound_objects
            .read()
            .unwrap()
            .contains_key(&(war_bound_id as u64)));
    }
}

pub(crate) mod zarzakh_deathless_shipcaster_entry_beacons {
    // Integration tests for open Zarzakh & Deathless Shipcaster `feat:navigation.*` rows (batch 8, part 1):
    // - `feat:navigation.deathless-shipcaster-zarzakh`
    // - `feat:navigation.zarzakh-emanation-lock`
    // - `feat:navigation.zarzakh-fulcrum-gates`


    use std::collections::HashMap;

    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, DVec3, MovementMode, TargetLock};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::cluster_macho_misc_services::{
        set_zarzakh_horizon_siege_active, ClusterMiscRuntimeState,
    };

    use crate::common;
    use crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite;

    pub(crate) const ZARZAKH_SYS: u32 = 30_100_000;
    pub(crate) const FULCRUM_STATION_ID: i64 = 60_015_187;
    pub(crate) const JITA_STATION_ID: i64 = 60_003_760;
    pub(crate) const TURNUR_SYS: u32 = 30_002_086;
    pub(crate) const VARD_SYS: u32 = 30_002_538;
    pub(crate) const ALSAVOINON_SYS: u32 = 30_003_841;
    pub(crate) const HIKKOKEN_SYS: u32 = 30_045_338;

    pub(crate) const TURNUR_GATE_ID: i64 = 50_016_562;
    pub(crate) const ALSAVOINON_GATE_ID: i64 = 50_016_563;
    pub(crate) const ZARZAKH_TURNUR_GATE_ID: i64 = 50_016_566;
    pub(crate) const ZARZAKH_ALSAVOINON_GATE_ID: i64 = 50_016_567;
    pub(crate) const ZARZAKH_PAD_ID: i64 = 104_000_030;

    const ERR_EMPIRE_MILITIA_REFUSED: &str = "EmpireMilitiaRefused";
    const ERR_CAP_SHIPCASTER: &str = "CapitalShipCannotJumpShipcaster";
    const ERR_OUT_OF_RANGE: &str = "OutsideOperationalRange";
    pub(crate) const ERR_HORIZON_SIEGE_CASTER: &str = "HorizonSiegeActive";
    const ERR_GATE_LOCKED: &str = "ZarzakhGateLocked";
    const ERR_CAP_STARGATE: &str = "ShipCannotUseStargate";
    const ERR_NO_DSCAN: &str = "CannotDscanInThisSystem";
    const ERR_DISALLOWED_HAZARD_MOD: &str = "ModuleDisallowedInHazardSystem";
    pub(crate) const ERR_ANCHORING_ZARZAKH: &str = "AnchoringDisallowedInZarzakh";
    const ERR_FULCRUM_EMPIRE: &str = "FulcrumRefusesEmpireMilitia";

    pub(crate) async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, svc, None, method, args, &kw, None).await
    }

    pub(crate) async fn gate_jump(
        ctx: &ServerContext,
        sess: &mut SessionState,
        from_gate_id: i64,
        to_gate_id: i64,
        ship_id: i64,
    ) -> Result<PyRep, BridgeRpcError> {
        rpc(
            ctx,
            sess,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(from_gate_id), PyRep::Integer(to_gate_id), PyRep::Integer(ship_id)],
        )
        .await
    }

    pub(crate) async fn setup_space_pilot(
        ctx: &ServerContext,
        sess_id: u64,
        char_id: i64,
        ship_id: i64,
        ship_type_id: u32,
        sys_id: u32,
        pos: DVec3,
        war_faction_id: Option<i64>,
    ) -> SessionState {
        let mut sess = SessionState::new(sess_id);
        if char_id != 90_000_001 {
            ctx.db.with_mut_snapshot(|s| {
                if !s.characters.contains_key(&char_id) {
                    let mut ch = s.characters.get(&90_000_001).cloned().unwrap();
                    ch.character_id = char_id;
                    ch.user_id = 2;
                    ch.name = format!("Zarzakh Pilot {char_id}");
                    s.characters.insert(char_id, ch);
                }
            });
            common::login_pilot(ctx, &mut sess, "other", "other-secret").await;
        } else {
            common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
        }
        rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        ctx.advance_secs(11);

        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        sess.station_id = None;
        sess.structure_id = None;

        let mass = ctx
            .sde
            .types
            .get(&ship_type_id)
            .map(|t| t.mass)
            .filter(|&m| m > 0.0)
            .unwrap_or(1_200_000.0);

        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.active_ship_id = ship_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.war_faction_id = war_faction_id;
                ch.balance = IskCents::from_isk(250_000_000);
            }
            s.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            s.item_positions.insert(ship_id, pos.to_array());
        });

        let sys = ctx.systems.get_or_spawn(sys_id);
        let ball = Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = sys
            .spawn_ball_with_meta(
                ball,
                ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
                ctx.build_ship_slim_meta(char_id, ship_type_id),
            )
            .await;
        eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
        sess
    }

    #[tokio::test]
    async fn test_zarzakh_deathless_shipcaster_entry_beacons() {
        // parity: feat:navigation.deathless-shipcaster-zarzakh
        let ctx = ServerContext::new();
        let char_id = 90_000_001_i64;
        let ship_id = 100_000_001_i64;
        let beacon_pos = DVec3::new(250_000.0, 0.0, 0.0);

        // 0. Spawning Turnur, Vard, Alsavoinon, Hikkoken, and Zarzakh populates Deathless Shipcaster balls + SlimItemMeta,
        //    and `ClusterMiscRuntimeState::from_db` merges canonical Deathless beacons/pads onto pre-existing DB blobs
        for (sys_id, item_id) in [
            (TURNUR_SYS, 105_000_011_i64),
            (VARD_SYS, 105_000_012_i64),
            (ALSAVOINON_SYS, 105_000_013_i64),
            (HIKKOKEN_SYS, 105_000_014_i64),
            (ZARZAKH_SYS, 105_000_010_i64),
            (ZARZAKH_SYS, ZARZAKH_PAD_ID),
        ] {
            let sys_h = ctx.systems.get_or_spawn(sys_id);
            let bp = sys_h.ballpark.read().unwrap();
            assert_eq!(bp.balls.get(&item_id).map(|b| b.type_id), Some(79_018));
            assert_eq!(bp.ball_slim_meta.get(&item_id).map(|m| m.category_id), Some(65));
        }
        ctx.db.with_mut_snapshot(|s| {
            s.service_state_blobs.insert(
                "cluster_misc_state".into(),
                r#"{"landing_pads":[{"item_id":104000001,"type_id":75929,"solar_system_id":30002811,"faction_id":500001,"is_built":true,"link_timestamp":1700000000,"is_disrupted":false,"position":[100000.0,0.0,0.0]}],"launchers":[{"item_id":105000001,"type_id":76466,"solar_system_id":30045324,"faction_id":500001,"linked_pad_ids":[104000001],"position":[200000.0,0.0,0.0]}]}"#.into(),
            );
        });
        let merged = ClusterMiscRuntimeState::from_db(&ctx.db);
        assert!(merged.landing_pads.iter().any(|p| p.item_id == ZARZAKH_PAD_ID));
        for bid in 105_000_010_i64..=105_000_014 {
            assert!(merged.launchers.iter().any(|l| l.item_id == bid));
        }

        // 1. All 4 Empire militia factions (500_001..=500_004) are refused at Deathless Shipcaster beacons
        for emp_faction in 500_001_i64..=500_004 {
            let mut sess = setup_space_pilot(
                &ctx,
                884_100 + emp_faction as u64,
                char_id,
                ship_id,
                587,
                TURNUR_SYS,
                beacon_pos,
                Some(emp_faction),
            )
            .await;
            let emp_err = rpc(
                &ctx,
                &mut sess,
                "shipcasterTravelMgr",
                "CmdJumpThroughShipcaster",
                &[PyRep::Integer(105_000_011), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
            )
            .await
            .expect_err("Empire militia must be refused by Deathless Shipcaster");
            assert_eq!(emp_err.code, ERR_EMPIRE_MILITIA_REFUSED);
        }

        // 2. Capital / Freighter ships (Chimera 23915, Charon 20185) are refused
        for (idx, cap_tid) in [23_915_u32, 20_185_u32].into_iter().enumerate() {
            let mut cap_sess = setup_space_pilot(
                &ctx,
                884_120 + idx as u64,
                char_id,
                ship_id,
                cap_tid,
                VARD_SYS,
                beacon_pos,
                None,
            )
            .await;
            let cap_err = rpc(
                &ctx,
                &mut cap_sess,
                "shipcasterTravelMgr",
                "CmdJumpThroughShipcaster",
                &[PyRep::Integer(105_000_012), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
            )
            .await
            .expect_err("Capital/Freighter ship must be refused by Deathless Shipcaster");
            assert_eq!(cap_err.code, ERR_CAP_SHIPCASTER);
        }

        // 3. Ship outside 5,000m operational range is refused
        let mut far_sess = setup_space_pilot(
            &ctx,
            884_130,
            char_id,
            ship_id,
            587,
            ALSAVOINON_SYS,
            beacon_pos + DVec3::new(12_000.0, 0.0, 0.0),
            None,
        )
        .await;
        let range_err = rpc(
            &ctx,
            &mut far_sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(105_000_013), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
        )
        .await
        .expect_err("Ship beyond 5,000m from Deathless beacon must be refused");
        assert_eq!(range_err.code, ERR_OUT_OF_RANGE);

        // 4. Horizon Siege active blocks Deathless Shipcaster entry into Zarzakh
        let mut siege_sess = setup_space_pilot(&ctx, 884_135, char_id, ship_id, 587, ALSAVOINON_SYS, beacon_pos, None).await;
        set_zarzakh_horizon_siege_active(&ctx, true);
        let siege_err = rpc(
            &ctx,
            &mut siege_sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(105_000_013), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
        )
        .await
        .expect_err("Horizon Siege must block Deathless Shipcaster into Zarzakh");
        assert_eq!(siege_err.code, ERR_HORIZON_SIEGE_CASTER);
        set_zarzakh_horizon_siege_active(&ctx, false);

        // 5. All 4 beacons (Turnur, Vard, Alsavoinon, Hikkoken) jump eligible subcaps into Zarzakh
        for (idx, (origin_sys, beacon_id, faction_opt)) in [
            (TURNUR_SYS, 105_000_011_i64, None),
            (VARD_SYS, 105_000_012_i64, Some(500_010_i64)),
            (ALSAVOINON_SYS, 105_000_013_i64, Some(500_011_i64)),
            (HIKKOKEN_SYS, 105_000_014_i64, None),
        ]
        .into_iter()
        .enumerate()
        {
            let mut jump_sess = setup_space_pilot(
                &ctx,
                884_140 + idx as u64,
                char_id,
                ship_id,
                587,
                origin_sys,
                beacon_pos + DVec3::new(500.0, 0.0, 0.0),
                faction_opt,
            )
            .await;
            rpc(
                &ctx,
                &mut jump_sess,
                "shipcasterTravelMgr",
                "CmdJumpThroughShipcaster",
                &[PyRep::Integer(beacon_id), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID)],
            )
            .await
            .expect("Eligible subcap should jump into Zarzakh via Deathless Shipcaster beacon");
            assert_eq!(jump_sess.solar_system_id, Some(ZARZAKH_SYS));
        }

        // 6. SQLite reload persists pilot and ship arrival in Zarzakh
        let reloaded = persist_and_reopen_sqlite(&ctx, "b8_deathless_shipcaster");
        let (persisted_sys, persisted_ship_loc) = reloaded.db.with_snapshot(|s| {
            (
                s.characters.get(&char_id).map(|c| c.solar_system_id),
                s.items.get(&ship_id).map(|i| i.location_id),
            )
        });
        assert_eq!(persisted_sys, Some(ZARZAKH_SYS));
        assert_eq!(persisted_ship_loc, Some(ZARZAKH_SYS as i64));
    }

    #[tokio::test]
    async fn test_zarzakh_emanation_lock_lifecycle_and_exits() {
        // parity: feat:navigation.zarzakh-emanation-lock
        let ctx = ServerContext::new();
        let char_id = 90_000_001_i64;
        let ship_id = 100_000_001_i64;

        let turnur_gate_pos = ctx.sde.jumps[&TURNUR_GATE_ID].position;
        let alsa_gate_pos = ctx.sde.jumps[&ALSAVOINON_GATE_ID].position;
        let z_turnur_pos = ctx.sde.jumps[&ZARZAKH_TURNUR_GATE_ID].position;
        let z_alsa_pos = ctx.sde.jumps[&ZARZAKH_ALSAVOINON_GATE_ID].position;

        let mut sess = setup_space_pilot(
            &ctx,
            884_201,
            char_id,
            ship_id,
            621,
            TURNUR_SYS,
            turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0),
            Some(500_010),
        )
        .await;

        // 1. Entering Zarzakh via Turnur gate locks pilot to Zarzakh-Turnur gate (50016566) for 6h (21,600s)
        gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
            .await
            .expect("Jump into Zarzakh via Turnur gate");
        assert_eq!(sess.solar_system_id, Some(ZARZAKH_SYS));
        let (locked_gate, lock_exp) = ctx.db.with_snapshot(|s| s.zarzakh_gate_locks[&char_id]);
        assert_eq!(locked_gate, ZARZAKH_TURNUR_GATE_ID);
        assert!(lock_exp >= ctx.now_unix() + 21_590);

        // 2. Exiting via Alsavoinon gate (50016567) is refused; exiting via locked Turnur gate (50016566) succeeds
        let z_sys = ctx.systems.get_or_spawn(ZARZAKH_SYS);
        z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = z_alsa_pos;
        ctx.advance_secs(12);
        let lock_err = gate_jump(&ctx, &mut sess, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_GATE_ID, ship_id)
            .await
            .expect_err("Different Zarzakh exit gate must be refused while 6h lock is active");
        assert_eq!(lock_err.code, ERR_GATE_LOCKED);

        z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = z_turnur_pos;
        gate_jump(&ctx, &mut sess, ZARZAKH_TURNUR_GATE_ID, TURNUR_GATE_ID, ship_id)
            .await
            .expect("Same Zarzakh gate exit must succeed");
        assert_eq!(sess.solar_system_id, Some(TURNUR_SYS));

        // 3. Entry from outside is never blocked by old lock, and entering via Alsavoinon resets lock to 50016567
        let mut alsa_sess = setup_space_pilot(
            &ctx, 884_202, char_id, ship_id, 621, ALSAVOINON_SYS, alsa_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
        ).await;
        gate_jump(&ctx, &mut alsa_sess, ALSAVOINON_GATE_ID, ZARZAKH_ALSAVOINON_GATE_ID, ship_id)
            .await
            .expect("External entry via Alsavoinon gate must succeed and reset lock");
        assert_eq!(ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.get(&char_id).map(|v| v.0)), Some(ZARZAKH_ALSAVOINON_GATE_ID));

        // 4. After 6h (21,601s) expiry, exiting via Turnur gate (50016566) is allowed
        z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = z_turnur_pos;
        ctx.advance_secs(21_605);
        gate_jump(&ctx, &mut alsa_sess, ZARZAKH_TURNUR_GATE_ID, TURNUR_GATE_ID, ship_id)
            .await
            .expect("Zarzakh gate exit via another gate must succeed once 6h lock expires");

        // 5. Leaving via Deathless Shipcaster (105_000_010 -> 104_000_020) is allowed while locked and clears lock
        let mut caster_sess = setup_space_pilot(
            &ctx, 884_203, char_id, ship_id, 621, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
        ).await;
        gate_jump(&ctx, &mut caster_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
            .await
            .expect("Re-enter Zarzakh to acquire fresh gate lock");
        z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = DVec3::new(250_000.0, 0.0, 0.0);
        ctx.advance_secs(12);
        rpc(
            &ctx,
            &mut caster_sess,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(105_000_010), PyRep::Integer(30_002_056), PyRep::Integer(104_000_020)],
        )
        .await
        .expect("Leaving Zarzakh via Deathless Shipcaster must succeed while gate-locked");
        assert!(!ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));

        // 6. Live gate entry -> Fulcrum CloneJump preserves lock; Jita CloneJump & SelfDestruct pod death clear lock
        let (fulcrum_clone_id, jita_clone_id, pod_id, pod_b_id, char_b_id) =
            (82_000_880_i64, 82_000_881_i64, 100_000_881_i64, 100_000_882_i64, 90_000_882_i64);
        let mut fulcrum_swap_sess = setup_space_pilot(
            &ctx, 884_204, char_b_id, pod_b_id, 670, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
        ).await;
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&char_b_id).unwrap();
            ch.last_clone_jump_unix = 0;
            ch.home_station_id = JITA_STATION_ID;
            s.skills.entry(char_b_id).or_default().insert(24_242, TrainedSkillRecord { type_id: 24_242, level: 3, skill_points: 45_000.0 });
            s.jump_clones.entry(char_b_id).or_default().push(JumpCloneRecord {
                jump_clone_id: fulcrum_clone_id,
                character_id: char_b_id,
                location_id: FULCRUM_STATION_ID,
                implants: vec![],
            });
        });
        gate_jump(&ctx, &mut fulcrum_swap_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, pod_b_id)
            .await
            .expect("Enter Zarzakh via Turnur gate to acquire live gate lock");
        let fulcrum_pos = ctx.sde.stations[&FULCRUM_STATION_ID].position;
        z_sys.ballpark.write().unwrap().balls.get_mut(&pod_b_id).unwrap().position =
            fulcrum_pos + DVec3::new(100.0, 0.0, 0.0);
        ctx.advance_secs(12);
        rpc(&ctx, &mut fulcrum_swap_sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
            .await
            .expect("Dock capsule at The Fulcrum");
        ctx.advance_secs(12);
        rpc(&ctx, &mut fulcrum_swap_sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(fulcrum_clone_id)])
            .await
            .expect("Same-station clone swap inside The Fulcrum must succeed");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.get(&char_b_id).map(|v| v.0)),
            Some(ZARZAKH_TURNUR_GATE_ID),
            "Swapping jump clones inside The Fulcrum must NOT clear Zarzakh 6-hour gate lock"
        );

        let mut clone_sess = setup_space_pilot(
            &ctx, 884_205, char_id, pod_id, 670, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
        ).await;
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&char_id).unwrap();
            ch.last_clone_jump_unix = 0;
            ch.home_station_id = JITA_STATION_ID;
            s.skills.entry(char_id).or_default().insert(24_242, TrainedSkillRecord { type_id: 24_242, level: 3, skill_points: 45_000.0 });
            s.jump_clones.entry(char_id).or_default().push(JumpCloneRecord {
                jump_clone_id: jita_clone_id,
                character_id: char_id,
                location_id: JITA_STATION_ID,
                implants: vec![],
            });
        });
        gate_jump(&ctx, &mut clone_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, pod_id)
            .await
            .expect("Enter Zarzakh via Turnur gate before Jita CloneJump");
        z_sys.ballpark.write().unwrap().balls.get_mut(&pod_id).unwrap().position =
            fulcrum_pos + DVec3::new(100.0, 0.0, 0.0);
        ctx.advance_secs(12);
        rpc(&ctx, &mut clone_sess, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
            .await
            .expect("Dock capsule at The Fulcrum before Jita CloneJump");
        ctx.advance_secs(12);
        rpc(&ctx, &mut clone_sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(jita_clone_id)])
            .await
            .expect("CloneJump out of Zarzakh to Jita must succeed while locked");
        assert!(!ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));

        let mut pod_sess = setup_space_pilot(
            &ctx, 884_206, char_id, pod_id, 670, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), Some(500_010),
        ).await;
        gate_jump(&ctx, &mut pod_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, pod_id)
            .await
            .expect("Re-enter Zarzakh via Turnur gate before capsule self-destruct");
        assert!(ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));
        rpc(&ctx, &mut pod_sess, "ship", "SelfDestruct", &[])
            .await
            .expect("Start capsule SelfDestruct countdown in Zarzakh");
        ctx.advance_secs(125);
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters[&char_id].station_id),
            Some(JITA_STATION_ID),
            "Capsule self-destruct countdown tick must detonate pod and respawn pilot at home station"
        );
        let reloaded = persist_and_reopen_sqlite(&ctx, "b8_emanation_lock");
        assert!(!reloaded.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&char_id)));
    }

    #[tokio::test]
    async fn test_zarzakh_fulcrum_gates_dscan_size_bubbles_and_docking() {
        // parity: feat:navigation.zarzakh-fulcrum-gates
        let ctx = ServerContext::new();
        let char_id = 90_000_001_i64;
        let ship_id = 100_000_001_i64;
        let turnur_gate_pos = ctx.sde.jumps[&TURNUR_GATE_ID].position;
        let z_alsa_pos = ctx.sde.jumps[&ZARZAKH_ALSAVOINON_GATE_ID].position;

        // 1. Capital/Freighter hulls refused on Zarzakh stargates; Orca (28606) allowed and receives 6h lockout
        let mut dread_sess = setup_space_pilot(
            &ctx, 884_301, char_id, ship_id, 19_720, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), None,
        ).await;
        let dread_err = gate_jump(&ctx, &mut dread_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
            .await
            .expect_err("Dreadnought must be refused on Zarzakh stargate");
        assert_eq!(dread_err.code, ERR_CAP_STARGATE);

        let mut orca_sess = setup_space_pilot(
            &ctx, 884_302, char_id, ship_id, 28_606, TURNUR_SYS, turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0), None,
        ).await;
        let prompt_err = gate_jump(&ctx, &mut orca_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
            .await
            .expect_err("First 3-arg Orca jump must prompt for Zarzakh gate toll confirmation");
        assert_eq!(prompt_err.code, "AskConfirmZarzakhGateToll");
        gate_jump(&ctx, &mut orca_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
            .await
            .expect("Confirmed Orca jump must be allowed through Zarzakh stargate");
        assert_eq!(orca_sess.solar_system_id, Some(ZARZAKH_SYS));

        let z_sys = ctx.systems.get_or_spawn(ZARZAKH_SYS);
        z_sys.ballpark.write().unwrap().balls.get_mut(&ship_id).unwrap().position = z_alsa_pos;
        ctx.advance_secs(12);
        let back_err = gate_jump(&ctx, &mut orca_sess, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_GATE_ID, ship_id)
            .await
            .expect_err("Horizon lockout must prevent back-jump via different Zarzakh gate");
        assert_eq!(back_err.code, ERR_GATE_LOCKED);

        // 2. Directional scanner rejected via BOTH V24 `scanSvc.ConeScan` and `scanMgr.ConeScan`
        for svc in ["scanSvc", "scanMgr"] {
            let dscan_err = rpc(&ctx, &mut orca_sess, svc, "ConeScan", &[PyRep::Real(360.0), PyRep::Real(1_000_000_000.0)])
                .await
                .expect_err("Directional scan must be disabled in Zarzakh");
            assert_eq!(dscan_err.code, ERR_NO_DSCAN);
        }

        // 3. Zarzakh no-bubble rules (`disallowInHazardSystem` attr 5561): ISL, unscripted WDFG, and Mobile Warp Disruptor
        let (sabre_ship_id, sabre_mod_id) = (100_000_303_i64, 200_000_301_i64);
        let mut sabre_sess = setup_space_pilot(&ctx, 884_303, char_id, sabre_ship_id, 22_456, ZARZAKH_SYS, DVec3::ZERO, None).await;
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(sabre_mod_id, InventoryItemRecord {
                item_id: sabre_mod_id, type_id: 22_782, owner_id: char_id, location_id: sabre_ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            });
        });
        ctx.invalidate_ship_fitting(sabre_ship_id);
        let isl_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", &[PyRep::Integer(sabre_mod_id)])
            .await
            .expect_err("Interdiction Sphere Launcher must be disallowed in Zarzakh");
        assert_eq!(isl_err.code, ERR_DISALLOWED_HAZARD_MOD);

        let (onyx_ship_id, wdfg_mod_id, script_id, bubble_deploy_id, target_ship_id) =
            (100_000_305_i64, 200_000_302, 200_000_303, 200_000_304, 100_000_399);
        let _target_sess = setup_space_pilot(
            &ctx, 884_304, 90_000_302, target_ship_id, 587, ZARZAKH_SYS, DVec3::new(5_000.0, 0.0, 0.0), None,
        ).await;
        let mut onyx_sess = setup_space_pilot(&ctx, 884_305, char_id, onyx_ship_id, 11_995, ZARZAKH_SYS, DVec3::ZERO, None).await;
        ctx.db.with_mut_snapshot(|s| {
            let sk = s.skills.entry(char_id).or_default();
            sk.insert(11_584, TrainedSkillRecord { type_id: 11_584, level: 5, skill_points: 256_000.0 });
            sk.insert(3_435, TrainedSkillRecord { type_id: 3_435, level: 5, skill_points: 256_000.0 });
            s.items.insert(wdfg_mod_id, InventoryItemRecord {
                item_id: wdfg_mod_id, type_id: 28_654, owner_id: char_id, location_id: onyx_ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            });
            s.items.insert(bubble_deploy_id, InventoryItemRecord {
                item_id: bubble_deploy_id, type_id: 12_198, owner_id: char_id, location_id: onyx_ship_id,
                flag: InventoryFlag::Cargo, quantity: 1, ..Default::default()
            });
        });
        ctx.invalidate_ship_fitting(onyx_ship_id);
        let unscripted_err = rpc(&ctx, &mut onyx_sess, "dogmaIM", "Activate", &[PyRep::Integer(wdfg_mod_id)])
            .await
            .expect_err("Unscripted WDFG bubble must be disallowed in Zarzakh");
        assert_eq!(unscripted_err.code, ERR_DISALLOWED_HAZARD_MOD);

        let mob_bubble_err = rpc(&ctx, &mut onyx_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bubble_deploy_id)])
            .await
            .expect_err("Mobile Warp Disruptor anchoring must be disallowed in Zarzakh");
        assert_eq!(mob_bubble_err.code, ERR_ANCHORING_ZARZAKH);

        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(script_id, InventoryItemRecord {
                item_id: script_id, type_id: 29_003, owner_id: char_id, location_id: onyx_ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 1, ..Default::default()
            });
        });
        ctx.invalidate_ship_fitting(onyx_ship_id);
        let _ = z_sys.add_target_lock(onyx_ship_id, TargetLock {
            target_id: target_ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
        }).await;
        rpc(&ctx, &mut onyx_sess, "dogmaIM", "Activate", &[PyRep::Integer(wdfg_mod_id), PyRep::String(String::new()), PyRep::Integer(target_ship_id)])
            .await
            .expect("Focused-scripted WDFG point (attr 5561=0) should succeed in Zarzakh");

        // 4. Docking at The Fulcrum (60015187): Empire militia refused, neutral docks and persists across SQLite reload
        let fulcrum_pos = ctx.sde.stations[&FULCRUM_STATION_ID].position;
        let mut emp_dock = setup_space_pilot(
            &ctx, 884_306, char_id, ship_id, 621, ZARZAKH_SYS, fulcrum_pos + DVec3::new(100.0, 0.0, 0.0), Some(500_001),
        ).await;
        let emp_dock_err = rpc(&ctx, &mut emp_dock, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
            .await
            .expect_err("Empire militia must be refused docking at The Fulcrum");
        assert_eq!(emp_dock_err.code, ERR_FULCRUM_EMPIRE);

        let mut neutral_dock = setup_space_pilot(
            &ctx, 884_307, char_id, ship_id, 621, ZARZAKH_SYS, fulcrum_pos + DVec3::new(100.0, 0.0, 0.0), None,
        ).await;
        let timer_err = rpc(&ctx, &mut neutral_dock, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
            .await
            .expect_err("Active weapons timer from scripted WDFG point must block Fulcrum docking");
        assert_eq!(timer_err.code, "FulcrumRefusesTimerActive");
        ctx.advance_secs(65);
        let retribution_err = rpc(&ctx, &mut neutral_dock, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
            .await
            .expect_err("15-minute Zarzakh disapproval timer must still block Fulcrum docking at +65s");
        assert_eq!(retribution_err.code, "FulcrumRefusesTimerActive");
        ctx.advance_secs(845);
        rpc(&ctx, &mut neutral_dock, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
            .await
            .expect("Neutral pilot should dock at The Fulcrum once weapons and Zarzakh disapproval timers expire");
        let reloaded = persist_and_reopen_sqlite(&ctx, "b8_fulcrum_dock");
        assert_eq!(reloaded.db.with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.station_id)), Some(FULCRUM_STATION_ID));
    }
}

pub(crate) mod zarzakh_gate_toll_mass_scaling {
    // Integration tests for open Zarzakh `feat:navigation.*` rows (batch 8, part 2):
    // - `feat:navigation.zarzakh-gate-toll`
    // - `feat:navigation.zarzakh-local-horizon-siege`


    use std::collections::HashMap;

    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
    use eve_destiny::DVec3;
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, GoogleAny,
        GoogleTimestamp, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::STATUS_OK;
    use eve_server::router::ServerContext;
    use eve_server::services::cluster_macho_misc_services::{
        get_zarzakh_horizon_siege_state, is_zarzakh_horizon_siege_active, HorizonSiegePhase,
    };
    use eve_server::services::fw_gameplay::{
        apply_pirate_fob_structure_damage, ensure_campaign_pirate_fob, tick_insurgency_campaigns,
    };
    use eve_server::services::insurgency_services::{
        add_system_suppression, init_insurgency_campaign,
    };
    use eve_server::services::quasar_social_chat::on_character_local_chat_membership_refreshed;

    use crate::gameplay_services_and_rpcs::zarzakh_deathless_shipcaster_entry_beacons::{
        gate_jump, rpc, setup_space_pilot, ALSAVOINON_GATE_ID, ALSAVOINON_SYS, ERR_ANCHORING_ZARZAKH,
        ERR_HORIZON_SIEGE_CASTER, FULCRUM_STATION_ID, TURNUR_GATE_ID, TURNUR_SYS,
        ZARZAKH_ALSAVOINON_GATE_ID, ZARZAKH_SYS, ZARZAKH_TURNUR_GATE_ID,
    };
    use crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite;

    const ERR_HORIZON_SIEGE_GATE: &str = "HorizonSiegeGateAccessDenied";
    const ERR_TOLL_PROMPT: &str = "AskConfirmZarzakhGateToll";
    const ERR_TOLL_DECLINED: &str = "ZarzakhGateTollDeclined";
    const ERR_TOLL_FUNDS: &str = "InsufficientFundsForZarzakhGateToll";
    const ERR_CYNO_ZARZAKH: &str = "CynoDisallowedInZarzakh";
    const ERR_DISALLOWED_HAZARD_MOD: &str = "ModuleDisallowedInHazardSystem";

    fn make_public_req(type_name: &str, value: Vec<u8>, now_unix: u64) -> EvePublicRequest {
        EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: now_unix as i64,
                nanos: 0,
            }),
            correlation_uuid: b"corr-zarzakh-b8".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-zarzakh-b8".to_vec(),
            authoritative_context: None,
            trace_context: None,
        }
    }

    #[tokio::test]
    async fn test_zarzakh_gate_toll_mass_scaling_decline_and_pirates() {
        // parity: feat:navigation.zarzakh-gate-toll
        let ctx = ServerContext::new();
        let char_id = 90_000_001_i64;
        let ship_id = 100_000_001_i64;
        let turnur_gate_pos = ctx.sde.jumps[&TURNUR_GATE_ID].position;
        let alsa_gate_pos = ctx.sde.jumps[&ALSAVOINON_GATE_ID].position;

        // 1. Two-step confirmation challenge: first 3-arg call quotes toll (`AskConfirmZarzakhGateToll`),
        //    debits 0 ISK, and declining (`confirmToll: false`) aborts jump with `ZarzakhGateTollDeclined`
        let mut sess = setup_space_pilot(
            &ctx,
            884_401,
            char_id,
            ship_id,
            670, // Capsule (10,000 kg -> 10,000 ISK floor)
            TURNUR_SYS,
            turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0),
            None,
        )
        .await;
        let bal_init = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
        let quote_err = gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
            .await
            .expect_err("Unconfirmed 3-arg Zarzakh gate jump must return AskConfirmZarzakhGateToll");
        assert_eq!(quote_err.code, ERR_TOLL_PROMPT);
        assert!(quote_err.message.contains("10000 ISK"));
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), bal_init);
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].solar_system_id), TURNUR_SYS);

        let mut decline_kw = HashMap::new();
        decline_kw.insert("confirmToll".into(), PyRep::Bool(false));
        let dec_err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                None,
                "CmdStargateJump",
                &[PyRep::Integer(TURNUR_GATE_ID), PyRep::Integer(ZARZAKH_TURNUR_GATE_ID), PyRep::Integer(ship_id)],
                &decline_kw,
                None,
            )
            .await
            .expect_err("Declining Zarzakh gate toll must abort jump");
        assert_eq!(dec_err.code, ERR_TOLL_DECLINED);
        assert_eq!(sess.solar_system_id, Some(TURNUR_SYS));
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), bal_init);

        // 2. Insufficient funds after quote confirmation aborts jump BEFORE mutating character system state
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(5_000));
        let _ = gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id).await;
        let broke_err = gate_jump(&ctx, &mut sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
            .await
            .expect_err("Insufficient ISK must abort confirmed Zarzakh gate jump");
        assert_eq!(broke_err.code, ERR_TOLL_FUNDS);
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].solar_system_id), TURNUR_SYS);

        // 3. Real SDE hull mass scaling (zero `ball.mass` overrides):
        //    Capsule (670, 10k kg) -> 10,000 ISK floor; Caracal (621, 11.91M kg) -> 7,940,000 ISK;
        //    Raven (638, 99.3M kg) -> 66,200,000 ISK; Orca (28606, 150M kg) -> 100,000,000 ISK cap
        for (idx, (type_id, gate_id, to_gate_id, sys_id, gate_pos, expected_toll_isk)) in [
            (670_u32, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, TURNUR_SYS, turnur_gate_pos, 10_000_i64),
            (621_u32, ALSAVOINON_GATE_ID, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_SYS, alsa_gate_pos, 7_940_000_i64),
            (638_u32, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, TURNUR_SYS, turnur_gate_pos, 66_200_000_i64),
            (28_606_u32, ALSAVOINON_GATE_ID, ZARZAKH_ALSAVOINON_GATE_ID, ALSAVOINON_SYS, alsa_gate_pos, 100_000_000_i64),
        ]
        .into_iter()
        .enumerate()
        {
            let mut hull_sess = setup_space_pilot(
                &ctx,
                884_402 + idx as u64,
                char_id,
                ship_id + idx as i64,
                type_id,
                sys_id,
                gate_pos + DVec3::new(200.0, 0.0, 0.0),
                None,
            )
            .await;
            ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(200_000_000));
            let _ = ctx.drain_notifications_for_char(char_id);
            let q_err = gate_jump(&ctx, &mut hull_sess, gate_id, to_gate_id, ship_id + idx as i64)
                .await
                .expect_err("Step 1 of 2-step Zarzakh toll must return quote");
            assert_eq!(q_err.code, ERR_TOLL_PROMPT);
            gate_jump(&ctx, &mut hull_sess, gate_id, to_gate_id, ship_id + idx as i64)
                .await
                .expect("Step 2 confirmed Zarzakh toll jump must succeed");
            assert_eq!(
                ctx.db.with_snapshot(|s| s.characters[&char_id].balance),
                IskCents::from_isk(200_000_000 - expected_toll_isk)
            );
            let notes = ctx.drain_notifications_for_char(char_id);
            assert!(
                notes.iter().any(|n| n.event == "OnAccountChange"),
                "Zarzakh gate toll debit must emit OnAccountChange"
            );
        }

        // 4. Both Angel Cartel (500_010) and Guristas (500_011) pirate militias pay 0 ISK without toll prompt
        for (idx, pirate_faction) in [500_010_i64, 500_011_i64].into_iter().enumerate() {
            let mut p_sess = setup_space_pilot(
                &ctx,
                884_410 + idx as u64,
                char_id,
                ship_id,
                621,
                TURNUR_SYS,
                turnur_gate_pos + DVec3::new(200.0, 0.0, 0.0),
                Some(pirate_faction),
            )
            .await;
            let p_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
            gate_jump(&ctx, &mut p_sess, TURNUR_GATE_ID, ZARZAKH_TURNUR_GATE_ID, ship_id)
                .await
                .expect("Pirate militia pilot must jump into Zarzakh free of toll");
            assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].balance), p_before);
        }

        // 5. SQLite reload preserves journaled `zarzakh_gate_toll` entries
        let reloaded = persist_and_reopen_sqlite(&ctx, "b8_gate_toll");
        let toll_entries = reloaded.db.with_snapshot(|s| {
            s.wallet_journal
                .iter()
                .filter(|j| j.character_id == char_id && j.entry_type == "zarzakh_gate_toll")
                .count()
        });
        assert_eq!(toll_entries, 4, "All 4 confirmed non-pirate SDE hull jumps must be journaled");
    }

    #[tokio::test]
    async fn test_zarzakh_local_horizon_siege_chat_anchoring_and_cyno() {
        // parity: feat:navigation.zarzakh-local-horizon-siege
        let ctx = ServerContext::new();
        let char_a = 90_000_001_i64;
        let char_b = 90_000_502_i64;
        let ship_a = 100_000_501_i64;
        let ship_b = 100_000_502_i64;

        // 1. Delayed local chat in Zarzakh (Quasar `eve_public.chat.local.api.*` AND `LSC.GetMembers`)
        let mut sess_a = setup_space_pilot(&ctx, 884_501, char_a, ship_a, 11_957, ZARZAKH_SYS, DVec3::ZERO, None).await;
        let sess_b = setup_space_pilot(
            &ctx,
            884_502,
            char_b,
            ship_b,
            587,
            ZARZAKH_SYS,
            DVec3::new(1_000.0, 0.0, 0.0),
            None,
        )
        .await;
        let now = ctx.now_unix();
        ctx.quasar_gateway.subscribe_session(&sess_a, vec![], now);
        ctx.quasar_gateway.subscribe_session(&sess_b, vec![], now);
        let _ = ctx.quasar_gateway.poll_notices(sess_a.session_id);
        ctx.sync_character_local_chat(char_b, Some(TURNUR_SYS), ZARZAKH_SYS);

        let entry_notices = ctx.quasar_gateway.poll_notices(sess_a.session_id);
        assert!(
            !entry_notices
                .iter()
                .any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("JoinNotice"))),
            "Entering Zarzakh must not emit JoinNotice before speaking"
        );
        let mem_req = make_public_req("eve_public.chat.local.api.GetMembershipListRequest", Vec::new(), now);
        let mem_rsp = ctx.quasar_gateway.handle_public_request(&sess_a, &mem_req, None, false, now);
        assert_eq!(mem_rsp.status_code, STATUS_OK);
        let mem_wm = WireMessage::decode(&mem_rsp.payload.unwrap().value).unwrap();
        assert_eq!(
            mem_wm.fields.iter().filter(|f| f.field_number == 1).count(),
            0,
            "Silent Zarzakh pilots must stay hidden"
        );

        let z_chan = format!("solarsystemid2:{ZARZAKH_SYS}");
        let lsc_before = rpc(&ctx, &mut sess_a, "LSC", "GetMembers", &[PyRep::String(z_chan)])
            .await
            .expect("LSC.GetMembers in Zarzakh");
        if let PyRep::Dict(entries) = lsc_before {
            assert!(!entries.iter().any(|(k, _)| k.as_i64() == Some(char_b)));
        }

        // Silent pilot B must NOT leak presence via `on_character_local_chat_membership_refreshed` or `MuteRequest`
        let ref_delivered = on_character_local_chat_membership_refreshed(&ctx.quasar_gateway, &ctx.db, char_b as u32, now)
            .expect("on_character_local_chat_membership_refreshed for silent Zarzakh pilot");
        assert_eq!(ref_delivered, 0, "Silent Zarzakh pilot must not emit MembershipRefreshedNotice");
        let mut mute_payload = Vec::new();
        let mut char_id_msg = Vec::new();
        encode_varint_field(1, char_b as u64, &mut char_id_msg);
        encode_bytes_field(1, &char_id_msg, &mut mute_payload);
        let mute_req = make_public_req("eve_public.chat.local.api.admin.MuteRequest", mute_payload, now);
        let _ = ctx.quasar_gateway.handle_public_request(&sess_a, &mute_req, None, true, now);
        let mute_notices = ctx.quasar_gateway.poll_notices(sess_a.session_id);
        assert!(
            !mute_notices
                .iter()
                .any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("MembershipRefreshedNotice"))),
            "MuteRequest on silent Zarzakh pilot must not leak MembershipRefreshedNotice"
        );

        // Pilot B speaks -> JoinNotice + MessageBroadcastNotice emitted and Pilot B appears in Zarzakh local
        let mut chat_msg = Vec::new();
        encode_string_field(1, "o7 from Zarzakh", &mut chat_msg);
        let bcast_req = make_public_req("eve_public.chat.local.api.BroadcastMessageRequest", chat_msg, now + 400);
        let bcast_rsp = ctx.quasar_gateway.handle_public_request(&sess_b, &bcast_req, None, false, now + 400);
        assert_eq!(bcast_rsp.status_code, STATUS_OK);
        let mem_rsp_after = ctx.quasar_gateway.handle_public_request(&sess_a, &mem_req, None, false, now + 400);
        let mem_wm_after = WireMessage::decode(&mem_rsp_after.payload.unwrap().value).unwrap();
        assert_eq!(mem_wm_after.fields.iter().filter(|f| f.field_number == 1).count(), 1);

        // 2. Capsuleer Upwell structure + mobile deployable anchoring/launching prohibited in Zarzakh
        let (depot_id, cyno_mod_id, cov_cyno_mod_id, ind_cyno_mod_id, mjfg_mod_id) = (
            200_000_510_i64,
            200_000_511_i64,
            200_000_512_i64,
            200_000_513_i64,
            200_000_514_i64,
        );
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(
                depot_id,
                InventoryItemRecord {
                    item_id: depot_id,
                    type_id: 33_474,
                    owner_id: char_a,
                    location_id: ship_a,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
            for (mid, tid, flag) in [
                (cyno_mod_id, 21_096_u32, InventoryFlag::HiSlot0),     // Cynosural Field Generator I
                (cov_cyno_mod_id, 28_646_u32, InventoryFlag::HiSlot1), // Covert Cynosural Field Generator I
                (ind_cyno_mod_id, 52_694_u32, InventoryFlag::HiSlot2), // Industrial Cynosural Field Generator I
                (mjfg_mod_id, 4_248_u32, InventoryFlag::HiSlot3),      // Warp Disruption Field Generator II (attr 5561=1)
            ] {
                s.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: tid,
                        owner_id: char_a,
                        location_id: ship_a,
                        flag,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            }
        });
        ctx.invalidate_ship_fitting(ship_a);
        let upwell_err = rpc(&ctx, &mut sess_a, "structureDeployment", "Anchor", &[PyRep::Integer(35_832)])
            .await
            .expect_err("Upwell structure anchoring must be disallowed in Zarzakh");
        assert_eq!(upwell_err.code, ERR_ANCHORING_ZARZAKH);
        let depot_anchor_err = rpc(&ctx, &mut sess_a, "posMgr", "AnchorStructure", &[PyRep::Integer(depot_id)])
            .await
            .expect_err("Deployable anchoring must be disallowed in Zarzakh");
        assert_eq!(depot_anchor_err.code, ERR_ANCHORING_ZARZAKH);
        let depot_launch_err = rpc(
            &ctx,
            &mut sess_a,
            "ship",
            "Drop",
            &[PyRep::List(vec![PyRep::Integer(depot_id)])],
        )
        .await
        .expect_err("Launching deployables from ship bay must be disallowed in Zarzakh");
        assert_eq!(depot_launch_err.code, ERR_ANCHORING_ZARZAKH);

        // 3. Cynosural fields (standard 21096, covert 28646, industrial 52694) & hazard-banned modules (37480) prohibited
        for mid in [cyno_mod_id, cov_cyno_mod_id, ind_cyno_mod_id] {
            let cyno_err = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(mid)])
                .await
                .expect_err("Cynosural field activation must be disallowed in Zarzakh");
            assert_eq!(cyno_err.code, ERR_CYNO_ZARZAKH);
        }
        let mjfg_err = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", &[PyRep::Integer(mjfg_mod_id)])
            .await
            .expect_err("Micro Jump Field Generator (attr 5561=1) must be disallowed in Zarzakh");
        assert_eq!(mjfg_err.code, ERR_DISALLOWED_HAZARD_MOD);

        // 4. Live Horizon Siege state machine: Insurgency FOB armor reinforcement -> Warning (300s) -> Active (3600s) -> Stabilizing -> Normal
        let camp_id = 777_001_i64;
        let camp_systems = vec![TURNUR_SYS, ALSAVOINON_SYS, 30_002_538, 30_045_338, 30_002_056];
        init_insurgency_campaign(&ctx, camp_id, 500_010, 500_002, TURNUR_SYS, camp_systems.clone());
        ctx.advance_secs(48 * 3_600 + 10);
        for &sys in &camp_systems {
            let _ = add_system_suppression(&ctx, camp_id, sys, 1_000);
        }
        let fob_id = ensure_campaign_pirate_fob(&ctx, camp_id).await.expect("spawn pirate FOB");
        apply_pirate_fob_structure_damage(&ctx, char_a, fob_id, 25_000_000.0)
            .await
            .expect("Reinforcing Pirate FOB armor must schedule Zarzakh Horizon Siege Warning");
        let warning_st = get_zarzakh_horizon_siege_state(&ctx);
        assert_eq!(warning_st.phase, HorizonSiegePhase::Warning);
        assert!(!is_zarzakh_horizon_siege_active(&ctx));

        ctx.advance_secs(305);
        tick_insurgency_campaigns(&ctx, ctx.now_unix()).await;
        let active_st = get_zarzakh_horizon_siege_state(&ctx);
        assert_eq!(active_st.phase, HorizonSiegePhase::Active);
        assert!(active_st.fulcrum_restricted_to_pirates);
        assert!(is_zarzakh_horizon_siege_active(&ctx));

        let fulcrum_pos = ctx.sde.stations[&FULCRUM_STATION_ID].position;
        let mut neutral_fulcrum = setup_space_pilot(
            &ctx,
            884_508,
            char_b,
            ship_b,
            587,
            ZARZAKH_SYS,
            fulcrum_pos + DVec3::new(100.0, 0.0, 0.0),
            None,
        )
        .await;
        let fulcrum_siege_err = rpc(&ctx, &mut neutral_fulcrum, "beyonce", "CmdDock", &[PyRep::Integer(FULCRUM_STATION_ID)])
            .await
            .expect_err("Non-pirate must be refused Fulcrum docking during active Horizon Siege");
        assert_eq!(fulcrum_siege_err.code, "HorizonSiegeFulcrumLockdown");

        let reloaded = persist_and_reopen_sqlite(&ctx, "b8_horizon_siege");
        assert!(is_zarzakh_horizon_siege_active(&reloaded));

        let z_turnur_pos = reloaded.sde.jumps[&ZARZAKH_TURNUR_GATE_ID].position;
        let mut siege_exit = setup_space_pilot(
            &reloaded,
            884_510,
            char_a,
            ship_a,
            621,
            ZARZAKH_SYS,
            z_turnur_pos + DVec3::new(200.0, 0.0, 0.0),
            Some(500_010),
        )
        .await;
        let gate_siege_err = gate_jump(&reloaded, &mut siege_exit, ZARZAKH_TURNUR_GATE_ID, TURNUR_GATE_ID, ship_a)
            .await
            .expect_err("Horizon Siege must block Zarzakh stargates after SQLite reload");
        assert_eq!(gate_siege_err.code, ERR_HORIZON_SIEGE_GATE);

        reloaded
            .systems
            .get_or_spawn(ZARZAKH_SYS)
            .ballpark
            .write()
            .unwrap()
            .balls
            .get_mut(&ship_a)
            .unwrap()
            .position = DVec3::new(250_000.0, 0.0, 0.0);
        let caster_siege_err = rpc(
            &reloaded,
            &mut siege_exit,
            "shipcasterTravelMgr",
            "CmdJumpThroughShipcaster",
            &[PyRep::Integer(105_000_010), PyRep::Integer(30_002_056), PyRep::Integer(104_000_020)],
        )
        .await
        .expect_err("Horizon Siege must block Zarzakh Deathless Shipcaster after SQLite reload");
        assert_eq!(caster_siege_err.code, ERR_HORIZON_SIEGE_CASTER);

        let after_siege_ts = get_zarzakh_horizon_siege_state(&reloaded).active_until_unix + 5;
        tick_insurgency_campaigns(&reloaded, after_siege_ts).await;
        assert_eq!(get_zarzakh_horizon_siege_state(&reloaded).phase, HorizonSiegePhase::Stabilizing);
        assert!(!is_zarzakh_horizon_siege_active(&reloaded));
        let after_cooldown_ts = get_zarzakh_horizon_siege_state(&reloaded).cooldown_until_unix + 5;
        tick_insurgency_campaigns(&reloaded, after_cooldown_ts).await;
        assert_eq!(get_zarzakh_horizon_siege_state(&reloaded).phase, HorizonSiegePhase::Normal);
    }
}

pub(crate) mod ui_star_map_data_stats {
    // Integration tests closing UI, star map, neocom badges, info panels, photo mode,
    // medical clone grades, session changes, and GM tournament `feat:` parity rows.

    use crate::common;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;

    async fn select_pilot(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
    ) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        ctx.dispatch_rpc(
            &mut s,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        s
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
            .await
    }

    #[tokio::test]
    async fn test_ui_star_map_data_and_map_stats() {
        // parity: feat:ui.star-map-data, feat:ui.map-stats
        eve_testkit::attest(&["feat:ui.star-map-data", "feat:ui.map-stats"]);

        let ctx = ServerContext::new();
        let mut s = select_pilot(&ctx, 9101, "demo", "demo-secret", P1).await;
        let jita: u32 = 30_000_142;

        // Seed a Pharolux Cyno Beacon (type_id 35840) in Jita
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(
                77_000_001,
                eve_db::UpwellStructureRecord {
                    structure_id: 77_000_001,
                    type_id: 35840,
                    solar_system_id: jita,
                    owner_corp_id: 98_000_001,
                    name: "Jita Cyno Beacon".into(),
                    state: "Online".into(),
                    ..Default::default()
                },
            );
        });

        let beacons = rpc(&ctx, &mut s, "map", "GetBeaconCount", &[])
            .await
            .expect("GetBeaconCount");
        let PyRep::Dict(beacon_entries) = beacons else {
            panic!("expected Dict from GetBeaconCount");
        };
        let jita_beacons = beacon_entries
            .iter()
            .find(|(k, _)| *k == PyRep::Integer(i64::from(jita)))
            .map(|(_, v)| v.clone());
        assert_eq!(
            jita_beacons,
            Some(PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)])),
            "GetBeaconCount must report seeded Pharolux Cyno Beacon in Jita"
        );

        let incursions = rpc(&ctx, &mut s, "map", "GetIncursionGlobalReport", &[])
            .await
            .expect("GetIncursionGlobalReport");
        let PyRep::List(inc_list) = incursions else {
            panic!("expected List from GetIncursionGlobalReport");
        };
        assert!(!inc_list.is_empty(), "GetIncursionGlobalReport must return active incursions");

        let inc_systems = rpc(&ctx, &mut s, "map", "GetSystemsInIncursions", &[])
            .await
            .expect("GetSystemsInIncursions");
        let PyRep::List(inc_sys_list) = inc_systems else {
            panic!("expected List from GetSystemsInIncursions");
        };
        assert!(!inc_sys_list.is_empty(), "GetSystemsInIncursions must return incursion systems");

        let fw_zone = rpc(&ctx, &mut s, "map", "GetFacWarZoneInfo", &[PyRep::Integer(500_001)])
            .await
            .expect("GetFacWarZoneInfo");
        assert!(
            format!("{fw_zone:?}").contains("solarSystemIds"),
            "GetFacWarZoneInfo must return KeyVal with solarSystemIds"
        );

        let edencom_fort = rpc(&ctx, &mut s, "map", "GetEdencomFortressSystems", &[])
            .await
            .expect("GetEdencomFortressSystems");
        let PyRep::List(ef_list) = edencom_fort else {
            panic!("expected List from GetEdencomFortressSystems");
        };
        assert!(!ef_list.is_empty(), "GetEdencomFortressSystems must be non-empty");

        let edencom_minor = rpc(&ctx, &mut s, "map", "GetEdencomMinorVictorySystems", &[])
            .await
            .expect("GetEdencomMinorVictorySystems");
        let PyRep::List(em_list) = edencom_minor else {
            panic!("expected List from GetEdencomMinorVictorySystems");
        };
        assert!(!em_list.is_empty(), "GetEdencomMinorVictorySystems must be non-empty");

        let trig_minor = rpc(&ctx, &mut s, "map", "GetTriglavianMinorVictorySystems", &[])
            .await
            .expect("GetTriglavianMinorVictorySystems");
        let PyRep::List(tm_list) = trig_minor else {
            panic!("expected List from GetTriglavianMinorVictorySystems");
        };
        assert!(!tm_list.is_empty(), "GetTriglavianMinorVictorySystems must be non-empty");

        let deadspace_complex = rpc(&ctx, &mut s, "map", "GetDeadspaceComplexMap", &[])
            .await
            .expect("GetDeadspaceComplexMap");
        assert!(matches!(deadspace_complex, PyRep::ObjectEx { .. }));

        let deadspace_agents = rpc(&ctx, &mut s, "map", "GetDeadspaceAgentsMap", &[])
            .await
            .expect("GetDeadspaceAgentsMap");
        assert!(matches!(deadspace_agents, PyRep::ObjectEx { .. }));

        // Record jump events via map_service::record_solar_system_jump and kill events, then verify map.GetHistory(1/2/3)
        eve_server::services::map_service::record_solar_system_jump(&ctx, P1, jita);
        eve_server::services::map_service::record_solar_system_jump(&ctx, P1, jita);
        let now_u = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.record_system_kill(jita, 1, 2, 0, now_u);
        });

        let find_sys_row = |rep: PyRep, sys_id: u32| -> (i64, i64, i64) {
            let PyRep::ObjectEx { list, .. } = rep else {
                panic!("expected ObjectEx CRowset from GetHistory");
            };
            list.into_iter()
                .find_map(|r| match r {
                    PyRep::DBRow { columns, .. } => {
                        let sid = columns.iter().find_map(|(k, v)| (k == "solarSystemID").then(|| v.as_i64()).flatten())?;
                        if sid != i64::from(sys_id) {
                            return None;
                        }
                        let v1 = columns.iter().find_map(|(k, v)| (k == "value1").then(|| v.as_i64()).flatten()).unwrap_or(0);
                        let v2 = columns.iter().find_map(|(k, v)| (k == "value2").then(|| v.as_i64()).flatten()).unwrap_or(0);
                        let v3 = columns.iter().find_map(|(k, v)| (k == "value3").then(|| v.as_i64()).flatten()).unwrap_or(0);
                        Some((v1, v2, v3))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing row for solarSystemID {sys_id}"))
        };

        // stat_type 1 => jumps
        let hist_jumps = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(1)])
            .await
            .expect("GetHistory(1)");
        assert_eq!(find_sys_row(hist_jumps, jita), (2, 0, 0), "GetHistory(1) must return 2 jumps");

        // stat_type 2 => kills (ship_kills + npc_kills, npc_kills, pod_kills)
        let hist_kills = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(2)])
            .await
            .expect("GetHistory(2)");
        assert_eq!(find_sys_row(hist_kills, jita), (3, 2, 0), "GetHistory(2) must return (3, 2, 0) kills");

        // stat_type 3 => active pilots (1 after P1 jumped in Jita)
        let hist_pilots = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(3)])
            .await
            .expect("GetHistory(3)");
        assert_eq!(find_sys_row(hist_pilots, jita), (1, 0, 0), "GetHistory(3) must return 1 active pilot in Jita");
    }

    #[tokio::test]
    async fn test_ui_neocom_badges_info_panels_and_photo_mode() {
        // parity: feat:ui.neocom-badges
        eve_testkit::attest(&["feat:ui.neocom-badges"]);

        let ctx = ServerContext::new();
        let mut s = select_pilot(&ctx, 9102, "demo", "demo-secret", P1).await;
        let now_u = ctx.now_unix();

        // Seed 1 unread mail and 1 unread notification for P1
        ctx.db.with_mut_snapshot(|snap| {
            snap.mails.push(eve_db::MailRecord {
                message_id: 880_001,
                sender_id: P2,
                recipient_id: P1,
                subject: "Neocom Badge Test".into(),
                body: "Unread mail body".into(),
            });
            snap.mail_assigned_labels.insert((P1, 880_001), vec![1]);
            snap.mail_timestamps.insert(880_001, now_u);
        });
        ctx.notification_records
            .write()
            .unwrap()
            .push(eve_server::router::NotificationRecord {
                notification_id: 990_101,
                type_id: 101,
                sender_id: 1_000_125,
                receiver_id: P1,
                processed: false,
                group_id: 12,
                created: now_u as i64,
                data: "planetID: 40009082\n".into(),
                deleted: false,
                interacted: false,
            });

        // Neocom badges: unread mail count = 1, unread notifications count = 1, skill queue & free SP
        let mail_headers = rpc(&ctx, &mut s, "mailMgr", "GetMailHeaders", &[])
            .await
            .expect("GetMailHeaders");
        let PyRep::Rowset { rows: mail_rows, .. } = mail_headers else {
            panic!("expected Rowset from GetMailHeaders");
        };
        assert_eq!(mail_rows.len(), 1, "GetMailHeaders must return 1 seeded unread mail");

        let notifs = rpc(&ctx, &mut s, "notificationMgr", "GetUnprocessed", &[])
            .await
            .expect("GetUnprocessed");
        let PyRep::ObjectEx { list: notif_rows, .. } = notifs else {
            panic!("expected ObjectEx CRowset from GetUnprocessed");
        };
        assert_eq!(notif_rows.len(), 1, "GetUnprocessed must return 1 seeded unread notification");

        let skill_q = rpc(&ctx, &mut s, "skillMgr2", "GetSkillQueueAndFreePoints", &[])
            .await
            .expect("GetSkillQueueAndFreePoints");
        let PyRep::Tuple(sq_parts) = skill_q else {
            panic!("expected Tuple from GetSkillQueueAndFreePoints");
        };
        assert_eq!(sq_parts.len(), 2, "GetSkillQueueAndFreePoints must return (queue, freePoints)");

        // Info panels: mission tracker & sov
        let journal = rpc(&ctx, &mut s, "agentMgr", "GetMyJournalDetails", &[])
            .await
            .expect("GetMyJournalDetails");
        let PyRep::Tuple(j_parts) = journal else {
            panic!("expected Tuple from GetMyJournalDetails");
        };
        assert_eq!(j_parts.len(), 2, "GetMyJournalDetails must return (missions, offers)");

        let sov_info = rpc(
            &ctx,
            &mut s,
            "sovMgr",
            "GetSovStructuresInfoForSolarSystem",
            &[PyRep::Integer(30_000_142)],
        )
        .await
        .expect("GetSovStructuresInfoForSolarSystem");
        assert!(matches!(sov_info, PyRep::List(_)), "GetSovStructuresInfoForSolarSystem must return List");

        // Photo mode: station / scene state queries succeed with concrete stationID
        let station_id = s.station_id.unwrap_or(60_003_760);
        assert_eq!(station_id, 60_003_760);
        let station_info = rpc(
            &ctx,
            &mut s,
            "stationSvc",
            "GetStation",
            &[PyRep::Integer(station_id)],
        )
        .await
        .expect("GetStation");
        assert!(
            format!("{station_info:?}").contains("60003760"),
            "GetStation must return station 60003760 details"
        );
    }

    #[tokio::test]
    async fn test_removed_medical_clone_grades_and_session_changes() {
        // parity: feat:removed.medical-clone-grades, feat:serverops.session-changes
        eve_testkit::attest(&[
            "feat:removed.medical-clone-grades",
            "feat:serverops.session-changes",
        ]);

        let ctx = ServerContext::new();
        let mut s = select_pilot(&ctx, 9103, "demo", "demo-secret", P1).await;

        let isk_before = rpc(&ctx, &mut s, "account", "GetCashBalance", &[PyRep::Bool(false)])
            .await
            .expect("GetCashBalance before");

        let home_before = rpc(&ctx, &mut s, "charMgr", "GetHomeStation", &[])
            .await
            .expect("GetHomeStation");
        assert!(matches!(home_before, PyRep::Integer(_) | PyRep::Object { .. } | PyRep::Dict(_)));

        // Setting home station is free (medical clone grades removed in Rhea)
        let target_station = 60_003_760_i64;
        let set_res = rpc(
            &ctx,
            &mut s,
            "charMgr",
            "SetHomeStation",
            &[PyRep::Integer(target_station)],
        )
        .await
        .expect("SetHomeStation");
        assert!(matches!(set_res, PyRep::None | PyRep::Bool(true) | PyRep::Integer(_)));

        let isk_after = rpc(&ctx, &mut s, "account", "GetCashBalance", &[PyRep::Bool(false)])
            .await
            .expect("GetCashBalance after");
        assert_eq!(isk_before, isk_after, "Medical clone home station change must be free");

        // Trigger real session changes (Undock -> Dock) and verify OnSessionChanged payload keys & old/new values
        let extract_station_change = |notifs: &[eve_server::router::PendingMachoNotification]| -> Option<(PyRep, PyRep)> {
            notifs
                .iter()
                .filter(|n| n.event == "OnSessionChanged")
                .find_map(|n| {
                    assert_eq!(n.args.len(), 3, "OnSessionChanged must have 3 args");
                    let PyRep::Dict(changes) = &n.args[2] else {
                        return None;
                    };
                    changes.iter().find_map(|(k, v)| {
                        if *k == PyRep::String("stationid".into()) {
                            if let PyRep::Tuple(pair) = v {
                                if pair.len() == 2 {
                                    return Some((pair[0].clone(), pair[1].clone()));
                                }
                            }
                        }
                        None
                    })
                })
        };

        let _ = ctx.drain_notifications_for_char(P1);
        rpc(&ctx, &mut s, "ship", "Undock", &[])
            .await
            .expect("Undock");
        assert_eq!(s.station_id, None);
        let undock_notifs = ctx.drain_notifications_for_char(P1);
        assert_eq!(
            extract_station_change(&undock_notifs),
            Some((PyRep::Integer(target_station), PyRep::None)),
            "Undock must emit OnSessionChanged with stationid: (60003760, None)"
        );

        ctx.advance_secs(12);
        let _ = ctx.drain_notifications_for_char(P1);
        rpc(&ctx, &mut s, "beyonce", "CmdDock", &[PyRep::Integer(target_station)])
            .await
            .expect("CmdDock");
        assert_eq!(s.station_id, Some(target_station));
        let dock_notifs = ctx.drain_notifications_for_char(P1);
        assert_eq!(
            extract_station_change(&dock_notifs),
            Some((PyRep::None, PyRep::Integer(target_station))),
            "CmdDock must emit OnSessionChanged with stationid: (None, 60003760)"
        );
    }

    #[tokio::test]
    async fn test_admin_gm_tournament_lifecycle() {
        // parity: feat:admin.gm-tournament
        eve_testkit::attest(&["feat:admin.gm-tournament"]);

        let ctx = ServerContext::new();
        let mut gm = select_pilot(&ctx, 9104, "demo", "demo-secret", P1).await;
        gm.role = u64::MAX;

        let match_id = 1001_i64;
        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "CreateMatch",
            &[
                PyRep::Integer(1),
                PyRep::Tuple(vec![
                    PyRep::Integer(match_id),
                    PyRep::Integer(98_000_001),
                    PyRep::Integer(98_000_002),
                    PyRep::Integer(P1),
                    PyRep::Integer(P2),
                ]),
            ],
        )
        .await
        .expect("CreateMatch");

        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "AddPlayer",
            &[
                PyRep::Integer(match_id),
                PyRep::Integer(0),
                PyRep::Integer(P1),
            ],
        )
        .await
        .expect("AddPlayer P1");

        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "AddPlayer",
            &[
                PyRep::Integer(match_id),
                PyRep::Integer(1),
                PyRep::Integer(P2),
            ],
        )
        .await
        .expect("AddPlayer P2");

        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "BanShip",
            &[
                PyRep::Integer(match_id),
                PyRep::Integer(0),
                PyRep::Integer(587),
            ],
        )
        .await
        .expect("BanShip");

        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "FinalizeBans",
            &[PyRep::Integer(match_id)],
        )
        .await
        .expect("FinalizeBans");

        let team0 = rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "GetTeamMembers",
            &[PyRep::Integer(match_id), PyRep::Integer(0)],
        )
        .await
        .expect("GetTeamMembers 0");
        let PyRep::List(teams) = team0 else {
            panic!("expected List from GetTeamMembers, got {team0:?}");
        };
        assert_eq!(teams.len(), 2, "GetTeamMembers must return [red_list, blue_list]");
        let PyRep::List(red_members) = &teams[0] else {
            panic!("expected List for red team");
        };
        let PyRep::List(blue_members) = &teams[1] else {
            panic!("expected List for blue team");
        };
        assert_eq!(red_members.len(), 1, "red team must have 1 player (P1)");
        assert_eq!(blue_members.len(), 1, "blue team must have 1 player (P2)");

        rpc(
            &ctx,
            &mut gm,
            "tourneyMgr",
            "CleanupGrid",
            &[PyRep::Integer(match_id)],
        )
        .await
        .expect("CleanupGrid");
    }

    #[tokio::test]
    async fn test_ui_browser_lockdown_policy_and_persistence() {
        // parity: feat:ui.browser-lockdown, rpc:browserLockdownSvc.IsBrowserInLockdown, rpc:browserLockdownSvc.GetDefaultHomePage, rpc:browserLockdownSvc.GetFlaggedSitesList, rpc:browserLockdownSvc.GetFlaggedSitesHash
        eve_testkit::attest(&[
            "feat:ui.browser-lockdown",
            "rpc:browserLockdownSvc.IsBrowserInLockdown",
            "rpc:browserLockdownSvc.GetDefaultHomePage",
            "rpc:browserLockdownSvc.GetFlaggedSitesList",
            "rpc:browserLockdownSvc.GetFlaggedSitesHash",
        ]);

        let ctx = ServerContext::new();
        let mut unauth = SessionState::new(9200);
        assert!(rpc(&ctx, &mut unauth, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await.is_err());

        let mut pilot = select_pilot(&ctx, 9201, "demo", "demo-secret", P1).await;
        assert_eq!(
            rpc(&ctx, &mut pilot, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await.unwrap(),
            PyRep::Bool(false)
        );
        assert_eq!(
            rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetDefaultHomePage", &[]).await.unwrap(),
            PyRep::String("https://www.eveonline.com/".to_string())
        );
        assert_eq!(
            rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetFlaggedSitesList", &[]).await.unwrap(),
            PyRep::List(vec![])
        );
        let h0 = rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetFlaggedSitesHash", &[]).await.unwrap();

        // Persist updated policy and verify RPCs reflect it
        ctx.db.with_mut_snapshot(|snap| {
            eve_server::services::notify_emitters_skills_trade_godma::set_browser_lockdown_policy(
                snap,
                true,
                "https://forums.eveonline.com",
                vec![("https://phishing.example.test".to_string(), 2)],
            );
        });

        assert_eq!(
            rpc(&ctx, &mut pilot, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await.unwrap(),
            PyRep::Bool(true)
        );
        assert_eq!(
            rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetDefaultHomePage", &[]).await.unwrap(),
            PyRep::String("https://forums.eveonline.com".to_string())
        );
        assert_eq!(
            rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetFlaggedSitesList", &[]).await.unwrap(),
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("siteUrl".into()), PyRep::String("https://phishing.example.test".into())),
                (PyRep::String("siteFlag".into()), PyRep::Integer(2)),
            ])])
        );
        let h1 = rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetFlaggedSitesHash", &[]).await.unwrap();
        assert_ne!(h0, h1);
    }

    #[tokio::test]
    async fn test_ui_search_and_agency_content_warp() {
        // parity: feat:ui.search, rpc:custom_agency_provider.warp_to_content
        eve_testkit::attest(&[
            "feat:ui.search",
            "rpc:custom_agency_provider.warp_to_content",
        ]);

        let ctx = ServerContext::new();
        let mut s = select_pilot(&ctx, 9210, "demo", "demo-secret", P1).await;

        // 1. search.Query & search.QuickQuery
        let q_res = rpc(&ctx, &mut s, "search", "Query", &[PyRep::String("Jita".to_string())])
            .await
            .expect("search.Query");
        assert!(format!("{q_res:?}").contains("30000142"), "search.Query('Jita') must include Jita (30000142)");
        let qq_res = rpc(&ctx, &mut s, "search", "QuickQuery", &[PyRep::String("Jita".to_string())])
            .await
            .expect("search.QuickQuery");
        assert!(format!("{qq_res:?}").contains("30000142"), "search.QuickQuery('Jita') must include Jita (30000142)");

        // 2. Agency content data & warp_to_content
        let mut unauth = SessionState::new(9211);
        let unauth_err = rpc(&ctx, &mut unauth, "custom_agency_provider", "get_content_data", &[])
            .await
            .unwrap_err();
        assert!(!unauth_err.code.is_empty());

        let content = rpc(&ctx, &mut s, "custom_agency_provider", "get_content_data", &[])
            .await
            .expect("get_content_data");
        let content_dbg = format!("{content:?}");
        assert!(
            content_dbg.contains("contentID") || content_dbg.contains("solarSystemID"),
            "get_content_data must return structured agency content"
        );

        // Negative: warp_to_content without contentID fails
        let warp_err = rpc(&ctx, &mut s, "custom_agency_provider", "warp_to_content", &[])
            .await
            .unwrap_err();
        assert!(!warp_err.code.is_empty());
    }

    #[tokio::test]
    async fn test_stargate_jump_records_single_map_history_jump() {
        let ctx = ServerContext::new();
        let mut s = select_pilot(&ctx, 9230, "demo", "demo-secret", P1).await;
        rpc(&ctx, &mut s, "stationSvc", "Undock", &[]).await.expect("Undock");
        let ship_id = s.ship_id.expect("ship_id");
        let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == 30_000_142).expect("Jita gate").clone();
        ctx.systems.get_or_spawn(30_000_142).set_ball_position(ship_id, gate.position).await;
        ctx.advance_secs(11);
        let bid = ctx.bind_object(&mut s, "beyonce", 30_000_142).expect("bind beyonce");
        ctx.dispatch_rpc(&mut s, "beyonce", Some(bid), "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)], &HashMap::new(), None).await.expect("CmdStargateJump");
        let PyRep::ObjectEx { list, .. } = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(1)]).await.expect("GetHistory(1)") else { panic!("expected CRowset") };
        let jumps = list.into_iter().find_map(|r| match r {
            PyRep::DBRow { columns, .. } if columns.iter().any(|(k, v)| k == "solarSystemID" && v.as_i64() == Some(i64::from(gate.to_solar_system_id))) => {
                columns.iter().find_map(|(k, v)| (k == "value1").then(|| v.as_i64()).flatten())
            }
            _ => None,
        });
        assert_eq!(jumps, Some(1), "one CmdStargateJump must record exactly 1 jump in map.GetHistory(1)");
    }
}

pub(crate) mod serverops_session_changes_all_attributes {
    // Integration tests for `feat:serverops.session-changes`,
    // `feat:removed.medical-clone-grades`, and cluster/DB2 fail-closed guards (batch 11).


    use crate::common;
    use crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite;
    use eve_db::{corp_roles, service_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::combat::death_pipeline::execute_death_pipeline_sync;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const JITA_4_4: i64 = 60_003_760;
    const JITA_SYS: u32 = 30_000_142;

    async fn select_char(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
    ) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        ctx.dispatch_rpc(
            &mut s,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        ctx.advance_secs(12);
        s
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
            .await
    }

    fn find_session_change_tuple(
        notifs: &[eve_server::router::cache_notifications::PendingMachoNotification],
        attr_key: &str,
    ) -> Option<(PyRep, PyRep)> {
        for n in notifs.iter().filter(|n| n.event == "OnSessionChanged") {
            for arg in &n.args {
                if let PyRep::Dict(entries) = arg {
                    for (k, v) in entries {
                        if k.as_str() == Some(attr_key) {
                            if let PyRep::Tuple(pair) = v {
                                if pair.len() == 2 {
                                    return Some((pair[0].clone(), pair[1].clone()));
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    #[tokio::test]
    async fn test_serverops_session_changes_all_attributes() {
        // parity: feat:serverops.session-changes
        eve_testkit::attest(&["feat:serverops.session-changes"]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let mut p1 = select_char(&ctx, 71_001, "demo", "demo-secret", P1).await;
        let mut p2 = select_char(&ctx, 71_002, "other", "other-secret", P2).await;
        assert_eq!(p1.station_id, Some(JITA_4_4));

        // 1. Corporation attribute change (`corpid`) + 10s session-change timer
        let old_corp_id = p1.corporation_id.expect("initial corp");
        let _ = ctx.drain_notifications_for_char(P1);
        rpc(
            &ctx,
            &mut p1,
            "corpRegistry",
            "AddCorporation",
            &[
                PyRep::String("Batch11 Session Ops Corp".into()),
                PyRep::String("B11S".into()),
                PyRep::String("Session propagation verification".into()),
            ],
        )
        .await
        .expect("AddCorporation");
        let new_corp_id = p1.corporation_id.expect("new corp id");
        assert_ne!(new_corp_id, old_corp_id);

        let corp_notifs = ctx.drain_notifications_for_char(P1);
        let (old_c, new_c) =
            find_session_change_tuple(&corp_notifs, "corpid").expect("corpid OnSessionChanged");
        assert_eq!(old_c.as_i64(), Some(old_corp_id));
        assert_eq!(new_c.as_i64(), Some(new_corp_id));

        assert!(
            eve_server::services::space_destiny::check_session_change_timer_active(
                &ctx,
                P1,
                ctx.now_unix(),
                None
            )
            .is_some(),
            "session-change timer must activate on corp change"
        );
        ctx.advance_secs(15);
        assert!(
            eve_server::services::space_destiny::check_session_change_timer_active(
                &ctx,
                P1,
                ctx.now_unix(),
                None
            )
            .is_none(),
            "session-change timer must expire after 15s"
        );

        // 2. Corporation role attribute change (`corprole`) propagating to client & downstream services
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P2) {
                ch.corporation_id = new_corp_id;
            }
            if let Some(corp) = snap.corporations.get_mut(&new_corp_id) {
                corp.members.insert(P2);
                corp.member_roles.insert(P2, 0);
            }
        });
        p2.corporation_id = Some(new_corp_id);
        p2.corp_role = 0;

        // Without ROLE_STATION_MANAGER, P2 cannot create a structure profile
        assert!(
            rpc(
                &ctx,
                &mut p2,
                "structureSettings",
                "CreateProfile",
                &[
                    PyRep::String("Denied Profile".into()),
                    PyRep::String("No role yet".into()),
                ],
            )
            .await
            .is_err(),
            "member without station manager/director role must be rejected"
        );

        let _ = ctx.drain_notifications_for_char(P2);
        rpc(
            &ctx,
            &mut p1,
            "corpRegistry",
            "UpdateMember",
            &[
                PyRep::Integer(P2),
                PyRep::Integer(corp_roles::ROLE_STATION_MANAGER as i64),
            ],
        )
        .await
        .expect("UpdateMember grant ROLE_STATION_MANAGER");

        let role_notifs = ctx.drain_notifications_for_char(P2);
        let (old_r, new_r) =
            find_session_change_tuple(&role_notifs, "corprole").expect("corprole OnSessionChanged");
        assert_eq!(old_r.as_i64(), Some(0));
        assert_eq!(new_r.as_i64(), Some(corp_roles::ROLE_STATION_MANAGER as i64));

        // Next RPC syncs P2's live SessionState.corp_role and authorizes structureSettings.CreateProfile
        let prof_id = rpc(
            &ctx,
            &mut p2,
            "structureSettings",
            "CreateProfile",
            &[
                PyRep::String("Authorized Profile".into()),
                PyRep::String("Granted via live session role sync".into()),
            ],
        )
        .await
        .expect("CreateProfile after role grant")
        .as_i64()
        .expect("profile ID");
        assert!(prof_id > 0);
        assert_eq!(p2.corp_role, corp_roles::ROLE_STATION_MANAGER);

        // Revoking ROLE_STATION_MANAGER propagates immediately to P2's live session and revokes access
        let _ = ctx.drain_notifications_for_char(P2);
        rpc(
            &ctx,
            &mut p1,
            "corpRegistry",
            "UpdateMember",
            &[PyRep::Integer(P2), PyRep::Integer(0)],
        )
        .await
        .expect("UpdateMember revoke ROLE_STATION_MANAGER");
        let revoke_notifs = ctx.drain_notifications_for_char(P2);
        let (prev_r, zero_r) = find_session_change_tuple(&revoke_notifs, "corprole")
            .expect("corprole OnSessionChanged on revoke");
        assert_eq!(
            prev_r.as_i64(),
            Some(corp_roles::ROLE_STATION_MANAGER as i64)
        );
        assert_eq!(zero_r.as_i64(), Some(0));
        assert!(
            rpc(
                &ctx,
                &mut p2,
                "structureSettings",
                "CreateProfile",
                &[
                    PyRep::String("Revoked Profile".into()),
                    PyRep::String("Must fail after role revocation".into()),
                ],
            )
            .await
            .is_err(),
            "revoked role must immediately block downstream service call"
        );
        assert_eq!(p2.corp_role, 0);

        // 3. Ship attribute change (`shipid`) in station hangar via `ship.Board`
        let old_ship_id = p1.ship_id.expect("initial ship");
        let alt_ship_id = 100_911_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                alt_ship_id,
                InventoryItemRecord {
                    item_id: alt_ship_id,
                    type_id: 606, // Velator (corvette, 0 skill prereqs)
                    owner_id: P1,
                    location_id: JITA_4_4,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let _ = ctx.drain_notifications_for_char(P1);
        rpc(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(alt_ship_id)])
            .await
            .expect("ship.Board in station");
        assert_eq!(p1.ship_id, Some(alt_ship_id));
        let ship_notifs = ctx.drain_notifications_for_char(P1);
        let (old_s, new_s) =
            find_session_change_tuple(&ship_notifs, "shipid").expect("shipid OnSessionChanged");
        assert_eq!(old_s.as_i64(), Some(old_ship_id));
        assert_eq!(new_s.as_i64(), Some(alt_ship_id));
        ctx.advance_secs(15);

        // 4. Location attribute changes (`stationid` / `solarsystemid`) across Undock and Dock
        let _ = ctx.drain_notifications_for_char(P1);
        rpc(&ctx, &mut p1, "stationSvc", "Undock", &[])
            .await
            .expect("stationSvc.Undock");
        assert_eq!(p1.station_id, None);
        assert_eq!(p1.solar_system_id, Some(JITA_SYS));
        let undock_notifs = ctx.drain_notifications_for_char(P1);
        let (st_before, st_after) =
            find_session_change_tuple(&undock_notifs, "stationid").expect("Undock stationid change");
        assert_eq!(st_before.as_i64(), Some(JITA_4_4));
        assert_eq!(st_after, PyRep::None);

        let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
        assert!(
            sys_handle
                .get_ball_snapshot(alt_ship_id)
                .await
                .expect("ball query")
                .is_some(),
            "undocked ship must be spawned in solar system ballpark"
        );

        ctx.advance_secs(15);
        let _ = ctx.drain_notifications_for_char(P1);
        rpc(
            &ctx,
            &mut p1,
            "stationSvc",
            "Dock",
            &[PyRep::Integer(JITA_4_4)],
        )
        .await
        .expect("stationSvc.Dock");
        assert_eq!(p1.station_id, Some(JITA_4_4));
        let dock_notifs = ctx.drain_notifications_for_char(P1);
        let (dock_before, dock_after) =
            find_session_change_tuple(&dock_notifs, "stationid").expect("Dock stationid change");
        assert_eq!(dock_before, PyRep::None);
        assert_eq!(dock_after.as_i64(), Some(JITA_4_4));
    }

    #[tokio::test]
    async fn test_removed_medical_clone_grades_free_clone_and_zero_sp_loss() {
        // parity: feat:removed.medical-clone-grades
        eve_testkit::attest(&["feat:removed.medical-clone-grades"]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        let mut p1 = select_char(&ctx, 71_010, "demo", "demo-secret", P1).await;

        // Seed > 5,000,000 SP across skills (far above pre-Rhea 900,000 SP basic medical clone cap)
        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(P1).or_default();
            skills.insert(
                3300,
                TrainedSkillRecord {
                    type_id: 3300,
                    level: 5,
                    skill_points: 2_560_000.0,
                },
            );
            skills.insert(
                3380,
                TrainedSkillRecord {
                    type_id: 3380,
                    level: 5,
                    skill_points: 2_560_000.0,
                },
            );
            skills.insert(
                3449,
                TrainedSkillRecord {
                    type_id: 3449,
                    level: 5,
                    skill_points: 768_000.0,
                },
            );
        });

        let (bal_before, school_station_id, skills_before) = ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&P1).expect("P1");
            let school_st = ctx
                .sde
                .schools
                .get(&ch.school_id)
                .map(|s| s.station_id)
                .filter(|&id| id > 0)
                .unwrap_or(JITA_4_4);
            let sk: HashMap<u32, (u8, f64)> = snap
                .skills
                .get(&P1)
                .expect("skills")
                .iter()
                .map(|(&tid, r)| (tid, (r.level, r.skill_points)))
                .collect();
            (ch.balance.0, school_st, sk)
        });
        let total_sp_before: f64 = skills_before.values().map(|(_, sp)| *sp).sum();
        assert!(total_sp_before > 5_000_000.0);

        // 1. SetHomeStation to school station is free (0 ISK, no medical clone grade fee)
        let _ = ctx.drain_notifications_for_char(P1);
        let set_res = rpc(
            &ctx,
            &mut p1,
            "charMgr",
            "SetHomeStation",
            &[PyRep::Integer(school_station_id)],
        )
        .await
        .expect("SetHomeStation");
        assert_eq!(set_res.as_i64(), Some(school_station_id));
        let home = rpc(&ctx, &mut p1, "charMgr", "GetHomeStation", &[])
            .await
            .expect("GetHomeStation");
        assert_eq!(home.as_i64(), Some(school_station_id));

        let bal_after_set = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&P1).expect("P1").balance.0);
        assert_eq!(
            bal_before, bal_after_set,
            "medical clone home station registration must charge 0 ISK"
        );

        // 2. Undock, lose ship -> capsule, then lose capsule (pod death) -> respawn at home_station_id with 0 SP loss
        rpc(&ctx, &mut p1, "stationSvc", "Undock", &[])
            .await
            .expect("Undock");
        let ship_id = p1.ship_id.expect("active ship");
        let ship_death = execute_death_pipeline_sync(
            &ctx,
            JITA_SYS,
            P1,
            ship_id,
            P2,
            DVec3::new(1_000.0, 0.0, 0.0),
            42,
        )
        .expect("ship destruction");
        let capsule_id = ship_death.ejected_capsule_id.expect("ejected capsule");

        let _pod_death = execute_death_pipeline_sync(
            &ctx,
            JITA_SYS,
            P1,
            capsule_id,
            P2,
            DVec3::new(1_000.0, 0.0, 0.0),
            43,
        )
        .expect("pod destruction");

        let (respawn_station, bal_after_pod, skills_after) = ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&P1).expect("P1 after pod death");
            let sk: HashMap<u32, (u8, f64)> = snap
                .skills
                .get(&P1)
                .expect("skills after pod death")
                .iter()
                .map(|(&tid, r)| (tid, (r.level, r.skill_points)))
                .collect();
            (ch.station_id, ch.balance.0, sk)
        });
        assert_eq!(
            respawn_station,
            Some(school_station_id),
            "pod destruction must respawn pilot at home_station_id"
        );
        assert_eq!(
            skills_before, skills_after,
            "Rhea medical clone rules: pod destruction must cause zero skill-point loss"
        );
        assert!(
            bal_after_pod >= bal_before,
            "pod destruction must never deduct a medical clone upgrade fee"
        );

        // 3. Reload from SQLite and verify home station and zero-SP-loss skill state persist
        let ctx_reloaded = persist_and_reopen_sqlite(&ctx, "b11_medical_clone_reload");
        let mut p1_re = select_char(&ctx_reloaded, 71_011, "demo", "demo-secret", P1).await;
        let home_reloaded = rpc(&ctx_reloaded, &mut p1_re, "charMgr", "GetHomeStation", &[])
            .await
            .expect("GetHomeStation after reload");
        assert_eq!(home_reloaded.as_i64(), Some(school_station_id));
        let skills_reloaded: HashMap<u32, (u8, f64)> = ctx_reloaded.db.with_snapshot(|snap| {
            snap.skills
                .get(&P1)
                .expect("reloaded skills")
                .iter()
                .map(|(&tid, r)| (tid, (r.level, r.skill_points)))
                .collect()
        });
        assert_eq!(skills_before, skills_reloaded);
    }

    #[tokio::test]
    async fn test_cluster_admin_and_db2_fail_closed_guards() {
        let ctx = ServerContext::new();
        let mut admin = select_char(&ctx, 71_020, "demo", "demo-secret", P1).await;
        let mut unpriv = select_char(&ctx, 71_021, "other", "other-secret", P2).await;

        // Unprivileged sessions must be rejected on all cluster/QA/admin RPCs
        for (svc, method, args) in [
            ("loadService", "StartLoad", vec![]),
            ("loadService", "StopLoad", vec![]),
            ("loadService", "GetTotalStats", vec![]),
            ("sessionMgr", "EndAllGameSessions", vec![]),
            ("raffleMgr", "QA_SeedRaffles", vec![PyRep::Integer(1)]),
            (
                "DB2",
                "CallProc",
                vec![PyRep::String("zcluster.Nodes_PolarisID".into())],
            ),
            ("debug", "Exec", vec![PyRep::String("status".into())]),
            ("localizationServer", "ReloadFSDPickle", vec![]),
            ("localizationServer", "UpdateLocalizationQAWrap", vec![]),
        ] {
            assert!(
                rpc(&ctx, &mut unpriv, svc, method, &args).await.is_err(),
                "{svc}.{method} must reject unprivileged player session"
            );
        }

        admin.role |= service_roles::ROLE_GM_DEFAULT | eve_machonet::service_roles::ROLE_PROGRAMMER;

        let polaris = rpc(
            &ctx,
            &mut admin,
            "DB2",
            "CallProc",
            &[PyRep::String("zcluster.Nodes_PolarisID".into())],
        )
        .await
        .expect("known DB2.CallProc");
        assert_eq!(polaris, PyRep::Integer(1));

        // Unknown stored procedures must fail closed instead of silently returning None
        let unknown_proc = rpc(
            &ctx,
            &mut admin,
            "DB2",
            "CallProc",
            &[PyRep::String("zcluster.NonExistentProc".into())],
        )
        .await
        .expect_err("unknown DB2.CallProc procedure must fail closed");
        assert_eq!(unknown_proc.message, "InvalidProcedure");
    }
}

pub(crate) mod invariant_atomic_rollback_randomized_fault {
    // Integration tests for `feat:invariant.atomic-rollback-all-stores` and
    // Quasar `plex` / `character` / `user` gateway domains (batch 11 part 2).


    use crate::common;
    use crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite;
    use eve_db::{InventoryFlag, IskCents, MiningLedgerRecord, WorldRepository};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
        EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
        QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::quasar_gateway::{
        STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
    };
    use eve_server::router::ServerContext;
    use eve_server::services::quasar_auth_user::{
        activate_user_omega_license, expire_user_omega_license,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::mpsc::Receiver;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const JITA_4_4: i64 = 60_003_760;

    pub(crate) fn temp_sqlite_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("evomu_{tag}_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
        p
    }

    pub(crate) fn cleanup_sqlite_path(p: &PathBuf) {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
    }

    pub(crate) async fn select_char(
        ctx: &ServerContext,
        sid: u64,
        user: &str,
        pass: &str,
        char_id: i64,
    ) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        ctx.dispatch_rpc(
            &mut s,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        ctx.advance_secs(12);
        s
    }

    pub(crate) async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
            .await
    }

    pub(crate) async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_url: &str,
        payload_value: Vec<u8>,
    ) -> EvePublicResponse {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: format!("b11-q-{n:08}").into_bytes(),
            payload: Some(GoogleAny {
                type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                    type_url.to_string()
                } else {
                    format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
                },
                value: payload_value,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-b11-0001".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let rep = rpc(
            ctx,
            sess,
            "publicGatewaySvc",
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
        )
        .await
        .expect("publicGatewaySvc.SendRequest");
        if let PyRep::KeyVal(entries) = &rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                    }
                }
            }
        }
        panic!("expected response_proto KeyVal, got {rep:?}");
    }

    fn flush_and_drain_notices(
        ctx: &ServerContext,
        rx: &mut Receiver<EvePublicNotice>,
    ) -> Vec<EvePublicNotice> {
        let queued: Vec<_> = ctx
            .quasar_gateway
            .pending_notices
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        for (scope, payload) in queued {
            let _ = ctx
                .quasar_gateway
                .publish_notice(scope, payload, ctx.now_unix());
        }
        let mut out = Vec::new();
        for _ in 0..512 {
            match rx.try_recv() {
                Ok(msg) => out.push(msg),
                Err(_) => break,
            }
        }
        out
    }

    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct ObservableWorldState {
        p1_balance: i64,
        p2_balance: i64,
        p1_corp_id: i64,
        corp_count: usize,
        item_count: usize,
        journal_len: usize,
        mining_ledger_len: usize,
        mining_total_qty: i64,
        custom_names: Vec<(i64, String)>,
        contacts: Vec<(i64, i64)>,
        blobs_digest: Vec<(String, String)>,
        esi_ship_name: Option<String>,
    }

    pub(crate) fn capture_observable_state(ctx: &ServerContext) -> ObservableWorldState {
        let esi_ship_name = ctx.esi_state.get_custom_item_name(100_000_001);
        ctx.db.with_snapshot(|snap| {
            let mut custom_names: Vec<(i64, String)> = snap
                .item_custom_names
                .iter()
                .filter(|(&id, _)| id > 0)
                .map(|(&id, v)| (id, v.clone()))
                .collect();
            custom_names.sort_by_key(|(id, _)| *id);

            let mut contacts: Vec<(i64, i64)> = snap
                .char_contacts
                .get(&P1)
                .map(|list| {
                    list.iter()
                        .map(|c| (c.contact_id, (c.standing * 100.0) as i64))
                        .collect()
                })
                .unwrap_or_default();
            contacts.sort_by_key(|(id, _)| *id);

            let mut blobs_digest: Vec<(String, String)> = snap
                .service_state_blobs
                .iter()
                .filter(|(k, _)| !k.starts_with("season_manager:") && *k != "market_seeder_version")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            blobs_digest.sort_by(|a, b| a.0.cmp(&b.0));

            ObservableWorldState {
                p1_balance: snap.characters.get(&P1).map(|c| c.balance.0).unwrap_or(0),
                p2_balance: snap.characters.get(&P2).map(|c| c.balance.0).unwrap_or(0),
                p1_corp_id: snap
                    .characters
                    .get(&P1)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0),
                corp_count: snap.corporations.len(),
                item_count: snap.items.len(),
                journal_len: snap.wallet_journal.len(),
                mining_ledger_len: snap.mining_ledger.len(),
                mining_total_qty: snap.mining_ledger.iter().map(|e| e.quantity).sum(),
                custom_names,
                contacts,
                blobs_digest,
                esi_ship_name,
            }
        })
    }

    #[tokio::test]
    async fn test_invariant_atomic_rollback_randomized_fault_injection() {
        let db_path = temp_sqlite_path("b11_atomic_rollback");
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
        let ctx = Arc::new(ServerContext::with_repository(repo));
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| *snap = demo);
        common::pin_clock(&ctx);

        let mut p1 = select_char(&ctx, 72_010, "demo", "demo-secret", P1).await;
        let mut rng_state: u64 = 0xB11A_701C_5EED_0042;

        for step in 0..40u64 {
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            let op = rng_state % 8;
            let pre = capture_observable_state(&ctx);

            match op {
                0 => {
                    rpc(
                        &ctx,
                        &mut p1,
                        "account",
                        "GiveCash",
                        &[
                            PyRep::Integer(P2),
                            PyRep::Real(100.0),
                            PyRep::String(format!("step-{step}")),
                        ],
                    )
                    .await
                    .expect("valid GiveCash");
                    let post = capture_observable_state(&ctx);
                    assert_eq!(post.p1_balance, pre.p1_balance - 10_000);
                    assert_eq!(post.p2_balance, pre.p2_balance + 10_000);
                }
                1 => {
                    let name = format!("Ship-{step}");
                    rpc(
                        &ctx,
                        &mut p1,
                        "invbroker",
                        "SetLabel",
                        &[PyRep::Integer(100_000_001), PyRep::String(name.clone())],
                    )
                    .await
                    .expect("valid SetLabel");
                    assert_eq!(capture_observable_state(&ctx).esi_ship_name, Some(name));
                }
                2 => {
                    assert!(
                        rpc(
                            &ctx,
                            &mut p1,
                            "account",
                            "GiveCash",
                            &[
                                PyRep::Integer(P2),
                                PyRep::Real(999_999_999_999.0),
                                PyRep::String("overdraft".into()),
                            ],
                        )
                        .await
                        .is_err()
                    );
                    assert_eq!(capture_observable_state(&ctx), pre);
                }
                3 => {
                    assert!(
                        rpc(
                            &ctx,
                            &mut p1,
                            "corpRegistry",
                            "AddCorporation",
                            &[
                                PyRep::String("".into()),
                                PyRep::String("BAD".into()),
                                PyRep::String("invalid".into()),
                            ],
                        )
                        .await
                        .is_err()
                    );
                    assert_eq!(capture_observable_state(&ctx), pre);
                }
                4 => {
                    assert!(
                        rpc(
                            &ctx,
                            &mut p1,
                            "charMgr",
                            "AddContact",
                            &[PyRep::Integer(P1), PyRep::Real(5.0)],
                        )
                        .await
                        .is_err()
                    );
                    assert_eq!(capture_observable_state(&ctx), pre);
                }
                5 => {
                    assert!(
                        rpc(
                            &ctx,
                            &mut p1,
                            "invbroker",
                            "SetLabel",
                            &[
                                PyRep::Integer(100_000_002),
                                PyRep::String("Hijacked".into()),
                            ],
                        )
                        .await
                        .is_err()
                    );
                    assert_eq!(capture_observable_state(&ctx), pre);
                }
                6 => {
                    let err_res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&P1) {
                            ch.balance = IskCents(ch.balance.0 - 500_000);
                        }
                        snap.mining_ledger.push(MiningLedgerRecord {
                            character_id: P1,
                            solar_system_id: 30_000_142,
                            type_id: 1230,
                            quantity: 7777,
                            date: "2026-10-31".into(),
                            timestamp_unix: ctx.now_unix(),
                        });
                        snap.service_state_blobs
                            .insert(format!("fault_blob_{step}"), "partial".into());
                        Err(BridgeRpcError {
                            code: "InjectedHandlerFault".into(),
                            message: "Simulated mid-transaction failure".into(),
                        })
                    });
                    assert!(err_res.is_err());
                    assert_eq!(capture_observable_state(&ctx), pre);
                }
                _ => {
                    ctx.db.with_mut_snapshot(|snap| {
                        snap.mining_ledger.push(MiningLedgerRecord {
                            character_id: P1,
                            solar_system_id: 30_000_142,
                            type_id: 1230,
                            quantity: 50,
                            date: "2026-10-31".into(),
                            timestamp_unix: ctx.now_unix(),
                        });
                    });
                    assert_eq!(
                        capture_observable_state(&ctx).mining_ledger_len,
                        pre.mining_ledger_len + 1
                    );
                }
            }
        }

        // Persistence-layer fault injection via SQLite BEFORE UPDATE / BEFORE INSERT triggers
        {
            let raw_conn = rusqlite::Connection::open(&db_path).expect("open sqlite for trigger");
            raw_conn
                .execute_batch(
                    "CREATE TRIGGER fail_char_persist BEFORE UPDATE ON characters \
                     BEGIN SELECT RAISE(ABORT, 'injected_disk_persist_failure'); END; \
                     CREATE TRIGGER fail_contact_ins BEFORE INSERT ON char_contacts \
                     BEGIN SELECT RAISE(ABORT, 'injected_contact_persist_failure'); END; \
                     CREATE TRIGGER fail_contact_upd BEFORE UPDATE ON char_contacts \
                     BEGIN SELECT RAISE(ABORT, 'injected_contact_persist_failure'); END; \
                     CREATE TRIGGER fail_label_ins BEFORE INSERT ON item_custom_names \
                     BEGIN SELECT RAISE(ABORT, 'injected_label_persist_failure'); END; \
                     CREATE TRIGGER fail_label_upd BEFORE UPDATE ON item_custom_names \
                     BEGIN SELECT RAISE(ABORT, 'injected_label_persist_failure'); END;",
                )
                .expect("install fault trigger");
        }
        let pre_persist_fault = capture_observable_state(&ctx);

        // 1. Dispatch real mutating RPCs while SQLite triggers reject persistence commits
        let fault_cash = rpc(
            &ctx,
            &mut p1,
            "account",
            "GiveCash",
            &[PyRep::Integer(P2), PyRep::Real(5_000.0), PyRep::String("fault_cash".into())],
        )
        .await;
        assert!(fault_cash.is_err());
        assert_eq!(capture_observable_state(&ctx), pre_persist_fault);

        let fault_contact = rpc(
            &ctx,
            &mut p1,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(90_000_003), PyRep::Real(10.0), PyRep::Bool(true)],
        )
        .await;
        assert!(fault_contact.is_err());
        assert_eq!(capture_observable_state(&ctx), pre_persist_fault);

        let fault_label = rpc(
            &ctx,
            &mut p1,
            "invbroker",
            "SetLabel",
            &[PyRep::Integer(100_000_001), PyRep::String("Uncommitted Ship Name".into())],
        )
        .await;
        assert!(fault_label.is_err());
        assert_eq!(capture_observable_state(&ctx), pre_persist_fault);

        // 2. Direct multi-store snapshot transaction fault injection
        let persist_fault_res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.balance = IskCents(ch.balance.0 - 250_000);
            }
            snap.mining_ledger.push(MiningLedgerRecord {
                character_id: P1,
                solar_system_id: 30_000_142,
                type_id: 1228,
                quantity: 9_999,
                date: "2026-10-31".into(),
                timestamp_unix: ctx.now_unix(),
            });
            snap.service_state_blobs
                .insert("uncommitted_blob".into(), "must_rollback".into());
            Ok(())
        });
        assert!(persist_fault_res.is_err());
        assert_eq!(capture_observable_state(&ctx), pre_persist_fault);
        {
            let raw_conn = rusqlite::Connection::open(&db_path).expect("open sqlite to drop trigger");
            raw_conn
                .execute_batch(
                    "DROP TRIGGER fail_char_persist; \
                     DROP TRIGGER fail_contact_ins; \
                     DROP TRIGGER fail_contact_upd; \
                     DROP TRIGGER fail_label_ins; \
                     DROP TRIGGER fail_label_upd;",
                )
                .expect("drop fault trigger");
        }

        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
        assert!(repo2.with_snapshot(|s| !s.service_state_blobs.contains_key("uncommitted_blob")));
        let ctx2 = ServerContext::with_repository(repo2);
        assert_eq!(capture_observable_state(&ctx2), pre_persist_fault);

        cleanup_sqlite_path(&db_path);
    }

    #[tokio::test]
    async fn test_quasar_plex_character_and_user_gateway_domains() {
        // Hardened Quasar PLEX, character, and user flows (reopened pending full sub-feature coverage)

        let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let mut unauth = SessionState::new(72_040);
        for url in [
            "eve_public.plex.vault.api.BalanceRequest",
            "eve_public.character.GetNamesByUserRequest",
            "eve_public.user.license.api.GetRequest",
        ] {
            assert_eq!(
                quasar_rpc(&ctx, &mut unauth, url, vec![]).await.status_code,
                STATUS_UNAUTHORIZED
            );
        }

        let mut p1 = select_char(&ctx, 72_041, "demo", "demo-secret", P1).await;
        let mut p2 = select_char(&ctx, 72_042, "other", "other-secret", P2).await;
        let mut rx1 = ctx
            .quasar_gateway
            .subscribe_session(&p1, vec![], ctx.now_unix());

        // 1. feat:quasar.plex
        assert_eq!(
            quasar_rpc(&ctx, &mut p1, "eve_public.plex.vault.api.BalanceRequest", vec![])
                .await
                .status_code,
            STATUS_OK
        );
        let plex_item = ctx
            .db
            .add_or_stack_item(P1, JITA_4_4, InventoryFlag::Hangar, 44_992, 40)
            .expect("seed plex")
            .item_id;
        rpc(
            &ctx,
            &mut p1,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_4_4),
                PyRep::Integer(plex_item),
                PyRep::Integer(40),
            ],
        )
        .await
        .expect("DepositPlexToVault");
        let plex_notices = flush_and_drain_notices(&ctx, &mut rx1);
        assert!(plex_notices.iter().any(|n| {
            n.payload
                .as_ref()
                .is_some_and(|p| p.type_url.contains("plex.vault.api.BalanceChangedNotice"))
        }));

        assert_eq!(
            quasar_rpc(
                &ctx,
                &mut p1,
                "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest",
                vec![]
            )
            .await
            .status_code,
            STATUS_OK
        );
        let tx_id = ctx.db.with_snapshot(|snap| {
            eve_server::services::quasar_payment_store::get_plex_transactions(snap)
                .into_iter()
                .find(|t| t.user_id == 1)
                .expect("user 1 plex tx")
                .id
        });
        let (mut b_id, mut log_req) = (Vec::new(), Vec::new());
        encode_varint_field(1, tx_id, &mut b_id);
        encode_bytes_field(1, &b_id, &mut log_req);
        assert_eq!(
            quasar_rpc(
                &ctx,
                &mut p1,
                "eve_public.plex.vault.transaction.api.GetLogRequest",
                log_req.clone()
            )
            .await
            .status_code,
            STATUS_OK
        );
        let log_url = "eve_public.plex.vault.transaction.api.GetLogRequest";
        assert_eq!(quasar_rpc(&ctx, &mut p2, log_url, log_req.clone()).await.status_code, STATUS_NOT_FOUND);

        // 2. feat:quasar.character
        let names_rsp = quasar_rpc(&ctx, &mut p1, "eve_public.character.GetNamesByUserRequest", vec![]).await;
        assert_eq!(names_rsp.status_code, STATUS_OK);
        assert!(!names_rsp.payload.expect("names").value.is_empty());
        let sp_create_url = "eve_public.character.skill.plan.CreateRequest";
        assert_eq!(quasar_rpc(&ctx, &mut p1, sp_create_url, vec![]).await.status_code, STATUS_BAD_REQUEST);

        let (mut attr_b, mut req_b, mut tid_b, mut create_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_string_field(1, "B11 Gunnery Plan", &mut attr_b);
        encode_varint_field(1, 3300, &mut tid_b);
        encode_bytes_field(1, &tid_b, &mut req_b);
        encode_varint_field(2, 4, &mut req_b);
        encode_bytes_field(2, &req_b, &mut attr_b);
        encode_string_field(3, "Gunnery IV milestone plan", &mut attr_b);
        encode_bytes_field(1, &attr_b, &mut create_b);

        let sp_create = quasar_rpc(&ctx, &mut p1, sp_create_url, create_b).await;
        assert_eq!(sp_create.status_code, STATUS_OK);
        let sp_wm = WireMessage::decode(&sp_create.payload.expect("sp create").value).expect("wm");
        let id_b = sp_wm.fields.iter().find(|f| f.field_number == 1).expect("id").value.as_bytes().expect("bytes");
        let sp_id_wm = WireMessage::decode(id_b).expect("id wm");
        let uuid_b = sp_id_wm.fields.iter().find(|f| f.field_number == 1).expect("uuid").value.as_bytes().expect("uuid bytes");
        let plan_uuid: [u8; 16] = uuid_b.try_into().expect("16b uuid");
        let (mut id_inner, mut plan_id_req) = (Vec::new(), Vec::new());
        encode_bytes_field(1, &plan_uuid, &mut id_inner);
        encode_bytes_field(1, &id_inner, &mut plan_id_req);

        let sp_active_url = "eve_public.character.skill.plan.SetActiveRequest";
        let sp_ms_url = "eve_public.character.skill.plan.milestone.GetAllRequest";
        assert_eq!(quasar_rpc(&ctx, &mut p1, sp_active_url, plan_id_req.clone()).await.status_code, STATUS_OK);
        assert_eq!(quasar_rpc(&ctx, &mut p1, sp_ms_url, plan_id_req.clone()).await.status_code, STATUS_OK);

        // 3. feat:quasar.user
        let now = ctx.now_unix();
        let expected_exp = (now + 30 * 86_400).to_string();
        activate_user_omega_license(&ctx.db, &ctx.quasar_gateway.pending_notices, 1, 30 * 86_400, now);
        let lic_notices = flush_and_drain_notices(&ctx, &mut rx1);
        assert!(lic_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("user.license.api.ActivatedNotice"))));
        let lic_url = "eve_public.user.license.api.GetRequest";
        let lic_active = quasar_rpc(&ctx, &mut p1, lic_url, vec![]).await;
        assert_eq!(lic_active.status_code, STATUS_OK);
        let lic_dyn = reg.decode_dynamic("eve_public.user.license.api.GetResponse", &lic_active.payload.expect("license").value).expect("decode");
        assert!(format!("{lic_dyn:?}").contains(&expected_exp));

        let (mut doc_id, mut accept_req) = (Vec::new(), Vec::new());
        encode_string_field(1, "privacy_policy_2026", &mut doc_id);
        encode_bytes_field(3, &doc_id, &mut accept_req);
        assert_eq!(
            quasar_rpc(&ctx, &mut p1, "eve.user.legal.agreement.api.AcceptRequest", accept_req)
                .await
                .status_code,
            STATUS_OK
        );
        let latest_eula = quasar_rpc(&ctx, &mut p1, "eve.legal.agreement.api.GetLatestRequest", vec![]).await;
        assert_eq!(latest_eula.status_code, STATUS_OK);
        let eula_wm = WireMessage::decode(&latest_eula.payload.expect("latest eula").value).expect("eula wm");
        let eula_doc = WireMessage::decode(
            eula_wm.fields.iter().find(|f| f.field_number == 1).expect("doc").value.as_bytes().expect("bytes"),
        )
        .expect("doc wm");
        let eula_id = std::str::from_utf8(
            eula_doc.fields.iter().find(|f| f.field_number == 2).expect("field 2").value.as_bytes().expect("bytes"),
        )
        .expect("utf8");
        assert_eq!(eula_id, "privacy_policy_2026");

        let ctx_reloaded = persist_and_reopen_sqlite(&ctx, "b11_quasar_domains_reload");
        let mut p1_re = select_char(&ctx_reloaded, 72_043, "demo", "demo-secret", P1).await;
        assert_eq!(quasar_rpc(&ctx_reloaded, &mut p1_re, log_url, log_req).await.status_code, STATUS_OK);
        let sp_get_url = "eve_public.character.skill.plan.GetRequest";
        assert_eq!(quasar_rpc(&ctx_reloaded, &mut p1_re, sp_get_url, plan_id_req).await.status_code, STATUS_OK);
        let lic_reloaded = quasar_rpc(&ctx_reloaded, &mut p1_re, lic_url, vec![]).await;
        assert_eq!(lic_reloaded.status_code, STATUS_OK);
        let lic_re_dyn = reg.decode_dynamic("eve_public.user.license.api.GetResponse", &lic_reloaded.payload.expect("reloaded").value).expect("decode");
        assert!(format!("{lic_re_dyn:?}").contains(&expected_exp));

        let mut rx_re = ctx_reloaded.quasar_gateway.subscribe_session(&p1_re, vec![], ctx_reloaded.now_unix());
        expire_user_omega_license(&ctx_reloaded.db, &ctx_reloaded.quasar_gateway.pending_notices, 1);
        let exp_notices = flush_and_drain_notices(&ctx_reloaded, &mut rx_re);
        assert!(exp_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("user.license.api.ExpiredNotice"))));
        assert_eq!(ctx_reloaded.db.with_snapshot(|s| s.characters.get(&P1).expect("P1").clone_grade), 1);
    }
}

pub(crate) mod invariant_single_source_of_truth {
    // Integration test for `feat:invariant.single-source-of-truth`:
    // Contacts, mail labels & deleted mail, custom item names, CSPA charges, war records,
    // alliances, and PLEX balances read identically across MachoNet client RPC, ESI, and
    // Quasar live and after SQLite restart (batch 11 part 3).


    use crate::common;
    use crate::gameplay_services_and_rpcs::invariant_atomic_rollback_randomized_fault::{
        cleanup_sqlite_path, quasar_rpc, rpc, select_char, temp_sqlite_path,
    };
    use eve_db::{
        AllianceLifecycleMeta, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
        IskCents, WarRecord, WorldRepository,
    };
    use eve_machonet::SessionState;
    use eve_proto::quasar::{encode_bytes_field, encode_varint_field, QuasarDescriptorRegistry};
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use eve_server::quasar_gateway::STATUS_OK;
    use eve_server::router::ServerContext;
    use std::sync::Arc;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const JITA_4_4: i64 = 60_003_760;

    const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn issue_token(
        ctx: &ServerContext,
        user: &str,
        pass: &str,
        char_id: i64,
        scopes: &[&str],
    ) -> String {
        let now = ctx.now_unix();
        let code = ctx
            .db
            .authorize_pkce(
                user,
                pass,
                char_id,
                "evomu-b11-ssot",
                "https://localhost/callback",
                scopes.iter().map(|s| s.to_string()).collect(),
                PKCE_CHALLENGE,
                "S256",
                now,
            )
            .expect("authorize_pkce");
        ctx.db
            .exchange_pkce_code(&code, "evomu-b11-ssot", PKCE_VERIFIER, now)
            .expect("exchange_pkce_code")
            .access_token
    }

    fn esi_call(
        router: &EsiRouter,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Vec<u8>>,
    ) -> serde_json::Value {
        let mut req = EsiHttpRequest::new(method, path);
        if let Some(t) = token {
            req = req.with_bearer_token(t);
        }
        if let Some(b) = body {
            req.body = b;
        }
        let resp = router.handle_request(&req);
        assert!(
            matches!(resp.status_code, 200 | 201),
            "ESI {method} {path} failed ({}): {}",
            resp.status_code,
            String::from_utf8_lossy(&resp.body)
        );
        serde_json::from_slice(&resp.body).expect("valid ESI JSON")
    }

    #[tokio::test]
    async fn test_invariant_single_source_of_truth_client_esi_quasar_and_restart() {
        let db_path = temp_sqlite_path("b11_ssot");
        let corp_id = 98_200_001i64;
        let rival_corp_id = 98_200_002i64;
        let alliance_id = 99_200_001i64;
        let war_id = 88_001i64;
        let ship_id = 100_000_001i64;
        let plex_stack_id = 100_777_001i64;

        let (mail_deleted, mail_kept, mail_label_id) = {
            let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 1"));
            let ctx = Arc::new(ServerContext::with_repository(repo));
            let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
            ctx.db.with_mut_snapshot(|snap| {
                *snap = demo;
                snap.corporations.insert(
                    corp_id,
                    CorporationRecord {
                        corporation_id: corp_id,
                        name: "B11 SSOT Corp".into(),
                        ticker: "BSOT".into(),
                        ceo_id: P1,
                        alliance_id: Some(alliance_id),
                        hq_station_id: Some(JITA_4_4),
                        members: [P1].into_iter().collect(),
                        ..Default::default()
                    },
                );
                snap.corporations.insert(
                    rival_corp_id,
                    CorporationRecord {
                        corporation_id: rival_corp_id,
                        name: "B11 Rival Corp".into(),
                        ticker: "BRVL".into(),
                        ceo_id: P2,
                        hq_station_id: Some(JITA_4_4),
                        members: [P2].into_iter().collect(),
                        ..Default::default()
                    },
                );
                snap.alliances.insert(
                    alliance_id,
                    AllianceRecord {
                        alliance_id,
                        name: "B11 Unified Alliance".into(),
                        short_name: "B11UA".into(),
                        executor_corp_id: corp_id,
                        member_corp_ids: [corp_id].into_iter().collect(),
                    },
                );
                snap.alliance_lifecycle_meta.insert(
                    alliance_id,
                    AllianceLifecycleMeta {
                        creator_id: P1,
                        creator_corporation_id: corp_id,
                        date_founded_unix: 1_700_000_000,
                        faction_id: None,
                    },
                );
                snap.wars.insert(
                    war_id,
                    WarRecord {
                        war_id,
                        aggressor_corp_id: corp_id,
                        defender_corp_id: rival_corp_id,
                        mutual: false,
                        started_unix: 1_700_000_000,
                        active: true,
                    },
                );
                if let Some(c1) = snap.characters.get_mut(&P1) {
                    c1.corporation_id = corp_id;
                    c1.alliance_id = Some(alliance_id);
                    c1.balance = IskCents(50_000_000_00);
                }
                if let Some(c2) = snap.characters.get_mut(&P2) {
                    c2.corporation_id = rival_corp_id;
                }
                snap.items.insert(
                    plex_stack_id,
                    InventoryItemRecord {
                        item_id: plex_stack_id,
                        type_id: 44_992,
                        owner_id: P1,
                        location_id: JITA_4_4,
                        flag: InventoryFlag::Hangar,
                        quantity: 200,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
            });
            common::pin_clock(&ctx);

            let mut s1 = select_char(&ctx, 72_020, "demo", "demo-secret", P1).await;
            let mut s2 = select_char(&ctx, 72_021, "other", "other-secret", P2).await;

            rpc(
                &ctx,
                &mut s1,
                "account",
                "SetContactCost",
                &[PyRep::Integer(25_000)],
            )
            .await
            .expect("P1 SetContactCost");
            rpc(
                &ctx,
                &mut s2,
                "account",
                "SetContactCost",
                &[PyRep::Integer(12_500)],
            )
            .await
            .expect("P2 SetContactCost");
            rpc(
                &ctx,
                &mut s1,
                "charMgr",
                "AddContact",
                &[PyRep::Integer(P2), PyRep::Real(8.0), PyRep::Bool(true)],
            )
            .await
            .expect("AddContact");

            let lbl_id = rpc(
                &ctx,
                &mut s1,
                "mailMgr",
                "CreateLabel",
                &[
                    PyRep::String("PriorityOps".into()),
                    PyRep::String("#ffaa00".into()),
                ],
            )
            .await
            .expect("mailMgr.CreateLabel")
            .as_i64()
            .expect("mail label id");
            let m_del = rpc(
                &ctx,
                &mut s1,
                "mailMgr",
                "SendMail",
                &[
                    PyRep::List(vec![PyRep::Integer(P1)]),
                    PyRep::String("Purged Dispatch".into()),
                    PyRep::String("Delete me".into()),
                    PyRep::Integer(0),
                ],
            )
            .await
            .expect("SendMail 1")
            .as_i64()
            .expect("m_del");
            let m_keep = rpc(
                &ctx,
                &mut s1,
                "mailMgr",
                "SendMail",
                &[
                    PyRep::List(vec![PyRep::Integer(P1)]),
                    PyRep::String("Retained Dispatch".into()),
                    PyRep::String("Keep me".into()),
                    PyRep::Integer(0),
                ],
            )
            .await
            .expect("SendMail 2")
            .as_i64()
            .expect("m_keep");
            rpc(
                &ctx,
                &mut s1,
                "mailMgr",
                "DeleteMail",
                &[PyRep::List(vec![PyRep::Integer(m_del)])],
            )
            .await
            .expect("DeleteMail");

            rpc(
                &ctx,
                &mut s1,
                "invbroker",
                "SetLabel",
                &[
                    PyRep::Integer(ship_id),
                    PyRep::String("B11 Sovereign Hull".into()),
                ],
            )
            .await
            .expect("invbroker.SetLabel");

            rpc(
                &ctx,
                &mut s1,
                "invbroker",
                "DepositPlexToVault",
                &[
                    PyRep::Integer(JITA_4_4),
                    PyRep::Integer(plex_stack_id),
                    PyRep::Integer(120),
                    PyRep::Integer(0),
                ],
            )
            .await
            .expect("DepositPlexToVault");

            verify_all_seven_ssot_domains(
                &ctx,
                &mut s1,
                corp_id,
                rival_corp_id,
                alliance_id,
                war_id,
                ship_id,
                lbl_id,
                m_del,
                m_keep,
            )
            .await;

            (m_del, m_keep, lbl_id)
        };

        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 2"));
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        common::pin_clock(&ctx2);
        let mut s1_re = select_char(&ctx2, 72_022, "demo", "demo-secret", P1).await;
        verify_all_seven_ssot_domains(
            &ctx2,
            &mut s1_re,
            corp_id,
            rival_corp_id,
            alliance_id,
            war_id,
            ship_id,
            mail_label_id,
            mail_deleted,
            mail_kept,
        )
        .await;

        cleanup_sqlite_path(&db_path);
    }

    #[allow(clippy::too_many_arguments)]
    async fn verify_all_seven_ssot_domains(
        ctx: &Arc<ServerContext>,
        s1: &mut SessionState,
        corp_id: i64,
        rival_corp_id: i64,
        alliance_id: i64,
        war_id: i64,
        ship_id: i64,
        mail_label_id: i64,
        mail_deleted: i64,
        mail_kept: i64,
    ) {
        let router = EsiRouter::new(Arc::clone(ctx));
        let tok1 = issue_token(
            ctx,
            "demo",
            "demo-secret",
            P1,
            &[
                "esi-characters.read_contacts.v1",
                "esi-characters.cspa.v1",
                "esi-mail.read_mail.v1",
                "esi-assets.read_assets.v1",
            ],
        );

        // 1. Contacts
        let contacts_rpc = rpc(ctx, s1, "charMgr", "GetContacts", &[])
            .await
            .expect("GetContacts");
        assert!(format!("{contacts_rpc:?}").contains("90000002"));
        let contacts_esi = esi_call(
            &router,
            "GET",
            &format!("/latest/characters/{P1}/contacts/"),
            Some(&tok1),
            None,
        );
        assert!(contacts_esi.to_string().contains("90000002"));

        // 2. Mail labels & deleted mail
        let labels_rpc = rpc(ctx, s1, "mailMgr", "GetLabels", &[])
            .await
            .expect("mailMgr.GetLabels");
        assert!(format!("{labels_rpc:?}").contains("PriorityOps"));
        let labels_esi = esi_call(
            &router,
            "GET",
            &format!("/latest/characters/{P1}/mail/labels/"),
            Some(&tok1),
            None,
        );
        assert!(labels_esi["labels"]
            .as_array()
            .expect("labels array")
            .iter()
            .any(|l| l["label_id"].as_i64() == Some(mail_label_id) && l["name"] == "PriorityOps"));
        let labels_quasar = quasar_rpc(
            ctx,
            s1,
            "eve.contact.label.GetForCharacterRequest",
            vec![],
        )
        .await;
        assert_eq!(labels_quasar.status_code, STATUS_OK);
        let labels_bytes = labels_quasar.payload.expect("labels payload").value;
        assert!(!String::from_utf8_lossy(&labels_bytes).contains("PriorityOps"));
        let headers_rpc = rpc(ctx, s1, "mailMgr", "GetMailHeaders", &[])
            .await
            .expect("GetMailHeaders");
        let hdr_dbg = format!("{headers_rpc:?}");
        assert!(!hdr_dbg.contains(&format!("Integer({mail_deleted})")));
        assert!(hdr_dbg.contains(&format!("Integer({mail_kept})")));
        let mail_esi = esi_call(
            &router,
            "GET",
            &format!("/latest/characters/{P1}/mail/"),
            Some(&tok1),
            None,
        );
        let mail_str = mail_esi.to_string();
        assert!(!mail_str.contains("Purged Dispatch") && mail_str.contains("Retained Dispatch"));

        // 3. Custom item names
        let decoded_item = rpc(
            ctx,
            s1,
            "encodedItems",
            "DecodeItem",
            &[PyRep::Integer(ship_id)],
        )
        .await
        .expect("DecodeItem");
        assert_eq!(
            decoded_item.dict_get("customName").and_then(|v| v.as_str()),
            Some("B11 Sovereign Hull")
        );
        let names_esi = esi_call(
            &router,
            "POST",
            &format!("/latest/characters/{P1}/assets/names/"),
            Some(&tok1),
            Some(serde_json::to_vec(&vec![ship_id]).expect("json")),
        );
        assert_eq!(names_esi[0]["name"], "B11 Sovereign Hull");

        // 4. CSPA charges
        let p1_cspa = rpc(ctx, s1, "account", "GetDefaultContactCost", &[])
            .await
            .expect("GetDefaultContactCost");
        assert_eq!(p1_cspa.as_f64(), Some(25_000.0));
        let cspa_esi = esi_call(
            &router,
            "POST",
            &format!("/latest/characters/{P1}/cspa/"),
            Some(&tok1),
            Some(serde_json::to_vec(&vec![P2]).expect("json")),
        );
        assert_eq!(cspa_esi.as_f64(), Some(12_500.0));
        let cspa_q_p1 = quasar_rpc(ctx, s1, "eve.mail.cspa.GetRequest", vec![]).await;
        assert_eq!(cspa_q_p1.status_code, STATUS_OK);
        let cspa_wm_p1 = eve_proto::quasar::WireMessage::decode(&cspa_q_p1.payload.expect("cspa p1").value).expect("wm");
        let cspa_inner_p1 = eve_proto::quasar::WireMessage::decode(
            cspa_wm_p1.fields[0].value.as_bytes().expect("charge bytes"),
        )
        .expect("inner wm");
        assert_eq!(cspa_inner_p1.fields[0].value.as_varint(), Some(25_000));
        let (mut p2_id, mut p2_cspa_req) = (Vec::new(), Vec::new());
        encode_varint_field(1, P2 as u64, &mut p2_id);
        encode_bytes_field(1, &p2_id, &mut p2_cspa_req);
        let cspa_q_p2 = quasar_rpc(ctx, s1, "eve.mail.cspa.GetRequest", p2_cspa_req).await;
        assert_eq!(cspa_q_p2.status_code, STATUS_OK);
        let cspa_wm_p2 = eve_proto::quasar::WireMessage::decode(&cspa_q_p2.payload.expect("cspa p2").value).expect("wm");
        let cspa_inner_p2 = eve_proto::quasar::WireMessage::decode(
            cspa_wm_p2.fields[0].value.as_bytes().expect("charge bytes"),
        )
        .expect("inner wm");
        assert_eq!(cspa_inner_p2.fields[0].value.as_varint(), Some(12_500));

        // 5. War records
        let wars_rpc = rpc(ctx, s1, "warRegistry", "GetWars", &[PyRep::Integer(corp_id)])
            .await
            .expect("warRegistry.GetWars");
        assert!(format!("{wars_rpc:?}").contains(&war_id.to_string()));
        let war_esi = esi_call(
            &router,
            "GET",
            &format!("/latest/wars/{war_id}/"),
            None,
            None,
        );
        assert_eq!(war_esi["aggressor"]["corporation_id"], corp_id);
        assert_eq!(war_esi["defender"]["corporation_id"], rival_corp_id);
        let reg = QuasarDescriptorRegistry::open_default().expect("quasar registry");
        let wars_all_q = quasar_rpc(ctx, s1, "eve.war.GetAllRequest", vec![]).await;
        assert_eq!(wars_all_q.status_code, STATUS_OK);
        let wars_all_dyn = reg
            .decode_dynamic("eve.war.GetAllResponse", &wars_all_q.payload.expect("wars all").value)
            .expect("decode GetAllResponse");
        assert!(format!("{wars_all_dyn:?}").contains(&war_id.to_string()));
        let (mut war_id_b, mut war_get_req) = (Vec::new(), Vec::new());
        encode_varint_field(1, war_id as u64, &mut war_id_b);
        encode_bytes_field(1, &war_id_b, &mut war_get_req);
        let war_get_q = quasar_rpc(ctx, s1, "eve.war.GetRequest", war_get_req).await;
        assert_eq!(war_get_q.status_code, STATUS_OK);
        let war_wm = eve_proto::quasar::WireMessage::decode(&war_get_q.payload.expect("war get").value).expect("war wm");
        let war_attr = eve_proto::quasar::WireMessage::decode(war_wm.fields[0].value.as_bytes().expect("attr")).expect("attr wm");
        let agg_wm = eve_proto::quasar::WireMessage::decode(war_attr.fields[0].value.as_bytes().expect("agg")).expect("agg wm");
        let def_wm = eve_proto::quasar::WireMessage::decode(war_attr.fields[1].value.as_bytes().expect("def")).expect("def wm");
        assert_eq!(agg_wm.fields[0].value.as_varint(), Some(corp_id as u64));
        assert_eq!(def_wm.fields[0].value.as_varint(), Some(rival_corp_id as u64));

        // 6. Alliances
        let ally_rpc = rpc(
            ctx,
            s1,
            "allianceRegistry",
            "GetAlliance",
            &[PyRep::Integer(alliance_id)],
        )
        .await
        .expect("allianceRegistry.GetAlliance");
        assert!(format!("{ally_rpc:?}").contains("B11UA"));
        let ally_esi = esi_call(
            &router,
            "GET",
            &format!("/latest/alliances/{alliance_id}/"),
            None,
            None,
        );
        assert_eq!(ally_esi["ticker"], "B11UA");
        let mut char_seq = Vec::new();
        encode_varint_field(1, P1 as u64, &mut char_seq);
        let mut pres_req = Vec::new();
        encode_bytes_field(1, &char_seq, &mut pres_req);
        let pres_rsp = quasar_rpc(
            ctx,
            s1,
            "eve_public.presence.api.GetAllianceRequest",
            pres_req,
        )
        .await;
        assert_eq!(pres_rsp.status_code, STATUS_OK);
        let pres_dyn = reg
            .decode_dynamic(
                "eve_public.presence.api.GetAllianceResponse",
                &pres_rsp.payload.expect("presence payload").value,
            )
            .expect("decode GetAllianceResponse");
        assert!(format!("{pres_dyn:?}").contains(&alliance_id.to_string()));

        // 7. PLEX balances
        let plex_client = rpc(ctx, s1, "vaultManager", "get_account_balance", &[])
            .await
            .expect("vaultManager.get_account_balance")
            .as_i64()
            .expect("plex i64");
        assert_eq!(plex_client, 620);
        let plex_quasar = quasar_rpc(ctx, s1, "eve_public.plex.vault.api.BalanceRequest", vec![]).await;
        assert_eq!(plex_quasar.status_code, STATUS_OK);
        let bal_dyn = reg
            .decode_dynamic(
                "eve_public.plex.vault.api.BalanceResponse",
                &plex_quasar.payload.expect("balance payload").value,
            )
            .expect("decode BalanceResponse");
        assert!(format!("{bal_dyn:?}").contains("62000"));
    }

    #[tokio::test]
    async fn test_b11_quasar_social_fail_closed_and_ssot_guards() {
        let ctx = ServerContext::new();
        let mut anon = SessionState::new(72_099);
        assert_eq!(
            quasar_rpc(&ctx, &mut anon, "eve.mail.cspa.GetRequest", vec![]).await.status_code,
            401
        );
        assert_eq!(
            quasar_rpc(&ctx, &mut anon, "eve.mail.CCPSendMailRequest", vec![]).await.status_code,
            401
        );

        let mut s1 = select_char(&ctx, 72_098, "demo", "demo-secret", P1).await;
        assert_eq!(
            quasar_rpc(&ctx, &mut s1, "eve.mail.CCPSendMailRequest", vec![]).await.status_code,
            403
        );

        let mut id_1001 = Vec::new();
        encode_varint_field(1, 1001, &mut id_1001);
        let mut war_1001 = Vec::new();
        eve_proto::quasar::encode_bytes_field(1, &id_1001, &mut war_1001);
        assert_eq!(quasar_rpc(&ctx, &mut s1, "eve.war.GetRequest", war_1001.clone()).await.status_code, 404);
        assert_eq!(quasar_rpc(&ctx, &mut s1, "eve.war.GetAlliesRequest", war_1001).await.status_code, 404);

        let empty_labels = quasar_rpc(&ctx, &mut s1, "eve.contact.label.GetForCharacterRequest", vec![]).await;
        assert_eq!(empty_labels.status_code, STATUS_OK);
        assert!(empty_labels.payload.expect("payload").value.is_empty());

        let mut id_p2 = Vec::new();
        encode_varint_field(1, P2 as u64, &mut id_p2);
        let mut other_char = Vec::new();
        eve_proto::quasar::encode_bytes_field(1, &id_p2, &mut other_char);
        assert_eq!(
            quasar_rpc(&ctx, &mut s1, "eve.contact.label.GetForCharacterRequest", other_char).await.status_code,
            403
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_b11_multithread_wal_error_survives_await_and_rolls_back() {
        let db_path = std::env::temp_dir().join(format!("b11_mt_wal_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        let mem_ctx = ServerContext::new();
        mem_ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
        let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
        let mut sess = select_char(&ctx, 72_049, "demo", "demo-secret", P1).await;
        let before = crate::gameplay_services_and_rpcs::invariant_atomic_rollback_randomized_fault::capture_observable_state(&ctx);
        let conn = rusqlite::Connection::open(&db_path).expect("open sqlite");
        conn.execute_batch(
            "CREATE TRIGGER abort_char_upd BEFORE UPDATE ON characters BEGIN SELECT RAISE(ABORT, 'injected_mt_wal_fail'); END;",
        )
        .expect("trigger");
        drop(conn);
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "account",
                None,
                "GiveCash",
                &[PyRep::Integer(P2), PyRep::Real(100.0), PyRep::String("mt-test".into())],
                &std::collections::HashMap::new(),
                None,
            )
            .await
            .expect_err("must fail on WAL abort");
        assert!(
            err.code == "DatabasePersistenceError" || err.code == "PersistenceFailure",
            "unexpected error code: {}",
            err.code
        );
        assert!(err.message.contains("injected_mt_wal_fail"));
        tokio::task::yield_now().await;
        assert_eq!(crate::gameplay_services_and_rpcs::invariant_atomic_rollback_randomized_fault::capture_observable_state(&ctx), before);
        let _ = std::fs::remove_file(&db_path);
    }
}

pub(crate) mod load_service_start_stop_stats {
    // Integration tests for open RPC and UI feature rows (Batch 5).


    use std::collections::HashMap;

    use eve_db::service_roles::{ROLE_ADMIN, ROLE_CONTENT, ROLE_GMH};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;

    use crate::common;

    const PILOT_1: i64 = 90_000_001;
    const PILOT_2: i64 = 90_000_002;
    const JITA_SYSTEM: u32 = 30_000_142;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        ServerContext::with_sde_and_repository(sde, db)
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
            .await
    }

    async fn setup_pilot(ctx: &ServerContext, char_id: i64, role: u64) -> SessionState {
        let mut sess = SessionState::new(char_id as u64);
        let (username, password) = if char_id == PILOT_2 {
            ("other", "other-secret")
        } else {
            ("demo", "demo-secret")
        };
        common::login_pilot(ctx, &mut sess, username, password).await;
        let kw = HashMap::new();
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID");
        sess.role |= role;
        sess
    }

    fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
        match rep {
            PyRep::Dict(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
        match rep {
            PyRep::KeyVal(entries) => entries
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v),
            PyRep::Object { args, .. } => kv_get(args, key),
            _ => dict_get(rep, key),
        }
    }

    #[tokio::test]
    async fn test_load_service_start_stop_and_stats() {
        let ctx = new_ctx();
        let mut player_sess = setup_pilot(&ctx, PILOT_1, 0).await;
        let err = rpc(&ctx, &mut player_sess, "loadService", "StartLoad", &[])
            .await
            .expect_err("non-GM should be rejected");
        assert_eq!(err.code, "UserError");

        let mut gm_sess = setup_pilot(&ctx, PILOT_1, ROLE_ADMIN).await;
        let res = rpc(
            &ctx,
            &mut gm_sess,
            "loadService",
            "StartLoad",
            &[PyRep::String("stress_profile_v1".into())],
        )
        .await
        .expect("StartLoad");
        assert_eq!(res, PyRep::None);

        let stats_active = rpc(&ctx, &mut gm_sess, "loadService", "GetTotalStats", &[])
            .await
            .expect("GetTotalStats");
        assert_eq!(dict_get(&stats_active, "active"), Some(&PyRep::Bool(true)));
        assert_eq!(
            dict_get(&stats_active, "runs_started"),
            Some(&PyRep::Integer(1))
        );

        let stop_res = rpc(&ctx, &mut gm_sess, "loadService", "StopLoad", &[])
            .await
            .expect("StopLoad");
        assert_eq!(stop_res, PyRep::None);

        let stats_stopped = rpc(&ctx, &mut gm_sess, "loadService", "GetTotalStats", &[])
            .await
            .expect("GetTotalStats after stop");
        assert_eq!(
            dict_get(&stats_stopped, "active"),
            Some(&PyRep::Bool(false))
        );
        assert_eq!(
            dict_get(&stats_stopped, "total_requests"),
            Some(&PyRep::Integer(2))
        );
    }

    #[tokio::test]
    async fn test_session_mgr_remove_and_end_all_sessions() {
        let ctx = new_ctx();
        let mut gm_sess = setup_pilot(&ctx, PILOT_1, ROLE_ADMIN).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("active_sessions".into(), "[101, 102, 103]".into());
        });

        let rem_res = rpc(
            &ctx,
            &mut gm_sess,
            "sessionMgr",
            "RemoveSessionsFromServer",
            &[PyRep::None, PyRep::List(vec![PyRep::Integer(102)])],
        )
        .await
        .expect("RemoveSessionsFromServer");
        assert_eq!(rem_res, PyRep::None);

        let remaining = ctx.db.with_snapshot(|snap| {
            snap.service_state_blobs
                .get("active_sessions")
                .cloned()
                .unwrap_or_default()
        });
        assert_eq!(remaining, "[101,103]");

        let end_res = rpc(&ctx, &mut gm_sess, "sessionMgr", "EndAllGameSessions", &[])
            .await
            .expect("EndAllGameSessions");
        assert_eq!(end_res, PyRep::None);

        let after_end = ctx.db.with_snapshot(|snap| {
            snap.service_state_blobs
                .get("active_sessions")
                .cloned()
                .unwrap_or_default()
        });
        assert_eq!(after_end, "[]");
    }

    #[tokio::test]
    async fn test_raffle_qa_seed_db2_callproc_and_debug_exec() {
        // parity: rpc:raffleMgr.QA_SeedRaffles
        let ctx = new_ctx();
        let mut gm_sess = setup_pilot(&ctx, PILOT_1, ROLE_ADMIN | ROLE_GMH).await;

        let seeded = rpc(
            &ctx,
            &mut gm_sess,
            "raffleMgr",
            "QA_SeedRaffles",
            &[PyRep::Integer(2)],
        )
        .await
        .expect("QA_SeedRaffles");
        match seeded {
            PyRep::List(items) => assert_eq!(items.len(), 2),
            other => panic!("expected list of 2 seeded raffles, got {other:?}"),
        }

        let polaris = rpc(
            &ctx,
            &mut gm_sess,
            "DB2",
            "CallProc",
            &[PyRep::String("zcluster.Nodes_PolarisID".into())],
        )
        .await
        .expect("DB2.CallProc");
        assert_eq!(polaris, PyRep::Integer(1));

        let exec_err = rpc(
            &ctx,
            &mut gm_sess,
            "debug",
            "Exec",
            &[PyRep::String("status_check".into())],
        )
        .await
        .expect_err("debug.Exec fail-closed");
        assert_eq!(exec_err.code, "UnimplementedRPC");
    }

    #[tokio::test]
    async fn test_localization_server_and_user_svc_session_created() {
        let ctx = new_ctx();
        let mut gm_sess = setup_pilot(&ctx, PILOT_1, ROLE_GMH | ROLE_CONTENT).await;

        let toggle_ok = rpc(
            &ctx,
            &mut gm_sess,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
        )
        .await
        .expect("boolean toggle must succeed for GM");
        assert_eq!(toggle_ok, PyRep::Bool(true));

        let wrap_err = rpc(
            &ctx,
            &mut gm_sess,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Dict(vec![(
                PyRep::Integer(777_001),
                PyRep::String("QA Localized Label".into()),
            )])],
        )
        .await
        .expect_err("UpdateLocalizationQAWrap dict overload fail-closed");
        assert_eq!(wrap_err.code, "UnimplementedRPC");

        let reload_res = rpc(
            &ctx,
            &mut gm_sess,
            "localizationServer",
            "ReloadFSDPickle",
            &[PyRep::String("res:/localization/eve_en.pickle".into())],
        )
        .await
        .unwrap_err();
        assert_eq!(reload_res.code, "UnimplementedRPC");

        let sess_created = rpc(
            &ctx,
            &mut gm_sess,
            "userSvc",
            "ProcessEVESessionCreated",
            &[PyRep::Integer(PILOT_1)],
        )
        .await
        .expect("ProcessEVESessionCreated");
        assert_eq!(sess_created, PyRep::None);
    }

    #[tokio::test]
    async fn test_ui_neocom_badges_map_stats_and_agency() {
        let ctx = new_ctx();
        let mut sess = setup_pilot(&ctx, PILOT_1, 0).await;
        sess.solar_system_id = Some(JITA_SYSTEM);

        let set_pop = rpc(
            &ctx,
            &mut sess,
            "activityMgr",
            "SetActivitiesPopState",
            &[PyRep::Dict(vec![(
                PyRep::String("daily".into()),
                PyRep::Bool(true),
            )])],
        )
        .await
        .expect("SetActivitiesPopState");
        assert_eq!(set_pop, PyRep::Bool(true));

        let pop_state = rpc(&ctx, &mut sess, "activityMgr", "GetActivitiesPopState", &[])
            .await
            .expect("GetActivitiesPopState");
        assert_eq!(dict_get(&pop_state, "daily"), Some(&PyRep::Bool(true)));

        let badges = rpc(
            &ctx,
            &mut sess,
            "activityMgr",
            "GetActivityBadgeStatuses",
            &[],
        )
        .await
        .expect("GetActivityBadgeStatuses");
        assert!(matches!(badges, PyRep::PySet(_)));

        let history = rpc(
            &ctx,
            &mut sess,
            "map",
            "GetHistory",
            &[PyRep::Integer(0), PyRep::Integer(24)],
        )
        .await
        .expect("map.GetHistory");
        assert!(matches!(history, PyRep::ObjectEx { .. }));

        let visits = rpc(&ctx, &mut sess, "map", "GetSolarSystemVisits", &[])
            .await
            .expect("map.GetSolarSystemVisits");
        assert!(matches!(visits, PyRep::List(_)));

        let agency_data = rpc(
            &ctx,
            &mut sess,
            "custom_agency_provider",
            "get_content_data",
            &[],
        )
        .await
        .expect("custom_agency_provider.get_content_data");
        match &agency_data {
            PyRep::List(items) => {
                if let Some(first) = items.first() {
                    assert_eq!(
                        kv_get(first, "solarSystemID"),
                        Some(&PyRep::Integer(JITA_SYSTEM as i64))
                    );
                    assert!(kv_get(first, "contentType").is_some());
                    assert!(kv_get(first, "instanceID").is_some());
                }
            }
            other => panic!("expected PyRep::List from get_content_data, got {other:?}"),
        }
    }
}

pub(crate) mod blockade_runner_cargo_scan_immunity {
    // Integration tests for open ship and PvE feature rows (Batch 5).


    use std::collections::HashMap;

    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, DVec3, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_sde::attr_ids;
    use eve_server::router::ServerContext;

    use crate::common;

    const PILOT_1: i64 = 90_000_001;
    const PILOT_2: i64 = 90_000_002;
    const JITA_SYSTEM: u32 = 30_000_142;
    const RND_AGENT_ID: i64 = 3_009_318;
    const RND_STATION_ID: i64 = 60_010_273;
    const RND_SYSTEM_ID: u32 = 30_004_288;

    const CRANE: u32 = 12_729; // Blockade Runner (group 1202)
    const RAVEN: u32 = 638; // Battleship with Frigate Escape Bay
    const RIFTER: u32 = 587; // Frigate
    const FALCON: u32 = 11_957; // Force Recon (Covert Cyno)
    const AVATAR: u32 = 11_567; // Titan (Jump Portal)
    const CARGO_SCANNER: u32 = 442; // Cargo Scanner I (group 47)
    const CYNO: u32 = 21_096;
    const COVERT_CYNO: u32 = 28_646;
    const JUMP_PORTAL: u32 = 23_953;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock_paused(true);
        ctx
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
            .await
    }

    fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
        match rep {
            PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
            PyRep::Dict(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .map(|(_, v)| v),
            PyRep::Tuple(items) => items.iter().find_map(|i| kv_get(i, key)),
            PyRep::Object { args, .. } => kv_get(args, key),
            _ => None,
        }
    }

    async fn setup_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(char_id as u64);
        let (username, password) = if char_id == PILOT_2 {
            ("other", "other-secret")
        } else {
            ("demo", "demo-secret")
        };
        common::login_pilot(ctx, &mut sess, username, password).await;
        let kw = HashMap::new();
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID");
        sess
    }

    fn seed_item(
        ctx: &ServerContext,
        item_id: i64,
        type_id: u32,
        owner_id: i64,
        location_id: i64,
        flag: InventoryFlag,
        quantity: i32,
    ) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id,
                    location_id,
                    flag,
                    quantity,
                    singleton: quantity == 1,
                    version: 1,
                    ..Default::default()
                },
            );
        });
    }

    fn grant_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
        let pairs = [
            (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
            (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
            (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
        ];
        let skills: Vec<u32> = pairs
            .iter()
            .filter_map(|(s, _)| {
                ctx.sde.types[&type_id]
                    .default_attributes
                    .get(s)
                    .map(|&v| v as u32)
            })
            .filter(|&s| s > 0)
            .collect();
        ctx.db.with_mut_snapshot(|snap| {
            let entry = snap.skills.entry(char_id).or_default();
            for sid in skills {
                entry.insert(
                    sid,
                    eve_db::TrainedSkillRecord {
                        type_id: sid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        });
    }

    async fn put_in_space(
        ctx: &ServerContext,
        sess: &mut SessionState,
        char_id: i64,
        ship_id: i64,
        type_id: u32,
        sys_id: u32,
        pos: DVec3,
    ) {
        seed_item(ctx, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.active_ship_id = ship_id;
                ch.station_id = None;
                ch.solar_system_id = sys_id;
                ch.jump_cooldown_until_unix = 0;
            }
        });
        sess.ship_id = Some(ship_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.solar_system_id = Some(sys_id);
        ctx.invalidate_ship_fitting(ship_id);
        let t = &ctx.sde.types[&type_id];
        let ball = Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.0,
            mass: t.mass,
            radius: t.radius,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let cs = ctx.build_ship_combat_state(char_id, ship_id, type_id);
        let meta = ctx.build_ship_slim_meta(char_id, type_id);
        ctx.systems
            .get_or_spawn(sys_id)
            .spawn_ball_with_meta(ball, cs, meta)
            .await
            .expect("spawn ship");
    }

    #[tokio::test]
    async fn test_blockade_runner_cargo_scan_immunity_and_frigate_escape_bay() {
        let ctx = new_ctx();
        let mut s1 = setup_pilot(&ctx, PILOT_1).await;
        let mut s2 = setup_pilot(&ctx, PILOT_2).await;

        let scanner_ship = 200_001;
        let br_ship = 200_002;
        let scanner_mod = 200_003;
        put_in_space(&ctx, &mut s1, PILOT_1, scanner_ship, RAVEN, JITA_SYSTEM, DVec3::ZERO).await;
        put_in_space(
            &ctx,
            &mut s2,
            PILOT_2,
            br_ship,
            CRANE,
            JITA_SYSTEM,
            DVec3::new(1000.0, 0.0, 0.0),
        )
        .await;

        grant_skills(&ctx, PILOT_1, CARGO_SCANNER);
        seed_item(
            &ctx,
            scanner_mod,
            CARGO_SCANNER,
            PILOT_1,
            scanner_ship,
            InventoryFlag::MedSlot0,
            1,
        );
        ctx.db.with_mut_snapshot(|s| {
            s.module_online_states.insert(scanner_mod, true);
        });
        ctx.invalidate_ship_fitting(scanner_ship);

        ctx.systems
            .get_or_spawn(JITA_SYSTEM)
            .add_target_lock(
                scanner_ship,
                eve_destiny::TargetLock {
                    target_id: br_ship,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(100_000.0),
                },
            )
            .await
            .expect("add_target_lock");

        let scan_res = rpc(
            &ctx,
            &mut s1,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(scanner_mod),
                PyRep::String("cargoScan".into()),
                PyRep::Integer(br_ship),
            ],
        )
        .await
        .expect("Activate cargo scanner");
        assert_eq!(
            kv_get(&scan_res, "cargoScanImmune"),
            Some(&PyRep::Bool(true))
        );

        // Frigate Escape Bay: Raven (battleship) accepts 1 assembled frigate in FrigateEscapeBay (flag 179)
        let bs_id = 200_010;
        let frig_id = 200_011;
        seed_item(&ctx, bs_id, RAVEN, PILOT_1, 60_003_760, InventoryFlag::Hangar, 1);
        seed_item(&ctx, frig_id, RIFTER, PILOT_1, 60_003_760, InventoryFlag::Hangar, 1);
        s1.station_id = Some(60_003_760);
        s1.solar_system_id = Some(JITA_SYSTEM);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
                ch.station_id = Some(60_003_760);
            }
        });

        let kw = HashMap::new();
        let bind = ctx
            .dispatch_rpc(
                &mut s1,
                "invbroker",
                None,
                "MachoBindObject",
                &[
                    PyRep::Tuple(vec![PyRep::Integer(60_003_760), PyRep::Integer(15)]),
                    PyRep::None,
                ],
                &kw,
                None,
            )
            .await
            .expect("MachoBindObject");
        let mgr_oid = match &bind {
            PyRep::Tuple(parts) => parts[0].as_i64().unwrap_or(0) as u64,
            _ => 0,
        };
        let ship_inv = ctx
            .dispatch_rpc(
                &mut s1,
                "invbroker",
                Some(mgr_oid),
                "GetInventoryFromId",
                &[PyRep::Integer(bs_id), PyRep::Integer(0)],
                &kw,
                None,
            )
            .await
            .expect("GetInventoryFromId");
        let ship_oid = ship_inv.as_i64().unwrap_or(0) as u64;
        let add_res = ctx
            .dispatch_rpc(
                &mut s1,
                "invbroker",
                Some(ship_oid),
                "Add",
                &[
                    PyRep::Integer(frig_id),
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(InventoryFlag::FrigateEscapeBay as i64),
                ],
                &kw,
                None,
            )
            .await
            .expect("Add frigate to FrigateEscapeBay");
        assert_eq!(
            kv_get(&add_res, "flagID"),
            Some(&PyRep::Integer(InventoryFlag::FrigateEscapeBay as i64))
        );
        assert_eq!(kv_get(&add_res, "itemID"), Some(&PyRep::Integer(frig_id)));
    }

    #[tokio::test]
    async fn test_cyno_covert_cyno_jump_fatigue_and_titan_portal() {
        let ctx = new_ctx();
        let mut s1 = setup_pilot(&ctx, PILOT_1).await;
        let mut s2 = setup_pilot(&ctx, PILOT_2).await;

        let lowsec_sys = 30_002_537; // Amamake (0.4 low-sec)
        let origin_sys = 30_002_538; // Vard (0.4 low-sec, ~1.2 LY from Amamake)
        let beacon_ship = 210_001;
        let titan_ship = 210_002;
        let covert_mod = 210_003;
        let portal_mod = 210_004;

        put_in_space(&ctx, &mut s1, PILOT_1, beacon_ship, FALCON, lowsec_sys, DVec3::ZERO).await;
        put_in_space(&ctx, &mut s2, PILOT_2, titan_ship, AVATAR, origin_sys, DVec3::ZERO).await;

        grant_skills(&ctx, PILOT_1, COVERT_CYNO);
        seed_item(
            &ctx,
            covert_mod,
            COVERT_CYNO,
            PILOT_1,
            beacon_ship,
            InventoryFlag::HiSlot0,
            1,
        );
        seed_item(
            &ctx,
            210_005,
            16_273,
            PILOT_1,
            beacon_ship,
            InventoryFlag::Cargo,
            500,
        );
        ctx.db.with_mut_snapshot(|s| {
            s.module_online_states.insert(covert_mod, true);
            s.fleets.insert(
                777,
                eve_db::FleetRecord {
                    fleet_id: 777,
                    boss_id: PILOT_1,
                    members: vec![PILOT_1, PILOT_2],
                    ..Default::default()
                },
            );
        });
        ctx.invalidate_ship_fitting(beacon_ship);

        let cyno_res = rpc(
            &ctx,
            &mut s1,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(covert_mod),
                PyRep::String("cynosuralGeneration".into()),
            ],
        )
        .await
        .expect("Activate covert cyno");
        assert!(kv_get(&cyno_res, "beaconTypeID").is_some());
        let ozone_after_covert = ctx.db.with_snapshot(|s| {
            s.items
                .get(&210_005)
                .map(|it| it.quantity)
                .unwrap_or(0)
        });
        assert_eq!(
            ozone_after_covert, 495,
            "Falcon (-80% role bonus) with CFT V (-50%) consumes 5 Liquid Ozone (50 * 0.2 * 0.5)"
        );

        // Verify jump fatigue accumulation
        let (cd_min, fat_min) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 4.5, 0.0);
        assert!(cd_min > 0.0 && fat_min > 0.0);

        // Verify concurrent cyno activation on the same ship is rejected
        let dup_cyno_mod = 210_009;
        grant_skills(&ctx, PILOT_1, CYNO);
        seed_item(
            &ctx,
            dup_cyno_mod,
            CYNO,
            PILOT_1,
            beacon_ship,
            InventoryFlag::HiSlot1,
            1,
        );
        ctx.db.with_mut_snapshot(|s| {
            s.module_online_states.insert(dup_cyno_mod, true);
        });
        ctx.invalidate_ship_fitting(beacon_ship);
        let dup_err = rpc(
            &ctx,
            &mut s1,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(dup_cyno_mod),
                PyRep::String("cynosuralGeneration".into()),
            ],
        )
        .await
        .expect_err("concurrent cyno on same ship must be rejected");
        assert_eq!(dup_err.code, "CannotLightCynoInSystem");

        // Verify Titan Jump Portal generator bridge to standard cyno on a separate ship
        let std_beacon_ship = 210_008;
        let std_cyno_mod = 210_007;
        put_in_space(&ctx, &mut s1, PILOT_1, std_beacon_ship, FALCON, lowsec_sys, DVec3::new(5_000.0, 0.0, 0.0)).await;
        seed_item(&ctx, std_cyno_mod, CYNO, PILOT_1, std_beacon_ship, InventoryFlag::HiSlot0, 1);
        seed_item(&ctx, 210_010, 16_273, PILOT_1, std_beacon_ship, InventoryFlag::Cargo, 500);
        ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(std_cyno_mod, true); });
        ctx.invalidate_ship_fitting(std_beacon_ship);
        let std_cyno_res = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno_mod), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect("Activate standard cyno");
        let beacon_id = kv_get(&std_cyno_res, "beaconID").cloned().unwrap_or(PyRep::Integer(1));
        let beacon_type_id = kv_get(&std_cyno_res, "beaconTypeID").cloned().unwrap_or(PyRep::Integer(21096));

        grant_skills(&ctx, PILOT_2, JUMP_PORTAL);
        seed_item(&ctx, portal_mod, JUMP_PORTAL, PILOT_2, titan_ship, InventoryFlag::HiSlot0, 1);
        seed_item(&ctx, 210_006, 16_275, PILOT_2, titan_ship, InventoryFlag::Cargo, 5_000);
        ctx.db.with_mut_snapshot(|s| {
            s.module_online_states.insert(portal_mod, true);
            if let Some(ch) = s.characters.get_mut(&PILOT_2) { ch.jump_cooldown_until_unix = 0; }
        });
        ctx.invalidate_ship_fitting(titan_ship);

        let portal_res = rpc(&ctx, &mut s2, "beyonce", "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(PILOT_1), PyRep::Integer(lowsec_sys as i64), beacon_id, beacon_type_id])
            .await
            .expect("Activate Titan Jump Portal");
        assert_ne!(portal_res, PyRep::None);

        // Verify bridge_to_fleet_beacon is rejected when the ship already has an active cyno
        let (active_cyno_portal_mod, active_cyno_stront) = (210_011, 210_012);
        seed_item(&ctx, active_cyno_portal_mod, JUMP_PORTAL, PILOT_1, std_beacon_ship, InventoryFlag::HiSlot1, 1);
        seed_item(&ctx, active_cyno_stront, 16_275, PILOT_1, std_beacon_ship, InventoryFlag::Cargo, 1_000);
        ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(active_cyno_portal_mod, true); });
        ctx.invalidate_ship_fitting(std_beacon_ship);
        let _ = ctx.systems.get_or_spawn(lowsec_sys).remove_module_cycle(std_beacon_ship, std_cyno_mod).await;
        let portal_on_cyno_err = rpc(&ctx, &mut s1, "beyonce", "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(PILOT_1), PyRep::Integer(lowsec_sys as i64)])
            .await
            .expect_err("bridge_to_fleet_beacon must fail when ship has an active cyno");
        assert_eq!(portal_on_cyno_err.code, "CynosuralFieldActive");
        let stront_left = ctx.db.with_snapshot(|s| s.items.get(&active_cyno_stront).map(|it| it.quantity).unwrap_or(0));
        assert_eq!(stront_left, 1_000, "rejected portal must not consume Strontium Clathrates");

        // Advance past cyno expiry, verify SDE attr 1296 Covert Cyno Liquid Ozone at CFT V:
        // Enforcer (44995, attr 1296 = -80%) consumes 5 LO; Anathema (11188) and Stealth Bomber Purifier (12038) consume 25 LO
        ctx.advance_time_seconds(700);
        let (enforcer_ship, enforcer_cyno_mod, enforcer_lo) = (210_013, 210_014, 210_022);
        put_in_space(&ctx, &mut s1, PILOT_1, enforcer_ship, 44_995, lowsec_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;
        seed_item(&ctx, enforcer_cyno_mod, COVERT_CYNO, PILOT_1, enforcer_ship, InventoryFlag::HiSlot0, 1);
        seed_item(&ctx, enforcer_lo, 16_273, PILOT_1, enforcer_ship, InventoryFlag::Cargo, 5);
        ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(enforcer_cyno_mod, true); });
        ctx.invalidate_ship_fitting(enforcer_ship);
        let enforcer_cyno_res = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(enforcer_cyno_mod), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect("Enforcer with 5 Liquid Ozone must light Covert Cyno at CFT V");
        assert_eq!(kv_get(&enforcer_cyno_res, "consumptionQuantity").and_then(|v| v.as_i64()), Some(5));
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&enforcer_lo).map(|i| i.quantity).unwrap_or(0)), 0);

        ctx.advance_time_seconds(700);
        let (anathema_ship, anathema_cyno_mod, anathema_lo) = (210_016, 210_017, 210_018);
        put_in_space(&ctx, &mut s1, PILOT_1, anathema_ship, 11_188, lowsec_sys, DVec3::new(11_000.0, 0.0, 0.0)).await;
        seed_item(&ctx, anathema_cyno_mod, COVERT_CYNO, PILOT_1, anathema_ship, InventoryFlag::HiSlot0, 1);
        seed_item(&ctx, anathema_lo, 16_273, PILOT_1, anathema_ship, InventoryFlag::Cargo, 25);
        ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(anathema_cyno_mod, true); });
        ctx.invalidate_ship_fitting(anathema_ship);
        let anathema_cyno_res = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(anathema_cyno_mod), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect("Anathema with 25 Liquid Ozone must light Covert Cyno at CFT V");
        assert_eq!(kv_get(&anathema_cyno_res, "consumptionQuantity").and_then(|v| v.as_i64()), Some(25));
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&anathema_lo).map(|i| i.quantity).unwrap_or(0)), 0);

        ctx.advance_time_seconds(700);
        let (bomber_ship, bomber_cyno_mod, bomber_lo) = (210_019, 210_020, 210_021);
        put_in_space(&ctx, &mut s1, PILOT_1, bomber_ship, 12_038, lowsec_sys, DVec3::new(12_000.0, 0.0, 0.0)).await;
        seed_item(&ctx, bomber_cyno_mod, COVERT_CYNO, PILOT_1, bomber_ship, InventoryFlag::HiSlot0, 1);
        seed_item(&ctx, bomber_lo, 16_273, PILOT_1, bomber_ship, InventoryFlag::Cargo, 25);
        ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(bomber_cyno_mod, true); });
        ctx.invalidate_ship_fitting(bomber_ship);
        let bomber_cyno_res = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(bomber_cyno_mod), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect("Stealth Bomber Purifier with 25 Liquid Ozone must light Covert Cyno at CFT V");
        assert_eq!(kv_get(&bomber_cyno_res, "consumptionQuantity").and_then(|v| v.as_i64()), Some(25));
        assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&bomber_lo).map(|i| i.quantity).unwrap_or(0)), 0);

        ctx.advance_time_seconds(700);
        put_in_space(&ctx, &mut s1, PILOT_1, std_beacon_ship, FALCON, lowsec_sys, DVec3::new(15_000.0, 0.0, 0.0)).await;
        rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(std_cyno_mod), PyRep::String("cynosuralGeneration".into())])
            .await
            .expect("Re-activate standard cyno after expiry");
        seed_item(&ctx, 210_015, 16_274, PILOT_2, titan_ship, InventoryFlag::Cargo, 5_000);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&PILOT_2) { ch.jump_cooldown_until_unix = 0; }
        });
        let jump_2arg_res = rpc(&ctx, &mut s2, "beyonce", "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(PILOT_1), PyRep::Integer(lowsec_sys as i64)])
            .await
            .expect("2-arg jump after reship must match live beacon ahead of expired beacons");
        assert_ne!(jump_2arg_res, PyRep::None);
    }

    #[tokio::test]
    async fn test_rnd_research_agents_and_datacore_purchase() {
        let ctx = new_ctx();
        let mut sess = setup_pilot(&ctx, PILOT_1).await;
        sess.station_id = Some(RND_STATION_ID);
        sess.solar_system_id = Some(RND_SYSTEM_ID);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
                ch.station_id = Some(RND_STATION_ID);
                ch.solar_system_id = RND_SYSTEM_ID;
            }
        });

        // Without required science skill 11448, StartResearch (action 12) must fail
        let err = rpc(
            &ctx,
            &mut sess,
            "agentMgr",
            "DoAction",
            &[
                PyRep::Integer(12),
                PyRep::Integer(RND_AGENT_ID),
                PyRep::Integer(11_448),
            ],
        )
        .await
        .expect_err("missing research skill must fail");
        assert_eq!(err.code, "ErrorNoRelevantResearchSkills");

        // Grant Science V (3402), Mechanics V (3392), and field skill 11448 at level 3 and start research
        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(PILOT_1).or_default();
            skills.insert(
                3_402,
                eve_db::TrainedSkillRecord {
                    type_id: 3_402,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            skills.insert(
                3_392,
                eve_db::TrainedSkillRecord {
                    type_id: 3_392,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            skills.insert(
                11_448,
                eve_db::TrainedSkillRecord {
                    type_id: 11_448,
                    level: 3,
                    skill_points: 40_000.0,
                },
            );
        });
        let start_res = rpc(
            &ctx,
            &mut sess,
            "agentMgr",
            "DoAction",
            &[
                PyRep::Integer(12),
                PyRep::Integer(RND_AGENT_ID),
                PyRep::Integer(11_448),
            ],
        )
        .await
        .expect("StartResearch");
        assert_eq!(
            kv_get(&start_res, "researchStarted"),
            Some(&PyRep::Bool(true))
        );
        let points_per_day = match kv_get(&start_res, "pointsPerDay") {
            Some(PyRep::Real(v)) => *v,
            other => panic!("expected pointsPerDay Real, got {other:?}"),
        };
        assert!(points_per_day > 0.0, "pointsPerDay must be positive");

        let isk_before = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0));

        // Advance clock by 6 days to accumulate RP via the real RP formula, then redeem 1 datacore (action 14)
        ctx.advance_secs(86_400 * 6);
        let buy_res = rpc(
            &ctx,
            &mut sess,
            "agentMgr",
            "DoAction",
            &[
                PyRep::Integer(14),
                PyRep::Integer(RND_AGENT_ID),
                PyRep::Integer(1),
            ],
        )
        .await
        .expect("BuyDatacores");
        let dc_item_id = match kv_get(&buy_res, "datacoreItemID") {
            Some(PyRep::Integer(id)) => *id,
            other => panic!("expected datacoreItemID Integer, got {other:?}"),
        };
        let dc_type_id = match kv_get(&buy_res, "datacoreTypeID") {
            Some(PyRep::Integer(tid)) => *tid as u32,
            other => panic!("expected datacoreTypeID Integer, got {other:?}"),
        };
        assert!(dc_type_id > 0);
        assert_eq!(kv_get(&buy_res, "quantity"), Some(&PyRep::Integer(1)));
        let rem_rp = match kv_get(&buy_res, "remainingResearchPoints") {
            Some(PyRep::Real(v)) => *v,
            other => panic!("expected remainingResearchPoints Real, got {other:?}"),
        };
        let expected_rp = points_per_day * 6.0 - 100.0;
        assert!(
            (rem_rp - expected_rp).abs() < 1e-6,
            "expected remaining RP {expected_rp}, got {rem_rp}"
        );
        let isk_after = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&PILOT_1).map(|c| c.balance.0).unwrap_or(0));
        assert_eq!(isk_before - isk_after, 1_000_000, "10,000 ISK fee must be debited");
        ctx.db.with_snapshot(|snap| {
            let item = snap.items.get(&dc_item_id).expect("datacore item in inventory");
            assert_eq!(item.type_id, dc_type_id);
            assert_eq!(item.quantity, 1);
            assert_eq!(item.owner_id, PILOT_1);
            assert_eq!(item.location_id, RND_STATION_ID);
        });
    }
}

pub(crate) mod lp_store_militia_enlistment_gate {
    // Integration tests for open `feat:` rows (Batch 6).
    //
    // Covers FW militia LP store standing/enlistment gates, Mobile Scan Inhibitor
    // d-scan/probe suppression + internal sensor inoperability + 1h expiry, Mobile
    // Siphon Unit POS-only hourly material theft, AIR operations tutorial gates &
    // server-observed progression, and Opportunities server-observed completion.


    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use eve_destiny::Ball;
    use eve_machonet::SessionState;
    use eve_proto::quasar::QuasarMessage;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::{ActiveScannerProbe, ServerContext};
    use eve_server::services::operations_manager::get_operation_defs;
    use glam::DVec3;

    const PILOT_1: i64 = 90_000_001;
    const STATION_ID: i64 = 60_003_760;
    const SYS_ID: u32 = 30_000_142;
    const LOWSEC_SYS_ID: u32 = 30_000_015; // Sendaya (security ~0.29 < 0.4)

    static NEXT_TMP_DB: AtomicU64 = AtomicU64::new(1);

    struct TempDbGuard(PathBuf);
    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("sqlite-wal"));
            let _ = std::fs::remove_file(self.0.with_extension("sqlite-shm"));
        }
    }

    fn new_persistent_ctx() -> (TempDbGuard, PathBuf, ServerContext) {
        let seq = NEXT_TMP_DB.fetch_add(1, Ordering::Relaxed);
        let db_path = std::env::temp_dir().join(format!("evomu_b6_{}_{}.sqlite", std::process::id(), seq));
        let _ = std::fs::remove_file(&db_path);
        let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"));
        let demo_snap = eve_db::WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        let db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
        db.with_mut_snapshot(|s| *s = demo_snap);
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        eve_server::services::quasar_cosmetics_live::install_macho_notify_sink(&ctx);
        crate::common::pin_clock(&ctx);
        (TempDbGuard(db_path.clone()), db_path, ctx)
    }

    fn reload_ctx(db_path: &PathBuf, sde: Arc<eve_sde::SdeRegistry>) -> ServerContext {
        let db = Arc::new(eve_db::WorldRepository::open_sqlite(db_path).expect("reopen WorldRepository"));
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        eve_server::services::quasar_cosmetics_live::install_macho_notify_sink(&ctx);
        crate::common::pin_clock(&ctx);
        ctx
    }

    fn make_ball(id: i64, type_id: u32, position: DVec3, radius: f64) -> Ball {
        Ball {
            id, type_id, position, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius, speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::GotoDirection { direction: DVec3::Z },
        }
    }

    fn char_session(ctx: &ServerContext, char_id: i64, station_id: Option<i64>, solar_system_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
        let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(char_id as u64));
        SessionState {
            user_id: Some(uid), character_id: Some(char_id), corporation_id: Some(1_000_044),
            station_id, solar_system_id, ship_id, ..SessionState::default()
        }
    }

    async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
    }

    fn grant_anchoring_skill(ctx: &ServerContext, char_id: i64) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                11_584,
                eve_db::TrainedSkillRecord { type_id: 11_584, level: 5, skill_points: 256_000.0 },
            );
        });
    }

    fn item_rec(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: eve_db::InventoryFlag, quantity: i32, singleton: bool) -> eve_db::InventoryItemRecord {
        eve_db::InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, ..Default::default() }
    }

    fn dscan_ids(rep: PyRep) -> Vec<i64> {
        let PyRep::List(rows) = rep else { panic!("expected dscan list") };
        rows.iter().filter_map(|r| r.dict_get("id").and_then(|v| v.as_i64())).collect()
    }

    #[tokio::test]
    async fn test_lp_store_militia_enlistment_gate_and_clean_catalog() {
        let (_dir, db_path, ctx) = new_persistent_ctx();
        let mut sess = char_session(&ctx, PILOT_1, Some(STATION_ID), Some(SYS_ID), None);

        let mut chosen_offer: Option<(i64, u32)> = None;
        for corp_id in 1_000_179_i64..=1_000_182 {
            let rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(corp_id)])
                .await
                .expect("GetAvailableOffersFromCorp");
            let PyRep::List(offers) = rep else { panic!("expected list of LP offers") };
            assert!(!offers.is_empty(), "Militia corp {corp_id} must serve TQ LP offers");
            for o in &offers {
                let oid = o.dict_get("offerID").and_then(|v| v.as_i64()).unwrap_or(0);
                let tid = o.dict_get("typeID").and_then(|v| v.as_i64()).unwrap_or(0);
                assert!(!(910_001..=910_048).contains(&oid), "Synthetic FW booster offer {oid} must not appear");
                assert!(!(91_916..=91_977).contains(&tid), "Unverified FW booster type {tid} must not appear");
                if corp_id == 1_000_179 && chosen_offer.is_none() {
                    let no_req = matches!(o.dict_get("reqItems"), Some(PyRep::List(items)) if items.is_empty());
                    let ak = o.dict_get("akCost").and_then(|v| v.as_i64()).unwrap_or(0);
                    if no_req && ak == 0 && oid > 0 && tid > 0 {
                        chosen_offer = Some((oid, tid as u32));
                    }
                }
            }
        }
        let (offer_id, reward_tid) = chosen_offer.expect("24th Imperial Crusade offer without trade-ins");

        let (fw_station_id, fw_sys_id) = (60_015_054_i64, 30_002_978_u32);
        sess.station_id = Some(fw_station_id);
        sess.solar_system_id = Some(fw_sys_id);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&PILOT_1).unwrap();
            ch.loyalty_points.insert(1_000_179, 500_000);
            ch.balance = eve_db::IskCents(500_000_000_00);
            ch.standings.insert(1_000_179, -1.0);
            ch.war_faction_id = None;
            ch.station_id = Some(fw_station_id);
            ch.solar_system_id = fw_sys_id;
        });

        // Negative 1: Unenlisted pilot with negative corp standing is rejected
        let args = [PyRep::Integer(1_000_179), PyRep::Integer(offer_id), PyRep::Integer(1)];
        let err_unenlisted = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &args).await.unwrap_err();
        assert_eq!(err_unenlisted.code, "InsufficientStanding");

        // Negative 2: Pilot enlisted in opposing militia (Minmatar 500_002) is rejected
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_1).unwrap().war_faction_id = Some(500_002));
        let err_wrong_fac = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &args).await.unwrap_err();
        assert_eq!(err_wrong_fac.code, "InsufficientStanding");

        // Positive: Enlisted in matching Amarr militia (500_003) with 0.0 standing succeeds and persists
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&PILOT_1).unwrap();
            ch.war_faction_id = Some(500_003);
            ch.standings.insert(1_000_179, 0.0);
        });
        let take_ok = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &args).await.expect("redeem offer");
        assert_eq!(take_ok.dict_get("deliveredTypeID").and_then(|v| v.as_i64()), Some(i64::from(reward_tid)));

        let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
        ctx_reloaded.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&PILOT_1).expect("character persisted");
            assert!(ch.loyalty_points.get(&1_000_179).copied().unwrap_or(500_000) < 500_000);
            assert!(snap.items.values().any(|i| i.owner_id == PILOT_1 && i.location_id == fw_station_id && i.type_id == reward_tid));
        });
    }

    #[tokio::test]
    async fn test_mobile_scan_inhibitor_hides_ships_from_dscan_and_probes() {
        // parity: feat:structures.mobile-scan-inhibitor
        let (_dir, _db_path, ctx) = new_persistent_ctx();
        grant_anchoring_skill(&ctx, PILOT_1);
        let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS_ID);

        let base = DVec3::splat(2.0e11);
        let (scanner_ship_id, hidden_ship_id, visible_ship_id, deployer_ship_id, inhibitor_id) =
            (700_001, 700_002, 700_003, 700_004, 700_005);
        for (bid, pos) in [
            (scanner_ship_id, base),
            (hidden_ship_id, base + DVec3::new(100_000.0, 0.0, 0.0)),
            (visible_ship_id, base + DVec3::new(200_000.0, 0.0, 0.0)),
            (deployer_ship_id, base + DVec3::new(110_000.0, 0.0, 0.0)),
        ] {
            sys_handle.spawn_ball(make_ball(bid, 587, pos, 40.0)).await.unwrap();
        }

        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&PILOT_1).unwrap();
            ch.station_id = None;
            ch.solar_system_id = LOWSEC_SYS_ID;
            ch.active_ship_id = deployer_ship_id;
            snap.items.insert(deployer_ship_id, item_rec(deployer_ship_id, 587, PILOT_1, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true));
            snap.items.insert(inhibitor_id, item_rec(inhibitor_id, 33_589, PILOT_1, deployer_ship_id, eve_db::InventoryFlag::Cargo, 1, false));
        });

        // Live deployment via posMgr.AnchorStructure (60s activation delay + 3600s active lifetime)
        let mut deploy_sess = char_session(&ctx, PILOT_1, None, Some(LOWSEC_SYS_ID), Some(deployer_ship_id));
        let anchor_rep = rpc(&ctx, &mut deploy_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(inhibitor_id)])
            .await
            .expect("posMgr.AnchorStructure Mobile Scan Inhibitor");
        assert_eq!(anchor_rep.dict_get("anchoringDelaySeconds").and_then(|v| v.as_f64()), Some(60.0));

        // Negative 1: Mobile Scan Inhibitor (group 1275) cannot be unanchored / scooped once deployed
        let scoop_err = rpc(&ctx, &mut deploy_sess, "entity", "CmdUnanchor", &[PyRep::Integer(inhibitor_id)]).await.unwrap_err();
        assert_eq!(scoop_err.code, "CannotUnanchor");
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&PILOT_1).unwrap().active_ship_id = scanner_ship_id;
            let c2 = s.characters.get_mut(&90_000_002).unwrap();
            c2.station_id = None;
            c2.solar_system_id = LOWSEC_SYS_ID;
            c2.active_ship_id = hidden_ship_id;
        });

        let inhib_pos = [base.x + 110_250.0, base.y, base.z];
        let vis_pos = [base.x + 200_000.0, base.y, base.z];
        if let Ok(mut probes) = ctx.active_scanner_probes.write() {
            for (pid, cid, sid, pos) in [(800_001, PILOT_1, scanner_ship_id, inhib_pos), (800_002, 90_000_002, hidden_ship_id, vis_pos)] {
                probes.insert(pid, ActiveScannerProbe {
                    probe_id: pid, owner_char_id: cid, ship_id: sid, solar_system_id: LOWSEC_SYS_ID,
                    probe_type_id: 30_486, position: pos, destination: pos, range_step: 1,
                    scan_range_m: 1_000_000.0, base_sensor_strength: 44.0, launcher_module_id: 0,
                    max_flight_time_s: 7_200.0, launched_at_unix: ctx.now_unix(), state: 1, warp_end_ft: None,
                });
            }
        }

        // Negative 2: At t + 30s (during 60s warmup), inhibitor does NOT mask ships
        ctx.advance_secs(30);
        let mut ext_sess = char_session(&ctx, PILOT_1, None, Some(LOWSEC_SYS_ID), Some(scanner_ship_id));
        let mut inside_sess = char_session(&ctx, 90_000_002, None, Some(LOWSEC_SYS_ID), Some(hidden_ship_id));
        let dscan_args = [PyRep::Real(360.0), PyRep::Real(1_000_000.0)];
        let warmup_ids = dscan_ids(rpc(&ctx, &mut ext_sess, "scanMgr", "ConeScan", &dscan_args).await.unwrap());
        assert!(warmup_ids.contains(&hidden_ship_id), "Ship must remain visible before 60s inhibitor activation completes");

        // Positive: Advance past 60s activation delay (t + 65s) -> active suppression & internal sensor lockout
        ctx.advance_secs(35);
        let active_ids = dscan_ids(rpc(&ctx, &mut ext_sess, "scanMgr", "ConeScan", &dscan_args).await.unwrap());
        assert!(!active_ids.contains(&hidden_ship_id), "Ship inside active 30km sphere must be hidden from d-scan");
        assert!(active_ids.contains(&visible_ship_id) && active_ids.contains(&inhibitor_id));

        let PyRep::List(probe_rows) = rpc(&ctx, &mut ext_sess, "scanMgr", "RequestScans", &[PyRep::List(Vec::new())]).await.unwrap() else {
            panic!("expected probe scan list");
        };
        let probe_ids: Vec<i64> = probe_rows.iter().filter_map(|r| r.dict_get("itemID").and_then(|v| v.as_i64())).collect();
        assert!(!probe_ids.contains(&hidden_ship_id), "Ship inside active 30km sphere must be hidden from combat probes");
        assert!(probe_ids.contains(&visible_ship_id) && probe_ids.contains(&inhibitor_id));

        // Ships INSIDE the active 30km sphere have inoperable d-scan & probe scanning sensors
        assert_eq!(rpc(&ctx, &mut inside_sess, "scanMgr", "ConeScan", &dscan_args).await.unwrap(), PyRep::List(Vec::new()));
        assert_eq!(rpc(&ctx, &mut inside_sess, "scanMgr", "RequestScans", &[PyRep::List(Vec::new())]).await.unwrap(), PyRep::List(Vec::new()));

        // Expiry after 1 hour active lifetime -> actor tick despawns inhibitor and restores scanning
        ctx.advance_secs(3_600);
        sys_handle.step_manual(1.0).await.unwrap();
        assert!(sys_handle.get_ball_snapshot(inhibitor_id).await.unwrap().is_none(), "Expired inhibitor must despawn");
        let post_ids = dscan_ids(rpc(&ctx, &mut ext_sess, "scanMgr", "ConeScan", &dscan_args).await.unwrap());
        assert!(post_ids.contains(&hidden_ship_id));
    }

    #[tokio::test]
    async fn test_mobile_siphon_steals_material_from_pos_within_range() {
        // parity: feat:structures.mobile-siphon
        let (_dir, db_path, ctx) = new_persistent_ctx();
        grant_anchoring_skill(&ctx, PILOT_1);
        let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS_ID);

        let base = DVec3::splat(2.0e11);
        let (pos_tower_id, pos_silo_id, athanor_id) = (710_001, 710_002, 710_003);
        let (deployer_near_ship, deployer_far_ship) = (710_004, 710_005);
        let (siphon_near_id, siphon_far_id, unanchored_siphon_id) = (710_006, 710_007, 710_008);
        let (pos_silo_stack_id, pos_hangar_stack_id, athanor_stack_id) = (710_010, 710_011, 710_012);

        for (bid, tid, dx, rad) in [
            (pos_tower_id, 16_213, 0.0, 2_000.0),
            (pos_silo_id, 14_343, 2_000.0, 500.0),
            (athanor_id, 35_835, 100_000.0, 5_000.0),
            (deployer_near_ship, 587, 40_000.0, 40.0),
            (deployer_far_ship, 587, 250_000.0, 40.0),
            (unanchored_siphon_id, 33_477, 25_000.0, 100.0),
        ] {
            let p = base + DVec3::new(dx, 0.0, 0.0);
            sys_handle.spawn_ball(make_ball(bid, tid, p, rad)).await.unwrap();
            ctx.db.with_mut_snapshot(|snap| {
                snap.item_positions.insert(bid, [p.x, p.y, p.z]);
            });
        }

        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&PILOT_1).unwrap();
            ch.station_id = None;
            ch.solar_system_id = LOWSEC_SYS_ID;
            ch.active_ship_id = deployer_near_ship;
            for (iid, tid, owner, loc, flag, qty, sing) in [
                (pos_tower_id, 16_213, 1_000_044, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
                (pos_silo_id, 14_343, 1_000_044, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
                (athanor_id, 35_835, 1_000_044, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
                (deployer_near_ship, 587, PILOT_1, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
                (deployer_far_ship, 587, PILOT_1, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
                (siphon_near_id, 33_477, PILOT_1, deployer_near_ship, eve_db::InventoryFlag::Cargo, 1, false),
                (siphon_far_id, 33_477, PILOT_1, deployer_far_ship, eve_db::InventoryFlag::Cargo, 1, false),
                (unanchored_siphon_id, 33_477, PILOT_1, LOWSEC_SYS_ID as i64, eve_db::InventoryFlag::None, 1, true),
                (pos_silo_stack_id, 16_634, 1_000_044, pos_silo_id, eve_db::InventoryFlag::Cargo, 200, false),
                (pos_hangar_stack_id, 16_634, 90_000_002, pos_silo_id, eve_db::InventoryFlag::Hangar, 200, false),
                (athanor_stack_id, 16_634, 1_000_044, athanor_id, eve_db::InventoryFlag::Cargo, 200, false),
            ] {
                snap.items.insert(iid, item_rec(iid, tid, owner, loc, flag, qty, sing));
            }
        });

        let mut sess_near = char_session(&ctx, PILOT_1, None, Some(LOWSEC_SYS_ID), Some(deployer_near_ship));
        rpc(&ctx, &mut sess_near, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_near_id)]).await.expect("Anchor near siphon");
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_1).unwrap().active_ship_id = deployer_far_ship);
        let mut sess_far = char_session(&ctx, PILOT_1, None, Some(LOWSEC_SYS_ID), Some(deployer_far_ship));
        let err_far = rpc(&ctx, &mut sess_far, "posMgr", "AnchorStructure", &[PyRep::Integer(siphon_far_id)]).await.unwrap_err();
        assert_eq!(err_far.code, "NoSiphonTargetInRange");

        // Negative 1: Zero theft during anchoring and immediately upon anchor completion (t = 60s)
        ctx.advance_secs(60);
        sys_handle.step_manual(1.0).await.unwrap();
        let cap_t0 = rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
            .await.unwrap().as_f64().unwrap_or(1.0);
        assert_eq!(cap_t0, 0.0, "Zero theft must occur before a full hourly POS cycle elapses");

        // Positive: System actor tick runs cycle 1 -> 60 stolen + 6 waste = 66 drained from POS Silo (134 left)
        ctx.advance_secs(3_600);
        sys_handle.step_manual(1.0).await.unwrap();
        let cap1 = rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
            .await.unwrap().as_f64().unwrap_or(0.0);
        assert!(cap1 > 0.0, "Anchored siphon within 100km of POS Silo must steal raw material on hourly tick");

        // Second hourly cycle -> stacks to 120 in near siphon; Athanor, private Hangar, unanchored & far siphons untouched
        ctx.advance_secs(3_600);
        sys_handle.step_manual(1.0).await.unwrap();
        let cap2 = rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_near_id)])
            .await.unwrap().as_f64().unwrap_or(0.0);
        assert!(cap2 > cap1);
        assert_eq!(rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(siphon_far_id)]).await.unwrap().as_f64(), Some(0.0));
        assert_eq!(rpc(&ctx, &mut sess_near, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(unanchored_siphon_id)]).await.unwrap().as_f64(), Some(0.0));

        let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
        ctx_reloaded.db.with_snapshot(|snap| {
            assert_eq!(snap.items[&pos_silo_stack_id].quantity, 68, "POS Silo stack must lose 2x(60+6) units");
            assert_eq!(snap.items[&pos_hangar_stack_id].quantity, 200, "Private player Hangar stack must never be siphoned");
            assert_eq!(snap.items[&athanor_stack_id].quantity, 200, "Upwell Refinery cargo must never be siphoned");
            let stolen: Vec<_> = snap.items.values().filter(|i| i.location_id == siphon_near_id && i.type_id == 16_634).collect();
            assert_eq!(stolen.len(), 1);
            assert_eq!(stolen[0].quantity, 120);
        });
    }

    #[tokio::test]
    async fn test_operations_tutorial_tasks_categories_progress_and_rewards() {
        // parity: feat:pve.operations-tutorial
        let (_dir, db_path, ctx) = new_persistent_ctx();
        let ship_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&PILOT_1).unwrap().home_station_id = 60_015_054);
        let mut sess = char_session(&ctx, PILOT_1, Some(STATION_ID), Some(SYS_ID), Some(ship_id));
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_1)]).await.unwrap();
        sess.station_id = Some(STATION_ID);
        sess.solar_system_id = Some(SYS_ID);
        sess.ship_id = Some(ship_id);

        let op_defs = get_operation_defs(&ctx.sde);
        let (&cat_id, cat_ops) = op_defs
            .category_operations
            .iter()
            .filter(|(&cid, ops)| (1..=1_000).contains(&cid) && !ops.is_empty())
            .min_by_key(|(cid, ops)| (!op_defs.category_isk_rewards.contains_key(cid), ops.len()))
            .expect("SDE must define at least one operation category with constituent operations");
        let cat_ops = cat_ops.clone();
        let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance);

        // Negative 1: Non-GM cannot complete category before completing its SDE operations
        let cat_err = rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)]).await.unwrap_err();
        assert!(cat_err.code == "UserError" || cat_err.code.starts_with("OperationCategory"));
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance), bal_before);

        // Negative 2: Non-SDE operation ID is rejected by activate_operation_in_solar_system
        let bad_op = rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(99_999), PyRep::Integer(SYS_ID as i64), PyRep::Integer(cat_id)]).await.unwrap_err();
        assert_eq!(bad_op.code, "CustomInfo");

        // Negative 3: process_client_event with no active operation cannot complete tasks
        let any_tid = *op_defs.task_to_op.keys().next().expect("SDE task");
        let no_active = rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_complete".into()), PyRep::Integer(any_tid)]).await.unwrap();
        assert_eq!(no_active, PyRep::Bool(false));

        rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("start_tutorial".into()), PyRep::Integer(1)]).await.unwrap();
        let (&undock_tid, _) = op_defs
            .task_event_names
            .iter()
            .find(|(tid, ev)| {
                ev.as_str() == "undockFromStation"
                    && !op_defs.client_completable_tasks.contains(tid)
                    && !op_defs.task_is_client_skippable.contains(tid)
                    && !op_defs.task_is_optional.contains(tid)
                    && op_defs.task_to_op.contains_key(tid)
            })
            .expect("SDE server-observed undockFromStation operation task");
        let undock_op_id = op_defs.task_to_op[&undock_tid];
        sess.role |= eve_machonet::service_roles::ROLE_GMH;
        rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(undock_op_id), PyRep::Integer(SYS_ID as i64), PyRep::Integer(cat_id)]).await.unwrap();
        for &t in op_defs.op_tasks.get(&undock_op_id).into_iter().flatten() {
            if t == undock_tid { break; }
            let _ = rpc(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(cat_id), PyRep::Integer(undock_op_id), PyRep::Integer(t)]).await;
        }
        sess.role &= !eve_machonet::service_roles::ROLE_GMH;

        // Server-observed task rejects non-GM skip_task and pays no op/category ISK
        let skip_err = rpc(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(undock_tid)])
            .await
            .expect_err("non-GM skip_task on server-observed task must fail");
        assert!(skip_err.code == "CustomInfo" || skip_err.code.starts_with("TaskNot"));
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance), bal_before);

        // Server-observed task rejects client spoofing, then advances on real stationSvc.Undock
        let spoof_res = rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_complete".into()), PyRep::Integer(undock_tid)]).await.unwrap();
        assert_eq!(spoof_res, PyRep::Bool(false));
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(ship_id)]).await.expect("stationSvc.Undock");
        let undock_notifs = ctx.drain_notifications_for_char(PILOT_1);
        assert!(
            undock_notifs.iter().any(|n| {
                n.event == "OnOperationTaskCompleted"
                    && n.args == vec![PyRep::Integer(cat_id), PyRep::Integer(undock_op_id), PyRep::Integer(undock_tid)]
            }),
            "stationSvc.Undock must emit OnOperationTaskCompleted for server-observed task (cat={cat_id}, op={undock_op_id}, tid={undock_tid}), got: {:?}",
            undock_notifs.iter().map(|n| (&n.event, &n.args)).collect::<Vec<_>>()
        );
        if ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance) > bal_before {
            assert!(
                undock_notifs.iter().any(|n| {
                    n.event == "OnAccountChange" && n.args.first().and_then(|a| a.as_str()) == Some("cash")
                }),
                "stationSvc.Undock ISK reward must emit OnAccountChange('cash')"
            );
        }
        let PyRep::Dict(undock_prog) = rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[]).await.unwrap() else {
            panic!("expected progress dict after Undock");
        };
        assert!(
            undock_prog.iter().any(|(k, v)| k.as_i64() == Some(undock_tid) && v.as_i64() == Some(100)),
            "stationSvc.Undock must complete active SDE undockFromStation operation task"
        );

        let mut ops_to_complete = cat_ops.clone();
        if let Some((&isk_op, _)) = op_defs
            .op_isk_rewards
            .iter()
            .find(|(oid, _)| op_defs.op_tasks.get(oid).is_some_and(|ts| !ts.is_empty()))
        {
            if !ops_to_complete.contains(&isk_op) {
                ops_to_complete.push(isk_op);
            }
        }
        for &oid in &ops_to_complete {
            sess.role |= eve_machonet::service_roles::ROLE_GMH;
            rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(oid), PyRep::Integer(SYS_ID as i64), PyRep::Integer(cat_id)]).await.unwrap();
            sess.role &= !eve_machonet::service_roles::ROLE_GMH;
            sess.role |= eve_machonet::service_roles::ROLE_GMH;
            for &tid in op_defs.op_tasks.get(&oid).into_iter().flatten() {
                let _ = rpc(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_complete".into()), PyRep::Integer(tid)]).await.unwrap();
            }
            sess.role &= !eve_machonet::service_roles::ROLE_GMH;
        }

        let cat_ok = rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)]).await.expect("complete category");
        assert_eq!(cat_ok, PyRep::Bool(true));
        let bal_after_cat = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance);
        assert!(bal_after_cat >= bal_before, "SDE operation/category ISK rewards must not reduce character balance");
        let _ = rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)]).await.unwrap();
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance), bal_after_cat, "Category reward must be idempotent");

        let ctx_reloaded = reload_ctx(&db_path, Arc::clone(&ctx.sde));
        let mut sess_reloaded = char_session(&ctx_reloaded, PILOT_1, None, Some(SYS_ID), Some(ship_id));
        let PyRep::Dict(prog_entries) = rpc(&ctx_reloaded, &mut sess_reloaded, "operationsManager", "get_character_progress", &[]).await.unwrap() else {
            panic!("expected progress dict");
        };
        assert!(prog_entries.iter().any(|(k, v)| k.as_i64() == Some(cat_id) && v.as_i64() == Some(100)));
    }

    #[tokio::test]
    async fn test_opportunities_complete_from_events_and_persist() {
        // parity: feat:pve.opportunities
        let (_dir, db_path, ctx) = new_persistent_ctx();
        let (ship_id, account_id) = ctx.db.with_snapshot(|s| (s.characters[&PILOT_1].active_ship_id, s.characters[&PILOT_1].user_id));
        let mut sess = char_session(&ctx, PILOT_1, Some(STATION_ID), Some(SYS_ID), Some(ship_id));
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_1)]).await.unwrap();
        sess.station_id = Some(STATION_ID);
        sess.solar_system_id = Some(SYS_ID);
        sess.ship_id = Some(ship_id);
        sess.user_id = Some(account_id);

        let undock_ach_id = ctx
            .quasar_gateway
            .progression
            .achievements
            .definitions
            .iter()
            .find(|d| !d.is_client_achievement && d.target == 1 && d.conditions.iter().any(|c| c.event_name == "undockFromStation"))
            .map(|d| d.id)
            .expect("SDE server-observed undockFromStation opportunity");

        // Negative: Client opportunity PublishEventPayload records UI tracking/history ONLY and NEVER completes opportunities
        let (mut ctag, mut disc_opp, mut disc_ev) = (Vec::new(), Vec::new(), Vec::new());
        eve_proto::quasar::encode_string_field(1, "opp_undock_01", &mut ctag);
        eve_proto::quasar::encode_bytes_field(1, &ctag, &mut disc_opp);
        eve_proto::quasar::encode_varint_field(2, 999, &mut disc_opp);
        eve_proto::quasar::encode_bytes_field(1, &disc_opp, &mut disc_ev);
        let any = eve_proto::quasar::GoogleAny {
            type_url: "type.googleapis.com/eve_public.opportunity.api.Discovered".to_string(),
            value: disc_ev,
        };
        rpc(&ctx, &mut sess, "publicGatewaySvc", "PublishEventPayload", &[PyRep::Bytes(any.encode_proto())]).await.unwrap();

        ctx.db.with_snapshot(|s| {
            let opp_json: serde_json::Value = s.quasar_state_blobs.get("quasar_opportunities").and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
            let completed = opp_json.get(PILOT_1.to_string()).and_then(|c| c.get("completed")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
            assert!(completed.is_empty(), "Client PublishEventPayload must never mark server opportunities completed");
        });

        // Positive: Real stationSvc.Undock records server-observed opportunity completion in quasar_opportunities
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(ship_id)]).await.expect("stationSvc.Undock");
        ctx.quasar_gateway.progression.achievements.save_to_db();

        let reloaded_db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen WorldRepository"));
        reloaded_db.with_snapshot(|s| {
            let opp_json: serde_json::Value = serde_json::from_str(s.quasar_state_blobs.get("quasar_opportunities").unwrap()).unwrap();
            let ch_obj = &opp_json[PILOT_1.to_string()];
            let completed = ch_obj["completed"].as_array().expect("completed array");
            assert!(completed.iter().any(|x| x.as_u64() == Some(u64::from(undock_ach_id))));
            assert!(ch_obj["history"].to_string().contains("opp_undock_01"));
        });
    }
}

pub(crate) mod pve_mission_bonus_timer_payout {
    // Integration tests for corroborated `feat:` rows (Batch 6 Part 2).
    //
    // Covers mission bonus timers, mission offer expiry without penalty,
    // mission level clone/space restrictions, research agent daily missions,
    // ship boarding skill/clone checks, and Mercenary Den anchoring & loot.


    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::quasar_sov_equinox_hub::{id_req, rpc as q_sov_rpc, structure};
    use crate::sovereignty_equinox_gameplay::{
        find_nullsec_planets, make_ball, register_online_pilot, rpc_call, ALLIANCE, CORP, PILOT,
        PILOT_SHIP,
    };
    use eve_db::{
        corp_roles, ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents,
        TrainedSkillRecord, WorldRepository,
    };
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::quasar_gateway::STATUS_CONFLICT;
    use eve_server::router::ServerContext;
    use eve_server::services::agents_missions::{save_rnd_research_entries, RndResearchEntry};
    use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
    use eve_server::services::sov_gameplay::{
        apply_mercenary_den_weapon_damage, MERC_DEN_BASE_HP, MERC_DEN_REINFORCEMENT_SECS,
        TYPE_INFOMORPH_DECRYPTION_KEY, TYPE_MERCENARY_DEN,
    };
    use eve_server::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
    use glam::DVec3;

    const PILOT_A: i64 = 90_100_001;
    const STATION_ID: i64 = 60_003_760;
    const SYS_ID: u32 = 30_000_142;

    fn new_ctx() -> ServerContext {
        let sde = Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = Arc::new(WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        crate::common::pin_clock(&ctx);
        ctx
    }

    fn char_session(
        char_id: i64,
        station_id: Option<i64>,
        solar_system_id: Option<u32>,
        ship_id: Option<i64>,
    ) -> SessionState {
        let mut sess = SessionState::default();
        sess.user_id = Some(char_id as u64);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(1_000_044);
        sess.station_id = station_id;
        sess.solar_system_id = solar_system_id;
        sess.ship_id = ship_id;
        sess
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
    }

    fn reload_db(ctx: &ServerContext) -> Arc<WorldRepository> {
        let db = WorldRepository::new_empty();
        let snap = ctx.db.with_snapshot(|s| s.clone());
        db.with_mut_snapshot(|s| *s = snap);
        Arc::new(db)
    }

    fn ensure_char(ctx: &ServerContext, char_id: i64, name: &str, war_fac: Option<i64>) {
        ctx.db.with_mut_snapshot(|snap| {
            let template = snap.characters[&90_000_001].clone();
            let ch = snap.characters.entry(char_id).or_insert_with(|| template);
            ch.character_id = char_id;
            ch.name = name.to_string();
            ch.corporation_id = 1_000_044;
            ch.solar_system_id = SYS_ID;
            ch.station_id = Some(STATION_ID);
            ch.active_ship_id = char_id + 10_000;
            ch.balance = IskCents::from_isk(10_000_000);
            ch.war_faction_id = war_fac;
        });
    }

    fn colocate_with_agent(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, agent_id: i64) {
        let agent = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
        let sys_id = agent.solar_system_id.unwrap_or(SYS_ID);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = agent.station_id;
                ch.structure_id = None;
            }
        });
        sess.solar_system_id = Some(sys_id);
        sess.station_id = agent.station_id;
        sess.structure_id = None;
    }

    #[tokio::test]
    async fn test_pve_mission_bonus_timer_payout_and_expiry() {
        // parity: feat:pve.mission-bonus-timer
        eve_testkit::attest(&["feat:pve.mission-bonus-timer"]);
        let ctx = new_ctx();
        ensure_char(&ctx, PILOT_A, "BonusRunner", None);
        let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

        let agent = ctx
            .sde
            .agents
            .values()
            .find(|a| a.level == 2 && a.agent_type_id == 2 && a.division_id == 24 && a.solar_system_id.is_some())
            .expect("Level 2 agent");
        colocate_with_agent(&ctx, &mut sess, PILOT_A, agent.agent_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&PILOT_A).unwrap().standings.insert(agent.corporation_id, 5.0);
        });

        let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)])
            .await
            .expect("Offer mission");
        let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
        let accept_rep = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid as i64)])
            .await
            .expect("Accept mission");
        let bonus_expiry = accept_rep.dict_get("bonusExpiryUnix").and_then(|v| v.as_i64()).unwrap() as u64;
        assert!(bonus_expiry > ctx.now_unix());

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(m) = snap.active_missions.get_mut(&(PILOT_A, mid)) {
                m.npcs_remaining = 0;
            }
        });
        let comp = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid as i64)])
            .await
            .expect("Complete within bonus window");
        let bonus_paid = comp.dict_get("bonusReward").and_then(|v| v.as_i64()).unwrap();
        assert!(bonus_paid > 0);

        let reloaded = reload_db(&ctx);
        reloaded.with_snapshot(|snap| {
            assert_eq!(snap.active_missions[&(PILOT_A, mid)].state, "Completed");
        });

        let agent_l1 = ctx
            .sde
            .agents
            .values()
            .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 24 && a.agent_id != agent.agent_id && a.solar_system_id.is_some())
            .expect("Level 1 agent");
        colocate_with_agent(&ctx, &mut sess, PILOT_A, agent_l1.agent_id);
        let offer_b = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent_l1.agent_id)])
            .await
            .expect("Offer mission B");
        let mid_b = offer_b.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
        rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid_b as i64)])
            .await
            .expect("Accept mission B");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(m) = snap.active_missions.get_mut(&(PILOT_A, mid_b)) {
                m.npcs_remaining = 0;
            }
        });
        ctx.advance_secs(10_800);
        let comp_b = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid_b as i64)])
            .await
            .expect("Complete after bonus window");
        assert_eq!(comp_b.dict_get("bonusReward").and_then(|v| v.as_i64()), Some(0));
    }

    #[tokio::test]
    async fn test_pve_mission_offer_expiry_no_penalty_and_cosmos_lockout() {
        // parity: feat:pve.mission-offer-expiry-no-penalty
        eve_testkit::attest(&["feat:pve.mission-offer-expiry-no-penalty"]);
        let ctx = new_ctx();
        ensure_char(&ctx, PILOT_A, "OfferExpiryPilot", None);
        let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

        let agent = ctx
            .sde
            .agents
            .values()
            .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id != 18 && a.solar_system_id.is_some())
            .expect("Level 1 agent");
        colocate_with_agent(&ctx, &mut sess, PILOT_A, agent.agent_id);

        let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent.agent_id)])
            .await
            .expect("Offer mission");
        let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
        let standing_before = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&PILOT_A).and_then(|c| c.standings.get(&agent.corporation_id).copied()).unwrap_or(0.0)
        });

        ctx.advance_secs(605_000);
        let _ = rpc(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[]).await;

        let reloaded = reload_db(&ctx);
        reloaded.with_snapshot(|snap| {
            assert_eq!(snap.active_missions[&(PILOT_A, mid)].state, "Expired");
            let standing_after = snap.characters.get(&PILOT_A).and_then(|c| c.standings.get(&agent.corporation_id).copied()).unwrap_or(0.0);
            assert_eq!(standing_before, standing_after);
        });

        let cosmos_agent = ctx
            .sde
            .agents
            .values()
            .find(|a| a.agent_type_id == 5 && a.solar_system_id.is_some())
            .expect("Data Center agent");
        colocate_with_agent(&ctx, &mut sess, PILOT_A, cosmos_agent.agent_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&PILOT_A).unwrap().standings.insert(cosmos_agent.corporation_id, 5.0);
            snap.active_missions.insert(
                (PILOT_A, 99_999),
                ActiveMissionRecord {
                    mission_id: 99_999,
                    character_id: PILOT_A,
                    agent_id: cosmos_agent.agent_id,
                    solar_system_id: SYS_ID,
                    state: "Expired".into(),
                    current_room_id: 1,
                    npcs_remaining: 0,
                    spawned_npc_ball_ids: Vec::new(),
                    bookmark_id: None,
                },
            );
        });
        let reoffer_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(cosmos_agent.agent_id)])
            .await
            .unwrap_err();
        assert_eq!(reoffer_err.code, "CosmosMissionExpired");
    }

    #[tokio::test]
    async fn test_pve_mission_level_clone_and_space_restrictions() {
        // parity: feat:pve.mission-level-clone-and-space-restrictions
        eve_testkit::attest(&["feat:pve.mission-level-clone-and-space-restrictions"]);
        let ctx = new_ctx();
        ensure_char(&ctx, PILOT_A, "CloneSpacePilot", None);
        let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

        let l4_agent = ctx
            .sde
            .agents
            .values()
            .find(|a| a.level == 4 && a.solar_system_id.is_some() && a.station_id.is_some() && a.agent_type_id == 2 && a.division_id != 18)
            .expect("Level 4 agent");
        colocate_with_agent(&ctx, &mut sess, PILOT_A, l4_agent.agent_id);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&PILOT_A).unwrap();
            ch.clone_grade = 1;
            ch.standings.insert(l4_agent.corporation_id, 8.0);
            ch.standings.insert(l4_agent.agent_id, 8.0);
        });

        let alpha_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(l4_agent.agent_id)])
            .await
            .unwrap_err();
        assert_eq!(alpha_err.code, "CantDoMissionAlphaClone");

        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&PILOT_A).unwrap().clone_grade = 2;
        });
        let omega_ok = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(l4_agent.agent_id)])
            .await
            .expect("Omega clone L4 mission");
        assert!(omega_ok.dict_get("missionID").and_then(|v| v.as_i64()).is_some());

        let l5_highsec = ctx.sde.agents.get(&3008513).expect("L5 highsec agent");
        colocate_with_agent(&ctx, &mut sess, PILOT_A, l5_highsec.agent_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&PILOT_A).unwrap().standings.insert(l5_highsec.corporation_id, 9.0);
        });
        let space_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(l5_highsec.agent_id)])
            .await
            .unwrap_err();
        assert_eq!(space_err.code, "CantDoMission");
    }

    #[tokio::test]
    async fn test_pve_research_agent_daily_mission_and_persistence() {
        // parity: feat:pve.research-agent-daily-mission
        eve_testkit::attest(&["feat:pve.research-agent-daily-mission"]);
        let ctx = new_ctx();
        ensure_char(&ctx, PILOT_A, "RndPilot", None);
        let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));

        let rnd_agent = ctx.sde.agents.get(&3009318).expect("R&D agent 3009318");
        colocate_with_agent(&ctx, &mut sess, PILOT_A, rnd_agent.agent_id);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&PILOT_A).unwrap();
            ch.clone_grade = 2;
            ch.standings.insert(rnd_agent.corporation_id, 6.0);
        });

        let no_res_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_agent.agent_id)])
            .await
            .unwrap_err();
        assert_eq!(no_res_err.code, "ResearchNotActive");

        let now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            save_rnd_research_entries(
                snap,
                PILOT_A,
                &[RndResearchEntry {
                    agent_id: rnd_agent.agent_id,
                    research_type_id: 11444,
                    points_per_day: 120.0,
                    initial_points: 50.0,
                    started_unix: now,
                }],
            );
        });

        let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_agent.agent_id)])
            .await
            .expect("Offer daily R&D mission");
        let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
        assert!(mid > 0);

        let reloaded = reload_db(&ctx);
        reloaded.with_snapshot(|snap| {
            assert_eq!(snap.active_missions[&(PILOT_A, mid)].state, "Offered");
        });
    }

    #[tokio::test]
    async fn test_ships_board_ship_skill_and_clone_check_and_persistence() {
        // parity: feat:ships.board-ship-skill-and-clone-check
        eve_testkit::attest(&["feat:ships.board-ship-skill-and-clone-check"]);
        let ctx = new_ctx();
        ensure_char(&ctx, PILOT_A, "ShipBoarder", None);
        let target_ship_id = 910_001i64;
        let frigate_type_id = 587u32; // Rifter (requires Minmatar Frigate 3330)
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.remove(&PILOT_A);
            snap.items.insert(
                target_ship_id,
                InventoryItemRecord {
                    item_id: target_ship_id,
                    type_id: frigate_type_id,
                    owner_id: PILOT_A,
                    location_id: STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });

        let mut sess = char_session(PILOT_A, Some(STATION_ID), Some(SYS_ID), Some(PILOT_A + 10_000));
        let skill_err = rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(target_ship_id)])
            .await
            .unwrap_err();
        assert_eq!(skill_err.code, "ShipHasSkillPrerequisites");

        let saved_skills = ctx.db.with_snapshot(|s| s.skills.get(&90_000_001).cloned().unwrap_or_default());
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.insert(PILOT_A, saved_skills);
        });
        let board_rep = rpc(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(target_ship_id)])
            .await
            .expect("Board ship with required skill");
        assert_eq!(board_rep.dict_get("shipID").and_then(|v| v.as_i64()), Some(target_ship_id));
        assert_eq!(sess.ship_id, Some(target_ship_id));

        let reloaded = reload_db(&ctx);
        reloaded.with_snapshot(|snap| {
            assert_eq!(snap.characters[&PILOT_A].active_ship_id, target_ship_id);
        });
    }

    #[tokio::test]
    async fn test_sovereignty_mercenary_den_anchoring_validation_and_persistence() {
        // parity: feat:sovereignty.mercenary-den-anchoring
        eve_testkit::attest(&["feat:sovereignty.mercenary-den-anchoring"]);
        let ctx = new_ctx();
        crate::common::pin_clock(&ctx);
        let (null_sys, temp_planet, _, _, non_temp_planet) = find_nullsec_planets(&ctx);
        let skyhook_id = 1_050_000_002i64;
        let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

        let mut s_owner = register_online_pilot(
            &ctx,
            72_001,
            "demo",
            "demo-secret",
            PILOT,
            CORP,
            Some(ALLIANCE),
            null_sys,
            PILOT_SHIP,
            639,
            DVec3::new(50_000.0, 0.0, 0.0),
            corp_roles::ROLE_DIRECTOR,
        )
        .await;

        ctx.db.with_mut_snapshot(|snap| {
            snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
            snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
            snap.skills.entry(PILOT).or_default().insert(
                TYPE_MERCENARY_DEN_MANAGEMENT,
                TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 1, skill_points: 25_000.0 },
            );
            snap.items.insert(
                990_201,
                InventoryItemRecord {
                    item_id: 990_201,
                    type_id: TYPE_MERCENARY_DEN,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag: InventoryFlag::Cargo,
                    quantity: 2,
                    ..Default::default()
                },
            );
        });
        let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

        assert_eq!(
            rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
                .await
                .unwrap_err()
                .code,
            "MercenaryDenTooFarFromSkyhook"
        );

        let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.sov_runtime.skyhook_planets.insert(skyhook_id, non_temp_planet);
        });
        assert_eq!(
            rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(non_temp_planet)])
                .await
                .unwrap_err()
                .code,
            "PlanetNotTemperate"
        );

        ctx.db.with_mut_snapshot(|snap| {
            snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        });
        let den_rep = rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
            .await
            .expect("Anchor Mercenary Den");
        let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

        let reloaded = reload_db(&ctx);
        reloaded.with_snapshot(|snap| {
            assert!(snap.sov_runtime.mercenary_dens.contains_key(&den_id));
        });
    }

    async fn setup_anchored_merc_den(ctx: &ServerContext, sess_id: u64, with_hub: bool) -> (SessionState, i64, i64) {
        let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(ctx);
        let hub_id = 1_050_000_001i64;
        let skyhook_id = 1_050_000_002i64;
        let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);
        let mut s_owner = register_online_pilot(
            ctx, sess_id, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
            null_sys, PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
        )
        .await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
            if with_hub {
                snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
            }
            snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
            snap.skills.entry(PILOT).or_default().insert(
                TYPE_MERCENARY_DEN_MANAGEMENT,
                TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 1, skill_points: 25_000.0 },
            );
            snap.items.insert(
                990_200 + sess_id as i64,
                InventoryItemRecord {
                    item_id: 990_200 + sess_id as i64,
                    type_id: TYPE_MERCENARY_DEN,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;
        let den_rep = rpc_call(
            ctx, &mut s_owner, "structureDeployment", "Anchor",
            &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
        )
        .await
        .expect("Anchor Mercenary Den");
        let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
        (s_owner, hub_id, den_id)
    }

    #[tokio::test]
    async fn test_sovereignty_mercenary_den_reinforcement_loot_and_wreck() {
        // parity: feat:sovereignty.mercenary-den-reinforcement-loot
        eve_testkit::attest(&["feat:sovereignty.mercenary-den-reinforcement-loot"]);
        let ctx = new_ctx();
        let (mut s_owner, _hub_id, den_id) = setup_anchored_merc_den(&ctx, 72_004, false).await;

        ctx.db.with_mut_snapshot(|snap| {
            for idx in 0..8i64 {
                snap.items.insert(
                    994_000 + idx,
                    InventoryItemRecord {
                        item_id: 994_000 + idx,
                        type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                        owner_id: PILOT,
                        location_id: den_id,
                        flag: InventoryFlag::Hangar,
                        quantity: 10,
                        ..Default::default()
                    },
                );
            }
        });

        let reinf_rep = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP + 100.0)
            .await
            .expect("Deplete shield into reinforcement");
        assert_eq!(reinf_rep.dict_get("state").and_then(|v| v.as_str()), Some("HullReinforced"));
        assert_eq!(
            q_sov_rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64))
                .await
                .status_code,
            STATUS_CONFLICT
        );

        ctx.advance_secs(MERC_DEN_REINFORCEMENT_SECS + 120);
        let dest_rep = apply_mercenary_den_weapon_damage(&ctx, den_id, MERC_DEN_BASE_HP + 100.0)
            .await
            .expect("Deplete hull in vulnerability window");
        assert_eq!(dest_rep.dict_get("state").and_then(|v| v.as_str()), Some("Destroyed"));
        let dropped_stacks = dest_rep.dict_get("droppedStacks").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!((1..=7).contains(&dropped_stacks));
    }
}

pub(crate) mod cluster_session_localization_qa {
    // End-to-end integration tests for `work/open-feats-and-rpcs-batch2`.

    use std::collections::HashMap;

    use eve_db::{
        AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
        UpwellStructureRecord,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;

    fn make_gm_session(char_id: i64, corp_id: i64, alliance_id: Option<i64>, sys_id: u32) -> SessionState {
        let mut sess = SessionState::new(50_001);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.alliance_id = alliance_id;
        sess.solar_system_id = Some(sys_id);
        sess.role = eve_db::service_roles::ROLE_ADMIN
            | eve_db::service_roles::ROLE_GMH
            | eve_machonet::service_roles::ROLE_QA
            | eve_machonet::service_roles::ROLE_PROGRAMMER;
        sess
    }

    fn make_player_session(
        sess_id: u64,
        user_id: u64,
        char_id: i64,
        corp_id: i64,
        alliance_id: Option<i64>,
        sys_id: u32,
    ) -> SessionState {
        let mut sess = SessionState::new(sess_id);
        sess.user_id = Some(user_id);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.alliance_id = alliance_id;
        sess.solar_system_id = Some(sys_id);
        sess
    }

    #[tokio::test]
    async fn test_cluster_session_localization_qa_rpcs_e2e() {
        // parity: rpc:DB2.CallProc
        // parity: rpc:raffleMgr.QA_SeedRaffles, rpc:sessionMgr.EndAllGameSessions, rpc:sessionMgr.RemoveSessionsFromServer, rpc:userSvc.ProcessEVESessionCreated
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let kw = HashMap::new();

        let mut gm = make_gm_session(90_000_001, 98_000_001, Some(99_000_001), 30_000_142);
        let mut player = make_player_session(50_002, 2, 90_000_002, 98_000_001, Some(99_000_001), 30_000_142);

        // 1. userSvc.ProcessEVESessionCreated
        let created = ctx
            .dispatch_rpc(&mut player, "userSvc", None, "ProcessEVESessionCreated", &[], &kw, None)
            .await
            .expect("ProcessEVESessionCreated");
        assert_eq!(created, PyRep::None);
        ctx.db.with_snapshot(|snap| {
            let sessions_json = snap
                .service_state_blobs
                .get("user_sessions")
                .expect("user_sessions blob");
            assert!(sessions_json.contains("50002"), "Session 50002 must be recorded in user_sessions");
        });

        // 2. loadService.StartLoad / GetTotalStats / StopLoad
        let denied = ctx
            .dispatch_rpc(&mut player, "loadService", None, "StartLoad", &[], &kw, None)
            .await;
        assert!(denied.is_err(), "Non-admin must be denied loadService.StartLoad");

        ctx.dispatch_rpc(
            &mut gm,
            "loadService",
            None,
            "StartLoad",
            &[PyRep::String("stress_test_v24".into())],
            &kw,
            None,
        )
        .await
        .expect("StartLoad");

        let stats_active = ctx
            .dispatch_rpc(&mut gm, "loadService", None, "GetTotalStats", &[], &kw, None)
            .await
            .expect("GetTotalStats");
        assert_eq!(stats_active.dict_get("active"), Some(&PyRep::Bool(true)));
        assert_eq!(stats_active.dict_get("runs_started"), Some(&PyRep::Integer(1)));

        ctx.dispatch_rpc(&mut gm, "loadService", None, "StopLoad", &[], &kw, None)
            .await
            .expect("StopLoad");
        let stats_stopped = ctx
            .dispatch_rpc(&mut gm, "loadService", None, "GetTotalStats", &[], &kw, None)
            .await
            .expect("GetTotalStats after stop");
        assert_eq!(stats_stopped.dict_get("active"), Some(&PyRep::Bool(false)));

        // 3. localizationServer.UpdateLocalizationQAWrap (fail-closed UnimplementedRPC) & ReloadFSDPickle
        let qa_wrap_err = ctx
            .dispatch_rpc(
                &mut gm,
                "localizationServer",
                None,
                "UpdateLocalizationQAWrap",
                &[PyRep::Dict(vec![(
                    PyRep::Integer(12345),
                    PyRep::String("QA Override".into()),
                )])],
                &kw,
                None,
            )
            .await
            .expect_err("UpdateLocalizationQAWrap must fail-closed with UnimplementedRPC");
        assert_eq!(qa_wrap_err.code, "UnimplementedRPC");

        let reload_err = ctx.dispatch_rpc(
            &mut gm,
            "localizationServer",
            None,
            "ReloadFSDPickle",
            &[PyRep::String("localization_en_us".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(reload_err.code, "UnimplementedRPC");

        // 4. DB2.CallProc & debug.Exec
        let polaris = ctx.dispatch_rpc(&mut gm, "DB2", None, "CallProc", &[PyRep::String("zcluster.Nodes_PolarisID".into())], &kw, None).await.expect("DB2.CallProc");
        assert_eq!(polaris, PyRep::Integer(1));
        let users_rowset = ctx.dispatch_rpc(&mut gm, "DB2", None, "CallProc", &[PyRep::String("zuser.Users_SelectByRole".into())], &kw, None).await.expect("DB2.CallProc zuser.Users_SelectByRole");
        assert!(matches!(users_rowset, PyRep::Rowset { .. }));
        let unknown_proc_err = ctx.dispatch_rpc(&mut gm, "DB2", None, "CallProc", &[PyRep::String("zcluster.UnknownProc".into())], &kw, None).await.expect_err("DB2.CallProc unknown proc must return UnimplementedRPC");
        assert_eq!(unknown_proc_err.code, "UnimplementedRPC");

        // Non-GM client session may call zcluster.Attacks_Insert, but is rejected on zuser.Users_SelectByRole
        let mut player_sess = SessionState::new(999);
        player_sess.user_id = Some(9001);
        player_sess.character_id = Some(91_000_001);
        let attack_ok = ctx.dispatch_rpc(&mut player_sess, "DB2", None, "CallProc", &[PyRep::String("zcluster.Attacks_Insert".into()), PyRep::Integer(9001), PyRep::String("MalformedPacket".into())], &kw, None).await.expect("non-GM client must be allowed to call zcluster.Attacks_Insert");
        assert_eq!(attack_ok, PyRep::None);
        let unpriv_users_err = ctx.dispatch_rpc(&mut player_sess, "DB2", None, "CallProc", &[PyRep::String("zuser.Users_SelectByRole".into()), PyRep::Integer(0)], &kw, None).await.expect_err("non-GM client must be rejected on zuser.Users_SelectByRole");
        assert_eq!(unpriv_users_err.message, "NotAuthorized");
        let debug_exec_err = ctx.dispatch_rpc(&mut player_sess, "debug", None, "Exec", &[PyRep::String("status".into())], &kw, None).await.expect_err("debug.Exec must be rejected for unprivileged player");
        assert_eq!(debug_exec_err.message, "NotAuthorized");
        let gm_debug_exec_err = ctx.dispatch_rpc(&mut gm, "debug", None, "Exec", &[PyRep::String("status".into())], &kw, None).await.expect_err("debug.Exec must fail-closed with UnimplementedRPC for GM");
        assert_eq!(gm_debug_exec_err.code, "UnimplementedRPC");

        // 5. raffleMgr.QA_SeedRaffles
        let seeded = ctx
            .dispatch_rpc(
                &mut gm,
                "raffleMgr",
                None,
                "QA_SeedRaffles",
                &[PyRep::Integer(2)],
                &kw,
                None,
            )
            .await
            .expect("QA_SeedRaffles");
        let PyRep::List(seeded_ids) = seeded else {
            panic!("Expected list of seeded raffle IDs");
        };
        assert_eq!(seeded_ids.len(), 2);

        // 6. sessionMgr.RemoveSessionsFromServer & EndAllGameSessions
        ctx.dispatch_rpc(
            &mut gm,
            "sessionMgr",
            None,
            "RemoveSessionsFromServer",
            &[PyRep::None, PyRep::List(vec![PyRep::Integer(50_002)])],
            &kw,
            None,
        )
        .await
        .expect("RemoveSessionsFromServer");

        ctx.dispatch_rpc(
            &mut gm,
            "sessionMgr",
            None,
            "EndAllGameSessions",
            &[],
            &kw,
            None,
        )
        .await
        .expect("EndAllGameSessions");
    }

    #[tokio::test]
    async fn test_lookup_warable_and_sov_structures_e2e() {
        // parity: rpc:lookupSvc.LookupWarableCorporationsOrAlliances, feat:ui.lookup, rpc:sovMgr.GetSovStructuresInfoForSolarSystem
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let kw = HashMap::new();

        let null_sys: u32 = 30_001_269;
        let mut sess = make_player_session(50_010, 1, 90_000_001, 98_000_001, Some(99_000_001), null_sys);

        ctx.db.with_mut_snapshot(|snap| {
            // Standalone war-eligible corp (owns structure)
            snap.corporations.insert(
                98_000_101,
                CorporationRecord {
                    corporation_id: 98_000_101,
                    name: "Vanguard Horizon".into(),
                    ticker: "VGHD".into(),
                    ceo_id: 90_000_101,
                    alliance_id: None,
                    members: [90_000_101].into_iter().collect(),
                    ..Default::default()
                },
            );
            // Standalone non-war-eligible corp (no structure)
            snap.corporations.insert(
                98_000_102,
                CorporationRecord {
                    corporation_id: 98_000_102,
                    name: "Vanguard Peacekeepers".into(),
                    ticker: "VGPK".into(),
                    ceo_id: 90_000_102,
                    alliance_id: None,
                    members: [90_000_102].into_iter().collect(),
                    ..Default::default()
                },
            );
            // External alliance + member corp with structure
            snap.corporations.insert(
                98_000_103,
                CorporationRecord {
                    corporation_id: 98_000_103,
                    name: "Vanguard Sov Holding".into(),
                    ticker: "VGSH".into(),
                    ceo_id: 90_000_103,
                    alliance_id: Some(99_000_103),
                    members: [90_000_103].into_iter().collect(),
                    ..Default::default()
                },
            );
            snap.alliances.insert(
                99_000_103,
                AllianceRecord {
                    alliance_id: 99_000_103,
                    name: "Vanguard Coalition".into(),
                    short_name: "VGC".into(),
                    executor_corp_id: 98_000_103,
                    member_corp_ids: [98_000_103].into_iter().collect(),
                },
            );

            // Non-sov Upwell Astrahus in null_sys (must NOT appear in GetSovStructuresInfoForSolarSystem)
            snap.structures.insert(
                1_030_000_000_101,
                UpwellStructureRecord {
                    structure_id: 1_030_000_000_101,
                    type_id: 35832, // Astrahus
                    owner_corp_id: 98_000_101,
                    solar_system_id: null_sys,
                    name: "Vanguard Astrahus".into(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            // Sovereignty Hub (32458) owned by 98_000_103 (alliance 99_000_103)
            snap.structures.insert(
                1_030_000_000_103,
                UpwellStructureRecord {
                    structure_id: 1_030_000_000_103,
                    type_id: 32458, // Sovereignty Hub
                    owner_corp_id: 98_000_103,
                    solar_system_id: null_sys,
                    name: "Vanguard Sov Hub".into(),
                    position: [100_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            snap.sov_runtime
                .adm_vulnerability_windows
                .insert(1_030_000_000_103, (36_000, 46_800));
        });

        // 1. LookupWarableCorporationsOrAlliances
        let warable_res = ctx
            .dispatch_rpc(
                &mut sess,
                "lookupSvc",
                None,
                "LookupWarableCorporationsOrAlliances",
                &[PyRep::String("Vanguard".into()), PyRep::Bool(false)],
                &kw,
                None,
            )
            .await
            .expect("LookupWarableCorporationsOrAlliances");
        let PyRep::List(warable) = warable_res else {
            panic!("Expected list");
        };
        let ids: Vec<i64> = warable
            .iter()
            .filter_map(|r| r.dict_get("ownerID").and_then(PyRep::as_i64))
            .collect();
        assert!(ids.contains(&98_000_101), "Standalone corp with structure must be warable");
        assert!(!ids.contains(&98_000_102), "Structureless corp must NOT be warable");
        assert!(!ids.contains(&98_000_103), "Alliance member corp must NOT be listed individually");
        assert!(ids.contains(&99_000_103), "Alliance with structure-owning member must be warable");

        // 2. GetSovStructuresInfoForSolarSystem
        let sov_res = ctx
            .dispatch_rpc(
                &mut sess,
                "sovMgr",
                None,
                "GetSovStructuresInfoForSolarSystem",
                &[PyRep::Integer(null_sys as i64)],
                &kw,
                None,
            )
            .await
            .expect("GetSovStructuresInfoForSolarSystem");
        let PyRep::List(sov_structs) = sov_res else {
            panic!("Expected structures list");
        };
        assert_eq!(sov_structs.len(), 1, "Only Sovereignty Hub (32458) must be returned, not Astrahus");
        let hub = &sov_structs[0];
        assert_eq!(hub.dict_get("structureID"), Some(&PyRep::Integer(1_030_000_000_103)));
        assert_eq!(hub.dict_get("allianceID"), Some(&PyRep::Integer(99_000_103)));
        let v_start = hub.dict_get("vulnerableStartTime").and_then(PyRep::as_i64).unwrap_or(0);
        let v_end = hub.dict_get("vulnerableEndTime").and_then(PyRep::as_i64).unwrap_or(0);
        assert!(v_end > v_start && v_start > 100_000_000_000_000_000, "Must return valid Windows FILETIME window");
    }

    #[tokio::test]
    async fn test_homefront_sites_and_hypernet_raffle_e2e() {
        // parity: rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances, rpc:raffleProxy.CreateRaffle, rpc:raffleProxy.BuyRandomTickets, rpc:raffleProxy.GetActiveHistoricPrices
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let kw = HashMap::new();

        let jita: u32 = 30_000_142;
        let jita_station: i64 = 60_003_760;
        let unseeded_hisec: u32 = 30_000_144; // Perimeter
        let mut sess = make_player_session(50_020, 1, 90_000_001, 98_000_001, Some(99_000_001), jita);
        sess.station_id = Some(jita_station);

        // 1. GetHomefrontSiteInstances must NOT lazily fabricate sites in unseeded high-sec systems
        let unseeded_res = ctx
            .dispatch_rpc(
                &mut sess,
                "dungeonInstanceCacheMgr",
                None,
                "GetHomefrontSiteInstances",
                &[PyRep::Integer(unseeded_hisec as i64)],
                &kw,
                None,
            )
            .await
            .expect("GetHomefrontSiteInstances unseeded");
        let PyRep::Dict(unseeded_outer) = unseeded_res else {
            panic!("Expected dict");
        };
        let PyRep::Dict(unseeded_inner) = &unseeded_outer[0].1 else {
            panic!("Expected inner dict");
        };
        assert!(unseeded_inner.is_empty(), "Unseeded system must not lazily fabricate Homefront sites");

        // 2. HyperNet CreateRaffle, GetActiveHistoricPrices, BuyRandomTickets
        let raffle_item_id: i64 = 900_000_555;
        let hypercore_item_id: i64 = 900_000_556;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.station_id = Some(jita_station);
                ch.structure_id = None;
                ch.balance = IskCents(500_000_000 * 100);
            }
            snap.items.insert(
                raffle_item_id,
                InventoryItemRecord {
                    item_id: raffle_item_id,
                    type_id: 587, // Rifter
                    owner_id: 90_000_001,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                hypercore_item_id,
                InventoryItemRecord {
                    item_id: hypercore_item_id,
                    type_id: 52_568, // HyperCore
                    owner_id: 90_000_001,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 100,
                    singleton: false,
                    ..Default::default()
                },
            );
        });

        let created = ctx
            .dispatch_rpc(
                &mut sess,
                "raffleProxy",
                None,
                "CreateRaffle",
                &[
                    PyRep::Integer(raffle_item_id),
                    PyRep::Integer(8),
                    PyRep::Real(1_000_000.0),
                ],
                &kw,
                None,
            )
            .await
            .expect("CreateRaffle");
        let raffle_id = created.dict_get("raffle_id").and_then(PyRep::as_i64).expect("raffle_id");

        let prices_active = ctx
            .dispatch_rpc(
                &mut sess,
                "raffleProxy",
                None,
                "GetActiveHistoricPrices",
                &[PyRep::Integer(587)],
                &kw,
                None,
            )
            .await
            .expect("GetActiveHistoricPrices");
        let PyRep::Tuple(p_tup) = prices_active else {
            panic!("Expected tuple");
        };
        assert_eq!(p_tup[0].as_f64(), Some(8_000_000.0));

        let bought = ctx
            .dispatch_rpc(
                &mut sess,
                "raffleProxy",
                None,
                "BuyRandomTickets",
                &[PyRep::Integer(raffle_id), PyRep::Integer(8)],
                &kw,
                None,
            )
            .await
            .expect("BuyRandomTickets");
        assert_eq!(bought.dict_get("is_finished"), Some(&PyRep::Bool(true)));
        assert_eq!(bought.dict_get("winner_id"), Some(&PyRep::Integer(90_000_001)));
    }

    #[tokio::test]
    async fn test_navigation_jump_fatigue_cloak_and_thera_e2e() {
        // parity: feat:navigation.jump-fatigue-formula, feat:navigation.thera-npc-stations
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let kw = HashMap::new();
        let char_id = 90_000_001i64;
        let mut sess = make_gm_session(char_id, 98_000_001, Some(99_000_001), 30_000_142);

        // 1. Jump fatigue formula (V24: min 10 min fatigue, 1 min cooldown, 1 + LY scaling)
        let (cd_min, fat_min) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 5.0, 0.0);
        assert!((cd_min - 6.0).abs() < 1e-6, "Cooldown for 5 LY from 0 fatigue must be 6 min");
        assert!((fat_min - 60.0).abs() < 1e-6, "Fatigue for 5 LY from 0 fatigue must be 60 min");

        // Non-zero existing fatigue: 30 min * (1 + 5) = 180 min
        let (cd_nz, fat_nz) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(30.0, 5.0, 0.0);
        assert!((cd_nz - 6.0).abs() < 1e-6, "Cooldown for 5 LY from 30 min fatigue must be 6 min");
        assert!((fat_nz - 180.0).abs() < 1e-6, "Fatigue for 5 LY from 30 min fatigue must be 180 min");

        // 5h (300 min) fatigue cap
        let (_, fat_capped) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(100.0, 5.0, 0.0);
        assert!((fat_capped - 300.0).abs() < 1e-6, "Fatigue must be capped at 300 min (5h)");

        // -75% Black Ops and -90% industrial reductions
        let (cd_blops, fat_blops) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 8.0, 0.75);
        assert!((cd_blops - 3.0).abs() < 1e-6, "Black Ops -75% on 8 LY -> 2 effective LY -> 3 min cooldown");
        assert!((fat_blops - 30.0).abs() < 1e-6, "Black Ops -75% on 8 LY -> 30 min fatigue");

        let (cd_ind, fat_ind) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 10.0, 0.90);
        assert!((cd_ind - 2.0).abs() < 1e-6, "Industrial -90% on 10 LY -> 1 effective LY -> 2 min cooldown");
        assert!((fat_ind - 20.0).abs() < 1e-6, "Industrial -90% on 10 LY -> 20 min fatigue");

        // 30m cooldown cap & unclamped pre-jump fatigue/10 > (1+L): 120 min over 4 LY -> max(5, 12) = 12 min
        let (capped_cd, capped_fat) =
            eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(300.0, 35.0, 0.0);
        assert!((capped_cd - 30.0).abs() < 1e-6 && (capped_fat - 300.0).abs() < 1e-6);
        let (unclamped_cd, unclamped_fat) =
            eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(120.0, 4.0, 0.0);
        assert!((unclamped_cd - 12.0).abs() < 1e-6 && (unclamped_fat - 300.0).abs() < 1e-6);

        // End-to-end jump via structureCynoBeaconMgr.CmdJumpToStructureBeacon + jumpTimers.GetTimers + clock advancement
        let jump_beacon_id = 1_040_000_007_801_i64;
        let jump_fuel_item_id = 990_007_801_i64;
        let jump_dest_sys_id = 30_000_204_u32; // Ohkunen (low-sec, 4.80 LY from Jita)
        let (jump_ship_id, char_corp_id) = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&char_id)
                .map(|c| (c.active_ship_id, c.corporation_id))
                .unwrap_or((100_000_001, 98_000_001))
        });
        let jdc_type_id = ctx.sde.types.values().find(|t| t.name == "Jump Drive Calibration").map(|t| t.type_id).unwrap_or(3456);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = 30_000_142;
                ch.station_id = None;
                ch.structure_id = None;
                ch.jump_cooldown_until_unix = 0;
                ch.jump_fatigue_until_unix = 0;
                ch.jump_fatigue_minutes = 0.0;
            }
            snap.structures.insert(
                jump_beacon_id,
                eve_db::UpwellStructureRecord {
                    structure_id: jump_beacon_id,
                    type_id: 35840, // Pharolux Cyno Beacon
                    owner_corp_id: char_corp_id,
                    solar_system_id: jump_dest_sys_id,
                    name: "Ohkunen Pharolux Cyno Beacon".to_string(),
                    position: [15_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".to_string(),
                    quantum_core_installed: true,
                    fuel_blocks: 50,
                    shield_hp: 50_000.0,
                    armor_hp: 50_000.0,
                    hull_hp: 50_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            snap.skills.entry(char_id).or_default().insert(jdc_type_id, eve_db::TrainedSkillRecord { type_id: jdc_type_id, level: 5, skill_points: 1_280_000.0 });
            snap.items.insert(
                jump_fuel_item_id,
                eve_db::InventoryItemRecord {
                    item_id: jump_fuel_item_id,
                    type_id: 16274, // Helium Isotopes
                    owner_id: char_id,
                    location_id: jump_ship_id,
                    flag: eve_db::InventoryFlag::SpecializedFuelBay,
                    quantity: 50_000,
                    singleton: false,
                    ..Default::default()
                },
            );
        });
        let jita_handle = ctx.systems.get_or_spawn(30_000_142);
        jita_handle
            .spawn_ball(eve_destiny::Ball {
                id: jump_ship_id,
                type_id: 23757, // Archon carrier
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 85.0,
                inertia: 0.04,
                mass: 1_230_000_000.0,
                radius: 1_600.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await
            .expect("spawn capital ship in Jita for beacon jump");
        sess.ship_id = Some(jump_ship_id);
        sess.solar_system_id = Some(30_000_142);

        async fn get_timers(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, kw: &HashMap<String, PyRep>) -> (i64, i64, i64) {
            let res = ctx.dispatch_rpc(sess, "jumpTimers", None, "GetTimers", &[PyRep::Integer(char_id)], kw, None).await.expect("GetTimers");
            let PyRep::KeyVal(e) = res else { panic!("GetTimers must return KeyVal") };
            (e.get("jumpActivation").and_then(PyRep::as_i64).unwrap_or(0), e.get("jumpFatigue").and_then(PyRep::as_i64).unwrap_or(0), e.get("lastUpdated").and_then(PyRep::as_i64).unwrap_or(0))
        }
        ctx.dispatch_rpc(&mut sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(jump_dest_sys_id as i64), PyRep::Integer(jump_beacon_id)], &kw, None).await.expect("carrier jump");
        let (act_ticks, fat_ticks, upd_ticks) = get_timers(&ctx, &mut sess, char_id, &kw).await;
        assert!(act_ticks > upd_ticks && fat_ticks > act_ticks, "Active cooldown & fatigue must be in future");

        // Partial real-time decay check (300s) then full expiry (7200s)
        ctx.advance_time_seconds(300);
        let (_, mid_fat, mid_upd) = get_timers(&ctx, &mut sess, char_id, &kw).await;
        assert!(mid_fat > mid_upd && mid_fat == fat_ticks, "Fatigue timestamp remains fixed while now advances");
        ctx.advance_time_seconds(7_200);
        let (_, dec_fat, _) = get_timers(&ctx, &mut sess, char_id, &kw).await;
        let now_filetime = (ctx.now_unix() as i64 + 11_644_473_600) * 10_000_000;
        assert!(dec_fat < now_filetime, "Expired fatigue must be in the past after clock advancement");

        // Live jump RPCs on -75% hull (Black Ops 22428) and -90% hull (Jump Freighter 28846)
        let mut hull_fatigues = Vec::new();
        for hull_type_id in [22428_u32, 28846_u32] {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.solar_system_id = 30_000_142;
                    ch.jump_cooldown_until_unix = 0;
                    ch.jump_fatigue_until_unix = 0;
                    ch.jump_fatigue_minutes = 0.0;
                }
                if let Some(item) = snap.items.get_mut(&jump_ship_id) { item.type_id = hull_type_id; }
                if let Some(fuel) = snap.items.get_mut(&jump_fuel_item_id) { fuel.type_id = if hull_type_id == 28846 { 17889 } else { 16274 }; fuel.quantity = 50_000; }
            });
            sess.solar_system_id = Some(30_000_142);
            let _ = jita_handle.spawn_ball(eve_destiny::Ball { id: jump_ship_id, type_id: hull_type_id, position: glam::DVec3::ZERO, velocity: glam::DVec3::ZERO, max_velocity: 85.0, inertia: 0.04, mass: 1_230_000_000.0, radius: 1_600.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop }).await;
            ctx.dispatch_rpc(&mut sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(jump_dest_sys_id as i64), PyRep::Integer(jump_beacon_id)], &kw, None).await.expect("hull jump");
            let (h_act, h_fat, h_upd) = get_timers(&ctx, &mut sess, char_id, &kw).await;
            assert!(h_act > h_upd && h_fat > h_act);
            hull_fatigues.push(h_fat - h_upd);
            ctx.advance_time_seconds(7_200);
        }
        assert!(hull_fatigues[0] < (fat_ticks - upd_ticks) && hull_fatigues[1] < hull_fatigues[0], "-75% and -90% hulls must reduce live jump fatigue");

        // 2. Thera (31_000_005) NPC stations in SDE
        let thera_stations: Vec<_> = ctx
            .sde
            .stations
            .values()
            .filter(|s| s.solar_system_id == 31_000_005)
            .collect();
        assert!(!thera_stations.is_empty(), "Thera (31000005) must have NPC stations in SDE");
    }
}

pub(crate) mod milestones_seasons_cache_setting {
    // End-to-end integration tests for `work/open-feats-and-rpcs-batch2` (part 2).

    use std::collections::HashMap;

    use eve_db::{
        AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
        UpwellStructureRecord,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;

    fn make_gm_session(char_id: i64, corp_id: i64, alliance_id: Option<i64>, sys_id: u32) -> SessionState {
        let mut sess = make_player_session(51_001, 1, char_id, corp_id, alliance_id, sys_id);
        sess.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMH
            | eve_machonet::service_roles::ROLE_QA | eve_machonet::service_roles::ROLE_PROGRAMMER;
        sess
    }

    fn make_player_session(sess_id: u64, user_id: u64, char_id: i64, corp_id: i64, alliance_id: Option<i64>, sys_id: u32) -> SessionState {
        let mut sess = SessionState::new(sess_id);
        sess.user_id = Some(user_id);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.alliance_id = alliance_id;
        sess.solar_system_id = Some(sys_id);
        sess.station_id = Some(60_003_760);
        sess
    }

    fn keyval_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
        match rep {
            PyRep::KeyVal(entries) => entries.get(key),
            PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
            _ => None,
        }
    }

    #[tokio::test]
    async fn test_milestones_seasons_and_cache_setting_e2e() {
        // parity: rpc:milestoneMgr.ProcessCharacterLogon, rpc:seasonManager.fill_up_and_return_challenges_for_character, rpc:cache.Setting
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let kw = HashMap::new();

        let mut unauth = SessionState::new(51_099);
        let mut gm = make_gm_session(90_000_001, 98_000_001, Some(99_000_001), 30_000_142);
        let mut player = make_player_session(51_002, 2, 90_000_002, 98_000_001, Some(99_000_001), 30_000_142);

        // 1. milestoneMgr.ProcessCharacterLogon & ClaimRewards negative/auth + state mutation
        assert!(
            ctx.dispatch_rpc(&mut unauth, "milestoneMgr", None, "ProcessCharacterLogon", &[], &kw, None)
                .await
                .is_err(),
            "unauthenticated ProcessCharacterLogon must fail"
        );
        let prereq_err = ctx
            .dispatch_rpc(&mut player, "milestoneMgr", None, "ClaimRewards", &[PyRep::Integer(1)], &kw, None)
            .await
            .expect_err("ClaimRewards before ProcessCharacterLogon must fail");
        assert_eq!(prereq_err.code, "MilestonePrerequisiteNotMet");

        let logon_res = ctx
            .dispatch_rpc(&mut player, "milestoneMgr", None, "ProcessCharacterLogon", &[], &kw, None)
            .await
            .expect("ProcessCharacterLogon");
        assert_eq!(logon_res, PyRep::None);

        // Calling ClaimRewards(0) after ProcessCharacterLogon now reaches milestone_id validation
        let invalid_id_err = ctx
            .dispatch_rpc(&mut player, "milestoneMgr", None, "ClaimRewards", &[PyRep::Integer(0)], &kw, None)
            .await
            .expect_err("ClaimRewards(0) must fail with InvalidMilestoneID");
        assert_eq!(invalid_id_err.code, "InvalidMilestoneID");

        // 2. seasonManager.fill_up_and_return_challenges_for_character & challenge lifecycle
        assert!(
            ctx.dispatch_rpc(
                &mut unauth,
                "seasonManager",
                None,
                "fill_up_and_return_challenges_for_character",
                &[],
                &kw,
                None,
            )
            .await
            .is_err(),
            "unauthenticated fill_up_and_return_challenges_for_character must fail"
        );

        let filled = ctx
            .dispatch_rpc(
                &mut player,
                "seasonManager",
                None,
                "fill_up_and_return_challenges_for_character",
                &[],
                &kw,
                None,
            )
            .await
            .expect("fill_up_and_return_challenges_for_character");
        let challenges = match filled {
            PyRep::Dict(dict) => dict,
            other => panic!("expected Dict of challenges, got {other:?}"),
        };
        assert!(!challenges.is_empty(), "expected non-empty active challenges");
        let first_ch_id = challenges[0].0.as_i64().expect("challengeID");

        // Claiming uncompleted challenge must fail
        let uncompleted_err = ctx
            .dispatch_rpc(
                &mut player,
                "seasonManager",
                None,
                "claim_challenge_rewards",
                &[PyRep::Integer(first_ch_id)],
                &kw,
                None,
            )
            .await
            .expect_err("uncompleted challenge claim must fail");
        assert_eq!(uncompleted_err.code, "ChallengeNotCompleted");

        // 3. cache.Setting auth, persistence, and default lookup
        let unauth_write = ctx
            .dispatch_rpc(
                &mut player,
                "cache",
                None,
                "Setting",
                &[
                    PyRep::String("zsystem".into()),
                    PyRep::String("Batch2Key".into()),
                    PyRep::String("Batch2Val".into()),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("non-GM cache.Setting write must fail");
        assert_eq!(unauth_write.code, "UserError");

        let gm_write = ctx
            .dispatch_rpc(
                &mut gm,
                "cache",
                None,
                "Setting",
                &[
                    PyRep::String("zsystem".into()),
                    PyRep::String("Batch2Key".into()),
                    PyRep::String("Batch2Val".into()),
                ],
                &kw,
                None,
            )
            .await
            .expect("GM cache.Setting write");
        assert_eq!(gm_write, PyRep::String("Batch2Val".into()));

        let read_back = ctx
            .dispatch_rpc(
                &mut player,
                "cache",
                None,
                "Setting",
                &[PyRep::String("zsystem".into()), PyRep::String("Batch2Key".into())],
                &kw,
                None,
            )
            .await
            .expect("cache.Setting read");
        assert_eq!(read_back, PyRep::String("Batch2Val".into()));

        let default_ext = ctx
            .dispatch_rpc(
                &mut player,
                "cache",
                None,
                "Setting",
                &[PyRep::String("SP-Allowed-Extensions".into())],
                &kw,
                None,
            )
            .await
            .expect("cache.Setting default");
        assert_eq!(
            default_ext,
            PyRep::String("txt,csv,log,png,jpg,dds,json,yaml".into())
        );
    }

    #[tokio::test]
    async fn test_search_and_paperdoll_apparel_e2e() {
        // parity: feat:ui.search, feat:cosmetics.apparel
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let kw = HashMap::new();

        let mut player = make_player_session(51_010, 2, 90_000_002, 98_000_001, Some(99_000_001), 30_000_142);

        // 1. search.Query & QuickQuery validation, exact/partial matching, and structure/system lookup
        let short_err = ctx
            .dispatch_rpc(&mut player, "search", None, "Query", &[PyRep::String("   ".into())], &kw, None)
            .await
            .expect_err("empty search query must fail");
        assert_eq!(short_err.code, "SearchStringTooShort");

        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.insert(
                98_000_555,
                CorporationRecord {
                    corporation_id: 98_000_555,
                    name: "Vanguard Horizon".into(),
                    ticker: "VGHR".into(),
                    ceo_id: 90_000_002,
                    hq_station_id: Some(60_003_760),
                    alliance_id: Some(99_000_555),
                    ..Default::default()
                },
            );
            snap.alliances.insert(
                99_000_555,
                AllianceRecord {
                    alliance_id: 99_000_555,
                    name: "Horizon Coalition".into(),
                    short_name: "HRZN".into(),
                    executor_corp_id: 98_000_555,
                    member_corp_ids: [98_000_555].into_iter().collect(),
                },
            );
            snap.structures.insert(
                1_000_000_000_555,
                UpwellStructureRecord {
                    structure_id: 1_000_000_000_555,
                    type_id: 35_832,
                    owner_corp_id: 98_000_555,
                    solar_system_id: 30_000_142,
                    name: "Jita Horizon Astrahus".into(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        });

        // Partial match on "Horizon" across corp (3), alliance (4), structure (11)
        let partial_res = ctx
            .dispatch_rpc(
                &mut player,
                "search",
                None,
                "Query",
                &[
                    PyRep::String("Horizon".into()),
                    PyRep::List(vec![PyRep::Integer(3), PyRep::Integer(4), PyRep::Integer(11)]),
                    PyRep::Bool(false),
                ],
                &kw,
                None,
            )
            .await
            .expect("search.Query partial");
        let PyRep::Dict(groups) = partial_res else {
            panic!("expected Dict from search.Query");
        };
        let corp_hits = groups
            .iter()
            .find(|(k, _)| k.as_i64() == Some(3))
            .and_then(|(_, v)| match v {
                PyRep::List(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(corp_hits.contains(&98_000_555), "expected corp 98000555 in partial hits");

        // Exact match on "Horizon" should NOT match "Vanguard Horizon"
        let exact_res = ctx
            .dispatch_rpc(
                &mut player,
                "search",
                None,
                "Query",
                &[
                    PyRep::String("Horizon".into()),
                    PyRep::List(vec![PyRep::Integer(3)]),
                    PyRep::Bool(true),
                ],
                &kw,
                None,
            )
            .await
            .expect("search.Query exact");
        let PyRep::Dict(exact_groups) = exact_res else {
            panic!("expected Dict from search.Query exact");
        };
        let exact_corps = exact_groups
            .iter()
            .find(|(k, _)| k.as_i64() == Some(3))
            .and_then(|(_, v)| match v {
                PyRep::List(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>()),
                _ => None,
            })
            .unwrap_or_default();
        assert!(!exact_corps.contains(&98_000_555), "exact match must exclude partial corp name");

        // QuickQuery for Jita solar system (group 7)
        let quick_jita = ctx
            .dispatch_rpc(
                &mut player,
                "search",
                None,
                "QuickQuery",
                &[PyRep::String("Jita".into()), PyRep::List(vec![PyRep::Integer(7)]), PyRep::Bool(true)],
                &kw,
                None,
            )
            .await
            .expect("search.QuickQuery Jita");
        let PyRep::List(jita_ids) = quick_jita else {
            panic!("expected List from QuickQuery");
        };
        assert!(jita_ids.iter().any(|x| x.as_i64() == Some(30_000_142)));

        // 2. paperDollServer apparel wear & persistence
        let apparel_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.category_id == 30 && t.published)
            .map(|t| t.type_id)
            .expect("published category 30 apparel type in SDE");

        // Unowned apparel must fail
        let unowned_doll = PyRep::keyval([
            ("dollDNA", PyRep::String("dna:test".into())),
            ("apparel", PyRep::List(vec![PyRep::Integer(i64::from(apparel_type_id))])),
        ]);
        let unowned_err = ctx
            .dispatch_rpc(
                &mut player,
                "paperDollServer",
                None,
                "UpdateExistingCharacterLimited",
                &[PyRep::Integer(90_000_002), unowned_doll],
                &kw,
                None,
            )
            .await
            .expect_err("unowned apparel must fail");
        assert_eq!(unowned_err.code, "InvalidApparelItem");

        // Grant owned apparel item in Hangar and wear it
        let apparel_item_id = 880_999_001;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                apparel_item_id,
                InventoryItemRecord {
                    item_id: apparel_item_id,
                    type_id: apparel_type_id,
                    owner_id: 90_000_002,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });

        let owned_doll = PyRep::keyval([
            ("dollDNA", PyRep::String("dna:worn".into())),
            ("apparel", PyRep::List(vec![PyRep::Integer(apparel_item_id)])),
        ]);
        let update_res = ctx
            .dispatch_rpc(
                &mut player,
                "paperDollServer",
                None,
                "UpdateExistingCharacterLimited",
                &[PyRep::Integer(90_000_002), owned_doll],
                &kw,
                None,
            )
            .await
            .expect("UpdateExistingCharacterLimited");
        assert_eq!(keyval_get(&update_res, "updated"), Some(&PyRep::Bool(true)));

        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&apparel_item_id).expect("apparel item");
            assert_eq!(it.flag, InventoryFlag::Wardrobe, "worn apparel must move to Wardrobe flag");
        });

        let doll_data = ctx
            .dispatch_rpc(&mut player, "paperDollServer", None, "GetMyPaperDollData", &[], &kw, None)
            .await
            .expect("GetMyPaperDollData");
        let worn = match keyval_get(&doll_data, "wornApparel") {
            Some(PyRep::List(l)) => l.iter().filter_map(|v| v.as_i64()).collect::<Vec<_>>(),
            other => panic!("expected wornApparel List, got {other:?}"),
        };
        assert_eq!(worn, vec![i64::from(apparel_type_id)]);
    }

    #[tokio::test]
    async fn test_insurance_void_expiry_and_mission_rewards_e2e() {
        // parity: feat:combat.insurance-void-and-expiry, feat:pve.mission-lp-isk-reward-formula, feat:pve.mission-complete
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        let kw = HashMap::new();

        let mut player = make_player_session(51_020, 2, 90_000_002, 98_000_001, Some(99_000_001), 30_000_142);

        // 1. Insurance 12-week contract, repackage voiding, and destruction payout + EVE-mail
        let spare_ship_id = 880_999_101;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                ch.balance = IskCents::from_isk(50_000_000);
            }
            snap.items.insert(
                spare_ship_id,
                InventoryItemRecord {
                    item_id: spare_ship_id,
                    type_id: 587, // Rifter
                    owner_id: 90_000_002,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });

        // Insure at Platinum (1.0 fraction)
        let insure_res = ctx
            .dispatch_rpc(
                &mut player,
                "insuranceSvc",
                None,
                "InsureShip",
                &[PyRep::Integer(spare_ship_id), PyRep::Real(1.0)],
                &kw,
                None,
            )
            .await
            .expect("InsureShip");
        assert_eq!(
            keyval_get(&insure_res, "levelName"),
            Some(&PyRep::String("Platinum".into()))
        );

        let contract = ctx
            .dispatch_rpc(
                &mut player,
                "insuranceSvc",
                None,
                "GetContractForShip",
                &[PyRep::Integer(spare_ship_id)],
                &kw,
                None,
            )
            .await
            .expect("GetContractForShip");
        let start_date = keyval_get(&contract, "startDate").and_then(|v| v.as_i64()).unwrap_or(0);
        let end_date = keyval_get(&contract, "endDate").and_then(|v| v.as_i64()).unwrap_or(0);
        let twelve_weeks_ticks = 12 * 7 * 86_400 * 10_000_000_i64;
        assert_eq!(end_date - start_date, twelve_weeks_ticks, "insurance must last 12 weeks");

        // Repackaging voids insurance
        ctx.dispatch_rpc(
            &mut player,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::List(vec![PyRep::Integer(spare_ship_id)])],
            &kw,
            None,
        )
        .await
        .expect("RepackageItems");

        let after_repack = ctx
            .dispatch_rpc(
                &mut player,
                "insuranceSvc",
                None,
                "GetContractForShip",
                &[PyRep::Integer(spare_ship_id)],
                &kw,
                None,
            )
            .await
            .expect("GetContractForShip after repackage");
        assert_eq!(after_repack, PyRep::None, "repackaging must void insurance contract");

        // 2. Mission LP/ISK reward formula & completion payout
        let agent_id = ctx
            .sde
            .agents
            .values()
            .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 24 && a.station_id.unwrap_or(0) > 0)
            .or_else(|| {
                ctx.sde
                    .agents
                    .values()
                    .find(|a| a.level == 1 && a.agent_type_id == 2 && a.station_id.unwrap_or(0) > 0)
            })
            .map(|a| a.agent_id)
            .expect("level 1 station agent in SDE");

        // Grant Negotiation (3410) level 4 and Connections (3359) level 3
        let agent_rec = &ctx.sde.agents[&agent_id];
        let agent_station_id = agent_rec.station_id.unwrap();
        let agent_sys_id = agent_rec.solar_system_id.unwrap_or(30_000_142);
        player.station_id = Some(agent_station_id);
        player.solar_system_id = Some(agent_sys_id);
        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(90_000_002).or_default();
            skills.insert(3410, eve_db::TrainedSkillRecord { type_id: 3410, level: 4, skill_points: 45_255.0 });
            skills.insert(3359, eve_db::TrainedSkillRecord { type_id: 3359, level: 3, skill_points: 8_000.0 });
            if let Some(ch) = snap.characters.get_mut(&90_000_002) {
                ch.station_id = Some(agent_station_id);
                ch.solar_system_id = agent_sys_id;
            }
        });

        let offer_res = ctx.dispatch_rpc(&mut player, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent_id)], &kw, None).await.expect("offer");
        let mission_id = keyval_get(&offer_res, "missionID").and_then(|v| v.as_i64()).expect("missionID");
        ctx.dispatch_rpc(&mut player, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(mission_id)], &kw, None).await.expect("accept");
        let dest_station = keyval_get(&offer_res, "dropoffStationID").and_then(|v| v.as_i64());
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(m) = snap.active_missions.get_mut(&(90_000_002, mission_id as u32)) { m.npcs_remaining = 0; }
            if let Some(dest) = dest_station { if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.station_id = Some(dest); } }
        });
        if let Some(dest) = dest_station { player.station_id = Some(dest); }
        let bal_before = ctx.db.with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
        let complete_res = ctx.dispatch_rpc(&mut player, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(mission_id)], &kw, None).await.expect("complete");
        let isk_paid = keyval_get(&complete_res, "iskReward").and_then(|v| v.as_i64()).unwrap_or(0);
        let bonus_paid = keyval_get(&complete_res, "bonusReward").and_then(|v| v.as_i64()).unwrap_or(0);
        let lp_paid = keyval_get(&complete_res, "lpReward").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(isk_paid > 0 && bonus_paid > 0 && lp_paid > 0);
        let bal_after = ctx.db.with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
        assert!(bal_after > bal_before && bal_after - bal_before <= IskCents::from_isk(isk_paid + bonus_paid).0);
    }

    #[tokio::test]
    async fn test_jump_fatigue_decay_bridges_and_hulls_e2e() {
        // parity: feat:navigation.jump-fatigue-formula
        let ctx = ServerContext::new();
        ctx.seed_demo_world_fixtures();
        for tid in [28606_u32, 42244, 670, 672, 648, 12729, 12753, 20185, 28846, 28352] {
            assert!((eve_server::services::space_destiny::space_nav_travel::ship_fatigue_reduction_bonus(&ctx, tid) - 0.90).abs() < 1e-6);
        }
        assert!((eve_server::services::space_destiny::space_nav_travel::ship_fatigue_reduction_bonus(&ctx, 22428) - 0.75).abs() < 1e-6);

        let kw = HashMap::new();
        let (cyno_char, bridger_char, passenger_char) = (90_000_002_i64, 90_000_003_i64, 90_000_001_i64);
        let (src_sys, dst_sys) = (30_000_142_u32, 30_000_204_u32);
        let (cyno_ship, cyno_mod, bridger_ship, portal_mod, bridger_fuel, passenger_ship) =
            (990_010_001_i64, 990_010_002, 990_010_011, 990_010_012, 990_010_013, 990_010_021);
        let mut pass_sess = make_player_session(51_103, 1, passenger_char, 98_000_001, None, src_sys);
        ctx.dispatch_rpc(&mut pass_sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
        ctx.dispatch_rpc(&mut pass_sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(passenger_char)], &kw, None).await.expect("SelectCharacterID");

        ctx.db.with_mut_snapshot(|snap| {
            if !snap.characters.contains_key(&bridger_char) {
                if let Some(mut c3) = snap.characters.get(&passenger_char).cloned() {
                    c3.character_id = bridger_char;
                    snap.characters.insert(bridger_char, c3);
                }
            }
            for (cid, sid, sys) in [
                (cyno_char, cyno_ship, dst_sys),
                (bridger_char, bridger_ship, src_sys),
                (passenger_char, passenger_ship, src_sys),
            ] {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.solar_system_id = sys;
                    ch.station_id = None;
                    ch.structure_id = None;
                    ch.active_ship_id = sid;
                    ch.jump_cooldown_until_unix = 0;
                    ch.jump_fatigue_until_unix = 0;
                    ch.jump_fatigue_minutes = 0.0;
                }
                let sk = snap.skills.entry(cid).or_default();
                for stid in [21603_u32, 24562, 3456, 21610, 21611] {
                    sk.insert(stid, eve_db::TrainedSkillRecord { type_id: stid, level: 5, skill_points: 256_000.0 });
                }
            }
            let mut member_details = HashMap::new();
            for cid in [cyno_char, bridger_char, passenger_char] {
                member_details.insert(cid, eve_db::FleetMemberRecord {
                    character_id: cid, role: eve_db::FleetRole::SquadMember,
                    wing_id: Some(1), squad_id: Some(1), takes_fleet_warp: true,
                    accepts_regroup: true, accepts_conduit_jumps: true, job: 0,
                    role_name: String::new(), join_time_unix: 0,
                });
            }
            snap.fleets.insert(77_001, eve_db::FleetRecord {
                fleet_id: 77_001, boss_id: bridger_char, free_move: true,
                wings: vec![1], squads: vec![1],
                members: vec![cyno_char, bridger_char, passenger_char],
                member_details, ..Default::default()
            });
            for (id, tid, owner, loc, flag, qty) in [
                (cyno_ship, 12032_u32, cyno_char, i64::from(dst_sys), InventoryFlag::None, 1_i32),
                (cyno_mod, 28646, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1),
                (990_010_003, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 1_000),
                (bridger_ship, 22428, bridger_char, i64::from(src_sys), InventoryFlag::None, 1),
                (portal_mod, 28652, bridger_char, bridger_ship, InventoryFlag::HiSlot0, 1),
                (bridger_fuel, 16274, bridger_char, bridger_ship, InventoryFlag::Cargo, 50_000),
                (passenger_ship, 12032, passenger_char, i64::from(src_sys), InventoryFlag::None, 1),
            ] {
                snap.items.insert(id, InventoryItemRecord {
                    item_id: id, type_id: tid, owner_id: owner, location_id: loc,
                    flag, quantity: qty, singleton: qty == 1, ..Default::default()
                });
            }
            snap.module_online_states.insert(cyno_mod, true);
            snap.module_online_states.insert(portal_mod, true);
        });
        for (cid, sid, tid, sys) in [
            (cyno_char, cyno_ship, 12032_u32, dst_sys),
            (bridger_char, bridger_ship, 22428, src_sys),
            (passenger_char, passenger_ship, 12032, src_sys),
        ] {
            let ball = eve_destiny::Ball {
                id: sid, type_id: tid, position: glam::DVec3::ZERO, velocity: glam::DVec3::ZERO,
                max_velocity: 150.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0,
                speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
            };
            let cs = ctx.build_ship_combat_state(cid, sid, tid);
            let meta = ctx.build_ship_slim_meta(cid, tid);
            ctx.systems.get_or_spawn(sys).spawn_ball_with_meta(ball, cs, meta).await.expect("spawn ship");
        }

        let mut cyno_sess = make_player_session(51_101, 101, cyno_char, 98_000_001, None, dst_sys);
        cyno_sess.ship_id = Some(cyno_ship); cyno_sess.station_id = None;
        let mut bridger_sess = make_player_session(51_102, 102, bridger_char, 98_000_001, None, src_sys);
        bridger_sess.ship_id = Some(bridger_ship); bridger_sess.station_id = None;
        pass_sess.ship_id = Some(passenger_ship); pass_sess.station_id = None; pass_sess.solar_system_id = Some(src_sys); pass_sess.fleet_id = Some(77_001);

        let cyno_res = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())], &kw, None).await.expect("activate covert cyno");
        let beacon_id = if let PyRep::KeyVal(entries) = &cyno_res {
            entries.iter().find(|(k, _)| *k == "beaconID").and_then(|(_, v)| match v { PyRep::Integer(n) => Some(*n), _ => None }).unwrap_or(cyno_ship)
        } else { cyno_ship };
        ctx.dispatch_rpc(&mut bridger_sess, "beyonce", None, "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(i64::from(dst_sys)), PyRep::Integer(beacon_id)], &kw, None).await.expect("open covert bridge");

        eve_server::services::space_destiny::clear_session_change_timer(&ctx, passenger_char);
        ctx.dispatch_rpc(&mut pass_sess, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(bridger_char), PyRep::Integer(bridger_ship)], &kw, None)
            .await
            .expect("passenger jump through covert portal");
        assert_eq!(pass_sess.solar_system_id, Some(dst_sys));
        let jnotifs = ctx.drain_notifications_for_char(passenger_char);
        let jtimer_notif = jnotifs.iter().find(|n| n.event == "OnJumpTimersUpdated").expect("OnJumpTimersUpdated");
        assert_eq!(jtimer_notif.args.len(), 4, "OnJumpTimersUpdated must include 4th doAlert arg");
        assert_eq!(jtimer_notif.args[3], PyRep::Bool(true));

        let timers = ctx.dispatch_rpc(&mut pass_sess, "jumpTimers", None, "GetTimers", &[], &kw, None).await.expect("GetTimers");
        assert!(!matches!(timers, PyRep::None));
        assert!(ctx.db.with_snapshot(|s| s.characters[&passenger_char].jump_fatigue_minutes > 0.0));
        let (cd1, fat1) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(0.0, 5.0, 0.0);
        let (cd2, fat2) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(50.0, 5.0, 0.0);
        let (cd3, fat3) = eve_server::formulas::compute_jump_fatigue_and_cooldown_minutes(270.0, 5.0, 0.0);
        assert!((cd1 - 6.0).abs() < 1e-6 && (fat1 - 60.0).abs() < 1e-6 && (cd2 - 6.0).abs() < 1e-6 && (fat2 - 300.0).abs() < 1e-6 && (cd3 - 27.0).abs() < 1e-6 && (fat3 - 300.0).abs() < 1e-6);
    }
}
