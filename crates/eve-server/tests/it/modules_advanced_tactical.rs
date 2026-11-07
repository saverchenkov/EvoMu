//! End-to-End Integration Tests for Tactical, Capital, Command, Fitting & Support Modules (`feat:modules.*`).

use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_SYS: u32 = 30000142;
pub(crate) const JITA_STATION: i64 = 60003760;
pub(crate) const NULL_SYS: u32 = 30004759;

pub(crate) fn build_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

pub(crate) fn seed_pilot(
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
        ch.jump_cooldown_until_unix = 0;
        ch.jump_fatigue_until_unix = 0;
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

pub(crate) fn set_skill_level(ctx: &ServerContext, char_id: i64, skill_id: u32, level: u8) {
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

pub(crate) fn set_char_alliance(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, alliance_id: i64) {
    sess.alliance_id = Some(alliance_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.alliance_id = Some(alliance_id);
        }
    });
}

pub(crate) fn set_item_dyn_attr(ctx: &ServerContext, item_id: i64, attr_id: u16, val: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&item_id) {
            it.dynamic_attributes.insert(attr_id, val);
        }
    });
}

pub(crate) fn seed_wormhole_pair(ctx: &ServerContext, from_sys: u32, to_sys: u32, mass_kg: f64) {
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
            last_sov_claim_tick: 0, ess_cycle_started_unix: 0, ess_main_bank_contributors: Default::default(),
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
            last_sov_claim_tick: 0, ess_cycle_started_unix: 0, ess_main_bank_contributors: Default::default(),
        });
        dst.wormhole_remaining_mass_kg = mass_kg;
    });
}

pub(crate) fn seed_fleet(ctx: &ServerContext, fleet_id: i64, boss_id: i64, members: Vec<i64>) {
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

pub(crate) fn give_item(
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

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, &args, &HashMap::new(), None)
        .await
}

pub(crate) async fn fit_mod(
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

pub(crate) async fn spawn_in_space(
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

pub(crate) fn attr_from_all_info(info: &PyRep, section: &str, attr_id: u16) -> f64 {
    let dict_opt = if let Some(sec) = info.dict_get("shipAttributes") {
        Some(sec)
    } else if let Some(sec) = info.dict_get(section) {
        if let Some(sub) = sec.dict_get("attributes") {
            Some(sub)
        } else if let PyRep::Dict(entries) = sec {
            entries.first().and_then(|(_, item_data)| item_data.dict_get("attributes")).or(Some(sec))
        } else {
            Some(sec)
        }
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

pub(crate) fn attr_from_item_info(info: &PyRep, attr_id: u16) -> f64 {
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
                is_cloaked: false, ..Default::default()
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
    // parity: feat:modules.siege-module-v24-effects, feat:modules.triage-module-v24-effects
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
    assert_eq!(launch_err.unwrap_err().code, "MaxBandwidthExceeded");

    rpc(&ctx, &mut fax_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(81010)]).await.unwrap();
    for _ in 0..25 {
        let _ = sys.tick().await;
    }
    let ewar_err = rpc(&ctx, &mut fax_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71013), PyRep::String("default".into()), PyRep::Integer(81010)]).await;
    assert_eq!(ewar_err.unwrap_err().code, "EwarDisabledInTriage");

    // 3. Disruptive Lance on Bane (77283, group 4594)
    let mut bane_sess = seed_pilot(&ctx, 91015, 81015, 77283, NULL_SYS, true);
    fit_mod(&ctx, &mut bane_sess, 81015, 71016, 4292, 27).await.expect("fit Siege II on Bane");
    fit_mod(&ctx, &mut bane_sess, 81015, 71017, 77399, 28).await.expect("fit Disruptive Lance on Bane");
    fit_mod(&ctx, &mut bane_sess, 81015, 71018, 11370, 29).await.expect("fit Cloak on Bane");
    give_item(&ctx, 71019, 16275, 91015, 81015, InventoryFlag::Cargo, 500);
    give_item(&ctx, 71020, 16274, 91015, 81015, InventoryFlag::Cargo, 30_000);
    bane_sess.station_id = None;
    spawn_in_space(&ctx, 91015, 81015, 77283, NULL_SYS, DVec3::new(0.0, 0.0, 0.0)).await;

    // Cloak rejected while Lance is fitted
    let cloak_err = rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71018)]).await;
    assert_eq!(cloak_err.unwrap_err().code, "CannotCloakWithLanceFitted");

    // Lance rejected before Siege is active
    let no_siege_err = rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71017), PyRep::String("default".into()), PyRep::Integer(81010)]).await;
    assert_eq!(no_siege_err.unwrap_err().code, "SiegeRequiredForLance");

    // Spawn victim ship in beam corridor at (50 km, 0, 0) and bystander outside corridor at (50 km, 10 km, 0)
    let mut victim_sess = seed_pilot(&ctx, 91016, 81016, 19724, NULL_SYS, false);
    spawn_in_space(&ctx, 91016, 81016, 19724, NULL_SYS, DVec3::new(50_000.0, 0.0, 0.0)).await;
    let _ = seed_pilot(&ctx, 91017, 81017, 624, NULL_SYS, false);
    spawn_in_space(&ctx, 91017, 81017, 624, NULL_SYS, DVec3::new(50_000.0, 10_000.0, 0.0)).await;

    rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71016)]).await.expect("activate Siege on Bane");
    let lance_res = rpc(&ctx, &mut bane_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71017), PyRep::String("default".into()), PyRep::Integer(81016)]).await.expect("fire Disruptive Lance");
    assert_eq!(lance_res.dict_get("hitCount").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(lance_res.dict_get("casterLockSeconds").and_then(|v| v.as_i64()), Some(300));
    assert_eq!(lance_res.dict_get("victimDebuffSeconds").and_then(|v| v.as_i64()), Some(60));
    assert!((lance_res.dict_get("remoteRepairImpedance").and_then(|v| v.as_f64()).unwrap_or(0.0) - 0.50).abs() < 1e-4);

    let rem_iso = ctx.db.with_snapshot(|s| s.items.get(&71020).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(rem_iso, 10_000, "Disruptive Lance must consume 20,000 isotopes from SDE attr 714");

    // Victim cannot warp/jump/dock and has 50% incoming remote repair impedance
    let beyonce_id = ctx.bind_object(&mut victim_sess, "beyonce", NULL_SYS as i64).unwrap();
    let victim_warp = ctx.dispatch_rpc(&mut victim_sess, "beyonce", Some(beyonce_id), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(40000001)], &HashMap::new(), None).await;
    assert_eq!(victim_warp.unwrap_err().code, "DisruptiveLanceDebuffActive");
    assert!((eve_server::services::tactical_modules::incoming_remote_repair_multiplier(&ctx, 81016) - 0.50).abs() < 1e-4);

    eve_testkit::attest(&[
        "feat:modules.siege-module-v24-effects",
        "feat:modules.triage-module-v24-effects",
    ]);
}


#[allow(unused_imports, dead_code)]
mod entosis_command_bursts_and_ewar_modules {
    use super::*;

// End-to-End Integration Tests for Tactical, Capital, Command, Fitting & Support Modules (`feat:modules.*`).

use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_SYS: u32 = 30000142;
use crate::modules_advanced_tactical::*;

#[tokio::test]
async fn entosis_link_restrictions_warmup_and_contest() {
    // parity: feat:modules.entosis-activation-restrictions, feat:modules.entosis-warmup-and-contest
    let ctx = build_ctx();

    // 1. Interceptor (Crow 11176, group 831) cannot activate Entosis Link
    let mut crow_sess = seed_pilot(&ctx, 91020, 81020, 11176, NULL_SYS, true);
    set_item_dyn_attr(&ctx, 81020, 48, 10000.0); // CPU_OUTPUT
    set_item_dyn_attr(&ctx, 81020, 11, 10000.0); // POWER_OUTPUT
    give_item(&ctx, 71021, 34593, 91020, 81020, InventoryFlag::HiSlot0, 1);
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.module_online_states.insert(71021, true);
            Ok::<(), ()>(())
        })
        .unwrap();
    crow_sess.station_id = None;
    spawn_in_space(&ctx, 91020, 81020, 11176, NULL_SYS, DVec3::ZERO).await;
    let node_id = 88001i64;
    give_item(&ctx, node_id, 32458, 1, NULL_SYS as i64, InventoryFlag::None, 1);
    spawn_in_space(&ctx, 1, node_id, 32458, NULL_SYS, DVec3::new(5_000.0, 0.0, 0.0)).await;

    let inty_err =
        rpc(&ctx, &mut crow_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71021), PyRep::String("default".into()), PyRep::Integer(node_id)]).await;
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
    let c1 = rpc(&ctx, &mut atk_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71022), PyRep::String("default".into()), PyRep::Integer(node_id)])
        .await
        .expect("Entosis cycle 1");
    assert_eq!(c1.dict_get("cycleIndex").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(c1.dict_get("isWarmup").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(c1.dict_get("progressDelta").and_then(|v| v.as_f64()), Some(0.0));

    // While active, warp is blocked
    let b_id = ctx.bind_object(&mut atk_sess, "beyonce", NULL_SYS as i64).unwrap();
    let warp_err = ctx
        .dispatch_rpc(&mut atk_sess, "beyonce", Some(b_id), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(40000001)], &HashMap::new(), None)
        .await;
    assert_eq!(warp_err.unwrap_err().code, "EntosisLinkActive");

    // Advance clock past cycle 1 (120s) and run Cycle 2: Active capture (isWarmup = false, progressDelta > 0.0)
    ctx.advance_virtual_clock(125);
    let c2 = rpc(&ctx, &mut atk_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71022), PyRep::String("default".into()), PyRep::Integer(node_id)])
        .await
        .expect("Entosis cycle 2");
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
    let _def_c2 =
        rpc(&ctx, &mut def_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71024), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.unwrap();
    let atk_c3 =
        rpc(&ctx, &mut atk_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71022), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.unwrap();
    assert_eq!(atk_c3.dict_get("contested").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(atk_c3.dict_get("progressDelta").and_then(|v| v.as_f64()), Some(0.0));

    // Unlocking target resets warm-up back to cycle 1
    ctx.advance_virtual_clock(125);
    rpc(&ctx, &mut atk_sess, "dogmaIM", "RemoveTarget", vec![PyRep::Integer(node_id)]).await.unwrap();
    rpc(&ctx, &mut atk_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(node_id)]).await.unwrap();
    for _ in 0..20 {
        let _ = sys.tick().await;
    }
    let reset_c1 =
        rpc(&ctx, &mut atk_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71022), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.unwrap();
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
    let cap_ent =
        rpc(&ctx, &mut cap_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71026), PyRep::String("default".into()), PyRep::Integer(node_id)]).await.unwrap();
    assert!(
        (cap_ent.dict_get("cycleDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0) - 600.0).abs() < 1.0,
        "Capital Entosis II must have 5x cycle duration (600s)"
    );
    assert!((cap_ent.dict_get("remoteAssistanceImpedance").and_then(|v| v.as_f64()).unwrap_or(0.0) - 0.15).abs() < 1e-4);

    eve_testkit::attest(&["feat:modules.entosis-activation-restrictions", "feat:modules.entosis-warmup-and-contest"]);
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
    assert!(
        (load_res.dict_get("reloadTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0) - 30_000.0).abs() < 1.0,
        "Command Burst Specialist L5 must reduce reload time from 60s to 30s"
    );

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
    assert!(
        (burst_res.dict_get("durationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0) - 22.5).abs() < 0.1,
        "Shield Command L5 (+50%) must scale 15s cycle to 22.5s"
    );
    assert!(
        (burst_res.dict_get("buffDurationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0) - 90.0).abs() < 0.1,
        "Shield Command L5 (+50%) must scale 60s buff to 90s"
    );
    assert!(
        burst_res.dict_get("burstStrengthMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0) > 1.85,
        "Specialist L5 * T2 * Command Ship bonus must exceed 1.85x"
    );

    let post_near = rpc(&ctx, &mut near_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let post_far = rpc(&ctx, &mut far_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!(attr_from_all_info(&post_near, "shipInfo", 263) > pre_shield * 1.15, "In-range fleetmate must receive Shield Extension buff");
    assert!((attr_from_all_info(&post_far, "shipInfo", 263) - pre_shield).abs() < 1.0, "Out-of-range fleetmate must not receive buff");

    // 3. Industrial Core II (42890) & Clone Vat Bay I (23735) on Rorqual (28352, group 883)
    let mut rorq_sess = seed_pilot(&ctx, 91035, 81035, 28352, NULL_SYS, true);
    fit_mod(&ctx, &mut rorq_sess, 81035, 71036, 42890, 27).await.expect("fit Industrial Core II");
    fit_mod(&ctx, &mut rorq_sess, 81035, 71037, 43551, 28).await.expect("fit Mining Foreman Burst II");
    fit_mod(&ctx, &mut rorq_sess, 81035, 71038, 23735, 29).await.expect("fit Clone Vat Bay I");
    give_item(&ctx, 71039, 42830, 91035, JITA_STATION, InventoryFlag::Hangar, 200);
    rpc(
        &ctx,
        &mut rorq_sess,
        "dogmaIM",
        "LoadAmmo",
        vec![PyRep::Integer(81035), PyRep::List(vec![PyRep::Integer(71037)]), PyRep::List(vec![PyRep::Integer(71039)])],
    )
    .await
    .unwrap();
    give_item(&ctx, 71040, 16272, 91035, 81035, InventoryFlag::Cargo, 2_000);
    give_item(&ctx, 71041, 16273, 91035, 81035, InventoryFlag::Cargo, 10);
    rorq_sess.station_id = None;
    spawn_in_space(&ctx, 91035, 81035, 28352, NULL_SYS, DVec3::ZERO).await;

    let pre_ind_burst = rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71037)]).await.unwrap();
    let pre_ind_str = pre_ind_burst.dict_get("burstStrengthMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let pre_ind_rng = pre_ind_burst.dict_get("aoeRange").and_then(|v| v.as_f64()).unwrap_or(0.0);

    rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71036)]).await.expect("activate Industrial Core II");
    let post_ind_burst = rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71037)]).await.unwrap();
    assert!(
        post_ind_burst.dict_get("burstStrengthMultiplier").and_then(|v| v.as_f64()).unwrap_or(0.0) > pre_ind_str,
        "Industrial Core II must boost Mining Foreman Burst strength"
    );
    assert!(
        post_ind_burst.dict_get("aoeRange").and_then(|v| v.as_f64()).unwrap_or(0.0) > pre_ind_rng,
        "Industrial Core II must boost Mining Foreman Burst range"
    );

    // Clone Vat Bay I activation, offer & accept jump clone installation consuming 1 Liquid Ozone (16273)
    rpc(&ctx, &mut rorq_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71038)])
        .await
        .expect("activate Clone Vat Bay I");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(fl) = snap.fleets.get_mut(&5001) {
            fl.members.push(91035);
        }
        if let Some(ch) = snap.characters.get_mut(&91032) {
            ch.solar_system_id = NULL_SYS;
        }
    });
    near_sess.solar_system_id = Some(NULL_SYS);
    rpc(&ctx, &mut rorq_sess, "jumpCloneSvc", "OfferShipCloneInstallation", vec![PyRep::Integer(91032), PyRep::Integer(81035)])
        .await
        .expect("offer ship clone");
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
    rpc(&ctx, &mut ship_sess, "invbroker", "Add", vec![PyRep::Integer(71047), PyRep::Integer(81040), PyRep::Integer(InventoryFlag::Cargo as i64)])
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
    assert!(
        (attr_from_all_info(&post_vex_info, "shipInfo", 458) - (pre_dcd + 20_000.0)).abs() < 1.0,
        "Drone Link Augmentor I must add +20 km droneControlDistance"
    );
    let post_drone = rpc(&ctx, &mut vexor_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71048)]).await.unwrap();
    assert!(attr_from_item_info(&post_drone, 37) > pre_d_vel * 1.20, "Drone Navigation Computer I must boost drone maxVelocity");
    assert!(attr_from_item_info(&post_drone, 54) > pre_d_opt * 1.04, "Active Omnidirectional Tracking Link I must boost drone optimal range");
    assert!(attr_from_item_info(&post_drone, 64) > pre_d_dmg * 1.14, "Drone Damage Amplifier I must boost drone damageMultiplier");

    // 3. Stasis Webification Probe (56748) in Interdiction Sphere Launcher I (22782) on Sabre (22456)
    let mut sabre_sess = seed_pilot(&ctx, 91042, 81042, 22456, NULL_SYS, true);
    fit_mod(&ctx, &mut sabre_sess, 81042, 71053, 22782, 27).await.expect("fit Interdiction Sphere Launcher I");
    give_item(&ctx, 71054, 56748, 91042, JITA_STATION, InventoryFlag::Hangar, 10);
    let wub_load = rpc(
        &ctx,
        &mut sabre_sess,
        "dogmaIM",
        "LoadAmmo",
        vec![PyRep::Integer(81042), PyRep::List(vec![PyRep::Integer(71053)]), PyRep::List(vec![PyRep::Integer(71054)])],
    )
    .await
    .expect("load Stasis Webification Probes");
    assert_eq!(wub_load.dict_get("totalLoaded").and_then(|v| v.as_i64()), Some(5), "15 m3 launcher holds 5x 3 m3 Stasis Webification Probes");

    sabre_sess.station_id = None;
    spawn_in_space(&ctx, 91042, 81042, 22456, NULL_SYS, DVec3::new(0.0, 50_000.0, 0.0)).await;
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let (_, pre_sabre_ball) = sys.get_ball_snapshot(81042).await.unwrap().unwrap();
    let base_sabre_vel = pre_sabre_ball.max_velocity;

    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71053)]).await.expect("launch Stasis Webification Probe");

    // After 1s (before 2s SDE arming delay), velocity is unchanged
    let _ = sys.tick().await;
    let (_, mid_ball) = sys.get_ball_snapshot(81042).await.unwrap().unwrap();
    assert!((mid_ball.max_velocity - base_sabre_vel).abs() < 1e-3, "Wubble probe must not web before 2s arming delay");

    // After 2s total, probe detonates and applies -20% velocity debuff (0.80x from SDE speedFactor=-20) for 30s
    for _ in 0..2 {
        let _ = sys.tick().await;
    }
    let (_, webbed_ball) = sys.get_ball_snapshot(81042).await.unwrap().unwrap();
    assert!((webbed_ball.max_velocity - base_sabre_vel * 0.80).abs() < 1.0, "Detonated Wubble probe must apply -20% velocity debuff");

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
    assert!((rev_cs.structure_resonance[0] - 0.05).abs() < 1e-4, "CEHE I must grant 95% hull damage resistance (0.05 resonance)");
    let rev_info = rpc(&ctx, &mut rev_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!((attr_from_all_info(&rev_info, "shipInfo", 109) - 0.05).abs() < 1e-4);

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
    rpc(
        &ctx,
        &mut logi_sess,
        "dogmaIM",
        "LoadAmmo",
        vec![PyRep::Integer(81052), PyRep::List(vec![PyRep::Integer(71062)]), PyRep::List(vec![PyRep::Integer(71067)])],
    )
    .await
    .unwrap();
    rpc(
        &ctx,
        &mut logi_sess,
        "dogmaIM",
        "LoadAmmo",
        vec![PyRep::Integer(81052), PyRep::List(vec![PyRep::Integer(71063)]), PyRep::List(vec![PyRep::Integer(71068)])],
    )
    .await
    .unwrap();
    let marar_load = rpc(
        &ctx,
        &mut logi_sess,
        "dogmaIM",
        "LoadAmmo",
        vec![PyRep::Integer(81052), PyRep::List(vec![PyRep::Integer(71064)]), PyRep::List(vec![PyRep::Integer(71069)])],
    )
    .await
    .unwrap();
    let marsb_load = rpc(
        &ctx,
        &mut logi_sess,
        "dogmaIM",
        "LoadAmmo",
        vec![PyRep::Integer(81052), PyRep::List(vec![PyRep::Integer(71065)]), PyRep::List(vec![PyRep::Integer(71070)])],
    )
    .await
    .unwrap();
    assert!((marar_load.dict_get("reloadTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0) - 60_000.0).abs() < 1.0);
    assert!((marsb_load.dict_get("reloadTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0) - 60_000.0).abs() < 1.0);

    // Prepare ally ship with a turret (2929) and an active Weapon Timer
    let mut ally_sess = seed_pilot(&ctx, 91053, 81053, 642, NULL_SYS, true);
    fit_mod(&ctx, &mut ally_sess, 81053, 71071, 2929, 27).await.unwrap();
    logi_sess.station_id = None;
    ally_sess.station_id = None;
    spawn_in_space(&ctx, 91052, 81052, 624, NULL_SYS, DVec3::new(0.0, 20_000.0, 0.0)).await;
    spawn_in_space(&ctx, 91053, 81053, 642, NULL_SYS, DVec3::new(5_000.0, 20_000.0, 0.0)).await;
    sys.spawn_ball_with_meta(
        eve_destiny::Ball {
            id: 81_099,
            type_id: 2_372,
            position: DVec3::new(10_000.0, 20_000.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 35.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
        eve_destiny::SlimItemMeta { group_id: 550, category_id: 11, owner_id: 500_010, ..Default::default() },
    )
    .await
    .unwrap();

    // Auto Targeting System II: passive +3 maxLockedTargets & active auto-locking of hostile targets within 60 km obeying lock-time rules
    let logi_info = rpc(&ctx, &mut logi_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let base_maller_locks = ctx.sde.types.get(&624).and_then(|t| t.default_attributes.get(&192).copied()).unwrap_or(6.0);
    assert!(
        (attr_from_all_info(&logi_info, "shipInfo", 192) - (base_maller_locks + 3.0)).abs() < 1e-3,
        "Auto Targeting System II must add +3 maxLockedTargets while online"
    );
    let ats_res = rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71066)]).await.expect("activate Auto Targeting System II");
    assert!(ats_res.dict_get("lockingCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 1, "Auto Targeting System II must initiate locks on nearby hostile targets");
    let init_locks = sys.get_target_locks(81052).await.unwrap();
    assert!(init_locks.iter().any(|l| l.target_id == 81_099 && !l.locked && l.lock_time_ms > 0.0), "Auto-Targeter locks must obey normal lock-time rules");
    rpc(&ctx, &mut logi_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(81053)]).await.unwrap();
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

    rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71062), PyRep::String("default".into()), PyRep::Integer(81053)])
        .await
        .expect("activate RSB II");
    rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71063), PyRep::String("default".into()), PyRep::Integer(81053)])
        .await
        .expect("activate RTC II");
    assert!(
        ctx.crimewatch.read().unwrap().states.get(&91052).is_some_and(|s| s.is_weapon_timer_active(now_u)),
        "RSB/RTC must propagate logistics aggression timer"
    );

    let post_ally_info = rpc(&ctx, &mut ally_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    let post_ally_gun = rpc(&ctx, &mut ally_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(71071)]).await.unwrap();
    assert!(attr_from_all_info(&post_ally_info, "shipInfo", 564) > pre_ally_sr * 1.40, "Scripted RSB II must boost target scanResolution");
    assert!(attr_from_item_info(&post_ally_gun, 54) > pre_ally_opt * 1.14, "Scripted RTC II must boost target turret optimal range");

    // Deactivate RSB II and verify bonus is removed
    rpc(&ctx, &mut logi_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(71062)]).await.unwrap();
    let deact_ally_info = rpc(&ctx, &mut ally_sess, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)]).await.unwrap();
    assert!((attr_from_all_info(&deact_ally_info, "shipInfo", 564) - pre_ally_sr).abs() < 1.0, "Deactivating RSB II must remove projected bonus");

    // Activate Ancillary Remote Shield Booster (0 cap cost while loaded) & Ancillary Remote Armor Repairer (3x repair while paste loaded)
    let logi_cap_before = sys.get_combat_state(81052).await.unwrap().unwrap().capacitor;
    rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71065), PyRep::String("default".into()), PyRep::Integer(81053), PyRep::Integer(0)])
        .await
        .expect("activate MARSB");
    let logi_cap_after = sys.get_combat_state(81052).await.unwrap().unwrap().capacitor;
    assert!((logi_cap_before - logi_cap_after).abs() < 1e-3, "Ancillary Remote Shield Booster must consume 0 capacitor while loaded with Cap Booster charges");

    rpc(&ctx, &mut logi_sess, "dogmaIM", "Activate", vec![PyRep::Integer(71064), PyRep::String("default".into()), PyRep::Integer(81053), PyRep::Integer(0)])
        .await
        .expect("activate MARAR");

    eve_testkit::attest(&[
        "feat:modules.target-spectrum-breaker",
        "feat:modules.capital-emergency-hull-energizer",
        "feat:modules.remote-sensor-booster-rtc",
        "feat:modules.ancillary-remote-repairers",
        "feat:modules.automated-targeting-system",
    ]);
}

}

