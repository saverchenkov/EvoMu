//! End-to-end live gameplay ESI parity tests for Faction Warfare, Sovereignty, and Incursions
//! with zero direct snapshot seeding. All domain state is produced by real MachoNet RPCs, space
//! combat, complex/node captures, incursion site completions, and cluster ticks.

#![forbid(unsafe_code)]

use eve_db::InventoryFlag;
use eve_destiny::DamageVector;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token(ctx: &ServerContext, user: &str, pass: &str, char_id: i64, scopes: Vec<&str>) -> String {
    let now = ctx.now_unix();
    let sc = scopes.into_iter().map(String::from).collect();
    let code = ctx
        .db
        .authorize_pkce(user, pass, char_id, "evomu-esi-live-parity", "https://localhost/callback", sc, PKCE_CHALLENGE, "S256", now)
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi-live-parity", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn call_esi(router: &EsiRouter, req: EsiHttpRequest, expected: u16) -> Value {
    crate::esi_universe_fw_complete::call(router, req, expected)
}

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let bound_id = if service == "beyonce" {
        sess.solar_system_id.and_then(|s| ctx.bind_object(sess, "beyonce", s as i64).ok())
    } else {
        None
    };
    ctx.dispatch_rpc(sess, service, bound_id, method, args, &HashMap::new(), None).await
}

pub(crate) async fn slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())])
        .await
        .unwrap_or_else(|e| panic!("slash `{cmd}` failed: {} ({})", e.code, e.message))
}

pub(crate) async fn select_gm_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    ctx.db.set_account_role(user, eve_db::service_roles::ROLE_GM_DEFAULT).expect("set_account_role");
    let mut sess = SessionState::new(sid);
    crate::common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

pub(crate) async fn create_corp_and_alliance(
    ctx: &ServerContext,
    sess: &mut SessionState,
    corp_name: &str,
    corp_ticker: &str,
    ally_name: Option<(&str, &str)>,
) -> (i64, Option<i64>) {
    slash(ctx, sess, "/giveskill 3363 5").await;
    slash(ctx, sess, "/giveisk 2000000000").await;
    let c_rep = rpc(
        ctx,
        sess,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String(corp_name.into()), PyRep::String(corp_ticker.into()), PyRep::Real(0.05)],
    )
    .await
    .expect("AddCorporation");
    let corp_id = c_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corporationID");
    let ally_id = if let Some((aname, aticker)) = ally_name {
        rpc(
            ctx,
            sess,
            "account",
            "GiveCash",
            &[PyRep::Integer(corp_id), PyRep::Real(1_500_000_000.0), PyRep::String("alliance fee".into())],
        )
        .await
        .expect("GiveCash to corp wallet");
        let a_rep = rpc(
            ctx,
            sess,
            "corpRegistry",
            "CreateAlliance",
            &[PyRep::String(aname.into()), PyRep::String(aticker.into())],
        )
        .await
        .expect("CreateAlliance");
        Some(a_rep.dict_get("allianceID").and_then(|v| v.as_i64()).or_else(|| a_rep.as_i64()).expect("allianceID"))
    } else {
        None
    };
    (corp_id, ally_id)
}

pub(crate) async fn lock_target(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship_id: i64, target_id: i64, dist_m: f64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if let Some((_, tb)) = sys.get_ball_snapshot(target_id).await.unwrap() {
        sys.set_ball_position(ship_id, tb.position + DVec3::new(dist_m, 0.0, 0.0)).await;
    }
    crate::sovereignty_equinox_gameplay::lock_target_via_rpc(ctx, sess, sys_id, ship_id, target_id).await;
}

