//! Integration tests for Account, Alerts, AIR NPE, Banned Words, EventLog, and PaperDoll RPCs:
//! - `account`: `GetCashBalance`, `GetDefaultContactCost`, `GiveCash`, `SetContactCost`
//! - `alert`: `BeanCount`, `BeanDelivery`, `GetLogModeForError`, `GroupBeanDelivery`, `SendClientStackTraceAlert`
//! - `air_npe`: `get_air_npe_state`, `is_air_npe_enabled`, `skip_air_npe`
//! - `bannedwords`: `check_search_words_allowed`, `check_words_allowed`, `fetch_regexes`
//! - `eventLog`: `LogClientEvent`, `LogClientStats`, `LogPlayerRequestedDisconnect`
//! - `paperDollServer`: `GetPaperDollData`, `GetPaperDollDataBatch`, `GetPaperDollPortraitDataFor`, `UpdateExistingCharacterLimited`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

use super::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;

fn field<'a>(rep: &'a PyRep, key: &str) -> &'a PyRep {
    match rep {
        PyRep::KeyVal(kv) => kv
            .get(key)
            .unwrap_or_else(|| panic!("missing keyval field: {key}")),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("missing dict field: {key}")),
        other => panic!("expected KeyVal or Dict, got {other:?}"),
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_pilot_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

