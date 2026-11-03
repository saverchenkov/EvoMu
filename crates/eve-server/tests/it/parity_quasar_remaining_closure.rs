//! End-to-end parity closure tests for `feat:ui.info-panels` and Quasar
//! `character`, `payment`, and `plex` domains.

#![forbid(unsafe_code)]

use crate::open_feats_and_rpcs_b11_part2::{quasar_rpc, rpc, select_char};
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;
use eve_db::InventoryFlag;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    QuasarDescriptorRegistry, WireMessage,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_payment_store_types::{
    read_store_catalog, save_store_catalog, PaymentScheduleRecord, PaymentTokenRecord, StoreOffer,
};
use tokio::sync::mpsc::Receiver;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA: i64 = 30_000_142;
const PERIMETER: i64 = 30_000_144;
const JITA_4_4: i64 = 60_003_760;

pub(crate) fn encode_double_field(field_number: u32, v: f64, out: &mut Vec<u8>) {
    out.push(((field_number as u8) << 3) | 1);
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn encode_seq_id(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    b
}

pub(crate) fn drain_notices(ctx: &ServerContext, rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let pending = {
        let mut p = ctx.quasar_gateway.pending_notices.write().unwrap();
        std::mem::take(&mut *p)
    };
    let now = ctx.now_unix();
    for (scope, payload) in pending {
        let _ = ctx.quasar_gateway.publish_notice(scope, payload, now);
    }
    let mut out = Vec::new();
    while let Ok(n) = rx.try_recv() {
        out.push(n);
    }
    out
}

#[tokio::test]
async fn test_ui_info_panels_all_eight_panels_e2e() {
    // parity: feat:ui.info-panels
    eve_testkit::attest(&["feat:ui.info-panels"]);

    let ctx = ServerContext::new();
    let mut s = select_char(&ctx, 81_001, "demo", "demo-secret", P1).await;
    let reg = QuasarDescriptorRegistry::open_default().expect("load descriptors");

    // 1. Route info panel: autopilot waypoints via charMgr + shortest jump count via Quasar
    let wps = PyRep::List(vec![PyRep::Integer(PERIMETER), PyRep::Integer(JITA)]);
    rpc(
        &ctx,
        &mut s,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("waypoints".into()), wps],
    )
    .await
    .expect("SaveCharacterSetting waypoints");
    let settings = rpc(&ctx, &mut s, "charMgr", "GetCharacterSettings", &[])
        .await
        .expect("GetCharacterSettings");
    assert!(
        format!("{settings:?}").contains("30000144"),
        "Route info panel waypoints must be persisted in charMgr settings"
    );

    let mut nav_req = Vec::new();
    encode_bytes_field(1, &encode_seq_id(JITA as u64), &mut nav_req);
    encode_bytes_field(2, &encode_seq_id(PERIMETER as u64), &mut nav_req);
    let nav_rsp = quasar_rpc(
        &ctx,
        &mut s,
        "eve.universe.navigation.GetShortestGateJumpCountRequest",
        nav_req,
    )
    .await;
    assert_eq!(nav_rsp.status_code, STATUS_OK);

    // 2. Location info panel: station + system sovereignty info
    let st_info = rpc(&ctx, &mut s, "map", "GetStationInfo", &[PyRep::Integer(JITA_4_4)])
        .await
        .expect("map.GetStationInfo");
    assert!(format!("{st_info:?}").contains("30000142"));
    let sys_sov = rpc(
        &ctx,
        &mut s,
        "sovMgr",
        "GetSystemSovereigntyInfo",
        &[PyRep::Integer(JITA)],
    )
    .await
    .expect("sovMgr.GetSystemSovereigntyInfo");
    assert!(!matches!(sys_sov, PyRep::Bool(false)));

    // 3. Mission tracker info panel: agentMgr.GetMyJournalDetails
    let journal = rpc(&ctx, &mut s, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(j_parts) = journal else {
        panic!("expected Tuple from GetMyJournalDetails");
    };
    assert_eq!(j_parts.len(), 2);

    // 4. Incursion info panel: taleMgr.GetGlobalWorldEventTales + map.GetIncursionGlobalReport
    let tales = rpc(&ctx, &mut s, "taleMgr", "GetGlobalWorldEventTales", &[])
        .await
        .expect("GetGlobalWorldEventTales");
    assert!(matches!(tales, PyRep::List(_)));
    let inc_report = rpc(&ctx, &mut s, "map", "GetIncursionGlobalReport", &[])
        .await
        .expect("GetIncursionGlobalReport");
    assert!(matches!(inc_report, PyRep::List(_)));
    let inc_systems = rpc(&ctx, &mut s, "map", "GetSystemsInIncursions", &[])
        .await
        .expect("GetSystemsInIncursions");
    assert!(matches!(inc_systems, PyRep::List(_) | PyRep::Dict(_)));

    // 5. FW info panel: map.GetFacWarZoneInfo + facWarMgr.GetStats_Character
    let fw_zone = rpc(&ctx, &mut s, "map", "GetFacWarZoneInfo", &[])
        .await
        .expect("GetFacWarZoneInfo");
    assert!(
        format!("{fw_zone:?}").contains("solarSystemID"),
        "GetFacWarZoneInfo must return warzone KeyVal with solarSystemID"
    );
    let fw_stats = rpc(&ctx, &mut s, "facWarMgr", "GetStats_Character", &[])
        .await
        .expect("GetStats_Character");
    assert!(matches!(fw_stats, PyRep::Dict(_)));

    // 6. Insurgency info panel: Quasar eve.pirate.insurgency.api.GetCurrentRequest & GetForWarzoneRequest
    let ins_cur = quasar_rpc(&ctx, &mut s, "eve.pirate.insurgency.api.GetCurrentRequest", vec![]).await;
    assert_eq!(ins_cur.status_code, STATUS_OK);
    let ins_dyn = reg
        .decode_dynamic(
            "eve.pirate.insurgency.api.GetCurrentResponse",
            &ins_cur.payload.expect("insurgency current").value,
        )
        .expect("decode GetCurrentResponse");
    assert!(format!("{ins_dyn:?}").contains("DynamicQuasarMessage"));
    let mut wz_req = Vec::new();
    encode_bytes_field(1, &encode_seq_id(1), &mut wz_req);
    let ins_wz = quasar_rpc(&ctx, &mut s, "eve.pirate.insurgency.api.GetForWarzoneRequest", wz_req).await;
    assert_eq!(ins_wz.status_code, STATUS_OK);

    // 7. ESS info panel: essMgr.GetDataForClientSolarSystem + dynamicBountyMgr.GetOutputForClientSolarSystem
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|sys| sys.security_status <= 0.0 && (30_000_001..=30_005_500).contains(&sys.solar_system_id))
        .expect("nullsec system")
        .solar_system_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default();
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.station_id = None;
            ch.solar_system_id = null_sys;
        }
    });
    s.station_id = None;
    s.solar_system_id = Some(null_sys);
    let ess_data = rpc(
        &ctx,
        &mut s,
        "essMgr",
        "GetDataForClientSolarSystem",
        &[PyRep::Integer(i64::from(null_sys))],
    )
    .await
    .expect("essMgr.GetDataForClientSolarSystem");
    assert!(!matches!(ess_data, PyRep::None));
    let dbs_out = rpc(&ctx, &mut s, "dynamicBountyMgr", "GetOutputForClientSolarSystem", &[])
        .await
        .expect("dynamicBountyMgr.GetOutputForClientSolarSystem");
    assert!(matches!(dbs_out, PyRep::Tuple(_)));

    // 8. Sov info panel: sovMgr.GetSovStructuresInfoForSolarSystem + map.GetCurrentSovData
    let sov_structs = rpc(
        &ctx,
        &mut s,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(JITA)],
    )
    .await
    .expect("GetSovStructuresInfoForSolarSystem");
    assert!(matches!(sov_structs, PyRep::List(_)));
    let sov_map = rpc(&ctx, &mut s, "map", "GetCurrentSovData", &[PyRep::Integer(10_000_002)])
        .await
        .expect("map.GetCurrentSovData");
    assert!(matches!(sov_map, PyRep::Dict(_) | PyRep::List(_)));
}

