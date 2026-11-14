//! End-to-end live-RPC ESI integration tests for corporate blueprints and character/corporation industry jobs:
//! - `esi:GET /corporations/{corporation_id}/blueprints`, `feat:esi.corporation.blueprints`
//! - `esi:GET /characters/{character_id}/industry/jobs`, `feat:esi.industry.char-jobs`
//! - `esi:GET /corporations/{corporation_id}/industry/jobs`, `feat:esi.industry.corp-jobs`

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_and_select(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn fund_pilots_via_gm_transfer(ctx: &ServerContext, targets: &[(i64, f64)]) {
    ctx.db
        .provision_gm_account("isd_treasury", "isd-treasury-secret", 90_000_099, "ISD Treasury");
    let mut gm = login_and_select(ctx, 7900, "isd_treasury", "isd-treasury-secret", 90_000_099).await;
    rpc(ctx, &mut gm, "slash", "SlashCmd", &[PyRep::String("/giveisk 200000000000".into())])
        .await
        .expect("GM /giveisk");
    for &(cid, amount) in targets {
        rpc(
            ctx,
            &mut gm,
            "account",
            "GiveCash",
            &[PyRep::Integer(cid), PyRep::Real(amount), PyRep::String("test-grant".into())],
        )
        .await
        .expect("GM GiveCash");
    }
}

fn collect_skill_topo(sde: &eve_sde::SdeRegistry, sid: u32, seen: &mut Vec<u32>, out: &mut Vec<u32>) {
    if seen.contains(&sid) {
        return;
    }
    seen.push(sid);
    if let Some(t) = sde.types.get(&sid) {
        for attr in [182_u16, 183, 184, 1285, 1289, 1290] {
            if let Some(&req) = t.default_attributes.get(&attr) {
                if req > 0.0 {
                    collect_skill_topo(sde, req as u32, seen, out);
                }
            }
        }
    }
    out.push(sid);
}

async fn train_skills_to_five(ctx: &ServerContext, sess: &mut SessionState, skill_ids: &[u32]) {
    let char_id = sess.character_id.unwrap();
    let (mut seen, mut ordered) = (Vec::new(), Vec::new());
    for &sid in skill_ids {
        collect_skill_topo(&ctx.sde, sid, &mut seen, &mut ordered);
    }
    for sid in ordered {
        let curr = ctx
            .db
            .with_snapshot(|s| s.skills.get(&char_id).and_then(|m| m.get(&sid)).map(|r| r.level).unwrap_or(0));
        if curr >= 5 {
            continue;
        }
        let _ = rpc(ctx, sess, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(i64::from(sid))])]).await;
        let q: Vec<PyRep> = ((curr + 1)..=5)
            .map(|lvl| PyRep::Tuple(vec![PyRep::Integer(i64::from(sid)), PyRep::Integer(i64::from(lvl))]))
            .collect();
        rpc(ctx, sess, "skillMgr2", "SaveNewQueue", &[PyRep::List(q)])
            .await
            .unwrap_or_else(|e| panic!("SaveNewQueue skill {sid} failed: {e:?}"));
        ctx.advance_virtual_clock(90 * 86_400);
        let _ = rpc(ctx, sess, "skillMgr2", "GetSkillQueueAndFreePoints", &[]).await;
    }
}

async fn buy_jita(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i64, max_price: f64) {
    rpc(
        ctx,
        sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(i64::from(type_id)),
            PyRep::Real(max_price),
            PyRep::Integer(qty),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap_or_else(|e| panic!("PlaceBuyOrder type {type_id} x{qty} failed: {e:?}"));
}

fn issue_sso_token(ctx: &ServerContext, user: &str, pass: &str, char_id: i64, scopes: &[&str]) -> String {
    let now = ctx.now_unix();
    let scope_vec: Vec<String> = scopes.iter().map(|s| (*s).to_string()).collect();
    let code = ctx
        .db
        .authorize_pkce(
            user,
            pass,
            char_id,
            "evomu-live-esi-it",
            "https://localhost/callback",
            scope_vec,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-live-esi-it", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn check_esi_200(router: &EsiRouter, spec: &OpenApiSpec, target: &str, token: Option<&str>) -> (Value, String, Option<String>) {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code, 200,
        "Expected 200 for {target}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {target}: {e}"));
    let etag = resp.header("ETag").expect("ETag header required").to_string();
    assert!(resp.header("Cache-Control").is_some() || resp.header("Expires").is_some());
    let x_pages = resp.header("X-Pages").map(str::to_string);
    (resp.json().expect("valid JSON"), etag, x_pages)
}

