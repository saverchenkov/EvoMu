#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle::sync_all_structures;
use glam::DVec3;
use std::collections::HashMap;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

pub(crate) async fn gm_docked_in_jita(ctx: &ServerContext, sess_id: u64, char_id: i64, ship_type_id: u32) -> (SessionState, i64) {
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    let created = common::slash_cmd(ctx, &mut sess, &format!("/createitem {ship_type_id} 1")).await;
    let ship_id = created.dict_get("itemID").and_then(PyRep::as_i64).expect("ship itemID");
    let skills: Vec<u32> = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| {
            [182_u16, 183, 184, 1285, 1289, 1290]
                .iter()
                .filter_map(|a| t.default_attributes.get(a))
                .map(|v| v.round() as u32)
                .filter(|&s| s > 0)
                .collect()
        })
        .unwrap_or_default();
    for sid in skills {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {sid} 5")).await;
    }
    for skill_id in [3363_u32, 3368, 11584] {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {skill_id} 5")).await;
    }
    ctx.advance_secs(31);
    rpc(ctx, &mut sess, "ship", "Board", &[PyRep::Integer(ship_id)])
        .await
        .expect("ship.Board");
    ctx.advance_secs(31);
    (sess, ship_id)
}

pub(crate) async fn undock_to_system(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, ship_id: i64, sys_id: u32) -> LivePilot {
    if sess.station_id.is_some() || sess.structure_id.is_some() {
        rpc(ctx, sess, "stationSvc", "Undock", &[]).await.expect("stationSvc.Undock");
    }
    common::slash_cmd(ctx, sess, &format!("/tr {sys_id}")).await;
    sess.ship_id = Some(ship_id);
    let bind = rpc(ctx, sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(sys_id)), PyRep::None])
        .await
        .expect("MachoBindObject");
    let PyRep::Tuple(parts) = bind else { panic!("expected bind tuple") };
    let beyonce = parts[0].as_i64().expect("beyonce oid") as u64;
    ctx.advance_secs(31);
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);
    LivePilot {
        sess: sess.clone(),
        char_id,
        ship_id,
        beyonce,
    }
}

fn extract_id(rep: &PyRep, key: &str) -> Option<i64> {
    rep.as_i64().or_else(|| rep.dict_get(key).and_then(PyRep::as_i64))
}

async fn anchor_online_astrahus(
    ctx: &ServerContext,
    p: &mut LivePilot,
    sys_id: u32,
    name: &str,
) -> i64 {
    let item_kv = common::slash_cmd(ctx, &mut p.sess, "/createitem 35832 1").await;
    let item_id = item_kv.dict_get("itemID").and_then(PyRep::as_i64).expect("astrahus itemID");
    rpc(
        ctx,
        &mut p.sess,
        "invbroker",
        "Add",
        &[PyRep::Integer(item_id), PyRep::Integer(p.ship_id), PyRep::Integer(5)],
    )
    .await
    .expect("move astrahus to cargo");
    let res = rpc(
        ctx,
        &mut p.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(item_id), PyRep::String(name.into())],
    )
    .await
    .expect("Anchor Astrahus");
    let sid = extract_id(&res, "structureID").expect("anchored structureID");

    ctx.advance_secs(87_305);
    let _ = sync_all_structures(ctx);

    let st_pos = ctx
        .systems
        .get_or_spawn(sys_id)
        .get_ball_snapshot(sid)
        .await
        .unwrap()
        .expect("anchored structure ball")
        .1
        .position;
    common::fly_to(ctx, p, st_pos - glam::DVec3::new(15_000.0, 0.0, 0.0)).await;

    rpc(ctx, &mut p.sess, "stationSvc", "Dock", &[PyRep::Integer(sid), PyRep::Integer(p.ship_id)])
        .await
        .expect("Dock in NoCore structure");
    let core_kv = common::slash_cmd(ctx, &mut p.sess, "/createitem 56201 1").await;
    let core_id = core_kv.dict_get("itemID").and_then(PyRep::as_i64).expect("core itemID");
    rpc(
        ctx,
        &mut p.sess,
        "invbroker",
        "Add",
        &[PyRep::Integer(core_id), PyRep::Integer(sid), PyRep::Integer(180)],
    )
    .await
    .expect("install quantum core");
    ctx.advance_secs(905);
    let _ = sync_all_structures(ctx);
    let _ = ctx.systems.get_or_spawn(sys_id);
    sid
}

