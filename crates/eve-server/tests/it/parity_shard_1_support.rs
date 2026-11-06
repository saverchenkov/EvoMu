//! Shared non-GM player-flow helpers for Parity Shard 1 end-to-end integration tests.
//! Zero world-state snapshot mutation (`with_mut_snapshot`), zero ephemeral `esi_state`
//! injection, and zero `/slash` GM shortcuts.

#![forbid(unsafe_code)]

use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
static DB_SEQ: AtomicU64 = AtomicU64::new(1);

pub(crate) fn temp_db_path(tag: &str) -> PathBuf {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!(
        "evomu_parity_s1_{tag}_{}_{seq}_{nanos}.sqlite",
        std::process::id()
    ));
    cleanup_sqlite_files(&p);
    p
}

pub(crate) fn cleanup_sqlite_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

pub(crate) fn issue_token(ctx: &ServerContext, character_id: i64, scopes: &[&str]) -> String {
    let now = ctx.now_unix();
    let user = if character_id == 90_000_002 { "other" } else { "demo" };
    let pass = format!("{user}-secret");
    let scps: Vec<String> = scopes.iter().map(|s| (*s).to_string()).collect();
    let code = ctx
        .db
        .authorize_pkce(
            user,
            &pass,
            character_id,
            "evomu-parity-shard1",
            "https://localhost/cb",
            scps,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-parity-shard1", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

pub(crate) fn esi_call(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
    expected_status: u16,
) -> (Value, HashMap<String, String>) {
    let mut req = EsiHttpRequest::new(method, path);
    if let Some(tok) = token {
        req = req.with_bearer_token(tok);
    }
    if let Some(b) = body {
        req = req.with_body(serde_json::to_vec(&b).unwrap());
    }
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        expected_status,
        "{method} {path} expected {expected_status}, got {} (body: {})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    spec.validate_response(method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed on {method} {path}: {e}"));
    let parsed = if matches!(expected_status, 204 | 304) || resp.body.is_empty() {
        Value::Null
    } else {
        resp.json().expect("valid JSON ESI response")
    };
    (parsed, resp.headers)
}

pub(crate) fn assert_etag_304(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) {
    let (_, headers) = esi_call(router, spec, "GET", path, token, None, 200);
    let etag = headers.get("ETag").or_else(|| headers.get("etag")).cloned().expect("ETag header");
    let mut req = EsiHttpRequest::new("GET", path).with_header("If-None-Match", &etag);
    if let Some(tok) = token {
        req = req.with_bearer_token(tok);
    }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 304, "ETag If-None-Match on GET {path}");
}

pub(crate) async fn lock_target_live(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    target_id: i64,
    approach_dist_m: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if let Some((_, tb)) = sys.get_ball_snapshot(target_id).await.unwrap() {
        sys.set_ball_position(ship_id, tb.position + DVec3::new(approach_dist_m, 0.0, 0.0)).await;
    }
    let beyonce = ctx.bind_object(sess, "beyonce", sys_id as i64).ok();
    let _ = ctx
        .dispatch_rpc(
            sess,
            "beyonce",
            beyonce,
            "CmdSetSpeedFraction",
            &[PyRep::Real(0.5)],
            &HashMap::new(),
            None,
        )
        .await;
    for _ in 0..45 {
        sys.step_manual(1.0).await.unwrap();
        let ready = sys
            .get_combat_state(ship_id)
            .await
            .ok()
            .flatten()
            .is_none_or(|cs| {
                cs.gate_cloak_remaining_ticks == 0 && !cs.is_cloaked && cs.decloak_targeting_delay_remaining_s <= 0.0
            });
        if ready {
            break;
        }
    }
    ctx.dispatch_rpc(
        sess,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(target_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("dogmaIM.AddTarget");
    ctx.advance_secs(12);
    for _ in 0..15 {
        if sys
            .get_target_locks(ship_id)
            .await
            .is_ok_and(|l| l.iter().any(|t| t.target_id == target_id && t.locked))
        {
            return;
        }
        sys.step_manual(1.0).await.unwrap();
    }
}

pub(crate) async fn destroy_target_live(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    guns: &[i64],
    target_id: i64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    lock_target_live(ctx, sess, sys_id, ship_id, target_id, 1_200.0).await;
    for _ in 0..45 {
        if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
            return;
        }
        for &gid in guns {
            if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
                return;
            }
            let _ = ctx
                .dispatch_rpc(
                    sess,
                    "dogmaIM",
                    None,
                    "Activate",
                    &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(target_id)],
                    &HashMap::new(),
                    None,
                )
                .await;
        }
        for _ in 0..5 {
            sys.step_manual(1.0).await.unwrap();
        }
        ctx.advance_secs(5);
    }
    panic!("target {target_id} survived 45 live weapon volleys");
}

pub(crate) fn reload_repo(path: &Path) -> std::sync::Arc<WorldRepository> {
    if !path.exists() {
        WorldRepository::with_demo_seed()
            .save_to_sqlite(path)
            .expect("seed initial sqlite world");
    }
    std::sync::Arc::new(WorldRepository::open_sqlite(path).expect("reopen sqlite repo"))
}

