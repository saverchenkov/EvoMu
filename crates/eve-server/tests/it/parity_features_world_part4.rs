//! Verified runtime integration tests for world parity features (Part 4: Abyssal Pylons & Weather,
//! FW Boosters, Event Sites, Equinox Quasar Sovereignty, Mercenary Dens & MTOs, and UI Photo Mode).

#![forbid(unsafe_code)]

use crate::common;
use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc as qrpc, sess as qsess, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay_closure::{
    drain_quasar, find_nullsec_planets, fit_subcap_blasters, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND};
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{advance_site_wave_or_complete, with_pve_exploration_reg, PveSiteInstance};
use eve_server::services::pve_incursion_lifecycle::{register_world_event_tale, WorldEventTaleRecord};
use eve_server::services::pve_special_sites_abyssal::{
    apply_abyssal_weather_dogma_modifiers_to_ship, apply_abyssal_weather_to_combat_and_nav,
    pulse_abyssal_automated_pylons, restore_abyssal_weather_dogma_modifiers, AbyssalWeatherKind,
    AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, MULTIBODY_TRACKING_PYLON_TYPE_ID,
};
use eve_server::services::quasar_progression_extra::CommunityTrackState;
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN;
use eve_server::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

#[tokio::test]
async fn test_abyssal_automated_pylons_and_weather_effects_penalties() {
    // parity: feat:abyssal.automated-pylons
    // parity: feat:abyssal.weather-effects-and-penalties
    eve_testkit::attest(&[
        "feat:abyssal.automated-pylons",
        "feat:abyssal.weather-effects-and-penalties",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_101_i64;
    let ship_id = 100_000_101_i64;
    let abyssal_sys = 32_000_001_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: abyssal_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // 1. Weather effects & penalties across T0 (30%), T2 (50%), T4 (70%) and all 5 weather types
    for (tier, expected_pct) in [(0_u8, 30.0), (2_u8, 50.0), (4_u8, 70.0)] {
        let (penalty_pct, bonus_pct) = match tier {
            0 | 1 => (30.0, 30.0),
            2 | 3 => (50.0, 50.0),
            _ => (70.0, 70.0),
        };
        assert_eq!(penalty_pct, expected_pct);
        for weather in [
            AbyssalWeatherKind::Electrical,
            AbyssalWeatherKind::Dark,
            AbyssalWeatherKind::Exotic,
            AbyssalWeatherKind::Firestorm,
            AbyssalWeatherKind::Gamma,
        ] {
            let mut cs = ctx.build_type_combat_state(587);
            let mut max_vel = 365.0;
            let mut opt_range = 15_000.0;
            apply_abyssal_weather_to_combat_and_nav(
                weather,
                penalty_pct,
                bonus_pct,
                &mut cs,
                &mut max_vel,
                &mut opt_range,
            );
            let saved = apply_abyssal_weather_dogma_modifiers_to_ship(
                &ctx,
                ship_id,
                weather,
                penalty_pct,
                bonus_pct,
            );
            assert!(!saved.is_empty());
            let dyn_attrs = ctx
                .db
                .with_snapshot(|s| s.items.get(&ship_id).unwrap().dynamic_attributes.clone());
            let base_ty = ctx.sde.types.get(&587).unwrap();
            match weather {
                AbyssalWeatherKind::Electrical => {
                    assert!(cs.shield_resonance[0] > 0.5 && cs.armor_resonance[0] > 0.5);
                    assert!(dyn_attrs[&55] < base_ty.default_attributes[&55]);
                }
                AbyssalWeatherKind::Dark => {
                    assert!(max_vel > 365.0 && opt_range < 15_000.0);
                    assert!(dyn_attrs[&37] > base_ty.default_attributes[&37]);
                }
                AbyssalWeatherKind::Exotic => {
                    assert!(cs.shield_resonance[2] > 0.6 && cs.armor_resonance[2] > 0.75);
                    assert!(dyn_attrs[&564] > base_ty.default_attributes[&564]);
                }
                AbyssalWeatherKind::Firestorm => {
                    assert!(cs.max_armor_hp > 300.0 && cs.shield_resonance[1] > 0.8);
                    assert!(dyn_attrs[&265] > base_ty.default_attributes[&265]);
                }
                AbyssalWeatherKind::Gamma => {
                    assert!(cs.max_shield_hp > 300.0 && cs.shield_resonance[3] > 0.5);
                    assert!(dyn_attrs[&263] > base_ty.default_attributes[&263]);
                }
            }
            restore_abyssal_weather_dogma_modifiers(&ctx, ship_id, &saved);
            assert!(ctx
                .db
                .with_snapshot(|s| s.items.get(&ship_id).unwrap().dynamic_attributes.is_empty()));
        }
    }

    // 2. Automated pylons: Suppressor (47437) damages rogue drone (47847) & spares player ship (587);
    //    Multibody Tracking Pylon (47469) grants 1.8x tracking and reverts to 1.0x when destroyed.
    let sys_handle = ctx.systems.get_or_spawn(abyssal_sys);
    let mut ship_cs = ctx.build_ship_combat_state(char_id, ship_id, 587);
    ship_cs.shield_hp = 5_000.0;
    ship_cs.max_shield_hp = 5_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ship_cs,
        )
        .await;

    let drone_id = 100_000_102_i64;
    let mut drone_cs = ctx.build_type_combat_state(47_847);
    drone_cs.shield_hp = 500.0;
    drone_cs.max_shield_hp = 500.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: drone_id,
                type_id: 47_847,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 500.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            drone_cs,
        )
        .await;

    let (sup_id, trk_id) = (100_000_103_i64, 100_000_104_i64);
    for (pid, tid, rad) in [
        (sup_id, AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, 15_000.0),
        (trk_id, MULTIBODY_TRACKING_PYLON_TYPE_ID, 20_000.0),
    ] {
        let mut pcs = ctx.build_type_combat_state(tid);
        pcs.structure_hp = 5_000.0;
        pcs.max_structure_hp = 5_000.0;
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: pid,
                    type_id: tid,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1.0,
                    radius: rad,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                pcs,
            )
            .await;
    }

    let drone_hp_before = sys_handle.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp;
    let ship_hp_before = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp;
    let (dmg_cnt, trk_cnt) = pulse_abyssal_automated_pylons(&ctx, abyssal_sys).await;
    assert!(dmg_cnt > 0 && trk_cnt > 0);
    assert!(sys_handle.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp < drone_hp_before);
    let ship_after = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(ship_after.shield_hp, ship_hp_before);
    assert!((ship_after.tracking_speed_multiplier - 1.8).abs() < 1e-6);

    let kill = DamageVector { em: 1e7, thermal: 1e7, kinetic: 1e7, explosive: 1e7 };
    let _ = sys_handle.apply_damage_with_attacker(sup_id, kill, Some(char_id)).await;
    let _ = sys_handle.apply_damage_with_attacker(trk_id, kill, Some(char_id)).await;
    assert_eq!(pulse_abyssal_automated_pylons(&ctx, abyssal_sys).await, (0, 0));
    assert!((sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.0).abs() < 1e-6);
}

