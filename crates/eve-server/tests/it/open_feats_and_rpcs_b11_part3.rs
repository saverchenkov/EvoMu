//! Integration test for `feat:invariant.single-source-of-truth`:
//! Contacts, mail labels & deleted mail, custom item names, CSPA charges, war records,
//! alliances, and PLEX balances read identically across MachoNet client RPC, ESI, and
//! Quasar live and after SQLite restart (batch 11 part 3).

#![forbid(unsafe_code)]

use crate::common;
use crate::open_feats_and_rpcs_b11_part2::{
    cleanup_sqlite_path, quasar_rpc, rpc, select_char, temp_sqlite_path,
};
use eve_db::{
    AllianceLifecycleMeta, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, WarRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field, QuasarDescriptorRegistry};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_4_4: i64 = 60_003_760;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token(
    ctx: &ServerContext,
    user: &str,
    pass: &str,
    char_id: i64,
    scopes: &[&str],
) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            user,
            pass,
            char_id,
            "evomu-b11-ssot",
            "https://localhost/callback",
            scopes.iter().map(|s| s.to_string()).collect(),
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-b11-ssot", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn esi_call(
    router: &EsiRouter,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Vec<u8>>,
) -> serde_json::Value {
    let mut req = EsiHttpRequest::new(method, path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    if let Some(b) = body {
        req.body = b;
    }
    let resp = router.handle_request(&req);
    assert!(
        matches!(resp.status_code, 200 | 201),
        "ESI {method} {path} failed ({}): {}",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    serde_json::from_slice(&resp.body).expect("valid ESI JSON")
}

#[tokio::test]
async fn test_invariant_single_source_of_truth_client_esi_quasar_and_restart() {
    let db_path = temp_sqlite_path("b11_ssot");
    let corp_id = 98_200_001i64;
    let rival_corp_id = 98_200_002i64;
    let alliance_id = 99_200_001i64;
    let war_id = 88_001i64;
    let ship_id = 100_000_001i64;
    let plex_stack_id = 100_777_001i64;

    let (mail_deleted, mail_kept, mail_label_id) = {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 1"));
        let ctx = Arc::new(ServerContext::with_repository(repo));
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            snap.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "B11 SSOT Corp".into(),
                    ticker: "BSOT".into(),
                    ceo_id: P1,
                    alliance_id: Some(alliance_id),
                    hq_station_id: Some(JITA_4_4),
                    members: [P1].into_iter().collect(),
                    ..Default::default()
                },
            );
            snap.corporations.insert(
                rival_corp_id,
                CorporationRecord {
                    corporation_id: rival_corp_id,
                    name: "B11 Rival Corp".into(),
                    ticker: "BRVL".into(),
                    ceo_id: P2,
                    hq_station_id: Some(JITA_4_4),
                    members: [P2].into_iter().collect(),
                    ..Default::default()
                },
            );
            snap.alliances.insert(
                alliance_id,
                AllianceRecord {
                    alliance_id,
                    name: "B11 Unified Alliance".into(),
                    short_name: "B11UA".into(),
                    executor_corp_id: corp_id,
                    member_corp_ids: [corp_id].into_iter().collect(),
                },
            );
            snap.alliance_lifecycle_meta.insert(
                alliance_id,
                AllianceLifecycleMeta {
                    creator_id: P1,
                    creator_corporation_id: corp_id,
                    date_founded_unix: 1_700_000_000,
                    faction_id: None,
                },
            );
            snap.wars.insert(
                war_id,
                WarRecord {
                    war_id,
                    aggressor_corp_id: corp_id,
                    defender_corp_id: rival_corp_id,
                    mutual: false,
                    started_unix: 1_700_000_000,
                    active: true,
                },
            );
            if let Some(c1) = snap.characters.get_mut(&P1) {
                c1.corporation_id = corp_id;
                c1.alliance_id = Some(alliance_id);
                c1.balance = IskCents(50_000_000_00);
            }
            if let Some(c2) = snap.characters.get_mut(&P2) {
                c2.corporation_id = rival_corp_id;
            }
            snap.items.insert(
                plex_stack_id,
                InventoryItemRecord {
                    item_id: plex_stack_id,
                    type_id: 44_992,
                    owner_id: P1,
                    location_id: JITA_4_4,
                    flag: InventoryFlag::Hangar,
                    quantity: 200,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        common::pin_clock(&ctx);

        let mut s1 = select_char(&ctx, 72_020, "demo", "demo-secret", P1).await;
        let mut s2 = select_char(&ctx, 72_021, "other", "other-secret", P2).await;

        rpc(
            &ctx,
            &mut s1,
            "account",
            "SetContactCost",
            &[PyRep::Integer(25_000)],
        )
        .await
        .expect("P1 SetContactCost");
        rpc(
            &ctx,
            &mut s2,
            "account",
            "SetContactCost",
            &[PyRep::Integer(12_500)],
        )
        .await
        .expect("P2 SetContactCost");
        rpc(
            &ctx,
            &mut s1,
            "charMgr",
            "AddContact",
            &[PyRep::Integer(P2), PyRep::Real(8.0), PyRep::Bool(true)],
        )
        .await
        .expect("AddContact");

        let lbl_id = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "CreateLabel",
            &[
                PyRep::String("PriorityOps".into()),
                PyRep::String("#ffaa00".into()),
            ],
        )
        .await
        .expect("mailMgr.CreateLabel")
        .as_i64()
        .expect("mail label id");
        let m_del = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(P1)]),
                PyRep::String("Purged Dispatch".into()),
                PyRep::String("Delete me".into()),
                PyRep::Integer(0),
            ],
        )
        .await
        .expect("SendMail 1")
        .as_i64()
        .expect("m_del");
        let m_keep = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(P1)]),
                PyRep::String("Retained Dispatch".into()),
                PyRep::String("Keep me".into()),
                PyRep::Integer(0),
            ],
        )
        .await
        .expect("SendMail 2")
        .as_i64()
        .expect("m_keep");
        rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "DeleteMail",
            &[PyRep::List(vec![PyRep::Integer(m_del)])],
        )
        .await
        .expect("DeleteMail");

        rpc(
            &ctx,
            &mut s1,
            "invbroker",
            "SetLabel",
            &[
                PyRep::Integer(ship_id),
                PyRep::String("B11 Sovereign Hull".into()),
            ],
        )
        .await
        .expect("invbroker.SetLabel");

        rpc(
            &ctx,
            &mut s1,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_4_4),
                PyRep::Integer(plex_stack_id),
                PyRep::Integer(120),
                PyRep::Integer(0),
            ],
        )
        .await
        .expect("DepositPlexToVault");

        verify_all_seven_ssot_domains(
            &ctx,
            &mut s1,
            corp_id,
            rival_corp_id,
            alliance_id,
            war_id,
            ship_id,
            lbl_id,
            m_del,
            m_keep,
        )
        .await;

        (m_del, m_keep, lbl_id)
    };

    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 2"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    common::pin_clock(&ctx2);
    let mut s1_re = select_char(&ctx2, 72_022, "demo", "demo-secret", P1).await;
    verify_all_seven_ssot_domains(
        &ctx2,
        &mut s1_re,
        corp_id,
        rival_corp_id,
        alliance_id,
        war_id,
        ship_id,
        mail_label_id,
        mail_deleted,
        mail_kept,
    )
    .await;

    cleanup_sqlite_path(&db_path);
}