// Folded submodule: nav_tactical_modules_suite
#[allow(unused_imports, dead_code)]
mod nav_tactical_modules_suite {
    use super::*;

// End-to-End Integration Tests closing Navigation parity rows (Part 1):
// `feat:navigation.align-server-tick-rounding`, `feat:navigation.collision-bumping`,
// and `feat:navigation.cloak-velocity-penalty`.

use crate::modules_advanced_tactical::*;
use eve_db::{InventoryFlag, UpwellStructureRecord};
use eve_destiny::{Ball, DestinyEvent, DVec3, MovementMode, WarpPhase};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_waypoints_warp_vector_and_align_tick_rounding() {
    // parity: feat:navigation.align-server-tick-rounding
    eve_testkit::attest(&[
        "feat:navigation.align-server-tick-rounding",
    ]);

    let ctx = Arc::new(build_ctx());
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let char_id: i64 = 94_001;
    let ship_id: i64 = 84_001;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, true);

    // 1. Route waypoints via real V24 surfaces (`charMgr` + ESI `/v2/ui/autopilot/waypoint/` + `beyonce`)
    let init_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![]).await.expect("GetCharacterSettings initial");
    assert_eq!(init_settings, PyRep::Dict(Vec::new()));

    // Invalid destination ID rejected on charMgr.SaveCharacterSetting("waypoints", ...)
    let bad_wp = rpc(&ctx, &mut sess, "charMgr", "SaveCharacterSetting", vec![
        PyRep::String("waypoints".into()),
        PyRep::List(vec![PyRep::Integer(999_999_999)]),
    ]).await.unwrap_err();
    assert_eq!(bad_wp.code, "InvalidDestination");

    // Pick a Jita stargate jump destination so we can test live autopilot warp + jump arrival advancement
    let jita_jump = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == jita_sys)
        .expect("Jita stargate jump")
        .clone();
    let dest_sys_id = jita_jump.to_solar_system_id;

    // SaveCharacterSetting("waypoints", ...) persists to DB & survives fresh session reconnect
    rpc(&ctx, &mut sess, "charMgr", "SaveCharacterSetting", vec![
        PyRep::String("waypoints".into()),
        PyRep::List(vec![PyRep::Integer(dest_sys_id as i64), PyRep::Integer(JITA_STATION)]),
    ]).await.expect("SaveCharacterSetting waypoints");

    let mut reconnect_sess = SessionState::new(994_001);
    reconnect_sess.user_id = sess.user_id;
    reconnect_sess.character_id = Some(char_id);
    let reconnect_settings = rpc(&ctx, &mut reconnect_sess, "charMgr", "GetCharacterSettings", vec![])
        .await.expect("GetCharacterSettings on reconnected session");
    assert_eq!(
        reconnect_settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(dest_sys_id as i64), PyRep::Integer(JITA_STATION)]))
    );

    // Undock in Jita and verify beyonce.CmdWarpToStuffAutopilot uses 15 km default landing perimeter
    sess.station_id = None;
    let gate_radius = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
    spawn_in_space(
        &ctx,
        char_id,
        ship_id,
        587,
        jita_sys,
        jita_jump.position - DVec3::new(10_000_000.0, 0.0, 0.0),
    )
    .await;
    let b_jita = ctx
        .bind_object(&mut sess, "beyonce", jita_sys as i64)
        .expect("bind beyonce Jita");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(b_jita),
        "CmdWarpToStuffAutopilot",
        &[PyRep::Integer(jita_jump.stargate_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuffAutopilot to stargate");
    let (_, ap_ball) = ctx
        .systems
        .get_or_spawn(jita_sys)
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .unwrap();
    let MovementMode::Warp { destination: ap_dest, .. } = ap_ball.mode else {
        panic!("Expected Warp mode from CmdWarpToStuffAutopilot");
    };
    assert!(
        (ap_dest.distance(jita_jump.position) - (gate_radius + 15_000.0)).abs() < 10.0,
        "Autopilot warp must land at gate_radius + 15,000m"
    );

    // Stargate jump refused at ~3 km outside gate perimeter (> 2,500 m jump range)
    let jita_gate_ball_r = ctx
        .systems
        .get_or_spawn(jita_sys)
        .get_ball_snapshot(jita_jump.stargate_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.radius)
        .unwrap_or(gate_radius);
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, jita_jump.position + DVec3::new(jita_gate_ball_r + 3_000.0, 0.0, 0.0)).await;
    let far_jump = ctx
        .dispatch_rpc(&mut sess, "beyonce", Some(b_jita), "CmdStargateJump", &[PyRep::Integer(jita_jump.stargate_id)], &kw, None)
        .await;
    assert_eq!(far_jump.unwrap_err().code, "OutsideStargateJumpRange");

    // Stargate jump arrival pops matching front waypoint (`dest_sys_id`)
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, jita_jump.position).await;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(b_jita),
        "CmdStargateJump",
        &[PyRep::Integer(jita_jump.stargate_id)],
        &kw,
        None,
    )
    .await
    .expect("CmdStargateJump advances waypoint");
    let post_jump_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings post jump");
    assert_eq!(
        post_jump_settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(JITA_STATION)]))
    );

    // Return to Jita and dock at Jita 4-4 Station -> pops final waypoint & removes "waypoints" setting
    ctx.session_change_timers.lock().unwrap().remove(&char_id);
    sess.solar_system_id = Some(jita_sys);
    let station_pos = ctx.sde.stations[&JITA_STATION].position;
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, station_pos).await;
    ctx.systems
        .get_or_spawn(jita_sys)
        .set_ball_position(ship_id, station_pos)
        .await;
    let b_jita_dock = ctx
        .bind_object(&mut sess, "beyonce", jita_sys as i64)
        .expect("rebind beyonce Jita");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(b_jita_dock),
        "CmdDock",
        &[PyRep::Integer(JITA_STATION)],
        &kw,
        None,
    )
    .await
    .expect("CmdDock advances final waypoint");
    let post_dock_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings post dock");
    assert_eq!(post_dock_settings.dict_get("waypoints"), None);

    // ESI POST /v2/ui/autopilot/waypoint/ (clear_other_waypoints, append, add_to_beginning, invalid 400)
    let uid = ctx
        .db
        .provision_player_account("nav_esi_pilot", "nav-esi-secret");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().user_id = uid;
    });
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid,
        sess.session_id,
        Some(char_id),
        Some("nav_esi_hash".into()),
        true,
        ctx.now_unix(),
    );
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "nav_esi_pilot",
            "nav-esi-secret",
            char_id,
            "evomu-nav-test",
            "https://localhost/callback",
            vec!["esi-ui.write_waypoint.v1".into()],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let token = ctx
        .db
        .exchange_pkce_code(
            &code,
            "evomu-nav-test",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .expect("exchange_pkce_code")
        .access_token;
    let esi = EsiRouter::new(Arc::clone(&ctx));
    let bad_esi = esi.handle_request(
        &EsiHttpRequest::new(
            "POST",
            "/v2/ui/autopilot/waypoint/?destination_id=999999999&add_to_beginning=false&clear_other_waypoints=true",
        )
        .with_bearer_token(&token),
    );
    assert_eq!(bad_esi.status_code, 404);

    for url in [
        "/v2/ui/autopilot/waypoint/?destination_id=30002187&add_to_beginning=false&clear_other_waypoints=true".to_string(),
        format!("/v2/ui/autopilot/waypoint/?destination_id={JITA_STATION}&add_to_beginning=false&clear_other_waypoints=false"),
        format!("/v2/ui/autopilot/waypoint/?destination_id={dest_sys_id}&add_to_beginning=true&clear_other_waypoints=false"),
    ] {
        let resp = esi.handle_request(&EsiHttpRequest::new("POST", &url).with_bearer_token(&token));
        assert_eq!(resp.status_code, 204);
    }
    let esi_settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings after ESI waypoints");
    assert_eq!(
        esi_settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![
            PyRep::Integer(dest_sys_id as i64),
            PyRep::Integer(30_002_187),
            PyRep::Integer(JITA_STATION),
        ]))
    );
    rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "DeleteCharacterSetting",
        vec![PyRep::String("waypoints".into())],
    )
    .await
    .expect("DeleteCharacterSetting waypoints");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone()),
        Vec::<i64>::new()
    );

    // 2. Cargo Warp Vector Data: star-centered coordinates + 300s cooldown
    sess.station_id = None;
    let star_cel = ctx
        .sde
        .celestials_in_system(jita_sys)
        .into_iter()
        .find(|c| c.group_id == 6)
        .expect("Jita star");
    let vec_item_id: i64 = 74_001;
    give_item(&ctx, vec_item_id, 77_825, char_id, ship_id, InventoryFlag::Cargo, 2);

    // Non-inventory space ball IDs (e.g., stargate or container ball) are rejected with InvalidVectorItem (no beacon fallback)
    let far_from_star = star_cel.position + DVec3::new(2_000_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_id, ship_id, 587, jita_sys, far_from_star).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(jita_jump.stargate_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidVectorItem"
    );

    // Out of star proximity (> 1,496,000,000,000 m = 10 AU) -> rejected
    assert_eq!(
        rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .unwrap_err()
            .code,
        "WarpVectorTooFarFromStar"
    );

    // Place ship at a non-zero 3D offset from star so star_pos + delta != ship_pos + delta!
    let near_star = star_cel.position + DVec3::new(500_000_000.0, 250_000_000.0, -100_000_000.0);
    let sys = ctx.systems.get_or_spawn(jita_sys);
    sys.set_ball_position(ship_id, near_star).await;
    let vec_res = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
        .await
        .expect("UseVector with cargo Warp Vector Data");
    assert_eq!(vec_res.dict_get("consumed"), Some(&PyRep::Bool(true)));
    assert_eq!(vec_res.dict_get("remainingQuantity"), Some(&PyRep::Integer(1)));
    let expected_vec_dest = star_cel.position + DVec3::new(220_000_000_000.0, -140_000_000_000.0, 0.0);
    let (_, vec_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: actual_vec_dest, .. } = vec_ball.mode else {
        panic!("Expected MovementMode::Warp after cargo Warp Vector Data activation");
    };
    assert!(
        actual_vec_dest.distance(expected_vec_dest) < 10.0,
        "Warp Vector Data must resolve relative to solar-system star coordinates: got {actual_vec_dest:?} vs {expected_vec_dest:?}"
    );
    assert!(
        actual_vec_dest.distance(near_star + DVec3::new(220_000_000_000.0, -140_000_000_000.0, 0.0)) > 100_000_000.0,
        "Warp Vector destination must NOT be ship-relative"
    );

    // 300s per-character cooldown enforced on immediate 2nd activation, clears after 301s
    sys.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
    sys.set_ball_position(ship_id, near_star).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .unwrap_err()
            .code,
        "WarpVectorCooldown"
    );
    ctx.advance_secs(301);
    // Vector destination < 150,000 m from ship is rejected without consuming item
    sys.set_ball_position(ship_id, expected_vec_dest - DVec3::new(100_000.0, 0.0, 0.0)).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
            .await
            .unwrap_err()
            .code,
        "WarpDistanceTooShort"
    );
    sys.set_ball_position(ship_id, near_star).await;
    rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
        .await
        .expect("UseVector succeeds after 300s cooldown");

    // 3. Server-Tick Sub-Tick Align Rounding (`feat:navigation.align-server-tick-rounding`)
    // Exact align time = ln(4) * inertia * mass / 1e6 = 3.200000 s -> ceil(3.2) = 4 ticks!
    let t_align_3_2_inertia = 3.2 / 4.0_f64.ln();
    let align_origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    let align_dest = align_origin + DVec3::new(10_000_000.0, 0.0, 0.0);
    sys.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: align_origin,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: t_align_3_2_inertia,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::new_warp(align_origin, align_dest, 3.0),
    })
    .await
    .unwrap();

    for tick_idx in 1..=3 {
        sys.step_manual(1.0).await.unwrap();
        let (_, b) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        assert!(
            matches!(b.mode, MovementMode::Warp { phase: WarpPhase::Aligning, .. }),
            "Ship with 3.2s align time must remain Aligning on tick {tick_idx}"
        );
    }
    sys.step_manual(1.0).await.unwrap();
    let (_, b_t4) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(
            b_t4.mode,
            MovementMode::Warp {
                phase: WarpPhase::Accelerating | WarpPhase::Cruising | WarpPhase::Decelerating,
                ..
            }
        ),
        "Ship with 3.2s align time must enter warp on tick 4 (4.0s): got {:?}",
        b_t4.mode
    );
}

#[tokio::test]
async fn test_collision_bumping_warp_landing_points_and_cloak_velocity() {
    // parity: feat:navigation.collision-bumping, feat:navigation.cloak-velocity-penalty
    eve_testkit::attest(&[
        "feat:navigation.collision-bumping",
        "feat:navigation.cloak-velocity-penalty",
    ]);

    let ctx = build_ctx();
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let char_a: i64 = 94_011;
    let ship_a: i64 = 84_011;
    let char_b: i64 = 94_012;
    let ship_b: i64 = 84_012;
    let char_obs: i64 = 94_013;
    let ship_obs: i64 = 84_013;
    let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 587, jita_sys, false);
    let _sess_b = seed_pilot(&ctx, char_b, ship_b, 640, jita_sys, false);
    let _sess_obs = seed_pilot(&ctx, char_obs, ship_obs, 587, jita_sys, false);
    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Ship-to-ship collision bumping (SDE hull masses, closed-form impulse, observer updates, & align reset)
    let bump_origin = DVec3::new(80_000_000_000.0, 0.0, 0.0);
    let ship_b_pos = bump_origin + DVec3::new(150.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_a, ship_a, 587, jita_sys, bump_origin).await;
    spawn_in_space(&ctx, char_b, ship_b, 640, jita_sys, ship_b_pos).await;
    spawn_in_space(&ctx, char_obs, ship_obs, 587, jita_sys, bump_origin + DVec3::new(0.0, 2_000.0, 0.0)).await;
    for _ in 0..30 {
        let _ = sys.step_manual(1.0).await.unwrap();
    }
    let _ = sys.get_set_state(ship_obs).await.unwrap();
    let _ = sys.drain_observer_updates(ship_obs).await.unwrap();

    sys.set_movement_mode(ship_a, MovementMode::GotoDirection { direction: DVec3::X }).await.unwrap();
    sys.set_speed_fraction(ship_a, 1.0).await.unwrap();
    let mut align_b = MovementMode::new_warp(
        ship_b_pos,
        bump_origin + DVec3::new(50_000_000.0, 0.0, 0.0),
        3.0,
    );
    if let MovementMode::Warp { ref mut elapsed_warp_time, .. } = align_b {
        *elapsed_warp_time = 6.0;
    }
    sys.set_movement_mode(ship_b, align_b).await.unwrap();
    sys.set_speed_fraction(ship_b, 1.0).await.unwrap();
    let (_, pre_a) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let (_, pre_b) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
    assert_ne!(pre_a.mass, pre_b.mass, "Two different SDE hulls must have distinct masses");

    let tick_b = sys.step_manual(1.0).await.unwrap();
    let (_, post_a) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let (_, post_b) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
    assert!(post_a.position.distance(post_b.position) >= post_a.radius + post_b.radius - 1e-3);
    let tau_a = (pre_a.inertia * pre_a.mass / 1_000_000.0).max(0.01);
    let tau_b = (pre_b.inertia * pre_b.mass / 1_000_000.0).max(0.01);
    let v_a_pre = pre_a.max_velocity + (pre_a.velocity.x - pre_a.max_velocity) * (-1.0 / tau_a).exp();
    let v_b_pre = pre_b.max_velocity * (1.0 - (-1.0 / tau_b).exp());
    let impulse = 1.8 * (v_a_pre - v_b_pre) / ((1.0 / pre_a.mass) + (1.0 / pre_b.mass));
    assert!((post_a.velocity.x - (v_a_pre - impulse / pre_a.mass)).abs() < 1e-3);
    assert!((post_b.velocity.x - (v_b_pre + impulse / pre_b.mass)).abs() < 1e-3);
    let obs_evs = tick_b.observer_events.get(&ship_obs).cloned().unwrap_or_default();
    assert!(
        tick_b.events.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_b))
            && tick_b.events.iter().any(|e| matches!(e, DestinyEvent::SetBallVelocity { ball_id, .. } if *ball_id == ship_b))
            && obs_evs.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_b))
            && obs_evs.iter().any(|e| matches!(e, DestinyEvent::SetBallVelocity { ball_id, .. } if *ball_id == ship_b))
    );
    if let MovementMode::Warp { phase, elapsed_warp_time, .. } = post_b.mode {
        assert_eq!(phase, WarpPhase::Aligning);
        assert_eq!(elapsed_warp_time, 0.0);
    } else {
        panic!("Expected ship B to remain in Aligning warp phase after bump");
    }

    // 1b. Ship-vs-Station, Ship-vs-Upwell Structure, Ship-vs-Stargate, and Ship-vs-Planet static hull collision bumping
    let station_pos = ctx.sde.stations[&JITA_STATION].position;
    let (_, sta_ball) = sys.get_ball_snapshot(JITA_STATION).await.unwrap().unwrap();
    let struct_id: i64 = 1_040_000_001;
    let struct_pos = [90_000_000_000.0, 0.0, 0.0];
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id, type_id: 35832, owner_corp_id: 1_000_125, solar_system_id: jita_sys,
                name: "Test Astrahus".into(), position: struct_pos, state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 500, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    eve_server::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(&ctx, jita_sys, struct_id).await;
    let (_, struct_ball) = sys.get_ball_snapshot(struct_id).await.unwrap().unwrap();
    let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == jita_sys).expect("Jita gate");
    let (_, gate_ball) = sys.get_ball_snapshot(gate.stargate_id).await.unwrap().unwrap();
    let planet = ctx.sde.celestials_in_system(jita_sys).into_iter().find(|c| c.group_id == 7 && c.radius > 0.0).expect("Jita planet");
    let moon = ctx.sde.celestials_in_system(jita_sys).into_iter().find(|c| c.group_id == 8 && c.radius > 0.0).expect("Jita moon");
    for (static_id, static_pos, core_rad, label) in [
        (JITA_STATION, station_pos, sta_ball.radius, "NPC station"),
        (struct_id, DVec3::from_array(struct_pos), struct_ball.radius, "Upwell structure"),
        (gate.stargate_id, gate.position, gate_ball.radius, "stargate"),
        (planet.celestial_id, planet.position, planet.radius, "planet"),
        (moon.celestial_id, moon.position, moon.radius, "moon"),
    ] {
        let mut cs_coll = ctx.build_ship_combat_state(char_a, ship_a, 640);
        cs_coll.undock_invulnerable_remaining_ticks = 0;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_a, type_id: 640, position: static_pos + DVec3::new(core_rad - 50.0, 0.0, 0.0),
                velocity: DVec3::new(-500.0, 0.0, 0.0), max_velocity: 500.0, inertia: 0.5,
                mass: 10_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            cs_coll,
            ctx.build_ship_slim_meta(char_a, 640),
        ).await.unwrap();
        let tick_s = sys.step_manual(1.0).await.unwrap();
        let (_, post_s) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        assert!(
            post_s.position.distance(static_pos) >= core_rad + post_s.radius - 1e-3,
            "Ship must not interpenetrate {label} hull"
        );
        assert!(post_s.velocity.x > 0.0, "Ship must bounce off {label} hull");
        assert!(tick_s.events.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a, ball_b } if (*ball_a == ship_a && *ball_b == static_id) || (*ball_a == static_id && *ball_b == ship_a))));
    }
    let (_, post_struct) = sys.get_ball_snapshot(struct_id).await.unwrap().unwrap();
    assert!(post_struct.position.distance(DVec3::from_array(struct_pos)) < 1e-6, "Anchored Upwell structure must remain stationary");

    // 2. Celestial, Stargate, Station, & Upwell Structure Warp Landing Points
    let b_jita = ctx
        .bind_object(&mut sess_a, "beyonce", jita_sys as i64)
        .expect("bind beyonce Jita");
    let mut cs_warp = ctx.build_ship_combat_state(char_a, ship_a, 640);
    cs_warp.capacitor = 100_000.0;
    cs_warp.max_capacitor = 100_000.0;
    sys.set_combat_state(ship_a, cs_warp).await.unwrap();

    // 2a. Planet (group 7) at 0m -> anchor_pos; at 50km -> 50,000 m; <150km effective -> WarpDistanceTooShort; NaN/Inf -> InvalidArgument
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: planet.position - DVec3::new(planet.radius + 500_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    for bad_range in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let nan_err = ctx.dispatch_rpc(
            &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(bad_range)], &kw, None,
        ).await.unwrap_err();
        assert_eq!(nan_err.code, "InvalidArgument", "Non-finite warp range {bad_range} must be rejected");
    }
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to planet at 0m");
    let (_, w_planet) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_planet, .. } = w_planet.mode else { panic!("Expected Warp"); };
    assert!((dest_planet.distance(planet.position) - (planet.radius + 2_500.0)).abs() < 10.0);

    sys.set_movement_mode(ship_a, MovementMode::Stop).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(50_000.0)], &kw, None,
    ).await.expect("Warp to planet at 50km");
    let (_, w_planet_50) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_planet_50, .. } = w_planet_50.mode else { panic!("Expected Warp"); };
    assert!((dest_planet_50.distance(planet.position) - (planet.radius + 2_500.0 + 50_000.0)).abs() < 10.0);

    // 130 km from planet warp-in point (< 150 km minimum warp distance) -> WarpDistanceTooShort!
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: planet.position - DVec3::new(planet.radius + 2_500.0 + 130_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    let short_err = ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(planet.celestial_id), PyRep::Real(50_000.0)], &kw, None,
    ).await.unwrap_err();
    assert_eq!(short_err.code, "WarpDistanceTooShort");

    // 2b. Star (group 6) -> radius + 100,000 m standoff
    let star = ctx
        .sde
        .celestials_in_system(jita_sys)
        .into_iter()
        .find(|c| c.group_id == 6 && c.radius > 0.0)
        .expect("Jita star");
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: star.position - DVec3::new(star.radius + 2_000_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(star.celestial_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to star at 0m");
    let (_, w_star) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_star, .. } = w_star.mode else { panic!("Expected Warp"); };
    assert!((dest_star.distance(star.position) - (star.radius + 100_000.0)).abs() < 10.0);

    // 2c. Stargate (`sde.jumps`) -> gate_radius standoff (0m from gate perimeter)
    let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == jita_sys).expect("Jita gate");
    let gate_radius = ctx.sde.types.get(&16).map(|t| t.radius).unwrap_or(14_051.0);
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: gate.position - DVec3::new(10_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(gate.stargate_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to stargate at 0m");
    let (_, w_gate) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_gate, .. } = w_gate.mode else { panic!("Expected Warp"); };
    assert!((dest_gate.distance(gate.position) - gate_radius).abs() < 10.0);

    // 2d. NPC Station & Upwell Structure -> 2,000 m docking ring perimeter (>0m from center, inside <=2,500m CmdDock perimeter)
    let station_pos = ctx.sde.stations[&JITA_STATION].position;
    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: station_pos - DVec3::new(10_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(JITA_STATION), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to NPC station at 0m");
    let (_, w_sta) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_sta, .. } = w_sta.mode else { panic!("Expected Warp"); };
    assert!((dest_sta.distance(station_pos) - sta_ball.radius).abs() < 10.0, "NPC station landing must be on Zero Point Envelope (radius)");

    sys.spawn_ball(Ball {
        id: ship_a, type_id: 640, position: DVec3::from_array(struct_pos) - DVec3::new(10_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 0.1, mass: 1_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_a, "beyonce", Some(b_jita), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(struct_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Warp to Upwell structure at 0m");
    let (_, w_struct) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    let MovementMode::Warp { destination: dest_struct, .. } = w_struct.mode else { panic!("Expected Warp"); };
    assert!((dest_struct.distance(DVec3::from_array(struct_pos)) - struct_ball.radius).abs() < 10.0, "Structure landing must be on Zero Point Envelope (radius)");

    // 3. Cloak Velocity Penalty & Restoration (`feat:navigation.cloak-velocity-penalty`)
    for (cloak_tid, expected_mult) in [(11_370_u32, 0.10_f64), (11_577, 0.25), (11_578, 1.00)] {
        let hull_tid: u32 = if cloak_tid == 11_578 { 11_192 } else { 640 };
        let cloak_mod_id: i64 = 74_100 + cloak_tid as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|_, i| i.location_id != ship_a);
            if let Some(s) = snap.items.get_mut(&ship_a) {
                s.type_id = hull_tid;
            }
            snap.module_online_states.clear();
            snap.module_active_states.clear();
        });
        give_item(&ctx, cloak_mod_id, cloak_tid, char_a, ship_a, InventoryFlag::HiSlot0, 1);
        ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(cloak_mod_id, true); });
        ctx.invalidate_ship_fitting(ship_a);

        let pre_all_info = rpc(&ctx, &mut sess_a, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
            .await.unwrap();
        let base_vel = attr_from_all_info(&pre_all_info, "shipInfo", 37);
        let mut clean_cs = ctx.build_ship_combat_state(char_a, ship_a, hull_tid);
        clean_cs.capacitor = 100_000.0;
        clean_cs.max_capacitor = 100_000.0;
        let _ = sys.remove_ball(ship_a).await;
        sys.spawn_ball_with_meta(
            Ball {
                id: ship_a, type_id: hull_tid, position: DVec3::new(95_000_000_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO, max_velocity: base_vel, inertia: 1.0, mass: 10_000_000.0,
                radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            clean_cs,
            ctx.build_ship_slim_meta(char_a, hull_tid),
        ).await.unwrap();

        rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![PyRep::Integer(cloak_mod_id)])
            .await.expect("Activate cloaking device");
        let (_, cloaked_ball) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        assert!((cloaked_ball.max_velocity - base_vel * expected_mult).abs() < 1e-3);
        let all_info = rpc(&ctx, &mut sess_a, "dogmaIM", "GetAllInfo", vec![PyRep::Bool(true), PyRep::Bool(true)])
            .await.unwrap();
        assert!((attr_from_all_info(&all_info, "shipInfo", 37) - base_vel * expected_mult).abs() < 1e-3);

        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(cloak_mod_id)])
            .await.expect("Deactivate cloaking device");
        let (_, decloaked_ball) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
        assert!((decloaked_ball.max_velocity - base_vel).abs() < 1e-3);
    }
}