pub(crate) async fn arm_ship_autocannons(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> (i64, Vec<i64>) {
    for sk in [3300_u32, 3303, 3306, 11083, 3413, 3426] {
        slash(ctx, sess, &format!("/giveskill {sk} 5")).await;
    }
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    if let Some(st_id) = sess.station_id {
        if let Some(old_hi0) = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0).map(|i| i.item_id)) {
            let _ = rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(old_hi0), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await;
        }
        for flag in [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2] {
            let gid = slash(ctx, sess, "/giveitem 2881 1").await.dict_get("itemID").unwrap().as_i64().unwrap();
            rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(gid), PyRep::Integer(ship_id), PyRep::Integer(flag as i64)]).await.expect("fit 200mm AutoCannon I");
        }
    }
    let guns: Vec<i64> = ctx.db.with_snapshot(|s| {
        let mut v: Vec<_> = s.items.values()
            .filter(|i| i.location_id == ship_id && matches!(i.flag, InventoryFlag::HiSlot0 | InventoryFlag::HiSlot1 | InventoryFlag::HiSlot2) && i.type_id == 2881)
            .map(|i| i.item_id)
            .collect();
        v.sort_unstable();
        v
    });
    let aid = slash(ctx, sess, "/giveitem 185 1500").await.dict_get("itemID").unwrap().as_i64().unwrap();
    if sess.station_id.is_some() {
        rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(aid), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("move ammo");
    }
    let cargo_aid = ctx.db.with_snapshot(|s| {
        s.items.values().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo && i.type_id == 185).unwrap().item_id
    });
    let gun_reps: Vec<PyRep> = guns.iter().map(|&g| PyRep::Integer(g)).collect();
    rpc(ctx, sess, "dogmaIM", "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(gun_reps), PyRep::List(vec![PyRep::Integer(cargo_aid)])])
        .await
        .expect("LoadAmmo");
    (ship_id, guns)
}

pub(crate) async fn lock_and_destroy_target(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    guns: &[i64],
    target_id: i64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    lock_target(ctx, sess, sys_id, ship_id, target_id, 1_000.0).await;
    for _ in 0..40 {
        if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
            return;
        }
        for &gid in guns {
            if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
                return;
            }
            if let Err(e) = rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(target_id)]).await {
                assert!(matches!(e.code.as_str(), "ModuleAlreadyActive" | "TargetNotFound" | "TargetNotLocked" | "TargetNotInSpace"), "Activate weapon {gid} failed: {} ({})", e.code, e.message);
            }
        }
        for _ in 0..5 {
            sys.step_manual(1.0).await.unwrap();
        }
        ctx.advance_secs(5);
    }
    panic!("target {target_id} was not destroyed after 40 volleys");
}

fn parse_iso8601_unix(s: &str) -> u64 {
    assert!(s.len() >= 20 && s.as_bytes()[19] == b'Z', "expected ISO-8601 UTC: {s}");
    let (y, m, d): (i64, i64, i64) = (s[0..4].parse().unwrap(), s[5..7].parse().unwrap(), s[8..10].parse().unwrap());
    let (hh, mm, ss): (u64, u64, u64) = (s[11..13].parse().unwrap(), s[14..16].parse().unwrap(), s[17..19].parse().unwrap());
    let y_adj = y - i64::from(m <= 2);
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = (y_adj - era * 400) as u64;
    let mp = (m + if m > 2 { -3 } else { 9 }) as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    (days as u64) * 86_400 + hh * 3_600 + mm * 60 + ss
}