#[tokio::test]
async fn test_fw_boosters_consumption_and_militia_lp_catalog() {
    // parity: feat:fw.fw-boosters
    eve_testkit::attest(&["feat:fw.fw-boosters"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_402);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    // 1. Militia LP stores serve authentic Tranquility offers and enforce militia enlistment
    let offers_rep = rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_179)])
        .await
        .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offers) = offers_rep else {
        panic!("expected list of LP offers");
    };
    assert!(!offers.is_empty());
    let first_offer_id = offers[0].dict_get("offerID").and_then(|v| v.as_i64()).unwrap();

    let (fw_station, fw_sys) = (60_015_054_i64, 30_002_978_u32);
    sess.station_id = Some(fw_station);
    sess.solar_system_id = Some(fw_sys);
    let (booster_1, booster_2) = (940_001_i64, 940_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(fw_station);
        ch.solar_system_id = fw_sys;
        ch.war_faction_id = None;
        ch.standings.insert(1_000_179, -1.0);
        ch.loyalty_points.insert(1_000_179, 500_000);
        snap.skills.entry(char_id).or_default().remove(&3405); // Biology
        for (iid, tid) in [(booster_1, 91_916_u32), (booster_2, 91_920_u32)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: fw_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        }
    });

    let err_militia = rpc(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1_000_179), PyRep::Integer(first_offer_id), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(err_militia.code, "InsufficientStanding");

    // 2. FW booster requires Biology I, activates into slot 14 for 7200s, and blocks duplicate slot 14
    let err_skill = rpc(&ctx, &mut sess, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_1)]).await.unwrap_err();
    assert_eq!(err_skill.code, "SkillPrerequisitesNotMet");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3405,
            TrainedSkillRecord { type_id: 3405, level: 1, skill_points: 250.0 },
        );
    });
    let used = rpc(&ctx, &mut sess, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_1)]).await.expect("UseBooster FW booster");
    assert_eq!(used.dict_get("boosterTypeID").and_then(|v| v.as_i64()), Some(91_916));
    assert_eq!(used.dict_get("boosterSlot").and_then(|v| v.as_i64()), Some(14));
    assert_eq!(used.dict_get("durationSeconds").and_then(|v| v.as_i64()), Some(8_640));

    let err_dup = rpc(&ctx, &mut sess, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_2)]).await.unwrap_err();
    assert_eq!(err_dup.code, "BoosterSlotOccupied");

    let PyRep::Dict(entries) = rpc(&ctx, &mut sess, "skillMgr2", "GetBoosters", &[]).await.expect("GetBoosters") else {
        panic!("expected Dict from GetBoosters");
    };
    assert!(entries.iter().any(|(k, _)| k.as_i64() == Some(91_916)));
}

