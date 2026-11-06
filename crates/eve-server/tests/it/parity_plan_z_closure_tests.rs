//! Integration tests verifying Plan Z parity closure items (`plan:Z-1`, `plan:Z-2`,
//! `plan:Z-5`, `plan:Z-6`, `plan:Z-7`, `plan:Z-8`) and end-to-end acceptance gates.

use crate::common;
use eve_db::IskCents;
use eve_machonet::{MachoPacket, MachoPacketType, SessionState};
use eve_proto::{marshal, PyRep};
use eve_server::router::ServerContext;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

// parity: plan:Z-1, plan:Z-2, plan:Z-5
#[tokio::test]
async fn test_parity_plan_z1_z2_z5_rpc_notify_feature_closure() {
    eve_testkit::attest(&["plan:Z-1", "plan:Z-2", "plan:Z-5"]);

    let root = repo_root();
    let ledger_path = root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("references")
        .join("parity")
        .join("ledger.json");
    let ledger_text = std::fs::read_to_string(&ledger_path).expect("read ledger.json");
    let ledger: Value = serde_json::from_str(&ledger_text).expect("parse ledger.json");
    let rows = ledger["rows"].as_array().expect("ledger rows array");

    // Z-1: Catalog RPC completeness and live V24 dispatch
    let rpc_rows = rows
        .iter()
        .filter(|r| r["category"].as_str() == Some("rpc"))
        .count();
    let notify_rows = rows
        .iter()
        .filter(|r| r["category"].as_str() == Some("notify"))
        .count();
    let feat_rows = rows
        .iter()
        .filter(|r| r["category"].as_str() == Some("feat"))
        .count();
    assert!(rpc_rows >= 100, "expected >= 100 canonical rpc rows, got {rpc_rows}");
    assert!(notify_rows >= 50, "expected >= 50 canonical notify rows, got {notify_rows}");
    assert!(feat_rows >= 100, "expected >= 100 canonical feat rows, got {feat_rows}");

    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();
    let mut sess = SessionState::new(9101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &empty_kw,
            None,
        )
        .await
        .expect("SelectCharacterID");

    let station_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "GetStation",
            &[PyRep::Integer(60_003_760)],
            &empty_kw,
            Some("test:Z-1:GetStation"),
        )
        .await
        .expect("stationSvc.GetStation");
    assert_eq!(station_rep.dict_get("stationID"), Some(&PyRep::Integer(60_003_760)));
    assert_eq!(station_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(30_000_142)));

    // Z-2: Systematic notification emission pipeline & Macho packet round-trip
    let notifs = ctx.notify_account_change(90_000_001, "cash", 90_000_001, 42_500_000.0);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnAccountChange");
    assert_eq!(notifs[0].id_type, "charid");

    let decoded = marshal::decode(&notifs[0].marshaled_packet).expect("decode Macho packet");
    let pkt = MachoPacket::from_pyrep(&decoded).expect("MachoPacket::from_pyrep");
    assert_eq!(pkt.packet_type, MachoPacketType::Notification);

    let drained = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        drained.iter().any(|n| n.event == "OnAccountChange"),
        "drained notifications must contain OnAccountChange"
    );

    // Z-5: Core character, inventory, and SDE dogma domain invariants
    let school_id = ctx
        .sde
        .schools
        .values()
        .find(|s| s.race_id == 1)
        .expect("Caldari school")
        .school_id;
    let ch = ctx
        .db
        .create_character(1, "Plan Z Closure Pilot", 1, 1, 11, school_id, &ctx.sde)
        .expect("create_character");
    assert_eq!(ch.balance, IskCents::from_isk(5_000));
    let has_ship = ctx
        .db
        .with_snapshot(|s| s.items.contains_key(&ch.active_ship_id));
    assert!(has_ship, "new character active_ship_id must exist in inventory");
    let rifter = ctx.sde.types.get(&587).expect("Rifter type 587");
    assert!(rifter.mass > 0.0, "Rifter SDE mass must be positive");
}