// parity: rpc:lookupSvc.LookupWarableCorporationsOrAlliances
#[tokio::test]
async fn test_lookup_warable_corporations_and_alliances_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    const PERIMETER: u32 = 30_000_144;
    const RANCER: u32 = 30_002_718;

    // 1. Caller corp + alliance (must NEVER be returned as warable to self)
    let (mut caller_sess, caller_ship) = gm_docked_in_jita(&ctx, 7401, 93_400_001, 20_185).await;
    common::slash_cmd(&ctx, &mut caller_sess, "/giveisk 5000000000").await;
    ctx.advance_secs(15);
    let caller_corp_res = rpc(
        &ctx,
        &mut caller_sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("ApexCaller Vanguard".into()),
            PyRep::String("APXC".into()),
            PyRep::String("Caller".into()),
            PyRep::String("https://evomu.local".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("caller AddCorporation");
    let caller_corp = extract_id(&caller_corp_res, "corporationID").expect("caller corpID");
    ctx.advance_secs(15);
    rpc(
        &ctx,
        &mut caller_sess,
        "account",
        "GiveCash",
        &[
            PyRep::Integer(caller_corp),
            PyRep::Real(1_500_000_000.0),
            PyRep::String("Alliance fee".into()),
        ],
    )
    .await
    .expect("fund caller corp wallet");
    let caller_ally_res = rpc(
        &ctx,
        &mut caller_sess,
        "corpRegistry",
        "CreateAlliance",
        &[
            PyRep::String("ApexCaller Coalition".into()),
            PyRep::String("APXCA".into()),
            PyRep::String("Caller Ally".into()),
            PyRep::String("https://evomu.local".into()),
        ],
    )
    .await
    .expect("caller CreateAlliance");
    let caller_alliance = extract_id(&caller_ally_res, "allianceID").expect("caller allianceID");

    // 2. Standalone Corp A with anchored Astrahus -> WARABLE
    let (mut corp_a_sess, corp_a_ship) = gm_docked_in_jita(&ctx, 7402, 93_400_002, 20_185).await;
    ctx.advance_secs(15);
    let corp_a_res = rpc(
        &ctx,
        &mut corp_a_sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("ApexTarget Holdings".into()),
            PyRep::String("APXT".into()),
            PyRep::String("Warable".into()),
            PyRep::String("https://evomu.local".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("corp_a AddCorporation");
    let corp_a = extract_id(&corp_a_res, "corporationID").expect("corp_a ID");

    // 3. Structureless Corp B -> NOT WARABLE
    let (mut corp_b_sess, _corp_b_ship) = gm_docked_in_jita(&ctx, 7403, 93_400_003, 587).await;
    ctx.advance_secs(15);
    let corp_b_res = rpc(
        &ctx,
        &mut corp_b_sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("ApexPacifist Industrial".into()),
            PyRep::String("APXP".into()),
            PyRep::String("No structures".into()),
            PyRep::String("https://evomu.local".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("corp_b AddCorporation");
    let corp_b = extract_id(&corp_b_res, "corporationID").expect("corp_b ID");

    // 4. Alliance C with executor Corp D owning an Astrahus -> Alliance C WARABLE, member Corp D excluded individually
    let (mut corp_d_sess, corp_d_ship) = gm_docked_in_jita(&ctx, 7404, 93_400_004, 20_185).await;
    common::slash_cmd(&ctx, &mut corp_d_sess, "/giveisk 5000000000").await;
    ctx.advance_secs(15);
    let corp_d_res = rpc(
        &ctx,
        &mut corp_d_sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("ApexSov Executor".into()),
            PyRep::String("APXS".into()),
            PyRep::String("Alliance exec".into()),
            PyRep::String("https://evomu.local".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("corp_d AddCorporation");
    let corp_d = extract_id(&corp_d_res, "corporationID").expect("corp_d ID");
    ctx.advance_secs(15);
    rpc(
        &ctx,
        &mut corp_d_sess,
        "account",
        "GiveCash",
        &[
            PyRep::Integer(corp_d),
            PyRep::Real(1_500_000_000.0),
            PyRep::String("Alliance fee".into()),
        ],
    )
    .await
    .expect("fund corp_d wallet");
    let ally_c_res = rpc(
        &ctx,
        &mut corp_d_sess,
        "corpRegistry",
        "CreateAlliance",
        &[
            PyRep::String("ApexSov Federation".into()),
            PyRep::String("APXSF".into()),
            PyRep::String("Warable ally".into()),
            PyRep::String("https://evomu.local".into()),
        ],
    )
    .await
    .expect("ally_c CreateAlliance");
    let ally_c = extract_id(&ally_c_res, "allianceID").expect("ally_c ID");

    // Age corps 7+ days and anchor War HQ for caller_corp + Astrahuses for Corp A (Rancer) and Corp D (Perimeter)
    ctx.advance_secs(7 * 86_400 + 10);
    let mut pilot_caller = undock_to_system(&ctx, &mut caller_sess, 93_400_001, caller_ship, PERIMETER).await;
    let ast_caller = common::stock_ship_bay(&ctx, &mut pilot_caller, 35832, 1, 5).await;
    let caller_hq_res = rpc(
        &ctx,
        &mut pilot_caller.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(ast_caller), PyRep::String("Caller War HQ".into())],
    )
    .await
    .expect("Anchor Caller War HQ");
    let caller_hq = extract_id(&caller_hq_res, "structureID").expect("caller_hq ID");

    let mut pilot_a = undock_to_system(&ctx, &mut corp_a_sess, 93_400_002, corp_a_ship, RANCER).await;
    let ast_a = common::stock_ship_bay(&ctx, &mut pilot_a, 35832, 1, 5).await;
    let war_hq_res = rpc(
        &ctx,
        &mut pilot_a.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(ast_a), PyRep::String("ApexTarget HQ".into())],
    )
    .await
    .expect("Anchor Corp A Astrahus");
    let _war_hq = extract_id(&war_hq_res, "structureID").expect("war_hq");

    const MAURASI: u32 = 30_000_140;
    let mut pilot_d = undock_to_system(&ctx, &mut corp_d_sess, 93_400_004, corp_d_ship, MAURASI).await;
    let ast_d = common::stock_ship_bay(&ctx, &mut pilot_d, 35832, 1, 5).await;
    rpc(
        &ctx,
        &mut pilot_d.sess,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(ast_d), PyRep::String("ApexSov HQ".into())],
    )
    .await
    .expect("Anchor Corp D Astrahus");

    // 5. Query LookupWarableCorporationsOrAlliances("ApexTarget", false)
    let PyRep::List(rows) = rpc(
        &ctx,
        &mut caller_sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("ApexTarget".into()), PyRep::Bool(false)],
    )
    .await
    .expect("LookupWarableCorporationsOrAlliances")
    else {
        panic!("expected List");
    };
    let ids: Vec<i64> = rows.iter().filter_map(|r| r.dict_get("ownerID").and_then(PyRep::as_i64)).collect();
    assert!(ids.contains(&corp_a), "Corp A owning Upwell structure must be warable");
    assert!(!ids.contains(&corp_b), "Structureless Corp B must NOT be warable");
    assert!(!ids.contains(&caller_corp), "Caller own corp must be excluded");
    assert!(!ids.contains(&caller_alliance), "Caller own alliance must be excluded");

    // Broad substring query ("Apex") includes Alliance C (group 32) and Corp A (group 2), excludes member Corp D
    let PyRep::List(all_rows) = rpc(
        &ctx,
        &mut caller_sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("Apex".into()), PyRep::Bool(false)],
    )
    .await
    .expect("broad LookupWarableCorporationsOrAlliances")
    else {
        panic!("expected List");
    };
    let all_ids: Vec<i64> = all_rows.iter().filter_map(|r| r.dict_get("ownerID").and_then(PyRep::as_i64)).collect();
    assert!(all_ids.contains(&ally_c), "Alliance C with structure-owning member must be warable");
    assert!(!all_ids.contains(&corp_d), "Member corp of alliance must NOT be listed separately");
    assert!(!all_ids.contains(&caller_corp) && !all_ids.contains(&caller_alliance));

    // Exact ticker query ("APXT", true) matches Corp A
    let PyRep::List(exact_rows) = rpc(
        &ctx,
        &mut caller_sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("APXT".into()), PyRep::Bool(true)],
    )
    .await
    .expect("exact ticker lookup")
    else {
        panic!("expected List");
    };
    assert_eq!(exact_rows.len(), 1);
    assert_eq!(exact_rows[0].dict_get("ownerID"), Some(&PyRep::Integer(corp_a)));
    assert_eq!(exact_rows[0].dict_get("groupID"), Some(&PyRep::Integer(2)));

    // 6. Active war against Corp A excludes Corp A from warable results for caller
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(hq) = snap.structures.get_mut(&caller_hq) {
            hq.quantum_core_installed = true;
            hq.state = "ShieldVulnerable".into();
        }
    });
    let _ = undock_to_system(&ctx, &mut caller_sess, 93_400_001, caller_ship, PERIMETER).await;
    rpc(
        &ctx,
        &mut caller_sess,
        "corpRegistry",
        "DeclareWarAgainst",
        &[PyRep::Integer(corp_a), PyRep::Integer(caller_hq)],
    )
    .await
    .expect("DeclareWarAgainst Corp A");

    let PyRep::List(post_war_rows) = rpc(
        &ctx,
        &mut caller_sess,
        "lookupSvc",
        "LookupWarableCorporationsOrAlliances",
        &[PyRep::String("ApexTarget".into()), PyRep::Bool(false)],
    )
    .await
    .expect("post-war lookup")
    else {
        panic!("expected List");
    };
    assert!(
        !post_war_rows
            .iter()
            .any(|r| r.dict_get("ownerID") == Some(&PyRep::Integer(corp_a))),
        "Corp A must be excluded once caller is already in an active war with it"
    );
}

