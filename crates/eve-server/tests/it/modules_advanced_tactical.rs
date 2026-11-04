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
        "feat:modules.disruptive-lance",
    ]);
}

pub(crate) mod entosis_link_restrictions_warmup_contest {
    // End-to-End Integration Tests for Tactical, Capital, Command, Fitting & Support Modules (`feat:modules.*`).

    use eve_db::InventoryFlag;
    use eve_destiny::DVec3;
    use eve_proto::PyRep;
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

pub(crate) mod emergency_hull_energizer_overheat_burnout {
    // Integration tests for modules, ships, and overheat closure rows.
    //
    // parity: feat:modules.emergency-hull-energizer, feat:modules.overheat-burnout-state

    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode, TargetLock};
    use eve_dogma::attr_ids;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        ServerContext::with_sde_and_repository(sde, db)
    }

    fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(item_id, InventoryItemRecord {
                item_id, type_id, owner_id, location_id, flag, quantity, singleton,
                version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            });
        });
    }

    fn make_test_ball(id: i64, type_id: u32, position: DVec3, radius: f64) -> Ball {
        Ball {
            id, type_id, position, velocity: DVec3::ZERO, max_velocity: 150.0,
            inertia: 1.0, mass: 10_000_000.0, radius, speed_fraction: 0.0, mode: MovementMode::Stop,
        }
    }

    #[tokio::test]
    async fn test_emergency_hull_energizer_and_overheat_burnout_state() {
        // parity: feat:modules.emergency-hull-energizer, feat:modules.overheat-burnout-state
        eve_testkit::attest(&["feat:modules.emergency-hull-energizer", "feat:modules.overheat-burnout-state"]);

        let ctx = new_ctx();
        let kw = HashMap::new();
        let mut sess = SessionState::new(8801);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let char_id = 90_000_001i64;
        let ship_id = 100_000_101i64;
        let sys_id = 30_000_142u32;
        let sys = ctx.systems.get_or_spawn(sys_id);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.active_ship_id = ship_id;
                ch.station_id = None;
                ch.solar_system_id = sys_id;
            }
        });

        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

        // 1. Capital-only restriction check: Battleship Raven (group 27, type 638) cannot activate Capital EHE
        let raven_id = 100_000_109i64;
        let raven_ehe_id = 881_099i64;
        seed_item(&ctx, raven_id, 638, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, raven_ehe_id, 40715, char_id, raven_id, InventoryFlag::LoSlot0, 1, true);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(raven_ehe_id, true);
            if let Some(ch) = snap.characters.get_mut(&char_id) { ch.active_ship_id = raven_id; }
        });
        sess.ship_id = Some(raven_id);
        ctx.invalidate_ship_fitting(raven_id);
        let raven_ball = make_test_ball(raven_id, 638, DVec3::new(20_000.0, 0.0, 0.0), 100.0);
        let raven_cs = ctx.build_ship_combat_state(char_id, raven_id, 638);
        let raven_meta = ctx.build_ship_slim_meta(char_id, 638);
        let _ = sys.spawn_ball_with_meta(raven_ball, raven_cs, raven_meta).await;

        let non_cap_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(raven_ehe_id), PyRep::String("emergencyHullEnergizer".into())], &kw, None).await.unwrap_err();
        assert_eq!(non_cap_err.code, "InvalidShipGroup", "Capital EHE on Battleship must fail with InvalidShipGroup");

        // 2. Setup Archon (Carrier, group 547, type 23757) with Compact EHE (40715) and 500MN MWD (439)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) { ch.active_ship_id = ship_id; }
        });
        sess.ship_id = Some(ship_id);

        seed_item(&ctx, ship_id, 23757, char_id, sys_id as i64, InventoryFlag::None, 1, true);
        let ehe_mod_id = 881_001i64;
        let mwd_mod_id = 881_002i64;
        // Use Compact EHE variant 40715 (effect 6484) to verify non-T1/T2 variant handling
        seed_item(&ctx, ehe_mod_id, 40715, char_id, ship_id, InventoryFlag::LoSlot0, 1, true);
        seed_item(&ctx, mwd_mod_id, 439, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);

        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(ehe_mod_id, true);
            snap.module_online_states.insert(mwd_mod_id, true);
        });
        ctx.invalidate_ship_fitting(ship_id);

        let ball = make_test_ball(ship_id, 23757, DVec3::new(10_000.0, 0.0, 0.0), 200.0);
        let cs = ctx.build_ship_combat_state(char_id, ship_id, 23757);
        let meta = ctx.build_ship_slim_meta(char_id, 23757);
        let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;

        // Record pre-activation base structure resonance (Archon base: 0.67 across damage types)
        let pre_res = sys.get_combat_state(ship_id).await.unwrap().expect("Ship combat state").structure_resonance;
        assert!(pre_res[0] > 0.50, "Base hull resonance must be unbuffed before EHE");

        // Activate Capital Emergency Hull Energizer (`feat:modules.emergency-hull-energizer`)
        let ehe_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ehe_mod_id), PyRep::String("emergencyHullEnergizer".into())], &kw, None).await.expect("Activate EHE");
        assert_eq!(ehe_rep.dict_get("active").and_then(|v| v.as_bool()), Some(true));

        // Assert cycle duration (e.g. 14-25 seconds for capital EHE variants)
        let duration_s = ehe_rep.dict_get("duration").and_then(|v| v.as_f64()).unwrap_or_else(|| {
            ctx.sde.types.get(&40715).and_then(|t| t.default_attributes.get(&attr_ids::DURATION).copied()).unwrap_or(0.0) / 1000.0
        });
        assert!(duration_s >= 14.0 && duration_s <= 25.0, "EHE duration must be short burst (14-25s), got {duration_s}");

        // Assert that active EHE grants 95% resistance to all hull damage layers (0.05 resonance)
        let active_res = sys.get_combat_state(ship_id).await.unwrap().expect("Active combat state").structure_resonance;
        assert_eq!(active_res, [0.05, 0.05, 0.05, 0.05], "Active EHE must grant 95% hull resistance");

        let (em_res, exp_res, kin_res, therm_res) = ctx.db.with_snapshot(|snap| {
            let tr = snap.transient_item_attributes.get(&ship_id);
            let em = tr.and_then(|s| s.get(&113).copied()).unwrap_or(0.0);
            let exp = tr.and_then(|s| s.get(&111).copied()).unwrap_or(0.0);
            let kin = tr.and_then(|s| s.get(&109).copied()).unwrap_or(0.0);
            let therm = tr.and_then(|s| s.get(&110).copied()).unwrap_or(0.0);
            (em, exp, kin, therm)
        });
        assert_eq!(em_res, 0.05, "Ship EM hull resonance attribute must be 0.05");
        assert_eq!(exp_res, 0.05, "Ship Explosive hull resonance attribute must be 0.05");
        assert_eq!(kin_res, 0.05, "Ship Kinetic hull resonance attribute must be 0.05");
        assert_eq!(therm_res, 0.05, "Ship Thermal hull resonance attribute must be 0.05");

        // Step cycle duration (26 ticks) so EHE completes cycle and burns out
        for _ in 0..26 { sys.tick().await.unwrap(); }

        // Verify EHE burnout state: burned out (damage == max HP), offlined, and deactivated
        let (is_online, is_active, dmg, max_hp) = ctx.db.with_snapshot(|snap| {
            let on = snap.module_online_states.get(&ehe_mod_id).copied().unwrap_or(false);
            let act = snap.module_active_states.contains(&ehe_mod_id);
            let it = snap.items.get(&ehe_mod_id);
            let cur_d = it.and_then(|i| i.dynamic_attributes.get(&attr_ids::DAMAGE).copied()).unwrap_or(0.0);
            let m_hp = it.and_then(|i| i.dynamic_attributes.get(&attr_ids::HP_STRUCT).copied()).unwrap_or(40.0);
            (on, act, cur_d, m_hp)
        });
        assert!(!is_online, "Burned-out EHE must be offlined automatically");
        assert!(!is_active, "Burned-out EHE must be deactivated");
        assert!(dmg >= max_hp - 1e-4, "Burned-out EHE damage ({dmg}) must equal max HP ({max_hp})");

        // Structure resonance must be restored after EHE burns out
        let post_res = sys.get_combat_state(ship_id).await.unwrap().expect("Post combat state").structure_resonance;
        assert_eq!(post_res, pre_res, "Resistances must revert after EHE deactivates");

        // Cannot reactivate burned-out EHE in space
        let react_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ehe_mod_id), PyRep::String("emergencyHullEnergizer".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
        assert_eq!(react_err.code, "ModuleBurnedOut");

        // Fully burned-out module refuses nanite repair in space
        let rep_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "RepairModule", &[PyRep::Integer(ship_id), PyRep::Integer(ehe_mod_id)], &kw, None).await.unwrap_err();
        assert_eq!(rep_err.code, "ModuleBurnedOut");

        // Cannot refit damaged or burned-out EHE: moving to cargo or fitting from cargo is rejected
        let unrefit_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(ehe_mod_id), PyRep::Integer(ship_id), PyRep::Integer(0)], &kw, None).await.unwrap_err();
        assert_eq!(unrefit_err.code, "CannotRefitDamagedModule", "Unfitting damaged EHE must fail");

        let damaged_cargo_ehe = 881_098i64;
        seed_item(&ctx, damaged_cargo_ehe, 40715, char_id, ship_id, InventoryFlag::Cargo, 1, true);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&damaged_cargo_ehe) {
                it.dynamic_attributes.insert(attr_ids::DAMAGE, 10.0);
                it.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
            }
        });
        let fit_damaged_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(damaged_cargo_ehe), PyRep::Integer(ship_id), PyRep::Integer(12)], &kw, None).await.unwrap_err();
        assert_eq!(fit_damaged_err.code, "CannotRefitDamagedModule", "Fitting damaged EHE must fail");

        // 3. Heat-driven module burnout on overloaded module (`feat:modules.overheat-burnout-state`)
        // Pre-accumulate heat damage to 39.0 / 40.0 HP on the 500MN MWD and pre-heat medium rack (1176 = 0.99)
        // with a deterministic mechanic RNG seed so a probabilistic heat hit pushes damage to 40.0 HP burnout.
        let burnout_seed = (1u64..100_000)
            .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() < 0.02)
            .expect("deterministic heat hit seed");
        ctx.set_mechanic_rng_seed(Some(burnout_seed));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(m_item) = snap.items.get_mut(&mwd_mod_id) {
                m_item.dynamic_attributes.insert(attr_ids::DAMAGE, 39.0);
                m_item.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
            }
            if let Some(s_item) = snap.items.get_mut(&ship_id) {
                s_item.dynamic_attributes.insert(1176, 0.99);
            }
        });

        let ovl_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Overload", &[PyRep::Integer(mwd_mod_id)], &kw, None).await.expect("Overload MWD");
        assert_eq!(ovl_rep.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));

        // Activate MWD in overload mode
        let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_mod_id), PyRep::String("propulsion".into())], &kw, None).await.expect("Activate overloaded MWD");

        // Step cycle ticks so probabilistic heat damage rolls and burns out the module at 40 HP
        for _ in 0..15 { sys.tick().await.unwrap(); }

        // Verify heat-driven burnout: module immediately deactivates, goes offline, damage == max HP
        let (mwd_online, mwd_active, mwd_dmg, mwd_max_hp) = ctx.db.with_snapshot(|snap| {
            let on = snap.module_online_states.get(&mwd_mod_id).copied().unwrap_or(false);
            let act = snap.module_active_states.contains(&mwd_mod_id);
            let it = snap.items.get(&mwd_mod_id);
            let cur_d = it.and_then(|i| i.dynamic_attributes.get(&attr_ids::DAMAGE).copied()).unwrap_or(0.0);
            let m_hp = it.and_then(|i| i.dynamic_attributes.get(&attr_ids::HP_STRUCT).copied()).unwrap_or(40.0);
            (on, act, cur_d, m_hp)
        });
        assert!(!mwd_online, "Heat-burned module must be taken offline");
        assert!(!mwd_active, "Heat-burned module must be deactivated");
        assert!(mwd_dmg >= mwd_max_hp - 1e-4, "Heat-burned module damage ({mwd_dmg}) must equal max HP ({mwd_max_hp})");

        // Reactivation of heat-burned module in space must fail with ModuleBurnedOut
        let mwd_react_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_mod_id), PyRep::String("propulsion".into())], &kw, None).await.unwrap_err();
        assert_eq!(mwd_react_err.code, "ModuleBurnedOut");

        // Online & offline attempts on burned-out module in space must fail with ModuleBurnedOut
        let online_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(mwd_mod_id)], &kw, None).await.unwrap_err();
        assert_eq!(online_err.code, "ModuleBurnedOut", "Onlining burned module in space must fail");

        let offline_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "TakeModuleOffline", &[PyRep::Integer(ship_id), PyRep::Integer(mwd_mod_id)], &kw, None).await.unwrap_err();
        assert_eq!(offline_err.code, "ModuleBurnedOut", "Offlining burned module in space must fail");

        // 4. Module group / weapon bank disabling when containing a burned module (`feat:modules.overheat-burnout-state`)
        // Fit two 250mm Light Artillery Cannon I (487) with Fusion S ammo (183) in high slots and link them via LinkWeapons RPC
        let gun1_id = 881_003i64;
        let gun2_id = 881_004i64;
        let ammo1_id = 881_005i64;
        let ammo2_id = 881_006i64;
        seed_item(&ctx, gun1_id, 487, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, gun2_id, 487, char_id, ship_id, InventoryFlag::HiSlot1, 1, true);
        seed_item(&ctx, ammo1_id, 183, char_id, ship_id, InventoryFlag::HiSlot0, 100, false);
        seed_item(&ctx, ammo2_id, 183, char_id, ship_id, InventoryFlag::HiSlot1, 100, false);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(gun1_id, true);
            snap.module_online_states.insert(gun2_id, true);
        });
        ctx.invalidate_ship_fitting(ship_id);

        // Target lock raven_id for weapon activation
        let _ = sys.add_target_lock(ship_id, TargetLock {
            target_id: raven_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await;

        // Form weapon group using client RPC LinkWeapons
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(gun1_id), PyRep::Integer(gun2_id)], &kw, None).await.expect("LinkWeapons");

        // Burn out gun2
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&gun2_id) {
                it.dynamic_attributes.insert(attr_ids::DAMAGE, 40.0);
                it.dynamic_attributes.insert(attr_ids::HP_STRUCT, 40.0);
            }
        });

        let group_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun1_id), PyRep::String("projectileFired".into()), PyRep::Integer(raven_id)], &kw, None).await.unwrap_err();
        assert_eq!(group_err.code, "ModuleBurnedOut");
        assert!(
            group_err.message.contains("disabled until repaired or ungrouped"),
            "Error message must specify group disabling until repaired or ungrouped, got: {}",
            group_err.message
        );

        // Ungrouping permits activating unburned module
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UnlinkAllModules", &[PyRep::Integer(ship_id)], &kw, None).await.expect("UnlinkAllModules");
        let ungrouped_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun1_id), PyRep::String("projectileFired".into()), PyRep::Integer(raven_id)], &kw, None).await;
        assert!(ungrouped_res.is_ok(), "Ungrouped healthy module can be activated");

        // Repairing burned module also re-enables grouped activation
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&gun2_id) {
                it.dynamic_attributes.insert(attr_ids::DAMAGE, 0.0);
            }
        });
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(gun1_id), PyRep::Integer(gun2_id)], &kw, None).await.expect("LinkWeapons re-link");
        let repaired_group_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun1_id), PyRep::String("projectileFired".into()), PyRep::Integer(raven_id)], &kw, None).await;
        assert!(repaired_group_res.is_ok(), "Repaired module group can be activated");
    }
}