#[tokio::test]
async fn test_quasar_character_domain_e2e() {
    // parity: feat:quasar.character
    eve_testkit::attest(&["feat:quasar.character"]);

    let ctx = ServerContext::new();
    let mut p1 = select_char(&ctx, 81_002, "demo", "demo-secret", P1).await;

    // 1. GetNamesByUserRequest
    let reg = QuasarDescriptorRegistry::open_default().expect("load descriptors");
    let names_rsp = quasar_rpc(&ctx, &mut p1, "eve_public.character.GetNamesByUserRequest", vec![]).await;
    assert_eq!(names_rsp.status_code, STATUS_OK);
    let names_bytes = names_rsp.payload.expect("names").value;
    let names_dyn = reg
        .decode_dynamic("eve_public.character.GetNamesByUserResponse", &names_bytes)
        .expect("decode GetNamesByUserResponse");
    assert!(
        format!("{names_dyn:?}").contains("90000001"),
        "GetNamesByUserResponse must include character 90000001"
    );

    // 2. Skill plan CRUD + milestone + active plan
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.character.skill.plan.CreateRequest", vec![])
            .await
            .status_code,
        STATUS_BAD_REQUEST
    );
    let (mut attr_b, mut req_b, mut tid_b, mut create_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "Closure Gunnery Plan", &mut attr_b);
    encode_varint_field(1, 3300, &mut tid_b);
    encode_bytes_field(1, &tid_b, &mut req_b);
    encode_varint_field(2, 4, &mut req_b);
    encode_bytes_field(2, &req_b, &mut attr_b);
    encode_string_field(3, "Gunnery IV", &mut attr_b);
    encode_bytes_field(1, &attr_b, &mut create_b);

    let sp_create = quasar_rpc(&ctx, &mut p1, "eve_public.character.skill.plan.CreateRequest", create_b).await;
    assert_eq!(sp_create.status_code, STATUS_OK);
    let sp_wm = WireMessage::decode(&sp_create.payload.expect("sp create").value).expect("wm");
    let id_b = sp_wm.fields.iter().find(|f| f.field_number == 1).expect("id").value.as_bytes().expect("bytes");
    let sp_id_wm = WireMessage::decode(id_b).expect("id wm");
    let uuid_b = sp_id_wm.fields.iter().find(|f| f.field_number == 1).expect("uuid").value.as_bytes().expect("uuid bytes");
    let plan_uuid: [u8; 16] = uuid_b.try_into().expect("16b uuid");
    let (mut id_inner, mut plan_id_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &plan_uuid, &mut id_inner);
    encode_bytes_field(1, &id_inner, &mut plan_id_req);

    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.character.skill.plan.SetActiveRequest", plan_id_req.clone())
            .await
            .status_code,
        STATUS_OK
    );
    let active_rsp = quasar_rpc(&ctx, &mut p1, "eve_public.character.skill.plan.GetActiveRequest", vec![]).await;
    assert_eq!(active_rsp.status_code, STATUS_OK);
    let active_dyn = reg
        .decode_dynamic(
            "eve_public.character.skill.plan.GetActiveResponse",
            &active_rsp.payload.expect("active").value,
        )
        .expect("decode GetActiveResponse");
    assert!(
        format!("{active_dyn:?}").contains("Closure Gunnery Plan"),
        "GetActiveResponse must contain created skill plan name"
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.character.skill.plan.GetAllRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.character.skill.plan.milestone.GetAllRequest", plan_id_req.clone())
            .await
            .status_code,
        STATUS_OK
    );

    // 3. Career goal progress, industry jobs, active implants
    let goal_uuid = ctx
        .quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(P1 as u32)[0]
        .uuid;
    let (mut g_id, mut g_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &goal_uuid, &mut g_id);
    encode_bytes_field(2, &g_id, &mut g_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.character.career.goal.GetProgressRequest", g_req)
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.character.industry.job.GetRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.character.implant.GetActiveImplantsByCharacterRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );

    // 4. Persistence across SQLite reload
    let ctx_re = persist_and_reopen_sqlite(&ctx, "quasar_char_closure_re");
    let mut p1_re = select_char(&ctx_re, 81_003, "demo", "demo-secret", P1).await;
    let get_re = quasar_rpc(&ctx_re, &mut p1_re, "eve_public.character.skill.plan.GetRequest", plan_id_req).await;
    assert_eq!(get_re.status_code, STATUS_OK);
    let get_re_dyn = reg
        .decode_dynamic(
            "eve_public.character.skill.plan.GetResponse",
            &get_re.payload.expect("reloaded plan").value,
        )
        .expect("decode reloaded GetResponse");
    assert!(
        format!("{get_re_dyn:?}").contains("Closure Gunnery Plan"),
        "Reloaded skill plan must preserve name"
    );
}

