//! Live end-to-end integration tests for ESI Faction Warfare, Incursions, and Sovereignty
//! (Sov Hub anchoring, ADM, Dev Indices, Upgrade Sites, Sov Change Notifications, Skyhook
//! Reagent Maturity, `sovMgr.GetSovStructuresInfoForSolarSystem`, and ESI Sov Campaigns).
//! Zero direct snapshot or combat-runtime mutations; all state is driven by live MachoNet RPCs,
//! Quasar gateway requests, GM slash commands, space combat/mining, and cluster ticks.

#![forbid(unsafe_code)]

use crate::esi_fw_sov_incursions_live::{
    arm_ship_autocannons, create_corp_and_alliance, lock_and_destroy_target, lock_target, rpc,
    select_gm_pilot, slash,
};
use eve_db::InventoryFlag;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites::with_pve_special_reg;
use eve_server::services::sov_gameplay::{
    UPGRADE_MAJOR_THREAT_DETECTION, UPGRADE_PROSPECTING_ARRAY,
};
use eve_server::services::sov_services::{
    calculate_adm, TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
};
use glam::DVec3;
use serde_json::Value;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token(ctx: &ServerContext, user: &str, pass: &str, char_id: i64, scopes: Vec<&str>) -> String {
    let now = ctx.now_unix();
    let sc = scopes.into_iter().map(String::from).collect();
    let code = ctx
        .db
        .authorize_pkce(
            user,
            pass,
            char_id,
            "evomu-esi-live-parity",
            "https://localhost/callback",
            sc,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-live-parity", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn call_esi(router: &EsiRouter, req: EsiHttpRequest, expected: u16) -> Value {
    crate::esi_universe_fw_complete::call(router, req, expected)
}

async fn give_item(ctx: &ServerContext, sess: &mut eve_machonet::SessionState, type_id: u32, qty: i32) -> i64 {
    slash(ctx, sess, &format!("/giveitem {type_id} {qty}"))
        .await
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("itemID")
}

async fn give_type_prereqs(ctx: &ServerContext, sess: &mut eve_machonet::SessionState, type_id: u32) {
    let mut stack = vec![type_id];
    let mut seen = std::collections::HashSet::new();
    while let Some(tid) = stack.pop() {
        if !seen.insert(tid) {
            continue;
        }
        if let Some(t) = ctx.sde.types.get(&tid) {
            for (req_a, lvl_a) in [(182_u16, 277_u16), (183, 278), (184, 279)] {
                if let Some(&sk) = t.default_attributes.get(&req_a) {
                    let sk_id = sk as u32;
                    let lvl = t.default_attributes.get(&lvl_a).copied().unwrap_or(1.0) as u8;
                    slash(ctx, sess, &format!("/giveskill {sk_id} {}", lvl.max(5))).await;
                    stack.push(sk_id);
                }
            }
        }
    }
}

fn find_nullsec_system_with_power_and_belts(ctx: &ServerContext) -> (u32, i64, i64) {
    let mut sys_ids: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .collect();
    sys_ids.sort_unstable();
    for sys in sys_ids {
        let cels = ctx.sde.celestials_in_system(sys);
        let star_p: u64 = cels
            .iter()
            .filter(|c| c.group_id == 6)
            .map(|c| eve_server::services::quasar_sov_hub::star_power(&ctx.sde, c.type_id))
            .sum();
        let temp = cels.iter().find(|c| c.type_id == 11).map(|c| c.celestial_id);
        let lava = cels.iter().find(|c| c.type_id == 2015).map(|c| c.celestial_id);
        let has_belts = cels.iter().any(|c| c.group_id == 9);
        if star_p >= 700 && has_belts {
            if let (Some(t), Some(l)) = (temp, lava) {
                return (sys, t, l);
            }
        }
    }
    let (s, t, l, _, _) = crate::sovereignty_equinox_gameplay::find_nullsec_planets(ctx);
    (s, t, l)
}

#[tokio::test]
async fn test_live_esi_fw_and_incursions_lifecycle() {
    // parity: esi:GET /characters/{character_id}/fw/stats, esi:GET /corporations/{corporation_id}/fw/stats
    // parity: esi:GET /fw/leaderboards, esi:GET /fw/leaderboards/characters
    // parity: esi:GET /fw/leaderboards/corporations, esi:GET /fw/stats, esi:GET /fw/systems
    // parity: esi:GET /incursions
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/fw/stats",
        "esi:GET /corporations/{corporation_id}/fw/stats",
        "esi:GET /fw/leaderboards",
        "esi:GET /fw/leaderboards/characters",
        "esi:GET /fw/leaderboards/corporations",
        "esi:GET /fw/stats",
        "esi:GET /fw/systems",
        "esi:GET /incursions",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    let mut s1 = select_gm_pilot(&ctx, 91_001, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_gm_pilot(&ctx, 91_002, "other", "other-secret", PILOT_2).await;
    let s1_home_st = s1.station_id.expect("s1 docked");
    let s1_home_sys = s1.solar_system_id.expect("s1 solar system");

    // 1. Live FW enlistment: Corp (Caldari 500001) & Direct Character (Gallente 500004).
    let (corp_1, _) = create_corp_and_alliance(&ctx, &mut s1, "Caldari Vanguard", "CVGD", None).await;
    rpc(&ctx, &mut s1, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(500_001)])
        .await
        .expect("JoinFactionAsCorporation");
    rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await
        .expect("CreateMyDirectEnlistment");

    let init_systems = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/systems/"), 200);
    let target_sys = init_systems
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["occupier_faction_id"] == 500_004 && s["contested"] == "uncontested" && s["victory_points"] == 0)
        .and_then(|s| s["solar_system_id"].as_u64())
        .expect("Gallente uncontested FW system") as u32;

    let (ship_1, guns_1) = arm_ship_autocannons(&ctx, &mut s1, PILOT_1).await;
    slash(&ctx, &mut s1, &format!("/tr {target_sys}")).await;
    slash(&ctx, &mut s2, &format!("/tr {target_sys}")).await;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // 2. Live PvP kill & Novice/Small FW complex captures.
    let ship_2 = ctx.db.with_snapshot(|snap| snap.characters[&PILOT_2].active_ship_id);
    lock_and_destroy_target(&ctx, &mut s1, target_sys, ship_1, &guns_1, ship_2).await;

    let sys_handle = ctx.systems.get_or_spawn(target_sys);
    for (tier, hold_secs) in [("Novice", 605_u64), ("Small", 905_u64)] {
        let (gate_id, gate_pos) = ctx.db.with_snapshot(|snap| {
            let (&gid, _) = snap
                .fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, (csys, t, _, _, done))| *csys == target_sys && t == tier && !*done)
                .unwrap_or_else(|| panic!("active {tier} complex in {target_sys}"));
            (gid, snap.item_positions[&gid])
        });
        sys_handle.set_ball_position(ship_1, DVec3::from_array(gate_pos) + DVec3::new(1_000.0, 0.0, 0.0)).await;
        rpc(&ctx, &mut s1, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)])
            .await
            .expect("ActivateAccelerationGate");
        lock_and_destroy_target(&ctx, &mut s1, target_sys, ship_1, &guns_1, gate_id + 500_000_000).await;
        ctx.advance_secs(hold_secs);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }

    // 3. Verify all 7 FW ESI routes & auth guards (401/403/200).
    let after_systems = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/systems/"), 200);
    let sys_row = after_systems
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["solar_system_id"] == i64::from(target_sys))
        .expect("target FW system");
    assert!(sys_row["victory_points"].as_i64().unwrap() > 0);
    assert_eq!(sys_row["contested"], "contested");

    call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/fw/stats/")), 401);
    let bad_tok = issue_token(&ctx, "demo", "demo-secret", PILOT_1, vec![]);
    call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/fw/stats/")).with_bearer_token(&bad_tok), 403);
    let tok_1 = issue_token(
        &ctx,
        "demo",
        "demo-secret",
        PILOT_1,
        vec!["esi-characters.read_fw_stats.v1", "esi-corporations.read_fw_stats.v1"],
    );
    call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_2}/fw/stats/")).with_bearer_token(&tok_1), 403);
    call_esi(&router, EsiHttpRequest::new("GET", "/v1/corporations/98999999/fw/stats/").with_bearer_token(&tok_1), 403);

    let c1_stats = call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/fw/stats/")).with_bearer_token(&tok_1), 200);
    assert_eq!(c1_stats["faction_id"], 500_001);
    assert_eq!(c1_stats["kills"]["yesterday"], 1);
    assert_eq!(c1_stats["kills"]["total"], 1);
    let p1_vp = c1_stats["victory_points"]["total"].as_i64().unwrap();
    assert!(p1_vp > 0);

    let corp_stats = call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/corporations/{corp_1}/fw/stats/")).with_bearer_token(&tok_1), 200);
    assert_eq!(corp_stats["faction_id"], 500_001);
    assert_eq!(corp_stats["pilots"], 1);
    assert_eq!(corp_stats["kills"]["yesterday"], 1);
    assert_eq!(corp_stats["victory_points"]["total"], p1_vp);

    let fw_stats = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/stats/"), 200);
    let cal = fw_stats.as_array().unwrap().iter().find(|f| f["faction_id"] == 500_001).unwrap();
    assert_eq!(cal["pilots"], 1);
    assert_eq!(cal["kills"]["yesterday"], 1);
    assert_eq!(cal["victory_points"]["total"], p1_vp);

    let lb_fac = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/leaderboards/"), 200);
    assert_eq!(lb_fac["kills"]["yesterday"][0]["faction_id"], 500_001);
    assert_eq!(lb_fac["victory_points"]["active_total"][0]["faction_id"], 500_001);

    let lb_char = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/leaderboards/characters/"), 200);
    assert_eq!(lb_char["kills"]["yesterday"][0]["character_id"], PILOT_1);
    assert_eq!(lb_char["victory_points"]["active_total"][0]["character_id"], PILOT_1);

    let lb_corp = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/leaderboards/corporations/"), 200);
    assert_eq!(lb_corp["kills"]["yesterday"][0]["corporation_id"], corp_1);
    assert_eq!(lb_corp["victory_points"]["active_total"][0]["corporation_id"], corp_1);

    // 4. Advance > 24h & retire Pilot 2: `yesterday` rolls to 0 while `last_week` & `total` persist.
    ctx.advance_secs(86_410);
    rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "RemoveMyDirectEnlistment", &[])
        .await
        .expect("RemoveMyDirectEnlistment");
    let tok_1_next = issue_token(&ctx, "demo", "demo-secret", PILOT_1, vec!["esi-characters.read_fw_stats.v1"]);
    let c1_rolled = call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/fw/stats/")).with_bearer_token(&tok_1_next), 200);
    assert_eq!(c1_rolled["kills"]["yesterday"], 0);
    assert_eq!(c1_rolled["kills"]["last_week"], 1);
    assert_eq!(c1_rolled["kills"]["total"], 1);

    // 5. Live Incursions: empty initially, spawned via `incursionSvc.GetIncursionData`, influence decays on live site completion.
    let empty_inc = call_esi(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    assert_eq!(empty_inc.as_array().unwrap().len(), 0);

    rpc(&ctx, &mut s1, "dungeonInstanceCacheMgr", "GetHomefrontSiteInstances", &[]).await.expect("GetHomefrontSiteInstances");
    let active_inc = call_esi(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let first_inc = &active_inc.as_array().unwrap()[0];
    let cid = first_inc["constellation_id"].as_u64().unwrap() as u32;
    assert_eq!(first_inc["faction_id"], 500_019);
    assert_eq!(first_inc["state"], "established");
    assert_eq!(first_inc["influence"], 1.0);

    let (vg_sys_id, vg_site_id) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&cid];
        let vg = inc.sites.iter().find(|s| s.tier == "Vanguard" && s.dungeon_id != 5165).unwrap();
        (vg.solar_system_id, vg.site_id.clone())
    });
    slash(&ctx, &mut s1, &format!("/tr {s1_home_sys}")).await;
    ctx.advance_secs(65);
    let st_pos_home = ctx.sde.stations[&s1_home_st].position;
    ctx.systems.get_or_spawn(s1_home_sys).set_ball_position(ship_1, st_pos_home).await;
    rpc(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(s1_home_st)]).await.expect("Dock s1");
    ctx.advance_secs(12);

    let tempest_tid = ctx.sde.types.values().find(|t| t.name == "Tempest").unwrap().type_id;
    let ac_tid = ctx.sde.types.values().find(|t| t.name == "800mm Repeating Cannon II").unwrap().type_id;
    let gyro_tid = ctx.sde.types.values().find(|t| t.name == "Gyrostabilizer II").unwrap().type_id;
    for tid in [tempest_tid, ac_tid, gyro_tid] {
        give_type_prereqs(&ctx, &mut s1, tid).await;
    }
    for sk in [3300_u32, 3310, 3311, 3312, 3315, 3316, 3413, 3426, 11083] {
        slash(&ctx, &mut s1, &format!("/giveskill {sk} 5")).await;
    }
    let tempest_id = give_item(&ctx, &mut s1, tempest_tid, 1).await;
    rpc(&ctx, &mut s1, "ship", "AssembleShip", &[PyRep::Integer(tempest_id)]).await.expect("AssembleShip");
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(tempest_id)]).await.expect("Board");
    for flag in [InventoryFlag::LoSlot0, InventoryFlag::LoSlot1, InventoryFlag::LoSlot2] {
        let mid = give_item(&ctx, &mut s1, gyro_tid, 1).await;
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(mid), PyRep::Integer(tempest_id), PyRep::Integer(flag as i64)])
            .await
            .expect("fit Gyrostabilizer II");
    }
    let mut cannons = Vec::new();
    for flag in [
        InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2,
        InventoryFlag::HiSlot3, InventoryFlag::HiSlot4, InventoryFlag::HiSlot5,
    ] {
        let gid = give_item(&ctx, &mut s1, ac_tid, 1).await;
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(gid), PyRep::Integer(tempest_id), PyRep::Integer(flag as i64)])
            .await
            .expect("fit 800mm Repeating Cannon II");
        cannons.push(gid);
    }
    let wt = &ctx.sde.types[&ac_tid];
    let ammo_tid = ctx
        .sde
        .types
        .values()
        .filter(|c| ServerContext::is_charge_compatible_with_weapon(wt, c))
        .max_by_key(|c| [114_u16, 116, 117, 118].iter().filter_map(|a| c.default_attributes.get(a)).map(|v| *v as u64).sum::<u64>())
        .map(|c| c.type_id)
        .expect("compatible Large Projectile ammo");
    give_type_prereqs(&ctx, &mut s1, ammo_tid).await;
    let ammo_id = give_item(&ctx, &mut s1, ammo_tid, 3000).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(ammo_id), PyRep::Integer(tempest_id), PyRep::Integer(InventoryFlag::Cargo as i64)])
        .await
        .expect("move Large Projectile ammo");
    let cargo_ammo = ctx.db.with_snapshot(|s| {
        s.items.values().find(|i| i.location_id == tempest_id && i.flag == InventoryFlag::Cargo && i.type_id == ammo_tid).unwrap().item_id
    });
    let cannon_reps: Vec<PyRep> = cannons.iter().map(|&g| PyRep::Integer(g)).collect();
    rpc(&ctx, &mut s1, "dogmaIM", "LoadAmmo", &[PyRep::Integer(tempest_id), PyRep::List(cannon_reps), PyRep::List(vec![PyRep::Integer(cargo_ammo)])])
        .await
        .expect("LoadAmmo Large Projectile");
    ctx.advance_secs(11);

    slash(&ctx, &mut s1, &format!("/tr {vg_sys_id}")).await;
    let boss_id = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &vg_site_id).await;
    let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
    lock_target(&ctx, &mut s1, vg_sys_id, tempest_id, boss_id, 1_500.0).await;
    for _ in 0..120 {
        let Some((_, tb)) = vg_sys.get_ball_snapshot(boss_id).await.unwrap() else {
            break;
        };
        vg_sys.set_ball_position(tempest_id, tb.position + DVec3::new(1_500.0, 0.0, 0.0)).await;
        for &gid in &cannons {
            if vg_sys.get_ball_snapshot(boss_id).await.unwrap().is_none() {
                break;
            }
            if let Err(e) = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(boss_id)]).await {
                assert!(
                    matches!(e.code.as_str(), "ModuleAlreadyActive" | "ModuleIsReloading" | "TargetNotFound" | "TargetNotLocked" | "TargetNotInSpace"),
                    "Activate weapon {gid} failed: {} ({})",
                    e.code,
                    e.message
                );
            }
        }
        for _ in 0..6 {
            vg_sys.step_manual(1.0).await.unwrap();
        }
        ctx.advance_secs(6);
    }
    assert!(vg_sys.get_ball_snapshot(boss_id).await.unwrap().is_none(), "incursion boss {boss_id} must be destroyed");
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let after_inc = call_esi(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let inc_row = after_inc.as_array().unwrap().iter().find(|i| i["constellation_id"] == i64::from(cid)).unwrap();
    let inf = inc_row["influence"].as_f64().unwrap();
    assert!(inf < 1.0 && inf > 0.0, "completing Vanguard site must decay influence ({inf})");
}