#[test]
fn test_realistic_player_ship_stargate_and_station_collision_separation() {
    let mut bp = eve_destiny::Ballpark::new(1);
    let (gate_id, station_id, ship_id) = (50_000_001, 60_000_001, 100_000_042);
    bp.add_ball(Ball { id: gate_id, type_id: 16, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1e12, radius: 2_000.0, speed_fraction: 0.0, mode: MovementMode::Stop });
    bp.add_ball(Ball { id: station_id, type_id: 1529, position: DVec3::new(50_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 0.0, mass: 1e12, radius: 5_000.0, speed_fraction: 0.0, mode: MovementMode::Stop });
    bp.add_ball(Ball { id: ship_id, type_id: 587, position: DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop });
    bp.step(1.0);
    assert!(bp.balls[&ship_id].position.x >= 2_049.9, "Ship must separate from stargate even at v_rel == 0: {:?}", bp.balls[&ship_id].position);
    bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(45_500.0, 0.0, 0.0);
    bp.step(1.0);
    assert!(bp.balls[&ship_id].position.x <= 44_950.1, "Ship must separate from NPC station even at v_rel == 0: {:?}", bp.balls[&ship_id].position);
}

}

#[allow(unused_imports, dead_code)]
mod warp_bubble_drag_nullifier_and_mjd_spool {
    use super::*;

// End-to-End Integration Tests closing Interdiction & MJD/Propulsion parity rows:
// `feat:navigation.warp-bubble-drag-and-catch`, `feat:navigation.interdiction-nullification-module`,
// `feat:modules.interdiction-probe-variants`, `feat:navigation.micro-jump-drive-spool`,
// and `feat:navigation.warp-scramble-propulsion-shutoff`.

use crate::modules_advanced_tactical::*;
use eve_db::{FleetRecord, InventoryFlag};
use eve_destiny::{Ball, DestinyEvent, DVec3, MovementMode};
use eve_proto::PyRep;
use eve_server::services::insurgency_services::{
    add_system_corruption, init_insurgency_campaign, FACTION_ANGEL_CARTEL, FORECASTING_DURATION_SECS,
};
use eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect;
use std::collections::HashMap;

#[tokio::test]
async fn test_warp_bubble_drag_catch_nullifier_and_probe_variants() {
    // parity: feat:navigation.warp-bubble-drag-and-catch, feat:navigation.interdiction-nullification-module, feat:modules.interdiction-probe-variants
    eve_testkit::attest(&[
        "feat:navigation.warp-bubble-drag-and-catch",
        "feat:navigation.interdiction-nullification-module",
        "feat:modules.interdiction-probe-variants",
    ]);

    let ctx = build_ctx();
    let kw = HashMap::new();
    let null_sys: u32 = 30_004_608; // 6VDT-H (-0.70 null-sec)
    let sabre_char: i64 = 94_021;
    let sabre_id: i64 = 84_021;
    let target_char: i64 = 94_022;
    let target_ship_id: i64 = 84_022;
    let shuttle_char: i64 = 94_023;
    let shuttle_id: i64 = 84_023;

    let mut sabre_sess = seed_pilot(&ctx, sabre_char, sabre_id, 22_456, null_sys, false);
    let mut target_sess = seed_pilot(&ctx, target_char, target_ship_id, 12_729, null_sys, false);
    let mut shuttle_sess = seed_pilot(&ctx, shuttle_char, shuttle_id, 672, null_sys, false);
    let sys = ctx.systems.get_or_spawn(null_sys);

    // 1. Warp Bubble Catch & Drag + Shuttle Immunity (`feat:navigation.warp-bubble-drag-and-catch`)
    let origin = DVec3::new(50_010_000_000.0, 0.0, 0.0);
    let dest = DVec3::new(50_030_000_000.0, 0.0, 0.0);
    let dest_beacon_id: i64 = 88_701;
    sys.spawn_ball(Ball {
        id: dest_beacon_id, type_id: 15, position: dest, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1e12, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await.unwrap();

    // 1a. Mid-path bubble at 20,000 km on a 30,000 km warp (10,000 km from destination) must NOT catch
    let mid_bubble_id: i64 = 88_702;
    sys.add_warp_bubble(mid_bubble_id, DVec3::new(50_020_000_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
    spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
    spawn_in_space(&ctx, shuttle_char, shuttle_id, 672, null_sys, origin).await;
    // Step 1 tick first to prove reconcile_tackle_and_bubbles preserves Shuttle passive SDE 1538 immunity!
    sys.step_manual(1.0).await.unwrap();

    let b_target = ctx.bind_object(&mut target_sess, "beyonce", null_sys as i64).unwrap();
    let b_shuttle = ctx.bind_object(&mut shuttle_sess, "beyonce", null_sys as i64).unwrap();

    ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Crane warp past mid-path bubble");
    let (_, crane_mid) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: crane_mid_dest, .. } = crane_mid.mode else {
        panic!("Expected Crane in Warp mode");
    };
    assert!((crane_mid_dest.x - 50_030_000_000.0).abs() < 10.0, "mid-path bubble must not catch; got {}", crane_mid_dest.x);

    // 1b. Exit-corridor catch bubble within 150 km in front of destination (at 29,900 km, 20 km radius -> catches at 29,880 km)
    let catch_bubble_id: i64 = 88_704;
    sys.add_warp_bubble(catch_bubble_id, DVec3::new(50_029_900_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
    let _ = sys.remove_ball(target_ship_id).await;
    spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
    sys.step_manual(1.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Crane warp into exit-corridor catch bubble");
    for _ in 0..10 { sys.step_manual(1.0).await.unwrap(); }
    let (_, crane_catch) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: crane_catch_dest, .. } = crane_catch.mode else {
        panic!("Expected Crane in Warp mode");
    };
    assert!((crane_catch_dest.x - 50_029_880_000.0).abs() < 10.0, "got {}", crane_catch_dest.x);

    // Shuttle (group 31, SDE attr 1538 = 1.0) ignores catch bubble AND warps out from inside an active bubble
    let origin_bubble_id: i64 = 88_705;
    sys.add_warp_bubble(origin_bubble_id, origin, 20_000.0).await.unwrap();
    spawn_in_space(&ctx, shuttle_char, shuttle_id, 672, null_sys, origin).await;
    ctx.dispatch_rpc(
        &mut shuttle_sess, "beyonce", Some(b_shuttle), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Shuttle warp from inside active bubble");
    let (_, shuttle_catch) = sys.get_ball_snapshot(shuttle_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: shuttle_dest, .. } = shuttle_catch.mode else {
        panic!("Expected Shuttle in Warp mode");
    };
    assert!((shuttle_dest.x - 50_030_000_000.0).abs() < 10.0, "got {}", shuttle_dest.x);
    sys.remove_warp_bubble(origin_bubble_id).await.unwrap();
    sys.remove_warp_bubble(catch_bubble_id).await.unwrap();

    // 1b. Drag bubble 100 km BEHIND destination at 30,100 km (20 km radius -> drags Crane to 30,080 km!)
    let drag_bubble_id: i64 = 88_703;
    sys.add_warp_bubble(drag_bubble_id, DVec3::new(50_030_100_000.0, 0.0, 0.0), 20_000.0).await.unwrap();
    let _ = sys.remove_ball(target_ship_id).await;
    spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, origin).await;
    sys.step_manual(1.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(dest_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Crane warp into drag bubble");
    for _ in 0..10 { sys.step_manual(1.0).await.unwrap(); }
    let (_, crane_drag) = sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap();
    let MovementMode::Warp { destination: crane_drag_dest, .. } = crane_drag.mode else {
        panic!("Expected Crane in Warp mode for drag");
    };
    assert!((crane_drag_dest.x - 50_030_080_000.0).abs() < 10.0, "got {}", crane_drag_dest.x);

    // 1c. Capital cyno jump blocked inside bubble, while disconnect emergency logoff warp succeeds!
    let near_null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.solar_system_id != null_sys
                && s.security_status < 0.0
                && s.solar_system_id < 31_000_000
                && {
                    let pa = ctx.sde.solar_systems[&null_sys].galactic_pos_ly;
                    let pb = s.galactic_pos_ly;
                    let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                    (0.1..5.0).contains(&d)
                }
        })
        .expect("nearby null-sec cyno destination")
        .solar_system_id;
    let cyno_char: i64 = 94_026;
    let cyno_ship: i64 = 84_026;
    let dread_char: i64 = 94_027;
    let dread_ship: i64 = 84_027;
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11_957, near_null_sys, false); // Falcon
    let mut dread_sess = seed_pilot(&ctx, dread_char, dread_ship, 19_720, null_sys, false); // Revelation
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            77_701,
            FleetRecord { fleet_id: 77_701, boss_id: cyno_char, members: vec![cyno_char, dread_char], ..Default::default() },
        );
    });
    spawn_in_space(&ctx, cyno_char, cyno_ship, 11_957, near_null_sys, DVec3::ZERO).await;
    give_item(&ctx, 74_190, 21_096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 74_191, 16_273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(74_190, true); });
    ctx.invalidate_ship_fitting(cyno_ship);
    let cyno_rep = rpc(&ctx, &mut cyno_sess, "dogmaIM", "Activate", vec![
        PyRep::Integer(74_190), PyRep::String("cynosuralGeneration".into()),
    ]).await.expect("Light cyno in near_null_sys");
    let cyno_beacon_id = cyno_rep.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    spawn_in_space(&ctx, dread_char, dread_ship, 19_720, null_sys, DVec3::new(50_030_100_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 74_192, 16_274, dread_char, dread_ship, InventoryFlag::Cargo, 50_000); // Helium Isotopes
    let jump_blocked = rpc(&ctx, &mut dread_sess, "beyonce", "CmdJumpToFleetModuleBeacon", vec![
        PyRep::Integer(cyno_char), PyRep::Integer(near_null_sys as i64), PyRep::Integer(cyno_beacon_id),
    ]).await.unwrap_err();
    assert_eq!(jump_blocked.code, "WarpScrambled", "Warp disruption bubble must block capital cyno jump");

    // Disconnect emergency logoff warp inside bubble is NOT blocked when ship has no point scramble
    let disc_entry = handle_character_logoff_or_disconnect(&ctx, dread_char)
        .await
        .expect("Disconnect emergency warp entry");
    assert!(disc_entry.emergency_warp_dest.is_some(), "Bubble alone must NOT prevent disconnect emergency logoff warp");

    // 2. Interdiction Nullification Module (`feat:navigation.interdiction-nullification-module`)
    let mut cruiser_sess = seed_pilot(&ctx, 94_028, 84_028, 621, 30_000_142, true); // Caracal (T1 Cruiser)
    assert!(fit_mod(&ctx, &mut cruiser_sess, 84_028, 74_200, 58_919, 27).await.is_err(), "T1 Cruiser must not fit Interdiction Nullifier I");
    let mut bs_sess = seed_pilot(&ctx, 94_024, 84_024, 640, 30_000_142, true);
    assert!(fit_mod(&ctx, &mut bs_sess, 84_024, 74_201, 58_919, 27).await.is_err(), "Battleship must not fit Interdiction Nullifier I");

    let mut crane_fit_sess = seed_pilot(&ctx, target_char, target_ship_id, 12_729, 30_000_142, true);
    let pre_null_info = rpc(&ctx, &mut crane_fit_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(target_ship_id)]).await.unwrap();
    let pre_target_range = attr_from_item_info(&pre_null_info, 76);
    let pre_scan_res = attr_from_item_info(&pre_null_info, 564);

    fit_mod(&ctx, &mut crane_fit_sess, target_ship_id, 74_202, 58_919, 27)
        .await.expect("Crane fits 1st Interdiction Nullifier I");
    assert!(fit_mod(&ctx, &mut crane_fit_sess, target_ship_id, 74_203, 58_919, 28).await.is_err(), "maxGroupFitted=1 must reject 2nd Nullifier");
    let post_null_info = rpc(&ctx, &mut crane_fit_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(target_ship_id)]).await.unwrap();
    assert!(attr_from_item_info(&post_null_info, 76) < pre_target_range);
    assert!(attr_from_item_info(&post_null_info, 564) < pre_scan_res);

    let mut nereus_sess = seed_pilot(&ctx, 94_025, 84_025, 650, 30_000_142, true);
    let pre_ner_info = rpc(&ctx, &mut nereus_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(84_025)]).await.unwrap();
    let pre_drone_bw = attr_from_item_info(&pre_ner_info, 1271);
    fit_mod(&ctx, &mut nereus_sess, 84_025, 74_204, 58_919, 27).await.expect("Nereus fits Interdiction Nullifier I");
    let post_ner_info = rpc(&ctx, &mut nereus_sess, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(84_025)]).await.unwrap();
    assert!((attr_from_item_info(&post_ner_info, 1271) - pre_drone_bw * 0.5).abs() < 1e-3);

    let far_beacon_id: i64 = 88_704;
    sys.spawn_ball(Ball {
        id: far_beacon_id, type_id: 15, position: DVec3::new(50_050_000_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1e12, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await.unwrap();
    target_sess.station_id = None;
    target_sess.solar_system_id = Some(null_sys);
    spawn_in_space(&ctx, target_char, target_ship_id, 12_729, null_sys, DVec3::new(50_030_100_000.0, 0.0, 0.0)).await;
    sys.set_ball_position(target_ship_id, DVec3::new(50_030_100_000.0, 0.0, 0.0)).await;
    let blocked_err = ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(far_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap_err();
    assert_eq!(blocked_err.code, "WarpScrambled");

    rpc(&ctx, &mut target_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74_202)])
        .await.expect("Activate Interdiction Nullifier I");
    ctx.dispatch_rpc(
        &mut target_sess, "beyonce", Some(b_target), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(far_beacon_id), PyRep::Real(0.0)], &kw, None,
    ).await.expect("Active Interdiction Nullifier allows warp out of bubble");

    rpc(&ctx, &mut target_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(74_202)]).await.unwrap();
    let react_err = rpc(&ctx, &mut target_sess, "dogmaIM", "Activate", vec![PyRep::Integer(74_202)]).await.unwrap_err();
    assert_eq!(react_err.code, "ModuleReactivationDelay");
    sys.remove_warp_bubble(drag_bubble_id).await.unwrap();

    // 3. Interdiction Sphere Probe Variants, Security/Corruption Gates, Capacity/Reload, & Wubble (`feat:modules.interdiction-probe-variants`)
    // 3a. Max-1 Interdiction Sphere Launcher I (`22782`, `maxGroupFitted = 1`)
    let mut sabre_fit_sess = seed_pilot(&ctx, sabre_char, sabre_id, 22_456, 30_000_142, true);
    let launcher_id: i64 = 74_301;
    fit_mod(&ctx, &mut sabre_fit_sess, sabre_id, launcher_id, 22_782, 27).await.expect("Sabre fits 1st Interdiction Sphere Launcher I");
    assert!(fit_mod(&ctx, &mut sabre_fit_sess, sabre_id, 74_399, 22_782, 28).await.is_err(), "maxGroupFitted=1 blocks 2nd launcher");

    // 3b. High-sec refusal (`30_000_142`) and Low-sec (`30_002_537` Amamake) Corruption Stage 0 vs Stage 5
    sabre_sess.station_id = None;
    sabre_sess.solar_system_id = Some(30_000_142);
    spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, 30_000_142, DVec3::new(10_000_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 74_302, 22_778, sabre_char, sabre_id, InventoryFlag::HiSlot0, 3);
    ctx.invalidate_ship_fitting(sabre_id);
    let hs_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(hs_err.code, "CannotLaunchInterdictionProbeInEmpire");

    let low_sys: u32 = 30_002_537; // Amamake (0.4 low-sec)
    sabre_sess.solar_system_id = Some(low_sys);
    spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, low_sys, DVec3::new(10_000_000.0, 0.0, 0.0)).await;
    let ls_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(ls_err.code, "CannotLaunchInterdictionProbeInEmpire");

    init_insurgency_campaign(&ctx, 501, FACTION_ANGEL_CARTEL, 500_002, low_sys, vec![low_sys]);
    ctx.advance_secs(FORECASTING_DURATION_SECS + 10);
    let (_, corr_stage) = add_system_corruption(&ctx, 501, low_sys, 1_000);
    assert_eq!(corr_stage, 5);
    let ls_stage5 = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Low-sec Corruption Stage 5 allows interdiction probe launch");
    assert_eq!(ls_stage5.dict_get("bubbleRadius"), Some(&PyRep::Real(20_000.0)));
    assert_eq!(ls_stage5.dict_get("lifetimeSeconds"), Some(&PyRep::Real(120.0)));
    ctx.advance_secs(6);

    // 3c. Move to null-sec (`null_sys`), test 15 m3 (3-probe) capacity + 60s auto-reload when last loaded probe is fired!
    sabre_sess.solar_system_id = Some(null_sys);
    let sabre_pos = DVec3::new(50_050_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, null_sys, sabre_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.items.get_mut(&74_302) {
            st.quantity = 1; // 1 probe left in launcher
        }
    });
    give_item(&ctx, 74_310, 22_778, sabre_char, sabre_id, InventoryFlag::Cargo, 4); // 4 in cargo -> 3 (15 m3) auto-reloaded
    let smartbomb_id: i64 = 74_303;
    let smartbomb_id_2: i64 = 74_306;
    let ab_sabre_id: i64 = 74_307;
    give_item(&ctx, smartbomb_id, 14_200, sabre_char, sabre_id, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, smartbomb_id_2, 14_200, sabre_char, sabre_id, InventoryFlag::HiSlot2, 1);
    give_item(&ctx, ab_sabre_id, 439, sabre_char, sabre_id, InventoryFlag::MedSlot0, 1); // 1MN Afterburner I (439)
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[launcher_id, smartbomb_id, smartbomb_id_2, ab_sabre_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(sabre_id);

    let p1 = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch last loaded Warp Disrupt Probe -> triggers 60s auto-reload");
    let p1_id = p1.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(p1.dict_get("lifetimeSeconds"), Some(&PyRep::Real(120.0)));
    assert_eq!(p1.dict_get("reloading"), Some(&PyRep::Bool(true)));
    assert_eq!(p1.dict_get("remainingInLauncher"), Some(&PyRep::Integer(3)));
    assert_eq!(ctx.crimewatch.read().unwrap().states.get(&sabre_char).map(|s| s.weapon_timer_until_unix).unwrap_or(0), 0);

    // At +10s (> 5s cadence, < 60s reload): blocked with ModuleIsReloading!
    ctx.advance_secs(10);
    let reload_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(reload_err.code, "ModuleIsReloading");

    // Smartbomb AoE destroys the 1,000 HP untargetable probe and removes its bubble
    assert_eq!(rpc(&ctx, &mut sabre_sess, "dogmaIM", "AddTarget", vec![PyRep::Integer(p1_id)]).await.unwrap_err().code, "InvalidTarget");
    assert!(sys.is_ball_scrambled(sabre_id).await.unwrap());
    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![
        PyRep::Integer(smartbomb_id), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1),
    ]).await.unwrap();
    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![
        PyRep::Integer(smartbomb_id_2), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..70 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    let _ = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(smartbomb_id)]).await;
    let _ = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(smartbomb_id_2)]).await;
    assert!(sys.get_ball_snapshot(p1_id).await.unwrap().is_none(), "Smartbomb AoE must destroy probe");
    assert!(!sys.is_ball_scrambled(sabre_id).await.unwrap(), "Destroyed probe must remove bubble");

    // After 60s reload completed, 3 probes (15 m3 / 5 m3) were auto-loaded from the 4-probe cargo stack (1 left in cargo)!
    let p_reloaded = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch after 60s auto-reload");
    assert!(p_reloaded.dict_get("probeID").is_some());
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&74_310).map(|i| i.quantity)), Some(1), "15m3 launcher reloads max 3 of 4 probes");
    let cadence_err = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(cadence_err.code, "ModuleReactivationDelay");
    ctx.advance_secs(6);
    // Move away from p_reloaded bubble before testing Wubble
    let wub_pos = DVec3::new(50_055_000_000.0, 0.0, 0.0);
    sys.set_ball_position(sabre_id, wub_pos).await;

    // 3d. Stasis Webification Probe (`56748`: SDE 15km, 30s lifetime, 2.0s arming delay, -20% speedFactor = 0.80x, composes with active AB!)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&smartbomb_id);
        snap.items.remove(&smartbomb_id_2);
        snap.items.retain(|_, i| !(i.location_id == sabre_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != launcher_id));
        snap.module_online_states.insert(ab_sabre_id, true);
    });
    give_item(&ctx, 74_304, 56_748, sabre_char, sabre_id, InventoryFlag::HiSlot0, 5);
    ctx.invalidate_ship_fitting(sabre_id);
    sys.set_capacitor(sabre_id, 5_000.0).await.unwrap();

    // Activate 1MN Afterburner II on Sabre BEFORE launching Wubble
    rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(ab_sabre_id)])
        .await.expect("Activate 1MN Afterburner II on Sabre");
    let (_, sabre_ab_ball) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    let ab_vel = sabre_ab_ball.max_velocity;
    assert!(ab_vel > 700.0, "Active Afterburner must boost Sabre max_velocity: got {ab_vel}");

    let pw = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch Stasis Webification Probe 56748");
    let pw_id = pw.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(pw.dict_get("isWubble"), Some(&PyRep::Bool(true)));
    assert_eq!(pw.dict_get("bubbleRadius"), Some(&PyRep::Real(15_000.0)));
    assert_eq!(pw.dict_get("lifetimeSeconds"), Some(&PyRep::Real(30.0)));

    // At t = 1.0s (< 2.0s SDE arming delay attr 2262): probe is on grid and ship is NOT webbed yet!
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_some(), "Arming wubble probe must remain on grid before detonation");
    let (_, sabre_t1) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!((sabre_t1.max_velocity - ab_vel).abs() < 1e-2, "Wubble must not web during 2.0s arming delay");

    // At t = 2.0s (detonated): probe ball is immediately removed from grid; 0.80x debuff persists 30s without warp scramble!
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_none(), "Detonated wubble probe must be removed from grid immediately");
    let (_, sabre_wub) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!((sabre_wub.max_velocity - ab_vel * 0.80).abs() < 1e-2, "Wubble must apply SDE 0.80x to AB velocity: got {} vs {}", sabre_wub.max_velocity, ab_vel * 0.80);
    assert!(!sys.is_ball_scrambled(sabre_id).await.unwrap());

    // Keep capacitor topped up and step 31s past 30s Wubble debuff lifetime -> restores FULL AB-boosted velocity!
    for _ in 0..31 {
        sys.set_capacitor(sabre_id, 5_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    let (_, sabre_post_wub) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!((sabre_post_wub.max_velocity - ab_vel).abs() < 1e-2, "Wubble expiry must restore active AB velocity ({ab_vel}), got {}", sabre_post_wub.max_velocity);

    // 3e. Surgical Warp Disrupt Probe (34260: 10km radius, 180s lifetime)
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&74_304); });
    give_item(&ctx, 74_305, 34_260, sabre_char, sabre_id, InventoryFlag::HiSlot0, 2);
    ctx.invalidate_ship_fitting(sabre_id);
    let ps = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await.expect("Launch Surgical Warp Disrupt Probe 34260");
    assert_eq!(ps.dict_get("bubbleRadius"), Some(&PyRep::Real(10_000.0)));
    assert_eq!(ps.dict_get("lifetimeSeconds"), Some(&PyRep::Real(180.0)));
}

