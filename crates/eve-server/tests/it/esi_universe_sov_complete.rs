//! `ESI-3` Sovereignty, Incursions, Wars, and Single-Killmail acceptance tests against the
//! real SDE (`SdeRegistry`) and live `WorldRepository` state, validated against `OpenApiSpec::pinned()`.
//!
//! Ledger tags live inside each `#[tokio::test]` body next to its `eve_testkit::attest` call.

#![forbid(unsafe_code)]

use eve_db::{
    AllianceRecord, CorporationRecord, IncursionRecord, InventoryFlag, InventoryItemRecord,
    IskCents, KillmailAttackerDetail, KillmailDetailRecord, KillmailRecord, SovCampaignScoreRecord,
    SystemDynamicState, UpwellStructureRecord, WarAllyRecord, WarLifecycleMeta, WarRecord,
};
use eve_server::esi::sov_fw_routes::{activity_defense_multiplier, sov_structure_type_ids};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<String>) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi-sov-complete-test",
            "https://localhost/callback",
            scopes,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-sov-complete-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

/// Issues the request, asserts the status, validates status/headers/body against the pinned
/// OpenAPI operation, checks the caching contract (ETag, Cache-Control, Expires, Last-Modified) and
/// that a conditional re-request with the ETag yields `304 Not Modified` with an empty body.
fn call(router: &EsiRouter, req: EsiHttpRequest, expected: u16) -> Value {
    let spec = OpenApiSpec::pinned();
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap().to_string();
    assert_eq!(
        resp.status_code,
        expected,
        "{} {} -> {} ({})",
        req.method,
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    // 405 has no OpenAPI operation for the wrong method; its body is checked against the error model below.
    if expected != 405 {
        spec.validate_response(&req.method, &raw_path, resp.status_code, &resp.headers, &resp.body)
            .unwrap_or_else(|e| panic!("OpenAPI validation failed for {}: {e}", req.raw_target));
    }
    let body = resp.json().expect("JSON body");
    if expected == 200 {
        let etag = resp.header("ETag").expect("ETag").to_string();
        let cc = resp.header("Cache-Control").expect("Cache-Control");
        assert!(
            cc.contains("max-age="),
            "expected max-age in Cache-Control: {cc}"
        );
        assert!(resp.header("Expires").is_some() && resp.header("Last-Modified").is_some());
        let again = router.handle_request(&req.clone().with_header("If-None-Match", etag.clone()));
        assert_eq!(again.status_code, 304, "conditional GET with matching ETag");
        assert!(again.body.is_empty());
        assert_eq!(again.header("ETag"), Some(etag.as_str()));
    } else {
        assert!(
            body["error"].as_str().is_some_and(|s| !s.is_empty()),
            "ESI error model"
        );
    }
    body
}

fn sys_state(sys: u32, alliance: Option<i64>, mil: u8, ind: u8, strat: u8) -> SystemDynamicState {
    SystemDynamicState {
        solar_system_id: sys,
        dbs_multiplier: 1.0,
        ess_main_bank: IskCents(0),
        ess_reserve_bank: IskCents(0),
        ess_linked_character_id: None,
        ess_link_started_tick: None,
        sov_alliance_id: alliance,
        military_index: mil,
        industrial_index: ind,
        strategic_index: strat,
        fw_contested_pct: 0.0,
        insurgency_corruption_stage: 0,
        insurgency_suppression_stage: 0,
        wormhole_remaining_mass_kg: 0.0,
        wormhole_destination_system_id: None,
        signature_position: None,
        fw_complex_position: None,
        fw_capture_seconds_elapsed: 0,
        fw_capture_duration_seconds: 0,
        last_fw_capture_tick: 0,
        last_sov_claim_tick: 0,
    }
}

fn structure(id: i64, type_id: u32, owner: i64, sys: u32, pos: [f64; 3]) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: id,
        type_id,
        owner_corp_id: owner,
        solar_system_id: sys,
        name: format!("S{id}"),
        position: pos,
        state: "ShieldVulnerable".to_string(),
        quantum_core_installed: true,
        fuel_blocks: 500,
        shield_hp: 5000.0,
        armor_hp: 5000.0,
        hull_hp: 5000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
    }
}