#[allow(clippy::too_many_arguments)]
async fn verify_all_seven_ssot_domains(
    ctx: &Arc<ServerContext>,
    s1: &mut SessionState,
    corp_id: i64,
    rival_corp_id: i64,
    alliance_id: i64,
    war_id: i64,
    ship_id: i64,
    mail_label_id: i64,
    mail_deleted: i64,
    mail_kept: i64,
) {
    let router = EsiRouter::new(Arc::clone(ctx));
    let tok1 = issue_token(
        ctx,
        "demo",
        "demo-secret",
        P1,
        &[
            "esi-characters.read_contacts.v1",
            "esi-characters.cspa.v1",
            "esi-mail.read_mail.v1",
            "esi-assets.read_assets.v1",
        ],
    );

    // 1. Contacts
    let contacts_rpc = rpc(ctx, s1, "charMgr", "GetContacts", &[])
        .await
        .expect("GetContacts");
    assert!(format!("{contacts_rpc:?}").contains("90000002"));
    let contacts_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/characters/{P1}/contacts/"),
        Some(&tok1),
        None,
    );
    assert!(contacts_esi.to_string().contains("90000002"));

    // 2. Mail labels & deleted mail
    let labels_rpc = rpc(ctx, s1, "mailMgr", "GetLabels", &[])
        .await
        .expect("mailMgr.GetLabels");
    assert!(format!("{labels_rpc:?}").contains("PriorityOps"));
    let labels_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/characters/{P1}/mail/labels/"),
        Some(&tok1),
        None,
    );
    assert!(labels_esi["labels"]
        .as_array()
        .expect("labels array")
        .iter()
        .any(|l| l["label_id"].as_i64() == Some(mail_label_id) && l["name"] == "PriorityOps"));
    let labels_quasar = quasar_rpc(
        ctx,
        s1,
        "eve.contact.label.GetForCharacterRequest",
        vec![],
    )
    .await;
    assert_eq!(labels_quasar.status_code, STATUS_OK);
    let labels_bytes = labels_quasar.payload.expect("labels payload").value;
    assert!(!String::from_utf8_lossy(&labels_bytes).contains("PriorityOps"));
    let headers_rpc = rpc(ctx, s1, "mailMgr", "GetMailHeaders", &[])
        .await
        .expect("GetMailHeaders");
    let hdr_dbg = format!("{headers_rpc:?}");
    assert!(!hdr_dbg.contains(&format!("Integer({mail_deleted})")));
    assert!(hdr_dbg.contains(&format!("Integer({mail_kept})")));
    let mail_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/characters/{P1}/mail/"),
        Some(&tok1),
        None,
    );
    let mail_str = mail_esi.to_string();
    assert!(!mail_str.contains("Purged Dispatch") && mail_str.contains("Retained Dispatch"));

    // 3. Custom item names
    let decoded_item = rpc(
        ctx,
        s1,
        "encodedItems",
        "DecodeItem",
        &[PyRep::Integer(ship_id)],
    )
    .await
    .expect("DecodeItem");
    assert_eq!(
        decoded_item.dict_get("customName").and_then(|v| v.as_str()),
        Some("B11 Sovereign Hull")
    );
    let names_esi = esi_call(
        &router,
        "POST",
        &format!("/latest/characters/{P1}/assets/names/"),
        Some(&tok1),
        Some(serde_json::to_vec(&vec![ship_id]).expect("json")),
    );
    assert_eq!(names_esi[0]["name"], "B11 Sovereign Hull");

    // 4. CSPA charges
    let p1_cspa = rpc(ctx, s1, "account", "GetDefaultContactCost", &[])
        .await
        .expect("GetDefaultContactCost");
    assert_eq!(p1_cspa.as_f64(), Some(25_000.0));
    let cspa_esi = esi_call(
        &router,
        "POST",
        &format!("/latest/characters/{P1}/cspa/"),
        Some(&tok1),
        Some(serde_json::to_vec(&vec![P2]).expect("json")),
    );
    assert_eq!(cspa_esi.as_f64(), Some(12_500.0));
    let cspa_q_p1 = quasar_rpc(ctx, s1, "eve.mail.cspa.GetRequest", vec![]).await;
    assert_eq!(cspa_q_p1.status_code, STATUS_OK);
    let cspa_wm_p1 = eve_proto::quasar::WireMessage::decode(&cspa_q_p1.payload.expect("cspa p1").value).expect("wm");
    let cspa_inner_p1 = eve_proto::quasar::WireMessage::decode(
        cspa_wm_p1.fields[0].value.as_bytes().expect("charge bytes"),
    )
    .expect("inner wm");
    assert_eq!(cspa_inner_p1.fields[0].value.as_varint(), Some(25_000));
    let (mut p2_id, mut p2_cspa_req) = (Vec::new(), Vec::new());
    encode_varint_field(1, P2 as u64, &mut p2_id);
    encode_bytes_field(1, &p2_id, &mut p2_cspa_req);
    let cspa_q_p2 = quasar_rpc(ctx, s1, "eve.mail.cspa.GetRequest", p2_cspa_req).await;
    assert_eq!(cspa_q_p2.status_code, STATUS_OK);
    let cspa_wm_p2 = eve_proto::quasar::WireMessage::decode(&cspa_q_p2.payload.expect("cspa p2").value).expect("wm");
    let cspa_inner_p2 = eve_proto::quasar::WireMessage::decode(
        cspa_wm_p2.fields[0].value.as_bytes().expect("charge bytes"),
    )
    .expect("inner wm");
    assert_eq!(cspa_inner_p2.fields[0].value.as_varint(), Some(12_500));

    // 5. War records
    let wars_rpc = rpc(ctx, s1, "warRegistry", "GetWars", &[PyRep::Integer(corp_id)])
        .await
        .expect("warRegistry.GetWars");
    assert!(format!("{wars_rpc:?}").contains(&war_id.to_string()));
    let war_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/wars/{war_id}/"),
        None,
        None,
    );
    assert_eq!(war_esi["aggressor"]["corporation_id"], corp_id);
    assert_eq!(war_esi["defender"]["corporation_id"], rival_corp_id);
    let reg = QuasarDescriptorRegistry::open_default().expect("quasar registry");
    let wars_all_q = quasar_rpc(ctx, s1, "eve.war.GetAllRequest", vec![]).await;
    assert_eq!(wars_all_q.status_code, STATUS_OK);
    let wars_all_dyn = reg
        .decode_dynamic("eve.war.GetAllResponse", &wars_all_q.payload.expect("wars all").value)
        .expect("decode GetAllResponse");
    assert!(format!("{wars_all_dyn:?}").contains(&war_id.to_string()));
    let (mut war_id_b, mut war_get_req) = (Vec::new(), Vec::new());
    encode_varint_field(1, war_id as u64, &mut war_id_b);
    encode_bytes_field(1, &war_id_b, &mut war_get_req);
    let war_get_q = quasar_rpc(ctx, s1, "eve.war.GetRequest", war_get_req).await;
    assert_eq!(war_get_q.status_code, STATUS_OK);
    let war_wm = eve_proto::quasar::WireMessage::decode(&war_get_q.payload.expect("war get").value).expect("war wm");
    let war_attr = eve_proto::quasar::WireMessage::decode(war_wm.fields[0].value.as_bytes().expect("attr")).expect("attr wm");
    let agg_wm = eve_proto::quasar::WireMessage::decode(war_attr.fields[0].value.as_bytes().expect("agg")).expect("agg wm");
    let def_wm = eve_proto::quasar::WireMessage::decode(war_attr.fields[1].value.as_bytes().expect("def")).expect("def wm");
    assert_eq!(agg_wm.fields[0].value.as_varint(), Some(corp_id as u64));
    assert_eq!(def_wm.fields[0].value.as_varint(), Some(rival_corp_id as u64));

    // 6. Alliances
    let ally_rpc = rpc(
        ctx,
        s1,
        "allianceRegistry",
        "GetAlliance",
        &[PyRep::Integer(alliance_id)],
    )
    .await
    .expect("allianceRegistry.GetAlliance");
    assert!(format!("{ally_rpc:?}").contains("B11UA"));
    let ally_esi = esi_call(
        &router,
        "GET",
        &format!("/latest/alliances/{alliance_id}/"),
        None,
        None,
    );
    assert_eq!(ally_esi["ticker"], "B11UA");
    let mut char_seq = Vec::new();
    encode_varint_field(1, P1 as u64, &mut char_seq);
    let mut pres_req = Vec::new();
    encode_bytes_field(1, &char_seq, &mut pres_req);
    let pres_rsp = quasar_rpc(
        ctx,
        s1,
        "eve_public.presence.api.GetAllianceRequest",
        pres_req,
    )
    .await;
    assert_eq!(pres_rsp.status_code, STATUS_OK);
    let pres_dyn = reg
        .decode_dynamic(
            "eve_public.presence.api.GetAllianceResponse",
            &pres_rsp.payload.expect("presence payload").value,
        )
        .expect("decode GetAllianceResponse");
    assert!(format!("{pres_dyn:?}").contains(&alliance_id.to_string()));

    // 7. PLEX balances
    let plex_client = rpc(ctx, s1, "vaultManager", "get_account_balance", &[])
        .await
        .expect("vaultManager.get_account_balance")
        .as_i64()
        .expect("plex i64");
    assert_eq!(plex_client, 620);
    let plex_quasar = quasar_rpc(ctx, s1, "eve_public.plex.vault.api.BalanceRequest", vec![]).await;
    assert_eq!(plex_quasar.status_code, STATUS_OK);
    let bal_dyn = reg
        .decode_dynamic(
            "eve_public.plex.vault.api.BalanceResponse",
            &plex_quasar.payload.expect("balance payload").value,
        )
        .expect("decode BalanceResponse");
    assert!(format!("{bal_dyn:?}").contains("62000"));
}

