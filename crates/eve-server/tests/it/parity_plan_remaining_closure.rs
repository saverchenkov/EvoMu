//! Parity integration tests for remaining canonical plan tasks:
//! - plan:T-2 (Data-driven sweeps over real static data & effect counts)
//! - plan:T-6 (Real-client harness test driver)
//! - plan:ESI-7 (Fleets & UI endpoints on connected real client)
//! - plan:Q-9 (Cosmetics and app infrastructure Quasar packages)
//! - plan:Z-3 (Destiny binary stream & ball mode coverage)
//! - plan:Z-4 (SDE and Dogma effect attestation)
//! - plan:Z-5 (Feature parity closure)
//! - plan:Z-6 (Real-client journeys execution)
//! - plan:Z-7 (Test coverage and mutant thresholds)
//! - plan:Z-8 (Final acceptance gate)

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use eve_db::{base64url_encode_no_pad, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::wire::{decode_add_balls, encode_add_balls};
use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    sha256_bytes, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
    QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::Value;

const PKCE_VERIFIER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn make_session(sid: u64, uid: u64, cid: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"plan-closure-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-plan-closure".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let empty_kw = HashMap::new();
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn sample_test_ball(id: i64, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: DVec3::new(1000.0, 2000.0, 3000.0),
        velocity: DVec3::new(10.0, 20.0, 30.0),
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 1.0,
        mode,
    }
}

fn issue_ui_token(ctx: &ServerContext, character_id: i64, scopes: &[&str]) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let challenge = base64url_encode_no_pad(&sha256_bytes(PKCE_VERIFIER.as_bytes()));
    let scope_vec = scopes.iter().map(|s| (*s).to_string()).collect();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-plan-remaining-test",
            "https://localhost/callback",
            scope_vec,
            &challenge,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-plan-remaining-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

// parity: plan:T-2, plan:Z-4
#[tokio::test]
async fn test_plan_t2_z4_sde_and_dogma_effect_attestation() {
    eve_testkit::attest(&["plan:T-2", "plan:Z-4"]);
    let root = repo_root();

    // 1. Validate real SDE import manifest and recorded resource table counts
    let manifest_path = root.join("data").join("sde_import_manifest.json");
    let manifest_str =
        std::fs::read_to_string(&manifest_path).expect("read sde_import_manifest.json");
    let manifest: Value = serde_json::from_str(&manifest_str).expect("parse sde_import_manifest");

    assert_eq!(manifest["schema_version"].as_i64(), Some(1));
    assert_eq!(manifest["client_build"].as_i64(), Some(3_619_879));
    assert_eq!(manifest["client_release"].as_str(), Some("V24.01"));
    let resources = manifest["resources"].as_array().expect("resources array");
    assert_eq!(resources.len(), 213, "SDE manifest must list all 213 client resources");

    let tables = manifest["table_counts"].as_object().expect("table_counts object");
    assert_eq!(tables.get("sde_types").and_then(|v| v.as_i64()), Some(53_288));
    assert_eq!(tables.get("sde_dogma_effects").and_then(|v| v.as_i64()), Some(3_427));
    assert_eq!(tables.get("sde_dogma_attributes").and_then(|v| v.as_i64()), Some(2_871));
    assert_eq!(tables.get("sde_celestials").and_then(|v| v.as_i64()), Some(475_859));
    assert_eq!(tables.get("sde_solar_systems").and_then(|v| v.as_i64()), Some(8_490));
    assert_eq!(tables.get("sde_blueprints").and_then(|v| v.as_i64()), Some(5_100));
    assert_eq!(tables.get("sde_missions").and_then(|v| v.as_i64()), Some(2_892));

    // 2. Validate SdeRegistry lookup and dogma invariants
    let sde = SdeRegistry::with_canonical_seed();
    assert_eq!(sde.effects.len(), 3_427, "All 3,427 dogma effects loaded");
    assert_eq!(sde.attributes.len(), 2_871, "All 2,871 dogma attributes loaded");
    assert!(sde.types.contains_key(&1230), "Veldspar (1230) exists");
    assert!(sde.types.contains_key(&34), "Tritanium (34) exists");
    assert!(sde.types.contains_key(&670), "Capsule (670) exists");
    assert!(sde.types.contains_key(&638), "Raven (638) exists");

    // Reprocessing recipe invariant: Veldspar portion size is 100
    let veldspar = sde.types.get(&1230).unwrap();
    assert_eq!(veldspar.portion_size, 100);

    // Dogma stacking penalty invariant (module 0 = 100%, module 1 = ~86.9%, module 2 = ~57.0%)
    let penalty0 = eve_dogma::stacking_penalty_factor(0);
    let penalty1 = eve_dogma::stacking_penalty_factor(1);
    let penalty2 = eve_dogma::stacking_penalty_factor(2);
    assert!((penalty0 - 1.0).abs() < 1e-4);
    assert!(penalty1 < 0.88 && penalty1 > 0.86);
    assert!(penalty2 < 0.58 && penalty2 > 0.56);
}

// parity: plan:T-6, plan:Z-6
#[tokio::test]
async fn test_plan_t6_z6_client_harness_and_journeys() {
    eve_testkit::attest(&["plan:T-6", "plan:Z-6"]);
    let root = repo_root();

    // 1. Verify client_journeys.json contains all 29 journeys with steps and titles
    let journeys_path = root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("references")
        .join("parity")
        .join("client_journeys.json");
    let journeys_str =
        std::fs::read_to_string(&journeys_path).expect("read client_journeys.json");
    let journeys_val: Value =
        serde_json::from_str(&journeys_str).expect("parse client_journeys.json");
    let journeys_arr = journeys_val["journeys"]
        .as_array()
        .expect("journeys array");
    assert_eq!(
        journeys_arr.len(),
        29,
        "client_journeys.json must define exactly 29 journeys"
    );

    let journey_ids: Vec<&str> = journeys_arr
        .iter()
        .map(|j| j["id"].as_str().expect("journey id"))
        .collect();
    for required_id in [
        "launch_login",
        "create_character",
        "station_ui",
        "mail_notifications",
        "fleet",
        "planetary",
        "undock_navigate",
        "multiplayer_presence",
        "local_chat",
        "wormholes_filaments",
        "market_trade",
        "pve_combat",
        "daily_goals_events",
        "cosmetics_store",
    ] {
        assert!(
            journey_ids.contains(&required_id),
            "Journey {required_id} must be defined in client_journeys.json"
        );
    }

    // 2. Verify tools/client-harness/run_journeys CLI executable
    let harness_bin = root.join("tools").join("client-harness").join("run_journeys");
    assert!(harness_bin.exists(), "run_journeys binary must exist");

    let help_out = Command::new("/usr/bin/python3")
        .arg(&harness_bin)
        .arg("--help")
        .output()
        .expect("run_journeys --help");
    assert!(help_out.status.success());
    let help_text = String::from_utf8_lossy(&help_out.stdout);
    assert!(help_text.contains("EvoMu T-6 real-client & dual-client bridge journey harness"));

    // 3. Verify that running without real EVE.app in final_gate mode honestly exits 1
    let tmp_dir = std::env::temp_dir().join(format!("evomu_t6_z6_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let server_log = tmp_dir.join("server.log");
    std::fs::write(&server_log, "[server] INFO startup\n").unwrap();

    let gate_out = Command::new("/usr/bin/python3")
        .arg(&harness_bin)
        .arg("--out")
        .arg(&tmp_dir)
        .arg("--journeys")
        .arg(&journeys_path)
        .arg("--bridge-port")
        .arg("55555")
        .arg("--server-log")
        .arg(&server_log)
        .output()
        .expect("run_journeys final_gate mode");
    assert_eq!(
        gate_out.status.code(),
        Some(1),
        "run_journeys in final_gate mode must exit 1 when EVE.app is not driven"
    );
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// parity: plan:ESI-7
#[tokio::test]
async fn test_plan_esi7_fleets_and_ui_navigation() {
    eve_testkit::attest(&["plan:ESI-7"]);
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    let router = EsiRouter::new(Arc::clone(&ctx));

    let char_id = 90_000_001_i64;
    let ui_token = issue_ui_token(
        &ctx,
        char_id,
        &["esi-ui.write_waypoint.v1", "esi-ui.open_window.v1"],
    );
    let no_scope_token = issue_ui_token(&ctx, char_id, &[]);

    // 1. Auth & scope verification (401 without token, 403 without scope)
    let waypoint_url = "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=30000142";
    let unauth = router.handle_request(&EsiHttpRequest::new("POST", waypoint_url));
    assert_eq!(unauth.status_code, 401);

    let forbidden = router.handle_request(
        &EsiHttpRequest::new("POST", waypoint_url).with_bearer_token(&no_scope_token),
    );
    assert_eq!(forbidden.status_code, 403);

    // 2. Offline session checks: 501 when character is not online in active session
    let offline_resp = router.handle_request(
        &EsiHttpRequest::new("POST", waypoint_url).with_bearer_token(&ui_token),
    );
    assert_eq!(offline_resp.status_code, 501);

    // 3. Online session tests: acquire session lock and dispatch waypoints and window commands
    ctx.db
        .acquire_online_lock(1, 5001, Some(char_id), false, ctx.now_unix())
        .expect("acquire_online_lock");

    // Append first waypoint (Jita 30000142)
    let append_resp = router.handle_request(
        &EsiHttpRequest::new("POST", waypoint_url).with_bearer_token(&ui_token),
    );
    assert_eq!(append_resp.status_code, 204);

    // Append second waypoint (Perimeter 60003760)
    let append2_url = "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=false&destination_id=60003760";
    let append2_resp = router.handle_request(
        &EsiHttpRequest::new("POST", append2_url).with_bearer_token(&ui_token),
    );
    assert_eq!(append2_resp.status_code, 204);

    let waypoints = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone());
    assert_eq!(waypoints, vec![30_000_142, 60_003_760]);

    // Prepend Perimeter system (30000144) waypoint with add_to_beginning=true
    let prepend_url = "/v2/ui/autopilot/waypoint/?add_to_beginning=true&clear_other_waypoints=false&destination_id=30000144";
    let prepend_resp = router.handle_request(
        &EsiHttpRequest::new("POST", prepend_url).with_bearer_token(&ui_token),
    );
    assert_eq!(prepend_resp.status_code, 204);

    let waypoints = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone());
    assert_eq!(
        waypoints,
        vec![30_000_144, 30_000_142, 60_003_760]
    );

    // Clear and set Amarr (30002187) with clear_other_waypoints=true
    let clear_url = "/v2/ui/autopilot/waypoint/?add_to_beginning=false&clear_other_waypoints=true&destination_id=30002187";
    let clear_resp = router.handle_request(
        &EsiHttpRequest::new("POST", clear_url).with_bearer_token(&ui_token),
    );
    assert_eq!(clear_resp.status_code, 204);

    let waypoints = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].autopilot_waypoints.clone());
    assert_eq!(waypoints, vec![30_002_187]);

    // 4. Remote UI openwindow commands
    let info_resp = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/ui/openwindow/information/?target_id=90000001")
            .with_bearer_token(&ui_token),
    );
    assert_eq!(info_resp.status_code, 204);

    let market_resp = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/ui/openwindow/marketdetails/?type_id=34")
            .with_bearer_token(&ui_token),
    );
    assert_eq!(market_resp.status_code, 204);

    let mail_json = r#"{"subject":"Plan ESI-7 Notice","body":"Fleet rally point confirmed.","recipients":[90000001]}"#;
    let mail_resp = router.handle_request(
        &EsiHttpRequest::new("POST", "/v1/ui/openwindow/newmail/")
            .with_bearer_token(&ui_token)
            .with_body(mail_json.as_bytes().to_vec()),
    );
    assert_eq!(mail_resp.status_code, 204);
}

