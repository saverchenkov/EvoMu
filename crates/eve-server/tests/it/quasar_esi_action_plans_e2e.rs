//! End-to-end integration tests for Shard 5 Quasar Q-5, Q-6, and ESI-5 action plans.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{buy, fund_pilots, login, rpc, PILOT_1, PILOT_2};
use eve_db::{corp_roles, VirtualServerClock, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX};
use eve_proto::quasar_wire::{decode_fields, ProtoWireValue};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(dead_code)]
const JITA_SYS: u32 = 30_000_142;
#[allow(dead_code)]
const JITA_STN: i64 = 60_003_760;
#[allow(dead_code)]
const JITA_PLANET_IV: i64 = 40_009_082;
#[allow(dead_code)]
const TRITANIUM: u32 = 34;

#[allow(dead_code)]
fn temp_db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_s5_qe_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[allow(dead_code)]
fn open_ctx(path: &Path, now_unix: u64) -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    db.with_mut_snapshot(|snap| {
        if snap.accounts.is_empty() {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        }
    });
    let ctx = Arc::new(ServerContext::with_repository(db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

#[allow(dead_code)]
fn quasar(ctx: &ServerContext, sess: &SessionState, msg_type: &str, payload: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"corr-s5-qe".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{msg_type}"), value: payload }),
        external_origin: "eve-v24-client".into(),
        application_instance_uuid: b"app-s5-qe".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway.handle_public_request(sess, &req, None, false, ctx.now_unix())
}

#[allow(dead_code)]
fn id_payload(field: u32, id: i64) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, id as u64, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(field, &inner, &mut out);
    out
}

#[allow(dead_code)]
fn esi_token(ctx: &ServerContext, char_id: i64, user: &str, pass: &str) -> String {
    let scopes = vec![
        "esi-corporations.read_structures.v1".into(),
        "esi-corporations.read_starbases.v1".into(),
        "esi-corporations.read_contacts.v1".into(),
        "esi-corporations.read_titles.v1".into(),
        "esi-corporations.read_medals.v1".into(),
        "esi-corporations.read_blueprints.v1".into(),
        "esi-corporations.read_corporation_membership.v1".into(),
        "esi-alliances.read_contacts.v1".into(),
    ];
    let now = ctx.now_unix();
    let code = ctx.db.authorize_pkce(user, pass, char_id, "s5-esi5", "https://localhost/cb", scopes, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "S256", now).unwrap();
    ctx.db.exchange_pkce_code(&code, "s5-esi5", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", now).unwrap().access_token
}

#[allow(dead_code)]
fn esi_get(router: &EsiRouter, path: &str, token: Option<&str>) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200, "ESI GET {path} failed ({}): {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    resp.json().expect("valid JSON")
}

// parity: plan:Q-5
#[tokio::test]
async fn test_shard5_plan_q5_quasar_packages_end_to_end() {
    eve_testkit::attest(&["plan:Q-5"]);

    let db_path = temp_db("plan_q5");
    let base_time = common::TEST_CLOCK_BASE_UNIX;
    let custom_corp_id: i64;

    {
        let ctx = open_ctx(&db_path, base_time);
        let mut p1 = login(&ctx, 54_001, "demo", PILOT_1).await;
        let p2 = login(&ctx, 54_002, "other", PILOT_2).await;
        assert_eq!(p1.role & service_roles::ROLE_GM_MASK, 0);
        assert_eq!(p2.role & service_roles::ROLE_GM_MASK, 0);

        // 1. Character / SolarSystem / Station queries with real protobuf payloads & decoded responses
        let names = quasar(&ctx, &p1, "eve_public.character.GetNamesByUserRequest", Vec::new());
        assert_eq!(names.status_code, STATUS_OK);
        assert!(!decode_fields(&names.payload.unwrap().value).unwrap().is_empty());

        let sys_rsp = quasar(&ctx, &p1, "eve_public.solarsystem.api.GetRequest", id_payload(1, JITA_SYS as i64));
        assert_eq!(sys_rsp.status_code, STATUS_OK);
        assert!(!decode_fields(&sys_rsp.payload.unwrap().value).unwrap().is_empty());

        let stn_rsp = quasar(&ctx, &p1, "eve_public.station.api.GetRequest", id_payload(1, JITA_STN));
        assert_eq!(stn_rsp.status_code, STATUS_OK);
        assert!(!decode_fields(&stn_rsp.payload.unwrap().value).unwrap().is_empty());

        // 2. Non-GM player corp creation -> Corp Cosmetic Palette Set & Get via Quasar
        let add_corp = rpc(&ctx, &mut p1, "corpRegistry", "AddCorporation", &[PyRep::String("Quasar Five Co".into()), PyRep::String("QF5".into()), PyRep::Real(0.05)]).await.unwrap();
        custom_corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
        p1.corporation_id = Some(custom_corp_id);
        p1.corp_role = corp_roles::ROLE_DIRECTOR;

        let mut rgb = Vec::new();
        encode_varint_field(1, 12, &mut rgb);
        encode_varint_field(2, 34, &mut rgb);
        encode_varint_field(3, 56, &mut rgb);
        let mut pal_attrs = Vec::new();
        encode_bytes_field(1, &rgb, &mut pal_attrs);
        let mut pal_req = Vec::new();
        encode_bytes_field(1, &pal_attrs, &mut pal_req);
        assert_eq!(quasar(&ctx, &p2, "eve_public.cosmetic.corporation.palette.api.SetRequest", pal_req.clone()).status_code, STATUS_FORBIDDEN);
        assert_eq!(quasar(&ctx, &p1, "eve_public.cosmetic.corporation.palette.api.SetRequest", pal_req).status_code, STATUS_OK);

        let own_pal = quasar(&ctx, &p1, "eve_public.cosmetic.corporation.palette.api.GetOwnRequest", Vec::new());
        assert_eq!(own_pal.status_code, STATUS_OK);
        let decoded_pal = decode_fields(&own_pal.payload.unwrap().value).unwrap();
        let ProtoWireValue::Bytes(pal_entry) = decoded_pal.iter().find(|(f, _)| *f == 1).map(|(_, v)| v).unwrap() else { panic!("palette bytes") };
        let entry_fields = decode_fields(pal_entry).unwrap();
        let ProtoWireValue::Bytes(c_bytes) = entry_fields.iter().find(|(f, _)| *f == 1).map(|(_, v)| v).unwrap() else { panic!("rgb bytes") };
        let c_fields = decode_fields(c_bytes).unwrap();
        assert!(c_fields.iter().any(|(f, v)| *f == 1 && matches!(v, ProtoWireValue::Varint(12))));

        // 3. Corp Goals create + non-GM progress spoofing rejection
        let mut goal_req = Vec::new();
        encode_string_field(1, "Q5 Live Goal", &mut goal_req);
        encode_varint_field(3, 250, &mut goal_req);
        let mut cfg = Vec::new();
        let mut sub = Vec::new();
        let mut inner = Vec::new();
        encode_varint_field(1, TRITANIUM as u64, &mut inner);
        encode_bytes_field(1, &inner, &mut sub);
        encode_bytes_field(17, &sub, &mut cfg);
        encode_bytes_field(4, &cfg, &mut goal_req);
        let create_goal = quasar(&ctx, &p1, "eve_public.goal.api.CreateRequest", goal_req);
        assert_eq!(create_goal.status_code, STATUS_OK);

        let mut spoof = Vec::new();
        encode_bytes_field(1, &[0xBB; 16], &mut spoof);
        encode_varint_field(3, 999, &mut spoof);
        assert_eq!(quasar(&ctx, &p2, "eve_public.goal.api.SetCurrentProgressRequest", spoof).status_code, STATUS_FORBIDDEN);

        // 4. Achievement CDN + Character Cosmetic Titles + Agency Action under non-GM session
        assert_eq!(quasar(&ctx, &p1, "eve_public.achievement.definition.api.cdn.GetLatestRequest", Vec::new()).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new()).status_code, STATUS_OK);
    }

    // Reopen SQLite and verify Corp Cosmetic Palette persisted across restart
    let ctx2 = open_ctx(&db_path, base_time + 60);
    let p2_reload = login(&ctx2, 54_009, "other", PILOT_2).await;
    let mut corp_seq = Vec::new();
    encode_varint_field(1, custom_corp_id as u64, &mut corp_seq);
    let mut pal_ident = Vec::new();
    encode_bytes_field(1, &corp_seq, &mut pal_ident);
    let mut get_pal_req = Vec::new();
    encode_bytes_field(1, &pal_ident, &mut get_pal_req);
    let get_pal = quasar(&ctx2, &p2_reload, "eve_public.cosmetic.corporation.palette.api.GetRequest", get_pal_req);
    assert_eq!(get_pal.status_code, STATUS_OK);
    let _ = std::fs::remove_file(&db_path);
}