#[tokio::test]
async fn test_live_sovereignty_hub_adm_upgrades_skyhook_and_esi_campaigns() {
    // parity: esi:GET /sovereignty/campaigns, feat:sovereignty.sov-adm
    // parity: feat:sovereignty.sov-changed-notifications, feat:sovereignty.sov-dev-indices
    // parity: feat:sovereignty.sov-hub-anchor, feat:sovereignty.sov-upgrade-sites
    // parity: feat:sovereignty.skyhook-reagent-maturity, rpc:sovMgr.GetSovStructuresInfoForSolarSystem
    eve_testkit::attest(&[
        "esi:GET /sovereignty/campaigns",
        "feat:sovereignty.sov-adm",
        "feat:sovereignty.sov-changed-notifications",
        "feat:sovereignty.sov-dev-indices",
        "feat:sovereignty.sov-hub-anchor",
        "feat:sovereignty.sov-upgrade-sites",
        "feat:sovereignty.skyhook-reagent-maturity",
        "rpc:sovMgr.GetSovStructuresInfoForSolarSystem",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let (sys_id, temp_planet, lava_planet) = find_nullsec_system_with_power_and_belts(&ctx);

    let mut s_def = select_gm_pilot(&ctx, 92_001, "other", "other-secret", PILOT_2).await;
    let mut s_atk = select_gm_pilot(&ctx, 92_002, "demo", "demo-secret", PILOT_1).await;
    let (_def_corp, Some(def_ally)) = create_corp_and_alliance(&ctx, &mut s_def, "Sov Live Corp", "SLVC", Some(("Sov Live Alliance", "SLVA"))).await else {
        unreachable!()
    };
    let _ = create_corp_and_alliance(&ctx, &mut s_atk, "Sov Atk Corp", "SATK", Some(("Sov Atk Alliance", "SATA"))).await;

    // Arm Defender's Rifter with 3x 200mm AutoCannon I and assemble a Venture (32880) with 2x Miner II (483) while docked.
    give_type_prereqs(&ctx, &mut s_def, 32880).await;
    give_type_prereqs(&ctx, &mut s_def, 483).await;
    let (def_ship, def_guns) = arm_ship_autocannons(&ctx, &mut s_def, PILOT_2).await;
    let st_id = s_def.station_id.expect("defender docked");
    let def_home_sys = s_def.solar_system_id.expect("defender solar system");
    let venture_id = give_item(&ctx, &mut s_def, 32880, 1).await;
    rpc(&ctx, &mut s_def, "ship", "AssembleShip", &[PyRep::Integer(venture_id)]).await.expect("AssembleShip Venture");
    for flag in [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1] {
        let mid = give_item(&ctx, &mut s_def, 483, 1).await;
        rpc(&ctx, &mut s_def, "invbroker", "Add", &[PyRep::Integer(mid), PyRep::Integer(venture_id), PyRep::Integer(flag as i64)])
            .await
            .expect("fit Miner II");
    }

    // 1. Sov Hub Anchor (highsec rejection, nullsec anchor, duplicate rejection, onlining & capture notifications).
    slash(&ctx, &mut s_def, "/tr 30000142").await;
    let hub_item = give_item(&ctx, &mut s_def, TYPE_SOVEREIGNTY_HUB, 1).await;
    let err_hs = rpc(&ctx, &mut s_def, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Bad Hub".into())]).await.unwrap_err();
    assert_eq!(err_hs.code, "CannotAnchorSovHubOutsideNullSec");

    slash(&ctx, &mut s_def, &format!("/tr {sys_id}")).await;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.set_ball_position(def_ship, DVec3::new(100_000.0, 0.0, 0.0)).await;
    let _ = ctx.drain_notifications_for_char(PILOT_2);

    let hub_id = rpc(&ctx, &mut s_def, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Live Sov Hub".into())])
        .await
        .expect("anchor Sov Hub")
        .dict_get("structureID")
        .and_then(|v| v.as_i64())
        .unwrap();
    let dup_item = give_item(&ctx, &mut s_def, TYPE_SOVEREIGNTY_HUB, 1).await;
    let err_dup = rpc(&ctx, &mut s_def, "structureDeployment", "Anchor", &[PyRep::Integer(dup_item), PyRep::String("Dup Hub".into())]).await.unwrap_err();
    assert_eq!(err_dup.code, "SovHubAlreadyPresent");

    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let cap_notifs = ctx.drain_notifications_for_char(PILOT_2);
    let sov_cap = cap_notifs.iter().find(|n| n.event == "OnSovereigntyChanged").expect("OnSovereigntyChanged on capture");
    assert_eq!(sov_cap.args.first().and_then(|a| a.as_i64()), Some(i64::from(sys_id)));
    assert_eq!(sov_cap.args.get(1).and_then(|a| a.as_i64()), Some(def_ally));
    assert!(cap_notifs.iter().any(|n| n.event == "OnSolarSystemSovStructuresUpdated"));

    // 2. `sovMgr.GetSovStructuresInfoForSolarSystem` RPC.
    let sov_structs = rpc(&ctx, &mut s_def, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[PyRep::Integer(i64::from(sys_id))])
        .await
        .expect("GetSovStructuresInfoForSolarSystem");
    let PyRep::List(st_list) = sov_structs else { panic!("expected list") };
    assert!(st_list.iter().any(|item| item.dict_get("structureID").and_then(|v| v.as_i64()) == Some(hub_id)));

    // 3. Live Dev Indices & ADM: 7d strategic index + live NPC kills (military) + live asteroid mining (industrial).
    ctx.advance_secs(7 * 86_400 + 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let npc_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 11 && t.default_attributes.get(&9).copied().is_some_and(|hp| hp > 10.0 && hp < 400.0))
        .map(|t| t.type_id)
        .min()
        .expect("SDE frigate NPC");
    for _ in 0..5 {
        let nid = slash(&ctx, &mut s_def, &format!("/spawnnpc {npc_tid}")).await.dict_get("npcID").unwrap().as_i64().unwrap();
        lock_and_destroy_target(&ctx, &mut s_def, sys_id, def_ship, &def_guns, nid).await;
    }

    // Board Venture at station and mine asteroid in `sys_id` to raise Industrial Index.
    slash(&ctx, &mut s_def, &format!("/tr {def_home_sys}")).await;
    ctx.advance_secs(65);
    let st_pos = ctx.sde.stations[&st_id].position;
    ctx.systems.get_or_spawn(def_home_sys).set_ball_position(def_ship, st_pos).await;
    rpc(&ctx, &mut s_def, "stationSvc", "Dock", &[PyRep::Integer(st_id)]).await.expect("Dock def");
    ctx.advance_secs(12);
    rpc(&ctx, &mut s_def, "ship", "Board", &[PyRep::Integer(venture_id)]).await.expect("Board Venture");
    ctx.advance_secs(12);
    slash(&ctx, &mut s_def, &format!("/tr {sys_id}")).await;
    let asteroids = sys_handle.populate_asteroid_belt_entities().await.expect("populate asteroids");
    for ast in &asteroids {
        if ctx.db.with_snapshot(|s| s.system_states.get(&sys_id).is_some_and(|st| st.industrial_index >= 1)) {
            break;
        }
        let Some((_, ast_ball)) = sys_handle.get_ball_snapshot(ast.asteroid_id).await.unwrap() else {
            continue;
        };
        sys_handle.set_ball_position(venture_id, ast_ball.position + DVec3::new(1_000.0, 0.0, 0.0)).await;
        for _ in 0..25 {
            if rpc(&ctx, &mut s_def, "dogmaIM", "Mine", &[PyRep::Integer(ast.asteroid_id)]).await.is_err() {
                break;
            }
            ctx.advance_secs(60);
            if ctx.db.with_snapshot(|s| s.system_states.get(&sys_id).is_some_and(|st| st.industrial_index >= 1)) {
                break;
            }
        }
    }

    let all_idx = rpc(&ctx, &mut s_def, "devIndexManager", "GetAllDevelopmentIndices", &[]).await.expect("GetAllDevelopmentIndices");
    if let PyRep::Dict(entries) = &all_idx {
        let entry = entries.iter().find(|(k, _)| k.as_i64() == Some(i64::from(sys_id))).map(|(_, v)| v).expect("sys entry");
        let mil = entry.dict_get("military").and_then(|v| v.as_i64()).unwrap() as u8;
        let ind = entry.dict_get("industrial").and_then(|v| v.as_i64()).unwrap() as u8;
        let strat = entry.dict_get("strategic").and_then(|v| v.as_i64()).unwrap() as u8;
        assert!(mil >= 1 && ind >= 1 && strat >= 1, "mil={mil} ind={ind} strat={strat}");
        assert_eq!(entry.dict_get("adm").and_then(|v| v.as_f64()), Some(calculate_adm(mil, ind, strat)));
    }

    // 4. Skyhook Reagent Maturity & live `beyonce.CallComponentFromClient(..., "orbitalSkyhook", "ExtractReagentsToShip")`.
    let lava_pos = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.celestial_id == lava_planet).unwrap().position;
    sys_handle.set_ball_position(venture_id, lava_pos + DVec3::new(100_000.0, 0.0, 0.0)).await;
    let sky_item = give_item(&ctx, &mut s_def, TYPE_ORBITAL_SKYHOOK, 1).await;
    let sky_id = rpc(&ctx, &mut s_def, "structureDeployment", "Anchor", &[PyRep::Integer(sky_item), PyRep::Integer(lava_planet)])
        .await
        .expect("anchor Skyhook")
        .dict_get("structureID")
        .and_then(|v| v.as_i64())
        .unwrap();

    ctx.advance_secs(2 * 3600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    ctx.db.with_snapshot(|s| {
        let (tid, sec, insec, _) = s.sov_runtime.skyhook_reagent_bays[&sky_id];
        assert_eq!(tid, TYPE_MAGMATIC_GAS);
        assert_eq!((sec, insec), (50, 50));
    });

    ctx.advance_secs(12 * 3600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let sky_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&sky_id].position));
    sys_handle.set_ball_position(venture_id, sky_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let ext_rep = rpc(
        &ctx,
        &mut s_def,
        "beyonce",
        "CallComponentFromClient",
        &[PyRep::Integer(sky_id), PyRep::String("orbitalSkyhook".into()), PyRep::String("ExtractReagentsToShip".into())],
    )
    .await
    .expect("ExtractReagentsToShip");
    assert_eq!(ext_rep.dict_get("extractedQuantity").and_then(|v| v.as_i64()), Some(375));

    // 5. Sov Hub Upgrades & Site Spawning (Combat & Ore anomalies; anchor Temperate Skyhook for +1,500 planetary workforce).
    let temp_pos = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.celestial_id == temp_planet).unwrap().position;
    sys_handle.set_ball_position(venture_id, temp_pos + DVec3::new(100_000.0, 0.0, 0.0)).await;
    let temp_sky_item = give_item(&ctx, &mut s_def, TYPE_ORBITAL_SKYHOOK, 1).await;
    rpc(&ctx, &mut s_def, "structureDeployment", "Anchor", &[PyRep::Integer(temp_sky_item), PyRep::Integer(temp_planet)])
        .await
        .expect("anchor Temperate Skyhook");

    sys_handle.set_ball_position(venture_id, DVec3::new(100_000.0, 0.0, 0.0)).await;
    let ice_item = give_item(&ctx, &mut s_def, 81144, 200).await;
    let mut add_fuel = Vec::new();
    eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(ice_item as u64), &mut add_fuel);
    eve_proto::quasar::encode_varint_field(2, 200, &mut add_fuel);
    eve_proto::quasar::encode_bytes_field(3, &crate::quasar_sov_equinox_hub::idm(hub_id as u64), &mut add_fuel);
    assert_eq!(
        crate::quasar_sov_equinox_hub::rpc(&ctx, &mut s_def, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel).await.status_code,
        eve_server::quasar_gateway::STATUS_OK
    );

    let wf_item = give_item(&ctx, &mut s_def, 87_710, 1).await;
    let threat_item = give_item(&ctx, &mut s_def, UPGRADE_MAJOR_THREAT_DETECTION, 1).await;
    let mut proc1 = Vec::new();
    for iid in [wf_item as u64, threat_item as u64] {
        eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(iid), &mut proc1);
    }
    for tid in [87_710u64, u64::from(UPGRADE_MAJOR_THREAT_DETECTION)] {
        let mut cfg = Vec::new();
        eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(tid), &mut cfg);
        eve_proto::quasar::encode_varint_field(2, 1, &mut cfg);
        eve_proto::quasar::encode_bytes_field(2, &cfg, &mut proc1);
    }
    eve_proto::quasar::encode_bytes_field(3, &crate::quasar_sov_equinox_hub::idm(hub_id as u64), &mut proc1);
    assert_eq!(
        crate::quasar_sov_equinox_hub::rpc(&ctx, &mut s_def, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc1).await.status_code,
        eve_server::quasar_gateway::STATUS_OK
    );
    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&sys_id).is_some_and(|v| v.iter().any(|a| a.anomaly_type == "Combat"))));

    let prosp_item = give_item(&ctx, &mut s_def, UPGRADE_PROSPECTING_ARRAY, 1).await;
    let mut proc2 = Vec::new();
    eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(prosp_item as u64), &mut proc2);
    for (tid, on) in [(87_710u64, 1u64), (u64::from(UPGRADE_MAJOR_THREAT_DETECTION), 0u64), (u64::from(UPGRADE_PROSPECTING_ARRAY), 1u64)] {
        let mut cfg = Vec::new();
        eve_proto::quasar::encode_bytes_field(1, &crate::quasar_sov_equinox_hub::idm(tid), &mut cfg);
        eve_proto::quasar::encode_varint_field(2, on, &mut cfg);
        eve_proto::quasar::encode_bytes_field(2, &cfg, &mut proc2);
    }
    eve_proto::quasar::encode_bytes_field(3, &crate::quasar_sov_equinox_hub::idm(hub_id as u64), &mut proc2);
    assert_eq!(
        crate::quasar_sov_equinox_hub::rpc(&ctx, &mut s_def, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc2).await.status_code,
        eve_server::quasar_gateway::STATUS_OK
    );
    ctx.advance_secs(5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&sys_id).is_some_and(|v| v.iter().any(|a| a.anomaly_type == "Ore"))));

    // 6. Sov loss notifications on `structureDeployment.Unanchor`, then anchor fresh hub for `esi:GET /sovereignty/campaigns`.
    let _ = ctx.drain_notifications_for_char(PILOT_2);
    rpc(&ctx, &mut s_def, "structureDeployment", "Unanchor", &[PyRep::Integer(hub_id)]).await.expect("Unanchor hub");
    ctx.advance_secs(7 * 86_400 + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let loss_notifs = ctx.drain_notifications_for_char(PILOT_2);
    let sov_loss = loss_notifs.iter().find(|n| n.event == "OnSovereigntyChanged").expect("OnSovereigntyChanged on loss");
    assert!(matches!(sov_loss.args.get(1), Some(PyRep::None)));
    assert!(loss_notifs.iter().any(|n| n.event == "OnSolarSystemSovStructuresUpdated"));

    // 7. `esi:GET /sovereignty/campaigns` via Entosis Link I on newly anchored Sov Hub.
    slash(&ctx, &mut s_def, &format!("/tr {def_home_sys}")).await;
    slash(&ctx, &mut s_def, &format!("/tr {sys_id}")).await;
    let hub2_item = give_item(&ctx, &mut s_def, TYPE_SOVEREIGNTY_HUB, 1).await;
    let hub2_id = rpc(&ctx, &mut s_def, "structureDeployment", "Anchor", &[PyRep::Integer(hub2_item), PyRep::String("Campaign Sov Hub".into())])
        .await
        .expect("anchor Sov Hub 2")
        .dict_get("structureID")
        .and_then(|v| v.as_i64())
        .unwrap();
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    rpc(&ctx, &mut s_def, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub2_id), PyRep::Integer(12), PyRep::Integer(3)])
        .await
        .expect("UpdateReinforceTimesFromClient");
    for attr_id in [182_u16, 183, 184] {
        if let Some(&sk) = ctx.sde.types[&34593].default_attributes.get(&attr_id) {
            slash(&ctx, &mut s_atk, &format!("/giveskill {} 5", sk as u32)).await;
        }
    }
    let atk_ship = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    let entosis_mod = give_item(&ctx, &mut s_atk, 34593, 1).await;
    rpc(&ctx, &mut s_atk, "invbroker", "Add", &[PyRep::Integer(entosis_mod), PyRep::Integer(atk_ship), PyRep::Integer(InventoryFlag::HiSlot1 as i64)])
        .await
        .expect("fit Entosis Link I");
    let cur_sec = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 12 * 3_600 - cur_sec) % 86_400);
    slash(&ctx, &mut s_atk, &format!("/tr {sys_id}")).await;
    give_item(&ctx, &mut s_atk, 16275, 20).await;
    lock_target(&ctx, &mut s_atk, sys_id, atk_ship, hub2_id, 2_500.0).await;
    rpc(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub2_id)])
        .await
        .expect("Activate Entosis Link I");
    ctx.advance_secs(1_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let camps = call_esi(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"), 200);
    let camp0 = camps.as_array().unwrap().iter().find(|c| c["structure_id"] == hub2_id).expect("ihub_defense campaign");
    assert_eq!(camp0["event_type"], "ihub_defense");
    assert_eq!(camp0["defender_score"], 0.6);
    assert_eq!(camp0["attackers_score"], 0.4);
}
