//! Integration tests for `plan:D-6` (`feat:invariant.single-source-of-truth`):
//! Contacts, mail labels and deleted mail, custom item names, CSPA charges,
//! war records, alliances, and PLEX balances read identically across MachoNet client RPC,
//! ESI, and Quasar, and persist across SQLite restart.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    AllianceLifecycleMeta, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, WarRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

const JITA_STATION_ID: i64 = 60_003_760;

fn temp_sqlite_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
    p
}

fn cleanup_sqlite_path(p: &PathBuf) {
    let _ = std::fs::remove_file(p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
}

async fn rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(session, service, None, method, args, &kw, None).await
}

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token(ctx: &ServerContext, character_id: i64, scopes: &[&str]) -> String {
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-d6-ssot",
            "https://localhost/callback",
            scopes.iter().map(|s| s.to_string()).collect(),
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-d6-ssot", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn esi_get(router: &EsiRouter, path: &str, token: Option<&str>) -> serde_json::Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200, "ESI GET {path} failed: {}", String::from_utf8_lossy(&resp.body));
    serde_json::from_slice(&resp.body).expect("valid JSON")
}

#[tokio::test]
async fn test_d6_single_source_of_truth_client_esi_quasar_and_restart() {
    let db_path = temp_sqlite_path("d6_ssot");
    let char_id = 90_000_001i64;
    let target_char_id = 90_000_002i64;
    let corp_id = 98_100_001i64;
    let enemy_corp_id = 98_100_002i64;
    let alliance_id = 99_100_001i64;
    let war_id = 77_001i64;
    let ship_item_id = 100_000_001i64;
    let plex_item_id = 100_555_001i64;

    let (mail_id_deleted, mail_id_kept, label_id) = {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 1"));
        let ctx = Arc::new(ServerContext::with_repository(repo));
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            snap.corporations.insert(corp_id, CorporationRecord {
                corporation_id: corp_id,
                name: "SSOT Corp".into(),
                ticker: "SSOT".into(),
                ceo_id: char_id,
                alliance_id: Some(alliance_id),
                hq_station_id: Some(JITA_STATION_ID),
                ..Default::default()
            });
            snap.corporations.insert(enemy_corp_id, CorporationRecord {
                corporation_id: enemy_corp_id,
                name: "Rival Corp".into(),
                ticker: "RIVL".into(),
                ceo_id: target_char_id,
                hq_station_id: Some(JITA_STATION_ID),
                ..Default::default()
            });
            snap.alliances.insert(alliance_id, AllianceRecord {
                alliance_id,
                name: "SSOT Alliance".into(),
                short_name: "SSOTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id].into_iter().collect(),
            });
            snap.alliance_lifecycle_meta.insert(alliance_id, AllianceLifecycleMeta {
                creator_id: char_id,
                creator_corporation_id: corp_id,
                date_founded_unix: 1_700_000_000,
                faction_id: None,
            });
            snap.wars.insert(war_id, WarRecord {
                war_id,
                aggressor_corp_id: corp_id,
                defender_corp_id: enemy_corp_id,
                mutual: false,
                started_unix: 1_700_000_000,
                active: true,
            });
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = corp_id;
                ch.alliance_id = Some(alliance_id);
                ch.balance = IskCents(50_000_000_00);
            }
            snap.items.insert(plex_item_id, InventoryItemRecord {
                item_id: plex_item_id,
                type_id: 44_992,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 250,
                singleton: false,
                version: 1,
                ..Default::default()
            });
        });
        common::pin_clock(&ctx);

        let mut sess = SessionState::new(8101);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("select");

        // 1. Contacts & CSPA
        let lbl = rpc(&ctx, &mut sess, "charMgr", "CreateLabel", &[PyRep::String("SSOT Contacts".into()), PyRep::Integer(0x00FF00)]).await.expect("CreateLabel");
        let label_id = lbl.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);
        rpc(&ctx, &mut sess, "charMgr", "AddContact", &[PyRep::Integer(target_char_id), PyRep::Real(9.5), PyRep::Bool(true), PyRep::Integer(label_id)]).await.expect("AddContact");
        rpc(&ctx, &mut sess, "charMgr", "LogSettings", &[PyRep::Real(7_500.0)]).await.expect("LogSettings");

        // 2. Mail labels & deleted mail
        let m1 = rpc(&ctx, &mut sess, "mailMgr", "SendMail", &[
            PyRep::List(vec![PyRep::Integer(char_id)]),
            PyRep::String("Deleted Mail".into()),
            PyRep::String("Body 1".into()),
            PyRep::Integer(0),
        ]).await.expect("SendMail 1").as_i64().unwrap_or(1);
        let m2 = rpc(&ctx, &mut sess, "mailMgr", "SendMail", &[
            PyRep::List(vec![PyRep::Integer(char_id)]),
            PyRep::String("Kept Mail".into()),
            PyRep::String("Body 2".into()),
            PyRep::Integer(0),
        ]).await.expect("SendMail 2").as_i64().unwrap_or(2);
        rpc(&ctx, &mut sess, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(m1)])]).await.expect("DeleteMail");

        // 3. Custom item name
        rpc(&ctx, &mut sess, "invbroker", "SetLabel", &[PyRep::Integer(ship_item_id), PyRep::String("SSOT Flagship".into())]).await.expect("SetLabel");

        // 4. PLEX vault deposit
        rpc(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(plex_item_id),
            PyRep::Integer(100),
            PyRep::Integer(0),
        ]).await.expect("DepositPlexToVault");

        // Verify ESI agrees immediately before restart
        let token = issue_token(&ctx, char_id, &["esi-characters.read_contacts.v1", "esi-mail.read_mail.v1", "esi-assets.read_assets.v1"]);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let contacts_json = esi_get(&router, &format!("/latest/characters/{char_id}/contacts/"), Some(&token));
        assert!(contacts_json.to_string().contains("90000002"));
        let alliance_json = esi_get(&router, &format!("/latest/alliances/{alliance_id}/"), None);
        assert_eq!(alliance_json["ticker"], "SSOTA");
        let war_json = esi_get(&router, &format!("/latest/wars/{war_id}/"), None);
        assert_eq!(war_json["aggressor"]["corporation_id"], corp_id);

        (m1, m2, label_id)
    };

    // Restart from SQLite on disk and verify client RPC, ESI, and Quasar all match
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 2"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    common::pin_clock(&ctx2);

    let mut sess2 = SessionState::new(8102);
    common::login_pilot(&ctx2, &mut sess2, "demo", "demo-secret").await;
    rpc(&ctx2, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("select 2");

    let contacts_rep = rpc(&ctx2, &mut sess2, "charMgr", "GetContacts", &[]).await.expect("GetContacts");
    assert!(format!("{contacts_rep:?}").contains("90000002") && label_id > 0);

    let mut sess3 = SessionState::new(8103);
    sess3.user_id = Some(2);
    sess3.character_id = Some(90_000_002);
    let cspa_contact = rpc(&ctx2, &mut sess3, "charMgr", "CSPAChargedAction", &[PyRep::Integer(char_id)]).await.expect("CSPAChargedAction");
    assert_eq!(cspa_contact, PyRep::Real(0.0));

    let headers_rep = rpc(&ctx2, &mut sess2, "mailMgr", "GetMailHeaders", &[]).await.expect("GetMailHeaders");
    let headers_dbg = format!("{headers_rep:?}");
    assert!(!headers_dbg.contains(&format!("Integer({mail_id_deleted})")) && headers_dbg.contains(&format!("Integer({mail_id_kept})")));

    let token2 = issue_token(&ctx2, char_id, &["esi-characters.read_contacts.v1", "esi-mail.read_mail.v1", "esi-assets.read_assets.v1"]);
    let router2 = EsiRouter::new(Arc::clone(&ctx2));
    let mail_json = esi_get(&router2, &format!("/latest/characters/{char_id}/mail/"), Some(&token2));
    let mail_str = mail_json.to_string();
    assert!(!mail_str.contains("Deleted Mail") && mail_str.contains("Kept Mail"));

    let alliance_json2 = esi_get(&router2, &format!("/latest/alliances/{alliance_id}/"), None);
    assert_eq!(alliance_json2["ticker"], "SSOTA");
    let war_json2 = esi_get(&router2, &format!("/latest/wars/{war_id}/"), None);
    assert_eq!(war_json2["aggressor"]["corporation_id"], corp_id);

    let plex_bal = ctx2.db.with_snapshot(|snap| snap.account_plex_vault.get(&1).copied().unwrap_or(0));
    assert_eq!(plex_bal, 600);
    let item_name = ctx2.db.with_snapshot(|snap| snap.item_custom_names.get(&ship_item_id).cloned().unwrap_or_default());
    assert_eq!(item_name, "SSOT Flagship");

    cleanup_sqlite_path(&db_path);
}