#[tokio::test]
// parity: rpc:account.GetCashBalance
// parity: rpc:account.GetDefaultContactCost
// parity: rpc:account.GiveCash
// parity: rpc:account.SetContactCost
// parity: rpc:alert.BeanCount
// parity: rpc:alert.BeanDelivery
// parity: rpc:alert.GetLogModeForError
// parity: rpc:alert.GroupBeanDelivery
// parity: rpc:alert.SendClientStackTraceAlert
async fn test_account_and_alert_rpcs() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = login_pilot_char(&ctx, 3001, "demo", "demo-secret", PILOT_1).await;
    let _sess2 = login_pilot_char(&ctx, 3002, "other", "other-secret", PILOT_2).await;

    // 1. account.GetCashBalance: returns authentic balance
    let bal_rep = rpc(&ctx, &mut sess1, "account", "GetCashBalance", &[])
        .await
        .expect("GetCashBalance");
    let initial_bal = match bal_rep {
        PyRep::Real(r) => r,
        other => panic!("expected Real cash balance, got {other:?}"),
    };
    assert!(initial_bal > 0.0, "expected positive starting balance");

    // 2. account.SetContactCost: sets custom CSPA charge in snapshot
    let set_cost_rep = rpc(
        &ctx,
        &mut sess1,
        "account",
        "SetContactCost",
        &[PyRep::Real(100_000.0)],
    )
    .await
    .expect("SetContactCost");
    assert_eq!(set_cost_rep, PyRep::None);
    ctx.db.with_snapshot(|snap| {
        let cost = snap.character_cspa_charges.get(&PILOT_1).expect("cspa cost in snap");
        assert_eq!(cost.to_wire_f64(), 100_000.0);
    });

    // 3. account.GetDefaultContactCost: queries current CSPA cost
    let cost_rep = rpc(&ctx, &mut sess1, "account", "GetDefaultContactCost", &[])
        .await
        .expect("GetDefaultContactCost");
    assert_eq!(cost_rep, PyRep::Real(100_000.0));

    // Reset contact cost to None
    rpc(&ctx, &mut sess1, "account", "SetContactCost", &[PyRep::None])
        .await
        .expect("Reset Contact Cost");
    let reset_rep = rpc(&ctx, &mut sess1, "account", "GetDefaultContactCost", &[])
        .await
        .expect("GetDefaultContactCost after reset");
    assert_eq!(reset_rep, PyRep::None);

    // 4. account.GiveCash: transfers cash between pilots with journal entry
    let p2_initial_bal = ctx.db.get_character(PILOT_2).unwrap().balance.to_wire_f64();
    let transfer_amount = 50_000.0;
    let give_rep = rpc(
        &ctx,
        &mut sess1,
        "account",
        "GiveCash",
        &[PyRep::Integer(PILOT_2), PyRep::Real(transfer_amount), PyRep::String("Fleet SRP".into())],
    )
    .await
    .expect("GiveCash");
    match give_rep {
        PyRep::Real(r) => assert_eq!(r, initial_bal - transfer_amount),
        other => panic!("expected Real new sender balance, got {other:?}"),
    }
    let p2_new_bal = ctx.db.get_character(PILOT_2).unwrap().balance.to_wire_f64();
    assert_eq!(p2_new_bal, p2_initial_bal + transfer_amount);

    // 5. alert.SendClientStackTraceAlert: reports stack trace, registers error ID
    let stack_tuple = PyRep::Tuple(vec![PyRep::Integer(42001), PyRep::String("TestException".into())]);
    let trace_text = "ZeroDivisionError: division by zero\n  File 'test.py', line 12";
    let alert_rep = rpc(
        &ctx,
        &mut sess1,
        "alert",
        "SendClientStackTraceAlert",
        &[stack_tuple, PyRep::String(trace_text.into()), PyRep::String("B".into())],
    )
    .await
    .expect("SendClientStackTraceAlert");
    let err_id = match alert_rep {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 2);
            items[0].as_i64().expect("errorID in first position")
        }
        other => panic!("expected Tuple from SendClientStackTraceAlert, got {other:?}"),
    };

    // 6. alert.BeanCount: queries logging mode for stack trace hash
    let bean_count_rep = rpc(&ctx, &mut sess1, "alert", "BeanCount", &[PyRep::Integer(42001)])
        .await
        .expect("BeanCount");
    match bean_count_rep {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].as_i64(), Some(err_id));
        }
        other => panic!("expected Tuple from BeanCount, got {other:?}"),
    }

    // 7. alert.GetLogModeForError: queries log mode for specific error ID
    let log_mode_rep = rpc(&ctx, &mut sess1, "alert", "GetLogModeForError", &[PyRep::Integer(err_id)])
        .await
        .expect("GetLogModeForError");
    match log_mode_rep {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].as_i64(), Some(err_id));
        }
        other => panic!("expected Tuple from GetLogModeForError, got {other:?}"),
    }

    // 8. alert.BeanDelivery: delivers telemetry bean occurrences
    let beans_dict = PyRep::Dict(vec![(
        PyRep::Integer(err_id),
        PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(1_700_000_000)]),
    )]);
    let deliv_rep = rpc(&ctx, &mut sess1, "alert", "BeanDelivery", &[beans_dict])
        .await
        .expect("BeanDelivery");
    assert_eq!(deliv_rep, PyRep::None);
    assert!(eve_server::services::client_login::delivered_bean_count(err_id) >= 5);

    // 9. alert.GroupBeanDelivery: delivers batched bean occurrences
    let grp_dict = PyRep::Dict(vec![(PyRep::Integer(err_id), PyRep::Integer(3))]);
    let grp_rep = rpc(&ctx, &mut sess1, "alert", "GroupBeanDelivery", &[grp_dict])
        .await
        .expect("GroupBeanDelivery");
    assert_eq!(grp_rep, PyRep::None);
}