#[tokio::test]
async fn test_mjd_spool_and_warp_scramble_propulsion_shutoff() {
    // parity: feat:navigation.micro-jump-drive-spool, feat:navigation.warp-scramble-propulsion-shutoff
    eve_testkit::attest(&[
        "feat:navigation.micro-jump-drive-spool",
        "feat:navigation.warp-scramble-propulsion-shutoff",
    ]);

    let ctx = build_ctx();
    let kw = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let char_mjd: i64 = 94_031;
    let ship_mjd: i64 = 84_031;
    let char_scram: i64 = 94_032;
    let ship_scram: i64 = 84_032;

    let mut sess_mjd = seed_pilot(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, false);
    let mut sess_scram = seed_pilot(&ctx, char_scram, ship_scram, 640, jita_sys, false);
    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Micro Jump Drive Spool-up, Turn During Spool, Velocity/Heading Preservation, & SetBallPosition Event
    set_skill_level(&ctx, char_mjd, 4_385, 0);
    let med_mjd_id: i64 = 75_001;
    give_item(&ctx, med_mjd_id, 33_915, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(med_mjd_id, true); });
    ctx.invalidate_ship_fitting(ship_mjd);

    let mjd_origin = DVec3::new(50_010_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, mjd_origin).await;
    let b_mjd = ctx.bind_object(&mut sess_mjd, "beyonce", jita_sys as i64).unwrap();
    let init_cs = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
    let base_sig = init_cs.signature_radius;

    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)], &kw, None,
    ).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_id)])
        .await.expect("Activate Medium MJD");

    for _ in 0..6 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, mid_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    let mid_cs = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
    assert!(mid_ball.position.distance(mjd_origin) < 5_000.0);
    assert!((mid_cs.signature_radius - base_sig * 2.5).abs() < 1.0);

    // Voluntary steering RPCs must be rejected mid-spool, while involuntary collision-bump deflection still steers the jump!
    let bumper_id: i64 = 84_099;
    let mut bumper_cs = eve_destiny::CombatState::new();
    bumper_cs.max_capacitor = 10_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: bumper_id, type_id: 24690, position: mid_ball.position - DVec3::new(0.0, 200.0, 0.0),
            velocity: DVec3::new(0.0, 150.0, 0.0), max_velocity: 150.0, inertia: 1.0, mass: 500_000_000.0,
            radius: 250.0, speed_fraction: 1.0, mode: MovementMode::GotoDirection { direction: DVec3::Y },
        },
        bumper_cs,
        ctx.build_ship_slim_meta(char_scram, 24690),
    ).await.unwrap();
    for (m_name, m_args) in [
        ("CmdGotoDirection", vec![PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)]),
        ("CmdSteerDirection", vec![PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)]),
        ("CmdGotoPoint", vec![PyRep::Real(0.0), PyRep::Real(10_000.0), PyRep::Real(0.0)]),
        ("CmdAlignTo", vec![PyRep::Integer(bumper_id)]),
        ("CmdOrbit", vec![PyRep::Integer(bumper_id), PyRep::Real(2_500.0)]),
        ("CmdFollowBall", vec![PyRep::Integer(bumper_id), PyRep::Real(0.0)]),
    ] {
        let expected_err = "CantMoveWhileModuleActive";
        let steer_err = ctx.dispatch_rpc(&mut sess_mjd, "beyonce", Some(b_mjd), m_name, &m_args, &kw, None).await.unwrap_err();
        assert_eq!(steer_err.code, expected_err, "{m_name} must be rejected during MJD spool");
    }
    let mut saw_set_ball_pos = false;
    for _ in 0..6 {
        let tb = sys.step_manual(1.0).await.unwrap();
        if tb.events.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_mjd)) {
            saw_set_ball_pos = true;
        }
    }
    let _ = sys.remove_ball(bumper_id).await;
    assert!(saw_set_ball_pos, "Same-grid 100 km MJD jump must emit DestinyEvent::SetBallPosition");
    let (_, post_mjd_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    let jump_vec = post_mjd_ball.position - mid_ball.position;
    assert!((jump_vec.length() - 100_000.0).abs() < 3_500.0, "MJD must jump 100 km: got {}", jump_vec.length());
    assert!(post_mjd_ball.position.y > 50.0, "Bump-induced deflection into +Y must steer MJD jump into +Y: got {:?}", post_mjd_ball.position);
    assert!(jump_vec.normalize().dot(post_mjd_ball.velocity.normalize()) > 0.98, "MJD jump must align with current ship velocity heading");

    let mjd_react_err = rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_id)]).await.unwrap_err();
    assert_eq!(mjd_react_err.code, "ModuleReactivationDelay");

    // Micro Jump Drive Operation L5 reduces spool time by 25% (12s -> 9s)
    let char_mjd_l5: i64 = 94_033;
    let ship_mjd_l5: i64 = 84_033;
    let med_mjd_l5_id: i64 = 75_002;
    let l5_origin = DVec3::new(50_020_000_000.0, 0.0, 0.0);
    let mut sess_mjd_l5 = seed_pilot(&ctx, char_mjd_l5, ship_mjd_l5, 16_231, jita_sys, false);
    set_skill_level(&ctx, char_mjd_l5, 4_385, 5);
    give_item(&ctx, med_mjd_l5_id, 33_915, char_mjd_l5, ship_mjd_l5, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(med_mjd_l5_id, true); });
    ctx.invalidate_ship_fitting(ship_mjd_l5);
    spawn_in_space(&ctx, char_mjd_l5, ship_mjd_l5, 16_231, jita_sys, l5_origin).await;
    let b_mjd_l5 = ctx.bind_object(&mut sess_mjd_l5, "beyonce", jita_sys as i64).unwrap();
    ctx.dispatch_rpc(
        &mut sess_mjd_l5, "beyonce", Some(b_mjd_l5), "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    rpc(&ctx, &mut sess_mjd_l5, "dogmaIM", "Activate", vec![PyRep::Integer(med_mjd_l5_id)]).await.unwrap();
    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, pre9_ball) = sys.get_ball_snapshot(ship_mjd_l5).await.unwrap().unwrap();
    assert!(pre9_ball.position.distance(l5_origin) < 5_000.0);
    sys.step_manual(1.0).await.unwrap();
    let (_, l5_ball) = sys.get_ball_snapshot(ship_mjd_l5).await.unwrap().unwrap();
    assert!((l5_ball.position.x - (l5_origin.x + 100_000.0)).abs() < 5_000.0);

    // Capital Micro Jump Drive (83465) on Archon (23757) jumps 250 km
    let cap_mjd_id: i64 = 75_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_mjd);
        snap.items.get_mut(&ship_mjd).unwrap().type_id = 23_757;
    });
    give_item(&ctx, cap_mjd_id, 83_465, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(cap_mjd_id, true); });
    ctx.invalidate_ship_fitting(ship_mjd);
    let cap_origin = DVec3::new(50_030_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_mjd, ship_mjd, 23_757, jita_sys, cap_origin).await;
    sys.set_ball_position(ship_mjd, cap_origin).await;
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)], &kw, None,
    ).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_mjd, "beyonce", Some(b_mjd), "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None,
    ).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(cap_mjd_id)]).await.unwrap();
    for _ in 0..12 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, cap_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!((cap_ball.position.x - (cap_origin.x + 250_000.0)).abs() < 5_000.0);

    // 2. Warp Scrambler Propulsion Shutoff vs Warp Disruptor, Bubble, WCS, & Afterburner (`feat:navigation.warp-scramble-propulsion-shutoff`)
    let mwd_id: i64 = 75_011;
    let ab_id: i64 = 75_012;
    let mid_spool_mjd_id: i64 = 75_013;
    let wcs_id: i64 = 75_014;
    let disruptor_id: i64 = 75_021;
    let scrambler_id: i64 = 75_022;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_mjd && i.location_id != ship_scram);
        snap.items.get_mut(&ship_mjd).unwrap().type_id = 16_231;
        snap.items.get_mut(&ship_scram).unwrap().type_id = 640;
    });
    give_item(&ctx, mwd_id, 5_975, char_mjd, ship_mjd, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, ab_id, 12_058, char_mjd, ship_mjd, InventoryFlag::MedSlot1, 1);
    give_item(&ctx, mid_spool_mjd_id, 33_915, char_mjd, ship_mjd, InventoryFlag::MedSlot2, 1);
    give_item(&ctx, wcs_id, 10_998, char_mjd, ship_mjd, InventoryFlag::LoSlot0, 1); // Warp Core Stabilizer I (+2 core strength)
    give_item(&ctx, disruptor_id, 3_242, char_scram, ship_scram, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, scrambler_id, 448, char_scram, ship_scram, InventoryFlag::MedSlot1, 1);
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[mwd_id, ab_id, mid_spool_mjd_id, wcs_id, disruptor_id, scrambler_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_mjd);
    ctx.invalidate_ship_fitting(ship_scram);

    let scram_arena = DVec3::new(50_040_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_mjd, ship_mjd, 16_231, jita_sys, scram_arena).await;
    spawn_in_space(&ctx, char_scram, ship_scram, 640, jita_sys, scram_arena + DVec3::new(3_000.0, 0.0, 0.0)).await;
    let (_, base_cyclone) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    let base_vel = base_cyclone.max_velocity;

    sys.add_target_lock(ship_scram, eve_destiny::TargetLock {
        target_id: ship_mjd, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)]).await.unwrap();
    let (_, mwd_on_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(mwd_on_ball.max_velocity > base_vel * 4.0);

    // 2a. Warp Disruptor keeps MWD active AND allows MJD activation, 12s spool (skill lvl 0), and 100 km jump
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
        PyRep::Integer(disruptor_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
    ]).await.unwrap();
    assert!(sys.is_ball_scrambled(ship_mjd).await.unwrap());
    let (_, under_disruptor_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(under_disruptor_ball.max_velocity > base_vel * 4.0);
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Deactivate", vec![PyRep::Integer(mwd_id)]).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap();
    for _ in 0..13 {
        sys.set_capacitor(ship_mjd, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, disruptor_jumped_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(
        disruptor_jumped_ball.position.distance(scram_arena) > 95_000.0,
        "MJD must complete 100 km jump under Warp Disruptor: got {:?}",
        disruptor_jumped_ball.position
    );
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Deactivate", vec![PyRep::Integer(disruptor_id)]).await.unwrap();
    ctx.advance_secs(200);

    // 2b. Interdiction bubble allows MJD activation, 12s spool, and 100 km jump out of bubble
    let bubble_pos = scram_arena + DVec3::new(200_000.0, 0.0, 0.0);
    sys.set_ball_position(ship_mjd, bubble_pos).await;
    sys.add_warp_bubble(980_101, bubble_pos, 20_000.0).await.unwrap();
    assert!(sys.is_ball_scrambled(ship_mjd).await.unwrap());
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap();
    for _ in 0..13 {
        sys.set_capacitor(ship_mjd, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, bubble_jumped_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(
        bubble_jumped_ball.position.distance(bubble_pos) > 95_000.0,
        "MJD must complete 100 km jump inside an interdiction bubble: got {:?}",
        bubble_jumped_ball.position
    );
    sys.remove_warp_bubble(980_101).await.unwrap();

    // Reset ship back to scram_arena and verify Warp Scrambler shuts down MWD, preserves AB,
    // and blocks/aborts MJD even when a Warp Core Stabilizer (+2 core strength) is active!
    sys.set_ball_position(ship_mjd, scram_arena).await;
    sys.add_target_lock(ship_scram, eve_destiny::TargetLock {
        target_id: ship_mjd, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)]).await.unwrap();
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
        PyRep::Integer(scrambler_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
    ]).await.unwrap();
    let (_, scrammed_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!((scrammed_ball.max_velocity - base_vel).abs() < 1e-2);
    assert_eq!(rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id)]).await.unwrap_err().code, "WarpScrambled");

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(ship_mjd);
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(ab_id)]).await.unwrap();
    sys.step_manual(2.0).await.unwrap();
    let (_, ab_scram_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(ab_scram_ball.max_velocity > base_vel * 1.8);
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Deactivate", vec![PyRep::Integer(scrambler_id)]).await.unwrap();

    // Activate Warp Core Stabilizer I (+2 core strength -> total 3 > 2 scram points) and verify scrambler STILL blocks & aborts MJD!
    ctx.advance_secs(200);
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(wcs_id)]).await.unwrap();
    rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap();
    sys.step_manual(3.0).await.unwrap();
    assert_eq!(rpc(&ctx, &mut sess_mjd, "dogmaIM", "Deactivate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap_err().code, "CantDeactivateWhileSpooling");
    let mjd_bid = ctx.bind_object(&mut sess_mjd, "beyonce", 30000142).unwrap();
    let empty_kw = std::collections::HashMap::new();
    assert_eq!(ctx.dispatch_rpc(&mut sess_mjd, "beyonce", Some(mjd_bid), "CmdStop", &[], &empty_kw, None).await.unwrap_err().code, "CantMoveWhileModuleActive");
    assert_eq!(ctx.dispatch_rpc(&mut sess_mjd, "beyonce", Some(mjd_bid), "CmdSetSpeedFraction", &[PyRep::Real(0.5)], &empty_kw, None).await.unwrap_err().code, "CantMoveWhileModuleActive");
    assert_eq!(ctx.dispatch_rpc(&mut sess_mjd, "beyonce", Some(mjd_bid), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(50001248)], &empty_kw, None).await.unwrap_err().code, "CantMoveWhileModuleActive");
    rpc(&ctx, &mut sess_scram, "dogmaIM", "Activate", vec![
        PyRep::Integer(scrambler_id), PyRep::String("warpScramble".into()), PyRep::Integer(ship_mjd), PyRep::Integer(1),
    ]).await.unwrap();
    let cs_wcs_scram = sys.get_combat_state(ship_mjd).await.unwrap().unwrap();
    assert!(!cs_wcs_scram.is_warp_scrambled(), "WCS (+2) overcomes 2 scram points for warping");
    assert!(cs_wcs_scram.is_mwd_mjd_disabled, "Scrambler still sets is_mwd_mjd_disabled with WCS active");
    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, aborted_mjd_ball) = sys.get_ball_snapshot(ship_mjd).await.unwrap().unwrap();
    assert!(aborted_mjd_ball.position.distance(scram_arena) < 5_000.0);
    ctx.advance_secs(200);
    assert_eq!(
        rpc(&ctx, &mut sess_mjd, "dogmaIM", "Activate", vec![PyRep::Integer(mid_spool_mjd_id)]).await.unwrap_err().code,
        "WarpScrambled"
    );
}

#[test]
fn test_warp_catch_bubble_350km_in_front_of_destination() {
    let origin = DVec3::ZERO;
    let dest = DVec3::new(10_000_000.0, 0.0, 0.0);
    let bubble = eve_destiny::WarpBubble { id: 99_001, position: DVec3::new(dest.x - 350_000.0, 0.0, 0.0), radius: 20_000.0 };
    let hit = bubble.intercept_warp(origin, dest).expect("Catch bubble 350 km in front of destination must intercept warp");
    assert!((hit.x - (dest.x - 370_000.0)).abs() < 1.0, "Expected entry at dest - 370 km, got {:?}", hit);
    let mid_route = eve_destiny::WarpBubble { id: 99_002, position: DVec3::new(dest.x - 600_000.0, 0.0, 0.0), radius: 20_000.0 };
    assert!(mid_route.intercept_warp(origin, dest).is_none(), "Bubble > 500 km mid-route must not intercept");
    let mut bp = eve_destiny::Ballpark::new(30000142);
    bp.add_ball(eve_destiny::Ball { id: 1, type_id: 587, position: origin, velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 3.45, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop });
    bp.warp_bubble_mgr.add_bubble(bubble);
    bp.set_mode(1, eve_destiny::MovementMode::new_warp(origin, dest, 3.0)).unwrap();
    bp.warp_bubble_mgr.remove_bubble(99_001);
    for _ in 0..20 { bp.step(1.0); }
    if let eve_destiny::MovementMode::Warp { destination, .. } = bp.balls[&1].mode {
        assert!((destination - dest).length() < 1.0, "Despawned bubble during alignment must not alter destination");
    }
}

}

#[allow(unused_imports, dead_code)]
mod overheat_rack_heat_and_nanite_repair {
    use super::*;

// End-to-End Integration Tests closing Overheat Rack Heat and Probabilistic Heat Damage Propagation
// parity rows:
// `feat:modules.overheat-rack-heat-generation` and `feat:modules.overheat-damage-propagation`.

use crate::modules_advanced_tactical::*;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_proto::PyRep;
use std::collections::HashMap;

fn set_ship_rack_heat(ctx: &eve_server::router::ServerContext, ship_id: i64, attr_id: u16, val: f64) {
    ctx.db.with_mut_snapshot(|snap| {
        let tr = snap.transient_item_attributes.entry(ship_id).or_default();
        if val > 0.0 {
            tr.insert(attr_id, val);
        } else {
            tr.remove(&attr_id);
        }
        if tr.is_empty() {
            snap.transient_item_attributes.remove(&ship_id);
        }
    });
}

#[tokio::test]
async fn test_overheat_rack_heat_analytical_curves_and_overload_bonuses() {
    // parity: feat:modules.overheat-rack-heat-generation
    eve_testkit::attest(&["feat:modules.overheat-rack-heat-generation"]);

    let ctx = build_ctx();
    let jita_sys: u32 = 30_000_142;
    let char_a: i64 = 94_041;
    let ship_a: i64 = 84_041;
    let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 24_690, jita_sys, false); // Hyperion (attr 1224 = 0.5)
    let sys = ctx.systems.get_or_spawn(jita_sys);

