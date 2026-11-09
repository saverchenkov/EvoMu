use crate::common;
use crate::quasar_progression_complete::quasar_rpc;
use eve_db::InventoryFlag;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystem;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::{BTreeSet, HashMap, HashSet};

const JITA_4_4: i64 = 60_003_760;
const PERIMETER_SYS: u32 = 30_000_144;
const ZARZAKH_SYS: u32 = 30_100_000;
const FULCRUM_STATION: i64 = 60_015_187;
const HYPERCORE_TYPE_ID: u32 = 52_568;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_select(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(11);
    sess
}

fn encode_seq_id(val: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, val, &mut b);
    b
}

fn read_varint(buf: &mut &[u8]) -> u64 {
    let mut val = 0u64;
    let mut shift = 0u32;
    while let Some((&byte, rest)) = buf.split_first() {
        *buf = rest;
        val |= u64::from(byte & 0x7F) << shift;
        if (byte & 0x80) == 0 {
            break;
        }
        shift += 7;
    }
    val
}

fn nested_id(resp: eve_proto::quasar::EvePublicResponse) -> u64 {
    let raw = resp.payload.expect("payload").value;
    let mut s = raw.as_slice();
    let _ = read_varint(&mut s);
    let len = read_varint(&mut s) as usize;
    let mut inner = &s[..len];
    let _ = read_varint(&mut inner);
    read_varint(&mut inner)
}

async fn cash_balance(ctx: &ServerContext, sess: &mut SessionState) -> f64 {
    rpc(ctx, sess, "account", "GetCashBalance", &[])
        .await
        .expect("GetCashBalance")
        .as_f64()
        .expect("balance float")
}

async fn list_hangar_items(ctx: &ServerContext, sess: &mut SessionState) -> Vec<PyRep> {
    rpc(
        ctx,
        sess,
        "invbroker",
        "List",
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
    )
    .await
    .expect("invbroker.List hangar")
    .crowset_rows()
    .unwrap_or_default()
    .to_vec()
}

async fn hangar_qty(ctx: &ServerContext, sess: &mut SessionState, type_id: u32) -> i64 {
    list_hangar_items(ctx, sess)
        .await
        .iter()
        .filter(|r| r.dbrow_get("typeID").and_then(|v| v.as_i64()) == Some(i64::from(type_id)))
        .map(|r| {
            let q = r.dbrow_get("quantity").and_then(|v| v.as_i64()).unwrap_or(0);
            if q < 0 { 1 } else { q }
        })
        .sum()
}

async fn hangar_has_item_id(ctx: &ServerContext, sess: &mut SessionState, item_id: i64) -> bool {
    list_hangar_items(ctx, sess)
        .await
        .iter()
        .any(|r| r.dbrow_get("itemID").and_then(|v| v.as_i64()) == Some(item_id))
}

async fn ship_hold_qty(
    ctx: &ServerContext,
    sess: &mut SessionState,
    ship_id: i64,
    flag: InventoryFlag,
    type_id: u32,
) -> i64 {
    assert_eq!(sess.ship_id, Some(ship_id));
    rpc(ctx, sess, "invbroker", "List", &[PyRep::Integer(flag as i64)])
        .await
        .expect("invbroker.List ship hold")
        .crowset_rows()
        .unwrap_or_default()
        .iter()
        .filter(|r| r.dbrow_get("typeID").and_then(|v| v.as_i64()) == Some(i64::from(type_id)))
        .map(|r| {
            let q = r.dbrow_get("quantity").and_then(|v| v.as_i64()).unwrap_or(0);
            if q < 0 { 1 } else { q }
        })
        .sum()
}

async fn spawned_belt_asteroids(
    handle: &eve_server::system_actor::SystemHandle,
) -> Vec<eve_server::system_actor::AsteroidState> {
    let balls = handle.list_balls().await.unwrap();
    let mut out = Vec::new();
    for b in balls {
        if b.id >= 750_000_000 {
            if let Some(st) = handle.get_asteroid_state(b.id).await.unwrap() {
                out.push(st);
            }
        }
    }
    out
}