#[tokio::test]
async fn test_quasar_payment_and_plex_domains_e2e() {
    // parity: feat:quasar.payment
    // parity: feat:quasar.plex
    eve_testkit::attest(&["feat:quasar.payment", "feat:quasar.plex"]);

    let ctx = ServerContext::new();
    let mut p1 = select_char(&ctx, 81_004, "demo", "demo-secret", P1).await;
    let mut p2 = select_char(&ctx, 81_005, "other", "other-secret", P2).await;
    let mut rx1 = ctx.quasar_gateway.subscribe_session(&p1, vec![], ctx.now_unix());

    // Seed store offer with real-money price_cents, payment token, and schedule for user 1
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cat = read_store_catalog(snap);
        cat.offers.push(StoreOffer {
            id: 7701,
            name: "Omega 30 Days".into(),
            description: "30 days Omega".into(),
            category_id: 1,
            price_plex: 500,
            product_ids: Vec::new(),
            targeted: false,
            price_cents: Some(1999),
            currency: Some("USD".into()),
        });
        save_store_catalog(snap, &cat);
        let tokens = vec![PaymentTokenRecord {
            id: 501,
            user_id: 1,
            token_str: "tok_visa_4242".into(),
            is_quick_pay: true,
            consumed: false,
        }];
        snap.service_state_blobs.insert(
            "payment_tokens".into(),
            serde_json::to_string(&tokens).expect("ser tok"),
        );
        let scheds = vec![PaymentScheduleRecord {
            id: 601,
            user_id: 1,
            schedule_type: 1,
            next_payment_unix: now + 30 * 86_400,
            offer_id: "7701".into(),
            interval_days: 30,
            created_unix: now,
        }];
        snap.service_state_blobs.insert(
            "payment_schedules".into(),
            serde_json::to_string(&scheds).expect("ser sched"),
        );
    });

    // Payment: CostRequest, TokenRequest, GetQuickPayRequest, GetRequest, DisableAllRequest, Schedule, CreditCardRequest
    let mut cost_req = Vec::new();
    encode_varint_field(1, 7701, &mut cost_req);
    encode_string_field(2, "USD", &mut cost_req);
    let cost_rsp = quasar_rpc(&ctx, &mut p1, "eve_public.payment.purchase.api.CostRequest", cost_req).await;
    assert_eq!(cost_rsp.status_code, STATUS_OK);

    let qp_rsp = quasar_rpc(&ctx, &mut p1, "eve_public.payment.token.api.GetQuickPayRequest", vec![]).await;
    assert_eq!(qp_rsp.status_code, STATUS_OK);
    assert!(!qp_rsp.payload.expect("qp").value.is_empty());

    let mut tok_get_req = Vec::new();
    encode_bytes_field(1, &encode_seq_id(501), &mut tok_get_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.payment.token.api.GetRequest", tok_get_req)
            .await
            .status_code,
        STATUS_OK
    );

    let (mut off_msg, mut order_b, mut tok_purch_req) = (Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "7701", &mut off_msg);
    encode_bytes_field(1, &off_msg, &mut order_b);
    encode_varint_field(2, 1, &mut order_b);
    encode_bytes_field(1, &order_b, &mut tok_purch_req);
    encode_bytes_field(2, &encode_seq_id(501), &mut tok_purch_req);
    let purch_rsp = quasar_rpc(&ctx, &mut p1, "eve_public.payment.purchase.api.TokenRequest", tok_purch_req).await;
    assert_eq!(purch_rsp.status_code, STATUS_OK);

    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.payment.token.api.DisableAllRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );
    let mut sched_req = Vec::new();
    encode_bytes_field(1, &encode_seq_id(601), &mut sched_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.payment.schedule.api.GetRequest", sched_req)
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.payment.schedule.api.GetActiveByTypeRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );
    // Real-money credit card backend must fail-closed as disabled
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve.payment.purchase.api.CreditCardRequest", vec![])
            .await
            .status_code,
        STATUS_FORBIDDEN
    );

    // PLEX: Balance, DepositPlexToVault -> BalanceChangedNotice, Transactions, Statistics
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.plex.vault.api.BalanceRequest", vec![])
            .await
            .status_code,
        STATUS_OK
    );
    let plex_item = ctx
        .db
        .add_or_stack_item(P1, JITA_4_4, InventoryFlag::Hangar, 44_992, 50)
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
            PyRep::Integer(50),
        ],
    )
    .await
    .expect("DepositPlexToVault");
    let notices = drain_notices(&ctx, &mut rx1);
    assert!(notices.iter().any(|n| {
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
    assert_eq!(
        quasar_rpc(
            &ctx,
            &mut p1,
            "eve_public.plex.vault.transaction.api.GetStatisticsRequest",
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
    let mut log_req = Vec::new();
    encode_bytes_field(1, &encode_seq_id(tx_id), &mut log_req);
    assert_eq!(
        quasar_rpc(&ctx, &mut p1, "eve_public.plex.vault.transaction.api.GetLogRequest", log_req.clone())
            .await
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        quasar_rpc(&ctx, &mut p2, "eve_public.plex.vault.transaction.api.GetLogRequest", log_req)
            .await
            .status_code,
        STATUS_NOT_FOUND
    );
}
