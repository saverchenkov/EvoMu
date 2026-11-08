    use super::*;

// Part 2 of `ESI-3` Sovereignty, Incursions, Wars, and Single-Killmail acceptance tests against
// the real SDE (`SdeRegistry`) and live `WorldRepository` state, validated against `OpenApiSpec::pinned()`.


use crate::esi_universe_sov_complete::{
    call, issue_token_with_scopes, structure, sys_state, type_in_group,
};
use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, KillmailAttackerDetail,
    KillmailDetailRecord, KillmailRecord, TrainedSkillRecord, WarAllyRecord, WarLifecycleMeta,
    WarRecord,
};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
use eve_server::services::sov_gameplay::{
    ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_NODE_BASE_CAPTURE_SECS,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_067_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

async fn setup_entosis_pilot(
    ctx: &ServerContext,
    session_id: u64,
    char_id: i64,
    corp_id: i64,
    alliance_id: i64,
    ship_id: i64,
    mod_id: i64,
    stront_id: i64,
    sys_id: u32,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&90_000_001].clone();
        let ch = snap.characters.entry(char_id).or_insert(tpl);
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.alliance_id = Some(alliance_id);
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.skills.entry(char_id).or_default().insert(
            3_426,
            TrainedSkillRecord { type_id: 3_426, level: 5, skill_points: 256_000.0 },
        );
        for (iid, tid, loc, flag, qty, sing) in [
            (ship_id, 621u32, sys_id as i64, InventoryFlag::None, 1i32, true),
            (mod_id, 34_593, ship_id, InventoryFlag::HiSlot0, 1, true),
            (stront_id, 16_275, ship_id, InventoryFlag::Cargo, 500, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: sing,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(mod_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let mut sess = SessionState::new(session_id);
    sess.user_id = Some(session_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.alliance_id = Some(alliance_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    let _ = ctx.db.acquire_online_lock(session_id, session_id, Some(char_id), true, ctx.now_unix());
    sess
}

async fn entosis_target_to_completion(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    mod_id: i64,
    target_id: i64,
    target_sys: u32,
    target_pos: [f64; 3],
    wait_secs: u64,
) {
    let prev_sys = sess.solar_system_id.unwrap_or(target_sys);
    if prev_sys != target_sys {
        let _ = ctx.systems.get_or_spawn(prev_sys).remove_ball(ship_id).await;
    }
    sess.solar_system_id = Some(target_sys);
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = target_sys;
            ch.active_ship_id = ship_id;
        }
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.location_id = target_sys as i64;
        }
    });
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 621);
    let sys_handle = ctx.systems.get_or_spawn(target_sys);
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(ship_id, 621, DVec3::from_array(target_pos) + DVec3::new(2_500.0, 0.0, 0.0)), cs)
        .await;
    crate::sovereignty_equinox_gameplay::lock_target_via_rpc(ctx, sess, target_sys, ship_id, target_id).await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(mod_id), PyRep::String("default".into()), PyRep::Integer(target_id)],
        &kw,
        None,
    )
    .await
    .expect("dogmaIM.Activate Entosis Link I");
    ctx.advance_secs(wait_secs);
    tick_background_cluster(ctx).await;
}

fn pick_node_for_structure(ctx: &ServerContext, structure_id: i64) -> (i64, u32, [f64; 3]) {
    ctx.db.with_snapshot(|s| {
        let (&nid, &(_, nsys, _)) = s
            .sov_runtime
            .command_nodes
            .iter()
            .find(|(_, &(hid, _, _))| hid == structure_id)
            .expect("active command node for structure");
        (nid, nsys, s.item_positions[&nid])
    })
}