    let mod_0: i64 = 76_001;
    let mod_1: i64 = 76_002;
    let ab_id: i64 = 76_005;
    let scram_id: i64 = 76_008;
    let rep_id: i64 = 76_009;
    give_item(&ctx, mod_0, 3_170, char_a, ship_a, InventoryFlag::HiSlot0, 1); // Light Ion Blaster II (attr 1180 = 0.02)
    give_item(&ctx, mod_1, 3_170, char_a, ship_a, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, 76_007, 222, char_a, ship_a, InventoryFlag::HiSlot0, 500);
    give_item(&ctx, 76_017, 222, char_a, ship_a, InventoryFlag::HiSlot1, 500);
    give_item(&ctx, ab_id, 12_058, char_a, ship_a, InventoryFlag::MedSlot0, 1); // 10MN AB II (+50% overload speed)
    give_item(&ctx, scram_id, 448, char_a, ship_a, InventoryFlag::MedSlot1, 1); // Warp Scrambler II (+20% overload range: 9km -> 10.8km)
    give_item(&ctx, rep_id, 3_530, char_a, ship_a, InventoryFlag::LoSlot0, 1); // Medium Armor Repairer II (+10% rep, -15% dur)
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[mod_0, mod_1, ab_id, scram_id, rep_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_a, ship_a, 24_690, jita_sys, origin).await;
    let target_id: i64 = 88_801;
    let mut t_cs = CombatState::default();
    t_cs.max_shield_hp = 500_000.0;
    t_cs.shield_hp = 500_000.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: target_id, type_id: 640, position: origin + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e7, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        t_cs,
    ).await.unwrap();
    sys.add_target_lock(ship_a, eve_destiny::TargetLock {
        target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await.unwrap();

    // 1a. Analytical single-module heat curve on 1175 (`heatHi`): H(10) = 1 - exp(-10 * 0.5 * 0.02) = 1 - exp(-0.10)
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.expect("Overload 1 blaster");
    for _ in 0..10 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let h_1mod = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(), 1175);
    let expected_1mod = 1.0 - (-0.10_f64).exp();
    assert!((h_1mod - expected_1mod).abs() < 1e-4, "1-module high rack heat (1175) must follow 1 - exp(-0.10): got {h_1mod} vs {expected_1mod}");
    assert!(
        ctx.db.with_snapshot(|s| {
            let sh = s.items.get(&ship_a).unwrap();
            !sh.dynamic_attributes.contains_key(&1175)
                && !sh.dynamic_attributes.contains_key(&1176)
                && !sh.dynamic_attributes.contains_key(&1177)
        }),
        "Transient ship rack heat (1175..=1177) must never be persisted to ship_item.dynamic_attributes"
    );

    // 1b. Exponential passive dissipation when overload is stopped: H_cool(10) = H_0 * exp(-10 * 0.01)
    rpc(&ctx, &mut sess_a, "dogmaIM", "StopOverload", vec![PyRep::Integer(mod_0)]).await.unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
    for _ in 0..10 {
        sys.step_manual(1.0).await.unwrap();
    }
    let h_cooled = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(), 1175);
    assert!((h_cooled - h_1mod * (-0.10_f64).exp()).abs() < 1e-4, "Passive cooling must follow H * exp(-t * diss): got {h_cooled}");

    // 1c. Linear 2-module sum (NOT N^2 * 2^N): 2 overloaded blasters -> gen_rate = 0.02 -> H(10) = 1 - exp(-0.20)
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.0);
    set_item_dyn_attr(&ctx, mod_0, 3, 0.0);
    set_item_dyn_attr(&ctx, mod_1, 3, 0.0);
    for &mid in &[mod_0, mod_1] {
        rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
            PyRep::Integer(mid), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
        ]).await.unwrap();
    }
    for _ in 0..10 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let h_2mod = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_a)]).await.unwrap(), 1175);
    let expected_2mod = 1.0 - (-0.20_f64).exp();
    assert!((h_2mod - expected_2mod).abs() < 1e-4, "2 overloaded modules must sum linearly to 1 - exp(-0.20): got {h_2mod} vs {expected_2mod}");
    for &mid in &[mod_0, mod_1] {
        rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mid)]).await.unwrap();
    }

    // 1d. Ship heatGenerationMultiplier (attr 1224): Rifter (587, 1224 = 1.0) heats at 2x rate of Hyperion (24690, 1224 = 0.5)
    let char_r: i64 = 94_042;
    let ship_r: i64 = 84_042;
    let mut sess_r = seed_pilot(&ctx, char_r, ship_r, 587, jita_sys, false);
    give_item(&ctx, 76_031, 3_170, char_r, ship_r, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 76_032, 222, char_r, ship_r, InventoryFlag::HiSlot0, 500);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(76_031, true); });
    ctx.invalidate_ship_fitting(ship_r);
    spawn_in_space(&ctx, char_r, ship_r, 587, jita_sys, origin + DVec3::new(0.0, 500.0, 0.0)).await;
    sys.add_target_lock(ship_r, eve_destiny::TargetLock {
        target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await.unwrap();
    rpc(&ctx, &mut sess_r, "dogmaIM", "Overload", vec![
        PyRep::Integer(76_031), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..10 {
        sys.set_capacitor(ship_r, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let h_rifter = attr_from_item_info(&rpc(&ctx, &mut sess_r, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(ship_r)]).await.unwrap(), 1175);
    assert!((h_rifter - expected_2mod).abs() < 1e-4, "Rifter (1224=1.0) with 1 blaster must match 2x Hyperion (1224=0.5) rate: got {h_rifter}");
    rpc(&ctx, &mut sess_r, "dogmaIM", "Deactivate", vec![PyRep::Integer(76_031)]).await.unwrap();

    // 1e. SDE Overload bonuses: AB (+50% speed), Warp Scrambler II (+20% range at 10km), Armor Repairer (+10% rep / -15% dur), Weapon (-15% ROF)
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![PyRep::Integer(ab_id)]).await.unwrap();
    let (_, ab_norm) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![PyRep::Integer(ab_id)]).await.unwrap();
    let (_, ab_ovl) = sys.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    assert!(ab_ovl.max_velocity > ab_norm.max_velocity * 1.15, "Overloaded AB must boost max_velocity: {} vs {}", ab_ovl.max_velocity, ab_norm.max_velocity);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(ab_id)]).await.unwrap();

    sys.set_ball_position(target_id, origin + DVec3::new(10_200.0, 0.0, 0.0)).await;
    let scram_cold_err = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(target_id), PyRep::Integer(0),
    ]).await.unwrap_err();
    assert_eq!(scram_cold_err.code, "TargetOutOfRange", "Unoverloaded Warp Scrambler II (9km) must fail at 10.2km");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(scram_id), PyRep::String("warpScramble".into()), PyRep::Integer(target_id), PyRep::Integer(0),
    ]).await.expect("Overloaded Warp Scrambler II (+20% -> 10.8km) must succeed at 10.2km");
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(scram_id)]).await.unwrap();

    let fit_eval = ctx.get_or_rebuild_ship_fitting(char_a, ship_a).unwrap();
    let rep_norm = fit_eval.build_effective_module_state(rep_id, 3_530, false, &ctx.sde);
    let rep_ovl = fit_eval.build_effective_module_state(rep_id, 3_530, true, &ctx.sde);
    assert!((rep_ovl.get_attribute(84, &ctx.sde) - rep_norm.get_attribute(84, &ctx.sde) * 1.10).abs() < 1e-3);
    assert!((rep_ovl.get_attribute(73, &ctx.sde) - rep_norm.get_attribute(73, &ctx.sde) * 0.85).abs() < 1e-3);
    let gun_norm = fit_eval.build_effective_module_state(mod_0, 3_170, false, &ctx.sde);
    let gun_ovl = fit_eval.build_effective_module_state(mod_0, 3_170, true, &ctx.sde);
    assert!((gun_ovl.get_attribute(64, &ctx.sde) - gun_norm.get_attribute(64, &ctx.sde) * 1.15).abs() < 1e-3);

    // Mid-cycle UpdateModuleOverload on active Medium Armor Repairer II (`3530`):
    // Per EVE Overheating rules (`Overheating.md`), toggling overload mid-cycle takes effect on the NEXT cycle.
    let mut rep_cs = sys.get_combat_state(ship_a).await.unwrap().unwrap();
    rep_cs.max_armor_hp = 50_000.0;
    rep_cs.armor_hp = 1_000.0;
    sys.set_combat_state(ship_a, rep_cs).await.unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(rep_id), PyRep::String("armorRepair".into()), PyRep::None, PyRep::Integer(1),
    ]).await.unwrap();
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![PyRep::Integer(rep_id)]).await.unwrap();
    // Base duration is 12.0s; after 11s, the in-progress cycle must NOT have finished early.
    for _ in 0..11 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let base_rep = rep_norm.get_attribute(84, &ctx.sde);
    let armor_at_11s = sys.get_combat_state(ship_a).await.unwrap().unwrap().armor_hp;
    assert!(
        (armor_at_11s - (1_000.0 + base_rep)).abs() < 1.0,
        "Mid-cycle Overload must complete in-progress cycle at base repair ({}): got {armor_at_11s}",
        1_000.0 + base_rep
    );
    // After +11s (22s total), the next cycle completes at overloaded duration and +10% armor repair!
    for _ in 0..11 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let armor_after_next_ovl = sys.get_combat_state(ship_a).await.unwrap().unwrap().armor_hp;
    let expected_armor = 1_000.0 + base_rep + base_rep * 1.10;
    assert!(
        (armor_after_next_ovl - expected_armor).abs() < 1.0,
        "Next cycle after mid-cycle Overload must apply +10% amount (attr 1230) and -15% duration (attr 1206, 10.2s): got {armor_after_next_ovl} vs {expected_armor}"
    );
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(rep_id)]).await.unwrap();
}

#[tokio::test]
async fn test_probabilistic_heat_damage_propagation_and_nanite_paste() {
    // parity: feat:modules.overheat-damage-propagation
    eve_testkit::attest(&["feat:modules.overheat-damage-propagation"]);

    let ctx = build_ctx();
    let _kw: HashMap<String, PyRep> = HashMap::new();
    let jita_sys: u32 = 30_000_142;
    let char_a: i64 = 94_045;
    let ship_a: i64 = 84_045;
    let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 24_690, jita_sys, false); // Hyperion (19 total hi+med+low slots)
    let sys = ctx.systems.get_or_spawn(jita_sys);

    set_skill_level(&ctx, char_a, 28_164, 0); // Thermodynamics L0
    let mod_0: i64 = 76_101;
    let mod_1: i64 = 76_102;
    let mod_2: i64 = 76_103;
    let mod_6: i64 = 76_104;
    let med_0: i64 = 76_105;
    let low_0: i64 = 76_106;
    give_item(&ctx, mod_0, 3_170, char_a, ship_a, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, mod_1, 3_170, char_a, ship_a, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, mod_2, 3_170, char_a, ship_a, InventoryFlag::HiSlot2, 1);
    give_item(&ctx, mod_6, 3_170, char_a, ship_a, InventoryFlag::HiSlot6, 1);
    give_item(&ctx, 76_107, 222, char_a, ship_a, InventoryFlag::HiSlot0, 2_000);
    give_item(&ctx, med_0, 12_058, char_a, ship_a, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, low_0, 3_530, char_a, ship_a, InventoryFlag::LoSlot0, 1);
    // Fill remaining med & low slots so slot_ratio = 16 / 19 (~0.84) when online
    let mut filler_ids = Vec::new();
    for (i, flag) in [
        InventoryFlag::MedSlot1, InventoryFlag::MedSlot2, InventoryFlag::MedSlot3, InventoryFlag::MedSlot4,
        InventoryFlag::LoSlot1, InventoryFlag::LoSlot2, InventoryFlag::LoSlot3, InventoryFlag::LoSlot4,
        InventoryFlag::LoSlot5, InventoryFlag::LoSlot6,
    ].into_iter().enumerate() {
        let fid = 76_200 + i as i64;
        give_item(&ctx, fid, 1_319, char_a, ship_a, flag, 1);
        filler_ids.push(fid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &[mod_0, mod_1, mod_2, mod_6, med_0, low_0] {
            snap.module_online_states.insert(mid, true);
        }
        for &fid in &filler_ids {
            snap.module_online_states.insert(fid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_a, ship_a, 24_690, jita_sys, origin).await;
    let target_id: i64 = 88_810;
    let mut t_cs = CombatState::default();
    t_cs.max_shield_hp = 5_000_000.0;
    t_cs.shield_hp = 5_000_000.0;
    sys.spawn_ball_with_combat(
        Ball {
            id: target_id, type_id: 640, position: origin + DVec3::new(800.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1e7, radius: 100.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        t_cs,
    ).await.unwrap();
    sys.add_target_lock(ship_a, eve_destiny::TargetLock {
        target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await.unwrap();

    // 1. Seeded probabilistic heat damage: cold vs hot rack, non-wrapping distance attenuation, cross-rack isolation,
    // Thermodynamics L0 vs L5 (-25%), and Online Slots / Total Slots reduction (`feat:modules.overheat-damage-propagation`)
    ctx.set_mechanic_rng_seed(Some(2026));
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.0);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..30 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let dmg_cold = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();

    // Hot rack (1175 = 0.85) under same seed 7 over 120s (~60 cycles on 400 HP cap): strictly probabilistic (0 < hits < 60),
    // monotonic attenuation d0 > d1 > d2 > d6, and zero cross-rack bleed!
    for &mid in &[mod_0, mod_1, mod_2, mod_6] {
        set_item_dyn_attr(&ctx, mid, 3, 0.0);
        set_item_dyn_attr(&ctx, mid, 40, 400.0);
    }
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.85);
    ctx.set_mechanic_rng_seed(Some(7));
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..120 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
    let d0_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    let d1_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_1)]).await.unwrap(), 3);
    let d2_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_2)]).await.unwrap(), 3);
    let d6_l0 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_6)]).await.unwrap(), 3);
    let d_med = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(med_0)]).await.unwrap(), 3);
    let d_low = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(low_0)]).await.unwrap(), 3);

    assert!(d0_l0 > dmg_cold, "Hot rack (0.85) must produce more heat hits than cold rack: {d0_l0} vs {dmg_cold}");
    assert!(d0_l0 > 0.0 && d0_l0 < 60.0 * 0.8, "Heat hits must be probabilistic (0 < hits < N): d0={d0_l0}");
    assert!(d0_l0 > d1_l0 && d1_l0 > d2_l0 && d2_l0 > d6_l0, "Non-wrapping slot distance attenuation: d0={d0_l0}, d1={d1_l0}, d2={d2_l0}, d6={d6_l0}");
    assert_eq!(d_med, 0.0, "High rack heat must not bleed into MedSlot0");
    assert_eq!(d_low, 0.0, "High rack heat must not bleed into LoSlot0");

    // Thermodynamics L5 (-25% per-hit damage) under identical seed 7 -> exact 0.75x damage!
    set_skill_level(&ctx, char_a, 28_164, 5);
    for &mid in &[mod_0, mod_1, mod_2, mod_6] { set_item_dyn_attr(&ctx, mid, 3, 0.0); }
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.85);
    ctx.set_mechanic_rng_seed(Some(7));
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..120 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
    let d0_l5 = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    assert!((d0_l5 - d0_l0 * 0.75).abs() < 1e-4, "Thermodynamics L5 must scale heat damage by 0.75x: {d0_l5} vs {}", d0_l0 * 0.75);

    // Online Slots / Total Slots reduction: offlining 12 non-weapon modules drops slot_ratio from 16/19 to 4/19 -> fewer hits under seed 7!
    ctx.db.with_mut_snapshot(|snap| {
        for &fid in &filler_ids { snap.module_online_states.insert(fid, false); }
        snap.module_online_states.insert(med_0, false);
        snap.module_online_states.insert(low_0, false);
    });
    ctx.invalidate_ship_fitting(ship_a);
    for &mid in &[mod_0, mod_1, mod_2, mod_6] { set_item_dyn_attr(&ctx, mid, 3, 0.0); }
    set_ship_rack_heat(&ctx, ship_a, 1175, 0.85);
    ctx.set_mechanic_rng_seed(Some(7));
    rpc(&ctx, &mut sess_a, "dogmaIM", "Overload", vec![
        PyRep::Integer(mod_0), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..120 {
        sys.set_capacitor(ship_a, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    rpc(&ctx, &mut sess_a, "dogmaIM", "Deactivate", vec![PyRep::Integer(mod_0)]).await.unwrap();
    let d0_sparse = attr_from_item_info(&rpc(&ctx, &mut sess_a, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(mod_0)]).await.unwrap(), 3);
    assert!(d0_sparse < d0_l5, "Offlining slots (lower Online/Total Slots) must reduce probabilistic heat hits: {d0_sparse} < {d0_l5}");

    // Analytical check of R-HEAT-03: total_slots includes rig slots (attr 1137), so 4 online hi+mid+low on a hull with rig slots
    // has slot_ratio = 4.0 / (hi + mid + low + rig).
    let thermo_eval = eve_server::services::tactical_module_mechanics::evaluate_ship_thermodynamics(
        ship_a,
        &Some(ctx.sde.clone()),
        &Some(ctx.db.clone()),
    );
    let sh_t = &ctx.sde.types[&ctx.db.with_snapshot(|s| s.items[&ship_a].type_id)];
    let expected_total_slots = sh_t.default_attributes.get(&14).copied().unwrap_or(0.0)
        + sh_t.default_attributes.get(&13).copied().unwrap_or(0.0)
        + sh_t.default_attributes.get(&12).copied().unwrap_or(0.0)
        + sh_t.default_attributes.get(&1137).copied().unwrap_or(0.0);
    assert!(
        (thermo_eval.slot_ratio - 4.0 / expected_total_slots).abs() < 1e-6,
        "slot_ratio must include rig slots (attr 1137) in denominator: got {} vs {}",
        thermo_eval.slot_ratio,
        4.0 / expected_total_slots
    );
}

#[tokio::test]
async fn test_nanite_repair_and_overload_cross_module_lockout() {
    let ctx = build_ctx();
    let (char_id, ship_id, mod_a, mod_b) = (94_081, 84_081, 75_801, 75_802);
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 24_690, 30_000_142, false);
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.set_capacitor(ship_id, 10_000.0).await.unwrap();
    give_item(&ctx, mod_a, 12_058, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, mod_b, 3_530, char_id, ship_id, InventoryFlag::LoSlot0, 1);
    give_item(&ctx, 75_803, 28668, char_id, ship_id, InventoryFlag::Cargo, 50);
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(char_id).or_default().insert(28164, eve_db::TrainedSkillRecord { type_id: 28164, level: 5, skill_points: 256_000.0 });
        s.items.get_mut(&mod_a).unwrap().dynamic_attributes.insert(3, 10.0);
    });
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(mod_a)]).await.unwrap();
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(mod_b)]).await.unwrap();
    // Direction 1: Active nanite repair on mod_a blocks overloading mod_b
    rpc(&ctx, &mut sess, "dogmaIM", "RepairModule", vec![PyRep::Integer(mod_a)]).await.unwrap();
    let err1 = rpc(&ctx, &mut sess, "dogmaIM", "Overload", vec![PyRep::Integer(mod_b)]).await.unwrap_err();
    assert_eq!(err1.code, "CannotOverheatWhileRepairing");
    for _ in 0..30 {
        sys.step_manual(1.0).await.unwrap();
    }
    // Direction 2: Active overloaded cycle on mod_b blocks initiating nanite repair on mod_a
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&mod_a).unwrap().dynamic_attributes.insert(3, 10.0);
    });
    rpc(&ctx, &mut sess, "dogmaIM", "Overload", vec![PyRep::Integer(mod_b)]).await.unwrap();
    let err2 = rpc(&ctx, &mut sess, "dogmaIM", "RepairModule", vec![PyRep::Integer(mod_a)]).await.unwrap_err();
    assert_eq!(err2.code, "CannotRepairWhileOverloaded");
}

#[tokio::test]
async fn test_interdiction_probe_excess_return_full_cargo_rejected() {
    let ctx = build_ctx();
    let sys_id = 30_004_608;
    let origin = DVec3::new(60_000_000_000.0, 0.0, 0.0);
    let (char_id, ship_id, launcher_id) = (94_082, 84_082, 75_811);
    let (char_b, ship_b) = (94_083, 84_083);
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 22_456, sys_id, false);
    let _sess_b = seed_pilot(&ctx, char_b, ship_b, 587, sys_id, false);
    spawn_in_space(&ctx, char_id, ship_id, 22_456, sys_id, origin).await;
    spawn_in_space(&ctx, char_b, ship_b, 587, sys_id, origin + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, mut b_snap) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
    b_snap.velocity = DVec3::new(100.0, 0.0, 0.0);
    b_snap.mode = MovementMode::new_warp(
        origin + DVec3::new(2_000.0, 0.0, 0.0),
        origin + DVec3::new(20_000_000.0, 0.0, 0.0),
        3.0,
    );
    sys.spawn_ball_with_meta(
        b_snap,
        ctx.build_ship_combat_state(char_b, ship_b, 587),
        ctx.build_ship_slim_meta(char_b, 587),
    )
    .await
    .unwrap();
    let _ = sys.get_set_state(ship_b).await.unwrap();
    let _ = sys.drain_observer_updates(ship_b).await.unwrap();

    give_item(&ctx, launcher_id, 22_782, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 75_812, 22_778, char_id, ship_id, InventoryFlag::HiSlot0, 4);
    give_item(&ctx, 75_813, 34, char_id, ship_id, InventoryFlag::Cargo, 100_000);
    ctx.invalidate_ship_fitting(ship_id);
    rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(launcher_id)]).await.unwrap();
    let err = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)]).await.unwrap_err();
    assert_eq!(err.code, "NotEnoughCargoSpace");

    let (_tick, obs_evs, _rep) = sys.drain_observer_updates(ship_b).await.unwrap();
    assert!(
        !obs_evs.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::AddBalls2 { .. })),
        "Rejected probe launch must emit zero AddBalls2 events"
    );
    let (_, post_b) = sys.get_ball_snapshot(ship_b).await.unwrap().unwrap();
    assert!(
        matches!(post_b.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Aligning, .. }),
        "Rejected probe launch must not disturb nearby Aligning ship: got {:?}",
        post_b.mode
    );
    assert!((post_b.velocity.x - 100.0).abs() < 1e-6);
}

#[tokio::test]
async fn test_prealigned_warp_bubble_interception_no_overshoot() {
    let ctx = build_ctx();
    let sys_id = 30_004_608;
    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    let (char_id, ship_id) = (94_084, 84_084);
    let _sess = seed_pilot(&ctx, char_id, ship_id, 11_198, sys_id, false);
    spawn_in_space(&ctx, char_id, ship_id, 11_198, sys_id, origin).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, mut ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    ball.velocity = DVec3::new(ball.max_velocity * 0.80, 0.0, 0.0);
    ball.mode = MovementMode::new_warp(origin, origin + DVec3::new(200_000.0, 0.0, 0.0), 10.0);
    sys.spawn_ball_with_meta(
        ball,
        ctx.build_ship_combat_state(char_id, ship_id, 11_198),
        ctx.build_ship_slim_meta(char_id, 11_198),
    )
    .await
    .unwrap();
    sys.add_interdiction_sphere(999_001, origin + DVec3::new(100_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();

    let _ = sys.step_manual(1.0).await.unwrap();
    let (_, post) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let edge_x = origin.x + 80_000.0;
    assert!(
        post.position.x <= edge_x + 1e-3,
        "Pre-aligned ship must not overshoot catch bubble edge ({edge_x}) on Tick 1: got {}",
        post.position.x
    );
    if let MovementMode::Warp { destination, .. } = post.mode {
        assert!((destination.x - edge_x).abs() < 1e-3);
    }
}

#[tokio::test]
async fn test_reconnect_in_space_clears_emergency_warp_bubble_immunity() {
    let ctx = build_ctx();
    let sys_id = 30_004_608;
    let origin = DVec3::new(60_000_000_000.0, 0.0, 0.0);
    let (char_id, ship_id) = (94_085, 84_085);
    let _sess = seed_pilot(&ctx, char_id, ship_id, 587, sys_id, false);
    spawn_in_space(&ctx, char_id, ship_id, 587, sys_id, origin).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.add_interdiction_sphere(999_002, origin, 25_000.0).await.unwrap();

    // Disconnect inside active bubble -> initiates emergency warp with is_emergency_warping = true
    let disc = eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .expect("disconnect entry");
    assert!(disc.emergency_warp_dest.is_some());
    assert!(sys.get_combat_state(ship_id).await.unwrap().unwrap().is_emergency_warping);

    // Reconnect immediately while still < 10 km from orig_pos -> must clear is_emergency_warping
    let reconn = eve_server::sp6_lifecycle::reconnect_character_in_space_sync(&ctx, char_id);
    assert!(reconn.is_some());
    assert!(
        !sys.get_combat_state(ship_id).await.unwrap().unwrap().is_emergency_warping,
        "reconnect_character_in_space_sync must unconditionally clear is_emergency_warping"
    );

    // Also verify Ballpark::set_mode clears is_emergency_warping on non-emergency mode transitions
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().is_emergency_warping = true;
        bp.set_mode(ship_id, MovementMode::Stop).unwrap();
        assert!(!bp.combat_states[&ship_id].is_emergency_warping);
    }

    // Reconnected pilot inside active bubble must be blocked from initiating warp
    let warp_err = sys
        .set_movement_mode(
            ship_id,
            MovementMode::new_warp(origin, origin + DVec3::new(1_000_000.0, 0.0, 0.0), 5.0),
        )
        .await;
    assert!(warp_err.is_err(), "Reconnected pilot inside active warp bubble must not warp");
}

#[tokio::test]
async fn test_high_velocity_subwarp_collision_no_tunneling() {
    let ctx = build_ctx();
    let sys_id = 30_000_142;
    let (char_id, ship_id) = (94_086, 84_086);
    let _sess = seed_pilot(&ctx, char_id, ship_id, 11_198, sys_id, false);
    spawn_in_space(&ctx, char_id, ship_id, 11_198, sys_id, DVec3::new(70_000_000_000.0, 0.0, 0.0)).await;
    let sys = ctx.systems.get_or_spawn(sys_id);

    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .expect("Jita stargate");
    let (_, gate_ball) = sys.get_ball_snapshot(gate.stargate_id).await.unwrap().unwrap();
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 11_198);
    cs.undock_invulnerable_remaining_ticks = 0;

    // Place ship 500 m outside gate hull moving inward at 6,500 m/s (single-tick penetration > 5,000 m)
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 11_198,
            position: gate.position + DVec3::new(gate_ball.radius + 500.0, 0.0, 0.0),
            velocity: DVec3::new(-6_500.0, 0.0, 0.0),
            max_velocity: 6_500.0,
            inertia: 0.5,
            mass: 1_500_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 11_198),
    )
    .await
    .unwrap();

    let tick = sys.step_manual(1.0).await.unwrap();
    let (_, post) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        post.position.distance(gate.position) >= gate_ball.radius + post.radius - 1e-3,
        "High-velocity (> 6 km/s) ship must not tunnel through stargate hull"
    );
    assert!(post.velocity.x > 0.0, "High-velocity ship must bounce off stargate hull");
    assert!(tick.events.iter().any(|e| matches!(
        e,
        eve_destiny::DestinyEvent::Collision { ball_a, ball_b }
            if (*ball_a == ship_id && *ball_b == gate.stargate_id)
                || (*ball_a == gate.stargate_id && *ball_b == ship_id)
    )));
}