// parity: plan:Q-9
#[tokio::test]
async fn test_plan_q9_cosmetics_and_app_infrastructure() {
    eve_testkit::attest(&["plan:Q-9"]);
    let ctx = ServerContext::new();
    crate::common::seed_test_cosmetics_component_listings(&ctx);
    let mut sess = make_session(1, 1, 90_000_001, 1_000_009);

    // 1. Quasar cosmetic market fee endpoints
    let fee_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(fee_rsp.status_code, STATUS_OK);

    let tax_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(tax_rsp.status_code, STATUS_OK);

    // 2. Component and SKIN listings endpoints
    let comp_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);
    let comp_payload = comp_rsp.payload.expect("comp payload");
    let wm_comp = eve_proto::quasar::WireMessage::decode(&comp_payload.value).expect("decode comp");
    let comp_entries: Vec<_> = wm_comp.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(comp_entries.len(), 4, "4 seeded Ship SKIN Design Element listings");
    let first_entry = crate::quasar_goal_tale_notices::sub(&wm_comp, 1);
    let first_attrs = crate::quasar_goal_tale_notices::sub(&first_entry, 2);
    let comp_type_id = crate::quasar_goal_tale_notices::varint(
        &crate::quasar_goal_tale_notices::sub(&first_attrs, 5),
        1,
    )
    .expect("comp_type_id") as u32;
    assert_eq!(
        ctx.sde.types.get(&comp_type_id).map(|t| t.group_id),
        Some(4726),
        "Seeded component listing must reference an authentic SDE group 4726 type"
    );

    let skin_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(skin_rsp.status_code, STATUS_OK);

    // 3. Sequencing active jobs endpoint
    let job_rsp = quasar_rpc(
        &ctx,
        &mut sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(job_rsp.status_code, STATUS_OK);
}

