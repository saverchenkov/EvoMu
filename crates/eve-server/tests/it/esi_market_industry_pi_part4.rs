//! `ESI-6` integration tests for Industry & Mining routes against live `WorldRepository` and
//! `SdeRegistry` state populated via `industryManager`, `moonExtractions`, and `dogmaIM` RPCs,
//! validated against `OpenApiSpec::pinned()`.

#![forbid(unsafe_code)]

use crate::esi_market_industry_pi::{
    get_json, issue_token_with_scopes, make_corp, make_item, rpc_call,
};
use eve_db::{
    corp_roles::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER},
    InventoryFlag, IskCents, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_esi6_industry_and_mining_routes() {
    // parity: esi:GET /industry/facilities, esi:GET /industry/systems
    // parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /characters/{character_id}/mining
    // parity: esi:GET /corporations/{corporation_id}/industry/jobs, esi:GET /corporation/{corporation_id}/mining/extractions
    // parity: esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}
    // parity: feat:esi.industry.facilities, feat:esi.industry.systems
    // parity: feat:esi.industry.char-jobs, feat:esi.industry.char-mining
    // parity: feat:esi.industry.corp-jobs, feat:esi.industry.corp-extractions, feat:esi.industry.corp-observers
    eve_testkit::attest(&[
        "esi:GET /industry/facilities",
        "esi:GET /industry/systems",
        "esi:GET /characters/{character_id}/industry/jobs",
        "esi:GET /characters/{character_id}/mining",
        "esi:GET /corporations/{corporation_id}/industry/jobs",
        "esi:GET /corporation/{corporation_id}/mining/extractions",
        "esi:GET /corporation/{corporation_id}/mining/observers",
        "esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}",
        "feat:esi.industry.facilities",
        "feat:esi.industry.systems",
        "feat:esi.industry.char-jobs",
        "feat:esi.industry.char-mining",
        "feat:esi.industry.corp-jobs",
        "feat:esi.industry.corp-extractions",
        "feat:esi.industry.corp-observers",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001_i64;
    let other_char_id = 90_000_002_i64;
    let corp_id = 98_000_631_i64;
    let other_corp_id = 98_000_632_i64;
    let ship_id = 100_000_001_i64;
    let struct_id = 1_030_000_631_001_i64;
    let jita_stn = 60_003_760_i64;
    let dodixie_stn = ctx.sde.solar_systems[&30_002_659].stations[0].station_id;
    let amarr_stn = ctx.sde.solar_systems[&30_002_187].stations[0].station_id;
    let perim_moon = ctx
        .sde
        .celestials_in_system(30_000_144)
        .into_iter()
        .find(|c| c.group_id == 8)
        .expect("SDE moon in Perimeter");
    let perim_pos = [
        perim_moon.position.x + 2_000.0,
        perim_moon.position.y,
        perim_moon.position.z,
    ];
    let bp_mats: Vec<(u32, i32)> = ctx.sde.blueprints[&691]
        .materials
        .iter()
        .map(|m| (m.type_id, (m.quantity as i32) * 500))
        .collect();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_144;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        if let Some(ch2) = snap.characters.get_mut(&other_char_id) {
            ch2.corporation_id = other_corp_id;
            ch2.solar_system_id = 30_000_142;
            ch2.balance = IskCents::from_isk(500_000_000);
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Industrial Corp", "E6IC", char_id, None, 0.08),
        );
        snap.corporations.insert(
            other_corp_id,
            make_corp(other_corp_id, "Other Industrial Corp", "OTHC", other_char_id, None, 0.05),
        );
        for cid in [char_id, other_char_id] {
            let sk = snap.skills.entry(cid).or_default();
            for &(sid, lvl) in &[(3380, 1), (3387, 5), (24625, 5), (24268, 5)] {
                sk.insert(
                    sid,
                    TrainedSkillRecord {
                        type_id: sid,
                        level: lvl,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Perimeter Moon Refinery".into(),
                position: perim_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (iid, tid, flag) in [
            (940_001, 35_878, InventoryFlag::ServiceSlot0),
            (940_002, 35_894, InventoryFlag::ServiceSlot1),
        ] {
            snap.items
                .insert(iid, make_item(iid, tid, corp_id, struct_id, flag, 1, true));
        }
        let mut next_iid = 941_000_i64;
        for (owner, loc) in [
            (other_char_id, jita_stn),
            (char_id, struct_id),
            (char_id, jita_stn),
            (other_char_id, dodixie_stn),
            (char_id, amarr_stn),
        ] {
            next_iid += 1;
            snap.items.insert(
                next_iid,
                make_item(next_iid, 691, owner, loc, InventoryFlag::Hangar, 1, true),
            );
            for &(mat_tid, mat_qty) in &bp_mats {
                next_iid += 1;
                snap.items.insert(
                    next_iid,
                    make_item(next_iid, mat_tid, owner, loc, InventoryFlag::Hangar, mat_qty, false),
                );
            }
        }
    });

    // 1. Start moon extraction #1 on struct_id, advance 6d, fracture chunk #1, and mine the fractured moon asteroid via dogmaIM.Mine
    let mut char_sess = SessionState::new(9841);
    char_sess.user_id = Some(1);
    char_sess.character_id = Some(char_id);
    char_sess.corporation_id = Some(corp_id);
    char_sess.corp_role =
        ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER | ROLE_ACCOUNTANT;
    char_sess.solar_system_id = Some(30_000_144);
    char_sess.ship_id = Some(ship_id);

    let ext_args = [PyRep::Integer(struct_id), PyRep::Integer(518_400)];
    rpc_call(&ctx, &mut char_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
    ctx.db.clock().advance_secs(520_000);
    let frac1 = rpc_call(
        &ctx,
        &mut char_sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
    )
    .await;
    let ast_id_1 = match frac1.dict_get("asteroidIDs") {
        Some(PyRep::List(v)) => v[0].as_i64().expect("first fractured asteroid ID"),
        other => panic!("unexpected asteroidIDs: {other:?}"),
    };
    let _ = ctx
        .systems
        .get_or_spawn(30_000_144)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: DVec3::from_array(perim_pos),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    rpc_call(&ctx, &mut char_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id_1)]).await;

    // 2. Install & deliver an old industry job in Jita via other_char_id, then advance 35 days so both the first mining ledger entry (>30d) and first Jita job (>28d) age out
    let mut other_sess = SessionState::new(9842);
    other_sess.user_id = Some(1);
    other_sess.character_id = Some(other_char_id);
    other_sess.corporation_id = Some(other_corp_id);
    other_sess.station_id = Some(jita_stn);
    other_sess.solar_system_id = Some(30_000_142);

    let old_job_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(10)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_stn)),
    ]);
    let old_job_rep =
        rpc_call(&ctx, &mut other_sess, "industryManager", "InstallJob", &[old_job_dict]).await;
    let old_job_id = old_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.clock().advance_secs(100_000);
    rpc_call(&ctx, &mut other_sess, "industryManager", "CompleteJob", &[PyRep::Integer(old_job_id)])
        .await;
    ctx.db.clock().advance_secs(35 * 86_400);

    // 3. Start moon extraction #2, fracture chunk #2, mine a fresh moon asteroid via dogmaIM.Mine, and start active moon extraction #3
    rpc_call(&ctx, &mut char_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
    ctx.db.clock().advance_secs(520_000);
    let frac2 = rpc_call(
        &ctx,
        &mut char_sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
    )
    .await;
    let ast_id_2 = match frac2.dict_get("asteroidIDs") {
        Some(PyRep::List(v)) => v.get(1).or_else(|| v.first()).and_then(|x| x.as_i64()).unwrap(),
        other => panic!("unexpected asteroidIDs: {other:?}"),
    };
    let fresh_ore_tid = ctx.db.get_asteroid(ast_id_2).unwrap().type_id;
    rpc_call(&ctx, &mut char_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id_2)]).await;
    rpc_call(&ctx, &mut char_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;

    // 4. Install & deliver Perimeter corp job (runs = 1), install active Jita corp job (runs = 1), and install active Dodixie job (runs = 8)
    char_sess.structure_id = Some(struct_id);
    char_sess.station_id = None;
    let perim_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("facilityID".into()), PyRep::Integer(struct_id)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let perim_job_rep =
        rpc_call(&ctx, &mut char_sess, "industryManager", "InstallJob", &[perim_dict]).await;
    let perim_job_id = perim_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.clock().advance_secs(10_000);
    rpc_call(&ctx, &mut char_sess, "industryManager", "CompleteJob", &[PyRep::Integer(perim_job_id)])
        .await;

    char_sess.structure_id = None;
    char_sess.station_id = Some(jita_stn);
    char_sess.solar_system_id = Some(30_000_142);
    let jita_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_stn)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let jita_job_rep =
        rpc_call(&ctx, &mut char_sess, "industryManager", "InstallJob", &[jita_dict]).await;
    let jita_job_id = jita_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    other_sess.station_id = Some(dodixie_stn);
    other_sess.solar_system_id = Some(30_002_659);
    let dodixie_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(8)),
        (PyRep::String("facilityID".into()), PyRep::Integer(dodixie_stn)),
    ]);
    rpc_call(&ctx, &mut other_sess, "industryManager", "InstallJob", &[dodixie_dict]).await;

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-industry.read_character_jobs.v1",
            "esi-industry.read_character_mining.v1",
            "esi-industry.read_corporation_jobs.v1",
            "esi-industry.read_corporation_mining.v1",
        ],
    );

    let facilities = get_json(&router, &spec, "/v1/industry/facilities/", None);
    let fac_arr = facilities.as_array().unwrap();
    assert!(fac_arr.iter().any(|f| {
        f["facility_id"] == json!(struct_id)
            && f["owner_id"] == json!(corp_id)
            && f["solar_system_id"] == json!(30_000_144)
            && f["tax"] == json!(0.08)
    }));
    assert!(fac_arr
        .iter()
        .any(|f| f["facility_id"] == json!(jita_stn) && f["solar_system_id"] == json!(30_000_142)));

    let systems = get_json(&router, &spec, "/v1/industry/systems/", None);
    let sys_arr = systems.as_array().unwrap();
    assert_eq!(sys_arr.len(), ctx.sde.solar_systems.len());
    let sci_for = |sid: u32, act: &str| -> f64 {
        sys_arr
            .iter()
            .find(|s| s["solar_system_id"] == json!(sid))
            .unwrap()["cost_indices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ci| ci["activity"] == json!(act))
            .unwrap()["cost_index"]
            .as_f64()
            .unwrap()
    };
    assert!((sci_for(30_000_142, "manufacturing") - 0.10_f64.sqrt()).abs() < 1e-6);
    assert!((sci_for(30_000_144, "manufacturing") - 0.10_f64.sqrt()).abs() < 1e-6);
    assert!((sci_for(30_002_659, "manufacturing") - 0.50).abs() < 1e-6);
    assert!((sci_for(30_002_187, "manufacturing") - 0.01).abs() < 1e-6);

    // Verify exact parity with `map.GetIndustryCostModifier` RPC across active, clamped, and idle systems:
    for sid in [30_000_142u32, 30_000_144, 30_002_659, 30_002_187] {
        let rep = rpc_call(
            &ctx,
            &mut char_sess,
            "map",
            "GetIndustryCostModifier",
            &[PyRep::Integer(i64::from(sid))],
        )
        .await;
        for (rpc_k, esi_k) in [
            ("manufacturing", "manufacturing"),
            ("research_te", "researching_time_efficiency"),
            ("research_me", "researching_material_efficiency"),
            ("copying", "copying"),
            ("invention", "invention"),
            ("reaction", "reaction"),
        ] {
            let Some(PyRep::Real(rpc_v)) = rep.dict_get(rpc_k) else {
                panic!("missing {rpc_k} in {rep:?}")
            };
            assert!((sci_for(sid, esi_k) - rpc_v).abs() < 1e-9);
        }
    }

    let char_jobs = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/industry/jobs/?include_completed=false"),
        Some(&token),
    );
    let char_jobs_arr = char_jobs.as_array().unwrap();
    assert_eq!(char_jobs_arr.len(), 1);
    assert_eq!(char_jobs_arr[0]["job_id"], json!(jita_job_id));
    assert_eq!(char_jobs_arr[0]["blueprint_type_id"], json!(691));
    assert_eq!(char_jobs_arr[0]["product_type_id"], json!(587));
    assert_eq!(char_jobs_arr[0]["runs"], json!(1));

    let char_mining =
        get_json(&router, &spec, format!("/v1/characters/{char_id}/mining/"), Some(&token));
    let char_mining_arr = char_mining.as_array().unwrap();
    assert_eq!(char_mining_arr.len(), 1, "Mining ledger entries >30d excluded");
    assert_eq!(char_mining_arr[0]["type_id"], json!(fresh_ore_tid));
    assert!(char_mining_arr[0]["quantity"].as_i64().unwrap() > 0);

    let corp_jobs = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/industry/jobs/?include_completed=true"),
        Some(&token),
    );
    assert_eq!(corp_jobs.as_array().unwrap().len(), 2);

    let extractions = get_json(
        &router,
        &spec,
        format!("/v1/corporation/{corp_id}/mining/extractions/"),
        Some(&token),
    );
    assert_eq!(extractions[0]["structure_id"], json!(struct_id));
    assert_eq!(extractions[0]["moon_id"], json!(perim_moon.celestial_id));

    let observers = get_json(
        &router,
        &spec,
        format!("/v1/corporation/{corp_id}/mining/observers/"),
        Some(&token),
    );
    assert!(observers.as_array().unwrap().iter().any(|o| o["observer_id"] == json!(struct_id)));

    let obs_detail = get_json(
        &router,
        &spec,
        format!("/v1/corporation/{corp_id}/mining/observers/{struct_id}/"),
        Some(&token),
    );
    assert_eq!(obs_detail[0]["character_id"], json!(char_id));
    assert!(obs_detail[0]["quantity"].as_i64().unwrap() > 0);

    // IND-01 regression: cancelling an active job via `industryManager.CancelJob` must exclude its
    // `duration_seconds` from both `GET /v1/industry/systems/` and `map.GetIndustryCostModifier`.
    char_sess.station_id = Some(amarr_stn);
    char_sess.solar_system_id = Some(30_002_187);
    let amarr_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(100)),
        (PyRep::String("facilityID".into()), PyRep::Integer(amarr_stn)),
    ]);
    let amarr_job_rep =
        rpc_call(&ctx, &mut char_sess, "industryManager", "InstallJob", &[amarr_dict]).await;
    let amarr_job_id = amarr_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let sys_active = get_json(&router, &spec, "/v1/industry/systems/", None);
    let sci_from = |arr: &serde_json::Value, sid: u32| -> f64 {
        arr.as_array()
            .unwrap()
            .iter()
            .find(|s| s["solar_system_id"] == json!(sid))
            .unwrap()["cost_indices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ci| ci["activity"] == json!("manufacturing"))
            .unwrap()["cost_index"]
            .as_f64()
            .unwrap()
    };
    assert!((sci_from(&sys_active, 30_002_187) - 0.50).abs() < 1e-6);
    rpc_call(
        &ctx,
        &mut char_sess,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(amarr_job_id)],
    )
    .await;
    let sys_cancelled = get_json(&router, &spec, "/v1/industry/systems/", None);
    assert!((sci_from(&sys_cancelled, 30_002_187) - 0.01).abs() < 1e-6);
    assert!((sci_from(&sys_cancelled, 30_000_142) - 0.10_f64.sqrt()).abs() < 1e-6);
    let amarr_rpc = rpc_call(
        &ctx,
        &mut char_sess,
        "map",
        "GetIndustryCostModifier",
        &[PyRep::Integer(30_002_187)],
    )
    .await;
    assert!((amarr_rpc.dict_get("manufacturing").and_then(|v| v.as_f64()).unwrap() - 0.01).abs() < 1e-9);
}