pub(crate) mod ships_frigate_escape_bay_blockade {
    // Integration tests closing `ships.*` and `modules.*` parity rows:
    // - `feat:ships.frigate-escape-bay`
    // - `feat:ships.blockade-runner-cargo-scan-immunity`
    // - `feat:ships.board-ship-skill-and-clone-check`
    // - `feat:ships.cynosural-beacon-spawning`
    // - `feat:ships.jump-drive-fatigue-accumulation`
    // - `feat:ships.titan-jump-portal-bridge`
    // - `feat:modules.stasis-grappler-mechanics`
    // - `feat:modules.emergency-hull-energizer`
    // - `feat:modules.mutadaptive-remote-armor-repairer`
    // - `feat:modules.entropic-disintegrator-damage-spool`

    use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
    use eve_destiny::{Ball, MovementMode, SlimItemMeta, TargetLock};
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::combat::death_pipeline::execute_death_pipeline;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    fn new_ctx() -> ServerContext {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
        ServerContext::with_sde_and_repository(sde, db)
    }

    async fn call_service(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        method: &str,
        args: Vec<PyRep>,
        kw: HashMap<String, PyRep>,
    ) -> Result<PyRep, BridgeRpcError> {
        eve_server::services::combat_crimewatch::dispatch_combat_rpc(ctx, sess, svc, method, &args, &kw)
            .await
    }

