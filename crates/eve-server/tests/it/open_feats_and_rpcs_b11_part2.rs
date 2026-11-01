//! Integration tests for `feat:invariant.atomic-rollback-all-stores` and
//! Quasar `plex` / `character` / `user` gateway domains (batch 11 part 2).

#![forbid(unsafe_code)]

use crate::common;
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;
use eve_db::{InventoryFlag, IskCents, MiningLedgerRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_auth_user::{
    activate_user_omega_license, expire_user_omega_license,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_4_4: i64 = 60_003_760;

pub(crate) fn temp_sqlite_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
    p
}

pub(crate) fn cleanup_sqlite_path(p: &PathBuf) {
    let _ = std::fs::remove_file(p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
}

pub(crate) async fn select_char(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    s
}

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

pub(crate) async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: format!("b11-q-{n:08}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-b11-0001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(
        ctx,
        sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("publicGatewaySvc.SendRequest");
    if let PyRep::KeyVal(entries) = &rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("expected response_proto KeyVal, got {rep:?}");
}

fn flush_and_drain_notices(
    ctx: &ServerContext,
    rx: &mut Receiver<EvePublicNotice>,
) -> Vec<EvePublicNotice> {
    let queued: Vec<_> = ctx
        .quasar_gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect();
    for (scope, payload) in queued {
        let _ = ctx
            .quasar_gateway
            .publish_notice(scope, payload, ctx.now_unix());
    }
    let mut out = Vec::new();
    for _ in 0..512 {
        match rx.try_recv() {
            Ok(msg) => out.push(msg),
            Err(_) => break,
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ObservableWorldState {
    p1_balance: i64,
    p2_balance: i64,
    p1_corp_id: i64,
    corp_count: usize,
    item_count: usize,
    journal_len: usize,
    mining_ledger_len: usize,
    mining_total_qty: i64,
    custom_names: Vec<(i64, String)>,
    contacts: Vec<(i64, i64)>,
    blobs_digest: Vec<(String, String)>,
    esi_ship_name: Option<String>,
}

pub(crate) fn capture_observable_state(ctx: &ServerContext) -> ObservableWorldState {
    let esi_ship_name = ctx.esi_state.get_custom_item_name(100_000_001);
    ctx.db.with_snapshot(|snap| {
        let mut custom_names: Vec<(i64, String)> = snap
            .item_custom_names
            .iter()
            .filter(|(&id, _)| id > 0)
            .map(|(&id, v)| (id, v.clone()))
            .collect();
        custom_names.sort_by_key(|(id, _)| *id);

        let mut contacts: Vec<(i64, i64)> = snap
            .char_contacts
            .get(&P1)
            .map(|list| {
                list.iter()
                    .map(|c| (c.contact_id, (c.standing * 100.0) as i64))
                    .collect()
            })
            .unwrap_or_default();
        contacts.sort_by_key(|(id, _)| *id);

        let mut blobs_digest: Vec<(String, String)> = snap
            .service_state_blobs
            .iter()
            .filter(|(k, _)| !k.starts_with("season_manager:") && *k != "market_seeder_version")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        blobs_digest.sort_by(|a, b| a.0.cmp(&b.0));

        ObservableWorldState {
            p1_balance: snap.characters.get(&P1).map(|c| c.balance.0).unwrap_or(0),
            p2_balance: snap.characters.get(&P2).map(|c| c.balance.0).unwrap_or(0),
            p1_corp_id: snap
                .characters
                .get(&P1)
                .map(|c| c.corporation_id)
                .unwrap_or(0),
            corp_count: snap.corporations.len(),
            item_count: snap.items.len(),
            journal_len: snap.wallet_journal.len(),
            mining_ledger_len: snap.mining_ledger.len(),
            mining_total_qty: snap.mining_ledger.iter().map(|e| e.quantity).sum(),
            custom_names,
            contacts,
            blobs_digest,
            esi_ship_name,
        }
    })
}

#[tokio::test]
async fn test_invariant_atomic_rollback_randomized_fault_injection() {
    // parity: feat:invariant.atomic-rollback-all-stores
    eve_testkit::attest(&["feat:invariant.atomic-rollback-all-stores"]);

    let db_path = temp_sqlite_path("b11_atomic_rollback");
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(repo));
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db.with_mut_snapshot(|snap| *snap = demo);
    common::pin_clock(&ctx);

    let mut p1 = select_char(&ctx, 72_010, "demo", "demo-secret", P1).await;
    let mut rng_state: u64 = 0xB11A_701C_5EED_0042;

    for step in 0..40u64 {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        let op = rng_state % 8;
        let pre = capture_observable_state(&ctx);

        match op {
            0 => {
                rpc(
                    &ctx,
                    &mut p1,
                    "account",
                    "GiveCash",
                    &[
                        PyRep::Integer(P2),
                        PyRep::Real(100.0),
                        PyRep::String(format!("step-{step}")),
                    ],
                )
                .await
                .expect("valid GiveCash");
                let post = capture_observable_state(&ctx);
                assert_eq!(post.p1_balance, pre.p1_balance - 10_000);
                assert_eq!(post.p2_balance, pre.p2_balance + 10_000);
            }
            1 => {
                let name = format!("Ship-{step}");
                rpc(
                    &ctx,
                    &mut p1,
                    "invbroker",
                    "SetLabel",
                    &[PyRep::Integer(100_000_001), PyRep::String(name.clone())],
                )
                .await
                .expect("valid SetLabel");
                assert_eq!(capture_observable_state(&ctx).esi_ship_name, Some(name));
            }
            2 => {
                assert!(
                    rpc(
                        &ctx,
                        &mut p1,
                        "account",
                        "GiveCash",
                        &[
                            PyRep::Integer(P2),
                            PyRep::Real(999_999_999_999.0),
                            PyRep::String("overdraft".into()),
                        ],
                    )
                    .await
                    .is_err()
                );
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            3 => {
                assert!(
                    rpc(
                        &ctx,
                        &mut p1,
                        "corpRegistry",
                        "AddCorporation",
                        &[
                            PyRep::String("".into()),
                            PyRep::String("BAD".into()),
                            PyRep::String("invalid".into()),
                        ],
                    )
                    .await
                    .is_err()
                );
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            4 => {
                assert!(
                    rpc(
                        &ctx,
                        &mut p1,
                        "charMgr",
                        "AddContact",
                        &[PyRep::Integer(P1), PyRep::Real(5.0)],
                    )
                    .await
                    .is_err()
                );
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            5 => {
                assert!(
                    rpc(
                        &ctx,
                        &mut p1,
                        "invbroker",
                        "SetLabel",
                        &[
                            PyRep::Integer(100_000_002),
                            PyRep::String("Hijacked".into()),
                        ],
                    )
                    .await
                    .is_err()
                );
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            6 => {
                let err_res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&P1) {
                        ch.balance = IskCents(ch.balance.0 - 500_000);
                    }
                    snap.mining_ledger.push(MiningLedgerRecord {
                        character_id: P1,
                        solar_system_id: 30_000_142,
                        type_id: 1230,
                        quantity: 7777,
                        date: "2026-10-31".into(),
                        timestamp_unix: ctx.now_unix(),
                    });
                    snap.service_state_blobs
                        .insert(format!("fault_blob_{step}"), "partial".into());
                    Err(BridgeRpcError {
                        code: "InjectedHandlerFault".into(),
                        message: "Simulated mid-transaction failure".into(),
                    })
                });
                assert!(err_res.is_err());
                assert_eq!(capture_observable_state(&ctx), pre);
            }
            _ => {
                ctx.db.with_mut_snapshot(|snap| {
                    snap.mining_ledger.push(MiningLedgerRecord {
                        character_id: P1,
                        solar_system_id: 30_000_142,
                        type_id: 1230,
                        quantity: 50,
                        date: "2026-10-31".into(),
                        timestamp_unix: ctx.now_unix(),
                    });
                });
                assert_eq!(
                    capture_observable_state(&ctx).mining_ledger_len,
                    pre.mining_ledger_len + 1
                );
            }
        }
    }

    // Persistence-layer fault injection via SQLite BEFORE UPDATE / BEFORE INSERT triggers
    {
        let raw_conn = rusqlite::Connection::open(&db_path).expect("open sqlite for trigger");
        raw_conn
            .execute_batch(
                "CREATE TRIGGER fail_char_persist BEFORE UPDATE ON characters \
                 BEGIN SELECT RAISE(ABORT, 'injected_disk_persist_failure'); END; \
                 CREATE TRIGGER fail_contact_ins BEFORE INSERT ON char_contacts \
                 BEGIN SELECT RAISE(ABORT, 'injected_contact_persist_failure'); END; \
                 CREATE TRIGGER fail_contact_upd BEFORE UPDATE ON char_contacts \
                 BEGIN SELECT RAISE(ABORT, 'injected_contact_persist_failure'); END; \
                 CREATE TRIGGER fail_label_ins BEFORE INSERT ON item_custom_names \
                 BEGIN SELECT RAISE(ABORT, 'injected_label_persist_failure'); END; \
                 CREATE TRIGGER fail_label_upd BEFORE UPDATE ON item_custom_names \
                 BEGIN SELECT RAISE(ABORT, 'injected_label_persist_failure'); END;",
            )
            .expect("install fault trigger");
    }
    let pre_persist_fault = capture_observable_state(&ctx);

    // 1. Dispatch real mutating RPCs while SQLite triggers reject persistence commits
    let fault_cash = rpc(
        &ctx,
        &mut p1,
        "account",
        "GiveCash",
        &[PyRep::Integer(P2), PyRep::Real(5_000.0), PyRep::String("fault_cash".into())],
    )
    .await;
    assert!(fault_cash.is_err());
    assert_eq!(capture_observable_state(&ctx), pre_persist_fault);

    let fault_contact = rpc(
        &ctx,
        &mut p1,
        "charMgr",
        "AddContact",
        &[PyRep::Integer(90_000_003), PyRep::Real(10.0), PyRep::Bool(true)],
    )
    .await;
    assert!(fault_contact.is_err());
    assert_eq!(capture_observable_state(&ctx), pre_persist_fault);

    let fault_label = rpc(
        &ctx,
        &mut p1,
        "invbroker",
        "SetLabel",
        &[PyRep::Integer(100_000_001), PyRep::String("Uncommitted Ship Name".into())],
    )
    .await;
    assert!(fault_label.is_err());
    assert_eq!(capture_observable_state(&ctx), pre_persist_fault);

    // 2. Direct multi-store snapshot transaction fault injection
    let persist_fault_res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.balance = IskCents(ch.balance.0 - 250_000);
        }
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: P1,
            solar_system_id: 30_000_142,
            type_id: 1228,
            quantity: 9_999,
            date: "2026-10-31".into(),
            timestamp_unix: ctx.now_unix(),
        });
        snap.service_state_blobs
            .insert("uncommitted_blob".into(), "must_rollback".into());
        Ok(())
    });
    assert!(persist_fault_res.is_err());
    assert_eq!(capture_observable_state(&ctx), pre_persist_fault);
    {
        let raw_conn = rusqlite::Connection::open(&db_path).expect("open sqlite to drop trigger");
        raw_conn
            .execute_batch(
                "DROP TRIGGER fail_char_persist; \
                 DROP TRIGGER fail_contact_ins; \
                 DROP TRIGGER fail_contact_upd; \
                 DROP TRIGGER fail_label_ins; \
                 DROP TRIGGER fail_label_upd;",
            )
            .expect("drop fault trigger");
    }

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    assert!(repo2.with_snapshot(|s| !s.service_state_blobs.contains_key("uncommitted_blob")));
    let ctx2 = ServerContext::with_repository(repo2);
    assert_eq!(capture_observable_state(&ctx2), pre_persist_fault);

    cleanup_sqlite_path(&db_path);
}