fn type_in_group(ctx: &ServerContext, group_name: &str, published: bool) -> u32 {
    let gid = ctx
        .sde
        .groups
        .values()
        .find(|g| g.name == group_name)
        .expect(group_name)
        .group_id;
    let mut ids: Vec<u32> = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == gid && t.published == published)
        .map(|t| t.type_id)
        .collect();
    ids.sort_unstable();
    *ids.first()
        .unwrap_or_else(|| panic!("{group_name} type (published={published})"))
}

#[tokio::test]
async fn test_esi_sovereignty_structures_reflect_live_sov_state() {
    // parity: esi:GET /sovereignty/structures, feat:esi.sovereignty.structures, rpc:sovMgr.AcquireSkyhooks
    eve_testkit::attest(&[
        "esi:GET /sovereignty/structures",
        "feat:esi.sovereignty.structures",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();

    // Sov structure types come from the SDE groups, published only.
    let sov_types = sov_structure_type_ids(&ctx.sde);
    let hub_type = type_in_group(&ctx, "Sovereignty Hub", true);
    let qa_hub_type = type_in_group(&ctx, "Sovereignty Hub", false);
    assert!(sov_types.contains(&hub_type) && !sov_types.contains(&qa_hub_type));
    let upwell_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.name == "Astrahus")
        .unwrap()
        .type_id;
    assert!(!sov_types.contains(&upwell_type));

    let mut nullsecs: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status <= -0.1 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .collect();
    nullsecs.sort_unstable();
    let (sys_a, sys_b) = (nullsecs[0], nullsecs[1]);
    let (corp, other_corp, lone_corp, alliance, other_alliance) = (
        98_100_001i64,
        98_100_002i64,
        98_100_003i64,
        99_100_001i64,
        99_100_002i64,
    );
    let (hub_a, hub_b, qa_hub, upwell, lone_hub) = (
        1_040_000_000_001i64,
        1_040_000_000_002,
        1_040_000_000_003,
        1_040_000_000_004,
        1_040_000_000_005,
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = corp;
            ch.alliance_id = Some(alliance);
            ch.user_id = 1;
        }
        for (cid, aid) in [
            (corp, Some(alliance)),
            (other_corp, Some(other_alliance)),
            (lone_corp, None),
        ] {
            snap.corporations.insert(
                cid,
                CorporationRecord {
                    corporation_id: cid,
                    name: format!("Corp {cid}"),
                    ticker: "SOVT".into(),
                    ceo_id: 90_000_001,
                    alliance_id: aid,
                    members: HashSet::from([90_000_001]),
                    ..Default::default()
                },
            );
        }
        for (aid, exec) in [(alliance, corp), (other_alliance, other_corp)] {
            snap.alliances.insert(
                aid,
                AllianceRecord {
                    alliance_id: aid,
                    name: format!("Alliance {aid}"),
                    short_name: "SOVA".into(),
                    executor_corp_id: exec,
                    member_corp_ids: HashSet::from([exec]),
                },
            );
        }
        snap.system_states
            .insert(sys_a, sys_state(sys_a, None, 2, 0, 5));
        snap.system_states
            .insert(sys_b, sys_state(sys_b, Some(other_alliance), 0, 0, 0));
        for s in [
            structure(hub_a, hub_type, corp, sys_a, [0.0, 0.0, 0.0]),
            structure(hub_b, hub_type, other_corp, sys_b, [0.0, 0.0, 0.0]),
            structure(qa_hub, qa_hub_type, corp, sys_a, [100.0, 0.0, 0.0]),
            structure(upwell, upwell_type, corp, sys_a, [200.0, 0.0, 0.0]),
            structure(lone_hub, hub_type, lone_corp, sys_a, [300.0, 0.0, 0.0]),
        ] {
            snap.structures.insert(s.structure_id, s);
        }
    });

    // Before any claim: alliance hubs listed without ADM/vulnerability fields; QA hub, Upwell
    // citadel and the hub of a corporation outside any alliance are not sov structures.
    let before = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"),
        200,
    );
    let ids: Vec<i64> = before
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["structure_id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![hub_a, hub_b],
        "sorted alliance-owned published sov structures only"
    );
    let a0 = &before[0];
    assert_eq!(a0["alliance_id"], json!(alliance));
    assert_eq!(a0["solar_system_id"], json!(sys_a));
    assert_eq!(a0["structure_type_id"], json!(hub_type));
    assert!(
        a0.get("vulnerability_occupancy_level").is_none()
            && a0.get("vulnerable_start_time").is_none()
    );

    // Claim sovereignty through the game RPC: indices rise and a vulnerability window is scheduled.
    let ship_id = ctx
        .db
        .with_snapshot(|s| s.characters[&90_000_001].active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.solar_system_id = sys_a;
        ch.station_id = None;
        ch.structure_id = None;
    });
    ctx.systems
        .get_or_spawn(sys_a)
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.5,
            mass: 1_067_000.0,
            radius: 38.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .expect("spawn ship");
    let mut sess = eve_machonet::SessionState::new(78_001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(corp);
    sess.alliance_id = Some(alliance);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_a);
    let _ = ctx
        .db
        .acquire_online_lock(1, sess.session_id, Some(90_000_001), true, now);
    ctx.dispatch_rpc(
        &mut sess,
        "sovMgr",
        None,
        "AcquireSkyhooks",
        &[],
        &HashMap::new(),
        None,
    )
    .await
    .expect("sovMgr.AcquireSkyhooks");

    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_vulnerability_windows.insert(hub_a, (1_000, 2_000));
        if let Some(st) = snap.system_states.get_mut(&sys_a) {
            st.military_index = 3;
            st.industrial_index = 1;
        }
    });

    let (mil, ind, strat, window) = ctx.db.with_snapshot(|snap| {
        let st = &snap.system_states[&sys_a];
        (
            st.military_index,
            st.industrial_index,
            st.strategic_index,
            snap.structure_vulnerability_windows[&hub_a],
        )
    });
    assert_eq!((mil, ind, strat), (3, 1, 5));
    let after = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"),
        200,
    );
    let a1 = after
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["structure_id"] == json!(hub_a))
        .unwrap();
    // ADM from the live indices per the wiki table: 1.0 + 1.7 (mil 3) + 0.6 (ind 1) + 1.0 (strat 5).
    assert_eq!(activity_defense_multiplier(mil, ind, strat), 4.3);
    assert_eq!(a1["vulnerability_occupancy_level"], json!(4.3));
    let fmt = eve_server::esi::sample_routes::format_iso8601_utc;
    assert_eq!(a1["vulnerable_start_time"], json!(fmt(window.0)));
    assert_eq!(a1["vulnerable_end_time"], json!(fmt(window.1)));
    let b1 = after
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["structure_id"] == json!(hub_b))
        .unwrap();
    assert!(
        b1.get("vulnerable_start_time").is_none(),
        "no window scheduled for the other hub"
    );

    // Live state changes are reflected: a destroyed hub and a hub whose corp left its alliance drop out.
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_b).unwrap().state = "Destroyed".into();
    });
    let destroyed = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"),
        200,
    );
    assert_eq!(destroyed.as_array().unwrap().len(), 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp).unwrap().alliance_id = None;
    });
    let left = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"),
        200,
    );
    assert_eq!(left, json!([]));

    // Public route: a bearer token is not required; error model for wrong method / datasource.
    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/sovereignty/structures/"),
        405,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/sovereignty/structures/?datasource=singularity"),
        400,
    );
}

