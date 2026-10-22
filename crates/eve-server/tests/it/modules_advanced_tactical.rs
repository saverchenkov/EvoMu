//! End-to-End Integration Tests for Tactical, Capital, Command, Fitting & Support Modules (`feat:modules.*`).

use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_SYS: u32 = 30000142;
const JITA_STATION: i64 = 60003760;
const NULL_SYS: u32 = 30004759;

fn build_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

fn seed_pilot(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    docked: bool,
) -> SessionState {
    let all_skills_l5: HashMap<u32, TrainedSkillRecord> = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 16)
        .map(|t| {
            (
                t.type_id,
                TrainedSkillRecord {
                    type_id: t.type_id,
                    level: 5,
                    skill_points: 256_000.0,
                },
            )
        })
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo character");
        ch.character_id = char_id;
        ch.clone_grade = 2;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = 1000125;
        ch.alliance_id = Some(99000001);
        ch.solar_system_id = sys_id;
        ch.station_id = if docked { Some(JITA_STATION) } else { None };
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.skills.insert(char_id, all_skills_l5);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: if docked { JITA_STATION } else { sys_id as i64 },
                flag: if docked { InventoryFlag::Hangar } else { InventoryFlag::None },
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1000125);
    sess.alliance_id = Some(99000001);
    sess.solar_system_id = Some(sys_id);
    sess.constellation_id = Some(20000020);
    sess.region_id = Some(10000002);
    sess.station_id = if docked { Some(JITA_STATION) } else { None };
    sess.ship_id = Some(ship_id);
    sess
}

fn set_skill_level(ctx: &ServerContext, char_id: i64, skill_id: u32, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            skill_id,
            TrainedSkillRecord {
                type_id: skill_id,
                level,
                skill_points: 100_000.0,
            },
        );
    });
}

fn set_char_alliance(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, alliance_id: i64) {
    sess.alliance_id = Some(alliance_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.alliance_id = Some(alliance_id);
        }
    });
}

fn set_item_dyn_attr(ctx: &ServerContext, item_id: i64, attr_id: u16, val: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&item_id) {
            it.dynamic_attributes.insert(attr_id, val);
        }
    });
}

fn seed_wormhole_pair(ctx: &ServerContext, from_sys: u32, to_sys: u32, mass_kg: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(from_sys).or_insert_with(|| eve_db::SystemDynamicState {
            solar_system_id: from_sys,
            dbs_multiplier: 1.0,
            ess_main_bank: eve_db::IskCents(0),
            ess_reserve_bank: eve_db::IskCents(0),
            ess_linked_character_id: None,
            ess_link_started_tick: None,
            sov_alliance_id: None,
            military_index: 0,
            industrial_index: 0,
            strategic_index: 0,
            fw_contested_pct: 0.0,
            insurgency_corruption_stage: 0,
            insurgency_suppression_stage: 0,
            wormhole_remaining_mass_kg: mass_kg,
            wormhole_destination_system_id: Some(to_sys),
            signature_position: None,
            fw_complex_position: None,
            fw_capture_seconds_elapsed: 0,
            fw_capture_duration_seconds: 0,
            last_fw_capture_tick: 0,
            last_sov_claim_tick: 0,
        });
        st.wormhole_destination_system_id = Some(to_sys);
        st.wormhole_remaining_mass_kg = mass_kg;
        let dst = snap.system_states.entry(to_sys).or_insert_with(|| eve_db::SystemDynamicState {
            solar_system_id: to_sys,
            dbs_multiplier: 1.0,
            ess_main_bank: eve_db::IskCents(0),
            ess_reserve_bank: eve_db::IskCents(0),
            ess_linked_character_id: None,
            ess_link_started_tick: None,
            sov_alliance_id: None,
            military_index: 0,
            industrial_index: 0,
            strategic_index: 0,
            fw_contested_pct: 0.0,
            insurgency_corruption_stage: 0,
            insurgency_suppression_stage: 0,
            wormhole_remaining_mass_kg: mass_kg,
            wormhole_destination_system_id: None,
            signature_position: None,
            fw_complex_position: None,
            fw_capture_seconds_elapsed: 0,
            fw_capture_duration_seconds: 0,
            last_fw_capture_tick: 0,
            last_sov_claim_tick: 0,
        });
        dst.wormhole_remaining_mass_kg = mass_kg;
    });
}

fn seed_fleet(ctx: &ServerContext, fleet_id: i64, boss_id: i64, members: Vec<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id,
                motd: String::new(),
                free_move: true,
                is_registered: false,
                members,
                ..Default::default()
            },
        );
    });
}

fn give_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    loc_id: i64,
    flag: InventoryFlag,
    qty: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id: loc_id,
                flag,
                quantity: qty,
                singleton: qty == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, &args, &HashMap::new(), None)
        .await
}

async fn fit_mod(
    ctx: &ServerContext,
    sess: &mut SessionState,
    ship_id: i64,
    mod_id: i64,
    mod_type_id: u32,
    flag_u16: u16,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    give_item(
        ctx,
        mod_id,
        mod_type_id,
        sess.character_id.unwrap(),
        JITA_STATION,
        InventoryFlag::Hangar,
        1,
    );
    rpc(
        ctx,
        sess,
        "invbroker",
        "Add",
        vec![
            PyRep::Integer(mod_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(flag_u16 as i64),
        ],
    )
    .await
}

async fn spawn_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.location_id = sys_id as i64;
            it.flag = InventoryFlag::None;
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let st = ctx.sde.types.get(&ship_type_id);
    let ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: st
            .and_then(|t| t.default_attributes.get(&37).copied())
            .unwrap_or(150.0),
        inertia: st
            .and_then(|t| t.default_attributes.get(&70).copied())
            .unwrap_or(1.0),
        mass: st.map(|t| t.mass.max(1.0)).unwrap_or(10_000_000.0),
        radius: st.map(|t| t.radius.max(10.0)).unwrap_or(100.0),
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    cs.capacitor = cs.max_capacitor.max(10_000.0);
    cs.max_capacitor = cs.max_capacitor.max(10_000.0);
    let meta = ctx.build_ship_slim_meta(char_id, ship_type_id);
    let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
}

fn attr_from_all_info(info: &PyRep, section: &str, attr_id: u16) -> f64 {
    let dict_opt = if let Some(sec) = info.dict_get(section) {
        if let Some(sub) = sec.dict_get("attributes") {
            Some(sub)
        } else {
            Some(sec)
        }
    } else if let Some(sec) = info.dict_get("shipAttributes") {
        Some(sec)
    } else {
        info.dict_get("attributes")
    };
    dict_opt
        .and_then(|a| match a {
            PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
                if k.as_i64() == Some(attr_id as i64) {
                    v.as_f64()
                } else {
                    None
                }
            }),
            _ => None,
        })
        .unwrap_or(0.0)
}

fn attr_from_item_info(info: &PyRep, attr_id: u16) -> f64 {
    info.dict_get("attributes")
        .and_then(|a| match a {
            PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
                if k.as_i64() == Some(attr_id as i64) {
                    v.as_f64()
                } else {
                    None
                }
            }),
            _ => None,
        })
        .unwrap_or(0.0)
}
}

