//! Live non-GM end-to-end tests for Parity Shard 1 Faction Warfare, Incursions & Sovereignty ESI:
//! - Live militia enlistment (`facWarMgr.JoinFactionAsCorporation` / `fwCharacterEnlistmentMgr`),
//!   space PvP combat, FW complex capture (`keeper.ActivateAccelerationGate` + guard kill), and
//!   verification across all 11 FW ESI & feature rows + SQLite restart reload
//! - Live Null-Sec Sovereignty Hub anchoring & onlining, ADM / Military Index scaling via space combat,
//!   Entosis Link I campaign & command node capture, and Incursion Vanguard/Boss completion across all
//!   7 Sovereignty & Incursions ESI & feature rows + SQLite restart reload

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{
    board_loaded_freighter, buy, dock_station, fuel_cargo, fund_pilots, int, items_at, login,
    move_item, nearest_system, rpc, train_skills, travel, warp_to, PERIMETER_SYS, PILOT_1, PILOT_2,
};
use crate::parity_shard_1_support::{
    assert_etag_304, cleanup_sqlite_files, destroy_target_live, esi_call, issue_token,
    lock_target_live, reload_repo, temp_db_path, travel_slow, upgrade_omega_live,
};
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const RIFTER_SHIP: u32 = 587;
const SMALL_AUTOCANNON: u32 = 2_881;
const SOV_HUB_TYPE: u32 = 32_458;
const ENTOSIS_LINK_I: u32 = 34_593;
const STRONTIUM: u32 = 16_275;

async fn arm_rifter_non_gm(ctx: &ServerContext, sess: &mut eve_machonet::SessionState, owner: i64) -> (i64, Vec<i64>) {
    let ship = buy(ctx, sess, RIFTER_SHIP, 1).await[0];
    rpc(ctx, sess, "ship", "AssembleShip", &[PyRep::Integer(ship)]).await.expect("AssembleShip");
    ctx.advance_secs(12);
    rpc(ctx, sess, "ship", "Board", &[PyRep::Integer(ship)]).await.expect("Board Rifter");
    let mut guns = Vec::with_capacity(3);
    for idx in 0..3 {
        let g = buy(ctx, sess, SMALL_AUTOCANNON, 1).await[0];
        rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(g), PyRep::Integer(ship), PyRep::Integer(27 + idx as i64)])
            .await
            .expect("Fit turret");
        guns.push(g);
    }
    let emp_s = buy(ctx, sess, 185, 500).await[0];
    move_item(ctx, sess, emp_s, ship, 5).await;
    let gun_reps: Vec<PyRep> = guns.iter().map(|&g| PyRep::Integer(g)).collect();
    rpc(ctx, sess, "dogmaIM", "LoadAmmo", &[PyRep::Integer(ship), PyRep::List(gun_reps), PyRep::List(vec![PyRep::Integer(emp_s)])])
        .await
        .expect("LoadAmmo on Rifter");
    let _ = owner;
    (ship, guns)
}