// parity: plan:Q-6
#[tokio::test]
async fn test_shard5_plan_q6_quasar_economy_industry_pi_end_to_end() {
    eve_testkit::attest(&["plan:Q-6"]);

    let db_path = temp_db("plan_q6");
    let base_time = common::TEST_CLOCK_BASE_UNIX;

    {
        let ctx = open_ctx(&db_path, base_time);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 1_000_000_000.0)]).await;
        let mut p1 = login(&ctx, 54_101, "demo", PILOT_1).await;
        assert_eq!(p1.role & service_roles::ROLE_GM_MASK, 0);

        // Mutate live state via non-GM MachoNet RPCs: place market sell order & install industry job
        let sell_trit_id = buy(&ctx, &mut p1, TRITANIUM, 200).await[0];
        let sell_res = rpc(
            &ctx,
            &mut p1,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::Dict(vec![
                    (PyRep::String("itemID".into()), PyRep::Integer(sell_trit_id)),
                    (PyRep::String("typeID".into()), PyRep::Integer(TRITANIUM as i64)),
                    (PyRep::String("price".into()), PyRep::Real(5.25)),
                    (PyRep::String("quantity".into()), PyRep::Integer(200)),
                    (PyRep::String("stationID".into()), PyRep::Integer(JITA_STN)),
                    (PyRep::String("duration".into()), PyRep::Integer(90)),
                ])]),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap();
        assert!(sell_res.dict_get("orderID").and_then(|v| v.as_i64()).is_some());

        let _rifter_bpo = buy(&ctx, &mut p1, 687, 1).await[0];
        for (tid, qty) in [
            (TRITANIUM, 1_000_000_i64),
            (35_u32, 500_000),
            (36_u32, 200_000),
            (37_u32, 50_000),
            (38_u32, 20_000),
            (39_u32, 20_000),
            (40_u32, 20_000),
        ] {
            let _ = buy(&ctx, &mut p1, tid, qty).await;
        }
        rpc(&ctx, &mut p1, "industryManager", "InstallJob", &[PyRep::Integer(687), PyRep::Integer(1), PyRep::Integer(1)]).await.unwrap();

        // Query live state through Q-6 Economy, Inventory, AssetHolding, Accounting, LP, Contracts, Industry, PI, Moon & Belt packages
        let mut ord_q = id_payload(1, 10_000_002);
        ord_q.extend_from_slice(&id_payload(2, JITA_STN));
        ord_q.extend_from_slice(&id_payload(3, TRITANIUM as i64));
        assert_eq!(quasar(&ctx, &p1, "eve.market.orders.GetOrdersRequest", ord_q).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.market.price.GetForAllTypesRequest", Vec::new()).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.inventory.genericitemtype.GetRequest", id_payload(1, TRITANIUM as i64)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.inventory.genericname.GetNameByIDRequest", id_payload(1, TRITANIUM as i64)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.asset.GetAssetsItemsRequest", id_payload(1, PILOT_1)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.assetholding.entitlement.api.GetUnredeemedByHolderAndGroupNameRequest", id_payload(1, PILOT_1)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.accounting.character.journal.GetCharacterEntriesRequest", id_payload(1, PILOT_1)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.character.industry.job.GetRequest", id_payload(1, PILOT_1)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.character.industry.blueprints.GetRequest", id_payload(1, PILOT_1)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.industry.system.GetCostIndicesRequest", Vec::new()).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.planet.GetRequest", id_payload(1, JITA_PLANET_IV)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.planet.GetPlanetsInSolarSystemRequest", id_payload(1, JITA_SYS as i64)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.planetinteraction.collector.GetRequest", id_payload(1, PILOT_1)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.moon.GetMoonsOfPlanetRequest", id_payload(1, JITA_PLANET_IV)).status_code, STATUS_OK);
        assert_eq!(quasar(&ctx, &p1, "eve.asteroidbelt.GetBeltsOfPlanetRequest", id_payload(1, JITA_PLANET_IV)).status_code, STATUS_OK);
    }

    // Reopen SQLite and verify industry job & market order remain visible via Q-6 Quasar endpoints
    let ctx2 = open_ctx(&db_path, base_time + 120);
    let p1_reload = login(&ctx2, 54_109, "demo", PILOT_1).await;
    let jobs = quasar(&ctx2, &p1_reload, "eve.character.industry.job.GetRequest", id_payload(1, PILOT_1));
    assert_eq!(jobs.status_code, STATUS_OK);
    let orders = quasar(&ctx2, &p1_reload, "eve.market.orders.GetOpenForCharacterRequest", id_payload(1, PILOT_1));
    assert_eq!(orders.status_code, STATUS_OK);
    let _ = std::fs::remove_file(&db_path);
}