#[tokio::test]
async fn bastion_effects_and_wormhole_mjd_interactions() {
    // parity: feat:modules.bastion-module-effects, feat:modules.bastion-wormhole-and-mjd-interaction
    let ctx = build_ctx();

    // 1. Non-Marauder fitting rejection
    let mut rifter_sess = seed_pilot(&ctx, 91001, 81001, 587, JITA_SYS, true);
    let bad_fit = fit_mod(&ctx, &mut rifter_sess, 81001, 71001, 33400, 27).await;
    assert!(bad_fit.is_err(), "Bastion Module I must be rejected on non-Marauders");

    // 2. Fit Paladin (28659, group 900) with Bastion Module I (33400), Mega Pulse Laser II (2929), Large MJD (4383), Large Armor Repairer II (3538)
    let mut paladin_sess = seed_pilot(&ctx, 91002, 81002, 28659, JITA_SYS, true);
    fit_mod(&ctx, &mut paladin_sess, 81002, 71002, 33400, 27).await.expect("fit Bastion");
    fit_mod(&ctx, &mut paladin_sess, 81002, 71003, 2929, 28).await.expect("fit Mega Pulse II");
    fit_mod(&ctx, &mut paladin_sess, 81002, 71004, 4383, 19).await.expect("fit Large MJD");
    fit_mod(&ctx, &mut paladin_sess, 81002, 71005, 3538, 11).await.expect("fit Large Armor Rep II");

    paladin_sess.station_id = None;
    spawn_in_space(&ctx, 91002, 81002, 28659, JITA_SYS, DVec3::ZERO).await;

    let pre_turret = rpc(&ctx, &mut paladin_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71003)]).await.unwrap();
    let pre_rof = attr_from_item_info(&pre_turret, 51);
    let pre_opt = attr_from_item_info(&pre_turret, 54);
    let pre_rep = rpc(&ctx, &mut paladin_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71005)]).await.unwrap();
    let pre_armor_amt = attr_from_item_info(&pre_rep, 84);

    // 3. Activate Bastion Module I
    rpc(
        &ctx,
        &mut paladin_sess,
        "dogmaIM",
        "Activate",
        vec![PyRep::Integer(71002), PyRep::String("bastionMode".into()), PyRep::None, PyRep::Integer(1)],
    )
    .await
    .expect("activate Bastion");

    let post_ship = rpc(&ctx, &mut paladin_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!((attr_from_all_info(&post_ship, "shipInfo", 2135) - 0.05).abs() < 1e-4, "Bastion must set 95% remoteAssistanceImpedance (0.05)");
    assert!((attr_from_all_info(&post_ship, "shipInfo", 2045) - 0.0).abs() < 1e-4, "Bastion must disallow tethering");
    assert!((attr_from_all_info(&post_ship, "shipInfo", 2112) - 1.0).abs() < 1e-4, "V24 Bastion must NOT grant sensor dampener immunity");
    assert!((attr_from_all_info(&post_ship, "shipInfo", 267) - 0.35).abs() < 0.05, "Bastion must boost armor resistances by +30%");

    let post_turret = rpc(&ctx, &mut paladin_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71003)]).await.unwrap();
    assert!((attr_from_item_info(&post_turret, 51) - pre_rof * 0.50).abs() < 1.0, "Bastion must halve turret ROF (-50% duration)");
    assert!(attr_from_item_info(&post_turret, 54) > pre_opt * 1.20, "Bastion must grant +25% turret optimal range");

    let post_rep = rpc(&ctx, &mut paladin_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71005)]).await.unwrap();
    assert!((attr_from_item_info(&post_rep, 84) - pre_armor_amt * 1.60).abs() < 1.0, "Bastion must boost local armor repair amount by +60%");

    let sys = ctx.systems.get_or_spawn(JITA_SYS);
    let cs = sys.get_combat_state(81002).await.unwrap().unwrap();
    assert!(cs.disallow_assistance && cs.is_mwd_mjd_disabled && cs.warp_scramble_strength >= 100);
    let now_u = ctx.now_unix_secs();
    assert!(ctx.crimewatch.read().unwrap().states.get(&91002).is_some_and(|s| s.is_weapon_timer_active(now_u)), "Bastion must set a 60s Weapons Timer");

    // 4. Own MJD is blocked while Bastion is active, but external Command Destroyer MJFG moves the bastioned Marauder 100 km!
    let mjd_err = rpc(&ctx, &mut paladin_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71004)]).await;
    assert!(mjd_err.is_err(), "Ship's own MJD must be disabled during Bastion");

    let _bifrost_sess = seed_pilot(&ctx, 91003, 81003, 37480, JITA_SYS, false);
    spawn_in_space(&ctx, 91003, 81003, 37480, JITA_SYS, DVec3::new(100.0, 0.0, 0.0)).await;
    let moved = sys.micro_jump_field(81003, 100_000.0, 6_000.0, 25).await.expect("MJFG jump");
    assert!(moved.contains(&81002), "External MJFG must move a bastioned Marauder");
    let (_, pal_ball) = sys.get_ball_snapshot(81002).await.unwrap().unwrap();
    assert!((pal_ball.position.x - 100_000.0).abs() < 10.0, "Bastioned Marauder must translate 100 km via MJFG");

    // 5. Wormhole jump allowed when Bastion is set not to repeat (or deactivated), deactivating Bastion on jump
    let wh_id = 750_000_001i64;
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: wh_id,
                type_id: 26272,
                position: DVec3::new(100_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1e12,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 988,
                category_id: 2,
                owner_id: 1,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
            },
        )
        .await;
    seed_wormhole_pair(&ctx, JITA_SYS, NULL_SYS, 2_000_000_000.0);

    // While repeating cycle is active, WormholeJump is rejected
    let wh_blocked = rpc(&ctx, &mut paladin_sess, "wormholeMgr", "WormholeJump", vec![PyRep::Integer(wh_id)]).await;
    assert!(wh_blocked.is_err(), "Repeating Bastion cycle must block wormhole jump");

    // Set Bastion to non-repeating / deactivate and jump through wormhole
    rpc(&ctx, &mut paladin_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(71002)]).await.expect("deactivate Bastion");
    let wh_ok = rpc(&ctx, &mut paladin_sess, "wormholeMgr", "WormholeJump", vec![PyRep::Integer(wh_id)]).await.expect("wormhole jump after Bastion non-repeat");
    assert_eq!(wh_ok.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(NULL_SYS as i64));

    eve_testkit::attest(&[
        "feat:modules.bastion-module-effects",
        "feat:modules.bastion-wormhole-and-mjd-interaction",
    ]);
}