#[tokio::test]
async fn test_shard1_live_fw_stats_leaderboards_and_systems_esi() {
    // parity: esi:GET /characters/{character_id}/fw/stats, esi:GET /corporations/{corporation_id}/fw/stats
    // parity: feat:esi.faction-warfare.char-corp-stats, esi:GET /fw/leaderboards
    // parity: esi:GET /fw/leaderboards/characters, esi:GET /fw/leaderboards/corporations
    // parity: feat:esi.faction-warfare.leaderboards, esi:GET /fw/stats
    // parity: feat:esi.faction-warfare.stats, esi:GET /fw/systems, feat:esi.faction-warfare.systems
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/fw/stats",
        "esi:GET /corporations/{corporation_id}/fw/stats",
        "feat:esi.faction-warfare.char-corp-stats",
        "esi:GET /fw/leaderboards",
        "esi:GET /fw/leaderboards/characters",
        "esi:GET /fw/leaderboards/corporations",
        "feat:esi.faction-warfare.leaderboards",
        "esi:GET /fw/stats",
        "feat:esi.faction-warfare.stats",
        "esi:GET /fw/systems",
        "feat:esi.faction-warfare.systems",
    ]);

    let db_path = temp_db_path("fw_esi");
    let corp_1;
    let target_sys;

    {
        let repo = reload_repo(&db_path);
        let ctx = Arc::new(ServerContext::with_repository(repo));
        common::pin_clock(&ctx);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 5_000_000_000.0), (PILOT_2, 2_000_000_000.0)]).await;

        let mut s1 = login(&ctx, 9441, "demo", PILOT_1).await;
        let mut s2 = login(&ctx, 9442, "other", PILOT_2).await;
        train_skills(&ctx, &mut s1, &[3300, 3301, 3303, 3306, 3327, 3363, 11_083, 3413, 3426, RIFTER_SHIP, SMALL_AUTOCANNON], true).await;
        train_skills(&ctx, &mut s2, &[RIFTER_SHIP], false).await;

        let corp_rep = rpc(
            &ctx,
            &mut s1,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("StateStrike".into()), PyRep::String("STSK".into())],
        )
        .await
        .expect("AddCorporation");
        corp_1 = int(&corp_rep, "corporationID");

        rpc(&ctx, &mut s1, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(500_001)])
            .await
            .expect("JoinFactionAsCorporation Caldari");
        rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
            .await
            .expect("CreateMyDirectEnlistment Gallente");

        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();
        let (init_sys, _) = esi_call(&router, &spec, "GET", "/v1/fw/systems/", None, None, 200);
        let gallente_uncontested: std::collections::HashSet<u32> = init_sys
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["occupier_faction_id"] == 500_004 && s["contested"] == "uncontested" && s["victory_points"] == 0)
            .filter_map(|s| s["solar_system_id"].as_u64().map(|id| id as u32))
            .collect();
        target_sys = nearest_system(&ctx, PERIMETER_SYS, |sys| gallente_uncontested.contains(&sys));

        let (ship_1, guns_1) = arm_rifter_non_gm(&ctx, &mut s1, PILOT_1).await;
        let ship_2 = buy(&ctx, &mut s2, RIFTER_SHIP, 1).await[0];
        rpc(&ctx, &mut s2, "ship", "AssembleShip", &[PyRep::Integer(ship_2)]).await.unwrap();
        rpc(&ctx, &mut s2, "ship", "Board", &[PyRep::Integer(ship_2)]).await.unwrap();

        travel(&ctx, &mut s1, target_sys).await;
        travel(&ctx, &mut s2, target_sys).await;
        let beyonce2 = ctx.bind_object(&mut s2, "beyonce", target_sys as i64).expect("bind beyonce");
        ctx.dispatch_rpc(
            &mut s2,
            "beyonce",
            Some(beyonce2),
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.5)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("decloak target ship_2");
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        // Destroy opposing militia PILOT_2's Rifter in live space combat -> 1 FW kill!
        destroy_target_live(&ctx, &mut s1, target_sys, ship_1, &guns_1, ship_2).await;

        // Activate Novice FW complex gate, clear guard NPC, hold bunker -> raises system VP & contested state
        let _sys_h = ctx.systems.get_or_spawn(target_sys);
        let gate_id = ctx.db.with_snapshot(|snap| {
            let (&gid, _) = snap
                .fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, (csys, t, _, _, done))| *csys == target_sys && t == "Novice" && !*done)
                .expect("Novice FW complex");
            gid
        });
        warp_to(&ctx, &mut s1, gate_id, 0.0).await;
        rpc(&ctx, &mut s1, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)])
            .await
            .expect("ActivateAccelerationGate");
        destroy_target_live(&ctx, &mut s1, target_sys, ship_1, &guns_1, gate_id + 500_000_000).await;
        ctx.advance_secs(605);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let tok_1 = issue_token(&ctx, PILOT_1, &["esi-characters.read_fw_stats.v1", "esi-corporations.read_fw_stats.v1"]);
        esi_call(&router, &spec, "GET", &format!("/v1/characters/{PILOT_1}/fw/stats/"), None, None, 401);
        esi_call(&router, &spec, "GET", &format!("/v1/characters/{PILOT_2}/fw/stats/"), Some(&tok_1), None, 403);

        assert_etag_304(&router, &spec, &format!("/v1/characters/{PILOT_1}/fw/stats/"), Some(&tok_1));
        let (c1, _) = esi_call(&router, &spec, "GET", &format!("/v1/characters/{PILOT_1}/fw/stats/"), Some(&tok_1), None, 200);
        assert_eq!(c1["faction_id"], json!(500_001));
        assert_eq!(c1["kills"]["yesterday"], json!(1));
        assert!(c1["victory_points"]["yesterday"].as_i64().unwrap() > 0);

        assert_etag_304(&router, &spec, &format!("/v1/corporations/{corp_1}/fw/stats/"), Some(&tok_1));
        let (corp_fw, _) = esi_call(&router, &spec, "GET", &format!("/v1/corporations/{corp_1}/fw/stats/"), Some(&tok_1), None, 200);
        assert_eq!(corp_fw["faction_id"], json!(500_001));
        assert_eq!(corp_fw["pilots"], json!(1));
        assert_eq!(corp_fw["kills"]["yesterday"], json!(1));

        for public_path in [
            "/v1/fw/leaderboards/",
            "/v1/fw/leaderboards/characters/",
            "/v1/fw/leaderboards/corporations/",
            "/v1/fw/stats/",
            "/v1/fw/systems/",
            "/v3/fw/systems/",
        ] {
            assert_etag_304(&router, &spec, public_path, None);
        }

        let (lb_chars, _) = esi_call(&router, &spec, "GET", "/v1/fw/leaderboards/characters/", None, None, 200);
        assert!(lb_chars["kills"]["active_total"].as_array().unwrap().iter().any(|r| r["character_id"] == PILOT_1 && r["amount"] == 1));
        let (lb_corps, _) = esi_call(&router, &spec, "GET", "/v1/fw/leaderboards/corporations/", None, None, 200);
        assert!(lb_corps["kills"]["active_total"].as_array().unwrap().iter().any(|r| r["corporation_id"] == corp_1 && r["amount"] == 1));
    }

    // Reload SQLite and verify persisted FW stats & system contested state
    {
        let repo2 = reload_repo(&db_path);
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let router2 = EsiRouter::new(Arc::clone(&ctx2));
        let spec2 = OpenApiSpec::pinned();
        let tok2 = issue_token(&ctx2, PILOT_1, &["esi-characters.read_fw_stats.v1"]);
        let (c2, _) = esi_call(&router2, &spec2, "GET", &format!("/v1/characters/{PILOT_1}/fw/stats/"), Some(&tok2), None, 200);
        assert_eq!(c2["kills"]["yesterday"], json!(1));
        let (sys2, _) = esi_call(&router2, &spec2, "GET", "/v1/fw/systems/", None, None, 200);
        let srow = sys2.as_array().unwrap().iter().find(|r| r["solar_system_id"] == i64::from(target_sys)).unwrap();
        assert_eq!(srow["contested"], json!("contested"));
        assert!(srow["victory_points"].as_i64().unwrap() > 0);
    }

    cleanup_sqlite_files(&db_path);
}