#[tokio::test]
async fn test_esi_sovereignty_map_reflects_alliance_and_faction_sov() {
    // parity: esi:GET /sovereignty/map
    eve_testkit::attest(&["esi:GET /sovereignty/map"]);

    let ctx = Arc::new(ServerContext::new());
    let router = EsiRouter::new(Arc::clone(&ctx));
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status <= -0.1 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .min()
        .unwrap();
    let (corp, alliance) = (98_200_001i64, 99_200_001i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            alliance,
            AllianceRecord {
                alliance_id: alliance,
                name: "Map Alliance".into(),
                short_name: "MAPA".into(),
                executor_corp_id: corp,
                member_corp_ids: HashSet::from([corp]),
            },
        );
    });

    let before = call(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/map/"), 200);
    let arr = before.as_array().unwrap();
    assert_eq!(
        arr.len(),
        ctx.sde.solar_systems.len(),
        "one entry per SDE solar system"
    );
    let find = |v: &Value, sys: u32| {
        v.as_array()
            .unwrap()
            .iter()
            .find(|e| e["system_id"] == json!(sys))
            .cloned()
            .unwrap()
    };
    // Empire space carries the SDE faction of its constellation/region (Jita -> Caldari State).
    let jita = find(&before, 30_000_142);
    let caldari = ctx
        .sde
        .factions
        .values()
        .find(|f| f.name == "Caldari State")
        .unwrap()
        .faction_id;
    assert_eq!(jita["faction_id"], json!(caldari));
    assert!(jita.get("alliance_id").is_none());
    let unclaimed = find(&before, null_sys);
    assert!(unclaimed.get("alliance_id").is_none());

    // Alliance sovereignty set in the world state shows alliance + executor corporation.
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states
            .insert(null_sys, sys_state(null_sys, Some(alliance), 0, 0, 0));
    });
    let after = call(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/map/"), 200);
    let claimed = find(&after, null_sys);
    assert_eq!(claimed["alliance_id"], json!(alliance));
    assert_eq!(claimed["corporation_id"], json!(corp));
    assert!(claimed.get("faction_id").is_none());

    call(
        &router,
        EsiHttpRequest::new("DELETE", "/v1/sovereignty/map/"),
        405,
    );
}