#[tokio::test]
async fn test_quasar_plex_character_and_user_gateway_domains() {
    // Hardened Quasar PLEX, character, and user flows (reopened pending full sub-feature coverage)

    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut unauth = SessionState::new(72_040);
    for url in [
        "eve_public.plex.vault.api.BalanceRequest",
        "eve_public.character.GetNamesByUserRequest",
        "eve_public.user.license.api.GetRequest",
    ] {
        assert_eq!(
            quasar_rpc(&ctx, &mut unauth, url, vec![]).await.status_code,
            STATUS_UNAUTHORIZED
        );
    }

    let mut p1 = select_char(&ctx, 72_041, "demo", "demo-secret", P1).await;
    let mut p2 = select_char(&ctx, 72_042, "other", "other-secret", P2).await;
    let mut rx1 = ctx
        .quasar_gateway
        .subscribe_session(&p1, vec![], ctx.now_unix());

    // 1. feat:quasar.plex
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.plex.vault.api.BalanceRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );
    let plex_item = ctx
        .db
        .add_or_stack_item(P1, JITA_4_4, InventoryFlag::Hangar, 44_992, 40)
        .expect("seed plex")
        .item_id;
    rpc(
        &ctx,
        &mut p1,
        "invbroker",
        "DepositPlexToVault",
        &[
            PyRep::Integer(JITA_4_4),
            PyRep::Integer(plex_item),
            PyRep::Integer(40),
        ],
    )
    .await
    .expect("DepositPlexToVault");
    let plex_notices = flush_and_drain_notices(&ctx, &mut rx1);
    assert!(plex_notices.iter().any(|n| {
        n.payload
            .as_ref()
            .is_some_and(|p| p.type_url.contains("plex.vault.api.BalanceChangedNotice"))
    }));

    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest",
            vec![]
        )
        .await
        .status_code,
        STATUS_OK
    );
    let tx_id = ctx.db.with_snapshot(|snap| {
        eve_server::services::quasar_payment_store::get_plex_transactions(snap)
            .into_iter()
            .find(|t| t.user_id == 1)
            .expect("user 1 plex tx")
            .id
    });
    let (mut b_id, mut log_req) = (Vec::new(), Vec::new());
    encode_varint_field(1, tx_id, &mut b_id);
    encode_bytes_field(1, &b_id, &mut log_req);
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve_public.plex.vault.transaction.api.GetLogRequest",
            log_req.clone()
        )
        .await
        .status_code,
        STATUS_OK
    );
    let log_url = "eve_public.plex.vault.transaction.api.GetLogRequest";
    assert_eq!(quasar_rpc(&ctx, &mut p2, log_url, log_req.clone()).await.status_code, STATUS_NOT_FOUND);

    // 2. feat:quasar.character
    let names_rsp = quasar_rpc(&ctx, &mut p1, "eve_public.character.GetNamesByUserRequest", vec![]).await;
    assert_eq!(names_rsp.status_code, STATUS_OK);
    assert!(!names_rsp.payload.expect("names").value.is_empty());
    let sp_create_url = "eve_public.character.skill.plan.CreateRequest";
    assert_eq!(quasar_rpc(&ctx, &mut p1, sp_create_url, vec![]).await.status_code, STATUS_BAD_REQUEST);

    let (mut attr_b, mut req_b, mut tid_b, mut create_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "B11 Gunnery Plan", &mut attr_b);
    encode_varint_field(1, 3300, &mut tid_b);
    encode_bytes_field(1, &tid_b, &mut req_b);
    encode_varint_field(2, 4, &mut req_b);
    encode_bytes_field(2, &req_b, &mut attr_b);
    encode_string_field(3, "Gunnery IV milestone plan", &mut attr_b);
    encode_bytes_field(1, &attr_b, &mut create_b);

    let sp_create = quasar_rpc(&ctx, &mut p1, sp_create_url, create_b).await;
    assert_eq!(sp_create.status_code, STATUS_OK);
    let sp_wm = WireMessage::decode(&sp_create.payload.expect("sp create").value).expect("wm");
    let id_b = sp_wm.fields.iter().find(|f| f.field_number == 1).expect("id").value.as_bytes().expect("bytes");
    let sp_id_wm = WireMessage::decode(id_b).expect("id wm");
    let uuid_b = sp_id_wm.fields.iter().find(|f| f.field_number == 1).expect("uuid").value.as_bytes().expect("uuid bytes");
    let plan_uuid: [u8; 16] = uuid_b.try_into().expect("16b uuid");
    let (mut id_inner, mut plan_id_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &plan_uuid, &mut id_inner);
    encode_bytes_field(1, &id_inner, &mut plan_id_req);

    let sp_active_url = "eve_public.character.skill.plan.SetActiveRequest";
    let sp_ms_url = "eve_public.character.skill.plan.milestone.GetAllRequest";
    assert_eq!(quasar_rpc(&ctx, &mut p1, sp_active_url, plan_id_req.clone()).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut p1, sp_ms_url, plan_id_req.clone()).await.status_code, STATUS_OK);

    // 3. feat:quasar.user
    let now = ctx.now_unix();
    let expected_exp = (now + 30 * 86_400).to_string();
    activate_user_omega_license(&ctx.db, &ctx.quasar_gateway.pending_notices, 1, 30 * 86_400, now);
    let lic_notices = flush_and_drain_notices(&ctx, &mut rx1);
    assert!(lic_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("user.license.api.ActivatedNotice"))));
    let lic_url = "eve_public.user.license.api.GetRequest";
    let lic_active = quasar_rpc(&ctx, &mut p1, lic_url, vec![]).await;
    assert_eq!(lic_active.status_code, STATUS_OK);
    let lic_dyn = reg.decode_dynamic("eve_public.user.license.api.GetResponse", &lic_active.payload.expect("license").value).expect("decode");
    assert!(format!("{lic_dyn:?}").contains(&expected_exp));

    let (mut doc_id, mut accept_req) = (Vec::new(), Vec::new());
    encode_string_field(1, "privacy_policy_2026", &mut doc_id);
    encode_bytes_field(3, &doc_id, &mut accept_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.user.legal.agreement.api.AcceptRequest", accept_req)
            .await
            .status_code,
        STATUS_OK
    );
    let latest_eula = quasar_rpc(&ctx, &mut p1, "eve.legal.agreement.api.GetLatestRequest", vec![]).await;
    assert_eq!(latest_eula.status_code, STATUS_OK);
    let eula_wm = WireMessage::decode(&latest_eula.payload.expect("latest eula").value).expect("eula wm");
    let eula_doc = WireMessage::decode(
        eula_wm.fields.iter().find(|f| f.field_number == 1).expect("doc").value.as_bytes().expect("bytes"),
    )
    .expect("doc wm");
    let eula_id = std::str::from_utf8(
        eula_doc.fields.iter().find(|f| f.field_number == 2).expect("field 2").value.as_bytes().expect("bytes"),
    )
    .expect("utf8");
    assert_eq!(eula_id, "privacy_policy_2026");

    let ctx_reloaded = persist_and_reopen_sqlite(&ctx, "b11_quasar_domains_reload");
    let mut p1_re = select_char(&ctx_reloaded, 72_043, "demo", "demo-secret", P1).await;
    assert_eq!(quasar_rpc(&ctx_reloaded, &mut p1_re, log_url, log_req).await.status_code, STATUS_OK);
    let sp_get_url = "eve_public.character.skill.plan.GetRequest";
    assert_eq!(quasar_rpc(&ctx_reloaded, &mut p1_re, sp_get_url, plan_id_req).await.status_code, STATUS_OK);
    let lic_reloaded = quasar_rpc(&ctx_reloaded, &mut p1_re, lic_url, vec![]).await;
    assert_eq!(lic_reloaded.status_code, STATUS_OK);
    let lic_re_dyn = reg.decode_dynamic("eve_public.user.license.api.GetResponse", &lic_reloaded.payload.expect("reloaded").value).expect("decode");
    assert!(format!("{lic_re_dyn:?}").contains(&expected_exp));

    let mut rx_re = ctx_reloaded.quasar_gateway.subscribe_session(&p1_re, vec![], ctx_reloaded.now_unix());
    expire_user_omega_license(&ctx_reloaded.db, &ctx_reloaded.quasar_gateway.pending_notices, 1);
    let exp_notices = flush_and_drain_notices(&ctx_reloaded, &mut rx_re);
    assert!(exp_notices.iter().any(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("user.license.api.ExpiredNotice"))));
    assert_eq!(ctx_reloaded.db.with_snapshot(|s| s.characters.get(&P1).expect("P1").clone_grade), 1);
}