#[tokio::test]
async fn test_esi_sovereignty_campaigns_reflect_live_command_node_events() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    let hub_type = type_in_group(&ctx, "Sovereignty Hub", true);
    let tcu_type = type_in_group(&ctx, "Territorial Claim Unit", true);
    let mut nullsecs: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status <= -0.1 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .collect();
    nullsecs.sort_unstable();
    let (sys_a, sys_b) = (nullsecs[0], nullsecs[1]);
    let const_a = ctx.sde.solar_systems[&sys_a].constellation_id as i64;
    let const_b = ctx.sde.solar_systems[&sys_b].constellation_id as i64;

    let (corp, alliance) = (98_300_001i64, 99_300_001i64);
    let (atk_corp, atk_alliance) = (98_300_002i64, 99_300_002i64);
    let (ihub_id, tcu_id) = (1_050_000_000_001i64, 1_050_000_000_002i64);

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, aid, ceo) in [
            (corp, alliance, 90_000_001i64),
            (atk_corp, atk_alliance, 90_000_002i64),
        ] {
            snap.corporations.insert(
                cid,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = cid;
                    __corp.name = format!("Sov Campaign Corp {cid}");
                    __corp.ticker = "SOVC".into();
                    __corp.ceo_id = ceo;
                    __corp.alliance_id = Some(aid);
                    __corp.members = HashSet::from([ceo]);
                    __corp },
            );
            snap.alliances.insert(
                aid,
                AllianceRecord {
                    alliance_id: aid,
                    name: format!("Sov Campaign Alliance {aid}"),
                    short_name: "SOVCA".into(),
                    executor_corp_id: cid,
                    member_corp_ids: HashSet::from([cid]),
                },
            );
        }
        snap.system_states.insert(sys_a, sys_state(sys_a, Some(alliance), 1, 1, 1));
        snap.system_states.insert(sys_b, sys_state(sys_b, Some(alliance), 1, 1, 1));
        snap.structures.insert(ihub_id, structure(ihub_id, hub_type, corp, sys_a, [0.0, 0.0, 0.0]));
        snap.structures.insert(tcu_id, structure(tcu_id, tcu_type, corp, sys_b, [0.0, 0.0, 0.0]));
    });
    let _ = ctx.systems.get_or_spawn(sys_a).spawn_ball(make_ball(ihub_id, hub_type, DVec3::ZERO)).await;
    let _ = ctx.systems.get_or_spawn(sys_b).spawn_ball(make_ball(tcu_id, tcu_type, DVec3::ZERO)).await;

    let mut s_def = setup_entosis_pilot(&ctx, 79_001, 90_000_001, corp, alliance, 991_000_001, 991_000_011, 991_000_021, sys_a).await;
    let mut s_atk = setup_entosis_pilot(&ctx, 79_002, 90_000_002, atk_corp, atk_alliance, 991_000_002, 991_000_012, 991_000_022, sys_a).await;

    // 1. Unreinforced sov structures in ShieldVulnerable state do not produce campaigns.
    assert_eq!(call(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"), 200), json!([]));

    // 2. Drive both structures into active Command Node campaigns via live Entosis Link I capture.
    let hub_wait = 320 + ((ENTOSIS_HUB_BASE_CAPTURE_SECS * 3.0).ceil() as u64) + 60;
    entosis_target_to_completion(&ctx, &mut s_atk, 90_000_002, 991_000_002, 991_000_012, ihub_id, sys_a, [0.0, 0.0, 0.0], hub_wait).await;
    let ihub_start_unix = ctx.db.with_snapshot(|s| s.structures[&ihub_id].reinforced_until_unix);

    entosis_target_to_completion(&ctx, &mut s_atk, 90_000_002, 991_000_002, 991_000_012, tcu_id, sys_b, [0.0, 0.0, 0.0], hub_wait).await;
    let tcu_start_unix = ctx.db.with_snapshot(|s| s.structures[&tcu_id].reinforced_until_unix);

    let active = call(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"), 200);
    let arr = active.as_array().expect("campaigns array");
    assert_eq!(arr.len(), 2);

    let fmt = eve_server::esi::sample_routes::format_iso8601_utc;
    let c0 = &arr[0];
    assert_eq!(c0["campaign_id"], json!(ihub_id));
    assert_eq!(c0["structure_id"], json!(ihub_id));
    assert_eq!(c0["solar_system_id"], json!(sys_a as i64));
    assert_eq!(c0["constellation_id"], json!(const_a));
    assert_eq!(c0["event_type"], json!("ihub_defense"));
    assert_eq!(c0["defender_id"], json!(alliance));
    assert_eq!(c0["defender_score"], json!(0.6));
    assert_eq!(c0["attackers_score"], json!(0.4));
    assert_eq!(c0["start_time"], json!(fmt(ihub_start_unix)));

    let c1 = &arr[1];
    assert_eq!(c1["campaign_id"], json!(tcu_id));
    assert_eq!(c1["structure_id"], json!(tcu_id));
    assert_eq!(c1["solar_system_id"], json!(sys_b as i64));
    assert_eq!(c1["constellation_id"], json!(const_b));
    assert_eq!(c1["event_type"], json!("tcu_defense"));
    assert_eq!(c1["defender_id"], json!(alliance));
    assert_eq!(c1["defender_score"], json!(0.6));
    assert_eq!(c1["attackers_score"], json!(0.4));
    assert_eq!(c1["start_time"], json!(fmt(tcu_start_unix)));

    // 3. Command nodes only spawn once the 48h reinforcement timers expire (no nodes before that).
    assert!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.is_empty()), "no nodes during reinforcement");
    ctx.advance_secs(tcu_start_unix.saturating_sub(ctx.now_unix()) + 60);
    tick_background_cluster(&ctx).await;

    // 4. Live Command Node captures update campaign scores: an attacker capture on the ihub moves it to
    // 0.53/0.47; defender captures (plus unattended regeneration) resolve the TCU campaign.
    let node_wait = 320 + ((ENTOSIS_NODE_BASE_CAPTURE_SECS * 3.0).ceil() as u64) + 60;
    let (ihub_node, ihub_node_sys, ihub_node_pos) = pick_node_for_structure(&ctx, ihub_id);
    entosis_target_to_completion(&ctx, &mut s_atk, 90_000_002, 991_000_002, 991_000_012, ihub_node, ihub_node_sys, ihub_node_pos, node_wait).await;
    let after_atk = call(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"), 200);
    let ihub_row = after_atk.as_array().unwrap().iter().find(|c| c["structure_id"] == json!(ihub_id)).cloned().expect("ihub campaign");
    assert_eq!(ihub_row["defender_score"], json!(0.53));
    assert_eq!(ihub_row["attackers_score"], json!(0.47));

    for _ in 0..6 {
        if ctx.db.with_snapshot(|s| !s.sov_runtime.command_nodes.values().any(|&(hid, _, _)| hid == tcu_id)) {
            break;
        }
        let (tcu_node, tcu_node_sys, tcu_node_pos) = pick_node_for_structure(&ctx, tcu_id);
        entosis_target_to_completion(&ctx, &mut s_def, 90_000_001, 991_000_001, 991_000_011, tcu_node, tcu_node_sys, tcu_node_pos, node_wait).await;
    }

    let updated = call(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"), 200);
    let upd_arr = updated.as_array().unwrap();
    assert!(upd_arr.iter().all(|c| c["structure_id"] != json!(tcu_id)), "resolved TCU campaign leaves ESI");
    if let Some(row) = upd_arr.iter().find(|c| c["structure_id"] == json!(ihub_id)) {
        assert!(row["defender_score"].as_f64().unwrap() > 0.53, "unattended ihub nodes regenerate for the defender");
    }

    call(&router, EsiHttpRequest::new("POST", "/v1/sovereignty/campaigns/"), 405);
    call(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/?datasource=invalid"), 400);
}

#[tokio::test]
async fn test_esi_incursions_wars_and_single_killmail_reflect_live_state() {
    // parity: esi:GET /killmails/{killmail_id}/{killmail_hash}
    // parity: feat:esi.killmails.single, feat:esi.wars.war
    eve_testkit::attest(&[
        "esi:GET /killmails/{killmail_id}/{killmail_hash}",
        "feat:esi.killmails.single",
        "feat:esi.wars.war",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();
    let fmt = eve_server::esi::sample_routes::format_iso8601_utc;

    // 1. Incursions (`GET /v1/incursions/`) driven by live `ensure_special_sites_spawned` and site boss combat
    let sansha_fid = ctx.sde.factions.values().find(|f| f.name.contains("Sansha")).expect("Sansha faction in SDE").faction_id as i64;
    assert_eq!(call(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200), json!([]));

    ensure_special_sites_spawned(&ctx).await;

    let incursions_initial = call(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let inc_arr = incursions_initial.as_array().expect("incursions array");
    assert_eq!(inc_arr.len(), 3, "3 live Sansha incursion constellations spawned");
    let first_inc = &inc_arr[0];
    let target_cid = first_inc["constellation_id"].as_i64().unwrap() as u32;
    let staging_sys = first_inc["staging_solar_system_id"].as_i64().unwrap() as u32;
    assert_eq!(first_inc["faction_id"], json!(sansha_fid));
    assert!(first_inc["infested_solar_systems"].as_array().unwrap().contains(&json!(staging_sys as i64)));
    assert_eq!(first_inc["state"], json!("established"));
    assert_eq!(first_inc["influence"], json!(1.0));
    assert_eq!(first_inc["has_boss"], json!(false));
    assert_eq!(first_inc["type"], json!("Incursion"));

    // Clear HQ site escort waves and destroy the HQ boss 4 times via live system combat (`apply_damage_with_attacker`)
    // to drop influence from 1.0 -> 0.75 -> 0.0 and spawn the Sansha Mothership (`state: "mobilizing"`, `has_boss: true`).
    let (hq_sys, hq_site_id, ms_site_id) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        let hq = inc.sites.iter().find(|s| s.tier == "Headquarters").unwrap();
        let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
        (inc.hq_solar_system_id, hq.site_id.clone(), ms.site_id.clone())
    });
    let hq_handle = ctx.systems.get_or_spawn(hq_sys);
    let lethal = DamageVector { em: 500_000.0, thermal: 500_000.0, kinetic: 500_000.0, explosive: 500_000.0 };
    for run in 0..4 {
        let hq_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &hq_site_id).await;
        let (_, dead, _) = hq_handle.apply_damage_with_attacker(hq_boss_bid, lethal, Some(90_000_001)).await.unwrap().unwrap();
        assert!(dead);
        if run == 0 {
            let after_one = call(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
            let row = after_one.as_array().unwrap().iter().find(|r| r["constellation_id"] == json!(target_cid as i64)).unwrap();
            assert!((row["influence"].as_f64().unwrap() - 0.75).abs() < 1e-6);
        }
    }

    let incursions_mobilizing = call(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let mob_row = incursions_mobilizing.as_array().unwrap().iter().find(|r| r["constellation_id"] == json!(target_cid as i64)).unwrap();
    assert_eq!(mob_row["influence"], json!(0.0));
    assert_eq!(mob_row["state"], json!("mobilizing"));
    assert_eq!(mob_row["has_boss"], json!(true));

    // Defeat the Mothership encounter via live combat -> state transitions to `"withdrawing"`, `has_boss: false`.
    let ms_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &ms_site_id).await;
    let mut ms_lethal = lethal;
    ms_lethal.em = 20_000_000.0;
    ms_lethal.thermal = 20_000_000.0;
    ms_lethal.kinetic = 20_000_000.0;
    ms_lethal.explosive = 20_000_000.0;
    let (_, ms_dead, _) = hq_handle.apply_damage_with_attacker(ms_boss_bid, ms_lethal, Some(90_000_001)).await.unwrap().unwrap();
    assert!(ms_dead);

    let incursions_withdrawing = call(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let with_row = incursions_withdrawing.as_array().unwrap().iter().find(|r| r["constellation_id"] == json!(target_cid as i64)).unwrap();
    assert_eq!(with_row["influence"], json!(0.0));
    assert_eq!(with_row["state"], json!("withdrawing"));
    assert_eq!(with_row["has_boss"], json!(false));

    // 2. Single Killmail (`GET /v1/killmails/{killmail_id}/{killmail_hash}/`)
    let (attacker_id, wingman_id, victim_id) = (90_000_001_i64, 90_000_003_i64, 90_000_002_i64);
    let (aggr_corp, def_corp, non_ally_member_corp, explicit_ally_corp) = (98_400_001_i64, 98_400_002_i64, 98_400_003_i64, 98_400_004_i64);
    let def_alliance = 99_400_001_i64;
    let (victim_ship_item_id, victim_cargo_item_id) = (990_100_001_i64, 990_100_002_i64);
    let killmail_id = 77_901_i64;
    let km_time_unix = now.saturating_sub(1_200);
    let war_id = 551_i64;
    let war_declared_unix = now.saturating_sub(100_000);
    let war_started_unix = war_declared_unix + 86_400;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&attacker_id) {
            ch.corporation_id = aggr_corp;
            ch.security_status = 1.75;
        }
        if let Some(ch) = snap.characters.get_mut(&victim_id) {
            ch.corporation_id = def_corp;
            ch.active_ship_id = victim_ship_item_id;
        }
        for (iid, loc, flag, tid, qty, sing) in [
            (victim_ship_item_id, 30_000_142i64, InventoryFlag::Hangar, 587u32, 1i32, true),
            (victim_cargo_item_id, victim_ship_item_id, InventoryFlag::Cargo, 34, 250, false),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    owner_id: victim_id,
                    location_id: loc,
                    flag,
                    type_id: tid,
                    quantity: qty,
                    singleton: sing,
                    ..Default::default()
                },
            );
        }
        snap.killmails.insert(
            killmail_id,
            KillmailRecord {
                kill_id: killmail_id,
                victim_character_id: victim_id,
                attacker_character_id: attacker_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 2_500_000.0,
                ..Default::default()
            },
        );
        snap.killmail_details.insert(
            killmail_id,
            KillmailDetailRecord {
                kill_id: killmail_id,
                killmail_time_unix: km_time_unix,
                damage_taken: 1_840,
                victim_position: Some([12_500.0, -4_000.0, 8_500.0]),
                attackers: vec![
                    KillmailAttackerDetail {
                        character_id: attacker_id,
                        corporation_id: aggr_corp,
                        alliance_id: None,
                        ship_type_id: 587,
                        weapon_type_id: 484,
                        damage_done: 1_200,
                        final_blow: true,
                        security_status: 1.75,
                    },
                    KillmailAttackerDetail {
                        character_id: wingman_id,
                        corporation_id: aggr_corp,
                        alliance_id: None,
                        ship_type_id: 585,
                        weapon_type_id: 484,
                        damage_done: 640,
                        final_blow: false,
                        security_status: 0.5,
                    },
                ],
            },
        );
        snap.combat_crimewatch_state.detailed_killmails.insert(
            killmail_id,
            eve_db::DetailedKillmail {
                kill_id: killmail_id,
                kill_time_unix: km_time_unix,
                solar_system_id: 30_000_142,
                victim_character_id: victim_id,
                victim_corporation_id: def_corp,
                victim_alliance_id: Some(def_alliance),
                victim_ship_type_id: 587,
                victim_damage_taken: 1_840.0,
                victim_position: [12_500.0, -4_000.0, 8_500.0],
                attackers: vec![
                    eve_db::KillmailAttackerEntry {
                        character_id: attacker_id,
                        corporation_id: aggr_corp,
                        alliance_id: None,
                        ship_type_id: 587,
                        weapon_type_id: 484,
                        damage_done: 1_200.0,
                        final_blow: true,
                        security_status: 1.75,
                    },
                    eve_db::KillmailAttackerEntry {
                        character_id: wingman_id,
                        corporation_id: aggr_corp,
                        alliance_id: None,
                        ship_type_id: 585,
                        weapon_type_id: 484,
                        damage_done: 640.0,
                        final_blow: false,
                        security_status: 0.5,
                    },
                ],
                dropped_items: vec![],
                destroyed_items: vec![eve_db::KillmailItemEntry {
                    type_id: 34,
                    flag: InventoryFlag::Cargo as u16,
                    qty_dropped: 0,
                    qty_destroyed: 250,
                    singleton: false,
                }],
                total_value_isk: 2_500_000.0,
                killmail_hash: String::new(),
            },
        );
        for (cid, aid) in [
            (aggr_corp, None),
            (def_corp, Some(def_alliance)),
            (non_ally_member_corp, Some(def_alliance)),
            (explicit_ally_corp, None),
        ] {
            snap.corporations.insert(
                cid,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = cid;
                    __corp.name = format!("War Corp {cid}");
                    __corp.ticker = "WARC".into();
                    __corp.ceo_id = attacker_id;
                    __corp.alliance_id = aid;
                    __corp.members = HashSet::from([attacker_id]);
                    __corp },
            );
        }
        snap.alliances.insert(
            def_alliance,
            AllianceRecord {
                alliance_id: def_alliance,
                name: "Defender Coalition".into(),
                short_name: "DEFC".into(),
                executor_corp_id: def_corp,
                member_corp_ids: HashSet::from([def_corp, non_ally_member_corp]),
            },
        );
        snap.wars.insert(
            war_id,
            WarRecord {
                war_id,
                aggressor_corp_id: aggr_corp,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: war_started_unix,
                active: true,
            },
        );
        snap.war_lifecycle_meta.insert(
            war_id,
            WarLifecycleMeta {
                declared_unix: Some(war_declared_unix),
                started_unix: Some(war_started_unix),
                retracted_unix: None,
                finished_unix: None,
                open_for_allies: Some(true),
                allies: vec![WarAllyRecord {
                    corporation_id: Some(explicit_ally_corp),
                    alliance_id: None,
                }],
            },
        );
    });

    let km_token = issue_token_with_scopes(&ctx, attacker_id, vec!["esi-killmails.read_killmails.v1".to_string()]);
    let recent_kms = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/characters/{attacker_id}/killmails/recent/")).with_bearer_token(&km_token),
        200,
    );
    let km_hash = recent_kms
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["killmail_id"] == json!(killmail_id))
        .expect("recent killmail entry")["killmail_hash"]
        .as_str()
        .expect("killmail_hash string")
        .to_string();

    let km_detail = call(&router, EsiHttpRequest::new("GET", format!("/v1/killmails/{killmail_id}/{km_hash}/")), 200);
    assert_eq!(km_detail["killmail_id"], json!(killmail_id));
    assert_eq!(km_detail["killmail_time"], json!(fmt(km_time_unix)));
    assert_eq!(km_detail["solar_system_id"], json!(30_000_142));
    assert_eq!(km_detail["victim"]["character_id"], json!(victim_id));
    assert_eq!(km_detail["victim"]["corporation_id"], json!(def_corp));
    assert_eq!(km_detail["victim"]["ship_type_id"], json!(587));
    assert_eq!(km_detail["victim"]["damage_taken"], json!(1_840));
    assert_eq!(km_detail["victim"]["position"], json!({ "x": 12_500.0, "y": -4_000.0, "z": 8_500.0 }));
    assert_eq!(
        km_detail["victim"]["items"],
        json!([{
            "item_type_id": 34,
            "flag": InventoryFlag::Cargo as i64,
            "singleton": 0,
            "quantity_destroyed": 250,
        }])
    );
    assert_eq!(km_detail["attackers"].as_array().unwrap().len(), 2);
    assert_eq!(km_detail["attackers"][0]["character_id"], json!(attacker_id));
    assert_eq!(km_detail["attackers"][0]["corporation_id"], json!(aggr_corp));
    assert_eq!(km_detail["attackers"][0]["ship_type_id"], json!(587));
    assert_eq!(km_detail["attackers"][0]["weapon_type_id"], json!(484));
    assert_eq!(km_detail["attackers"][0]["damage_done"], json!(1_200));
    assert_eq!(km_detail["attackers"][0]["final_blow"], json!(true));
    assert_eq!(km_detail["attackers"][0]["security_status"], json!(1.75));
    assert_eq!(km_detail["attackers"][1]["character_id"], json!(wingman_id));
    assert_eq!(km_detail["attackers"][1]["damage_done"], json!(640));
    assert_eq!(km_detail["attackers"][1]["final_blow"], json!(false));

    let legacy_guess_hash = format!("{:040x}", (killmail_id as u64).wrapping_mul(2_654_435_761));
    call(&router, EsiHttpRequest::new("GET", format!("/v1/killmails/{killmail_id}/{legacy_guess_hash}/")), 422);
    call(&router, EsiHttpRequest::new("GET", format!("/v1/killmails/{killmail_id}/deadbeef/")), 422);
    call(&router, EsiHttpRequest::new("GET", format!("/v1/killmails/99999999/{km_hash}/")), 404);

    // 3. Wars detail (`GET /v1/wars/{war_id}/`)
    let war_json = call(&router, EsiHttpRequest::new("GET", format!("/v1/wars/{war_id}/")), 200);
    assert_eq!(war_json["id"], json!(war_id));
    assert_eq!(war_json["declared"], json!(fmt(war_declared_unix)));
    assert_eq!(war_json["started"], json!(fmt(war_started_unix)));
    assert_eq!(war_json["mutual"], json!(false));
    assert_eq!(war_json["open_for_allies"], json!(true));
    assert_eq!(war_json["aggressor"]["corporation_id"], json!(aggr_corp));
    assert_eq!(war_json["aggressor"]["ships_killed"], json!(1));
    assert_eq!(war_json["aggressor"]["isk_destroyed"], json!(2_500_000.0));
    assert_eq!(war_json["defender"]["corporation_id"], json!(def_corp));
    assert_eq!(war_json["defender"]["ships_killed"], json!(0));
    assert_eq!(war_json["defender"]["isk_destroyed"], json!(0.0));
    assert_eq!(war_json["allies"], json!([{ "corporation_id": explicit_ally_corp }]));
    assert!(war_json.get("retracted").is_none());
    assert!(war_json.get("finished").is_none());

    let war_retracted_unix = war_started_unix + 36_000;
    let war_finished_unix = war_retracted_unix + 86_400;
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            killmail_id + 1,
            KillmailRecord {
                kill_id: killmail_id + 1,
                victim_character_id: attacker_id,
                attacker_character_id: victim_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 1_750_000.0,
                ..Default::default()
            },
        );
        if let Some(w) = snap.wars.get_mut(&war_id) {
            w.mutual = true;
            w.active = false;
        }
        if let Some(meta) = snap.war_lifecycle_meta.get_mut(&war_id) {
            meta.open_for_allies = Some(false);
            meta.retracted_unix = Some(war_retracted_unix);
            meta.finished_unix = Some(war_finished_unix);
        }
    });
    let war_concluded = call(&router, EsiHttpRequest::new("GET", format!("/v1/wars/{war_id}/")), 200);
    assert_eq!(war_concluded["mutual"], json!(true));
    assert_eq!(war_concluded["open_for_allies"], json!(false));
    assert_eq!(war_concluded["defender"]["ships_killed"], json!(1));
    assert_eq!(war_concluded["defender"]["isk_destroyed"], json!(1_750_000.0));
    assert_eq!(war_concluded["retracted"], json!(fmt(war_retracted_unix)));
    assert_eq!(war_concluded["finished"], json!(fmt(war_finished_unix)));

    call(&router, EsiHttpRequest::new("GET", "/v1/wars/99999999/"), 404);
}