#[tokio::test]
async fn test_emergency_warp_stopped_by_warp_scrambler_during_alignment() {
    let ctx = build_ctx();
    let sys_id = 30_004_609;
    let origin = DVec3::new(65_000_000_000.0, 0.0, 0.0);
    let (char_id, ship_id) = (94_087, 84_087);
    let _sess = seed_pilot(&ctx, char_id, ship_id, 587, sys_id, false);
    spawn_in_space(&ctx, char_id, ship_id, 587, sys_id, origin).await;
    let sys = ctx.systems.get_or_spawn(sys_id);

    // Disconnect unpointed ship -> initiates emergency logoff warp in Aligning phase
    let disc = eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .expect("disconnect entry");
    assert!(disc.emergency_warp_dest.is_some());
    assert!(sys.get_combat_state(ship_id).await.unwrap().unwrap().is_emergency_warping);
    let (_, pre) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(
        pre.mode,
        MovementMode::Warp {
            phase: eve_destiny::WarpPhase::Aligning,
            ..
        }
    ));

    // Apply a warp scrambler while the emergency warp is aligning -> must stop the warp
    sys.set_warp_scramble_strength(ship_id, 2).await.unwrap();
    let _ = sys.step_manual(1.0).await.unwrap();
    let (_, post) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(post.mode, MovementMode::Stop),
        "Warp scrambler applied during emergency warp alignment must stop the ball: got {:?}",
        post.mode
    );
    assert!(
        !sys.get_combat_state(ship_id).await.unwrap().unwrap().is_emergency_warping,
        "Stopping emergency warp via scrambler must clear is_emergency_warping"
    );
}


}

#[allow(unused_imports, dead_code)]
mod nosferatu_grappler_and_mutadaptive_repairer {
    use super::*;

// End-to-End Integration Tests closing Nosferatu, Grappler, and Mutadaptive RAR:
// `feat:modules.energy-nosferatu-relative-capacitor`, `feat:modules.stasis-grappler-mechanics`,
// and `feat:modules.mutadaptive-remote-armor-repairer`.

use crate::modules_advanced_tactical::*;
use eve_db::{ContractRecord, InventoryFlag, IskCents};
use eve_destiny::DVec3;
use eve_proto::PyRep;
use eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband;

#[tokio::test]
async fn test_nosferatu_relative_cap_grappler_mutadaptive_rar_and_blockade_runner() {
    // parity: feat:modules.stasis-grappler-mechanics, feat:modules.mutadaptive-remote-armor-repairer
    eve_testkit::attest(&[
        "feat:modules.stasis-grappler-mechanics",
        "feat:modules.mutadaptive-remote-armor-repairer",
    ]);

    let ctx = build_ctx();
    let jita_sys: u32 = 30_000_142;
    let char_src: i64 = 94_051;
    let ship_src: i64 = 84_051;
    let char_tgt: i64 = 94_052;
    let ship_tgt: i64 = 84_052;
    let char_tgt2: i64 = 94_053;
    let ship_tgt2: i64 = 84_053;

    let mut sess_src = seed_pilot(&ctx, char_src, ship_src, 640, jita_sys, false);
    let _sess_tgt = seed_pilot(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, false);
    let _sess_tgt2 = seed_pilot(&ctx, char_tgt2, ship_tgt2, 12_729, jita_sys, false); // Crane (Blockade Runner)
    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Energy Nosferatu Absolute GJ Rule & Blood Raider Override (`feat:modules.energy-nosferatu-relative-capacitor`)
    let nos_id: i64 = 77_001;
    give_item(&ctx, nos_id, 530, char_src, ship_src, InventoryFlag::HiSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(nos_id, true); });
    ctx.invalidate_ship_fitting(ship_src);

    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_src, ship_src, 640, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(1_500.0, 0.0, 0.0)).await;
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    // 1a. Lower % (30% < 50%) but higher absolute GJ (Source 1500 / 5000 GJ >= Target 150 / 300 GJ): 0 GJ drained!
    let mut s_cs = sys.get_combat_state(ship_src).await.unwrap().unwrap();
    s_cs.capacitor_recharge_time_ms = 0.0;
    s_cs.max_capacitor = 5_000.0;
    s_cs.capacitor = 1_500.0;
    sys.set_combat_state(ship_src, s_cs.clone()).await.unwrap();
    let mut t_cs = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
    t_cs.capacitor_recharge_time_ms = 0.0;
    t_cs.max_capacitor = 300.0;
    t_cs.capacitor = 150.0;
    sys.set_combat_state(ship_tgt, t_cs.clone()).await.unwrap();

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.expect("Activate Nosferatu");
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert_eq!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor, 150.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // 1b. Relative cap comparison:
    // When Source % >= Target % (80% > 50%), standard hull does not drain even if absolute GJ is lower (120 < 2000)
    let mut s_cs2 = sys.get_combat_state(ship_src).await.unwrap().unwrap();
    s_cs2.capacitor_recharge_time_ms = 0.0;
    s_cs2.max_capacitor = 150.0;
    s_cs2.capacitor = 120.0;
    sys.set_combat_state(ship_src, s_cs2.clone()).await.unwrap();
    let mut t_cs2 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap();
    t_cs2.capacitor_recharge_time_ms = 0.0;
    t_cs2.max_capacitor = 4_000.0;
    t_cs2.capacitor = 2_000.0;
    sys.set_combat_state(ship_tgt, t_cs2.clone()).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.unwrap();
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert_eq!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor, 2_000.0);
    assert_eq!(sys.get_combat_state(ship_src).await.unwrap().unwrap().capacitor, 120.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // When Source % < Target % (26.7% < 50%) and absolute GJ is lower (40 < 2000), drain occurs
    s_cs2.capacitor = 40.0;
    sys.set_combat_state(ship_src, s_cs2).await.unwrap();
    sys.set_combat_state(ship_tgt, t_cs2).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.unwrap();
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 2_000.0);
    assert!(sys.get_combat_state(ship_src).await.unwrap().unwrap().capacitor > 40.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // 1c. Blood Raider Bhaalgorn (17920) overrides absolute GJ limit (drains Target 150 GJ even with Source 1500 GJ)
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_src).unwrap().type_id = 17_920; });
    ctx.invalidate_ship_fitting(ship_src);
    sys.set_combat_state(ship_src, s_cs).await.unwrap();
    sys.set_combat_state(ship_tgt, t_cs).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(nos_id), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(0),
    ]).await.unwrap();
    for _ in 0..3 { sys.step_manual(1.0).await.unwrap(); }
    assert!(sys.get_combat_state(ship_tgt).await.unwrap().unwrap().capacitor < 150.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(nos_id)]).await.unwrap();

    // 2. Heavy Stasis Grappler Mechanics (`feat:modules.stasis-grappler-mechanics`)
    let mut cruiser_sess = seed_pilot(&ctx, 94_054, 84_054, 621, jita_sys, true);
    assert!(fit_mod(&ctx, &mut cruiser_sess, 84_054, 77_010, 41_040, 19).await.is_err());

    let mut bs_fit_sess = seed_pilot(&ctx, char_src, ship_src, 640, jita_sys, true);
    fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_011, 41_040, 19).await.expect("Battleship fits 1st Grappler");
    assert!(fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_012, 41_040, 20).await.is_err());
    fit_mod(&ctx, &mut bs_fit_sess, ship_src, 77_013, 527, 20).await.expect("Fit Stasis Webifier II");

    sess_src.station_id = None;
    sess_src.solar_system_id = Some(jita_sys);
    spawn_in_space(&ctx, char_src, ship_src, 640, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(500.0, 0.0, 0.0)).await;
    let (_, base_t_ball) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    let base_t_vel = base_t_ball.max_velocity;
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    let (_, b_500) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!((b_500.max_velocity - base_t_vel * 0.20).abs() < 1.0);

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_013), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    let (_, b_stacked) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(b_stacked.max_velocity > base_t_vel * 0.085 && b_stacked.max_velocity < base_t_vel * 0.11);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_013)]).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

    sys.set_ball_position(ship_tgt, origin + DVec3::new(16_000.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    let (_, b_16k) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(b_16k.max_velocity > base_t_vel * 0.90);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

    // Overloaded Grappler (`attr 1222` = +300% optimal -> 4,000m) MUST retain full -80% web across ticks at 3,500m!
    sys.set_ball_position(ship_tgt, origin + DVec3::new(3_500.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_011), PyRep::String("useMissiles".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    sys.step_manual(1.0).await.unwrap();
    let (_, b_3500_norm) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(b_3500_norm.max_velocity > base_t_vel * 0.23, "Cold grappler at 3.5km (>1km optimal) decays below full 80%");
    rpc(&ctx, &mut sess_src, "dogmaIM", "Overload", vec![PyRep::Integer(77_011)]).await.unwrap();
    sys.step_manual(1.0).await.unwrap();
    let (_, b_3500_ovl) = sys.get_ball_snapshot(ship_tgt).await.unwrap().unwrap();
    assert!(
        (b_3500_ovl.max_velocity - base_t_vel * 0.20).abs() < 1.0,
        "Overloaded Grappler (+300% optimal = 4km) must retain full 80% slow across ticks at 3.5km: got {}",
        b_3500_ovl.max_velocity
    );
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_011)]).await.unwrap();

    // 3. Mutadaptive Remote Armor Repairer (`feat:modules.mutadaptive-remote-armor-repairer`): spool, +150% cap (2.5x), unlock & switch reset
    assert!(fit_mod(&ctx, &mut cruiser_sess, 84_054, 77_020, 49_770, 27).await.is_err());
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_src);
        snap.module_online_states.clear();
        snap.module_active_states.clear();
    });
    let mut zarm_fit_sess = seed_pilot(&ctx, char_src, ship_src, 49_713, jita_sys, true);
    ctx.invalidate_ship_fitting(ship_src);
    fit_mod(&ctx, &mut zarm_fit_sess, ship_src, 77_021, 49_770, 27).await.expect("Zarmazd fits 1st Mutadaptive RAR");
    assert!(fit_mod(&ctx, &mut zarm_fit_sess, ship_src, 77_022, 49_770, 28).await.is_err());

    sess_src.station_id = None;
    sess_src.solar_system_id = Some(jita_sys);
    spawn_in_space(&ctx, char_src, ship_src, 49_713, jita_sys, origin).await;
    spawn_in_space(&ctx, char_tgt, ship_tgt, 24_690, jita_sys, origin + DVec3::new(3_000.0, 0.0, 0.0)).await;
    spawn_in_space(&ctx, char_tgt2, ship_tgt2, 12_729, jita_sys, origin + DVec3::new(4_000.0, 0.0, 0.0)).await;
    for &tid in &[ship_tgt, ship_tgt2] {
        let mut dmg_cs = sys.get_combat_state(tid).await.unwrap().unwrap();
        dmg_cs.max_armor_hp = 100_000.0;
        dmg_cs.armor_hp = 1_000.0;
        sys.set_combat_state(tid, dmg_cs).await.unwrap();
        sys.add_target_lock(ship_src, eve_destiny::TargetLock {
            target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await.unwrap();
    }

    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
    let a_c1 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap().armor_hp;
    assert!((a_c1 - 1_784.0).abs() < 1.0);
    for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
    let a_c2 = sys.get_combat_state(ship_tgt).await.unwrap().unwrap().armor_hp;
    assert!((a_c2 - (1_784.0 + 784.0 * 1.1)).abs() < 1.0);

    // Step 16 more cycles (total 18 > 15 maxspool) -> effective attr 84 caps at 784.0 * 2.5 = 1,960.0 HP (+150% cap)!
    for _ in 0..(16 * 6) {
        sys.set_capacitor(ship_src, 10_000.0).await.unwrap();
        sys.step_manual(1.0).await.unwrap();
    }
    let capped_rep = attr_from_item_info(&rpc(&ctx, &mut sess_src, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(77_021)]).await.unwrap(), 84);
    assert!((capped_rep - 784.0 * 2.5).abs() < 1e-2, "Mutadaptive RAR spool must cap at 2.5x (+150% = 1960.0): got {capped_rep}");

    // Unlocking target resets spool immediately back to 1.0x (784.0)
    rpc(&ctx, &mut sess_src, "dogmaIM", "RemoveTargets", vec![PyRep::List(vec![PyRep::Integer(ship_tgt)])]).await.unwrap();
    let rep_after_unlock = attr_from_item_info(&rpc(&ctx, &mut sess_src, "dogmaIM", "ItemGetInfo", vec![PyRep::Integer(77_021)]).await.unwrap(), 84);
    assert!((rep_after_unlock - 784.0).abs() < 1e-2);

    // Target switch resets spool back to 1.0x on new target
    sys.add_target_lock(ship_src, eve_destiny::TargetLock {
        target_id: ship_tgt, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..12 { sys.set_capacitor(ship_src, 10_000.0).await.unwrap(); sys.step_manual(1.0).await.unwrap(); }
    rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(77_021), PyRep::String("remoteArmorRepairFalloff".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
    ]).await.unwrap();
    for _ in 0..6 { sys.set_capacitor(ship_src, 10_000.0).await.unwrap(); sys.step_manual(1.0).await.unwrap(); }
    let a_tgt2_c1 = sys.get_combat_state(ship_tgt2).await.unwrap().unwrap().armor_hp;
    assert!((a_tgt2_c1 - 1_784.0).abs() < 1.0);
    rpc(&ctx, &mut sess_src, "dogmaIM", "Deactivate", vec![PyRep::Integer(77_021)]).await.unwrap();

    // 4. Blockade Runner Cargo Scan Immunity, Ship Scanner Visibility, & NPC Customs Contraband (pending V24 live message)
    let cargo_scanner_id: i64 = 77_031;
    let ship_scanner_id: i64 = 77_032;
    give_item(&ctx, cargo_scanner_id, 2_038, char_src, ship_src, InventoryFlag::MedSlot0, 1); // Cargo Scanner II
    give_item(&ctx, ship_scanner_id, 443, char_src, ship_src, InventoryFlag::MedSlot1, 1); // Ship Scanner I
    give_item(&ctx, 77_035, 1_319, char_tgt2, ship_tgt2, InventoryFlag::LoSlot0, 1); // Fitted module on Crane
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cargo_scanner_id, true);
        snap.module_online_states.insert(ship_scanner_id, true);
        snap.module_online_states.insert(77_035, true);
    });
    ctx.invalidate_ship_fitting(ship_src);
    ctx.invalidate_ship_fitting(ship_tgt2);

    // Crane (12729, Blockade Runner) carrying Tritanium, PLEX, and contraband Standard Crash Booster (9947)
    give_item(&ctx, 77_041, 34, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 500);
    give_item(&ctx, 77_042, 44_992, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 1);
    give_item(&ctx, 77_044, 9_947, char_tgt2, ship_tgt2, InventoryFlag::Cargo, 2);

    // 4a. Player Cargo Scanner II returns empty scan (immune)
    let scan_crane = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(cargo_scanner_id), PyRep::String("cargoScan".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
    ]).await.expect("Cargo Scan Crane Blockade Runner");
    assert_eq!(scan_crane.dict_get("cargoScanImmune"), Some(&PyRep::Bool(true)));
    assert_eq!(scan_crane.dict_get("totalScannedQuantity"), Some(&PyRep::Integer(0)));
    assert_eq!(scan_crane.dict_get("scannedItems"), Some(&PyRep::List(Vec::new())));

    // 4b. Player Ship Scanner I (group 48) is NOT blocked by cargoScanResistance -> reveals fitted modules!
    let ship_scan_crane = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(ship_scanner_id), PyRep::String("shipScan".into()), PyRep::Integer(ship_tgt2), PyRep::Integer(1),
    ]).await.expect("Ship Scan Crane Blockade Runner");
    let fitted_list = match ship_scan_crane.dict_get("fittedModules") {
        Some(PyRep::List(v)) => v.len(),
        _ => 0,
    };
    assert!(fitted_list >= 1, "Ship Scanner I must reveal fitted modules on Blockade Runner");

    // 4c. High-sec NPC customs contraband enforcement still detects, confiscates, and fines contraband inside Blockade Runner!
    let found_contraband = scan_and_enforce_highsec_contraband(&ctx, jita_sys, ship_tgt2);
    assert_eq!(found_contraband.len(), 1, "High-sec NPC customs must detect contraband inside Blockade Runner");
    assert_eq!(found_contraband[0].0, 9_947);
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&77_044)), "Contraband booster must be confiscated");

    // 4d. Bustard (12731, Deep Space Transport, cargoScanResistance = 0) exposes cargo manifest to player Cargo Scanner II
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_tgt).unwrap().type_id = 12_731; });
    give_item(&ctx, 77_043, 34, char_tgt, ship_tgt, InventoryFlag::Cargo, 500);
    let scan_bustard = rpc(&ctx, &mut sess_src, "dogmaIM", "Activate", vec![
        PyRep::Integer(cargo_scanner_id), PyRep::String("cargoScan".into()), PyRep::Integer(ship_tgt), PyRep::Integer(1),
    ]).await.expect("Scan Bustard DST");
    assert_eq!(scan_bustard.dict_get("cargoScanImmune"), None);
    assert_eq!(scan_bustard.dict_get("totalScannedQuantity"), Some(&PyRep::Integer(500)));
}

#[tokio::test]
async fn test_nanite_repair_paste_streaming_and_cargo_refund_capacity() {
    let ctx = build_ctx();
    let char_a: i64 = 94_161;
    let ship_a: i64 = 84_161;
    let mut sess_a = seed_pilot(&ctx, char_a, ship_a, 22_456, NULL_SYS, false); // Sabre (Interdictor, 180 m3 cargo)
    let sys = ctx.systems.get_or_spawn(NULL_SYS);
    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, char_a, ship_a, 22_456, NULL_SYS, origin).await;

    set_skill_level(&ctx, char_a, 28_164, 0); // Thermodynamics L0
    set_skill_level(&ctx, char_a, 28_879, 0); // Nanite Operation L0
    set_skill_level(&ctx, char_a, 28_880, 0); // Nanite Interfacing L0

    // 1. Streaming per-tick paste deduction + clean mid-repair halt when cargo paste is exhausted!
    // Mega Pulse Laser II (`3057`, SDE attr 1277 = 0.5 paste/HP) at 20.0 HP damage requires 10 paste.
    // Seed only 3 paste in cargo -> 1 consumed at t=0, 2 streamed over ticks, then repair halts cleanly at 3/10 (14.0 HP remaining)!
    let mod_stream: i64 = 77_201;
    let paste_stack: i64 = 77_202;
    give_item(&ctx, mod_stream, 3_057, char_a, ship_a, InventoryFlag::HiSlot1, 1);
    give_item(&ctx, paste_stack, 28_668, char_a, ship_a, InventoryFlag::Cargo, 3);
    set_item_dyn_attr(&ctx, mod_stream, 3, 20.0);

    let rep_start = rpc(&ctx, &mut sess_a, "dogmaIM", "InitiateModuleRepair", vec![PyRep::Integer(mod_stream)]).await.unwrap();
    assert_eq!(rep_start.dict_get("pasteConsumed"), Some(&PyRep::Integer(1)), "Only 1 paste unit deducted at t=0");
    assert_eq!(rep_start.dict_get("totalPasteRequired"), Some(&PyRep::Integer(10)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&paste_stack).unwrap().quantity), 2, "2 paste units remain in cargo at t=0");

    for _ in 0..8 {
        sys.step_manual(1.0).await.unwrap();
    }
    let paste_left = ctx.db.with_snapshot(|s| s.items.get(&paste_stack).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(paste_left, 0, "All 3 available paste units must be streamed and consumed");
    assert!(!sys.is_module_being_nanite_repaired_sync(ship_a, mod_stream), "Repair must halt cleanly when cargo paste is exhausted");
    let dmg_after_exhaust = ctx.db.with_snapshot(|s| s.items.get(&mod_stream).unwrap().dynamic_attributes.get(&3).copied().unwrap_or(0.0));
    assert!(
        (dmg_after_exhaust - 14.0).abs() < 0.05,
        "Exhausting 3 of 10 required paste must repair exactly 3/10 of 20 HP (14.0 HP left), got {dmg_after_exhaust}"
    );

    // 2. Cargo capacity guard on cancellation refund: filling cargo to 100% hull capacity before t=0 StopModuleRepair
    // must NOT overflow hull cargo volume!
    give_item(&ctx, 77_203, 28_668, char_a, ship_a, InventoryFlag::Cargo, 2);
    rpc(&ctx, &mut sess_a, "dogmaIM", "InitiateModuleRepair", vec![PyRep::Integer(mod_stream)]).await.unwrap();
    // Fill Sabre cargo (400 m3) completely: 1 unit of 28668 (0.01 m3) + 39,999 Tritanium (`34`, 0.01 m3) = 400.00 m3 (0.00 m3 free!)
    give_item(&ctx, 77_204, 34, char_a, ship_a, InventoryFlag::Cargo, 39_999);
    let stop_full = rpc(&ctx, &mut sess_a, "dogmaIM", "StopModuleRepair", vec![PyRep::Integer(mod_stream)]).await.unwrap();
    assert_eq!(stop_full.dict_get("pasteRefunded"), Some(&PyRep::Integer(0)), "Refund must cap to 0 when ship cargo has 0.00 m3 free");
    let cargo_used_m3: f64 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == ship_a && i.flag == InventoryFlag::Cargo).map(|i| {
            ctx.sde.types.get(&i.type_id).unwrap().volume * (i.quantity as f64)
        }).sum()
    });
    assert!(cargo_used_m3 <= 400.0 + 1e-6, "Cargo volume must never exceed Sabre 400 m3 capacity: got {cargo_used_m3}");
    ctx.db.with_mut_snapshot(|s| { s.items.remove(&77_204); });

    // 3. Interdiction Sphere Launcher (`BF-02`): excess probes above launcher capacity return to Cargo (never destroyed),
    // and contract-locked probes in Cargo are NOT auto-reloaded!
    let launcher_id: i64 = 77_210;
    let over_stack_id: i64 = 77_211;
    let locked_probe_id: i64 = 77_212;
    give_item(&ctx, launcher_id, 22_782, char_a, ship_a, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, over_stack_id, 22_778, char_a, ship_a, InventoryFlag::HiSlot0, 5); // 5 loaded > max 3 (15 m3 / 5 m3)
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(launcher_id, true); });
    ctx.invalidate_ship_fitting(ship_a);

    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(launcher_id), PyRep::String("useMissiles".into()), PyRep::None, PyRep::Integer(0),
    ]).await.expect("Launch 1st probe with 5 loaded");
    let (rem_slot, returned_cargo) = ctx.db.with_snapshot(|s| {
        let slot_q = s.items.get(&over_stack_id).map(|i| i.quantity).unwrap_or(0);
        let cargo_q: i32 = s.items.values().filter(|i| i.location_id == ship_a && i.flag == InventoryFlag::Cargo && i.type_id == 22_778).map(|i| i.quantity).sum();
        (slot_q, cargo_q)
    });
    assert_eq!((rem_slot, returned_cargo), (2, 2), "Excess 2 probes above capacity 3 must return to Cargo (5 - 1 = 4 total preserved)");

    // Clear unlocked cargo probes, leave 1 in launcher, and lock a cargo stack in an Outstanding contract -> auto-reload must skip locked stack!
    ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, i| !(i.location_id == ship_a && i.flag == InventoryFlag::Cargo && i.type_id == 22_778));
        s.items.get_mut(&over_stack_id).unwrap().quantity = 1;
    });
    give_item(&ctx, locked_probe_id, 22_778, char_a, ship_a, InventoryFlag::Cargo, 3);
    ctx.db.with_mut_snapshot(|s| {
        s.contracts.insert(990_001, ContractRecord {
            contract_id: 990_001, issuer_id: char_a, issuer_corp_id: 1_000_009, assignee_id: 0, acceptor_id: None,
            start_station_id: 60_003_760, end_station_id: 60_003_760, contract_type: "ItemExchange".into(),
            status: "Outstanding".into(), title: "Locked Probes".into(), for_corp: false, availability: "Public".into(),
            date_issued_unix: 1, expiration_unix: u64::MAX, num_days: 1,
            price: IskCents(100), reward: IskCents(0), collateral: IskCents(0), buyout: IskCents(0),
            current_bid: IskCents(0), current_bidder_id: None,
            item_ids: vec![locked_probe_id], requested_items: Vec::new(), bid_history: Vec::new(),
        });
    });
    for _ in 0..6 { sys.step_manual(1.0).await.unwrap(); }
    let launch_last = rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(launcher_id), PyRep::String("useMissiles".into()), PyRep::None, PyRep::Integer(0),
    ]).await.unwrap();
    assert_eq!(launch_last.dict_get("reloading"), Some(&PyRep::Bool(false)), "Contract-locked cargo probes must not be auto-reloaded");
    assert_eq!(launch_last.dict_get("remainingInLauncher"), Some(&PyRep::Integer(0)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&locked_probe_id).unwrap().quantity), 3);

    // 4. Scram-aborted MWD (`BF-04`): Warp Scrambler cleans up `snap.module_active_states` and emits `OnGodmaShipEffect` deactivation!
    let char_v: i64 = 94_162;
    let ship_v: i64 = 84_162;
    let mut sess_v = seed_pilot(&ctx, char_v, ship_v, 621, NULL_SYS, false);
    let mwd_id: i64 = 77_220;
    let scram_id: i64 = 77_221;
    give_item(&ctx, mwd_id, 5_973, char_v, ship_v, InventoryFlag::MedSlot0, 1); // 50MN MWD II
    give_item(&ctx, scram_id, 448, char_a, ship_a, InventoryFlag::MedSlot0, 1); // Warp Scrambler II
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(mwd_id, true);
        s.module_online_states.insert(scram_id, true);
    });
    ctx.invalidate_ship_fitting(ship_v);
    ctx.invalidate_ship_fitting(ship_a);
    spawn_in_space(&ctx, char_v, ship_v, 621, NULL_SYS, origin + DVec3::new(3_000.0, 0.0, 0.0)).await;
    sys.add_target_lock(ship_a, eve_destiny::TargetLock {
        target_id: ship_v, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0),
    }).await.unwrap();
    rpc(&ctx, &mut sess_v, "dogmaIM", "Activate", vec![
        PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1),
    ]).await.unwrap();
    assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&mwd_id)));
    let _ = sess_v.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_v);

    rpc(&ctx, &mut sess_a, "dogmaIM", "Activate", vec![
        PyRep::Integer(scram_id), PyRep::String("warpScramblerMWD".into()), PyRep::Integer(ship_v), PyRep::Integer(1),
    ]).await.unwrap();
    assert!(
        !ctx.db.with_snapshot(|s| s.module_active_states.contains(&mwd_id)),
        "Warp Scrambler aborting MWD must remove MWD from snap.module_active_states"
    );
    let notifs_scram = ctx.drain_notifications_for_char(char_v);
    assert!(
        notifs_scram.iter().any(|n| n.event == "OnGodmaShipEffect" && n.args.first() == Some(&PyRep::Integer(mwd_id))),
        "Warp Scrambler aborting MWD must emit OnGodmaShipEffect deactivation notification to victim"
    );
}