#[tokio::test]
// parity: rpc:air_npe.get_air_npe_state
// parity: rpc:air_npe.is_air_npe_enabled
// parity: rpc:air_npe.skip_air_npe
// parity: rpc:bannedwords.check_search_words_allowed
// parity: rpc:bannedwords.check_words_allowed
// parity: rpc:bannedwords.fetch_regexes
// parity: rpc:eventLog.LogClientEvent
// parity: rpc:eventLog.LogClientStats
// parity: rpc:eventLog.LogPlayerRequestedDisconnect
async fn test_air_npe_and_bannedwords_and_eventlog() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = login_pilot_char(&ctx, 4001, "demo", "demo-secret", PILOT_1).await;

    // 1. air_npe.is_air_npe_enabled: returns bool
    let enabled_rep = rpc(&ctx, &mut sess1, "air_npe", "is_air_npe_enabled", &[])
        .await
        .expect("is_air_npe_enabled");
    assert_eq!(enabled_rep, PyRep::Bool(true));

    // 2. air_npe.get_air_npe_state: returns state keyval
    let state_rep = rpc(&ctx, &mut sess1, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("get_air_npe_state");
    assert_eq!(field(&state_rep, "nodeGraphID"), &PyRep::Integer(473));
    assert_eq!(field(&state_rep, "enabled"), &PyRep::Bool(true));

    // 3. air_npe.skip_air_npe: marks NPE as completed in snapshot
    let skip_rep = rpc(&ctx, &mut sess1, "air_npe", "skip_air_npe", &[])
        .await
        .expect("skip_air_npe");
    assert_eq!(field(&skip_rep, "completed"), &PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let npe = snap.npe_progress.get(&PILOT_1).expect("npe progress in snap");
        assert!(npe.completed);
    });

    // 4. bannedwords.fetch_regexes: returns compiled regex patterns
    let regex_rep = rpc(&ctx, &mut sess1, "bannedwords", "fetch_regexes", &[])
        .await
        .expect("fetch_regexes");
    match regex_rep {
        PyRep::List(list) => assert!(!list.is_empty(), "expected regex patterns"),
        other => panic!("expected List from fetch_regexes, got {other:?}"),
    }

    // 5. bannedwords.check_words_allowed: checks list of candidate words
    let words_rep = rpc(
        &ctx,
        &mut sess1,
        "bannedwords",
        "check_words_allowed",
        &[PyRep::List(vec![
            PyRep::String("miner".into()),
            PyRep::String("bannedword".into()),
            PyRep::String("hauler".into()),
        ])],
    )
    .await
    .expect("check_words_allowed");
    match words_rep {
        PyRep::List(results) => {
            assert_eq!(results.len(), 3);
            assert_eq!(results[0], PyRep::Bool(true));
            assert_eq!(results[1], PyRep::Bool(false));
            assert_eq!(results[2], PyRep::Bool(true));
        }
        other => panic!("expected List of bools, got {other:?}"),
    }

    // 6. bannedwords.check_search_words_allowed: validates search phrases
    let clean_search = rpc(
        &ctx,
        &mut sess1,
        "bannedwords",
        "check_search_words_allowed",
        &[PyRep::String("Tritanium Ore".into())],
    )
    .await
    .expect("check_search_words_allowed clean");
    assert_eq!(clean_search, PyRep::Bool(true));

    let dirty_search = rpc(
        &ctx,
        &mut sess1,
        "bannedwords",
        "check_search_words_allowed",
        &[PyRep::String("exploit script".into())],
    )
    .await
    .expect("check_search_words_allowed dirty");
    assert_eq!(dirty_search, PyRep::Bool(false));

    // 7. eventLog.LogClientEvent: records client event telemetry into snapshot
    let ev_rep = rpc(
        &ctx,
        &mut sess1,
        "eventLog",
        "LogClientEvent",
        &[PyRep::String("AutopilotEngaged".into()), PyRep::Integer(1001)],
    )
    .await
    .expect("LogClientEvent");
    assert_eq!(ev_rep, PyRep::None);

    // 8. eventLog.LogClientStats: records performance telemetry into snapshot
    let stats_rep = rpc(
        &ctx,
        &mut sess1,
        "eventLog",
        "LogClientStats",
        &[PyRep::Dict(vec![(PyRep::String("fps".into()), PyRep::Integer(60))])],
    )
    .await
    .expect("LogClientStats");
    assert_eq!(stats_rep, PyRep::None);

    // 9. eventLog.LogPlayerRequestedDisconnect: logs graceful client quit
    let dc_rep = rpc(&ctx, &mut sess1, "eventLog", "LogPlayerRequestedDisconnect", &[])
        .await
        .expect("LogPlayerRequestedDisconnect");
    assert_eq!(dc_rep, PyRep::None);
}