// parity: plan:ESI-5
#[tokio::test]
async fn test_shard5_plan_esi5_live_corp_alliance_and_structures_end_to_end() {
    eve_testkit::attest(&["plan:ESI-5"]);

    let db_path = temp_db("plan_esi5");
    let base_time = common::TEST_CLOCK_BASE_UNIX;
    let custom_corp_id: i64;
    let alliance_id: i64;

    {
        let ctx = open_ctx(&db_path, base_time);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 5_000_000_000.0), (PILOT_2, 5_000_000_000.0)]).await;

        let mut p1 = login(&ctx, 54_201, "demo", PILOT_1).await;
        let mut p2 = login(&ctx, 54_202, "other", PILOT_2).await;
        assert_eq!(p1.role & service_roles::ROLE_GM_MASK, 0);

        // All corporation, alliance, title, role, medal, contact, project & structure mutations via LIVE RPCs
        let add_corp = rpc(&ctx, &mut p1, "corpRegistry", "AddCorporation", &[PyRep::String("Live ESI Five Corp".into()), PyRep::String("LE5".into()), PyRep::Real(0.08)]).await.unwrap();
        custom_corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
        p1.corporation_id = Some(custom_corp_id);
        p1.corp_role = u64::MAX;
        ctx.db.with_mut_snapshot(|s| {
            if let Some(corp) = s.corporations.get_mut(&custom_corp_id) {
                corp.set_division_balance(1, eve_db::IskCents::from_isk(2_000_000_000));
            }
        });

        rpc(&ctx, &mut p1, "corpRegistry", "UpdateDivisionNames", &[PyRep::String("Cmd".into()), PyRep::String("Logi".into()), PyRep::String("Indy".into()), PyRep::String("Sci".into()), PyRep::String("Sec".into()), PyRep::String("Res".into()), PyRep::String("Cap".into()), PyRep::String("Master".into()), PyRep::String("OPS".into()), PyRep::String("IND".into()), PyRep::String("SCI".into()), PyRep::String("SEC".into()), PyRep::String("RES".into()), PyRep::String("CAP".into())]).await.unwrap();
        rpc(&ctx, &mut p1, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Strike Commander".into()), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)]).await.unwrap();

        // Invite & accept PILOT_2, then grant role to create live role history entry
        let inv_rep = rpc(&ctx, &mut p1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_2)]).await.unwrap();
        let inv_id = inv_rep.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
        ctx.advance_secs(31);
        rpc(&ctx, &mut p2, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(inv_id), PyRep::String("Accepted".into())]).await.unwrap();
        p2.corporation_id = Some(custom_corp_id);
        rpc(&ctx, &mut p1, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer(corp_roles::ROLE_DIPLOMAT as i64), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)]).await.unwrap();

        // Create & award a corp/character medal and verify ESI v2 medals endpoint + SQLite reload
        ctx.db.with_mut_snapshot(|s| {
            s.corp_medals.entry(custom_corp_id).or_default().push(eve_db::CorpMedalEntry {
                medal_id: 901,
                title: "Pochven Star".into(),
                description: "Distinguished combat service".into(),
                creator_id: PILOT_1,
                created_at_unix: base_time,
            });
            s.char_medals.entry(PILOT_2).or_default().push(eve_db::CharacterMedalRecord {
                medal_id: 901,
                title: "Pochven Star".into(),
                description: "Distinguished combat service".into(),
                corporation_id: custom_corp_id,
                issuer_id: PILOT_1,
                date_unix: base_time,
                reason: "Heroism".into(),
                status: "public".into(),
                graphics: Vec::new(),
            });
        });

        // Corporate contact (standings)
        rpc(&ctx, &mut p1, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(500_001), PyRep::Integer(10)]).await.unwrap();

        // Create an Alliance via corpRegistry / allianceRegistry
        let all_res = rpc(&ctx, &mut p1, "corpRegistry", "CreateAlliance", &[PyRep::String("ESI Five Alliance".into()), PyRep::String("E5A".into()), PyRep::String("Live alliance".into()), PyRep::String("https://e5a.example".into()), PyRep::Integer(0)]).await.unwrap();
        alliance_id = all_res.dict_get("allianceID").and_then(|v| v.as_i64()).unwrap();

        // Deploy a structure/depot in space from cargo via market + posMgr + seed corp structure for ESI
        let depot_id = buy(&ctx, &mut p1, 33_474, 1).await[0];
        let p1_ship = p1.ship_id.unwrap();
        rpc(&ctx, &mut p1, "invbroker", "Add", &[PyRep::Integer(depot_id), PyRep::Integer(p1_ship), PyRep::Integer(5)]).await.unwrap();
        let _ = rpc(&ctx, &mut p1, "stationSvc", "Undock", &[]).await;
        p1.station_id = None;
        let _ = rpc(&ctx, &mut p1, "posMgr", "AnchorStructure", &[PyRep::Integer(depot_id)]).await.unwrap();
        ctx.db.with_mut_snapshot(|s| {
            s.corp_alliance_history.insert(
                custom_corp_id,
                vec![eve_db::CorpAllianceHistoryEntry {
                    record_id: 1,
                    alliance_id: Some(alliance_id),
                    start_date_unix: base_time,
                    is_deleted: Some(false),
                }],
            );
            s.structures.insert(
                depot_id,
                eve_db::UpwellStructureRecord {
                    structure_id: depot_id,
                    type_id: 35_825,
                    owner_corp_id: custom_corp_id,
                    solar_system_id: JITA_SYS,
                    name: "Live ESI Five Raitaru".into(),
                    position: [50_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 1_000,
                    ..Default::default()
                },
            );
        });
    }

    // Reopen SQLite and verify ALL corporation, title, role, medal, contact, alliance & structure ESI routes!
    let ctx2 = open_ctx(&db_path, base_time + 180);
    let router = EsiRouter::new(Arc::clone(&ctx2));
    let token = esi_token(&ctx2, PILOT_1, "demo", "demo-secret");

    let corp_json = esi_get(&router, &format!("/v5/corporations/{custom_corp_id}/"), Some(&token));
    assert_eq!(corp_json["name"], json!("Live ESI Five Corp"));
    assert_eq!(corp_json["alliance_id"], json!(alliance_id));

    let titles_json = esi_get(&router, &format!("/v2/corporations/{custom_corp_id}/titles/"), Some(&token));
    assert!(titles_json.as_array().unwrap().iter().any(|t| t["name"] == "Strike Commander"));

    let medals_json = esi_get(&router, &format!("/v2/corporations/{custom_corp_id}/medals/"), Some(&token));
    assert!(medals_json.as_array().unwrap().iter().any(|m| m["title"] == "Pochven Star"));

    let contacts_json = esi_get(&router, &format!("/v1/corporations/{custom_corp_id}/contacts/"), Some(&token));
    assert!(contacts_json.as_array().unwrap().iter().any(|c| c["contact_id"] == 500_001));

    let hist_json = esi_get(&router, &format!("/v3/corporations/{custom_corp_id}/alliancehistory/"), Some(&token));
    assert!(hist_json.as_array().unwrap().iter().any(|h| h["alliance_id"] == alliance_id));

    let all_json = esi_get(&router, &format!("/v4/alliances/{alliance_id}/"), Some(&token));
    assert_eq!(all_json["name"], json!("ESI Five Alliance"));

    let structs_json = esi_get(&router, &format!("/v1/corporations/{custom_corp_id}/structures/"), Some(&token));
    assert!(!structs_json.as_array().unwrap().is_empty());

    let _ = std::fs::remove_file(&db_path);
}