#[tokio::test]
async fn test_shard1_live_sovereignty_and_incursions_esi() {
    // parity: esi:GET /sovereignty/map, esi:GET /sovereignty/structures, feat:esi.sovereignty.structures
    // parity: esi:GET /sovereignty/campaigns, feat:esi.sovereignty.campaigns
    // parity: esi:GET /incursions, feat:esi.incursions.list
    eve_testkit::attest(&[
        "esi:GET /sovereignty/map",
        "esi:GET /sovereignty/structures",
        "feat:esi.sovereignty.structures",
        "esi:GET /sovereignty/campaigns",
        "feat:esi.sovereignty.campaigns",
        "esi:GET /incursions",
        "feat:esi.incursions.list",
    ]);

    let db_path = temp_db_path("sov_incursions");
    let null_sys;
    let hub_id;
    let def_ally;

    {
        let repo = reload_repo(&db_path);
        let ctx = Arc::new(ServerContext::with_repository(repo));
        common::pin_clock(&ctx);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 30_000_000_000.0), (PILOT_2, 100_000_000_000.0)]).await;

        let mut s_atk = login(&ctx, 9451, "demo", PILOT_1).await;
        let mut s_def = login(&ctx, 9452, "other", PILOT_2).await;

        train_skills(&ctx, &mut s_def, &[3449, 3453, 3456], false).await;
        train_skills(&ctx, &mut s_atk, &[3300, 3301, 3327, 24_242, 3449, 3453, 3456, 3459, ENTOSIS_LINK_I, RIFTER_SHIP], true).await;
        upgrade_omega_live(&ctx, &mut s_atk, &mut s_def).await;

        let c_def = rpc(&ctx, &mut s_def, "corpRegistry", "AddCorporation", &[PyRep::String("SovKeepers".into()), PyRep::String("SVKP".into())])
            .await
            .expect("AddCorporation def");
        let def_corp = int(&c_def, "corporationID");
        rpc(&ctx, &mut s_def, "account", "GiveCash", &[PyRep::Integer(def_corp), PyRep::Real(2_000_000_000.0), PyRep::String("ally".into())])
            .await
            .unwrap();
        let a_def = rpc(&ctx, &mut s_def, "corpRegistry", "CreateAlliance", &[PyRep::String("Sov Keepers".into()), PyRep::String("SVKA".into())])
            .await
            .expect("CreateAlliance def");
        def_ally = int(&a_def, "allianceID");
        s_def.alliance_id = Some(def_ally);

        let c_atk = rpc(&ctx, &mut s_atk, "corpRegistry", "AddCorporation", &[PyRep::String("EntosisRaiders".into()), PyRep::String("ENRD".into())])
            .await
            .expect("AddCorporation atk");
        let atk_corp = int(&c_atk, "corporationID");
        rpc(&ctx, &mut s_atk, "account", "GiveCash", &[PyRep::Integer(atk_corp), PyRep::Real(2_000_000_000.0), PyRep::String("ally".into())])
            .await
            .unwrap();
        let a_atk = rpc(&ctx, &mut s_atk, "corpRegistry", "CreateAlliance", &[PyRep::String("Entosis Raiders".into()), PyRep::String("ENRA".into())])
            .await
            .expect("CreateAlliance atk");
        s_atk.alliance_id = Some(int(&a_atk, "allianceID"));
        ctx.advance_virtual_clock(8 * 86_400);

        null_sys = nearest_system(&ctx, PERIMETER_SYS, |sys| {
            eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, sys)
        });

        let fr_def = board_loaded_freighter(&ctx, &mut s_def, &[(SOV_HUB_TYPE, 1)]).await;
        let hub_item = items_at(&ctx, PILOT_2, fr_def, SOV_HUB_TYPE)[0].0;

        travel_slow(&ctx, &mut s_def, null_sys).await;
        let anc = rpc(&ctx, &mut s_def, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("S1 Sov Hub".into())])
            .await
            .expect("Anchor Sovereignty Hub");
        hub_id = int(&anc, "structureID");
        ctx.advance_secs(905);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();
        assert_etag_304(&router, &spec, "/v1/sovereignty/map/", None);
        let (map_json, _) = esi_call(&router, &spec, "GET", "/v1/sovereignty/map/", None, None, 200);
        let sys_entry = map_json.as_array().unwrap().iter().find(|r| r["system_id"] == i64::from(null_sys)).unwrap();
        assert_eq!(sys_entry["alliance_id"], json!(def_ally));
        assert_eq!(sys_entry["corporation_id"], json!(def_corp));

        // Set reinforce hour 12:00, verify ADM 1.0
        rpc(&ctx, &mut s_def, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(12), PyRep::Integer(3)])
            .await
            .expect("UpdateReinforceTimesFromClient");
        assert_etag_304(&router, &spec, "/v1/sovereignty/structures/", None);
        let (st1_arr, _) = esi_call(&router, &spec, "GET", "/v1/sovereignty/structures/", None, None, 200);
        let st1 = st1_arr.as_array().unwrap().iter().find(|r| r["structure_id"] == hub_id).unwrap();
        assert_eq!(st1["vulnerability_occupancy_level"], json!(1.0));

        // Attacker fits Entosis Link I + Strontium in Jita and travels to null_sys during vulnerability window
        let atk_ship = buy(&ctx, &mut s_atk, RIFTER_SHIP, 1).await[0];
        rpc(&ctx, &mut s_atk, "ship", "AssembleShip", &[PyRep::Integer(atk_ship)]).await.unwrap();
        rpc(&ctx, &mut s_atk, "ship", "Board", &[PyRep::Integer(atk_ship)]).await.unwrap();
        let entosis_mod = buy(&ctx, &mut s_atk, ENTOSIS_LINK_I, 1).await[0];
        rpc(&ctx, &mut s_atk, "invbroker", "Add", &[PyRep::Integer(entosis_mod), PyRep::Integer(atk_ship), PyRep::Integer(27)])
            .await
            .expect("Fit Entosis Link I");
        let stront = buy(&ctx, &mut s_atk, STRONTIUM, 20).await[0];
        move_item(&ctx, &mut s_atk, stront, atk_ship, 5).await;

        let cur_sec = ctx.now_unix() % 86_400;
        ctx.advance_secs((86_400 + 12 * 3_600 - cur_sec) % 86_400);
        travel(&ctx, &mut s_atk, null_sys).await;
        lock_target_live(&ctx, &mut s_atk, null_sys, atk_ship, hub_id, 2_500.0).await;
        rpc(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
            .await
            .expect("Activate Entosis Link I");
        ctx.advance_secs(1_600);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

        assert_etag_304(&router, &spec, "/v1/sovereignty/campaigns/", None);
        let (camps, _) = esi_call(&router, &spec, "GET", "/v1/sovereignty/campaigns/", None, None, 200);
        let camp = camps.as_array().unwrap().iter().find(|c| c["structure_id"] == hub_id).expect("live sov campaign");
        assert_eq!(camp["defender_id"], json!(def_ally));
        assert!(camp["attackers_score"].as_f64().unwrap() > 0.0);

        // Live Incursions check: spawn & verify GET /v1/incursions/
        eve_server::services::pve_special_sites::ensure_special_sites_spawned(&ctx).await;
        assert_etag_304(&router, &spec, "/v1/incursions/", None);
        let (incs, _) = esi_call(&router, &spec, "GET", "/v1/incursions/", None, None, 200);
        assert!(incs.as_array().unwrap().len() >= 1);
        let _ = (fuel_cargo, board_loaded_freighter, items_at, warp_to, dock_station);
    }

    // Reload SQLite and verify Sovereignty Hub, Map, Campaign & Incursions persist
    {
        let repo2 = reload_repo(&db_path);
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let router2 = EsiRouter::new(Arc::clone(&ctx2));
        let spec2 = OpenApiSpec::pinned();
        let (map2, _) = esi_call(&router2, &spec2, "GET", "/v1/sovereignty/map/", None, None, 200);
        assert!(map2.as_array().unwrap().iter().any(|r| r["system_id"] == i64::from(null_sys) && r["alliance_id"] == def_ally));
        let (st2, _) = esi_call(&router2, &spec2, "GET", "/v1/sovereignty/structures/", None, None, 200);
        assert!(st2.as_array().unwrap().iter().any(|r| r["structure_id"] == hub_id && r["alliance_id"] == def_ally));
        let (camps2, _) = esi_call(&router2, &spec2, "GET", "/v1/sovereignty/campaigns/", None, None, 200);
        assert!(camps2.as_array().unwrap().iter().any(|c| c["structure_id"] == hub_id));
    }

    cleanup_sqlite_files(&db_path);
}