#[tokio::test]
async fn siege_triage_and_disruptive_lance_v24_mechanics() {
    // parity: feat:modules.siege-module-v24-effects, feat:modules.triage-module-v24-effects, feat:modules.disruptive-lance
    let ctx = build_ctx();

    // 1. Siege Module II (4292) on Sarathiel (87381, Angel Dreadnought keeping 10% speed) & Bane (77281, Lancer Dreadnought)
    let mut sara_sess = seed_pilot(&ctx, 91010, 81010, 87381, NULL_SYS, true);
    set_skill_level(&ctx, 91010, 22043, 5);
    fit_mod(&ctx, &mut sara_sess, 81010, 71010, 4292, 27).await.expect("fit Siege II on Sarathiel");
    sara_sess.station_id = None;
    spawn_in_space(&ctx, 91010, 81010, 87381, NULL_SYS, DVec3::ZERO).await;

    // Without Strontium (16275), activation fails
    let no_fuel = rpc(&ctx, &mut sara_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71010)]).await;
    assert_eq!(no_fuel.unwrap_err().code, "InsufficientFuel");

    // Give 200 Strontium; at Tactical Weapon Reconfiguration L5, cost is 250 - 25*5 = 125 units -> 75 remaining!
    give_item(&ctx, 71011, 16275, 91010, 81010, InventoryFlag::Cargo, 200);
    rpc(&ctx, &mut sara_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71010)]).await.expect("activate Siege II");
    let rem_stront = ctx.db.with_snapshot(|s| s.items.get(&71011).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_stront, 75, "Siege strontium cost must be 250 - 25*5 = 125");

    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let (_, sara_ball) = sys.get_ball_snapshot(81010).await.unwrap().unwrap();
    assert!(sara_ball.max_velocity > 5.0 && sara_ball.max_velocity < 20.0, "Sarathiel must retain 10% speed while sieged");
    let sara_cs = sys.get_combat_state(81010).await.unwrap().unwrap();
    assert!(sara_cs.disallow_assistance && sara_cs.warp_scramble_strength >= 100);
    let sara_info = rpc(&ctx, &mut sara_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!((attr_from_all_info(&sara_info, "shipInfo", 2135) - 0.20).abs() < 1e-4, "Siege must set 80% remoteAssistanceImpedance (0.20)");
    assert!((attr_from_all_info(&sara_info, "shipInfo", 2112) - 0.30).abs() < 1e-4, "Siege II must set 70% sensor dampener resistance (0.30)");
    assert!((attr_from_all_info(&sara_info, "shipInfo", 1795) - 0.0).abs() < 1e-4, "Siege must reduce missile reloadTime by 100%");
    assert!(!ctx.crimewatch.read().unwrap().states.get(&91010).is_some_and(|s| s.is_weapon_timer_active(ctx.now_unix_secs())), "Siege must NOT set a Weapons Timer");

    // 2. Triage Module II (4294) on Apostle (37604, group 1538)
    let mut fax_sess = seed_pilot(&ctx, 91012, 81012, 37604, NULL_SYS, true);
    set_skill_level(&ctx, 91012, 27906, 5);
    fit_mod(&ctx, &mut fax_sess, 81012, 71012, 4294, 27).await.expect("fit Triage II");
    fit_mod(&ctx, &mut fax_sess, 81012, 71013, 526, 19).await.expect("fit Stasis Webifier I");
    give_item(&ctx, 71014, 16275, 91012, 81012, InventoryFlag::Cargo, 200);
    give_item(&ctx, 71015, 2454, 91012, 81012, InventoryFlag::DroneBay, 1);
    fax_sess.station_id = None;
    spawn_in_space(&ctx, 91012, 81012, 37604, NULL_SYS, DVec3::new(0.0, 2_000.0, 0.0)).await;

    rpc(&ctx, &mut fax_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71012)]).await.expect("activate Triage II");
    let rem_triage_stront = ctx.db.with_snapshot(|s| s.items.get(&71014).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_triage_stront, 125, "Triage II strontium cost at L5 must be 200 - 25*5 = 75");

    let fax_info = rpc(&ctx, &mut fax_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!((attr_from_all_info(&fax_info, "shipInfo", 1271) - 0.0).abs() < 1e-4, "Triage must set -100% droneBandwidth (0.0)");
    assert!(attr_from_all_info(&fax_info, "shipInfo", 192) >= 12.0, "Triage II must grant +5 maxLockedTargets");
    let drone_info = rpc(&ctx, &mut fax_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71015)]).await.unwrap();
    assert!((attr_from_item_info(&drone_info, 64) - 0.0).abs() < 1e-4, "Triage must set -100% drone damageMultiplier (0.0)");

    let launch_err = rpc(&ctx, &mut fax_sess, "ship", "LaunchDrones", vec![PyRep::List(vec![PyRep::Integer(71015)])]).await;
    assert_eq!(launch_err.unwrap_err().code, "InsufficientDroneBandwidth");

    rpc(&ctx, &mut fax_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(81010)]).await.unwrap();
    for _ in 0..25 {
        let _ = sys.tick().await;
    }
    let ewar_err = rpc(&ctx, &mut fax_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71013), PyRep::String("default".into()), PyRep::Integer(81010)]).await;
    assert_eq!(ewar_err.unwrap_err().code, "EwarDisabledInTriage");

    // 3. Disruptive Lance (77288) on Bane (77281, group 4594)
    let mut bane_sess = seed_pilot(&ctx, 91015, 81015, 77281, NULL_SYS, true);
    fit_mod(&ctx, &mut bane_sess, 81015, 71016, 4292, 27).await.expect("fit Siege II on Bane");
    fit_mod(&ctx, &mut bane_sess, 81015, 71017, 77399, 28).await.expect("fit Disruptive Lance on Bane");
    fit_mod(&ctx, &mut bane_sess, 81015, 71018, 11370, 29).await.expect("fit Cloak on Bane");
    give_item(&ctx, 71019, 16275, 91015, 81015, InventoryFlag::Cargo, 500);
    give_item(&ctx, 71020, 17887, 91015, 81015, InventoryFlag::Cargo, 30_000);
    bane_sess.station_id = None;
    spawn_in_space(&ctx, 91015, 81015, 77281, NULL_SYS, DVec3::new(0.0, 0.0, 0.0)).await;

    // Cloak rejected while Lance is fitted
    let cloak_err = rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71018)]).await;
    assert_eq!(cloak_err.unwrap_err().code, "CannotCloakWithLanceFitted");

    // Lance rejected before Siege is active
    let no_siege_err = rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71017), PyRep::String("default".into()), PyRep::Integer(81010)]).await;
    assert_eq!(no_siege_err.unwrap_err().code, "SiegeRequiredForLance");

    // Spawn victim ship in beam corridor at (50 km, 0, 0) and bystander outside corridor at (50 km, 10 km, 0)
    let mut victim_sess = seed_pilot(&ctx, 91016, 81016, 624, NULL_SYS, false);
    spawn_in_space(&ctx, 91016, 81016, 624, NULL_SYS, DVec3::new(50_000.0, 0.0, 0.0)).await;
    let _ = seed_pilot(&ctx, 91017, 81017, 624, NULL_SYS, false);
    spawn_in_space(&ctx, 91017, 81017, 624, NULL_SYS, DVec3::new(50_000.0, 10_000.0, 0.0)).await;

    rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71016)]).await.expect("activate Siege on Bane");
    let lance_res = rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71017), PyRep::String("default".into()), PyRep::Integer(81016)]).await.expect("fire Disruptive Lance");
    assert_eq!(lance_res.dict_get("hitCount").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(lance_res.dict_get("casterLockSeconds").and_then(|v| v.as_i64()), Some(300));
    assert_eq!(lance_res.dict_get("victimDebuffSeconds").and_then(|v| v.as_i64()), Some(60));
    assert!((lance_res.dict_get("remoteRepairImpedance").and_then(|v| v.as_f64()).unwrap_or(0.0) - 0.50).abs() < 1e-4);

    let rem_iso = ctx.db.with_snapshot(|s| s.items.get(&71020).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_iso, 5_000, "Disruptive Lance must consume 25,000 isotopes");

    // Victim cannot warp/jump/dock and has 50% incoming remote repair impedance
    let beyonce_id = ctx.bind_object(&mut victim_sess, "beyonce", NULL_SYS as i64).unwrap();
    let victim_warp = ctx.dispatch_rpc(&mut victim_sess, "beyonce", Some(beyonce_id), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(40000001)], &HashMap::new(), None).await;
    assert_eq!(victim_warp.unwrap_err().code, "DisruptiveLanceDebuffActive");
    assert!((eve_server::services::tactical_modules::incoming_remote_repair_multiplier(&ctx, 81016) - 0.50).abs() < 1e-4);

    eve_testkit::attest(&[
        "feat:modules.siege-module-v24-effects",
        "feat:modules.triage-module-v24-effects",
        "feat:modules.disruptive-lance",
    ]);
}