#[tokio::test]
// parity: rpc:paperDollServer.GetPaperDollData
// parity: rpc:paperDollServer.GetPaperDollDataBatch
// parity: rpc:paperDollServer.GetPaperDollPortraitDataFor
// parity: rpc:paperDollServer.UpdateExistingCharacterLimited
async fn test_paperdoll_server_rpcs() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess1 = login_pilot_char(&ctx, 5001, "demo", "demo-secret", PILOT_1).await;
    let _sess2 = login_pilot_char(&ctx, 5002, "other", "other-secret", PILOT_2).await;

    // Seed custom paperdoll data for PILOT_1
    ctx.db.with_mut_snapshot(|snap| {
        snap.character_doll_info.insert(
            PILOT_1,
            ("dna:pilot1_custom".into(), "portrait:pilot1_custom".into()),
        );
    });

    // 1. paperDollServer.GetPaperDollData: returns character doll DNA and apparel
    let doll_rep = rpc(
        &ctx,
        &mut sess1,
        "paperDollServer",
        "GetPaperDollData",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("GetPaperDollData");
    assert_eq!(field(&doll_rep, "characterID"), &PyRep::Integer(PILOT_1));
    assert_eq!(field(&doll_rep, "dollDNA"), &PyRep::String("dna:pilot1_custom".into()));
    assert_eq!(field(&doll_rep, "portraitInfo"), &PyRep::String("portrait:pilot1_custom".into()));

    // 2. paperDollServer.GetPaperDollDataBatch: queries multiple characters
    let batch_rep = rpc(
        &ctx,
        &mut sess1,
        "paperDollServer",
        "GetPaperDollDataBatch",
        &[PyRep::List(vec![PyRep::Integer(PILOT_1), PyRep::Integer(PILOT_2)])],
    )
    .await
    .expect("GetPaperDollDataBatch");
    match batch_rep {
        PyRep::Dict(entries) => {
            assert_eq!(entries.len(), 2, "expected 2 characters in batch");
            let p1_entry = entries
                .iter()
                .find(|(k, _)| k.as_i64() == Some(PILOT_1))
                .map(|(_, v)| v)
                .expect("pilot 1 entry in batch");
            assert_eq!(field(p1_entry, "characterID"), &PyRep::Integer(PILOT_1));
        }
        other => panic!("expected Dict from GetPaperDollDataBatch, got {other:?}"),
    }

    // 3. paperDollServer.GetPaperDollPortraitDataFor: returns portrait render settings
    let port_rep = rpc(
        &ctx,
        &mut sess1,
        "paperDollServer",
        "GetPaperDollPortraitDataFor",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("GetPaperDollPortraitDataFor");
    assert_eq!(field(&port_rep, "characterID"), &PyRep::Integer(PILOT_1));
    assert_eq!(field(&port_rep, "portraitInfo"), &PyRep::String("portrait:pilot1_custom".into()));

    // 4. paperDollServer.UpdateExistingCharacterLimited: updates paperdoll attributes
    let update_kv = PyRep::keyval([
        ("hair", PyRep::Integer(10)),
        ("tattoo", PyRep::Integer(3)),
    ]);
    let update_rep = rpc(
        &ctx,
        &mut sess1,
        "paperDollServer",
        "UpdateExistingCharacterLimited",
        &[PyRep::Integer(PILOT_1), update_kv],
    )
    .await
    .expect("UpdateExistingCharacterLimited");
    assert_eq!(field(&update_rep, "characterID"), &PyRep::Integer(PILOT_1));
    assert_eq!(field(&update_rep, "updated"), &PyRep::Bool(true));

    // Ownership check: updating PILOT_2 from PILOT_1 session fails with CharacterOwnershipMismatch
    let mismatch_err = rpc(
        &ctx,
        &mut sess1,
        "paperDollServer",
        "UpdateExistingCharacterLimited",
        &[PyRep::Integer(PILOT_2), PyRep::keyval([("hair", PyRep::Integer(1))])],
    )
    .await
    .expect_err("Cross-character paperdoll update must fail");
    assert_eq!(mismatch_err.code, "CharacterOwnershipMismatch");
}