#[tokio::test]
async fn test_esi_sovereignty_campaigns_reflect_live_command_node_events() {
    // parity: esi:GET /sovereignty/campaigns, feat:esi.sovereignty.campaigns
    eve_testkit::attest(&[
        "esi:GET /sovereignty/campaigns",
        "feat:esi.sovereignty.campaigns",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();

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
    let (ihub_id, tcu_id) = (1_050_000_000_001i64, 1_050_000_000_002i64);
    let start_unix = now.saturating_add(1_800);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp,
            CorporationRecord {
                corporation_id: corp,
                name: "Sov Campaign Corp".into(),
                ticker: "SOVC".into(),
                ceo_id: 90_000_001,
                alliance_id: Some(alliance),
                members: HashSet::from([90_000_001]),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            alliance,
            AllianceRecord {
                alliance_id: alliance,
                name: "Sov Campaign Alliance".into(),
                short_name: "SOVCA".into(),
                executor_corp_id: corp,
                member_corp_ids: HashSet::from([corp]),
            },
        );
        snap.system_states
            .insert(sys_a, sys_state(sys_a, Some(alliance), 3, 3, 5));
        snap.system_states
            .insert(sys_b, sys_state(sys_b, Some(alliance), 2, 2, 3));
        snap.structures.insert(
            ihub_id,
            structure(ihub_id, hub_type, corp, sys_a, [0.0, 0.0, 0.0]),
        );
        snap.structures.insert(
            tcu_id,
            structure(tcu_id, tcu_type, corp, sys_b, [0.0, 0.0, 0.0]),
        );
    });

    // 1. Unreinforced sov structures in ShieldVulnerable state do not produce campaigns.
    let empty_campaigns = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
        200,
    );
    assert_eq!(empty_campaigns, json!([]));

    // 2. Transition both sov structures into active CommandNodeEvent campaigns.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ihub) = snap.structures.get_mut(&ihub_id) {
            ihub.state = "CommandNodeEvent".to_string();
            ihub.reinforced_until_unix = start_unix;
        }
        if let Some(tcu) = snap.structures.get_mut(&tcu_id) {
            tcu.state = "CommandNodeEvent".to_string();
            tcu.reinforced_until_unix = start_unix + 600;
        }
        snap.sov_campaign_scores.insert(
            ihub_id,
            SovCampaignScoreRecord {
                structure_id: ihub_id,
                attackers_score: 0.4,
                defender_score: 0.6,
            },
        );
        snap.sov_campaign_scores.insert(
            tcu_id,
            SovCampaignScoreRecord {
                structure_id: tcu_id,
                attackers_score: 0.75,
                defender_score: 0.25,
            },
        );
    });

    let active = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
        200,
    );
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
    assert_eq!(c0["start_time"], json!(fmt(start_unix)));

    let c1 = &arr[1];
    assert_eq!(c1["campaign_id"], json!(tcu_id));
    assert_eq!(c1["structure_id"], json!(tcu_id));
    assert_eq!(c1["solar_system_id"], json!(sys_b as i64));
    assert_eq!(c1["constellation_id"], json!(const_b));
    assert_eq!(c1["event_type"], json!("tcu_defense"));
    assert_eq!(c1["defender_id"], json!(alliance));
    assert_eq!(c1["defender_score"], json!(0.25));
    assert_eq!(c1["attackers_score"], json!(0.75));
    assert_eq!(c1["start_time"], json!(fmt(start_unix + 600)));

    // 3. Live node capture progress updates scores; resolving a campaign removes it.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sc) = snap.sov_campaign_scores.get_mut(&ihub_id) {
            sc.defender_score = 0.8;
            sc.attackers_score = 0.2;
        }
        if let Some(tcu) = snap.structures.get_mut(&tcu_id) {
            tcu.state = "ShieldVulnerable".to_string();
            tcu.reinforced_until_unix = 0;
        }
    });
    let updated = call(
        &router,
        EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
        200,
    );
    let upd_arr = updated.as_array().unwrap();
    assert_eq!(upd_arr.len(), 1);
    assert_eq!(upd_arr[0]["structure_id"], json!(ihub_id));
    assert_eq!(upd_arr[0]["defender_score"], json!(0.8));
    assert_eq!(upd_arr[0]["attackers_score"], json!(0.2));

    call(
        &router,
        EsiHttpRequest::new("POST", "/v1/sovereignty/campaigns/"),
        405,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/?datasource=invalid"),
        400,
    );
}