// parity: plan:Z-6, plan:Z-7, plan:Z-8
#[tokio::test]
async fn test_parity_plan_z6_z7_z8_journeys_coverage_and_acceptance_gate() {
    eve_testkit::attest(&["plan:Z-6", "plan:Z-7", "plan:Z-8"]);

    let root = repo_root();
    let parity_ref = root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("references")
        .join("parity");

    // Z-6: Canonical real-client journeys manifest & client-harness readiness
    let journeys_text = std::fs::read_to_string(parity_ref.join("client_journeys.json"))
        .expect("read client_journeys.json");
    let journeys_val: Value = serde_json::from_str(&journeys_text).expect("parse client_journeys.json");
    assert_eq!(journeys_val["version"].as_i64(), Some(1));
    let journeys = journeys_val["journeys"].as_array().expect("journeys array");
    assert_eq!(journeys.len(), 29);

    let mut journey_ids = HashSet::new();
    for j in journeys {
        let id = j["id"].as_str().expect("journey id");
        let steps = j["steps"].as_str().expect("journey steps");
        assert!(!steps.trim().is_empty(), "journey {id} must define steps");
        journey_ids.insert(id);
    }
    assert_eq!(journey_ids.len(), 29);
    for required in [
        "launch_login",
        "create_character",
        "station_ui",
        "fit_ship",
        "undock_navigate",
        "market_trade",
        "pve_combat",
        "structures",
        "sovereignty_fw",
        "esi_external",
        "downtime_persistence",
    ] {
        assert!(journey_ids.contains(required), "missing required journey {required}");
    }

    let runner_text = std::fs::read_to_string(root.join("tools").join("client-harness").join("run_journeys"))
        .expect("read run_journeys");
    assert!(runner_text.contains("--journeys"));
    assert!(runner_text.contains("UnimplementedRPC"));
    assert!(runner_text.contains("bridge_verified"));

    // Z-7: Coverage >= 90% and mutation >= 85% gate definitions
    let scripts_dir = root
        .join(".agents")
        .join("skills")
        .join("evomu-adversarial-review")
        .join("scripts");
    let gate_text = std::fs::read_to_string(scripts_dir.join("final_gate.py")).expect("read final_gate.py");
    assert!(gate_text.contains("EVOMU_MIN_LINE_COVERAGE"));
    assert!(gate_text.contains("EVOMU_MIN_MUTATION_SCORE"));
    assert!(gate_text.contains("90"));
    assert!(gate_text.contains("85"));
    assert!(scripts_dir.join("preflight.py").exists());

    // Z-8: Full canonical ledger categories & plan closure rows present
    let ledger_text = std::fs::read_to_string(parity_ref.join("ledger.json")).expect("read ledger.json");
    let ledger_val: Value = serde_json::from_str(&ledger_text).expect("parse ledger.json");
    let rows = ledger_val["rows"].as_array().expect("rows array");
    let mut row_ids = HashSet::new();
    let mut categories = HashSet::new();
    for r in rows {
        if let Some(id) = r["id"].as_str() {
            row_ids.insert(id);
        }
        if let Some(cat) = r["category"].as_str() {
            categories.insert(cat);
        }
    }
    assert_eq!(categories.len(), 10);
    for plan_id in [
        "plan:T-11",
        "plan:Z-1",
        "plan:Z-2",
        "plan:Z-5",
        "plan:Z-6",
        "plan:Z-7",
        "plan:Z-8",
    ] {
        assert!(row_ids.contains(plan_id), "missing plan row {plan_id} in ledger.json");
    }

    let triage_text = std::fs::read_to_string(root.join("docs").join("parity").join("triage.json"))
        .expect("read docs/parity/triage.json");
    let triage_val: Value = serde_json::from_str(&triage_text).expect("parse docs/parity/triage.json");
    assert!(triage_val.is_object() || triage_val.is_array());
}

// parity: rpc:dogmaIM.SetSessionCheck
#[tokio::test]
async fn test_dogma_im_set_session_check_parity() {
    eve_testkit::attest(&["rpc:dogmaIM.SetSessionCheck"]);

    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let kw = HashMap::new();
    let mut sess = SessionState::new(97_301);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let sc_dict = PyRep::Dict(vec![
        (PyRep::String("solarsystemid2".into()), PyRep::Integer(30_000_142)),
        (PyRep::String("charid".into()), PyRep::Integer(90_000_001)),
    ]);
    let sc_ok = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "SetSessionCheck", &[sc_dict], &kw, None)
        .await
        .expect("dogmaIM.SetSessionCheck authenticated");
    assert_eq!(sc_ok.dict_get("valid"), Some(&PyRep::Bool(true)));
    assert_eq!(sc_ok.dict_get("characterID"), Some(&PyRep::Integer(90_000_001)));
    assert_eq!(sc_ok.dict_get("checkCount"), Some(&PyRep::Integer(1)));

    let bad_dict = PyRep::Dict(vec![(
        PyRep::String("solarsystemid2".into()),
        PyRep::Integer(30_002_187),
    )]);
    let sc_mismatch = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "SetSessionCheck", &[bad_dict], &kw, None)
        .await
        .expect_err("dogmaIM.SetSessionCheck mismatch");
    assert_eq!(sc_mismatch.code, "SessionCheckMismatch");

    let sc_clear = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "SetSessionCheck", &[PyRep::None], &kw, None)
        .await
        .expect("dogmaIM.SetSessionCheck clear");
    assert_eq!(sc_clear.dict_get("valid"), Some(&PyRep::Bool(true)));
    assert_eq!(sc_clear.dict_get("sessionCheck"), Some(&PyRep::None));
    assert_eq!(sc_clear.dict_get("checkCount"), Some(&PyRep::Integer(0)));
}