#[tokio::test]
async fn test_b11_quasar_social_fail_closed_and_ssot_guards() {
    let ctx = ServerContext::new();
    let mut anon = SessionState::new(72_099);
    assert_eq!(
        quasar_rpc(&ctx, &mut anon, "eve.mail.cspa.GetRequest", vec![]).await.status_code,
        401
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut anon, "eve.mail.CCPSendMailRequest", vec![]).await.status_code,
        401
    );

    let mut s1 = select_char(&ctx, 72_098, "demo", "demo-secret", P1).await;
    assert_eq!(
        quasar_rpc(&ctx, &mut s1, "eve.mail.CCPSendMailRequest", vec![]).await.status_code,
        403
    );

    let mut id_1001 = Vec::new();
    encode_varint_field(1, 1001, &mut id_1001);
    let mut war_1001 = Vec::new();
    eve_proto::quasar::encode_bytes_field(1, &id_1001, &mut war_1001);
    assert_eq!(quasar_rpc(&ctx, &mut s1, "eve.war.GetRequest", war_1001.clone()).await.status_code, 404);
    assert_eq!(quasar_rpc(&ctx, &mut s1, "eve.war.GetAlliesRequest", war_1001).await.status_code, 404);

    let empty_labels = quasar_rpc(&ctx, &mut s1, "eve.contact.label.GetForCharacterRequest", vec![]).await;
    assert_eq!(empty_labels.status_code, STATUS_OK);
    assert!(empty_labels.payload.expect("payload").value.is_empty());

    let mut id_p2 = Vec::new();
    encode_varint_field(1, P2 as u64, &mut id_p2);
    let mut other_char = Vec::new();
    eve_proto::quasar::encode_bytes_field(1, &id_p2, &mut other_char);
    assert_eq!(
        quasar_rpc(&ctx, &mut s1, "eve.contact.label.GetForCharacterRequest", other_char).await.status_code,
        403
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_b11_multithread_wal_error_survives_await_and_rolls_back() {
    let db_path = std::env::temp_dir().join(format!("b11_mt_wal_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let mem_ctx = ServerContext::new();
    mem_ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
    let ctx = ServerContext::from_sqlite(&db_path).expect("from_sqlite");
    let mut sess = select_char(&ctx, 72_049, "demo", "demo-secret", P1).await;
    let before = crate::open_feats_and_rpcs_b11_part2::capture_observable_state(&ctx);
    let conn = rusqlite::Connection::open(&db_path).expect("open sqlite");
    conn.execute_batch(
        "CREATE TRIGGER abort_char_upd BEFORE UPDATE ON characters BEGIN SELECT RAISE(ABORT, 'injected_mt_wal_fail'); END;",
    )
    .expect("trigger");
    drop(conn);
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "account",
            None,
            "GiveCash",
            &[PyRep::Integer(P2), PyRep::Real(100.0), PyRep::String("mt-test".into())],
            &std::collections::HashMap::new(),
            None,
        )
        .await
        .expect_err("must fail on WAL abort");
    assert!(
        err.code == "DatabasePersistenceError" || err.code == "PersistenceFailure",
        "unexpected error code: {}",
        err.code
    );
    assert!(err.message.contains("injected_mt_wal_fail"));
    tokio::task::yield_now().await;
    assert_eq!(crate::open_feats_and_rpcs_b11_part2::capture_observable_state(&ctx), before);
    let _ = std::fs::remove_file(&db_path);
}