#[tokio::test]
async fn test_live_e2e_mining_belts_ore_distribution_and_mn02() {
    // parity: feat:mining.belts-as-balls, plan:MN-02
    eve_testkit::attest(&[
        "feat:mining.belts-as-balls",
        "plan:MN-02",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. Dynamically discover live systems with asteroid belts across security & empire buckets.
    let find_belt_sys = |pred: &dyn Fn(&SolarSystem) -> bool| -> u32 {
        ctx.sde
            .solar_systems
            .values()
            .filter(|s| {
                pred(s)
                    && ctx
                        .sde
                        .celestials_in_system(s.solar_system_id)
                        .iter()
                        .any(|c| c.group_id == 9)
            })
            .min_by_key(|s| s.solar_system_id)
            .expect("matching belt system in SDE")
            .solar_system_id
    };

    // Region IDs: The Forge (Caldari 10000002), Heimatar (Minmatar 10000030), Domain (Amarr 10000043).
    let caldari_07 = find_belt_sys(&|s| (0.65..0.85).contains(&s.security_status) && s.region_id == 10_000_002);
    let minmatar_07 = find_belt_sys(&|s| (0.65..0.85).contains(&s.security_status) && s.region_id == 10_000_030);
    let amarr_07 = find_belt_sys(&|s| (0.65..0.85).contains(&s.security_status) && s.region_id == 10_000_043);
    let lowsec_sys = find_belt_sys(&|s| (0.10..0.40).contains(&s.security_status) && s.region_id != 10_000_070);
    let high_null_sys = find_belt_sys(&|s| (-0.40..0.0).contains(&s.security_status) && s.region_id != 10_000_070);
    let deep_null_sys = find_belt_sys(&|s| s.security_status <= -0.50 && s.region_id != 10_000_070);

    // Pochven regions have Triglavian anomaly ores rather than static group 9 belts.
    let pochven_ores: BTreeSet<u32> = ctx
        .sde
        .belt_ores_for_security_and_region(-0.5, 10_000_070)
        .iter()
        .map(|o| o.type_id)
        .collect();
    assert_eq!(pochven_ores.len(), 3);

    for (label, sys_id) in [
        ("1.0 highsec Perimeter", PERIMETER_SYS),
        ("0.7 Caldari", caldari_07),
        ("0.7 Minmatar", minmatar_07),
        ("0.7 Amarr", amarr_07),
        ("lowsec", lowsec_sys),
        ("high nullsec", high_null_sys),
        ("deep nullsec", deep_null_sys),
    ] {
        let expected_ores = ctx.sde.belt_ores_for_system(sys_id);
        let expected_types: BTreeSet<u32> = expected_ores.iter().map(|o| o.type_id).collect();
        let handle = ctx.systems.get_or_spawn(sys_id);
        let _ = handle.populate_asteroid_belt_entities().await;
        let belts: Vec<_> = ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .filter(|c| c.group_id == 9)
            .collect();
        assert!(!belts.is_empty(), "{label} must define SDE asteroid belts");

        let mut present_belt_balls = 0usize;
        for belt in &belts {
            if handle.get_ball_snapshot(belt.celestial_id).await.unwrap().is_some() {
                present_belt_balls += 1;
            }
        }
        assert_eq!(present_belt_balls, belts.len(), "{label}: all belt balls must spawn in ballpark");

        let asteroids = spawned_belt_asteroids(&handle).await;
        assert!(!asteroids.is_empty(), "{label}: belts must populate live asteroid balls");
        let spawned_types: BTreeSet<u32> = asteroids.iter().map(|a| a.type_id).collect();
        assert!(
            expected_types.is_subset(&spawned_types),
            "{label}: spawned belt ores {:?} must include distribution {:?}",
            spawned_types, expected_types
        );
    }

    // Check distinctive empire & security distributions directly from live system_actor balls (V24 FSD belt data).
    let perim_types: HashSet<u32> = spawned_belt_asteroids(&ctx.systems.get_or_spawn(PERIMETER_SYS))
        .await
        .into_iter()
        .map(|a| a.type_id)
        .collect();
    assert!(HashSet::from([1228, 1230, 17463, 17464, 17470, 17471]).is_subset(&perim_types));
    let cal_types: HashSet<u32> = spawned_belt_asteroids(&ctx.systems.get_or_spawn(caldari_07))
        .await
        .into_iter()
        .map(|a| a.type_id)
        .collect();
    assert!(cal_types.contains(&18) && cal_types.contains(&1230));
    let amr_types: HashSet<u32> = spawned_belt_asteroids(&ctx.systems.get_or_spawn(amarr_07))
        .await
        .into_iter()
        .map(|a| a.type_id)
        .collect();
    assert!(amr_types.contains(&1224) && !amr_types.contains(&18));
    let min_types: HashSet<u32> = spawned_belt_asteroids(&ctx.systems.get_or_spawn(minmatar_07))
        .await
        .into_iter()
        .map(|a| a.type_id)
        .collect();
    assert!(min_types.contains(&1230) && min_types.contains(&1228) && (min_types.contains(&18) || min_types.contains(&1224)));
    let amamake_handle = ctx.systems.get_or_spawn(30_002_537);
    let _ = amamake_handle.populate_asteroid_belt_entities().await;
    let low_types: HashSet<u32> = spawned_belt_asteroids(&amamake_handle)
        .await
        .into_iter()
        .map(|a| a.type_id)
        .collect();
    assert!(low_types.contains(&20) && low_types.contains(&1227) && !low_types.contains(&1230));
    let oeg_handle = ctx.systems.get_or_spawn(30_000_439);
    let _ = oeg_handle.populate_asteroid_belt_entities().await;
    let deep_null_types: HashSet<u32> = spawned_belt_asteroids(&oeg_handle)
        .await
        .into_iter()
        .map(|a| a.type_id)
        .collect();
    assert!(deep_null_types.contains(&1223) && deep_null_types.contains(&22) && deep_null_types.contains(&11396));

    // 2. End-to-end docked Venture fit -> Undock -> /tr Perimeter -> warp to real belt -> approach -> mine -> verify hold + ledger.
    ctx.db.provision_gm_account("gm_miner_460", "gm-secret", 90_000_460, "GM Miner 460");
    let mut miner_sess = login_select(&ctx, 41, "gm_miner_460", "gm-secret", 90_000_460).await;
    common::slash_cmd(&ctx, &mut miner_sess, "/giveskill 3386 5").await;
    common::slash_cmd(&ctx, &mut miner_sess, "/giveskill 32918 5").await;
    let venture_id = common::slash_cmd(&ctx, &mut miner_sess, "/createitem 32880 1")
        .await
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("Venture itemID");
    ctx.advance_secs(31);
    rpc(&ctx, &mut miner_sess, "ship", "Board", &[PyRep::Integer(venture_id)])
        .await
        .expect("ship.Board Venture");
    let miner_id = common::slash_cmd(&ctx, &mut miner_sess, "/createitem 483 1")
        .await
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("Miner I itemID");
    rpc(
        &ctx,
        &mut miner_sess,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(miner_id),
            PyRep::Integer(venture_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
        ],
    )
    .await
    .expect("fit Miner I to HiSlot0");
    rpc(&ctx, &mut miner_sess, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(venture_id)])
        .await
        .expect("ShipOnlineModules");
    ctx.advance_secs(31);
    rpc(&ctx, &mut miner_sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    common::slash_cmd(&ctx, &mut miner_sess, &format!("/tr {PERIMETER_SYS}")).await;
    let PyRep::Tuple(bound_b) = rpc(
        &ctx,
        &mut miner_sess,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(i64::from(PERIMETER_SYS)), PyRep::None],
    )
    .await
    .expect("bind beyonce")
    else {
        panic!("bind tuple");
    };
    let beyonce_oid = bound_b[0].as_i64().unwrap() as u64;
    let mut pilot = common::LivePilot {
        sess: miner_sess,
        char_id: 90_000_460,
        ship_id: venture_id,
        beyonce: beyonce_oid,
    };

    let perim_handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let target_belt = ctx
        .sde
        .celestials_in_system(PERIMETER_SYS)
        .into_iter()
        .find(|c| c.group_id == 9)
        .expect("Perimeter belt");
    let target_ast = spawned_belt_asteroids(&perim_handle)
        .await
        .into_iter()
        .find(|a| a.belt_id == target_belt.celestial_id)
        .expect("asteroid in Perimeter belt");

    rpc(
        &ctx,
        &mut pilot.sess,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(target_belt.celestial_id), PyRep::Real(0.0)],
    )
    .await
    .expect("CmdWarpToStuff belt");
    for _ in 0..180 {
        perim_handle.step_manual(1.0).await.unwrap();
        let (_, ball) = perim_handle.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap();
        if !matches!(ball.mode, MovementMode::Warp { .. }) {
            break;
        }
    }
    let (_, ship_at_belt) = perim_handle.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap();
    assert!(!matches!(ship_at_belt.mode, MovementMode::Warp { .. }));
    let (_, ast_ball) = perim_handle.get_ball_snapshot(target_ast.asteroid_id).await.unwrap().unwrap();
    assert!(ship_at_belt.position.distance(target_belt.position) <= 50_000.0);

    if ship_at_belt.position.distance(ast_ball.position) > 7_000.0 {
        let dir = (ship_at_belt.position - ast_ball.position).normalize_or_zero();
        common::fly_to(&ctx, &mut pilot, ast_ball.position + dir * 5_000.0).await;
    }

    rpc(&ctx, &mut pilot.sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_ast.asteroid_id)])
        .await
        .expect("AddTarget");
    for _ in 0..10 {
        perim_handle.step_manual(1.0).await.unwrap();
    }
    rpc(
        &ctx,
        &mut pilot.sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(miner_id),
            PyRep::String("miningLaser".into()),
            PyRep::Integer(target_ast.asteroid_id),
        ],
    )
    .await
    .expect("Activate Miner I on belt asteroid");
    ctx.advance_secs(65);
    tick_background_cluster(&ctx).await;

    let ast_after = perim_handle
        .get_asteroid_state(target_ast.asteroid_id)
        .await
        .unwrap()
        .expect("asteroid remains");
    assert!(
        ast_after.remaining_units < target_ast.initial_units,
        "Mining cycle must deplete live belt asteroid: {} < {}",
        ast_after.remaining_units,
        target_ast.initial_units
    );
    let ore_qty = ship_hold_qty(
        &ctx,
        &mut pilot.sess,
        pilot.ship_id,
        InventoryFlag::SpecializedOreHold,
        target_ast.type_id,
    )
    .await
        + ship_hold_qty(
            &ctx,
            &mut pilot.sess,
            pilot.ship_id,
            InventoryFlag::Cargo,
            target_ast.type_id,
        )
        .await;
    assert!(ore_qty > 0, "Mined belt ore must appear in Venture ore/cargo hold");

    let PyRep::List(ledger_rows) = rpc(&ctx, &mut pilot.sess, "characterMiningLedger", "GetCharacterLogs", &[])
        .await
        .expect("GetCharacterLogs")
    else {
        panic!("GetCharacterLogs list");
    };
    let mined_row = ledger_rows
        .iter()
        .find(|r| r.dict_get("typeID").and_then(|v| v.as_i64()) == Some(i64::from(target_ast.type_id)))
        .expect("ledger row for mined belt ore");
    assert_eq!(mined_row.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(i64::from(PERIMETER_SYS)));
    assert!(mined_row.dict_get("quantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
}

#[tokio::test]
async fn test_live_e2e_omega_weekly_free_sp() {
    // parity: feat:skills.omega-weekly-free-sp
    eve_testkit::attest(&["feat:skills.omega-weekly-free-sp"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    ctx.db.provision_gm_account("gm_omega_sp", "gm-secret", 90_000_471, "GM SP Admin");
    let mut gm_sess = login_select(&ctx, 51, "gm_omega_sp", "gm-secret", 90_000_471).await;
    let mut user_sess = login_select(&ctx, 52, "other", "other-secret", 90_000_002).await;
    let char_id = 90_000_002i64;
    let uid = user_sess.user_id.expect("other user_id");

    // 1. Admin creates NES category -> product -> offer tagged "omega-weekly-free-sp" via Quasar admin RPCs.
    let (mut cat_a, mut cat_r) = (Vec::new(), Vec::new());
    encode_string_field(1, "Weekly SP Services", &mut cat_a);
    encode_bytes_field(1, &cat_a, &mut cat_r);
    let cat_resp = quasar_rpc(&ctx, &mut gm_sess, "eve.store.admin.api.CreateCategoryRequest", cat_r).await;
    assert_eq!(cat_resp.status_code, STATUS_OK);
    let cat_id = nested_id(cat_resp);

    let (mut item_m, mut prod_a, mut prod_r) = (Vec::new(), Vec::new(), Vec::new());
    encode_bytes_field(1, &encode_seq_id(0), &mut item_m);
    encode_varint_field(2, 15_000, &mut item_m);
    encode_string_field(1, "15k Weekly Skill Points", &mut prod_a);
    encode_bytes_field(2, &item_m, &mut prod_a);
    encode_varint_field(1, 1, &mut prod_r);
    encode_bytes_field(2, &prod_a, &mut prod_r);
    let prod_resp = quasar_rpc(&ctx, &mut gm_sess, "eve.store.admin.api.CreateProductRequest", prod_r).await;
    assert_eq!(prod_resp.status_code, STATUS_OK);
    let prod_id = nested_id(prod_resp);

    let (mut price_m, mut link_m, mut off_a, mut off_r) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, 0, &mut price_m);
    encode_bytes_field(1, &encode_seq_id(prod_id), &mut link_m);
    encode_string_field(1, "Omega Weekly 15,000 SP E2E", &mut off_a);
    encode_bytes_field(5, &price_m, &mut off_a);
    encode_string_field(16, "omega-weekly-free-sp", &mut off_a);
    encode_bytes_field(17, &encode_seq_id(cat_id), &mut off_a);
    encode_bytes_field(18, &link_m, &mut off_a);
    encode_bytes_field(1, &off_a, &mut off_r);
    let off_resp = quasar_rpc(&ctx, &mut gm_sess, "eve.store.admin.api.CreateOfferRequest", off_r).await;
    assert_eq!(off_resp.status_code, STATUS_OK);

    let PyRep::List(offers) = rpc(&ctx, &mut user_sess, "storeManager", "get_offers", &[]).await.unwrap() else {
        panic!("get_offers list");
    };
    let offer_id = offers
        .iter()
        .find(|o| o.dict_get("name").and_then(|v| v.as_str()) == Some("Omega Weekly 15,000 SP E2E"))
        .and_then(|o| o.dict_get("offerID").and_then(|v| v.as_i64()))
        .expect("offer_id");

    // 2. Alpha account ("other") is blocked with OmegaRequired.
    assert_eq!(
        rpc(&ctx, &mut user_sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(offer_id)])
            .await
            .unwrap()
            .as_bool(),
        Some(false)
    );
    let alpha_err = rpc(
        &ctx,
        &mut user_sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(alpha_err.code, "OmegaRequired");

    // 3. Upgrade to Omega via /setomega and claim first week's 15,000 SP.
    let exp_unix = ctx.now_unix() + 60 * 86_400;
    common::slash_cmd(&ctx, &mut gm_sess, &format!("/setomega {uid} {exp_unix}")).await;
    assert_eq!(
        rpc(&ctx, &mut user_sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(offer_id)])
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );

    let sp0 = rpc(&ctx, &mut user_sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.unwrap().as_f64().unwrap_or(0.0);
    let _ = ctx.drain_notifications_for_char(char_id);
    let claim1 = rpc(
        &ctx,
        &mut user_sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .expect("week 1 claim");
    assert_eq!(claim1.dict_get("freeSkillPointsGranted").and_then(|v| v.as_i64()), Some(15_000));
    let sp1 = rpc(&ctx, &mut user_sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.unwrap().as_f64().unwrap();
    assert!((sp1 - (sp0 + 15_000.0)).abs() < 1e-6);
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnFreeSkillPointsChanged")
    );

    // 4. Duplicate claim in same Tuesday-downtime week is rejected.
    assert_eq!(
        rpc(&ctx, &mut user_sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(offer_id)])
            .await
            .unwrap()
            .as_bool(),
        Some(false)
    );
    let dup_err = rpc(
        &ctx,
        &mut user_sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(dup_err.code.to_ascii_lowercase(), "weeklyofferalreadyclaimed");

    // 5. Advance virtual clock 7 days past Tuesday 11:00 UTC reset -> next-week claim succeeds.
    ctx.advance_secs(7 * 86_400);
    assert_eq!(
        rpc(&ctx, &mut user_sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(offer_id)])
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let claim2 = rpc(
        &ctx,
        &mut user_sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(offer_id), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .expect("week 2 claim");
    assert_eq!(claim2.dict_get("freeSkillPointsGranted").and_then(|v| v.as_i64()), Some(15_000));
    let sp2 = rpc(&ctx, &mut user_sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.unwrap().as_f64().unwrap();
    assert!((sp2 - (sp0 + 30_000.0)).abs() < 1e-6);
}

#[tokio::test]
async fn test_live_e2e_zarzakh_gravity_hazard_safe_zones() {
    // parity: feat:navigation.gravity-hazard-safe-zones
    eve_testkit::attest(&["feat:navigation.gravity-hazard-safe-zones"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut safe_anchors: Vec<DVec3> = ctx
        .sde
        .stargates()
        .values()
        .filter(|g| g.from_solar_system_id == ZARZAKH_SYS)
        .map(|g| g.position)
        .collect();
    if let Some(st) = ctx.sde.station(FULCRUM_STATION) {
        safe_anchors.push(st.position);
    }

    // 1. Spawn a Praxis at the star (outside all 150 km safe zones): ticks apply 50 HP/s structure damage.
    let mut pilot = common::gm_pilot_in_space(&ctx, 61, 90_000_461, 47466, ZARZAKH_SYS).await;
    let zk_handle = ctx.systems.get_or_spawn(ZARZAKH_SYS);
    let (_, start_ball) = zk_handle.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap();
    assert!(
        safe_anchors.iter().all(|a| start_ball.position.distance(*a) > 150_000.0),
        "Zarzakh spawn point must be outside all 150 km safe zones"
    );
    let cs_before = zk_handle.get_combat_state(pilot.ship_id).await.unwrap().unwrap();
    let max_hull = cs_before.max_structure_hp;
    for _ in 0..10 {
        zk_handle.step_manual(1.0).await.unwrap();
    }
    let cs_damaged = zk_handle.get_combat_state(pilot.ship_id).await.unwrap().unwrap();
    assert!(
        (cs_damaged.structure_hp - (cs_before.structure_hp - 500.0)).abs() < 1.0,
        "Outside safe zone must apply 50 HP/s structure damage (expected -500, got {} -> {})",
        cs_before.structure_hp,
        cs_damaged.structure_hp
    );

    // 2. Warp to The Fulcrum (60015187) inside the 150 km safe zone, heal, and verify 0 hazard damage across ticks.
    rpc(
        &ctx,
        &mut pilot.sess,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(FULCRUM_STATION), PyRep::Real(0.0)],
    )
    .await
    .expect("CmdWarpToStuff Fulcrum");
    for _ in 0..120 {
        zk_handle.step_manual(1.0).await.unwrap();
        let (_, ball) = zk_handle.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap();
        if !matches!(ball.mode, MovementMode::Warp { .. }) {
            break;
        }
    }
    common::slash_cmd(&ctx, &mut pilot.sess, "/heal").await;
    let cs_safe_0 = zk_handle
        .get_combat_state(pilot.ship_id)
        .await
        .unwrap()
        .expect("ship combat state at Fulcrum");
    assert_eq!(cs_safe_0.structure_hp, max_hull);

    for _ in 0..15 {
        zk_handle.step_manual(1.0).await.unwrap();
    }
    let cs_safe_after = zk_handle
        .get_combat_state(pilot.ship_id)
        .await
        .unwrap()
        .expect("ship survives inside Fulcrum safe zone");
    assert_eq!(cs_safe_after.structure_hp, max_hull, "Safe zone inside 150 km of Fulcrum must take 0 hazard damage");

    // 3. Sustained exposure outside safe zones destroys a Rifter cleanly.
    let doomed = common::gm_pilot_in_space(&ctx, 62, 90_000_462, 587, ZARZAKH_SYS).await;
    let mut destroyed = false;
    for _ in 0..40 {
        zk_handle.step_manual(1.0).await.unwrap();
        if zk_handle.get_combat_state(doomed.ship_id).await.unwrap().is_none() {
            destroyed = true;
            break;
        }
    }
    assert!(destroyed, "Sustained Zarzakh gravity hazard outside safe zone must destroy ship");
}

#[tokio::test]
async fn test_live_e2e_hypernet_relay_sizing_expiry_and_payout() {
    // parity: feat:wallet.hypernet-relay, feat:wallet.hypernet-hypercore-sizing-expiry
    eve_testkit::attest(&[
        "feat:wallet.hypernet-relay",
        "feat:wallet.hypernet-hypercore-sizing-expiry",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    ctx.db.provision_gm_account("gm_hyper_s1", "gm-secret", 90_000_472, "Seller Pilot");
    let mut s1 = login_select(&ctx, 71, "gm_hyper_s1", "gm-secret", 90_000_472).await;
    let mut s2 = login_select(&ctx, 72, "other", "other-secret", 90_000_002).await;
    let buyer_char_id = 90_000_002i64;
    assert_eq!(s1.station_id, Some(JITA_4_4));
    assert_eq!(s2.station_id, Some(JITA_4_4));

    common::slash_cmd(&ctx, &mut s1, "/giveisk 50000000").await;
    rpc(
        &ctx,
        &mut s1,
        "account",
        "GiveCash",
        &[
            PyRep::Integer(buyer_char_id),
            PyRep::Real(20_000_000.0),
            PyRep::String("Buyer funding".into()),
        ],
    )
    .await
    .expect("GiveCash to buyer");

    common::slash_cmd(&ctx, &mut s1, &format!("/giveitem {HYPERCORE_TYPE_ID} 300")).await;
    let r1 = common::slash_cmd(&ctx, &mut s1, "/giveitem 587 1")
        .await
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("Rifter 1 itemID");
    let r2 = common::slash_cmd(&ctx, &mut s1, "/giveitem 587 1")
        .await
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("Rifter 2 itemID");

    // 1. Undocked CreateRaffle is blocked; dock back afterward via real CmdDock.
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.expect("Undock");
    let err_space = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(r1)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(8)),
            (PyRep::String("ticketPrice".into()), PyRep::Real(100_000.0)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_space.code, "InvalidRaffleLocation");
    rpc(&ctx, &mut s1, "beyonce", "CmdDock", &[PyRep::Integer(JITA_4_4)])
        .await
        .expect("CmdDock back to Jita 4-4");

    // 2. Invalid node sizing (7 not in {8, 16, 48, 512}), invalid duration, and historic prices lookup.
    let err_nodes = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(r1)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(7)),
            (PyRep::String("ticketPrice".into()), PyRep::Real(100_000.0)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_nodes.code, "InvalidRaffleTicketCount");

    let err_dur = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(r1)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(8)),
            (PyRep::String("ticketPrice".into()), PyRep::Real(100_000.0)),
            (PyRep::String("duration".into()), PyRep::Integer(300_000)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_dur.code, "InvalidRaffleDuration");

    let PyRep::Tuple(price_vals) = rpc(&ctx, &mut s1, "raffleProxy", "GetActiveHistoricPrices", &[PyRep::Integer(587)])
        .await
        .expect("GetActiveHistoricPrices")
    else {
        panic!("GetActiveHistoricPrices tuple");
    };
    assert!(price_vals[0].as_f64().unwrap_or(0.0) > 0.0);

    // 3. Create 8-node raffle -> HyperCores deducted, item escrowed out of hangar, buyer wins & claims, 5% tax applied.
    let cores_before_1 = hangar_qty(&ctx, &mut s1, HYPERCORE_TYPE_ID).await;
    let seller_cash_before = cash_balance(&ctx, &mut s1).await;
    let create1 = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(r1)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(8)),
            (PyRep::String("ticketPrice".into()), PyRep::Real(100_000.0)),
        ])],
    )
    .await
    .expect("CreateRaffle 1");
    let raffle1_id = create1.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    let cores_used_1 = create1.dict_get("hypercores_used").and_then(|v| v.as_i64()).unwrap();
    assert!(cores_used_1 >= 1);
    assert_eq!(hangar_qty(&ctx, &mut s1, HYPERCORE_TYPE_ID).await, cores_before_1 - cores_used_1);
    assert!(!hangar_has_item_id(&ctx, &mut s1, r1).await, "Escrowed item must leave creator hangar");

    let buy1 = rpc(&ctx, &mut s2, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raffle1_id), PyRep::Integer(8)])
        .await
        .expect("BuyRandomTickets full");
    assert_eq!(buy1.dict_get("winner_id").and_then(|v| v.as_i64()), Some(buyer_char_id));
    let award = rpc(&ctx, &mut s2, "raffleProxy", "AwardItem", &[PyRep::Integer(raffle1_id)])
        .await
        .expect("AwardItem");
    assert_eq!(award, PyRep::Integer(r1));
    assert!(hangar_has_item_id(&ctx, &mut s2, r1).await, "Winner hangar must receive awarded item");

    // Total pool = 8 * 100,000 = 800,000; 5% HyperNet tax = 40,000; seller payout = +760,000.
    let seller_cash_after = cash_balance(&ctx, &mut s1).await;
    assert!((seller_cash_after - (seller_cash_before + 760_000.0)).abs() < 1.0);

    // 4. Create second 8-node raffle, buy 2 tickets, advance 72h+ -> verify expiry, refund, item return, non-refunded HyperCores.
    let cores_before_2 = hangar_qty(&ctx, &mut s1, HYPERCORE_TYPE_ID).await;
    let create2 = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(r2)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(8)),
            (PyRep::String("ticketPrice".into()), PyRep::Real(100_000.0)),
        ])],
    )
    .await
    .expect("CreateRaffle 2");
    let raffle2_id = create2.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    let cores_used_2 = create2.dict_get("hypercores_used").and_then(|v| v.as_i64()).unwrap();
    let cores_after_create2 = hangar_qty(&ctx, &mut s1, HYPERCORE_TYPE_ID).await;
    assert_eq!(cores_after_create2, cores_before_2 - cores_used_2);

    let buyer_cash_before_2 = cash_balance(&ctx, &mut s2).await;
    rpc(&ctx, &mut s2, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raffle2_id), PyRep::Integer(2)])
        .await
        .expect("BuyRandomTickets partial");
    let buyer_cash_mid = cash_balance(&ctx, &mut s2).await;
    assert!((buyer_cash_mid - (buyer_cash_before_2 - 200_000.0)).abs() < 1.0);

    ctx.advance_secs(72 * 3600 + 10);
    tick_background_cluster(&ctx).await;

    let raf2 = rpc(&ctx, &mut s1, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle2_id)])
        .await
        .expect("GetRaffle expired");
    assert_eq!(raf2.dict_get("is_expired").and_then(|v| v.as_bool()), Some(true));
    assert!(hangar_has_item_id(&ctx, &mut s1, r2).await, "Expired raffle item must return to creator hangar");
    assert_eq!(hangar_qty(&ctx, &mut s1, HYPERCORE_TYPE_ID).await, cores_after_create2, "Consumed HyperCores must NOT be refunded on expiry");
    let buyer_cash_after_exp = cash_balance(&ctx, &mut s2).await;
    assert!((buyer_cash_after_exp - buyer_cash_before_2).abs() < 1.0, "Buyer ISK must be refunded on expiry");
}