pub(crate) async fn upgrade_omega_live(
    ctx: &ServerContext,
    plex_seller: &mut SessionState,
    buyer: &mut SessionState,
) {
    use crate::fleet_boss_formations_corp_projects_live::quasar_call;
    use crate::industry_live_support::{rpc, JITA_STATION};
    use eve_proto::quasar::encode_varint_field;
    if buyer.address.is_empty() {
        buyer.address = "127.0.0.1".into();
    }
    let _ = rpc(ctx, buyer, "storeManager", "get_offers", &[]).await;
    rpc(
        ctx,
        plex_seller,
        "marketProxy",
        "PlacePlexSellOrder",
        &[PyRep::Real(5_100_000.0), PyRep::Integer(460), PyRep::Integer(90)],
    )
    .await
    .expect("PlacePlexSellOrder");
    rpc(
        ctx,
        buyer,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(44_992),
            PyRep::Real(5_100_000.0),
            PyRep::Integer(460),
            PyRep::Integer(32_767),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder PLEX");
    let mut buy_req = Vec::new();
    encode_varint_field(1, 1002, &mut buy_req);
    encode_varint_field(2, 4, &mut buy_req);
    let resp = quasar_call(
        ctx,
        buyer,
        "eve_public.plex.vault.api.PurchaseRequest",
        buy_req,
    );
    assert_eq!(resp.status_code, 200, "NES PLEX Omega purchase");
    for sid in [3330_i64, 3342, 3327, 20_342, 20_526] {
        let _ = rpc(ctx, buyer, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(sid)])]).await;
    }
    let char_id = buyer.character_id.unwrap();
    let mut queue = Vec::new();
    for (sid, target) in [(3330_u32, 5_u8), (3342, 3), (3327, 5), (20_342, 5), (20_526, 1)] {
        let curr = ctx
            .db
            .with_snapshot(|s| s.skills.get(&char_id).and_then(|m| m.get(&sid)).map(|r| r.level).unwrap_or(0));
        for lvl in (curr + 1)..=target {
            queue.push(PyRep::Tuple(vec![PyRep::Integer(i64::from(sid)), PyRep::Integer(i64::from(lvl))]));
        }
    }
    rpc(ctx, buyer, "skillMgr2", "SaveNewQueue", &[PyRep::List(queue)])
        .await
        .expect("queue Caldari Freighter skills");
    ctx.advance_virtual_clock(50 * 86_400);
    rpc(ctx, buyer, "skillMgr2", "GetSkillQueueAndFreePoints", &[])
        .await
        .expect("complete Caldari Freighter skills");
}

async fn settle_slow(ctx: &ServerContext, sess: &SessionState, max_steps: usize) {
    let (sys, ship) = (sess.solar_system_id.unwrap(), sess.ship_id.unwrap());
    let h = ctx.systems.get_or_spawn(sys);
    for _ in 0..max_steps {
        let _ = h.step_manual(1.0).await;
        let warping = matches!(
            h.get_ball_snapshot(ship).await,
            Ok(Some((_, ref b))) if matches!(b.mode, eve_destiny::MovementMode::Warp { .. })
        );
        if !warping {
            break;
        }
    }
    ctx.advance_secs(15);
}

pub(crate) async fn warp_to_slow(ctx: &ServerContext, sess: &mut SessionState, target: i64, dist: f64) {
    let sys = sess.solar_system_id.unwrap();
    let beyonce = ctx.bind_object(sess, "beyonce", sys as i64).ok();
    match ctx
        .dispatch_rpc(
            sess,
            "beyonce",
            beyonce,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(target), PyRep::Real(dist)],
            &HashMap::new(),
            None,
        )
        .await
    {
        Ok(_) => settle_slow(ctx, sess, 3_600).await,
        Err(e) if e.code == "WarpDistanceTooShort" => {}
        Err(e) => panic!("warp_to_slow {target}: {e:?}"),
    }
}