fn assert_esi_status(router: &EsiRouter, target: &str, token: Option<&str>, expected: u16) {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(
        resp.status_code, expected,
        "Expected {expected} for {target}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
}

fn find_hangar_items(ctx: &ServerContext, owner_id: i64, loc_id: i64, type_id: u32) -> Vec<(i64, i32, bool, bool)> {
    let mut out: Vec<_> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == owner_id && i.location_id == loc_id && i.type_id == type_id)
            .map(|i| (i.item_id, i.quantity, i.singleton, i.is_blueprint_copy))
            .collect()
    });
    out.sort_by_key(|x| x.0);
    out
}

#[tokio::test]
async fn test_esi_live_corp_blueprints_and_char_corp_industry_jobs() {
    // parity: esi:GET /corporations/{corporation_id}/blueprints, feat:esi.corporation.blueprints
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/blueprints",
        "feat:esi.corporation.blueprints",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots_via_gm_transfer(&ctx, &[(PILOT_1, 5_000_000_000.0), (PILOT_2, 100_000_000.0)]).await;

    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let mut s1 = login_and_select(&ctx, 7901, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_and_select(&ctx, 7902, "other", "other-secret", PILOT_2).await;

    let (bp_tid, inv_skills, inv_mats, mfg_mats) = {
        let has_jita_sell_supply = |type_id: u32, min_qty: i64, max_price_isk: f64| -> bool {
            let max_cents = eve_db::IskCents::from_f64_isk(max_price_isk);
            ctx.db.with_snapshot(|s| {
                let available: i64 = s
                    .market_orders
                    .values()
                    .filter(|o| {
                        o.station_id == JITA_STATION
                            && !o.is_bid
                            && o.type_id == type_id
                            && o.price <= max_cents
                    })
                    .map(|o| i64::from(o.vol_remaining))
                    .sum();
                available >= min_qty
            })
        };
        let mut bp_ids: Vec<u32> = ctx.sde.blueprints.keys().copied().collect();
        bp_ids.sort_unstable();
        let (tid, bp) = bp_ids
            .into_iter()
            .filter_map(|btid| ctx.sde.blueprints.get(&btid).map(|b| (btid, b)))
            .find(|&(btid, b)| {
                ctx.sde.types.get(&btid).is_some_and(|t| t.published && t.market_group_id.is_some())
                    && has_jita_sell_supply(btid, 9, 50_000_000.0)
                    && !b.materials.is_empty()
                    && b.materials.iter().all(|m| {
                        m.quantity <= 80
                            && has_jita_sell_supply(m.type_id, (m.quantity as i64 * 8).max(20).max(800), 5_000_000.0)
                    })
                    && b.activities.get("invention").or_else(|| b.activities.get("8")).is_some_and(|a| {
                        !a.products.is_empty()
                            && a.materials.iter().all(|m| {
                                m.quantity <= 20
                                    && has_jita_sell_supply(m.type_id, (m.quantity as i64 * 4).max(8).max(50), 50_000_000.0)
                            })
                    })
            })
            .expect("T1 inventable blueprint on market");
        let inv = bp.activities.get("invention").or_else(|| bp.activities.get("8")).unwrap();
        let mut sk: Vec<u32> = inv.skills.iter().map(|s| s.type_id).collect();
        for k in ["manufacturing", "research_time", "research_material", "copying"] {
            if let Some(a) = bp.activities.get(k) {
                sk.extend(a.skills.iter().map(|s| s.type_id));
            }
        }
        sk.extend([3363, 3380, 3387, 3402, 3403, 3406, 3409, 11448, 11452, 21791]);
        sk.sort_unstable();
        sk.dedup();
        let im: Vec<(u32, u64)> = inv.materials.iter().map(|m| (m.type_id, m.quantity)).collect();
        let mm: Vec<(u32, u64)> = bp.materials.iter().map(|m| (m.type_id, m.quantity)).collect();
        (tid, sk, im, mm)
    };

    train_skills_to_five(&ctx, &mut s1, &inv_skills).await;

    let corp_rep = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Live Foundry Corp".into()), PyRep::String("LFND".into()), PyRep::Real(0.05)],
    )
    .await
    .expect("AddCorporation");
    let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap_or_else(|| s1.corporation_id.unwrap());
    rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(500_000_000.0), PyRep::String("corp-seed".into())])
        .await
        .expect("GiveCash corp");
    let rent_rep = rpc(&ctx, &mut s1, "officeManager", "RentOffice", &[PyRep::Integer(JITA_STATION)])
        .await
        .expect("RentOffice");
    let office_id = rent_rep.dict_get("officeFolderID").or_else(|| rent_rep.dict_get("officeID")).and_then(|v| v.as_i64()).expect("officeID");
    assert_ne!(office_id, JITA_STATION, "Rented office must have its own office item ID");

    let app_rep = rpc(&ctx, &mut s2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("join".into())])
        .await
        .expect("InsertApplication");
    let app_id = app_rep.dict_get("applicationID").and_then(|v| v.as_i64()).expect("applicationID");
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())])
        .await
        .expect("Accept PILOT_2 into corp");

    let cont_tid = [17366_u32, 17368, 17367]
        .into_iter()
        .find(|&tid| ctx.db.with_snapshot(|s| s.market_orders.values().any(|o| o.station_id == JITA_STATION && !o.is_bid && o.type_id == tid)))
        .expect("station container on Jita market");
    buy_jita(&ctx, &mut s1, cont_tid, 1, 50_000_000.0).await;
    let in_cont_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, cont_tid).into_iter().find(|c| !c.2).unwrap().0;
    rpc(&ctx, &mut s1, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(in_cont_id)])
        .await
        .expect("AssembleCargoContainer in");
    buy_jita(&ctx, &mut s1, cont_tid, 1, 50_000_000.0).await;
    let out_cont_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, cont_tid).into_iter().find(|c| c.0 != in_cont_id).unwrap().0;
    rpc(&ctx, &mut s1, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(out_cont_id)])
        .await
        .expect("AssembleCargoContainer out");

    buy_jita(&ctx, &mut s1, bp_tid, 9, 50_000_000.0).await;
    for &(mtid, mqty) in &mfg_mats {
        buy_jita(&ctx, &mut s1, mtid, (mqty as i64 * 8).max(20), 5_000_000.0).await;
    }
    for &(dtid, dqty) in &inv_mats {
        buy_jita(&ctx, &mut s1, dtid, (dqty as i64 * 4).max(8), 50_000_000.0).await;
    }

    let bpo_stack_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, bp_tid)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(bpo_stack_id), PyRep::Integer(office_id), PyRep::Integer(117), PyRep::Integer(3)])
        .await
        .expect("Move BPO stack of 3 into CorpSAG2");
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(bpo_stack_id), PyRep::Integer(office_id), PyRep::Integer(116), PyRep::Integer(1)])
        .await
        .expect("Move corp_res_bpo to CorpSAG1");
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(bpo_stack_id), PyRep::Integer(office_id), PyRep::Integer(118), PyRep::Integer(1)])
        .await
        .expect("Move corp_copy_bpo to CorpSAG3");
    let corp_bpos: Vec<i64> = find_hangar_items(&ctx, corp_id, office_id, bp_tid)
        .into_iter()
        .filter(|x| x.1 == 1)
        .map(|x| x.0)
        .collect();
    let (corp_res_bpo, corp_copy_bpo) = (corp_bpos[0], corp_bpos[1]);

    for &(mtid, mqty) in &mfg_mats {
        let src_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, mtid)[0].0;
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(src_id), PyRep::Integer(office_id), PyRep::Integer(116), PyRep::Integer((mqty as i64 * 2).max(2))])
            .await
            .expect("Move corp minerals to office");
        let src_id2 = find_hangar_items(&ctx, PILOT_1, JITA_STATION, mtid)[0].0;
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(src_id2), PyRep::Integer(in_cont_id), PyRep::Integer(4), PyRep::Integer((mqty as i64 * 3).max(3))])
            .await
            .expect("Move personal minerals to in_cont_id");
    }

    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(bpo_stack_id), PyRep::Integer(in_cont_id), PyRep::Integer(4), PyRep::Integer(1)])
        .await
        .expect("Move char_copy_bpo into in_cont_id");
    let char_copy_bpo = find_hangar_items(&ctx, PILOT_1, in_cont_id, bp_tid)[0].0;

    // 1. Corporate ME + TE research on corp_res_bpo -> researched singleton BPO (quantity == -1, ME=1, TE=2)
    for act_id in [4_i64, 3_i64] {
        let j = rpc(
            &ctx,
            &mut s1,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(corp_res_bpo)),
                ("facilityID", PyRep::Integer(JITA_STATION)),
                ("activityID", PyRep::Integer(act_id)),
                ("runs", PyRep::Integer(1)),
                ("isCorpJob", PyRep::Bool(true)),
            ])],
        )
        .await
        .expect("Install corp research job");
        let jid = j.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let dur = j.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap() as u64;
        ctx.advance_virtual_clock(dur + 2);
        rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(jid)])
            .await
            .expect("Complete corp research job");
    }

    // 2. Corporate Copy job on corp_copy_bpo -> corp BPC (quantity == -2, runs == 4) in office_id CorpSAG2
    let corp_copy = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(corp_copy_bpo)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(4)),
            ("isCorpJob", PyRep::Bool(true)),
            ("outputFlag", PyRep::Integer(117)),
        ])],
    )
    .await
    .expect("Install corp copy job");
    let corp_copy_jid = corp_copy.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let corp_copy_dur = corp_copy.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap() as u64;
    ctx.advance_virtual_clock(corp_copy_dur + 2);
    rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(corp_copy_jid)])
        .await
        .expect("Complete corp copy job");

    // 3. Active Corporate Manufacturing job on corp_copy_bpo (leaves corp_res_bpo unlocked in CorpSAG1 while locking corp_copy_bpo)
    let corp_mfg = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(corp_copy_bpo)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("isCorpJob", PyRep::Bool(true)),
            ("outputFlag", PyRep::Integer(118)),
        ])],
    )
    .await
    .expect("Install active corp mfg job");
    let corp_mfg_jid = corp_mfg.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    // 4. Personal Copy job from in_cont_id -> out_cont_id, complete, then active Invention (8) + Manufacturing (1)
    let char_copy = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(char_copy_bpo)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(6)),
            ("inputLocationID", PyRep::Integer(in_cont_id)),
            ("outputLocationID", PyRep::Integer(out_cont_id)),
        ])],
    )
    .await
    .expect("Install personal container copy job");
    let char_copy_jid = char_copy.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let char_copy_dur = char_copy.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap() as u64;
    ctx.advance_virtual_clock(char_copy_dur + 2);
    let char_copy_done = rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(char_copy_jid)])
        .await
        .expect("Complete personal copy job");
    let char_bpc_id = char_copy_done.dict_get("createdBpcItemID").and_then(|v| v.as_i64()).expect("createdBpcItemID");
    assert!(find_hangar_items(&ctx, PILOT_1, out_cont_id, bp_tid).iter().any(|x| x.0 == char_bpc_id && x.3));

    let char_inv = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(char_bpc_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(8)),
            ("runs", PyRep::Integer(1)),
            ("inputLocationID", PyRep::Integer(JITA_STATION)),
            ("outputLocationID", PyRep::Integer(out_cont_id)),
        ])],
    )
    .await
    .expect("Install personal invention job");
    let char_inv_jid = char_inv.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let char_mfg_bpo = {
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(bpo_stack_id), PyRep::Integer(in_cont_id), PyRep::Integer(4), PyRep::Integer(1)])
            .await
            .expect("Move char_mfg_bpo into in_cont_id");
        find_hangar_items(&ctx, PILOT_1, in_cont_id, bp_tid).into_iter().find(|x| x.0 != char_copy_bpo).unwrap().0
    };
    let char_mfg = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(char_mfg_bpo)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(2)),
            ("inputLocationID", PyRep::Integer(in_cont_id)),
            ("outputLocationID", PyRep::Integer(out_cont_id)),
        ])],
    )
    .await
    .expect("Install personal container manufacturing job");
    let char_mfg_jid = char_mfg.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    // --- Verify ESI: GET /v3/corporations/{corp_id}/blueprints/ ---
    let p1_bp_tok = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-corporations.read_blueprints.v1"]);
    let p1_no_scope = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &[]);
    let p2_bp_tok = issue_sso_token(&ctx, "other", "other-secret", PILOT_2, &["esi-corporations.read_blueprints.v1"]);
    let bp_url = format!("/v3/corporations/{corp_id}/blueprints/");

    assert_esi_status(&router, &bp_url, None, 401);
    assert_esi_status(&router, &bp_url, Some(&p1_no_scope), 403);
    assert_esi_status(&router, &bp_url, Some(&p2_bp_tok), 403);

    let (corp_bps_json, bp_etag, bp_pages) = check_esi_200(&router, &spec, &bp_url, Some(&p1_bp_tok));
    assert_eq!(bp_pages.as_deref(), Some("1"));
    let cond_bp = EsiHttpRequest::new("GET", &bp_url).with_bearer_token(&p1_bp_tok).with_header("If-None-Match", &bp_etag);
    assert_eq!(router.handle_request(&cond_bp).status_code, 304);

    let bps = corp_bps_json.as_array().expect("blueprints array");
    let researched = bps.iter().find(|b| b["item_id"] == json!(corp_res_bpo)).expect("researched singleton BPO");
    assert_eq!(researched["quantity"], json!(-1));
    assert_eq!(researched["runs"], json!(-1));
    assert_eq!(researched["material_efficiency"], json!(1));
    assert_eq!(researched["time_efficiency"], json!(2));
    assert_eq!(researched["location_id"], json!(office_id));
    assert_eq!(researched["location_flag"], json!("CorpSAG1"));

    let locked_bpo = bps.iter().find(|b| b["item_id"] == json!(corp_copy_bpo)).expect("locked active-job BPO");
    assert_eq!(locked_bpo["location_flag"], json!("Locked"));

    let bpc_row = bps.iter().find(|b| b["quantity"] == json!(-2)).expect("copied corp BPC");
    assert_eq!(bpc_row["runs"], json!(4));
    assert_eq!(bpc_row["location_id"], json!(office_id));
    assert_eq!(bpc_row["location_flag"], json!("CorpSAG2"));

    let stack_row = bps.iter().find(|b| b["quantity"].as_i64().unwrap_or(0) >= 2).expect("unresearched BPO stack");
    assert_eq!(stack_row["runs"], json!(-1));
    assert_eq!(stack_row["material_efficiency"], json!(0));
    assert_eq!(stack_row["location_id"], json!(office_id));

    // --- Verify ESI: GET /v1/characters/{character_id}/industry/jobs/ ---
    let p1_cjob_tok = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-industry.read_character_jobs.v1"]);
    let p2_cjob_tok = issue_sso_token(&ctx, "other", "other-secret", PILOT_2, &["esi-industry.read_character_jobs.v1"]);
    let cjob_base = format!("/v1/characters/{PILOT_1}/industry/jobs/");

    assert_esi_status(&router, &cjob_base, None, 401);
    assert_esi_status(&router, &cjob_base, Some(&p1_no_scope), 403);
    assert_esi_status(&router, &cjob_base, Some(&p2_cjob_tok), 403);

    let (active_cjobs, cjob_etag, _) = check_esi_200(&router, &spec, &format!("{cjob_base}?include_completed=false"), Some(&p1_cjob_tok));
    let cond_cj = EsiHttpRequest::new("GET", format!("{cjob_base}?include_completed=false"))
        .with_bearer_token(&p1_cjob_tok)
        .with_header("If-None-Match", &cjob_etag);
    assert_eq!(router.handle_request(&cond_cj).status_code, 304);

    let active_carr = active_cjobs.as_array().unwrap();
    assert!(active_carr.iter().all(|j| j["status"] != json!("delivered")));
    let inv_row = active_carr.iter().find(|j| j["job_id"] == json!(char_inv_jid)).expect("active invention job");
    assert!(inv_row["probability"].as_f64().unwrap() > 0.0);
    assert_eq!(inv_row["blueprint_location_id"], json!(out_cont_id));
    assert_eq!(inv_row["output_location_id"], json!(out_cont_id));

    let mfg_row = active_carr.iter().find(|j| j["job_id"] == json!(char_mfg_jid)).expect("active container mfg job");
    assert!(mfg_row["cost"].as_f64().unwrap() > 0.0);
    assert_eq!(mfg_row["blueprint_location_id"], json!(in_cont_id));
    assert_eq!(mfg_row["output_location_id"], json!(out_cont_id));
    assert_ne!(mfg_row["blueprint_location_id"], json!(JITA_STATION));

    let (all_cjobs, _, _) = check_esi_200(&router, &spec, &format!("{cjob_base}?include_completed=true"), Some(&p1_cjob_tok));
    let copy_row = all_cjobs.as_array().unwrap().iter().find(|j| j["job_id"] == json!(char_copy_jid)).expect("completed copy job");
    assert_eq!(copy_row["status"], json!("delivered"));
    assert_eq!(copy_row["licensed_runs"], json!(6));
    assert_eq!(copy_row["completed_character_id"], json!(PILOT_1));
    assert!(copy_row["completed_date"].as_str().is_some());

    // --- Verify ESI: GET /v1/corporations/{corporation_id}/industry/jobs/ ---
    let p1_corpjob_tok = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-industry.read_corporation_jobs.v1"]);
    let p2_corpjob_tok = issue_sso_token(&ctx, "other", "other-secret", PILOT_2, &["esi-industry.read_corporation_jobs.v1"]);
    let corpjob_base = format!("/v1/corporations/{corp_id}/industry/jobs/");

    assert_esi_status(&router, &corpjob_base, None, 401);
    assert_esi_status(&router, &corpjob_base, Some(&p1_no_scope), 403);
    assert_esi_status(&router, &corpjob_base, Some(&p2_corpjob_tok), 403);

    let (active_corpjobs, corp_etag, corp_pages) =
        check_esi_200(&router, &spec, &format!("{corpjob_base}?include_completed=false"), Some(&p1_corpjob_tok));
    assert_eq!(corp_pages.as_deref(), Some("1"));
    let cond_corpj = EsiHttpRequest::new("GET", format!("{corpjob_base}?include_completed=false"))
        .with_bearer_token(&p1_corpjob_tok)
        .with_header("If-None-Match", &corp_etag);
    assert_eq!(router.handle_request(&cond_corpj).status_code, 304);

    let ac_arr = active_corpjobs.as_array().unwrap();
    assert_eq!(ac_arr.len(), 1);
    assert_eq!(ac_arr[0]["job_id"], json!(corp_mfg_jid));
    assert_eq!(ac_arr[0]["location_id"], json!(office_id));
    assert_eq!(ac_arr[0]["blueprint_location_id"], json!(office_id));
    assert_eq!(ac_arr[0]["facility_id"], json!(JITA_STATION));
    assert!(ac_arr[0]["cost"].as_f64().unwrap() > 0.0);

    let (all_corpjobs, _, _) = check_esi_200(&router, &spec, &format!("{corpjob_base}?include_completed=true"), Some(&p1_corpjob_tok));
    let all_corp_arr = all_corpjobs.as_array().unwrap();
    assert!(all_corp_arr.len() >= 4);
    let done_corp_copy = all_corp_arr.iter().find(|j| j["job_id"] == json!(corp_copy_jid)).expect("completed corp copy");
    assert_eq!(done_corp_copy["status"], json!("delivered"));
    assert_eq!(done_corp_copy["licensed_runs"], json!(4));
    assert_eq!(done_corp_copy["location_id"], json!(office_id));
}