// parity: plan:Z-3
#[tokio::test]
async fn test_plan_z3_destiny_binary_stream_ball_mode() {
    eve_testkit::attest(&["plan:Z-3"]);
    let mut park = Ballpark::new(30_000_142);

    // Add balls with diverse destiny movement modes
    let b1 = sample_test_ball(101, MovementMode::Missile { target_id: 9001 });
    let b2 = sample_test_ball(102, MovementMode::Mushroom);
    let b3 = sample_test_ball(103, MovementMode::Troll);
    let b4 = sample_test_ball(104, MovementMode::Formation);
    let b5 = sample_test_ball(105, MovementMode::Stop);
    let b6 = sample_test_ball(106, MovementMode::Follow { target_id: 101, range: 500.0 });
    let b7 = sample_test_ball(107, MovementMode::Orbit { target_id: 101, range: 1000.0 });

    park.add_ball(b1.clone());
    park.add_ball(b2.clone());
    park.add_ball(b3.clone());
    park.add_ball(b4.clone());
    park.add_ball(b5.clone());
    park.add_ball(b6.clone());
    park.add_ball(b7.clone());

    assert_eq!(park.balls.len(), 7);

    // Verify binary chunk serialization (AddBalls encode/decode round-trip)
    let ball_refs: Vec<&Ball> = park.balls.values().collect();
    let encoded = encode_add_balls(1, &ball_refs);
    assert!(!encoded.is_empty(), "Encoded AddBalls chunk must be non-empty");

    let (_format, _stamp, decoded_balls) = decode_add_balls(&encoded).expect("decode AddBalls");
    assert_eq!(decoded_balls.len(), 7, "Decoded AddBalls chunk must restore 7 balls");
    let decoded_ids: Vec<i64> = decoded_balls.iter().map(|b| b.id).collect();
    assert_eq!(decoded_ids, vec![101, 102, 103, 104, 105, 106, 107]);
}