pub(crate) async fn travel_slow(ctx: &ServerContext, sess: &mut SessionState, dest_sys: u32) {
    use crate::industry_live_support::rpc;
    if sess.structure_id.is_some() {
        rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("structure Undock");
    } else if sess.station_id.is_some() {
        rpc(ctx, sess, "stationSvc", "Undock", &[]).await.expect("station Undock");
    }
    ctx.advance_secs(15);
    while sess.solar_system_id != Some(dest_sys) {
        let cur = sess.solar_system_id.unwrap();
        let mut prev: HashMap<u32, (u32, i64)> = HashMap::new();
        let mut q = std::collections::VecDeque::from([cur]);
        let mut seen = std::collections::HashSet::from([cur]);
        while let Some(s) = q.pop_front() {
            if s == dest_sys {
                break;
            }
            let mut gates: Vec<(&i64, &eve_sde::StargateJump)> = ctx
                .sde
                .jumps
                .iter()
                .filter(|(_, j)| j.from_solar_system_id == s)
                .collect();
            gates.sort_by_key(|g| *g.0);
            for (&gid, j) in gates {
                if seen.insert(j.to_solar_system_id) {
                    prev.insert(j.to_solar_system_id, (s, gid));
                    q.push_back(j.to_solar_system_id);
                }
            }
        }
        let mut next_sys = dest_sys;
        while prev[&next_sys].0 != cur {
            next_sys = prev[&next_sys].0;
        }
        let gate = prev[&next_sys].1;
        warp_to_slow(ctx, sess, gate, 0.0).await;
        let beyonce = ctx.bind_object(sess, "beyonce", cur as i64).ok();
        ctx.dispatch_rpc(sess, "beyonce", beyonce, "CmdStargateJump", &[PyRep::Integer(gate)], &HashMap::new(), None)
            .await
            .unwrap_or_else(|e| panic!("jump {gate}: {e:?}"));
        ctx.advance_secs(15);
    }
}

pub(crate) async fn dock_station_slow(ctx: &ServerContext, sess: &mut SessionState, station_id: i64) {
    let sys = ctx.sde.stations[&station_id].solar_system_id;
    travel_slow(ctx, sess, sys).await;
    warp_to_slow(ctx, sess, station_id, 0.0).await;
    crate::industry_live_support::rpc(ctx, sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)])
        .await
        .unwrap_or_else(|e| panic!("Dock {station_id}: {e:?}"));
    ctx.advance_secs(15);
}

pub(crate) async fn deploy_online_upwell_slow(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    hull_tid: u32,
    services: &[u32],
    fuel_qty: i64,
) -> (i64, Vec<i64>) {
    use crate::industry_live_support::{dock_structure, int, items_at, rpc};
    use eve_server::services::station_structure::structure_lifecycle as life;
    let owner = sess.character_id.unwrap();
    travel_slow(ctx, sess, sys_id).await;
    let sun = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6)
        .expect("sun")
        .celestial_id;
    warp_to_slow(ctx, sess, sun, 100_000.0).await;
    let anc = rpc(
        ctx,
        sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(hull_tid)), PyRep::String("Live Industry Complex".into())],
    )
    .await
    .expect("Anchor Upwell structure");
    let st_id = int(&anc, "structureID");
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS + 10);
    let _ = rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await;
    dock_structure(ctx, sess, st_id).await;
    let ship = sess.ship_id.unwrap();
    let core = ctx.sde.types[&hull_tid].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    let core_item = items_at(ctx, owner, ship, core)[0].0;
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(st_id), PyRep::Integer(172)])
        .await
        .expect("Insert Quantum Core");
    let mut fuel_left = fuel_qty;
    for tid in [4_051_u32, 4_246, 4_247, 4_312] {
        for (item, qty) in items_at(ctx, owner, ship, tid) {
            let take = qty.min(fuel_left);
            if take > 0 {
                rpc(
                    ctx,
                    sess,
                    "invbroker",
                    "Add",
                    &[PyRep::Integer(item), PyRep::Integer(st_id), PyRep::Integer(133), PyRep::Integer(take)],
                )
                .await
                .expect("Load Fuel Bay");
                fuel_left -= take;
            }
        }
    }
    assert_eq!(fuel_left, 0, "freighter must carry {fuel_qty} fuel blocks");
    rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("Undock for onlining");
    ctx.advance_virtual_clock(life::ONLINING_SECS + 15);
    let beyonce = ctx.bind_object(sess, "beyonce", sys_id as i64).expect("bind beyonce");
    ctx.dispatch_rpc(
        sess,
        "beyonce",
        Some(beyonce),
        "CmdFollowBall",
        &[PyRep::Integer(st_id), PyRep::Real(0.0)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Approach structure");
    ctx.dispatch_rpc(sess, "beyonce", Some(beyonce), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &HashMap::new(), None)
        .await
        .expect("Full speed");
    let h = ctx.systems.get_or_spawn(sys_id);
    for attempt in 0..80 {
        for _ in 0..15 {
            let _ = h.step_manual(1.0).await;
        }
        ctx.advance_secs(15);
        match rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await {
            Ok(_) => break,
            Err(e) if e.code == "OutsideDockingPerimeter" && attempt < 79 => continue,
            Err(e) => panic!("Dock {st_id}: {e:?}"),
        }
    }
    ctx.advance_secs(15);
    let mut fitted = Vec::new();
    for (idx, &stid) in services.iter().enumerate() {
        let item = items_at(ctx, owner, ship, stid)[0].0;
        let fit = rpc(
            ctx,
            sess,
            "invbroker",
            "Add",
            &[PyRep::Integer(item), PyRep::Integer(st_id), PyRep::Integer(164 + idx as i64)],
        )
        .await
        .expect("Fit & online service module");
        assert_eq!(fit.dict_get("online").and_then(|v| v.as_bool()), Some(true));
        fitted.push(int(&fit, "itemID"));
    }
    (st_id, fitted)
}