    fn make_session(
        sid: u64,
        user_id: u32,
        char_id: i64,
        corp_id: i64,
        station_id: Option<i64>,
        sys_id: u32,
        ship_id: Option<i64>,
    ) -> SessionState {
        let mut sess = SessionState::new(sid);
        sess.user_id = Some(user_id as u64);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.station_id = station_id;
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = ship_id;
        sess
    }

    fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(item_id, InventoryItemRecord {
                item_id, type_id, owner_id, location_id, flag, quantity, singleton,
                version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            });
        });
    }

    async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, type_id: u32, owner_id: i64, x: f64) {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let mut cs = ctx.build_type_combat_state(type_id);
        cs.armor_hp = cs.max_armor_hp * 0.2;
        let b = Ball {
            id: ship_id, type_id, position: DVec3::new(x, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 150.0, inertia: 1.0, mass: 10_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        let meta = SlimItemMeta {
            group_id: 26, category_id: 6, owner_id, char_id: Some(owner_id),
            corp_id: 1000009, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default()
        };
        let _ = sys_h.spawn_ball_with_meta(b, cs, meta).await;
    }

    fn grant_type_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
        let Some(t) = ctx.sde.types.get(&type_id).cloned() else { return; };
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(char_id).or_default();
            for &(sa, la) in &[(182_u16, 277_u16), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)] {
                if let Some(&sid_f) = t.default_attributes.get(&sa) {
                    let sid = sid_f.round() as u32;
                    let lvl = t.default_attributes.get(&la).copied().unwrap_or(1.0).round() as u8;
                    if sid > 0 {
                        sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl.max(5), skill_points: 256_000.0 });
                    }
                }
            }
        });
    }

    #[tokio::test]
    async fn test_ships_frigate_escape_bay_blockade_runner_scan_and_board_skill_clone_checks() {
        // parity: feat:ships.board-ship-skill-and-clone-check
        eve_testkit::attest(&[
            "feat:ships.board-ship-skill-and-clone-check",
        ]);

        let ctx = new_ctx();
        ctx.set_clock_paused(true);
        let c1 = 90_000_001;
        let c2 = 90_000_002;
        let station_id = 60_003_760;
        let sys_id = 30_000_142;
        grant_type_skills(&ctx, c1, 638);
        grant_type_skills(&ctx, c1, 587);
        ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&c1) { ch.clone_grade = 2; });

        // 1. Frigate Escape Bay (`flag 179` on Raven `638`, `frigateEscapeBayCapacity` attr 3020 > 0)
        let raven_id = 940_001;
        let packaged_rifter = 940_002;
        let rifter_with_cargo = 940_003;
        let cargo_trit = 940_004;
        let valid_rifter = 940_005;
        let second_rifter = 940_006;
        let cruiser_feb_id = 940_007;

        seed_item(&ctx, raven_id, 638, c1, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, packaged_rifter, 587, c1, station_id, InventoryFlag::Hangar, 1, false);
        seed_item(&ctx, rifter_with_cargo, 587, c1, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, cargo_trit, 34, c1, rifter_with_cargo, InventoryFlag::Cargo, 100, false);
        seed_item(&ctx, valid_rifter, 587, c1, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, second_rifter, 587, c1, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, cruiser_feb_id, 626, c1, station_id, InventoryFlag::Hangar, 1, true);

        let mut sess1 = make_session(1, 1, c1, 1000009, Some(station_id), sys_id, Some(raven_id));

        let cruiser_feb_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(cruiser_feb_id), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Cruiser into FEB must fail");
        assert_eq!(cruiser_feb_err.code, "InvalidItemTypeForHold");

        let pkg_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(packaged_rifter), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Packaged frigate must be rejected");
        assert_eq!(pkg_err.code, "ShipMustBeAssembled");

        let cargo_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(rifter_with_cargo), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Frigate with non-charge cargo must be rejected");
        assert_eq!(cargo_err.code, "ShipHasItemsInCargo");

        let _ = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(valid_rifter), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect("Assembled empty frigate must fit in Frigate Escape Bay");

        let second_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(second_rifter), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Frigate Escape Bay must reject a second frigate");
        assert_eq!(second_err.code, "CapacityExceeded");

        // Undock Raven into space and destroy it via execute_death_pipeline -> pilot boards `valid_rifter`!
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&c1) {
                ch.station_id = None;
                ch.active_ship_id = raven_id;
            }
            if let Some(sh) = snap.items.get_mut(&raven_id) {
                sh.location_id = sys_id as i64;
                sh.flag = InventoryFlag::None;
            }
        });
        spawn_ship_ball(&ctx, sys_id, raven_id, 638, c1, 1_000.0).await;
        let outcome = execute_death_pipeline(&ctx, sys_id, c1, raven_id, c2, DVec3::new(1_000.0, 0.0, 0.0), ctx.now_unix_secs()).await.expect("execute_death_pipeline succeeds");
        assert_eq!(outcome.ejected_capsule_id, Some(valid_rifter));
        let active_after_death = ctx.db.with_snapshot(|s| s.characters.get(&c1).map(|c| c.active_ship_id).unwrap_or(0));
        assert_eq!(active_after_death, valid_rifter, "Pilot must be placed directly into escape frigate");
        assert!(!outcome.loot_dropped_item_ids.contains(&valid_rifter), "Frigate must not drop as loot");
        assert!(!outcome.loot_destroyed_item_ids.contains(&valid_rifter), "Frigate must not be destroyed in wreck");
        let dkm = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&outcome.killmail_id).cloned()).expect("Detailed killmail must exist");
        assert!(dkm.dropped_items.iter().any(|it| it.type_id == 587 && it.flag == 179 && it.qty_dropped == 0 && it.qty_destroyed == 0), "Killmail must record surviving frigate at flag 179");
        ctx.advance_virtual_clock(15);

        // 2. Blockade Runner (`Crane` 12729, group 1202) cargo scan immunity vs NPC customs contraband scan
        let crane_id = 940_020;
        let contraband_id = 940_021; // Slaves (type 3721)
        let scanner_mod = 940_022; // Cargo Scanner I (type 442)
        seed_item(&ctx, crane_id, 12729, c2, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, contraband_id, 3721, c2, crane_id, InventoryFlag::Cargo, 5, false);
        seed_item(&ctx, scanner_mod, 442, c1, valid_rifter, InventoryFlag::MedSlot0, 1, true);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(scanner_mod, true);
            if let Some(ch) = snap.characters.get_mut(&c2) {
                ch.balance = IskCents::from_isk(10_000_000);
                ch.active_ship_id = crane_id;
                ch.solar_system_id = sys_id;
                ch.station_id = None;
            }
        });
        spawn_ship_ball(&ctx, sys_id, crane_id, 12729, c2, 2_000.0).await;
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h.add_target_lock(valid_rifter, TargetLock { target_id: crane_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(25_000.0) }).await;

        sess1.station_id = None;
        sess1.ship_id = Some(valid_rifter);
        let scan_rep = call_service(&ctx, &mut sess1, "dogmaIM", "Activate", vec![PyRep::Integer(scanner_mod), PyRep::String("cargoScan".into()), PyRep::Integer(crane_id)], HashMap::new()).await.expect("Cargo Scanner activation on Blockade Runner");
        assert_eq!(scan_rep.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(scan_rep.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

        let spawned_police = eve_server::services::combat_npc_police::check_and_spawn_faction_police(&ctx, sys_id).await;
        assert!(!spawned_police.is_empty(), "High-sec customs police must spawn when contraband is detected");
        let (cargo_has_contraband, c2_bal, has_standing_loss) = ctx.db.with_snapshot(|s| {
            let in_cargo = s.items.get(&contraband_id).map(|i| i.location_id == crane_id).unwrap_or(false);
            let bal = s.characters.get(&c2).map(|c| c.balance).unwrap_or(IskCents(0));
            let standing = s.standing_transactions.values().flatten().any(|st| st.to_id == c2 && st.modification < 0.0);
            (in_cargo, bal, standing)
        });
        assert!(!cargo_has_contraband, "Contraband must be confiscated from Blockade Runner cargo");
        assert!(c2_bal < IskCents::from_isk(10_000_000), "Customs fine must be deducted from pilot wallet");
        assert!(has_standing_loss, "Standing penalty transaction must be recorded for contraband");

        // R-CUSTOMS-SCOPE negative tests:
        // Wrecks and jetcans with contraband are NOT confiscated:
        let (wreck_id, wreck_cb, jetcan_id, jetcan_cb) = (940_080, 940_081, 940_082, 940_083);
        seed_item(&ctx, wreck_id, 26561, c2, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, wreck_cb, 3721, c2, wreck_id, InventoryFlag::Cargo, 5, false);
        seed_item(&ctx, jetcan_id, 23, c2, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, jetcan_cb, 3721, c2, jetcan_id, InventoryFlag::Cargo, 5, false);
        spawn_ship_ball(&ctx, sys_id, wreck_id, 26561, c2, 3_000.0).await;
        spawn_ship_ball(&ctx, sys_id, jetcan_id, 23, c2, 4_000.0).await;
        assert!(eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, sys_id, wreck_id).is_empty());
        assert!(eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, sys_id, jetcan_id).is_empty());
        assert!(ctx.db.get_item(wreck_cb).is_some() && ctx.db.get_item(jetcan_cb).is_some());

        // System below confiscateMinSec: Tama (0.28) is below SDE confiscateMinSec -> contraband NOT confiscated:
        let (tama_sys, tama_ship, tama_cb) = (30_002_813, 940_084, 940_085);
        seed_item(&ctx, tama_ship, 587, c2, tama_sys as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, tama_cb, 3721, c2, tama_ship, InventoryFlag::Cargo, 5, false);
        ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&c2) { ch.solar_system_id = tama_sys; ch.active_ship_id = tama_ship; } });
        spawn_ship_ball(&ctx, tama_sys, tama_ship, 587, c2, 1_000.0).await;
        assert!(eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, tama_sys, tama_ship).is_empty());
        assert!(ctx.db.get_item(tama_cb).is_some(), "Contraband below confiscateMinSec must not be confiscated");
        ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&c2) { ch.solar_system_id = sys_id; ch.active_ship_id = crane_id; } });

        // 3. Board ship skill and Alpha clone grade checks (`ship.Board` and `ship.BoardStoredShip`)
        sess1.station_id = Some(station_id);
        let bustard_id = 940_030;
        seed_item(&ctx, bustard_id, 12731, c1, station_id, InventoryFlag::Hangar, 1, true);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&c1) {
                ch.station_id = Some(station_id);
            }
            snap.skills.entry(c1).or_default().insert(
                3300,
                TrainedSkillRecord {
                    type_id: 3300,
                    level: 1,
                    skill_points: 250.0,
                },
            );
        });
        let board_no_skill = call_service(&ctx, &mut sess1, "ship", "Board", vec![PyRep::Integer(bustard_id)], HashMap::new()).await.expect_err("Boarding Bustard without Transport Ships skill must fail");
        assert_eq!(board_no_skill.code, "ShipHasSkillPrerequisites");

        grant_type_skills(&ctx, c1, 12731);
        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.clone_grade = 0; } });
        let board_alpha_err = call_service(&ctx, &mut sess1, "ship", "Board", vec![PyRep::Integer(bustard_id)], HashMap::new()).await.expect_err("Alpha clone must be blocked from boarding Omega-only Bustard");
        assert_eq!(board_alpha_err.code, "TrialAccountRestriction");

        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.clone_grade = 2; } });
        let board_ok = call_service(&ctx, &mut sess1, "ship", "Board", vec![PyRep::Integer(bustard_id)], HashMap::new()).await.expect("Omega clone with skills must board Bustard");
        assert_eq!(board_ok.dict_get("shipID").and_then(|v| v.as_i64()), Some(bustard_id));

        // In space boarding (`ship.Board`):
        let unpiloted_bustard = 940_040;
        let c2_ship = 940_050;
        seed_item(&ctx, unpiloted_bustard, 12731, c2, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, c2_ship, 587, c2, sys_id as i64, InventoryFlag::None, 1, true);
        spawn_ship_ball(&ctx, sys_id, unpiloted_bustard, 12731, c2, 11_000.0).await;
        spawn_ship_ball(&ctx, sys_id, c2_ship, 587, c2, 11_500.0).await;
        let mut sess_space = make_session(2, 2, c2, 1000009, None, sys_id, Some(c2_ship));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&c2) {
                ch.solar_system_id = sys_id;
                ch.active_ship_id = c2_ship;
                ch.station_id = None;
                ch.clone_grade = 0;
            }
            snap.skills.entry(c2).or_default().clear();
        });
        let space_no_skill = call_service(&ctx, &mut sess_space, "ship", "Board", vec![PyRep::Integer(unpiloted_bustard)], HashMap::new()).await.expect_err("Missing skills in space");
        assert_eq!(space_no_skill.code, "ShipHasSkillPrerequisites");

        grant_type_skills(&ctx, c2, 12731);
        let space_alpha_err = call_service(&ctx, &mut sess_space, "ship", "Board", vec![PyRep::Integer(unpiloted_bustard)], HashMap::new()).await.expect_err("Alpha in space");
        assert_eq!(space_alpha_err.code, "TrialAccountRestriction");

        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c2) { ch.clone_grade = 2; } });
        let space_board_ok = call_service(&ctx, &mut sess_space, "ship", "Board", vec![PyRep::Integer(unpiloted_bustard)], HashMap::new()).await.expect("Omega in space boards");
        assert_eq!(space_board_ok.dict_get("shipID").and_then(|v| v.as_i64()), Some(unpiloted_bustard));
    }

    #[tokio::test]
    async fn test_ships_cyno_beacon_jump_fatigue_and_titan_portal_bridge() {

        let ctx = ServerContext::new();
        ctx.set_clock_paused(true);
        let cyno_char = 90_000_001;
        let titan_char = 90_000_002;
        let subcap_char = 90_000_003;
        let low_origin = 30_002_538;
        let low_dest = 30_002_539;

        let falcon_id = 950_001;
        let cyno_mod = 950_002;
        let lo_id = 950_003;
        let cloak_mod = 950_004;
        let avatar_id = 950_010;
        let portal_mod = 950_011;
        let stront_id = 950_012;
        let helium_id = 950_013;
        let rifter_id = 950_020;

        seed_item(&ctx, falcon_id, 11957, cyno_char, low_dest as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, cyno_mod, 21096, cyno_char, falcon_id, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, cloak_mod, 11578, cyno_char, falcon_id, InventoryFlag::HiSlot1, 1, true);
        seed_item(&ctx, lo_id, 16273, cyno_char, falcon_id, InventoryFlag::Cargo, 2_000, false);

        seed_item(&ctx, avatar_id, 11567, titan_char, low_origin as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, portal_mod, 23953, titan_char, avatar_id, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, stront_id, 16275, titan_char, avatar_id, InventoryFlag::Cargo, 5_000, false);
        seed_item(&ctx, helium_id, 16274, titan_char, avatar_id, InventoryFlag::Cargo, 500_000, false);
        seed_item(&ctx, rifter_id, 587, subcap_char, low_origin as i64, InventoryFlag::None, 1, true);

        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(cyno_mod, true);
            snap.module_online_states.insert(cloak_mod, true);
            snap.module_online_states.insert(portal_mod, true);
            snap.skills.entry(cyno_char).or_default().insert(21611, TrainedSkillRecord { type_id: 21611, level: 5, skill_points: 256_000.0 });
            snap.skills.entry(cyno_char).or_default().insert(21603, TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
            snap.skills.entry(titan_char).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
            let fleet_id = 77_001;
            snap.fleets.insert(
                fleet_id,
                FleetRecord {
                    fleet_id,
                    boss_id: titan_char,
                    members: vec![cyno_char, titan_char, subcap_char],
                    ..Default::default()
                },
            );
            if let Some(ch_base) = snap.characters.get(&cyno_char).cloned() {
                let mut sub_ch = ch_base;
                sub_ch.character_id = subcap_char;
                sub_ch.user_id = 3;
                sub_ch.active_ship_id = rifter_id;
                sub_ch.solar_system_id = low_origin;
                snap.characters.insert(subcap_char, sub_ch);
            }
            for (cid, sid, sys) in [(cyno_char, falcon_id, low_dest), (titan_char, avatar_id, low_origin), (subcap_char, rifter_id, low_origin)] {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.solar_system_id = sys;
                    ch.active_ship_id = sid;
                    ch.station_id = None;
                }
            }
            snap.accounts.insert(
                "sub".to_string(),
                eve_db::AccountRecord {
                    user_id: 3,
                    username: "sub".to_string(),
                    password_token: "dummy".to_string(),
                    role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        });
        spawn_ship_ball(&ctx, low_dest, falcon_id, 11957, cyno_char, 0.0).await;
        spawn_ship_ball(&ctx, low_origin, avatar_id, 11567, titan_char, 0.0).await;
        spawn_ship_ball(&ctx, low_origin, rifter_id, 587, subcap_char, 1_000.0).await;

        let mut s_cyno = make_session(1, 1, cyno_char, 1000009, None, low_dest, Some(falcon_id));
        let mut s_titan = make_session(2, 2, titan_char, 1000009, None, low_origin, Some(avatar_id));
        let mut s_sub = make_session(3, 3, subcap_char, 1000009, None, low_origin, Some(rifter_id));

        let now_ts = ctx.now_unix();
        let _ = ctx.db.acquire_online_lock(1, s_cyno.session_id, Some(cyno_char), true, now_ts);
        let _ = ctx.db.acquire_online_lock(2, s_titan.session_id, Some(titan_char), true, now_ts);
        let _ = ctx.db.acquire_online_lock(3, s_sub.session_id, Some(subcap_char), true, now_ts);

        let cyno_rep = call_service(&ctx, &mut s_cyno, "dogmaIM", "Activate", vec![PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())], HashMap::new()).await.expect("Falcon lights cynosural field");
        let beacon_id = cyno_rep.dict_get("beaconID").and_then(|v| v.as_i64()).expect("beaconID");
        assert!(beacon_id > 0);

        let lo_after = ctx.db.with_snapshot(|s| s.items.get(&lo_id).map(|i| i.quantity).unwrap_or(0));
        assert!(lo_after < 2_000, "Liquid Ozone must be consumed scaled by Cynosural Field Theory");
        let b_ball = ctx.systems.get_or_spawn(low_dest).get_ball_snapshot(beacon_id).await.ok().flatten();
        assert!(b_ball.is_some(), "Beacon ball must be spawned in space");
        let f_ball = ctx.systems.get_or_spawn(low_dest).get_ball_snapshot(falcon_id).await.ok().flatten().unwrap().1;
        assert_eq!(f_ball.max_velocity, 0.0, "Cyno ship must be immobilized (0 m/s)");

        let dock_err = call_service(&ctx, &mut s_cyno, "stationSvc", "Dock", vec![PyRep::Integer(60003760)], HashMap::new()).await.expect_err("Dock blocked");
        assert_eq!(dock_err.code, "CynosuralFieldActive");
        let b_cyno = ctx.bind_object(&s_cyno, "beyonce", low_dest as i64).expect("bind beyonce");
        let jump_err = ctx.dispatch_rpc(&mut s_cyno, "beyonce", Some(b_cyno), "CmdStargateJump", &[PyRep::Integer(50000001)], &HashMap::new(), None).await.expect_err("Gate jump blocked");
        assert_eq!(jump_err.code, "CynosuralFieldActive");
        let cloak_err = call_service(&ctx, &mut s_cyno, "dogmaIM", "Activate", vec![PyRep::Integer(cloak_mod), PyRep::String("cloaking".into())], HashMap::new()).await.expect_err("Cloak blocked");
        assert_eq!(cloak_err.code, "CynosuralFieldActive");

        let beacon_notifs = ctx.drain_notifications_for_char(titan_char);
        let beacon_notif = beacon_notifs.iter().find(|n| n.event == "OnFleetJumpBeaconModuleChange").expect("Beacon notification sent to fleet members");
        assert_eq!(beacon_notif.args.get(2).and_then(|v| v.as_i64()), Some(beacon_id), "Beacon must appear in fleet jump target list");

        let bridge_rep = call_service(&ctx, &mut s_titan, "beyonce", "CmdBridgeToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(low_dest as i64), PyRep::Integer(beacon_id)], HashMap::new()).await.expect("Titan opens jump portal bridge");
        assert_eq!(bridge_rep.dict_get("beaconID").and_then(|v| v.as_i64()), Some(beacon_id));

        let _ = ctx.systems.get_or_spawn(low_origin).set_ball_position(rifter_id, DVec3::new(3_500.0, 0.0, 0.0)).await;
        let dist_err = call_service(&ctx, &mut s_sub, "beyonce", "CmdJumpThroughFleet", vec![PyRep::Integer(titan_char), PyRep::Integer(avatar_id)], HashMap::new()).await.expect_err("Passenger > 2500m must be refused");
        assert_eq!(dist_err.code, "OutOfPortalRange");
        let _ = ctx.systems.get_or_spawn(low_origin).set_ball_position(rifter_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

        let titan_h_before = ctx.db.with_snapshot(|s| s.items.get(&helium_id).map(|i| i.quantity).unwrap_or(0));
        let jump_through = call_service(&ctx, &mut s_sub, "beyonce", "CmdJumpThroughFleet", vec![PyRep::Integer(titan_char), PyRep::Integer(avatar_id)], HashMap::new()).await.expect("Passenger jumps through portal");
        assert_eq!(jump_through.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(low_dest as i64));
        let titan_h_after = ctx.db.with_snapshot(|s| s.items.get(&helium_id).map(|i| i.quantity).unwrap_or(0));
        assert!(titan_h_after < titan_h_before, "Titan isotopes consumed scaled by passenger mass and distance");
        let fatigue_min = jump_through.dict_get("jumpFatigueMinutes").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!(fatigue_min > 0.0, "Passenger must accumulate jump fatigue");

        ctx.advance_secs(61);
        let _ = ctx.systems.get_or_spawn(low_origin).step_manual(1.0).await;

        let far_err = call_service(&ctx, &mut s_titan, "beyonce", "CmdJumpToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(30000142), PyRep::Integer(beacon_id)], HashMap::new()).await.expect_err("Jump distance out of range");
        assert_eq!(far_err.code, "JumpDistanceOutOfRange");

        let titan_jump = call_service(&ctx, &mut s_titan, "beyonce", "CmdJumpToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(low_dest as i64), PyRep::Integer(beacon_id)], HashMap::new()).await.expect("Titan jumps to fleet cyno beacon");
        assert_eq!(titan_jump.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(low_dest as i64));
        let titan_h_final = ctx.db.with_snapshot(|s| s.items.get(&helium_id).map(|i| i.quantity).unwrap_or(0));
        assert!(titan_h_final < titan_h_after, "Titan isotopes consumed scaled by Jump Fuel Conservation");
        let cd_err = call_service(&ctx, &mut s_titan, "beyonce", "CmdJumpToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(low_dest as i64), PyRep::Integer(beacon_id)], HashMap::new()).await.expect_err("Immediate jump cooldown");
        assert_eq!(cd_err.code, "JumpReactivationCooldown");
    }

    #[tokio::test]
    async fn test_modules_grappler_ehe_mutadaptive_repairer_and_entropic_disintegrator() {
        // parity: feat:modules.entropic-disintegrator-damage-spool
        eve_testkit::attest(&[
            "feat:modules.entropic-disintegrator-damage-spool",
        ]);

        let ctx = new_ctx();
        let c1 = 90_000_001;
        let c2 = 90_000_002;
        let station_id = 60_003_760;
        let sys_id = 30_004_759;
        grant_type_skills(&ctx, c1, 626);
        grant_type_skills(&ctx, c1, 638);
        grant_type_skills(&ctx, c1, 24483);
        grant_type_skills(&ctx, c1, 47272);
        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.clone_grade = 2; } });

        // 1. Heavy Stasis Grappler (group 1672): rejected on Cruiser (`626`), accepted on Battleship (`638`), max 1 per ship
        let grappler_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 1672 && t.category_id == 7)
            .map(|t| t.type_id)
            .expect("Heavy Stasis Grappler in SDE");
        let ehe_tid = 40714_u32;
        let vexor_id = 960_001;
        let raven_id = 960_002;
        let grappler_1 = 960_003;
        let grappler_2 = 960_004;
        seed_item(&ctx, vexor_id, 626, c1, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, raven_id, 638, c1, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, grappler_1, grappler_tid, c1, station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, grappler_2, grappler_tid, c1, station_id, InventoryFlag::Hangar, 1, true);
        grant_type_skills(&ctx, c1, grappler_tid);

        let mut sess = make_session(1, 1, c1, 1000009, Some(station_id), sys_id, Some(vexor_id));

        let cruiser_err = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(grappler_1), PyRep::Integer(vexor_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)], HashMap::new()).await.expect_err("Cruiser must not fit Heavy Stasis Grappler");
        assert_eq!(cruiser_err.code, "CanNotFitModuleToShip");

        sess.ship_id = Some(raven_id);
        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.active_ship_id = raven_id; } });
        let _ = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(grappler_1), PyRep::Integer(raven_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)], HashMap::new()).await.expect("Battleship must fit 1 Heavy Stasis Grappler");
        let max_group_err = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(grappler_2), PyRep::Integer(raven_id), PyRep::Integer(InventoryFlag::MedSlot1 as i64)], HashMap::new()).await.expect_err("Max 1 Heavy Stasis Grappler per ship");
        assert_eq!(max_group_err.code, "MaxGroupFitted");

        // Undock Raven to activate Stasis Grappler on target at 1km and falloff
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(grappler_1, true);
            if let Some(ch) = snap.characters.get_mut(&c1) { ch.station_id = None; ch.solar_system_id = sys_id; ch.active_ship_id = raven_id; }
        });
        sess.station_id = None;
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(raven_id);
        spawn_ship_ball(&ctx, sys_id, raven_id, 638, c1, 0.0).await;
        let grappler_target = 960_005;
        seed_item(&ctx, grappler_target, 626, c2, sys_id as i64, InventoryFlag::None, 1, true);
        spawn_ship_ball(&ctx, sys_id, grappler_target, 626, c2, 1_000.0).await;
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h.add_target_lock(raven_id, TargetLock { target_id: grappler_target, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
        let base_t_vel = sys_h.get_ball_snapshot(grappler_target).await.unwrap().unwrap().1.max_velocity;

        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(grappler_1), PyRep::String("stasisGrappler".into()), PyRep::Integer(grappler_target)], HashMap::new()).await.expect("Grappler activates at 1km");
        let grap_1km_vel = sys_h.get_ball_snapshot(grappler_target).await.unwrap().unwrap().1.max_velocity;
        assert!(grap_1km_vel < base_t_vel * 0.20, "Point blank 1km grappler must reduce speed by 85%");
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(grappler_1)], HashMap::new()).await.expect("Grappler deactivates");

        let _ = sys_h.set_ball_position(grappler_target, DVec3::new(8_000.0, 0.0, 0.0)).await;
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(grappler_1), PyRep::String("stasisGrappler".into()), PyRep::Integer(grappler_target)], HashMap::new()).await.expect("Grappler activates at 8km falloff");
        let grap_8km_vel = sys_h.get_ball_snapshot(grappler_target).await.unwrap().unwrap().1.max_velocity;
        assert!(grap_8km_vel > grap_1km_vel && grap_8km_vel < base_t_vel, "Grappler speed reduction must decay over falloff curve");
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(grappler_1)], HashMap::new()).await.expect("Grappler deactivates");

        // 2. Capital Emergency Hull Energizer (`ehe_tid`): 95% hull resist (0.05 resonance) for 1 cycle, burns out, refuses nanite repair
        let mutadaptive_tid = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 2018 && t.category_id == 7)
            .map(|t| t.type_id)
            .expect("Mutadaptive Remote Armor Repairer in SDE");
        let archon_id = 960_010;
        let ehe_id = 960_011;
        let mutadaptive_id = 960_012;
        let target_ship_a = 960_020;
        let target_ship_b = 960_021;
        seed_item(&ctx, archon_id, 24483, c1, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, ehe_id, ehe_tid, c1, archon_id, InventoryFlag::LoSlot0, 1, true);
        seed_item(&ctx, mutadaptive_id, mutadaptive_tid, c1, archon_id, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, target_ship_a, 638, c2, sys_id as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, target_ship_b, 638, c2, sys_id as i64, InventoryFlag::None, 1, true);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(ehe_id, true);
            snap.module_online_states.insert(mutadaptive_id, true);
            if let Some(ch) = snap.characters.get_mut(&c1) {
                ch.station_id = None;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = archon_id;
            }
        });
        sess.station_id = None;
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(archon_id);

        spawn_ship_ball(&ctx, sys_id, archon_id, 24483, c1, 0.0).await;
        spawn_ship_ball(&ctx, sys_id, target_ship_a, 638, c2, 2_000.0).await;
        spawn_ship_ball(&ctx, sys_id, target_ship_b, 638, c2, 3_000.0).await;
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;

        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(ehe_id), PyRep::String("emergencyHullEnergizer".into())], HashMap::new()).await.expect("EHE must activate on capital hull");
        let active_cs = sys_h.get_combat_state(archon_id).await.unwrap().unwrap();
        assert_eq!(active_cs.structure_resonance, [0.05, 0.05, 0.05, 0.05], "EHE must grant 95% hull resistance across EM/Therm/Kin/Exp");
        let snap_res = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&archon_id).map(|t| (t.get(&109).copied(), t.get(&110).copied())).unwrap());
        assert_eq!(snap_res, (Some(0.05), Some(0.05)));

        let _ = sys_h.step_manual(25.0).await;
        let burned_dmg = ctx.db.with_snapshot(|s| s.items.get(&ehe_id).and_then(|i| i.dynamic_attributes.get(&3).copied()).unwrap_or(0.0));
        assert!(burned_dmg >= 39.9, "EHE must burn out after 1 cycle");
        let nanite_err = call_service(&ctx, &mut sess, "dogmaIM", "RepairModule", vec![PyRep::Integer(archon_id), PyRep::Integer(ehe_id)], HashMap::new()).await.expect_err("Burned-out EHE must refuse Nanite Repair Paste");
        assert_eq!(nanite_err.code, "ModuleBurnedOut");

        // 3. Mutadaptive Remote Armor Repairer (`mutadaptive_id`) spools up per cycle on `target_ship_a` and resets on `target_ship_b`
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mutadaptive_id), PyRep::String("remoteArmorRepair".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Mutadaptive RAR activates on target A");
        let _ = sys_h.step_manual(12.0).await;
        let rep_after_1 = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(0.0));
        let _ = sys_h.step_manual(12.0).await;
        let rep_after_2 = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(0.0));
        assert!(rep_after_2 > rep_after_1, "Mutadaptive RAR must spool up repair amount across consecutive cycles");

        let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_b, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mutadaptive_id), PyRep::String("remoteArmorRepair".into()), PyRep::Integer(target_ship_b)], HashMap::new()).await.expect("Mutadaptive RAR retargets to target B");
        let _ = sys_h.step_manual(12.0).await;
        let rep_after_switch = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(0.0));
        assert!((rep_after_switch - rep_after_1).abs() < 1e-4, "Switching target on Mutadaptive RAR must reset spool to cycle 0");
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(mutadaptive_id)], HashMap::new()).await.expect("Deactivate Mutadaptive RAR");
        let rep_after_deact = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(rep_after_1));
        assert!((rep_after_deact - rep_after_1).abs() < 1e-4, "Deactivating Mutadaptive RAR must reset transient attr 84 to base");
        let _ = sys_h.remove_target_lock(archon_id, target_ship_b).await;

        // 4. Entropic Disintegrator (`47272` Light Entropic Disintegrator I) spools up per cycle, reaches damageMultiplierBonusMax, resets on target change / deactivation / lock loss
        let dis_mod_id = 960_030;
        let dis_ammo_id = 960_031;
        seed_item(&ctx, dis_mod_id, 47272, c1, archon_id, InventoryFlag::HiSlot1, 1, true);
        seed_item(&ctx, dis_ammo_id, 47885, c1, archon_id, InventoryFlag::HiSlot1, 100, false);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(dis_mod_id, true);
        });

        // Give target ship structure-only HP pool to test continuous structure damage ramp
        let mut cs_a = ctx.build_type_combat_state(638);
        cs_a.shield_hp = 0.0;
        cs_a.max_shield_hp = 0.0;
        cs_a.armor_hp = 0.0;
        cs_a.max_armor_hp = 0.0;
        cs_a.structure_hp = 100_000.0;
        cs_a.max_structure_hp = 100_000.0;
        let _ = sys_h.set_combat_state(target_ship_a, cs_a.clone()).await;
        let _ = sys_h.set_combat_state(target_ship_b, cs_a).await;
        let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
        ctx.set_invention_roll_override(Some(0.50));

        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Initial entropic activation on target A");
        let m0 = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
        assert_eq!(m0, 1.0);

        // Advance spool cycle 1 and record damage
        let hp0 = sys_h.get_combat_state(target_ship_a).await.unwrap().unwrap().structure_hp;
        sys_h.step_manual(4.5).await.unwrap();
        let hp1 = sys_h.get_combat_state(target_ship_a).await.unwrap().unwrap().structure_hp;
        let dmg_cycle1 = hp0 - hp1;
        let m1 = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
        assert!(m1 > 1.0, "Entropic Disintegrator must ramp damage multiplier per cycle");

        // Advance spool cycle 2 and verify continuous damage ramps: cycle 2 damage > cycle 1 damage
        sys_h.step_manual(4.5).await.unwrap();
        let hp2 = sys_h.get_combat_state(target_ship_a).await.unwrap().unwrap().structure_hp;
        let dmg_cycle2 = hp1 - hp2;
        assert!(dmg_cycle2 > dmg_cycle1, "Entropic disintegrator damage ramping must make cycle 2 damage ({dmg_cycle2}) > cycle 1 damage ({dmg_cycle1})");

        // Advance 31 more cycles up to damageMultiplierBonusMax (1.0 + attr 2734 == 3.125)
        for _ in 0..31 {
            sys_h.step_manual(4.5).await.unwrap();
        }
        let m_max = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
        assert!((m_max - 3.125).abs() < 1e-4, "Damage multiplier must reach damageMultiplierBonusMax (3.125)");

        // Target switch: lock target B and switch target: resets multiplier to 1.0
        let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_b, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_b)], HashMap::new()).await.expect("Target switch to target B");
        let m_switch = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
        assert_eq!(m_switch, 1.0, "Switching target must reset Entropic Disintegrator spool to 1.0");

        // Deactivation via dogmaIM.Deactivate: clears spool
        let deact_rep = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(dis_mod_id)], HashMap::new()).await.expect("dogmaIM.Deactivate must succeed");
        assert_eq!(deact_rep.dict_get("spoolMultiplier").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0);
        // Remove target B lock so only target A is locked
        let _ = sys_h.remove_target_lock(archon_id, target_ship_b).await;
        // Reactivate on target A and remove target lock via dogmaIM.RemoveTarget: module must deactivate and spool stop immediately
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Reactivating on target A");
        sys_h.step_manual(4.5).await.unwrap();
        assert!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id) > 1.0);
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "RemoveTarget", vec![PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("RemoveTarget on sole locked target");
        assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "RemoveTarget must stop entropic spool");
        assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "RemoveTarget must deactivate entropic disintegrator");

        // Reactivate on target A, then clear all locks in space (empty locks list) during step: module deactivates
        let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Reactivating on target A");
        let _ = sys_h.clear_target_locks(archon_id).await;
        sys_h.step_manual(4.5).await.unwrap();
        assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "Empty locks list must stop entropic spool on step");
        assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "Empty locks list must deactivate module on step");

        // a) Target moves beyond maximum range: spool multiplier resets to 1.0 and module deactivates
        let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Activate target A");
        sys_h.step_manual(4.5).await.unwrap();
        assert!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id) > 1.0);
        let _ = sys_h.set_ball_position(target_ship_a, DVec3::new(100_000.0, 0.0, 0.0)).await;
        sys_h.step_manual(4.5).await.unwrap();
        assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "Out of range must reset spool to 1.0");
        assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "Out of range must deactivate module");

        // b) Ship capacitor runs dry / insufficient capacitor: spool multiplier resets to 1.0 and module deactivates
        let _ = sys_h.set_ball_position(target_ship_a, DVec3::new(2_000.0, 0.0, 0.0)).await;
        let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
        let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Activate in range");
        sys_h.step_manual(4.5).await.unwrap();
        assert!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id) > 1.0);
        if let Ok(Some(mut cs)) = sys_h.get_combat_state(archon_id).await {
            cs.capacitor = 0.0;
            cs.max_capacitor = 0.0;
            let _ = sys_h.set_combat_state(archon_id, cs).await;
        }
        sys_h.step_manual(4.5).await.unwrap();
        assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "Capacitor dry must reset spool to 1.0");
        assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "Capacitor dry must deactivate module");
    }
}