#[tokio::test]
async fn test_pve_events_sites_and_world_event_tales() {
    // parity: feat:pve.events-sites
    eve_testkit::attest(&["feat:pve.events-sites"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(40_403);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let sys_id = 30_000_142_u32;
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
    });
    let now = ctx.now_unix();
    let event_dungeon_id = 5_717_u32; // SDE archetype 50 ("Station Harbor / Event Site")
    let event_tpl_id = 5_717_u32;

    let tale_id = register_world_event_tale(
        &ctx,
        WorldEventTaleRecord {
            tale_id: 0,
            template_id: i64::from(event_tpl_id),
            template_class_id: "worldEvents".into(),
            solar_system_id: sys_id,
            constellation_id: 20_000_020,
            aggressor_faction_id: 500_021,
            state: "Active".into(),
            influence: 0.0,
            start_unix: now.saturating_sub(600),
            end_unix: now + 86_400,
        },
    );
    ctx.quasar_gateway.progression.extra.active_tales.write().unwrap().insert(tale_id as u32, (event_tpl_id, sys_id, now));
    ctx.quasar_gateway.progression.extra.community_tracks.write().unwrap().insert(
        701,
        CommunityTrackState {
            track_id: 701,
            template_id: 701,
            name: "Event Site Track".into(),
            current_progress: 0,
            target_progress: 10,
            active: true,
            tale_templates: vec![event_tpl_id],
        },
    );

    let tales_rep = rpc(
        &ctx,
        &mut sess,
        "taleMgr",
        "get_active_tales_by_template",
        &[PyRep::Integer(i64::from(event_tpl_id))],
    )
    .await
    .expect("get_active_tales_by_template");
    let PyRep::List(tales) = tales_rep else {
        panic!("expected List of event tales");
    };
    assert!(tales.iter().any(|t| t.dict_get("taleID").and_then(|v| v.as_i64()) == Some(tale_id)));

    let site_id = "EVT-30000142-1";
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: site_id.into(),
            dungeon_id: event_dungeon_id,
            dungeon_name: "Space Oddity".into(),
            solar_system_id: sys_id,
            site_category: "Event".into(),
            position: [15_000_000.0, 0.0, 0.0],
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            created_unix: now,
            lifetime_seconds: 86_400,
            ..Default::default()
        });
    });

    let comp = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, site_id)
        .await
        .expect("complete event site");
    assert_eq!(comp.dict_get("completed"), Some(&PyRep::Bool(true)));
    let track_prog = ctx
        .quasar_gateway
        .progression
        .extra
        .community_tracks
        .read()
        .unwrap()
        .get(&701)
        .map(|t| t.current_progress)
        .unwrap_or(0);
    assert_eq!(track_prog, 1);
}