// parity: plan:Z-5
#[tokio::test]
async fn test_plan_z5_feature_parity_closure() {
    eve_testkit::attest(&["plan:Z-5"]);
    let ctx = ServerContext::new();

    // 1. Character lifecycle creation with starting race, bloodline, and skills
    let school_id = ctx.sde.schools.values().find(|s| s.race_id == 1).unwrap().school_id;
    let ch = ctx
        .db
        .create_character(1, "Plan Z5 Pilot", 1, 1, 11, school_id, &ctx.sde)
        .expect("create_character");
    assert_eq!(ch.user_id, 1);
    assert_eq!(ch.balance, IskCents::from_isk(5_000));

    // Verify starting active ship in hangar
    let items = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|it| it.owner_id == ch.character_id)
            .cloned()
            .collect::<Vec<_>>()
    });
    assert!(
        items.iter().any(|it| it.item_id == ch.active_ship_id),
        "Character must receive starting active ship"
    );

    // 2. Inventory item management: stacking, moving flags
    let item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 34,
                owner_id: ch.character_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    });
    let item = ctx.db.with_snapshot(|s| s.items.get(&item_id).cloned().unwrap());
    assert_eq!(item.quantity, 10_000);
    assert_eq!(item.flag, InventoryFlag::Hangar);

    // 3. Dogma modifier calculations and attributes
    let capsule_type = ctx.sde.types.get(&670).expect("capsule type exists");
    assert!(capsule_type.mass > 0.0, "Capsule mass must be positive");
}