// parity: rpc:raffleProxy.CreateRaffle
#[tokio::test]
async fn test_raffle_proxy_create_raffle_hypercores_and_expiry_live() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    const PERIMETER: u32 = 30_000_144;
    let (mut creator_sess, creator_ship) = gm_docked_in_jita(&ctx, 7501, 93_500_001, 20_185).await;
    ctx.advance_secs(15);
    rpc(
        &ctx,
        &mut creator_sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("HyperNet Host Corp".into()),
            PyRep::String("HNHC".into()),
            PyRep::String("HyperNet".into()),
            PyRep::String("https://evomu.local".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let (mut buyer_sess, _buyer_ship) = gm_docked_in_jita(&ctx, 7502, 93_500_002, 587).await;
    common::slash_cmd(&ctx, &mut buyer_sess, "/giveisk 50000000").await;

    let prize_id = common::slash_cmd(&ctx, &mut creator_sess, "/createitem 587 1")
        .await
        .dict_get("itemID")
        .and_then(PyRep::as_i64)
        .expect("prize itemID");
    common::slash_cmd(&ctx, &mut creator_sess, "/createitem 52568 20").await;

    // 1. Invalid ticket count (12) -> InvalidRaffleTicketCount
    let err_cnt = rpc(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(prize_id)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(12)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_000_000.0)),
        ])],
    )
    .await
    .expect_err("Invalid node count must fail");
    assert_eq!(err_cnt.code, "InvalidRaffleTicketCount");

    // 2. Duration > 72h (259_201s) -> InvalidRaffleDuration
    let err_dur = rpc(
        &ctx,
        &mut creator_sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(prize_id)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_000_000.0)),
            (PyRep::String("duration".into()), PyRep::Integer(259_201)),
        ])],
    )
    .await
    .expect_err("Duration > 72h must fail");
    assert_eq!(err_dur.code, "InvalidRaffleDuration");

    // 3. Undocked in space -> InvalidRaffleLocation
    let mut creator_pilot = undock_to_system(&ctx, &mut creator_sess, 93_500_001, creator_ship, PERIMETER).await;
    let err_space = rpc(
        &ctx,
        &mut creator_pilot.sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(prize_id)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_000_000.0)),
        ])],
    )
    .await
    .expect_err("Undocked CreateRaffle must fail");
    assert_eq!(err_space.code, "InvalidRaffleLocation");

    // 4. Docked in Upwell Astrahus -> InvalidRaffleLocation
    ctx.advance_secs(7 * 86_400 + 10);
    common::fly_to(&ctx, &mut creator_pilot, DVec3::new(3_500_000.0, 0.0, 0.0)).await;
    let _upwell_sid = anchor_online_astrahus(&ctx, &mut creator_pilot, PERIMETER, "No-Raffle Astrahus").await;
    let err_upwell = rpc(
        &ctx,
        &mut creator_pilot.sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(prize_id)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_000_000.0)),
        ])],
    )
    .await
    .expect_err("Docked in Upwell structure must fail");
    assert_eq!(err_upwell.code, "InvalidRaffleLocation");

    // Return to Jita 4-4 NPC station (60_003_760)
    rpc(&ctx, &mut creator_pilot.sess, "stationSvc", "Undock", &[]).await.expect("Undock");
    common::slash_cmd(&ctx, &mut creator_pilot.sess, "/tr 30000142").await;
    let bind_jita = rpc(
        &ctx,
        &mut creator_pilot.sess,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(30_000_142), PyRep::None],
    )
    .await
    .expect("bind Jita beyonce");
    let PyRep::Tuple(j_parts) = bind_jita else { panic!("expected tuple") };
    let beyonce_jita = j_parts[0].as_i64().expect("beyonce_jita") as u64;
    ctx.dispatch_rpc(
        &mut creator_pilot.sess,
        "beyonce",
        Some(beyonce_jita),
        "CmdWarpToStuff",
        &[PyRep::String("station".into()), PyRep::Integer(60_003_760), PyRep::Real(0.0)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Warp to Jita 4-4");
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..300 {
        let _ = jita_handle.step_manual(1.0).await;
        if jita_handle
            .get_ball_snapshot(creator_pilot.ship_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|(_, b)| !matches!(b.mode, eve_destiny::MovementMode::Warp { .. }))
        {
            break;
        }
    }
    ctx.advance_secs(5);
    rpc(
        &ctx,
        &mut creator_pilot.sess,
        "stationSvc",
        "Dock",
        &[PyRep::Integer(60_003_760), PyRep::Integer(creator_pilot.ship_id)],
    )
    .await
    .expect("Dock in Jita 4-4 NPC station");

    // 5. Valid 48-node raffle at 1.25M ISK/ticket (60M ISK total -> consumes >= 2 HyperCores)
    let PyRep::KeyVal(r_kv) = rpc(
        &ctx,
        &mut creator_pilot.sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(prize_id)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(48)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_250_000.0)),
        ])],
    )
    .await
    .expect("CreateRaffle 48-node")
    else {
        panic!("expected KeyVal");
    };
    let raffle_id = r_kv.get("raffle_id").and_then(PyRep::as_i64).expect("raffle_id");
    let cores_used = r_kv.get("hypercores_used").and_then(PyRep::as_i64).expect("hypercores_used");
    assert!(cores_used >= 2, "Expected >= 2 HyperCores consumed, got {cores_used}");

    // 6. Buyer buys 2 tickets (2.5M ISK); after 72h expiry buyer refunded, item returned, HyperCores NOT refunded
    let buyer_before = rpc(&ctx, &mut buyer_sess, "account", "GetCashBalance", &[])
        .await
        .expect("buyer balance")
        .as_f64()
        .unwrap();
    rpc(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(2)],
    )
    .await
    .expect("BuyRandomTickets");
    let buyer_mid = rpc(&ctx, &mut buyer_sess, "account", "GetCashBalance", &[])
        .await
        .expect("buyer balance")
        .as_f64()
        .unwrap();
    assert!((buyer_before - buyer_mid - 2_500_000.0).abs() < 0.05);

    ctx.advance_secs(72 * 3_600 + 1);
    let PyRep::KeyVal(exp_kv) = rpc(&ctx, &mut creator_pilot.sess, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)])
        .await
        .expect("GetRaffle")
    else {
        panic!("expected KeyVal");
    };
    assert_eq!(exp_kv.get("is_expired"), Some(&PyRep::Bool(true)));
    let buyer_after = rpc(&ctx, &mut buyer_sess, "account", "GetCashBalance", &[])
        .await
        .expect("buyer balance after expiry")
        .as_f64()
        .unwrap();
    assert!((buyer_after - buyer_before).abs() < 0.05, "Expired raffle must refund ticket buyer");

    rpc(&ctx, &mut creator_pilot.sess, "ship", "AssembleShip", &[PyRep::Integer(prize_id)])
        .await
        .expect("Returned prize item is back in creator Jita 4-4 hangar");
}
