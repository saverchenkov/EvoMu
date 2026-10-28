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
    give_item(&ctx, 71020, 17887, 91015, 81015, InventoryFlag::Cargo, 30_000);
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