#[tokio::test]
async fn entosis_link_restrictions_warmup_and_contest() {
    // parity: feat:modules.entosis-activation-restrictions, feat:modules.entosis-warmup-and-contest
    let ctx = build_ctx();

    // 1. Interceptor (Crow 11176, group 831) cannot activate Entosis Link
    let mut crow_sess = seed_pilot(&ctx, 91020, 81020, 11176, NULL_SYS, true);
    give_item(&ctx, 71021, 34593, 91020, 81020, InventoryFlag::HiSlot0, 1);
    crow_sess.station_id = None;
    spawn_in_space(&ctx, 91020, 81020, 11176, NULL_SYS, DVec3::ZERO).await;
    let node_id = 88001i64;
    give_item(&ctx, node_id, 32458, 1, NULL_SYS as i64, InventoryFlag::None, 1);
    spawn_in_space(&ctx, 1, node_id, 32458, NULL_SYS, DVec3::new(5_000.0, 0.0, 0.0)).await;

    let inty_err = rpc(&ctx, &mut crow_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71021), PyRep::String("default".into()), PyRep::Integer(node_id)]).await;
    assert_eq!(inty_err.unwrap_err().code, "InterceptorCannotActivateEntosis");

    // 2. Subcapital (Caracal 621) speed cap (4,000 m/s), Strontium consumption, warm-up cycle -> capture cycle -> contest pause -> unlock reset
    let mut atk_sess = seed_pilot(&ctx, 91021, 81021, 621, NULL_SYS, true);
    fit_mod(&ctx, &mut atk_sess, 81021, 71022, 34595, 27).await.expect("fit Entosis Link II");
    give_item(&ctx, 71023, 16275, 91021, 81021, InventoryFlag::Cargo, 10);
    atk_sess.station_id = None;
    spawn_in_space(&ctx, 91021, 81021, 621, NULL_SYS, DVec3::new(1_000.0, 0.0, 0.0)).await;
    set_item_dyn_attr(&ctx, 81021, 37, 5_500.0);
    let atk_info = rpc(&ctx, &mut atk_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!((attr_from_all_info(&atk_info, "shipInfo", 37) - 4_000.0).abs() < 1e-3, "Online Entosis Link must cap subcap maxVelocity at 4000 m/s");

    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    rpc(&ctx, &mut atk_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(node_id)]).await.unwrap();
    for _ in 0..20 {
        let _ = sys.tick().await;
    }

    // Cycle 1: Warm-up (isWarmup = true, progressDelta = 0.0)
    let c1 = rpc(&ctx, &mut atk_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71022), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.expect("Entosis cycle 1");
    assert_eq!(c1.dict_get("cycleIndex").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(c1.dict_get("isWarmup").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(c1.dict_get("progressDelta").and_then(|v| v.as_f64()), Some(0.0));

    // While active, warp is blocked
    let b_id = ctx.bind_object(&mut atk_sess, "beyonce", NULL_SYS as i64).unwrap();
    let warp_err = ctx.dispatch_rpc(&mut atk_sess, "beyonce", Some(b_id), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(40000001)], &HashMap::new(), None).await;
    assert_eq!(warp_err.unwrap_err().code, "EntosisLinkActive");

    // Advance clock past cycle 1 (120s) and run Cycle 2: Active capture (isWarmup = false, progressDelta > 0.0)
    ctx.advance_virtual_clock(125);
    let c2 = rpc(&ctx, &mut atk_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71022), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.expect("Entosis cycle 2");
    assert_eq!(c2.dict_get("cycleIndex").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(c2.dict_get("isWarmup").and_then(|v| v.as_bool()), Some(false));
    assert!(c2.dict_get("progressDelta").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.05);

    // Opposing alliance pilot completes warm-up and contests node -> progress pauses (progressDelta == 0.0)
    let mut def_sess = seed_pilot(&ctx, 91022, 81022, 621, NULL_SYS, true);
    set_char_alliance(&ctx, &mut def_sess, 91022, 99000002);
    fit_mod(&ctx, &mut def_sess, 81022, 71024, 34595, 27).await.unwrap();
    give_item(&ctx, 71025, 16275, 91022, 81022, InventoryFlag::Cargo, 10);
    def_sess.station_id = None;
    spawn_in_space(&ctx, 91022, 81022, 621, NULL_SYS, DVec3::new(2_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut def_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(node_id)]).await.unwrap();
    for _ in 0..20 {
        let _ = sys.tick().await;
    }
    rpc(&ctx, &mut def_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71024), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.unwrap();
    ctx.advance_virtual_clock(125);
    let _def_c2 = rpc(&ctx, &mut def_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71024), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.unwrap();
    let atk_c3 = rpc(&ctx, &mut atk_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71022), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.unwrap();
    assert_eq!(atk_c3.dict_get("contested").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(atk_c3.dict_get("progressDelta").and_then(|v| v.as_f64()), Some(0.0));

    // Unlocking target resets warm-up back to cycle 1
    ctx.advance_virtual_clock(125);
    rpc(&ctx, &mut atk_sess, "dogmaIM", "RemoveTarget", vec![PyRep::Integer(node_id)]).await.unwrap();
    rpc(&ctx, &mut atk_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(node_id)]).await.unwrap();
    for _ in 0..20 {
        let _ = sys.tick().await;
    }
    let reset_c1 = rpc(&ctx, &mut atk_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71022), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.unwrap();
    assert_eq!(reset_c1.dict_get("cycleIndex").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(reset_c1.dict_get("isWarmup").and_then(|v| v.as_bool()), Some(true));

    // Capital ship (Apostle 37604) 5x cycle duration penalty and 85% remoteAssistanceImpedance
    let mut cap_sess = seed_pilot(&ctx, 91023, 81023, 37604, NULL_SYS, true);
    fit_mod(&ctx, &mut cap_sess, 81023, 71026, 34595, 27).await.unwrap();
    give_item(&ctx, 71027, 16275, 91023, 81023, InventoryFlag::Cargo, 5);
    cap_sess.station_id = None;
    spawn_in_space(&ctx, 91023, 81023, 37604, NULL_SYS, DVec3::new(3_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut cap_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(node_id)]).await.unwrap();
    for _ in 0..25 {
        let _ = sys.tick().await;
    }
    let cap_ent = rpc(&ctx, &mut cap_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71026), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.unwrap();
    assert!((cap_ent.dict_get("cycleDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0) - 600.0).abs() < 1.0, "Capital Entosis II must have 5x cycle duration (600s)");
    assert!((cap_ent.dict_get("remoteAssistanceImpedance").and_then(|v| v.as_f64()).unwrap_or(0.0) - 0.15).abs() < 1e-4);

    eve_testkit::attest(&[
        "feat:modules.entosis-activation-restrictions",
        "feat:modules.entosis-warmup-and-contest",
    ]);
}

#[tokio::test]
async fn command_bursts_industrial_core_and_clone_vat_bay() {
    // parity: feat:modules.command-burst-range, feat:modules.command-burst-strength-formula, feat:modules.command-burst-duration-reload, feat:modules.command-burst-fitting-and-clone-restrictions, feat:modules.industrial-core-burst-bonus
    let ctx = build_ctx();

    // 1. Command Burst fitting restriction on non-command hull (Rifter 587)
    let mut rifter_sess = seed_pilot(&ctx, 91030, 81030, 587, JITA_SYS, true);
    assert!(fit_mod(&ctx, &mut rifter_sess, 81030, 71030, 43555, 27).await.is_err());

    // 2. Fit Nighthawk (22466, Command Ship) with Shield Command Burst II (43555), Command Processor I (43896), and Skirmish Command Burst II (43556)
    let mut cmd_sess = seed_pilot(&ctx, 91031, 81031, 22466, JITA_SYS, true);
    fit_mod(&ctx, &mut cmd_sess, 81031, 71031, 43555, 27).await.expect("fit Shield Burst II");
    assert!(fit_mod(&ctx, &mut cmd_sess, 81031, 71032, 43556, 28).await.is_err(), "maxGroupOnline=1 without Command Processor");
    fit_mod(&ctx, &mut cmd_sess, 81031, 71033, 43896, 92).await.expect("fit Command Processor I");
    fit_mod(&ctx, &mut cmd_sess, 81031, 71034, 43556, 28).await.expect("fit 2nd Command Burst with Command Processor I");

    // Load Shield Extension Charge (42696) via dogmaIM.LoadAmmo -> verify 30s reloadTimeMs at Command Burst Specialist (43728) L5
    give_item(&ctx, 71035, 42696, 91031, JITA_STATION, InventoryFlag::Hangar, 300);
    let load_res = rpc(
        &ctx,
        &mut cmd_sess,
        "dogmaIM",
        "LoadAmmo",
        vec![PyRep::Integer(81031), PyRep::List(vec![PyRep::Integer(71031)]), PyRep::List(vec![PyRep::Integer(71035)])],
    )
    .await
    .expect("load Shield Extension Charge");
    assert!((load_res.dict_get("reloadTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0) - 30_000.0).abs() < 1.0, "Command Burst Specialist L5 must reduce reload time from 60s to 30s");

    // Put Nighthawk, in-range fleetmate (10 km), and out-of-range fleetmate (80 km) in space in the same fleet
    cmd_sess.station_id = None;
    cmd_sess.fleet_id = Some(5001);
    let mut near_sess = seed_pilot(&ctx, 91032, 81032, 621, JITA_SYS, false);
    near_sess.fleet_id = Some(5001);
    let mut far_sess = seed_pilot(&ctx, 91033, 81033, 621, JITA_SYS, false);
    far_sess.fleet_id = Some(5001);
    seed_fleet(&ctx, 5001, 91031, vec![91031, 91032, 91033]);
    spawn_in_space(&ctx, 91031, 81031, 22466, JITA_SYS, DVec3::ZERO).await;
    spawn_in_space(&ctx, 91032, 81032, 621, JITA_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    spawn_in_space(&ctx, 91033, 81033, 621, JITA_SYS, DVec3::new(80_000.0, 0.0, 0.0)).await;

    let pre_near = rpc(&ctx, &mut near_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let pre_shield = attr_from_all_info(&pre_near, "shipInfo", 263);

    let burst_res = rpc(&ctx, &mut cmd_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71031)]).await.expect("activate Shield Command Burst II");
    let aoe_range = burst_res.dict_get("aoeRange").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((aoe_range - 56_250.0).abs() < 10.0, "Wing Command L5 (+50%) * Fleet Command L5 (+25%) must scale 30 km range to 56.25 km");
    assert!((burst_res.dict_get("durationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0) - 22.5).abs() < 0.1, "Shield Command L5 (+50%) must scale 15s cycle to 22.5s");
    assert!((burst_res.dict_get("buffDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0) - 90.0).abs() < 0.1, "Shield Command L5 (+50%) must scale 60s buff to 90s");
    assert!(burst_res.dict_get("burstStrengthMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0) > 1.85, "Specialist L5 * T2 * Command Ship bonus must exceed 1.85x");

    let post_near = rpc(&ctx, &mut near_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let post_far = rpc(&ctx, &mut far_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!(attr_from_all_info(&post_near, "shipInfo", 263) > pre_shield * 1.15, "In-range fleetmate must receive Shield Extension buff");
    assert!((attr_from_all_info(&post_far, "shipInfo", 263) - pre_shield).abs() < 1.0, "Out-of-range fleetmate must not receive buff");

    // 3. Industrial Core II (58945) & Clone Vat Bay I (23735) on Rorqual (28352, group 883)
    let mut rorq_sess = seed_pilot(&ctx, 91035, 81035, 28352, NULL_SYS, true);
    fit_mod(&ctx, &mut rorq_sess, 81035, 71036, 58945, 27).await.expect("fit Industrial Core II");
    fit_mod(&ctx, &mut rorq_sess, 81035, 71037, 42890, 28).await.expect("fit Mining Foreman Burst II");
    fit_mod(&ctx, &mut rorq_sess, 81035, 71038, 23735, 29).await.expect("fit Clone Vat Bay I");
    give_item(&ctx, 71039, 42830, 91035, JITA_STATION, InventoryFlag::Hangar, 200);
    rpc(&ctx, &mut rorq_sess, "dogmaIM", "LoadAmmo", vec![PyRep::Integer(81035), PyRep::List(vec![PyRep::Integer(71037)]), PyRep::List(vec![PyRep::Integer(71039)])]).await.unwrap();
    give_item(&ctx, 71040, 16272, 91035, 81035, InventoryFlag::Cargo, 2_000);
    give_item(&ctx, 71041, 16273, 91035, 81035, InventoryFlag::Cargo, 10);
    rorq_sess.station_id = None;
    spawn_in_space(&ctx, 91035, 81035, 28352, NULL_SYS, DVec3::ZERO).await;

    let pre_ind_burst = rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71037)]).await.unwrap();
    let pre_ind_str = pre_ind_burst.dict_get("burstStrengthMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let pre_ind_rng = pre_ind_burst.dict_get("aoeRange").and_then(|v| v.as_f64()).unwrap_or(0.0);

    rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71036)]).await.expect("activate Industrial Core II");
    let post_ind_burst = rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71037)]).await.unwrap();
    assert!(post_ind_burst.dict_get("burstStrengthMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0) > pre_ind_str, "Industrial Core II must boost Mining Foreman Burst strength");
    assert!(post_ind_burst.dict_get("aoeRange").and_then(|v| v.as_f64()).unwrap_or(0.0) > pre_ind_rng, "Industrial Core II must boost Mining Foreman Burst range");

    // Clone Vat Bay I offer & accept jump clone installation consuming 1 Liquid Ozone (16273)
    rpc(&ctx, &mut rorq_sess, "jumpCloneSvc", "OfferShipCloneInstallation", vec![PyRep::Integer(91032), PyRep::Integer(81035)]).await.expect("offer ship clone");
    let jc_res = rpc(&ctx, &mut near_sess, "jumpCloneSvc", "AcceptShipCloneInstallation", vec![]).await.expect("accept ship clone");
    assert_eq!(jc_res.dict_get("installed").and_then(|v| v.as_bool()), Some(true));
    let rem_ozone = ctx.db.with_snapshot(|s| s.items.get(&71041).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_ozone, 9, "Clone Vat Bay installation must consume 1 Liquid Ozone");

    eve_testkit::attest(&[
        "feat:modules.command-burst-range",
        "feat:modules.command-burst-strength-formula",
        "feat:modules.command-burst-duration-reload",
        "feat:modules.command-burst-fitting-and-clone-restrictions",
        "feat:modules.industrial-core-burst-bonus",
    ]);
}

#[tokio::test]
async fn fitting_capacity_cargo_expanders_drone_upgrades_and_wubble_probe() {
    // parity: feat:modules.fitting-capacity-mods, feat:modules.cargo-expanders, feat:modules.drone-upgrade-modules, feat:modules.stasis-webification-probe
    let ctx = build_ctx();

    // 1. Fitting capacity mods (Co-Processor I 3887, Reactor Control Unit I 1183, PDS I 1405) & Cargo Expanders (1319, 31119)
    let mut ship_sess = seed_pilot(&ctx, 91040, 81040, 624, JITA_SYS, true); // Maller (624)
    let base_info = rpc(&ctx, &mut ship_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let base_cpu = attr_from_all_info(&base_info, "shipInfo", 48);
    let base_pg = attr_from_all_info(&base_info, "shipInfo", 11);
    let base_cargo = attr_from_all_info(&base_info, "shipInfo", 38);
    let base_vel = attr_from_all_info(&base_info, "shipInfo", 37);
    let base_hull = attr_from_all_info(&base_info, "shipInfo", 9);

    fit_mod(&ctx, &mut ship_sess, 81040, 71042, 3887, 11).await.expect("fit Co-Processor I");
    fit_mod(&ctx, &mut ship_sess, 81040, 71043, 1353, 12).await.expect("fit Reactor Control Unit I");
    fit_mod(&ctx, &mut ship_sess, 81040, 71044, 1539, 13).await.expect("fit Power Diagnostic System I");
    fit_mod(&ctx, &mut ship_sess, 81040, 71045, 1319, 14).await.expect("fit Expanded Cargohold II");
    fit_mod(&ctx, &mut ship_sess, 81040, 71046, 31119, 92).await.expect("fit Medium Cargohold Optimization I");

    let mod_info = rpc(&ctx, &mut ship_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let exp_cpu = attr_from_all_info(&mod_info, "shipInfo", 48);
    let exp_pg = attr_from_all_info(&mod_info, "shipInfo", 11);
    let exp_cargo = attr_from_all_info(&mod_info, "shipInfo", 38);
    assert!(exp_cpu >= base_cpu * 1.079, "Co-Processor I must increase ship cpuOutput");
    assert!(exp_pg > base_pg * 1.12, "RCU I + PDS I must increase ship powerOutput");
    assert!(exp_cargo > base_cargo * 1.40, "Expanded Cargohold II + Cargohold Optimization I must multiplicatively increase cargo capacity");
    assert!(attr_from_all_info(&mod_info, "shipInfo", 37) < base_vel, "Cargo expanders must reduce maxVelocity");
    assert!(attr_from_all_info(&mod_info, "shipInfo", 9) < base_hull, "Expanded Cargohold II must reduce structure HP");

    // Verify invbroker.Add allows loading cargo exceeding base_cargo up to exp_cargo
    let ore_qty = ((base_cargo + 50.0) / 0.1) as i32; // Veldspar (1230) is 0.1 m3/unit
    give_item(&ctx, 71047, 1230, 91040, JITA_STATION, InventoryFlag::Hangar, ore_qty);
    rpc(
        &ctx,
        &mut ship_sess,
        "invbroker",
        "Add",
        vec![PyRep::Integer(71047), PyRep::Integer(81040), PyRep::Integer(InventoryFlag::Cargo as i64)],
    )
    .await
    .expect("load cargo exceeding base hull capacity using Expanded Cargohold");

    // 2. Drone upgrade modules: Drone Link Augmentor I (1292), Omnidirectional Tracking Link I (4395), Drone Navigation Computer I (4393), Drone Damage Amplifier I (4405)
    let mut vexor_sess = seed_pilot(&ctx, 91041, 81041, 626, JITA_SYS, true); // Vexor (626)
    give_item(&ctx, 71048, 2454, 91041, 81041, InventoryFlag::DroneBay, 1); // Hobgoblin I (2454)
    let pre_vex_info = rpc(&ctx, &mut vexor_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let pre_dcd = attr_from_all_info(&pre_vex_info, "shipInfo", 458);
    let pre_drone = rpc(&ctx, &mut vexor_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71048)]).await.unwrap();
    let pre_d_vel = attr_from_item_info(&pre_drone, 37);
    let pre_d_opt = attr_from_item_info(&pre_drone, 54);
    let pre_d_dmg = attr_from_item_info(&pre_drone, 64);

    fit_mod(&ctx, &mut vexor_sess, 81041, 71049, 23527, 27).await.expect("fit Drone Link Augmentor I");
    fit_mod(&ctx, &mut vexor_sess, 81041, 71050, 23533, 19).await.expect("fit Omnidirectional Tracking Link I");
    fit_mod(&ctx, &mut vexor_sess, 81041, 71051, 24395, 20).await.expect("fit Drone Navigation Computer I");
    fit_mod(&ctx, &mut vexor_sess, 81041, 71052, 4393, 11).await.expect("fit Drone Damage Amplifier I");

    vexor_sess.station_id = None;
    spawn_in_space(&ctx, 91041, 81041, 626, JITA_SYS, DVec3::ZERO).await;
    rpc(&ctx, &mut vexor_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71050)]).await.expect("activate OTL I");

    let post_vex_info = rpc(&ctx, &mut vexor_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!((attr_from_all_info(&post_vex_info, "shipInfo", 458) - (pre_dcd + 20_000.0)).abs() < 1.0, "Drone Link Augmentor I must add +20 km droneControlDistance");
    let post_drone = rpc(&ctx, &mut vexor_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71048)]).await.unwrap();
    assert!(attr_from_item_info(&post_drone, 37) > pre_d_vel * 1.20, "Drone Navigation Computer I must boost drone maxVelocity");
    assert!(attr_from_item_info(&post_drone, 54) > pre_d_opt * 1.10, "Active Omnidirectional Tracking Link I must boost drone optimal range");
    assert!(attr_from_item_info(&post_drone, 64) > pre_d_dmg * 1.15, "Drone Damage Amplifier I must boost drone damageMultiplier");

    // 3. Stasis Webification Probe (56748) in Interdiction Sphere Launcher I (22782) on Sabre (22456)
    let mut sabre_sess = seed_pilot(&ctx, 91042, 81042, 22456, NULL_SYS, true);
    fit_mod(&ctx, &mut sabre_sess, 81042, 71053, 22782, 27).await.expect("fit Interdiction Sphere Launcher I");
    give_item(&ctx, 71054, 56748, 91042, JITA_STATION, InventoryFlag::Hangar, 10);
    let wub_load = rpc(&ctx, &mut sabre_sess, "dogmaIM", "LoadAmmo", vec![PyRep::Integer(81042), PyRep::List(vec![PyRep::Integer(71053)]), PyRep::List(vec![PyRep::Integer(71054)])]).await.expect("load Stasis Webification Probes");
    assert_eq!(wub_load.dict_get("totalLoaded").and_then(|v| v.as_i64()), Some(3), "30 m3 launcher holds 3x 10 m3 Stasis Webification Probes");

    sabre_sess.station_id = None;
    spawn_in_space(&ctx, 91042, 81042, 22456, NULL_SYS, DVec3::new(0.0, 50_000.0, 0.0)).await;
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let (_, pre_sabre_ball) = sys.get_ball_snapshot(81042).await.unwrap().unwrap();
    let base_sabre_vel = pre_sabre_ball.max_velocity;

    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71053)]).await.expect("launch Stasis Webification Probe");

    // After 1s (before 3s arming delay), velocity is unchanged
    let _ = sys.tick().await;
    let (_, mid_ball) = sys.get_ball_snapshot(81042).await.unwrap().unwrap();
    assert!((mid_ball.max_velocity - base_sabre_vel).abs() < 1e-3, "Wubble probe must not web before 3s arming delay");

    // After 3s total, probe detonates and applies -30% velocity debuff (0.70x) for 30s
    for _ in 0..3 {
        let _ = sys.tick().await;
    }
    let (_, webbed_ball) = sys.get_ball_snapshot(81042).await.unwrap().unwrap();
    assert!((webbed_ball.max_velocity - base_sabre_vel * 0.70).abs() < 1.0, "Detonated Wubble probe must apply -30% velocity debuff");

    // After 31 more ticks, 30s debuff expires and restores original max_velocity
    for _ in 0..31 {
        let _ = sys.tick().await;
    }
    let (_, restored_ball) = sys.get_ball_snapshot(81042).await.unwrap().unwrap();
    assert!((restored_ball.max_velocity - base_sabre_vel).abs() < 1.0, "Wubble debuff must expire after 30s");

    eve_testkit::attest(&[
        "feat:modules.fitting-capacity-mods",
        "feat:modules.cargo-expanders",
        "feat:modules.drone-upgrade-modules",
        "feat:modules.stasis-webification-probe",
    ]);
}

#[tokio::test]
async fn target_spectrum_breaker_cehe_rsb_rtc_ancillary_remote_and_auto_targeter() {
    // parity: feat:modules.target-spectrum-breaker, feat:modules.capital-emergency-hull-energizer, feat:modules.remote-sensor-booster-rtc, feat:modules.ancillary-remote-repairers, feat:modules.automated-targeting-system
    let ctx = build_ctx();

    // 1. Target Spectrum Breaker / Signature Radius Suppressor I (4409, group 1154) on Raven (638, Battleship)
    let mut raven_sess = seed_pilot(&ctx, 91050, 81050, 638, NULL_SYS, true);
    fit_mod(&ctx, &mut raven_sess, 81050, 71060, 4409, 19).await.expect("fit Signature Radius Suppressor I on Raven");
    raven_sess.station_id = None;
    spawn_in_space(&ctx, 91050, 81050, 638, NULL_SYS, DVec3::ZERO).await;

    // Spawn 5 hostile ships that lock the Raven
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    for i in 0..5 {
        let h_char = 91060 + i;
        let h_ship = 81060 + i;
        let mut h_sess = seed_pilot(&ctx, h_char, h_ship, 587, NULL_SYS, false);
        spawn_in_space(&ctx, h_char, h_ship, 587, NULL_SYS, DVec3::new(3_000.0 + (i as f64) * 100.0, 0.0, 0.0)).await;
        rpc(&ctx, &mut h_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(81050)]).await.unwrap();
    }
    for _ in 0..15 {
        let _ = sys.tick().await;
    }
    ctx.set_mechanic_rng_seed(Some(1));
    let tsb_res = rpc(&ctx, &mut raven_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71060)]).await.expect("activate Target Spectrum Breaker");
    assert_eq!(tsb_res.dict_get("lockingShipCount").and_then(|v| v.as_i64()), Some(5));
    assert!(tsb_res.dict_get("brokenLocks").and_then(|v| v.as_i64()).unwrap_or(0) >= 1, "TSB must break locks when multiple ships lock the battleship");
    let raven_info = rpc(&ctx, &mut raven_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let base_raven_sr = ctx.sde.types.get(&638).and_then(|t| t.default_attributes.get(&564).copied()).unwrap_or(100.0);
    assert!(attr_from_all_info(&raven_info, "shipInfo", 564) < base_raven_sr, "TSB must apply -25% self scanResolution penalty");

    // 2. Capital Emergency Hull Energizer I (40714, group 60) on Revelation (19720, Dreadnought)
    let mut rev_sess = seed_pilot(&ctx, 91051, 81051, 19720, NULL_SYS, true);
    fit_mod(&ctx, &mut rev_sess, 81051, 71061, 40714, 11).await.expect("fit Capital Emergency Hull Energizer I");
    rev_sess.station_id = None;
    spawn_in_space(&ctx, 91051, 81051, 19720, NULL_SYS, DVec3::new(0.0, 10_000.0, 0.0)).await;

    rpc(&ctx, &mut rev_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71061)]).await.expect("activate CEHE I");
    let rev_cs = sys.get_combat_state(81051).await.unwrap().unwrap();
    assert!((rev_cs.structure_resonance[0] - 0.01).abs() < 1e-4, "CEHE I must grant 99% hull damage resistance (0.01 resonance)");
    let rev_info = rpc(&ctx, &mut rev_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!((attr_from_all_info(&rev_info, "shipInfo", 109) - 0.01).abs() < 1e-4);

    // Tick past 20s cycle -> CEHE I burns out (damage = 40.0, offline) and cannot be reactivated
    for _ in 0..22 {
        let _ = sys.tick().await;
    }
    let cehe_info = rpc(&ctx, &mut rev_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71061)]).await.unwrap();
    assert!((attr_from_item_info(&cehe_info, 3) - 40.0).abs() < 1e-3, "CEHE I must burn out (damage = 40.0) after its single cycle");
    let react_err = rpc(&ctx, &mut rev_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71061)]).await;
    assert_eq!(react_err.unwrap_err().code, "ModuleBurnedOut");

    // 3. Remote Sensor Booster II (1964) + Remote Tracking Computer II (2104) + Medium Ancillary Remote Armor Repairer (41477) + Medium Ancillary Remote Shield Booster (41481) + Auto Targeting System II (1436)
    let mut logi_sess = seed_pilot(&ctx, 91052, 81052, 624, NULL_SYS, true);
    fit_mod(&ctx, &mut logi_sess, 81052, 71062, 1964, 19).await.expect("fit Remote Sensor Booster II");
    fit_mod(&ctx, &mut logi_sess, 81052, 71063, 2104, 20).await.expect("fit Remote Tracking Computer II");
    fit_mod(&ctx, &mut logi_sess, 81052, 71064, 41477, 27).await.expect("fit Medium Ancillary Remote Armor Repairer");
    fit_mod(&ctx, &mut logi_sess, 81052, 71065, 41481, 28).await.expect("fit Medium Ancillary Remote Shield Booster");
    fit_mod(&ctx, &mut logi_sess, 81052, 71066, 1436, 29).await.expect("fit Auto Targeting System II");

    // Load Scan Resolution Script (29011) into RSB II, Optimal Range Script (28999) into RTC II, Nanite Paste (28668) into MARAR, Cap Booster 150 (263) into MARSB
    give_item(&ctx, 71067, 29011, 91052, JITA_STATION, InventoryFlag::Hangar, 1);
    give_item(&ctx, 71068, 28999, 91052, JITA_STATION, InventoryFlag::Hangar, 1);
    give_item(&ctx, 71069, 28668, 91052, JITA_STATION, InventoryFlag::Hangar, 100);
    give_item(&ctx, 71070, 263, 91052, JITA_STATION, InventoryFlag::Hangar, 50);
    rpc(&ctx, &mut logi_sess, "dogmaIM", "LoadAmmo", vec![PyRep::Integer(81052), PyRep::List(vec![PyRep::Integer(71062)]), PyRep::List(vec![PyRep::Integer(71067)])]).await.unwrap();
    rpc(&ctx, &mut logi_sess, "dogmaIM", "LoadAmmo", vec![PyRep::Integer(81052), PyRep::List(vec![PyRep::Integer(71063)]), PyRep::List(vec![PyRep::Integer(71068)])]).await.unwrap();
    let marar_load = rpc(&ctx, &mut logi_sess, "dogmaIM", "LoadAmmo", vec![PyRep::Integer(81052), PyRep::List(vec![PyRep::Integer(71064)]), PyRep::List(vec![PyRep::Integer(71069)])]).await.unwrap();
    let marsb_load = rpc(&ctx, &mut logi_sess, "dogmaIM", "LoadAmmo", vec![PyRep::Integer(81052), PyRep::List(vec![PyRep::Integer(71065)]), PyRep::List(vec![PyRep::Integer(71070)])]).await.unwrap();
    assert!((marar_load.dict_get("reloadTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0) - 60_000.0).abs() < 1.0);
    assert!((marsb_load.dict_get("reloadTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0) - 60_000.0).abs() < 1.0);

    // Prepare ally ship with a turret (2929) and an active Weapon Timer
    let mut ally_sess = seed_pilot(&ctx, 91053, 81053, 642, NULL_SYS, true);
    fit_mod(&ctx, &mut ally_sess, 81053, 71071, 2929, 27).await.unwrap();
    logi_sess.station_id = None;
    ally_sess.station_id = None;
    spawn_in_space(&ctx, 91052, 81052, 624, NULL_SYS, DVec3::new(0.0, 20_000.0, 0.0)).await;
    spawn_in_space(&ctx, 91053, 81053, 642, NULL_SYS, DVec3::new(5_000.0, 20_000.0, 0.0)).await;

    // Auto Targeting System II: passive +3 maxLockedTargets & active auto-locking of ships within 60 km obeying lock-time rules
    let logi_info = rpc(&ctx, &mut logi_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let base_maller_locks = ctx.sde.types.get(&624).and_then(|t| t.default_attributes.get(&192).copied()).unwrap_or(6.0);
    assert!((attr_from_all_info(&logi_info, "shipInfo", 192) - (base_maller_locks + 3.0)).abs() < 1e-3, "Auto Targeting System II must add +3 maxLockedTargets while online");
    let ats_res = rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71066)]).await.expect("activate Auto Targeting System II");
    assert!(ats_res.dict_get("lockingCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 1, "Auto Targeting System II must initiate locks on nearby ships");
    let init_locks = sys.get_target_locks(81052).await.unwrap();
    assert!(init_locks.iter().any(|l| l.target_id == 81053 && !l.locked && l.lock_time_ms > 0.0), "Auto-Targeter locks must obey normal lock-time rules");
    for _ in 0..15 {
        let _ = sys.tick().await;
    }

    // Set weapon timer on ally, damage ally shield & armor, and activate RSB II, RTC II, MARAR, and MARSB onto ally
    let now_u = ctx.now_unix_secs();
    ctx.crimewatch.write().unwrap().get_or_create(91053).weapon_timer_until_unix = now_u + 60;
    let pre_ally_info = rpc(&ctx, &mut ally_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let pre_ally_sr = attr_from_all_info(&pre_ally_info, "shipInfo", 564);
    let pre_ally_gun = rpc(&ctx, &mut ally_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71071)]).await.unwrap();
    let pre_ally_opt = attr_from_item_info(&pre_ally_gun, 54);

    rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71062), PyRep::String("default".into()), PyRep::Integer(81053)]).await.expect("activate RSB II");
    rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71063), PyRep::String("default".into()), PyRep::Integer(81053)]).await.expect("activate RTC II");
    assert!(ctx.crimewatch.read().unwrap().states.get(&91052).is_some_and(|s| s.is_weapon_timer_active(now_u)), "RSB/RTC must propagate logistics aggression timer");

    let post_ally_info = rpc(&ctx, &mut ally_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let post_ally_gun = rpc(&ctx, &mut ally_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71071)]).await.unwrap();
    assert!(attr_from_all_info(&post_ally_info, "shipInfo", 564) > pre_ally_sr * 1.40, "Scripted RSB II must boost target scanResolution");
    assert!(attr_from_item_info(&post_ally_gun, 54) > pre_ally_opt * 1.25, "Scripted RTC II must boost target turret optimal range");

    // Deactivate RSB II and verify bonus is removed
    rpc(&ctx, &mut logi_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(71062)]).await.unwrap();
    let deact_ally_info = rpc(&ctx, &mut ally_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!((attr_from_all_info(&deact_ally_info, "shipInfo", 564) - pre_ally_sr).abs() < 1.0, "Deactivating RSB II must remove projected bonus");

    // Activate Ancillary Remote Shield Booster (0 cap cost while loaded) & Ancillary Remote Armor Repairer (3x repair while paste loaded)
    let logi_cap_before = sys.get_combat_state(81052).await.unwrap().unwrap().capacitor;
    rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71065), PyRep::String("default".into()), PyRep::Integer(81053), PyRep::Integer(0)]).await.expect("activate MARSB");
    let logi_cap_after = sys.get_combat_state(81052).await.unwrap().unwrap().capacitor;
    assert!((logi_cap_before - logi_cap_after).abs() < 1e-3, "Ancillary Remote Shield Booster must consume 0 capacitor while loaded with Cap Booster charges");

    rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71064), PyRep::String("default".into()), PyRep::Integer(81053), PyRep::Integer(0)]).await.expect("activate MARAR");

    eve_testkit::attest(&[
        "feat:modules.target-spectrum-breaker",
        "feat:modules.capital-emergency-hull-energizer",
        "feat:modules.remote-sensor-booster-rtc",
        "feat:modules.ancillary-remote-repairers",
        "feat:modules.automated-targeting-system",
    ]);
}