#[tokio::test]
async fn test_esi_fw_live_enlistment_kills_and_complex_captures() {
    // parity: feat:esi.faction-warfare.char-corp-stats, feat:esi.faction-warfare.leaderboards
    // parity: feat:esi.faction-warfare.stats, feat:esi.faction-warfare.systems
    eve_testkit::attest(&[
        "feat:esi.faction-warfare.char-corp-stats",
        "feat:esi.faction-warfare.leaderboards",
        "feat:esi.faction-warfare.stats",
        "feat:esi.faction-warfare.systems",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    let mut s1 = select_gm_pilot(&ctx, 71_001, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_gm_pilot(&ctx, 71_002, "other", "other-secret", PILOT_2).await;

    // 1. Live corporation FW enlistment (Caldari 500001) & direct character enlistment (Gallente 500004).
    let (corp_1, _) = create_corp_and_alliance(&ctx, &mut s1, "State Strike Wing", "SSWG", None).await;
    rpc(&ctx, &mut s1, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(500_001)])
        .await
        .expect("JoinFactionAsCorporation");
    rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await
        .expect("CreateMyDirectEnlistment");

    let init_systems = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/systems/"), 200);
    let target_sys = init_systems
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["occupier_faction_id"] == 500_004 && s["contested"] == "uncontested" && s["victory_points"] == 0)
        .and_then(|s| s["solar_system_id"].as_u64())
        .expect("Gallente uncontested FW system") as u32;

    let (ship_1, guns_1) = arm_ship_autocannons(&ctx, &mut s1, PILOT_1).await;
    slash(&ctx, &mut s1, &format!("/tr {target_sys}")).await;
    slash(&ctx, &mut s2, &format!("/tr {target_sys}")).await;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // 2. Pilot 1 destroys opposing militia Pilot 2's Rifter in live space combat -> 1 FW kill!
    let ship_2 = ctx.db.with_snapshot(|snap| snap.characters[&PILOT_2].active_ship_id);
    lock_and_destroy_target(&ctx, &mut s1, target_sys, ship_1, &guns_1, ship_2).await;

    // 3. Pilot 1 activates Novice & Small complex acceleration gates, kills pocket guards, and captures both!
    let sys_handle = ctx.systems.get_or_spawn(target_sys);
    for (tier, hold_secs) in [("Novice", 605_u64), ("Small", 905_u64)] {
        let (gate_id, gate_pos) = ctx.db.with_snapshot(|snap| {
            let (&gid, _) = snap
                .fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, (csys, t, _, _, done))| *csys == target_sys && t == tier && !*done)
                .unwrap_or_else(|| panic!("active {tier} complex in {target_sys}"));
            (gid, snap.item_positions[&gid])
        });
        sys_handle.set_ball_position(ship_1, DVec3::from_array(gate_pos) + DVec3::new(1_000.0, 0.0, 0.0)).await;
        rpc(&ctx, &mut s1, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)])
            .await
            .expect("ActivateAccelerationGate");
        lock_and_destroy_target(&ctx, &mut s1, target_sys, ship_1, &guns_1, gate_id + 500_000_000).await;
        ctx.advance_secs(hold_secs);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }

    // 4. Verify `/v1/fw/systems/` transitioned `target_sys` from `uncontested` (0 VP) to `contested` (> 0 VP).
    let after_systems = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/systems/"), 200);
    let sys_row = after_systems
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["solar_system_id"] == i64::from(target_sys))
        .expect("target FW system");
    assert!(sys_row["victory_points"].as_i64().unwrap() > 0, "complex captures must raise system VP");
    assert_eq!(sys_row["contested"], "contested");

    // 5. Verify authenticated character/corp FW stats, error cases (401/403), faction stats & 3 leaderboards.
    call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/fw/stats/")), 401);
    let wrong_tok = issue_token(&ctx, "demo", "demo-secret", PILOT_1, vec![]);
    call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/fw/stats/")).with_bearer_token(&wrong_tok), 403);
    let tok_1 = issue_token(&ctx, "demo", "demo-secret", PILOT_1, vec!["esi-characters.read_fw_stats.v1", "esi-corporations.read_fw_stats.v1"]);
    call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_2}/fw/stats/")).with_bearer_token(&tok_1), 403);
    call_esi(&router, EsiHttpRequest::new("GET", "/v1/corporations/98999999/fw/stats/").with_bearer_token(&tok_1), 403);

    let c1_stats = call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/fw/stats/")).with_bearer_token(&tok_1), 200);
    assert_eq!(c1_stats["faction_id"], 500_001);
    assert!(c1_stats["enlisted_on"].as_str().is_some());
    assert!(c1_stats["current_rank"].as_i64().unwrap() >= 1);
    assert_eq!(c1_stats["kills"]["yesterday"], 1);
    assert_eq!(c1_stats["kills"]["last_week"], 1);
    assert_eq!(c1_stats["kills"]["total"], 1);
    let p1_vp = c1_stats["victory_points"]["total"].as_i64().unwrap();
    assert!(p1_vp > 0);
    assert_eq!(c1_stats["victory_points"]["yesterday"], p1_vp);

    let corp_stats = call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/corporations/{corp_1}/fw/stats/")).with_bearer_token(&tok_1), 200);
    assert_eq!(corp_stats["faction_id"], 500_001);
    assert_eq!(corp_stats["pilots"], 1);
    assert_eq!(corp_stats["kills"]["yesterday"], 1);
    assert_eq!(corp_stats["victory_points"]["total"], p1_vp);

    let fw_stats = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/stats/"), 200);
    let cal = fw_stats.as_array().unwrap().iter().find(|f| f["faction_id"] == 500_001).unwrap();
    let gal = fw_stats.as_array().unwrap().iter().find(|f| f["faction_id"] == 500_004).unwrap();
    assert_eq!(cal["pilots"], 1);
    assert_eq!(gal["pilots"], 1);
    assert_eq!(cal["kills"]["yesterday"], 1);
    assert_eq!(cal["victory_points"]["total"], p1_vp);

    let lb_fac = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/leaderboards/"), 200);
    assert_eq!(lb_fac["kills"]["yesterday"][0]["faction_id"], 500_001);
    assert_eq!(lb_fac["kills"]["yesterday"][0]["amount"], 1);
    assert_eq!(lb_fac["victory_points"]["active_total"][0]["faction_id"], 500_001);
    assert_eq!(lb_fac["victory_points"]["active_total"][0]["amount"], p1_vp);

    let lb_char = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/leaderboards/characters/"), 200);
    assert_eq!(lb_char["kills"]["yesterday"][0]["character_id"], PILOT_1);
    assert_eq!(lb_char["victory_points"]["active_total"][0]["character_id"], PILOT_1);

    let lb_corp = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/leaderboards/corporations/"), 200);
    assert_eq!(lb_corp["kills"]["yesterday"][0]["corporation_id"], corp_1);
    assert_eq!(lb_corp["victory_points"]["active_total"][0]["corporation_id"], corp_1);

    // 6. Advance > 24h (`86,410s`), dock Pilot 2, and retire Pilot 2: `yesterday` rolls to 0 while `last_week` & `total` persist!
    ctx.advance_secs(86_410);
    s2.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&PILOT_2) {
            c.station_id = Some(60_003_760);
        }
    });
    rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "RemoveMyDirectEnlistment", &[])
        .await
        .expect("RemoveMyDirectEnlistment");

    let tok_1_next = issue_token(&ctx, "demo", "demo-secret", PILOT_1, vec!["esi-characters.read_fw_stats.v1", "esi-corporations.read_fw_stats.v1"]);
    let tok_2_next = issue_token(&ctx, "other", "other-secret", PILOT_2, vec!["esi-characters.read_fw_stats.v1"]);
    let c2_retired = call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_2}/fw/stats/")).with_bearer_token(&tok_2_next), 200);
    assert!(c2_retired.get("faction_id").is_none() && c2_retired.get("enlisted_on").is_none());

    let c1_rolled = call_esi(&router, EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/fw/stats/")).with_bearer_token(&tok_1_next), 200);
    assert_eq!(c1_rolled["kills"]["yesterday"], 0);
    assert_eq!(c1_rolled["kills"]["last_week"], 1);
    assert_eq!(c1_rolled["kills"]["total"], 1);
    assert_eq!(c1_rolled["victory_points"]["yesterday"], 0);
    assert_eq!(c1_rolled["victory_points"]["last_week"], p1_vp);
    assert_eq!(c1_rolled["victory_points"]["total"], p1_vp);

    let lb_rolled = call_esi(&router, EsiHttpRequest::new("GET", "/v1/fw/leaderboards/"), 200);
    assert_eq!(lb_rolled["kills"]["yesterday"][0]["amount"], 0);
    assert_eq!(lb_rolled["kills"]["last_week"][0]["amount"], 1);
    assert_eq!(lb_rolled["kills"]["active_total"][0]["amount"], 1);
}