#[tokio::test]
async fn test_esi_incursions_wars_and_single_killmail_reflect_live_state() {
    // parity: esi:GET /incursions, feat:esi.incursions.list, esi:GET /killmails/{killmail_id}/{killmail_hash}
    // parity: feat:esi.killmails.single, feat:esi.wars.war
    eve_testkit::attest(&[
        "esi:GET /incursions",
        "feat:esi.incursions.list",
        "esi:GET /killmails/{killmail_id}/{killmail_hash}",
        "feat:esi.killmails.single",
        "feat:esi.wars.war",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();
    let fmt = eve_server::esi::sample_routes::format_iso8601_utc;

    // 1. Incursions (`GET /v1/incursions/`)
    let sansha_fid = ctx
        .sde
        .factions
        .values()
        .find(|f| f.name.contains("Sansha"))
        .expect("Sansha faction in SDE")
        .faction_id as i64;
    let jita_constellation = ctx.sde.solar_systems[&30_000_142].constellation_id;

    let empty_incursions = call(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    assert_eq!(empty_incursions, json!([]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.incursions.insert(
            jita_constellation,
            IncursionRecord {
                constellation_id: jita_constellation,
                staging_solar_system_id: 30_000_142,
                infested_solar_systems: vec![30_000_142, 30_000_144],
                faction_id: sansha_fid,
                has_boss: true,
                influence: 0.85,
                state: "established".to_string(),
                incursion_type: "Incursion".to_string(),
            },
        );
    });

    let incursions_initial = call(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let inc_arr = incursions_initial.as_array().expect("incursions array");
    assert_eq!(inc_arr.len(), 1);
    let kimotoro = &inc_arr[0];
    assert_eq!(kimotoro["constellation_id"], json!(jita_constellation as i64));
    assert_eq!(kimotoro["faction_id"], json!(sansha_fid));
    assert_eq!(kimotoro["staging_solar_system_id"], json!(30_000_142));
    assert_eq!(
        kimotoro["infested_solar_systems"],
        json!([30_000_142, 30_000_144])
    );
    assert_eq!(kimotoro["state"], json!("established"));
    assert_eq!(kimotoro["influence"], json!(0.85));
    assert_eq!(kimotoro["has_boss"], json!(true));
    assert_eq!(kimotoro["type"], json!("Incursion"));

    // Mutate incursion state (mothership defeated -> withdrawing, influence drops to 0.15).
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(inc) = snap.incursions.get_mut(&jita_constellation) {
            inc.influence = 0.15;
            inc.has_boss = false;
            inc.state = "withdrawing".to_string();
        }
    });
    let incursions_withdrawing = call(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let kimotoro_after = &incursions_withdrawing.as_array().unwrap()[0];
    assert_eq!(kimotoro_after["influence"], json!(0.15));
    assert_eq!(kimotoro_after["state"], json!("withdrawing"));
    assert_eq!(kimotoro_after["has_boss"], json!(false));

    // 2. Single Killmail (`GET /v1/killmails/{killmail_id}/{killmail_hash}/`)
    let attacker_id = 90_000_001_i64;
    let wingman_id = 90_000_003_i64;
    let victim_id = 90_000_002_i64;
    let aggr_corp = 98_400_001_i64;
    let def_corp = 98_400_002_i64;
    let non_ally_member_corp = 98_400_003_i64;
    let explicit_ally_corp = 98_400_004_i64;
    let def_alliance = 99_400_001_i64;
    let victim_ship_item_id = 990_100_001_i64;
    let victim_cargo_item_id = 990_100_002_i64;
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
        snap.items.insert(
            victim_ship_item_id,
            InventoryItemRecord {
                item_id: victim_ship_item_id,
                owner_id: victim_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Hangar,
                type_id: 587, // Rifter
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                ..Default::default()
            },
        );
        snap.items.insert(
            victim_cargo_item_id,
            InventoryItemRecord {
                item_id: victim_cargo_item_id,
                owner_id: victim_id,
                location_id: victim_ship_item_id,
                flag: InventoryFlag::Cargo,
                type_id: 34, // Tritanium
                quantity: 250,
                singleton: false,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                ..Default::default()
            },
        );
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
        for (cid, aid) in [
            (aggr_corp, None),
            (def_corp, Some(def_alliance)),
            (non_ally_member_corp, Some(def_alliance)),
            (explicit_ally_corp, None),
        ] {
            snap.corporations.insert(
                cid,
                CorporationRecord {
                    corporation_id: cid,
                    name: format!("War Corp {cid}"),
                    ticker: "WARC".into(),
                    ceo_id: attacker_id,
                    alliance_id: aid,
                    members: HashSet::from([attacker_id]),
                    ..Default::default()
                },
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

    let km_token = issue_token_with_scopes(
        &ctx,
        attacker_id,
        vec!["esi-killmails.read_killmails.v1".to_string()],
    );
    let recent_kms = call(
        &router,
        EsiHttpRequest::new(
            "GET",
            format!("/v1/characters/{attacker_id}/killmails/recent/"),
        )
        .with_bearer_token(&km_token),
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

    let km_detail = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/killmails/{killmail_id}/{km_hash}/")),
        200,
    );
    assert_eq!(km_detail["killmail_id"], json!(killmail_id));
    assert_eq!(km_detail["killmail_time"], json!(fmt(km_time_unix)));
    assert_eq!(km_detail["solar_system_id"], json!(30_000_142));
    assert_eq!(km_detail["victim"]["character_id"], json!(victim_id));
    assert_eq!(km_detail["victim"]["corporation_id"], json!(def_corp));
    assert_eq!(km_detail["victim"]["ship_type_id"], json!(587));
    assert_eq!(km_detail["victim"]["damage_taken"], json!(1_840));
    assert_eq!(
        km_detail["victim"]["position"],
        json!({ "x": 12_500.0, "y": -4_000.0, "z": 8_500.0 })
    );
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

    // Error codes for wrong hash (422), legacy guessable hash (422), and unknown killmail (404).
    let legacy_guess_hash = format!("{:040x}", (killmail_id as u64).wrapping_mul(2_654_435_761));
    call(
        &router,
        EsiHttpRequest::new(
            "GET",
            format!("/v1/killmails/{killmail_id}/{legacy_guess_hash}/"),
        ),
        422,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/killmails/{killmail_id}/deadbeef/")),
        422,
    );
    call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/killmails/99999999/{km_hash}/")),
        404,
    );

    // 3. Wars detail (`GET /v1/wars/{war_id}/`)
    let war_json = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/wars/{war_id}/")),
        200,
    );
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
    // Only explicit war allies are reported (not `non_ally_member_corp` in the defender's alliance).
    assert_eq!(
        war_json["allies"],
        json!([{ "corporation_id": explicit_ally_corp }])
    );
    assert!(war_json.get("retracted").is_none());
    assert!(war_json.get("finished").is_none());

    // Mutate war state (defender kills an aggressor ship and war concludes with explicit timestamps).
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
    let war_concluded = call(
        &router,
        EsiHttpRequest::new("GET", format!("/v1/wars/{war_id}/")),
        200,
    );
    assert_eq!(war_concluded["mutual"], json!(true));
    assert_eq!(war_concluded["open_for_allies"], json!(false));
    assert_eq!(war_concluded["defender"]["ships_killed"], json!(1));
    assert_eq!(war_concluded["defender"]["isk_destroyed"], json!(1_750_000.0));
    assert_eq!(war_concluded["retracted"], json!(fmt(war_retracted_unix)));
    assert_eq!(war_concluded["finished"], json!(fmt(war_finished_unix)));

    call(&router, EsiHttpRequest::new("GET", "/v1/wars/99999999/"), 404);
}