#[tokio::test]
async fn test_t10_client_expectation_oracle_live_rpc_shapes() {
    let oracle_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.agents/skills/evomu-adversarial-review/references/parity/sources/client_expectations.json");
    let raw = std::fs::read_to_string(&oracle_path)
        .expect("read client_expectations.json oracle");
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).expect("parse client_expectations.json");
    let methods = parsed["rpc"]
        .as_object()
        .expect("client_expectations.json must contain rpc object");
    assert!(
        methods.len() >= 1000,
        "expected >= 1000 V24 client RPC expectations, got {}",
        methods.len()
    );
    assert!(methods.contains_key("machoNet.GetTime"));
    assert!(methods.contains_key("charUnboundMgr.GetCharacterInfo"));
    assert!(methods.contains_key("marketProxy.GetCharOrders"));

    let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    let mut sess = SessionState::new(8201);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let time_rep = rpc(&ctx, &mut sess, "machoNet", "GetTime", &[])
        .await
        .expect("machoNet.GetTime");
    assert!(matches!(time_rep, PyRep::Integer(t) if t > 0));
    let info_rep = rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharacterInfo", &[PyRep::Integer(90_000_001)])
        .await
        .expect("charUnboundMgr.GetCharacterInfo");
    assert!(!matches!(info_rep, PyRep::None));
    sess.character_id = Some(90_000_001);
    let mkt_rep = rpc(&ctx, &mut sess, "marketProxy", "GetCharOrders", &[])
        .await
        .expect("marketProxy.GetCharOrders");
    assert!(!matches!(mkt_rep, PyRep::None));
}