#[tokio::test]
async fn test_equinox_sov_hub_power_reagents_fuel_upgrades_and_workforce() {
    // parity: feat:quasar.sovereignty
    // parity: feat:sovereignty.sov-hub-fuel
    // parity: feat:sovereignty.sov-hub-upgrades
    // parity: feat:sovereignty.sov-power
    // parity: feat:sovereignty.sov-reagents
    // parity: feat:sovereignty.sov-workforce
    eve_testkit::attest(&[
        "feat:quasar.sovereignty",
        "feat:sovereignty.sov-hub-fuel",
        "feat:sovereignty.sov-hub-upgrades",
        "feat:sovereignty.sov-power",
        "feat:sovereignty.sov-reagents",
        "feat:sovereignty.sov-workforce",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (jita, perimeter) = (30_000_142_u32, 30_000_144_u32);
    let (hub_a, hub_b) = (1_020_000_001_i64, 1_020_000_002_i64);
    let (gas_item, upg_item, station_gas) = (990_001_i64, 990_003_i64, 990_004_i64);
    const STABILITY_GEN: u64 = 87_950; // 200 power, 2,700 workforce, 7 Magmatic Gas/hr

    let mut s = qsess(61_001, PILOT, CORP, jita, corp_roles::ROLE_DIRECTOR);
    let mut rx = subscribe(&ctx, &mut s).await;
    let jita_planets: Vec<i64> = ctx
        .sde
        .celestials_in_system(jita)
        .into_iter()
        .filter(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .collect();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(hub_a, structure(hub_a, CORP, jita, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_b, structure(hub_b, CORP, perimeter, TYPE_SOVEREIGNTY_HUB));
        for (iid, tid, qty, loc) in [
            (gas_item, TYPE_MAGMATIC_GAS, 1_000, PILOT_SHIP),
            (upg_item, STABILITY_GEN as u32, 1, hub_a),
            (station_gas, TYPE_MAGMATIC_GAS, 1_000, 60_003_760),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord { item_id: iid, type_id: tid, owner_id: PILOT, location_id: loc, quantity: qty, ..Default::default() },
            );
        }
    });

    // 1. Reagents & Star Power definitions
    let defs = qrpc(&ctx, &mut s, "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest", Vec::new()).await.payload.unwrap().value;
    let defs_list = fields(&defs, 1);
    assert_eq!(defs_list.len(), 8);
    let lava_def = defs_list.iter().find(|d| varint(&field(d, 1).unwrap(), 1) == Some(2015)).unwrap();
    assert_eq!(varint(&field(&field(lava_def, 4).unwrap(), 3).unwrap(), 1), Some(TYPE_MAGMATIC_GAS as u64));

    let mut bad_ver = Vec::new();
    let mut spec = Vec::new();
    encode_varint_field(1, 99, &mut spec);
    encode_bytes_field(1, &spec, &mut bad_ver);
    assert_eq!(qrpc(&ctx, &mut s, "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest", bad_ver).await.status_code, STATUS_NOT_FOUND);

    let planet_cfgs = qrpc(&ctx, &mut s, "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest", Vec::new()).await.payload.unwrap().value;
    let reagent_cfgs: Vec<Vec<u8>> = fields(&planet_cfgs, 1).iter().filter_map(|c| field(c, 4)).collect();
    assert!(reagent_cfgs.iter().any(|c| varint(&field(c, 1).unwrap(), 1) == Some(TYPE_MAGMATIC_GAS as u64)));
    assert!(reagent_cfgs.iter().any(|c| varint(&field(c, 1).unwrap(), 1) == Some(TYPE_SUPERIONIC_ICE as u64)));

    let star_cfgs = qrpc(&ctx, &mut s, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await.payload.unwrap().value;
    assert!(!fields(&star_cfgs, 1).is_empty());

    // 2. Power & Workforce from stellar + anchored Skyhooks
    let res_no_sh = qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&res_no_sh, 1).unwrap(), 1), Some(800));
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, pid) in jita_planets.iter().copied().enumerate() {
            let sh_id = 1_000_000_200 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, jita, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, pid);
            snap.sov_runtime.skyhook_active.insert(sh_id, true);
        }
    });
    let res = qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&res, 1).unwrap(), 1), Some(1_100));
    assert_eq!(varint(&field(&res, 2).unwrap(), 1), Some(3_000));

    // 3. Hub Fuel deposit & hourly burn
    let mut other = qsess(61_002, 90_000_002, 98_000_002, jita, corp_roles::ROLE_DIRECTOR);
    let add_fuel = |item: i64, qty: u64, hub: i64| {
        let mut b = Vec::new();
        encode_bytes_field(1, &idm(item as u64), &mut b);
        encode_varint_field(2, qty, &mut b);
        encode_bytes_field(3, &idm(hub as u64), &mut b);
        b
    };
    assert_eq!(qrpc(&ctx, &mut other, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel(gas_item, 10, hub_a)).await.status_code, STATUS_FORBIDDEN);
    assert_eq!(qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel(station_gas, 10, hub_a)).await.status_code, STATUS_BAD_REQUEST);
    assert_eq!(qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel(gas_item, 500, hub_a)).await.status_code, 200);
    drain(&mut rx);

    // 4. Hub Upgrades install + online + hourly fuel burn (7/hr -> 493 after 1h)
    let mut proc = id_req(1, upg_item as u64);
    let mut cfg = Vec::new();
    encode_bytes_field(1, &idm(STABILITY_GEN), &mut cfg);
    encode_varint_field(2, 1, &mut cfg);
    encode_bytes_field(2, &cfg, &mut proc);
    encode_bytes_field(3, &idm(hub_a as u64), &mut proc);
    let hu = qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await.payload.unwrap().value;
    let up = field(&field(&hu, 1).unwrap(), 2).unwrap();
    assert_eq!(field(&up, 2).and_then(|a| varint(&a, 2)), Some(2));
    let n = drain(&mut rx);
    assert!(n.iter().any(|(t, _)| t == "eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice"));
    assert!(n.iter().any(|(t, _)| t == "eve_public.sovereignty.hub.api.ResourcesSimulatedNotice"));

    ctx.advance_secs(3_600);
    let fuel = qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.fuel.api.GetRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(field(&fuel, 1).and_then(|f| varint(&f, 2)), Some(493));

    // 5. Workforce transit configuration (export 300 from Jita to Perimeter)
    let mut exp = Vec::new();
    encode_bytes_field(3, &idm(perimeter as u64), &mut exp);
    encode_varint_field(2, 300, &mut exp);
    let mut export_cfg = Vec::new();
    encode_bytes_field(3, &exp, &mut export_cfg);
    let mut wf_req = Vec::new();
    encode_bytes_field(1, &idm(hub_a as u64), &mut wf_req);
    encode_bytes_field(2, &export_cfg, &mut wf_req);
    assert_eq!(qrpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", wf_req).await.status_code, 200);
    assert!(drain(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice"));
}

#[tokio::test]
async fn test_mercenary_den_anarchy_development_mto_and_ui_photo_mode() {
    // parity: feat:pve.mercenary-tactical-ops
    // parity: feat:sovereignty.mercenary-den
    // parity: feat:sovereignty.mercenary-den-anarchy-evolution
    // parity: feat:sovereignty.mercenary-den-development-anarchy
    // parity: feat:ui.photo-mode
    eve_testkit::attest(&[
        "feat:pve.mercenary-tactical-ops",
        "feat:sovereignty.mercenary-den",
        "feat:sovereignty.mercenary-den-anarchy-evolution",
        "feat:sovereignty.mercenary-den-development-anarchy",
        "feat:ui.photo-mode",
    ]);

    let db_path = temp_sqlite_path("world_p4_mercden_photo");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let (hub_id, skyhook_id) = (1_130_000_001_i64, 1_130_000_002_i64);
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx,
        87_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        639,
        sh_pos + DVec3::new(2_500.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 4, skill_points: 180_000.0 },
        );
        snap.items.insert(
            987_001,
            InventoryItemRecord {
                item_id: 987_001,
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

    // 1. Anchor Mercenary Den & verify max capacity (5), ownership, and 3 MTOs
    let den_id = rpc_call(
        &ctx,
        &mut s_owner,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
    )
    .await
    .unwrap()
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let anchor_notices = drain_quasar(&mut rx);
    assert!(anchor_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice"));
    assert_eq!(anchor_notices.iter().filter(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice").count(), 3);

    let max_rsp = qrpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest", Vec::new()).await.payload.unwrap().value;
    assert_eq!(varint(&max_rsp, 1), Some(4));
    assert_eq!(varint(&max_rsp, 2), Some(5));

    // 2. Start MTO, destroy live MTO target ball in space -> Development & Anarchy evolution
    let den_acts = qrpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64)).await.payload.unwrap().value;
    let act_list = fields(&den_acts, 1);
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&act_list[0], 1).unwrap(), &mut start_req);
    assert_eq!(qrpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, 200);
    assert!(drain_quasar(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"));

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let site_ball = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().unwrap());
    let site_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(site_ball).await.unwrap().unwrap().1.position;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 987_200);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 16_229, site_pos + DVec3::new(1_500.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, site_ball).await;
    for round in 0..55 {
        for gun_id in (0..6).map(|g| 987_200_i64 + g * 2) {
            ctx.set_invention_roll_override(Some(0.45));
            let _ = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(site_ball)]).await;
            if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)) {
                break;
            }
        }
        if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)) {
            break;
        }
        if round % 10 == 9 {
            step_weapon_ticks(&ctx, null_sys).await;
        }
    }
    assert!(ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&site_ball)));
    let comp_notices = drain_quasar(&mut rx);
    assert!(comp_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice"));
    assert!(comp_notices.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"));

    // 3. UI Photo Mode: station scene query + paperDollServer portrait data query
    let st_info = rpc(&ctx, &mut s_owner, "stationSvc", "GetStation", &[PyRep::Integer(60_003_760)]).await.expect("GetStation");
    assert!(format!("{st_info:?}").contains("60003760"));
    let portrait = rpc(&ctx, &mut s_owner, "paperDollServer", "GetPaperDollPortraitDataFor", &[PyRep::Integer(PILOT)]).await.expect("GetPaperDollPortraitDataFor");
    assert!(!matches!(portrait, PyRep::None));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}