#[tokio::test]
async fn test_wubble_destroyed_during_arming_cancels_detonation() {
    let ctx = build_ctx();
    let null_sys: u32 = 30_004_608; // 6VDT-H (-0.70 null-sec)
    let sabre_char: i64 = 94_061;
    let sabre_id: i64 = 84_061;
    let sb_char: i64 = 94_062;
    let sb_ship_id: i64 = 84_062;

    let mut sabre_sess = seed_pilot(&ctx, sabre_char, sabre_id, 22_456, null_sys, false);
    let mut sb_sess = seed_pilot(&ctx, sb_char, sb_ship_id, 640, null_sys, false);
    give_item(&ctx, 77_201, 22_782, sabre_char, sabre_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 77_202, 56_748, sabre_char, sabre_id, InventoryFlag::HiSlot0, 2);
    for (slot_idx, flag) in [
        (0, InventoryFlag::HiSlot0),
        (1, InventoryFlag::HiSlot1),
        (2, InventoryFlag::HiSlot2),
        (3, InventoryFlag::HiSlot3),
    ] {
        give_item(&ctx, 77_210 + slot_idx, 3_995, sb_char, sb_ship_id, flag, 1); // Large EMP Smartbomb II
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(77_201, true);
        for slot_idx in 0..4 {
            snap.module_online_states.insert(77_210 + slot_idx, true);
        }
    });
    ctx.invalidate_ship_fitting(sabre_id);
    ctx.invalidate_ship_fitting(sb_ship_id);

    let sys = ctx.systems.get_or_spawn(null_sys);
    let arena = DVec3::new(50_060_000_000.0, 0.0, 0.0);
    spawn_in_space(&ctx, sabre_char, sabre_id, 22_456, null_sys, arena).await;
    spawn_in_space(&ctx, sb_char, sb_ship_id, 640, null_sys, arena + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let (_, pre_sabre) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    let base_vel = pre_sabre.max_velocity;

    let pw = rpc(&ctx, &mut sabre_sess, "dogmaIM", "Activate", vec![PyRep::Integer(77_201)])
        .await
        .expect("Launch Wubble 56748");
    let pw_id = pw.dict_get("probeID").and_then(|v| v.as_i64()).unwrap();

    // Advance 1.0s (< 2.0s arming window): probe is alive on grid
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_some(), "Probe must still exist at t=1.0s while arming");

    // Fire 4x Large EMP Smartbomb II before 2.0s detonation -> destroys the 1,000 HP arming probe
    for slot_idx in 0..4 {
        rpc(
            &ctx,
            &mut sb_sess,
            "dogmaIM",
            "Activate",
            vec![PyRep::Integer(77_210 + slot_idx), PyRep::String("empWave".into()), PyRep::None, PyRep::Integer(1)],
        )
        .await
        .unwrap();
    }
    assert!(sys.get_ball_snapshot(pw_id).await.unwrap().is_none(), "Smartbomb AoE must destroy arming wubble probe");

    // Step well past the 2.0s detonation window: destroyed probe must NOT trigger a ghost web debuff
    for _ in 0..4 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, post_sabre) = sys.get_ball_snapshot(sabre_id).await.unwrap().unwrap();
    assert!(
        (post_sabre.max_velocity - base_vel).abs() < 1e-2,
        "Destroyed arming wubble must NOT apply ghost velocity debuff: got {} vs {}",
        post_sabre.max_velocity,
        base_vel
    );
}


}

#[allow(unused_imports, dead_code)]
mod interdiction_launcher_and_warp_vector_negatives {
    use super::*;

// Negative integration tests for `R-INT-01` (Interdiction Sphere Launcher activation
// rejections when docked, not in space, cloaked, tethered, or in active warp) and `R-MJD-02`
// (`warpVectorMgr.UseVector` rejection during MJD spool + `Ballpark::micro_jump` /
// `Ballpark::micro_jump_field` rejection during active warp).

use crate::esi_market_industry_pi::make_structure;
use crate::modules_advanced_tactical::*;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, Ballpark, DVec3, MovementMode, WarpPhase};
use eve_proto::PyRep;
use std::sync::Arc;

#[tokio::test]
async fn test_interdiction_sphere_launcher_negative_states() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 94_201;
    let ship_id: i64 = 84_201;
    let launcher_id: i64 = 74_201;
    let probe_id: i64 = 74_202;

    // Sabre (22456, group 541 Interdictor) + Interdiction Sphere Launcher I (22782) + Warp Disrupt Probe (22778)
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 22_456, jita_sys, false);
    give_item(&ctx, launcher_id, 22_782, char_id, ship_id, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, probe_id, 22_778, char_id, ship_id, InventoryFlag::HiSlot0, 5);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(launcher_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(jita_sys);

    // 1. Docked -> ShipDocked (even if ball is present)
    let mut cs_init = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
    cs_init.capacitor = 100_000.0;
    cs_init.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 22_456,
            position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_500_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs_init,
        ctx.build_ship_slim_meta(char_id, 22_456),
    )
    .await
    .unwrap();
    sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_id) {
            c.station_id = Some(60_003_760);
        }
    });
    let err_docked = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await
        .unwrap_err();
    assert_eq!(err_docked.code, "ShipDocked");
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_id) {
            c.station_id = None;
        }
    });

    // 2. Not in space -> ShipNotInSpace
    let _ = sys.remove_ball(ship_id).await;
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let err_no_ball = eve_server::services::tactical_module_mechanics::activate_interdiction_sphere_launcher(
        &ctx, &sys, jita_sys, char_id, ship_id, 22_456, launcher_id,
        fit.modules.get(&launcher_id).unwrap(), ctx.sde.types.get(&22_782).unwrap(), "online", None,
    )
    .await
    .unwrap_err();
    assert_eq!(err_no_ball.code, "ShipNotInSpace");

    // 3. Cloaked -> CannotActivateCloaked
    let mut cs_cloaked = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
    cs_cloaked.capacitor = 100_000.0;
    cs_cloaked.max_capacitor = 100_000.0;
    cs_cloaked.is_cloaked = true;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 22_456,
            position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_500_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs_cloaked,
        ctx.build_ship_slim_meta(char_id, 22_456),
    )
    .await
    .unwrap();
    let err_cloaked = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await
        .unwrap_err();
    assert_eq!(err_cloaked.code, "CannotActivateCloaked");

    // 4. Tethered -> CantActivateWhileTethered
    let _ = sys.remove_ball(ship_id).await;
    let mut cs_tethered = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
    cs_tethered.capacitor = 100_000.0;
    cs_tethered.max_capacitor = 100_000.0;
    cs_tethered.is_tethered = true;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 22_456,
            position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_500_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs_tethered,
        ctx.build_ship_slim_meta(char_id, 22_456),
    )
    .await
    .unwrap();
    let err_tethered = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await
        .unwrap_err();
    assert_eq!(err_tethered.code, "CantActivateWhileTethered");

    // 5. Active non-Aligning Warp -> CantActivateInWarp
    let _ = sys.remove_ball(ship_id).await;
    let mut cs_warping = ctx.build_ship_combat_state(char_id, ship_id, 22_456);
    cs_warping.capacitor = 100_000.0;
    cs_warping.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 22_456,
            position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::new(1_000_000.0, 0.0, 0.0),
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_500_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::Warp {
                origin: DVec3::new(50_000_000_000.0, 0.0, 0.0),
                destination: DVec3::new(100_000_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 50_000_000_000.0,
                elapsed_warp_time: 2.0,
                phase: WarpPhase::Cruising,
            },
        },
        cs_warping,
        ctx.build_ship_slim_meta(char_id, 22_456),
    )
    .await
    .unwrap();
    let err_warping = rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(launcher_id)])
        .await
        .unwrap_err();
    assert_eq!(err_warping.code, "CantActivateInWarp");
}

#[tokio::test]
async fn test_warp_vector_rejected_during_mjd_spool() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 94_202;
    let ship_id: i64 = 84_202;
    let mjd_id: i64 = 74_211;
    let vector_id: i64 = 74_212;

    // Scorpion (640, Battleship) + Large Micro Jump Drive (4383) + Jovian Symbolic Navigator (77825)
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 640, jita_sys, false);
    give_item(&ctx, mjd_id, 4_383, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    give_item(&ctx, vector_id, 77_825, char_id, ship_id, InventoryFlag::Cargo, 2);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mjd_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys = ctx.systems.get_or_spawn(jita_sys);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 640);
    cs.capacitor = 100_000.0;
    cs.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 640,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 150.0,
            inertia: 1.0,
            mass: 100_000_000.0,
            radius: 250.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 640),
    )
    .await
    .unwrap();

    // Start MJD spool
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mjd_id)])
        .await
        .expect("Activate MJD spool");

    // warpVectorMgr.UseVector must fail during MJD spool
    let err_spool = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vector_id)])
        .await
        .unwrap_err();
    assert!(
        err_spool.code.contains("Mjd") || err_spool.message.to_lowercase().contains("micro jump"),
        "Expected MJD spool rejection, got: {err_spool:?}"
    );

    // Defense-in-depth: Ballpark::micro_jump and micro_jump_field reject active non-Aligning Warp
    let mut bp = Ballpark::new(jita_sys);
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 640,
        position: DVec3::ZERO,
        velocity: DVec3::new(1_000_000.0, 0.0, 0.0),
        max_velocity: 150.0,
        inertia: 1.0,
        mass: 100_000_000.0,
        radius: 250.0,
        speed_fraction: 1.0,
        mode: MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(100_000_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 100_000_000_000.0,
            elapsed_warp_time: 2.0,
            phase: WarpPhase::Cruising,
        },
    });
    assert!(bp.micro_jump(ship_id, 100_000.0).unwrap_err().contains("CantActivateInWarp"));
    assert!(bp.micro_jump_field(ship_id, 100_000.0, 6_000.0, 25).unwrap_err().contains("CantActivateInWarp"));
}

#[tokio::test]
async fn test_warp_bubble_warp_entry_only_and_shuttle_emergency_exemptions() {
    let mut bp = Ballpark::new(30_000_142);
    let origin = DVec3::ZERO;
    let dest = DVec3::new(100_000_000.0, 0.0, 0.0);
    let bubble_center = DVec3::new(100_000_000.0, 0.0, 0.0);

    // (a) Bubble present at set_mode that despawns during Aligning must NOT trap the ship
    bp.add_ball(Ball {
        id: 1001, type_id: 587, position: origin, velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 3.45, mass: 1_067_000.0, radius: 37.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    });
    let bid: i64 = 5001;
    bp.warp_bubble_mgr.add_bubble(eve_destiny::WarpBubble::new(bid, bubble_center, 20_000.0));
    bp.set_mode(1001, MovementMode::new_warp(origin, dest, 3.0)).unwrap();
    if let MovementMode::Warp { destination, .. } = bp.balls[&1001].mode {
        assert_eq!(destination, dest, "set_mode must not prematurely mutate warp destination");
    } else {
        panic!("Expected Warp mode");
    }
    bp.warp_bubble_mgr.remove_bubble(bid);
    for _ in 0..10 {
        bp.step(1.0);
    }
    if let MovementMode::Warp { destination, .. } = bp.balls[&1001].mode {
        assert_eq!(destination, dest, "Despawned bubble during alignment must not trap ship");
    }

    // (b) is_emergency_warping disconnect warp is exempt from origin-bubble alignment cancellation,
    //     but is still intercepted by destination drag/catch bubbles.
    bp.warp_bubble_mgr.add_bubble(eve_destiny::WarpBubble::new(5002, origin, 20_000.0));
    bp.warp_bubble_mgr.add_bubble(eve_destiny::WarpBubble::new(5003, bubble_center, 20_000.0));
    let mut cs_emerg = eve_destiny::CombatState::default();
    cs_emerg.is_emergency_warping = true;
    bp.combat_states.insert(1002, cs_emerg);
    bp.add_ball(Ball {
        id: 1002, type_id: 587, position: origin, velocity: DVec3::ZERO,
        max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0, radius: 37.0,
        speed_fraction: 0.0, mode: MovementMode::new_warp(origin, dest, 3.0),
    });
    for _ in 0..5 { bp.step(1.0); }
    let MovementMode::Warp { destination, phase, .. } = bp.balls[&1002].mode else {
        panic!("Expected emergency warp to enter warp out of origin bubble");
    };
    assert_ne!(phase, eve_destiny::WarpPhase::Aligning, "Emergency warp must not be aborted by origin bubble");
    assert!((destination.x - 99_980_000.0).abs() < 1e-3, "Emergency warp must still be caught by destination bubble");

    // (c) Newly spawned group 31 shuttle warping toward a bubbled destination is exempt
    bp.ball_group_ids.insert(1003, 31);
    bp.add_ball(Ball {
        id: 1003, type_id: 672, position: origin, velocity: DVec3::ZERO,
        max_velocity: 500.0, inertia: 1.0, mass: 1_000_000.0, radius: 25.0,
        speed_fraction: 0.0, mode: MovementMode::new_warp(origin, dest, 5.0),
    });
    for _ in 0..5 {
        bp.step(1.0);
    }
    if let MovementMode::Warp { destination, .. } = bp.balls[&1003].mode {
        assert_eq!(destination, dest, "Group 31 shuttle must be exempt from warp bubbles");
    }
}

#[tokio::test]
async fn test_mid_cycle_overload_and_burnout_ship_fitting_sync() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 95_011;
    let ship_id: i64 = 96_011;
    let mwd_id: i64 = 97_011;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
    give_item(&ctx, mwd_id, 439, char_id, ship_id, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let sys = ctx.systems.get_or_spawn(jita_sys);
    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    cs.capacitor = 100_000.0;
    cs.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: DVec3::new(50_000_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0,
            radius: 37.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    ).await.unwrap();

    // Activate MWD (non-overloaded)
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id), PyRep::String("online".into())])
        .await
        .expect("Activate MWD");
    let fit_before = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(fit_before.modules[&mwd_id].state, eve_dogma::ModuleState::Active);

    // Mid-cycle Overload must defer ShipFittingEngine state until cycle boundary
    rpc(&ctx, &mut sess, "dogmaIM", "Overload", vec![PyRep::Integer(mwd_id)])
        .await
        .expect("Overload mid-cycle");
    let fit_mid = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(
        fit_mid.modules[&mwd_id].state,
        eve_dogma::ModuleState::Active,
        "Mid-cycle Overload must not immediately mutate cached ShipFittingEngine state"
    );

    // Advance past 10s MWD cycle boundary -> SyncOverloadState updates ShipFittingEngine to Overloaded
    for _ in 0..11 {
        sys.step_manual(1.0).await.unwrap();
    }
    let fit_after = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(
        fit_after.modules[&mwd_id].state,
        eve_dogma::ModuleState::Overloaded,
        "Cycle boundary must transition cached ShipFittingEngine state to Overloaded"
    );
}

#[tokio::test]
async fn test_warp_to_within_min_range_sub_150km_rejected() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 95_012;
    let ship_id: i64 = 96_012;
    let target_id: i64 = 1_000_000_098_012;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
    let sys = ctx.systems.get_or_spawn(jita_sys);
    let origin = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    let target_pos = DVec3::new(50_000_250_000.0, 0.0, 0.0); // 250 km away (Astrahus radius 47 km -> 203 km landing dist)

    let mut cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    cs.capacitor = 100_000.0;
    cs.max_capacitor = 100_000.0;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id, type_id: 587, position: origin,
            velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0, mass: 1_067_000.0,
            radius: 37.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    ).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = make_structure(target_id, 35_832, 98_000_001, jita_sys, "Target Astrahus");
        st.position = target_pos.to_array();
        snap.structures.insert(target_id, st);
    });

    // "Warp to Within 100 km" on a 180 km target -> 80 km effective warp < 150 km -> must reject
    let err = rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        vec![
            PyRep::String("item".into()),
            PyRep::Integer(target_id),
            PyRep::Real(100_000.0),
        ],
    )
    .await
    .expect_err("Warp to Within 100 km on 180 km target must be rejected");
    assert_eq!(err.code, "WarpDistanceTooShort");

    // "Warp to Within 20 km" on a 180 km target -> 160 km effective warp >= 150 km -> must succeed
    rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        vec![
            PyRep::String("item".into()),
            PyRep::Integer(target_id),
            PyRep::Real(20_000.0),
        ],
    )
    .await
    .expect("Warp to Within 20 km on 180 km target must succeed");
}

#[tokio::test]
async fn test_structure_docking_advances_waypoints_and_uses_sde_radius_fallback() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_004_759;
    let char_id: i64 = 95_013;
    let ship_id: i64 = 96_013;
    let structure_id: i64 = 1_000_000_099_013;
    let astrahus_type_id: u32 = 35_832;
    let sde_radius = ctx
        .sde
        .types
        .get(&astrahus_type_id)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .expect("Astrahus SDE radius must be > 0");

    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
    let corp_id = sess.corporation_id.unwrap_or(1_000_009);
    let structure_pos = DVec3::new(50_000_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = make_structure(structure_id, astrahus_type_id, corp_id, jita_sys, "Test Astrahus");
        st.position = structure_pos.to_array();
        snap.structures.insert(structure_id, st);
    });

    // Set waypoints [structure_id, 30_004_759]
    rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        vec![
            PyRep::String("waypoints".into()),
            PyRep::List(vec![
                PyRep::Integer(structure_id),
                PyRep::Integer(jita_sys as i64),
            ]),
        ],
    )
    .await
    .expect("SaveCharacterSetting waypoints");

    // Spawn ship at sde_radius + 2_000 m (outside 2_500 m if radius were 0.0, inside sde_radius + 2_500 m)
    // Intentionally do NOT spawn a Ballpark ball for structure_id to test SDE radius fallback.
    let sys = ctx.systems.get_or_spawn(jita_sys);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(structure_pos.x + sde_radius + 2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    )
    .await
    .unwrap();

    rpc(
        &ctx,
        &mut sess,
        "structureDocking",
        "Dock",
        vec![PyRep::Integer(structure_id)],
    )
    .await
    .expect("structureDocking.Dock must succeed using SDE radius fallback");

    let settings = rpc(&ctx, &mut sess, "charMgr", "GetCharacterSettings", vec![])
        .await
        .expect("GetCharacterSettings after structureDocking.Dock");
    assert_eq!(
        settings.dict_get("waypoints"),
        Some(&PyRep::List(vec![PyRep::Integer(jita_sys as i64)])),
        "structureDocking.Dock must advance waypoints on arrival"
    );
}

#[tokio::test]
async fn test_system_actor_post_entry_bubble_does_not_yank_active_warp() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_000_142;
    let char_id: i64 = 900_991;
    let ship_id: i64 = 100_991;
    let sys = ctx.systems.get_or_spawn(jita_sys);

    let origin = DVec3::ZERO;
    let dest = DVec3::new(149_597_870_700.0, 0.0, 0.0); // 1 AU
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: origin,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::new_warp(origin, dest, 3.0),
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    )
    .await
    .unwrap();

    // Step until ship transitions past WarpPhase::Aligning into active warp
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, mid_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(mid_ball.mode, MovementMode::Warp { phase, .. } if phase != eve_destiny::WarpPhase::Aligning),
        "Ship should have exited Aligning into active warp, got {:?}",
        mid_ball.mode
    );

    // Launch a warp disruption bubble at the destination AFTER warp entry
    sys.add_warp_bubble(88_001, dest, 20_000.0).await.unwrap();

    // Step through remaining warp ticks; SystemActor must NOT yank ship out of active warp
    for _ in 0..45 {
        sys.step_manual(1.0).await.unwrap();
    }
    let (_, final_ball) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        final_ball.position.distance(dest) <= 3_000.0,
        "Post-entry bubble must not yank active warp; ship landed at {:?} (dist {:.1} m from dest)",
        final_ball.position,
        final_ball.position.distance(dest)
    );
}