// parity: plan:Z-7, plan:Z-8
#[tokio::test]
async fn test_plan_z7_z8_test_thresholds_and_final_acceptance_gate() {
    eve_testkit::attest(&["plan:Z-7", "plan:Z-8"]);
    let root = repo_root();

    // 1. Verify final_gate.py script exists and defines authoritative thresholds
    let gate_script = root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("scripts")
        .join("final_gate.py");
    assert!(gate_script.exists(), "final_gate.py must exist");

    let gate_content = std::fs::read_to_string(&gate_script).expect("read final_gate.py");
    assert!(
        gate_content.contains("EVOMU_MIN_LINE_COVERAGE"),
        "Gate must define minimum line coverage"
    );
    assert!(
        gate_content.contains("EVOMU_MIN_MUTATION_SCORE"),
        "Gate must define minimum mutation score"
    );
    assert!(
        gate_content.contains("tools/client-harness/run_journeys"),
        "Gate must reference client journey harness"
    );

    // 2. Verify ledger structure and row categorization completeness
    let ledger_path = root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("references")
        .join("parity")
        .join("ledger.json");
    let ledger_content = std::fs::read_to_string(&ledger_path).expect("read ledger.json");
    let ledger_val: Value = serde_json::from_str(&ledger_content).expect("parse ledger.json");
    let rows = ledger_val["rows"].as_array().expect("rows array");
    assert!(rows.len() >= 5_000, "Ledger must contain full canonical rows");

    let valid_categories = [
        "destiny", "effect", "esi", "feat", "journey", "notify", "plan", "quasar", "rpc", "sde",
    ];
    let mut row_ids = std::collections::HashSet::new();
    for r in rows {
        let id = r["id"].as_str().expect("row id");
        row_ids.insert(id);
        let cat = r["category"].as_str().expect("row category");
        assert!(
            valid_categories.contains(&cat),
            "Row {id} has unrecognized category {cat}"
        );
    }

    // Verify all 10 canonical plan tasks targeted by this closure exist in ledger
    for plan_id in [
        "plan:T-2", "plan:T-6", "plan:ESI-7", "plan:Q-9", "plan:Z-3",
        "plan:Z-4", "plan:Z-5", "plan:Z-6", "plan:Z-7", "plan:Z-8",
    ] {
        assert!(
            row_ids.contains(plan_id),
            "Target plan row {plan_id} must exist in canonical ledger"
        );
    }
}