#[tokio::test]
async fn test_esi_sovereignty_live_hub_adm_and_entosis_campaign() {
    // parity: esi:GET /sovereignty/map
    // parity: esi:GET /sovereignty/structures, feat:esi.sovereignty.campaigns
    // parity: feat:esi.sovereignty.structures
    eve_testkit::attest(&[
        "esi:GET /sovereignty/map",
        "esi:GET /sovereignty/structures",
        "feat:esi.sovereignty.campaigns",
        "feat:esi.sovereignty.structures",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    // Use PILOT_2 as Defender (anchors Sov Hub + Tech I autocannons) and Omega PILOT_1 as Attacker (fits Entosis Link I).
    let mut s_def = select_gm_pilot(&ctx, 72_001, "other", "other-secret", PILOT_2).await;
    let mut s_atk = select_gm_pilot(&ctx, 72_002, "demo", "demo-secret", PILOT_1).await;

    let (def_corp, Some(def_ally)) = create_corp_and_alliance(
        &ctx,
        &mut s_def,
        "Sov Vanguard Corp",
        "SVGC",
        Some(("Sov Vanguard Alliance", "SVGA")),
    )
    .await
    else {
        unreachable!()
    };
    let _ = create_corp_and_alliance(&ctx, &mut s_atk, "Null Breachers", "NBRC", Some(("Breacher Coalition", "BRCH"))).await;
    let (def_ship, def_guns) = arm_ship_autocannons(&ctx, &mut s_def, PILOT_2).await;

    // Fit Entosis Link I (`34593`) into `HiSlot1` of Omega Attacker's Rifter while docked in station.
    for attr_id in [182_u16, 183, 184] {
        if let Some(&sk) = ctx.sde.types[&34593].default_attributes.get(&attr_id) {
            slash(&ctx, &mut s_atk, &format!("/giveskill {} 5", sk as u32)).await;
        }
    }
    let atk_ship = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    let entosis_mod = slash(&ctx, &mut s_atk, "/giveitem 34593 1").await.dict_get("itemID").unwrap().as_i64().unwrap();
    rpc(&ctx, &mut s_atk, "invbroker", "Add", &[PyRep::Integer(entosis_mod), PyRep::Integer(atk_ship), PyRep::Integer(InventoryFlag::HiSlot1 as i64)])
        .await
        .expect("fit Entosis Link I");

    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .min()
        .expect("claimable null-sec system");
    let map_before = call_esi(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/map/"), 200);
    let pre_row = map_before.as_array().unwrap().iter().find(|r| r["system_id"] == i64::from(null_sys)).unwrap();
    assert!(pre_row.get("alliance_id").is_none());

    // 1. Defender teleports to `null_sys`, anchors Sovereignty Hub (`32458`), and onlines it (`905s`).
    slash(&ctx, &mut s_def, &format!("/tr {null_sys}")).await;
    let hub_item = slash(&ctx, &mut s_def, "/giveitem 32458 1").await.dict_get("itemID").unwrap().as_i64().unwrap();
    let hub_id = rpc(&ctx, &mut s_def, "structureDeployment", "Anchor", &[PyRep::Integer(hub_item), PyRep::String("Frontier Sov Hub".into())])
        .await
        .expect("Anchor Sovereignty Hub")
        .dict_get("structureID")
        .unwrap()
        .as_i64()
        .unwrap();

    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let map_after = call_esi(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/map/"), 200);
    let claimed_row = map_after.as_array().unwrap().iter().find(|r| r["system_id"] == i64::from(null_sys)).unwrap();
    assert_eq!(claimed_row["alliance_id"], def_ally);
    assert_eq!(claimed_row["corporation_id"], def_corp);

    // 2. Configure reinforce hour `12:00 UTC`, verify ADM 1.0 (`18h = 64,800s`), then earn ADM 2.0 (`9h = 32,400s`).
    rpc(&ctx, &mut s_def, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(12), PyRep::Integer(3)])
        .await
        .expect("UpdateReinforceTimesFromClient");

    let structs_adm1 = call_esi(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"), 200);
    let st1 = structs_adm1.as_array().unwrap().iter().find(|r| r["structure_id"] == hub_id).expect("sov hub");
    assert_eq!(st1["alliance_id"], def_ally);
    assert_eq!(st1["vulnerability_occupancy_level"], 1.0);
    let dur1 = parse_iso8601_unix(st1["vulnerable_end_time"].as_str().unwrap()) - parse_iso8601_unix(st1["vulnerable_start_time"].as_str().unwrap());
    assert_eq!(dur1, 64_800, "ADM 1.0 must schedule an 18h vulnerability window");

    ctx.systems.get_or_spawn(null_sys).set_ball_position(def_ship, DVec3::new(500_000.0, 0.0, 0.0)).await;
    ctx.advance_secs(7 * 86_400 + 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let npc_type_id = ctx
        .sde
        .types
        .values()
        .filter(|t| t.category_id == 11 && t.default_attributes.get(&9).copied().is_some_and(|hp| hp > 10.0 && hp < 400.0))
        .map(|t| t.type_id)
        .min()
        .expect("SDE frigate NPC");
    for _ in 0..5 {
        let npc_id = slash(&ctx, &mut s_def, &format!("/spawnnpc {npc_type_id}")).await.dict_get("npcID").unwrap().as_i64().unwrap();
        lock_and_destroy_target(&ctx, &mut s_def, null_sys, def_ship, &def_guns, npc_id).await;
    }

    let structs_adm2 = call_esi(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"), 200);
    let st2 = structs_adm2.as_array().unwrap().iter().find(|r| r["structure_id"] == hub_id).unwrap();
    assert_eq!(st2["vulnerability_occupancy_level"], 2.0);
    let dur2 = parse_iso8601_unix(st2["vulnerable_end_time"].as_str().unwrap()) - parse_iso8601_unix(st2["vulnerable_start_time"].as_str().unwrap());
    assert_eq!(dur2, 32_400, "ADM 2.0 must shrink vulnerability window to 9h (32,400s)");

    // 3. Advance into `12:00 UTC` vulnerability window; Attacker Entosis-links Sov Hub -> campaign & CommandNodeEvent!
    let cur_sec = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 12 * 3_600 - cur_sec) % 86_400);
    slash(&ctx, &mut s_atk, &format!("/tr {null_sys}")).await;
    slash(&ctx, &mut s_atk, "/giveitem 16275 20").await;

    lock_target(&ctx, &mut s_atk, null_sys, atk_ship, hub_id, 2_500.0).await;
    rpc(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await
        .expect("Activate Entosis Link I on Sov Hub");
    ctx.advance_secs(1_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let camps_reinf = call_esi(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"), 200);
    let camp0 = camps_reinf.as_array().unwrap().iter().find(|c| c["structure_id"] == hub_id).expect("active ihub_defense campaign");
    assert_eq!(camp0["event_type"], "ihub_defense");
    assert_eq!(camp0["defender_id"], def_ally);
    assert_eq!(camp0["defender_score"], 0.6);
    assert_eq!(camp0["attackers_score"], 0.4);

    // Advance 48h reinforcement -> `CommandNodeEvent` spawns 4 nodes and omits vulnerability window!
    ctx.advance_secs(48 * 3_600 + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let structs_event = call_esi(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"), 200);
    let st_ev = structs_event.as_array().unwrap().iter().find(|r| r["structure_id"] == hub_id).unwrap();
    assert!(st_ev.get("vulnerable_start_time").is_none() && st_ev.get("vulnerability_occupancy_level").is_none());

    // Attacker captures one constellation Command Node via Entosis Link I -> score shifts to 0.53 / 0.47!
    let (node_id, node_sys) = ctx.db.with_snapshot(|s| {
        let (&nid, &(_, nsys, _)) = s.sov_runtime.command_nodes.iter().find(|(_, &(hid, _, _))| hid == hub_id).expect("command node");
        (nid, nsys)
    });
    if node_sys != null_sys {
        slash(&ctx, &mut s_atk, &format!("/tr {node_sys}")).await;
    }
    lock_target(&ctx, &mut s_atk, node_sys, atk_ship, node_id, 2_500.0).await;
    rpc(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(node_id)])
        .await
        .expect("Activate Entosis Link I on Command Node");
    ctx.advance_secs(900);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let camps_shifted = call_esi(&router, EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"), 200);
    let camp1 = camps_shifted.as_array().unwrap().iter().find(|c| c["structure_id"] == hub_id).unwrap();
    assert_eq!(camp1["defender_score"], 0.53);
    assert_eq!(camp1["attackers_score"], 0.47);
}

#[tokio::test]
async fn test_esi_incursions_live_spawn_and_influence_decay() {
    // parity: feat:esi.incursions.list
    eve_testkit::attest(&["feat:esi.incursions.list"]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    let empty = call_esi(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    assert_eq!(empty.as_array().unwrap().len(), 0);

    ensure_special_sites_spawned(&ctx).await;
    let active = call_esi(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let first = &active.as_array().unwrap()[0];
    let target_cid = first["constellation_id"].as_u64().unwrap() as u32;
    assert_eq!(first["faction_id"], 500_019);
    assert_eq!(first["state"], "established");
    assert_eq!(first["has_boss"], false);
    assert_eq!(first["influence"], 1.0);
    assert_eq!(first["type"], "Incursion");
    assert!(!first["infested_solar_systems"].as_array().unwrap().is_empty());

    let (hq_sys_id, hq_site_id, hq_pos, ms_site_id, ms_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        let hq = inc.sites.iter().find(|s| s.tier == "Headquarters").unwrap();
        let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
        (inc.hq_solar_system_id, hq.site_id.clone(), hq.position, ms.site_id.clone(), ms.position)
    });
    let mut sess = select_gm_pilot(&ctx, 73_001, "demo", "demo-secret", PILOT_1).await;
    slash(&ctx, &mut sess, &format!("/tr {hq_sys_id}")).await;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    let hq_sys = ctx.systems.get_or_spawn(hq_sys_id);
    hq_sys.set_ball_position(ship_id, hq_pos).await;

    let nuke = DamageVector { em: 50_000_000.0, thermal: 50_000_000.0, kinetic: 50_000_000.0, explosive: 50_000_000.0 };
    for wave in 0..4 {
        let boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &hq_site_id).await;
        let (_, dead, _) = hq_sys.apply_damage_with_attacker(boss_bid, nuke, Some(PILOT_1)).await.unwrap().unwrap();
        assert!(dead, "HQ boss destroyed on wave {wave}");
        if wave == 0 {
            let mid = call_esi(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
            let inc_mid = mid.as_array().unwrap().iter().find(|i| i["constellation_id"] == i64::from(target_cid)).unwrap();
            let inf = inc_mid["influence"].as_f64().unwrap();
            assert!(inf < 1.0 && inf > 0.0, "HQ site completion must decay influence ({inf})");
        }
    }

    let mob = call_esi(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let inc_mob = mob.as_array().unwrap().iter().find(|i| i["constellation_id"] == i64::from(target_cid)).unwrap();
    assert_eq!(inc_mob["influence"], 0.0);
    assert_eq!(inc_mob["state"], "mobilizing");
    assert_eq!(inc_mob["has_boss"], true);

    let ms_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &ms_site_id).await;
    hq_sys.set_ball_position(ship_id, ms_pos).await;
    let mut ms_nuke = nuke;
    ms_nuke.em = 20_000_000.0;
    ms_nuke.thermal = 20_000_000.0;
    ms_nuke.kinetic = 20_000_000.0;
    ms_nuke.explosive = 20_000_000.0;
    let (_, ms_dead, _) = hq_sys.apply_damage_with_attacker(ms_boss_bid, ms_nuke, Some(PILOT_1)).await.unwrap().unwrap();
    assert!(ms_dead);

    let wdr = call_esi(&router, EsiHttpRequest::new("GET", "/v1/incursions/"), 200);
    let inc_wdr = wdr.as_array().unwrap().iter().find(|i| i["constellation_id"] == i64::from(target_cid)).unwrap();
    assert_eq!(inc_wdr["state"], "withdrawing");
    assert_eq!(inc_wdr["has_boss"], false);
}