#[tokio::test]
async fn test_warp_vector_rejects_mid_warp_and_abyssal_and_cmd_dock_checks_upwell_tackle() {
    let ctx = Arc::new(build_ctx());
    let jita_sys: u32 = 30_000_142;
    let char_id: i64 = 900_992;
    let ship_id: i64 = 100_992;
    let mut sess = seed_pilot(&ctx, char_id, ship_id, 587, jita_sys, false);
    let corp_id = sess.corporation_id.unwrap_or(1_000_009);
    let sys = ctx.systems.get_or_spawn(jita_sys);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        ctx.build_ship_slim_meta(char_id, 587),
    )
    .await
    .unwrap();

    // Seed a Warp Vector Data item (77825, group 4599) in ship cargo
    let vec_item_id: i64 = 777_825;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            vec_item_id,
            eve_db::InventoryItemRecord {
                item_id: vec_item_id,
                type_id: 77825,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. Mid-warp trajectory redirection via warpVectorMgr.UseVector must be rejected
    sys.set_movement_mode(
        ship_id,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(10_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 10_000_000.0,
            elapsed_warp_time: 1.0,
            phase: eve_destiny::WarpPhase::Cruising,
        },
    )
    .await
    .unwrap();
    let err_mid = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
        .await
        .unwrap_err();
    assert_eq!(err_mid.code, "ShipAlreadyInWarp");

    // 2. Abyssal Deadspace (32_000_001) must reject warpVectorMgr.UseVector
    sys.force_stop_warp(ship_id).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = 32_000_001;
        }
    });
    sess.solar_system_id = Some(32_000_001);
    let err_abyss = rpc(&ctx, &mut sess, "warpVectorMgr", "UseVector", vec![PyRep::Integer(vec_item_id)])
        .await
        .unwrap_err();
    assert_eq!(err_abyss.code, "CannotWarpInAbyssalSpace");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = jita_sys;
        }
    });
    sess.solar_system_id = Some(jita_sys);

    // 3. beyonce.CmdDock must reject scrambled/bubbled ships at Upwell structures
    let structure_id: i64 = 1_000_000_999_002;
    let structure_pos = DVec3::new(50_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = make_structure(structure_id, 35832, corp_id, jita_sys, "Test Citadel");
        st.position = structure_pos.to_array();
        snap.structures.insert(structure_id, st);
    });
    sys.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
    let _ = sys.set_ball_position(ship_id, structure_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    sys.add_warp_bubble(88_002, structure_pos, 20_000.0).await.unwrap();

    let err_dock = rpc(&ctx, &mut sess, "beyonce", "CmdDock", vec![PyRep::Integer(structure_id)])
        .await
        .unwrap_err();
    assert_eq!(err_dock.code, "ShipWarpScrambled");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod tactical_cyno_lighting {
    use super::*;

//  Dedicated integration tests for remediated tactical modules parity rows (Part 1):
//  - `feat:modules.cyno-lighting-rules`
//  - `feat:modules.assault-damage-control`


use crate::sovereignty_equinox_gameplay::{
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, set_pilot_location,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::services::structure_acl_defense_rules::TYPE_MOBILE_CYNO_INHIBITOR;
use glam::DVec3;
use std::collections::HashMap;

fn err_code<T: std::fmt::Debug>(res: Result<T, eve_proto::BridgeRpcError>) -> String {
    res.unwrap_err().code
}

#[tokio::test]
async fn test_cyno_lighting_rules_parity_remediation() {
    // parity: feat:modules.cyno-lighting-rules
    eve_testkit::attest(&["feat:modules.cyno-lighting-rules"]);

    let db_path = temp_sqlite_path("parity_cyno_lighting_rules");
    let ctx = open_sqlite_ctx(&db_path, true);
    let high_sys = 30_000_142u32; // Jita (0.95 highsec)
    let low_sys = 30_002_718u32;  // Rancer (0.23 lowsec)
    let wh_sys = 31_000_001u32;   // Wormhole space

    let mut s_recon = register_online_pilot(
        &ctx,
        88_101,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        high_sys,
        PILOT_SHIP,
        11_957, // Falcon (Force Recon)
        DVec3::ZERO,
        0,
    )
    .await;

    let cyno_mod_id = 988_701i64;
    let ozone_id = 988_702i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cyno_mod_id,
            InventoryItemRecord {
                item_id: cyno_mod_id,
                type_id: 21_096, // Cynosural Field Generator I
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ozone_id,
            InventoryItemRecord {
                item_id: ozone_id,
                type_id: 16_273, // Liquid Ozone
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                ..Default::default()
            },
        );
        snap.skills.entry(PILOT).or_default().insert(
            21_603, // Cynosural Field Theory V
            TrainedSkillRecord {
                type_id: 21_603,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    // 1. Hull restriction: normal cynos fit only on Force Recons and Black Ops.
    let mut cyno_skills = HashMap::new();
    cyno_skills.insert(21_603, 5); // Cynosural Field Theory V

    let mut fit_falcon = eve_dogma::ShipFittingEngine::from_sde(1, 11_957, &ctx.sde);
    fit_falcon.character_skills = cyno_skills.clone();
    assert!(
        fit_falcon.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_ok(),
        "normal cyno must fit on Force Recon (Falcon 11957)"
    );

    let mut fit_redeemer = eve_dogma::ShipFittingEngine::from_sde(1, 22_428, &ctx.sde);
    fit_redeemer.character_skills = cyno_skills.clone();
    assert!(
        fit_redeemer.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_ok(),
        "normal cyno must fit on Black Ops (Redeemer 22428)"
    );

    let mut fit_rifter = eve_dogma::ShipFittingEngine::from_sde(1, 587, &ctx.sde);
    fit_rifter.character_skills = cyno_skills.clone();
    assert!(
        fit_rifter.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_err(),
        "normal cyno must not fit on Frigate (Rifter 587)"
    );

    let mut fit_megathron = eve_dogma::ShipFittingEngine::from_sde(1, 641, &ctx.sde);
    fit_megathron.character_skills = cyno_skills.clone();
    assert!(
        fit_megathron.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_err(),
        "normal cyno must not fit on Battleship (Megathron 641)"
    );

    let mut fit_rook = eve_dogma::ShipFittingEngine::from_sde(1, 11_959, &ctx.sde);
    fit_rook.character_skills = cyno_skills.clone();
    assert!(
        fit_rook.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_err(),
        "normal cyno must not fit on Combat Recon (Rook 11959)"
    );

    // 2. High-sec refusal: refused in high-sec.
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CannotLightCynoInSystem"
    );

    // Move to low-sec.
    let _ = ctx.systems.get_or_spawn(high_sys).remove_ball(PILOT_SHIP).await;
    set_pilot_location(&ctx, &mut s_recon, low_sys, None);
    place_ship_ball(&ctx, low_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;

    // 3. Activation requires fleet membership.
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "MustBeInFleet"
    );

    // Create fleet.
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            77_101,
            FleetRecord {
                fleet_id: 77_101,
                boss_id: PILOT,
                members: vec![PILOT],
                ..Default::default()
            },
        );
    });

    // 4. Activation requires speed below 500 m/s.
    let sys_low = ctx.systems.get_or_spawn(low_sys);
    let _ = sys_low.remove_ball(PILOT_SHIP).await;
    let cs_moving = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 11_957);
    let moving_ball = Ball {
        id: PILOT_SHIP,
        type_id: 11_957,
        position: DVec3::ZERO,
        velocity: DVec3::new(600.0, 0.0, 0.0), // 600 m/s >= 500 m/s
        max_velocity: 800.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 100.0,
        speed_fraction: 0.75,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    };
    sys_low
        .spawn_ball_with_combat(moving_ball, cs_moving)
        .await
        .expect("spawn fast moving ball");

    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "ShipMovingTooFast"
    );

    // Restore ship ball to stationary (0 m/s < 500 m/s).
    place_ship_ball(&ctx, low_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;

    // 5. Refused within range of Mobile Cynosural Inhibitor.
    let inhib_id = 988_705i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            inhib_id,
            InventoryItemRecord {
                item_id: inhib_id,
                type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id: PILOT,
                location_id: low_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(inhib_id, [20_000.0, 0.0, 0.0]); // 20 km away
    });

    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CynoInhibited"
    );

    // Remove inhibitor.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&inhib_id);
    });

    // 6. Refused in jammed systems.
    let jammer_id = 988_706i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            jammer_id,
            InventoryItemRecord {
                item_id: jammer_id,
                type_id: 33_700, // Cynosural Jammer item
                owner_id: PILOT,
                location_id: low_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CynoJammed"
    );

    // Clear jammer item.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&jammer_id);
    });

    // 7. Successful activation in low-sec: consumes Liquid Ozone, immobilizes ship.
    rpc_call(
        &ctx,
        &mut s_recon,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
    )
    .await
    .expect("light normal cyno on Force Recon in low-sec");

    let ozone_left = ctx.db.with_snapshot(|s| s.items.get(&ozone_id).unwrap().quantity);
    assert_eq!(ozone_left, 450, "Force Recon consumes 50 units of Liquid Ozone");
    assert_eq!(
        err_code(rpc_call(&ctx, &mut s_recon, "ship", "Eject", &[]).await),
        "CynosuralFieldActive"
    );

    // 8. Wormhole space: cyno can be lit, but jump drive lock rejected.
    let _ = ctx.systems.get_or_spawn(low_sys).remove_ball(PILOT_SHIP).await;
    eve_server::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(
        &ctx, PILOT, PILOT_SHIP,
    );
    ctx.advance_secs(601);
    set_pilot_location(&ctx, &mut s_recon, wh_sys, None);
    place_ship_ball(&ctx, wh_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;

    rpc_call(
        &ctx,
        &mut s_recon,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
    )
    .await
    .expect("cyno can be lit in wormhole space");

    let jumper_cid = 90_000_102i64;
    let jumper_ship = 100_000_102i64;
    let mut s_jumper = register_online_pilot(
        &ctx,
        88_102,
        "jumper",
        "sec",
        jumper_cid,
        CORP,
        Some(ALLIANCE),
        low_sys,
        jumper_ship,
        22_428, // Redeemer (Black Ops)
        DVec3::ZERO,
        0,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets
            .get_mut(&77_101)
            .unwrap()
            .members
            .push(jumper_cid);
    });

    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_jumper,
                "beyonce",
                "CmdJumpToFleetModuleBeacon",
                &[PyRep::Integer(PILOT), PyRep::Integer(wh_sys as i64), PyRep::Integer(0)],
            )
            .await
        ),
        "JumpDriveForbiddenInWormholeSpace"
    );
}

#[tokio::test]
async fn test_assault_damage_control_parity_remediation() {
    // parity: feat:modules.assault-damage-control
    eve_testkit::attest(&["feat:modules.assault-damage-control"]);

    let db_path = temp_sqlite_path("parity_adc_remediation");
    let ctx = open_sqlite_ctx(&db_path, true);
    let low_sys = 30_002_718u32;
    let pilot_id = 90_000_201i64;
    let harpy_id = 100_000_201i64;
    let adc_id = 988_801i64;

    let mut sess = register_online_pilot(
        &ctx,
        88_201,
        "demo",
        "demo-secret",
        pilot_id,
        CORP,
        Some(ALLIANCE),
        low_sys,
        harpy_id,
        11_381, // Harpy (Assault Frigate, group 324)
        DVec3::ZERO,
        0,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            adc_id,
            InventoryItemRecord {
                item_id: adc_id,
                type_id: 47_257, // Assault Damage Control II (group 60, effect 7012)
                owner_id: pilot_id,
                location_id: harpy_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(harpy_id);

    // 1. Activation on non-AF/non-HAC hull is rejected.
    let rifter_id = 100_000_202i64;
    let mut sess_rifter = register_online_pilot(
        &ctx,
        88_202,
        "demo2",
        "demo2-secret",
        90_000_202,
        CORP,
        Some(ALLIANCE),
        low_sys,
        rifter_id,
        587, // Rifter (Frigate, group 25)
        DVec3::new(1_000.0, 0.0, 0.0),
        0,
    )
    .await;
    let adc_rifter_id = 988_802i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            adc_rifter_id,
            InventoryItemRecord {
                item_id: adc_rifter_id,
                type_id: 47_257,
                owner_id: 90_000_202,
                location_id: rifter_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(rifter_id);

    assert_eq!(
        err_code(
            ctx.dispatch_rpc(
                &mut sess_rifter,
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(adc_rifter_id),
                    PyRep::String("moduleBonusAssaultDamageControl".into()),
                    PyRep::None,
                    PyRep::Integer(0),
                ],
                &HashMap::new(),
                None,
            )
            .await
        ),
        "InvalidShipGroup"
    );

    // 2. Activation on Harpy triggers invulnerability burst (0.25 resonances = 75% resistance).
    let sys = ctx.systems.get_or_spawn(low_sys);
    let cs_before = sys.get_combat_state(harpy_id).await.unwrap().unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(adc_id),
            PyRep::String("moduleBonusAssaultDamageControl".into()),
            PyRep::None,
            PyRep::Integer(0),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Activate Assault Damage Control II on Harpy");

    let cs_active = sys.get_combat_state(harpy_id).await.unwrap().unwrap();
    assert_eq!(cs_active.shield_resonance, [0.25; 4]);
    assert_eq!(cs_active.armor_resonance, [0.25; 4]);
    assert_eq!(cs_active.structure_resonance, [0.25; 4]);

    // 3. Short duration: step forward through cycle duration (15s for ADC II).
    for _ in 0..16 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }

    let cs_after = sys.get_combat_state(harpy_id).await.unwrap().unwrap();
    assert_eq!(cs_after.shield_resonance, cs_before.shield_resonance);
    assert_eq!(cs_after.armor_resonance, cs_before.armor_resonance);
    assert_eq!(cs_after.structure_resonance, cs_before.structure_resonance);

    // 4. Long cooldown: reactivation during 150s delay must fail with ModuleReactivationDelay.
    let react_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(adc_id),
                PyRep::String("moduleBonusAssaultDamageControl".into()),
                PyRep::None,
                PyRep::Integer(0),
            ],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(
        react_err.expect_err("ADC II enforces 150s reactivation delay").code,
        "ModuleReactivationDelay"
    );

    // 5. Cooldown expiration allows reactivation.
    ctx.advance_secs(160);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(adc_id),
            PyRep::String("moduleBonusAssaultDamageControl".into()),
            PyRep::None,
            PyRep::Integer(0),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Reactivate ADC II after cooldown expires");
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod tactical_command_bursts {
    use super::*;

//  Dedicated integration tests for remediated tactical modules parity rows (Part 2):
//  - `feat:modules.command-bursts`
//  - `feat:modules.armor-repairer`


use crate::sovereignty_equinox_gameplay::{
    open_sqlite_ctx, register_online_pilot, temp_sqlite_path, ALLIANCE, CORP,
};
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
};
use eve_destiny::TargetLock;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

fn err_code<T: std::fmt::Debug>(res: Result<T, eve_proto::BridgeRpcError>) -> String {
    res.unwrap_err().code
}

fn extract_list_ints(rep: &PyRep, key: &str) -> Vec<i64> {
    match rep {
        PyRep::KeyVal(entries) => {
            if let Some(PyRep::List(items)) = entries.get(key) {
                return items
                    .iter()
                    .filter_map(|it| match it {
                        PyRep::Integer(i) => Some(*i),
                        _ => None,
                    })
                    .collect();
            }
        }
        PyRep::Dict(entries) => {
            for (k, v) in entries {
                if let PyRep::String(s) = k {
                    if s == key {
                        if let PyRep::List(items) = v {
                            return items
                                .iter()
                                .filter_map(|it| match it {
                                    PyRep::Integer(i) => Some(*i),
                                    _ => None,
                                })
                                .collect();
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Vec::new()
}

fn extract_bool_field(rep: &PyRep, key: &str) -> bool {
    match rep {
        PyRep::KeyVal(entries) => {
            if let Some(PyRep::Bool(b)) = entries.get(key) {
                return *b;
            }
        }
        PyRep::Dict(entries) => {
            for (k, v) in entries {
                if let PyRep::String(s) = k {
                    if s == key {
                        if let PyRep::Bool(b) = v {
                            return *b;
                        }
                    }
                }
            }
        }
        _ => {}
    }
    false
}

#[tokio::test]
async fn test_command_bursts_parity_remediation() {
    // parity: feat:modules.command-bursts
    eve_testkit::attest(&["feat:modules.command-bursts"]);

    let db_path = temp_sqlite_path("parity_command_bursts_remediation");
    let ctx = open_sqlite_ctx(&db_path, true);
    let low_sys = 30_002_718u32;

    let booster_char = 90_000_301i64;
    let booster_ship = 100_000_301i64;
    let fleet_near_char = 90_000_302i64;
    let fleet_near_ship = 100_000_302i64;
    let fleet_far_char = 90_000_303i64;
    let fleet_far_ship = 100_000_303i64;
    let neutral_char = 90_000_304i64;
    let neutral_ship = 100_000_304i64;

    let mut _s_booster = register_online_pilot(
        &ctx, 88_301, "booster", "sec", booster_char, CORP, Some(ALLIANCE),
        low_sys, booster_ship, 22_444, // Eos (Command Ship, group 540)
        DVec3::ZERO, 0,
    ).await;

    let _s_near = register_online_pilot(
        &ctx, 88_302, "near", "sec", fleet_near_char, CORP, Some(ALLIANCE),
        low_sys, fleet_near_ship, 624, // Maller (5 km away, within AoE range)
        DVec3::new(5_000.0, 0.0, 0.0), 0,
    ).await;

    let mut _s_far = register_online_pilot(
        &ctx, 88_303, "far", "sec", fleet_far_char, CORP, Some(ALLIANCE),
        low_sys, fleet_far_ship, 624, // Maller (150 km away, out of range)
        DVec3::new(150_000.0, 0.0, 0.0), 0,
    ).await;

    let mut _s_neutral = register_online_pilot(
        &ctx, 88_304, "neutral", "sec", neutral_char, CORP, Some(ALLIANCE),
        low_sys, neutral_ship, 624, // Maller (5 km away, but not in fleet)
        DVec3::new(5_000.0, 0.0, 0.0), 0,
    ).await;

    let burst_mod_id = 988_901i64;
    let charge_item_id = 988_902i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            77_301,
            FleetRecord {
                fleet_id: 77_301,
                boss_id: booster_char,
                members: vec![booster_char, fleet_near_char, fleet_far_char],
                ..Default::default()
            },
        );
        snap.items.insert(
            burst_mod_id,
            InventoryItemRecord {
                item_id: burst_mod_id,
                type_id: 42_529, // Shield Command Burst I (group 1770)
                owner_id: booster_char,
                location_id: booster_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_item_id,
            InventoryItemRecord {
                item_id: charge_item_id,
                type_id: 42_695, // Shield Harmonizing Charge (group 1769)
                owner_id: booster_char,
                location_id: booster_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        let b_skills = snap.skills.entry(booster_char).or_default();
        b_skills.insert(3350, TrainedSkillRecord { type_id: 3350, level: 5, skill_points: 256_000.0 });
        b_skills.insert(3351, TrainedSkillRecord { type_id: 3351, level: 5, skill_points: 256_000.0 });
        b_skills.insert(11574, TrainedSkillRecord { type_id: 11574, level: 5, skill_points: 256_000.0 });
    });
    ctx.invalidate_ship_fitting(booster_ship);

    let sys_h = ctx.systems.get_or_spawn(low_sys);
    let fit = ctx.get_or_rebuild_ship_fitting(booster_char, booster_ship).unwrap();
    let mod_state = eve_dogma::DogmaItemState::from_sde(burst_mod_id, 42_529, &ctx.sde);
    let mod_type = ctx.sde.types.get(&42_529).unwrap();

    let res = eve_server::services::tactical_modules::activate_command_burst(
        &ctx, &sys_h, booster_char, booster_ship, low_sys,
        burst_mod_id, mod_type, &mod_state, Some(42_695), &fit,
    )
    .await
    .expect("activate Shield Command Burst");

    assert!(extract_bool_field(&res, "active"), "Burst must be active");
    let affected_ships = extract_list_ints(&res, "affectedShipIDs");
    assert!(affected_ships.contains(&booster_ship), "Booster must receive its own AoE burst");
    assert!(affected_ships.contains(&fleet_near_ship), "Fleet member within AoE range must receive burst");
    assert!(!affected_ships.contains(&fleet_far_ship), "Fleet member beyond AoE range must NOT receive burst");
    assert!(!affected_ships.contains(&neutral_ship), "Non-fleet pilot must NOT receive fleet burst");

    // Charge consumption: 1 charge consumed (5 -> 4).
    let rem_charges = ctx.db.with_snapshot(|s| s.items.get(&charge_item_id).unwrap().quantity);
    assert_eq!(rem_charges, 4, "Activating command burst consumes 1 loaded charge");

    // Notifications: near fleetmate receives OnDbuffUpdated.
    let near_notes = ctx.drain_notifications_for_char(fleet_near_char);
    assert!(
        near_notes.iter().any(|n| n.event == "OnDbuffUpdated"),
        "Affected fleet member must receive OnDbuffUpdated notification: {near_notes:?}"
    );
}

#[tokio::test]
async fn test_remote_armor_repairer_parity_remediation() {
    // parity: feat:modules.armor-repairer
    eve_testkit::attest(&["feat:modules.armor-repairer"]);

    let db_path = temp_sqlite_path("parity_rar_remediation");
    let ctx = open_sqlite_ctx(&db_path, true);
    let low_sys = 30_002_718u32;

    let logi_char = 90_000_401i64;
    let logi_ship = 100_000_401i64;
    let ally_char = 90_000_402i64;
    let ally_ship = 100_000_402i64;
    let rar_id = 988_951i64;

    let mut sess_logi = register_online_pilot(
        &ctx, 88_401, "logi", "sec", logi_char, CORP, Some(ALLIANCE),
        low_sys, logi_ship, 11_989, // Oneiros (Logistics Cruiser, group 900)
        DVec3::ZERO, 0,
    ).await;

    let mut _sess_ally = register_online_pilot(
        &ctx, 88_402, "ally", "sec", ally_char, CORP, Some(ALLIANCE),
        low_sys, ally_ship, 624, // Maller (friendly ally ship, 5 km away)
        DVec3::new(5_000.0, 0.0, 0.0), 0,
    ).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rar_id,
            InventoryItemRecord {
                item_id: rar_id,
                type_id: 11_357, // Medium Remote Armor Repairer I (group 325)
                owner_id: logi_char,
                location_id: logi_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let l_skills = snap.skills.entry(logi_char).or_default();
        l_skills.insert(16069, TrainedSkillRecord { type_id: 16069, level: 5, skill_points: 256_000.0 }); // Remote Armor Repair Systems V
        snap.fleets.insert(
            77_401,
            FleetRecord {
                fleet_id: 77_401,
                boss_id: logi_char,
                members: vec![logi_char, ally_char],
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(logi_ship);

    let sys_h = ctx.systems.get_or_spawn(low_sys);

    // Set damaged armor on ally ship.
    let mut ally_cs = ctx.build_ship_combat_state(ally_char, ally_ship, 624);
    ally_cs.max_armor_hp = 1_500.0;
    ally_cs.armor_hp = 300.0;
    sys_h.set_combat_state(ally_ship, ally_cs).await.unwrap();

    let kw = HashMap::new();

    // 1. Activating without target fails with TargetRequired.
    let no_target_err = ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::None, PyRep::Integer(0)],
        &kw, None,
    ).await;
    assert_eq!(err_code(no_target_err), "TargetRequired");

    // 2. Activating on self fails with CannotActivateOnSelf.
    let self_err = ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(logi_ship), PyRep::Integer(0)],
        &kw, None,
    ).await;
    assert_eq!(err_code(self_err), "CannotActivateOnSelf");

    // 3. Activating on friendly target without lock fails with TargetNotLocked.
    let unlock_err = ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)],
        &kw, None,
    ).await;
    assert_eq!(err_code(unlock_err), "TargetNotLocked");

    // Lock friendly target.
    sys_h.add_target_lock(
        logi_ship,
        TargetLock {
            target_id: ally_ship,
            lock_time_ms: 0.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(100_000.0),
        },
    ).await.unwrap();

    // 4. Insufficient capacitor fails with NotEnoughCapacitor.
    sys_h.set_capacitor(logi_ship, 5.0).await.unwrap();
    let cap_err = ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)],
        &kw, None,
    ).await;
    assert_eq!(err_code(cap_err), "NotEnoughCapacitor");

    // 5. Sufficient capacitor: activation consumes capacitor.
    sys_h.set_capacitor(logi_ship, 500.0).await.unwrap();
    ctx.dispatch_rpc(
        &mut sess_logi, "dogmaIM", None, "Activate",
        &[PyRep::Integer(rar_id), PyRep::String("shipModuleRemoteArmorRepairer".into()), PyRep::Integer(ally_ship), PyRep::Integer(0)],
        &kw, None,
    ).await.expect("Activate Remote Armor Repairer");

    let cap_after_act = sys_h.get_combat_state(logi_ship).await.unwrap().unwrap().capacitor;
    assert!(cap_after_act < 500.0, "Capacitor must be consumed on cycle start: {cap_after_act} < 500.0");

    // 6. Armor is repaired at cycle completion (Medium RAR duration is 6s).
    for _ in 0..6 {
        sys_h.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }

    let ally_armor_repaired = sys_h.get_combat_state(ally_ship).await.unwrap().unwrap().armor_hp;
    assert!(
        ally_armor_repaired > 300.0,
        "Friendly target armor must be repaired after cycle completion (got {ally_armor_repaired} vs initial 300.0)"
    );
    let hp_repaired = ally_armor_repaired - 300.0;
    assert!(
        hp_repaired >= 190.0,
        "Medium RAR I must repair at least 190 HP (got {hp_repaired})"
    );
}

}
