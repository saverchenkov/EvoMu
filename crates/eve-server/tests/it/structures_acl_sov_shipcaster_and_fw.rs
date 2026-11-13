//! Integration tests covering Upwell structure combat/fitting/ACL enforcement, V24 ownerGroup
//! membership roles, POS forcefield entry & password clearing, Equinox Sovereignty & Entosis
//! campaign mechanics, Shipcaster combat state preservation, and Faction Warfare militia rules.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::rpc as quasar_rpc;
use eve_db::{corp_roles, InventoryFlag};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_ship_forcefield::can_enter_forcefield;
use eve_server::services::sov_services::{TYPE_MAGMATIC_GAS, TYPE_SUPERIONIC_ICE};
use glam::DVec3;
use std::collections::HashMap;

const UPGRADE_ADVANCED_LOGISTICS_NETWORK: u32 = 81_621;

const JITA_44: i64 = 60_003_760;
const PERIMETER_SYS: u32 = 30_000_144;
const VBPT_T: u32 = 30_000_806;
const TTP_2B: u32 = 30_000_812;
const TURNUR_SYS: u32 = 30_002_086;
const ZARZAKH_SYS: u32 = 30_100_000;
const ZARZAKH_LAUNCHER_ID: i64 = 105_000_011;
const ZARZAKH_PAD_ID: i64 = 104_000_030;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const P1_SHIP: i64 = 100_000_001;
const P2_SHIP: i64 = 100_000_002;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let bound_id = if svc == "dogmaIM" {
        sess.station_id
            .or(sess.structure_id)
            .or(sess.ship_id)
            .and_then(|loc| ctx.bind_object(sess, "dogmaIM", loc).ok())
    } else if svc == "beyonce" {
        sess.solar_system_id.and_then(|s| ctx.bind_object(sess, "beyonce", s as i64).ok())
    } else {
        None
    };
    ctx.dispatch_rpc(sess, svc, bound_id, method, args, &HashMap::new(), None).await
}

async fn slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())])
        .await
        .unwrap_or_else(|e| panic!("SlashCmd({cmd}) failed: {e:?}"))
}

fn secret_for(username: &str) -> &'static str {
    match username {
        "demo" => "demo-secret",
        "other" => "other-secret",
        "out93" => "out93-secret",
        _ => panic!("unknown test account {username}"),
    }
}

async fn refresh_session(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) {
    ctx.advance_secs(12);
    rpc(ctx, sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID refresh");
    ctx.advance_secs(12);
}

async fn relogin_with_role(ctx: &ServerContext, sess: &mut SessionState, username: &str, char_id: i64, role: u64) {
    ctx.db.set_account_role(username, role).unwrap();
    crate::common::login_pilot(ctx, sess, username, secret_for(username)).await;
    refresh_session(ctx, sess, char_id).await;
}

async fn grant_skills_and_isk(ctx: &ServerContext, sess: &mut SessionState, username: &str, char_id: i64, skills: &[u32], isk: u64) {
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_GM_DEFAULT).await;
    let uid = sess.user_id.expect("user_id");
    let omega_until = ctx.now_unix() + 90 * 86_400;
    slash(ctx, sess, &format!("/setomega {uid} {omega_until}")).await;
    slash(ctx, sess, &format!("/giveisk {isk}")).await;
    for &sk in skills {
        slash(ctx, sess, &format!("/giveskill {sk} 5")).await;
    }
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_PLAYER_DEFAULT).await;
}

async fn create_item_as_gm(ctx: &ServerContext, sess: &mut SessionState, username: &str, char_id: i64, type_id: u32, qty: i32) -> i64 {
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_GM_DEFAULT).await;
    let rep = slash(ctx, sess, &format!("/createitem {type_id} {qty}")).await;
    let item_id = rep.dict_get("itemID").and_then(|v| v.as_i64()).expect("createitem itemID");
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_PLAYER_DEFAULT).await;
    item_id
}

async fn gm_tr(ctx: &ServerContext, sess: &mut SessionState, username: &str, char_id: i64, sys_id: u32) {
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_GM_DEFAULT).await;
    slash(ctx, sess, &format!("/tr {sys_id}")).await;
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_PLAYER_DEFAULT).await;
    settle_warp(ctx, sess.solar_system_id.unwrap(), sess.ship_id.unwrap()).await;
}

async fn settle_warp(ctx: &ServerContext, sys_id: u32, ship_id: i64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..12 {
        sys.step_manual(10.0).await.unwrap();
        if sys.get_ball_snapshot(ship_id).await.unwrap().is_some_and(|(_, b)| !matches!(b.mode, MovementMode::Warp { .. })) {
            break;
        }
    }
    let _ = sys.set_movement_mode(ship_id, MovementMode::Stop).await;
}

async fn lock_target(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship_id: i64, target_id: i64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) {
        return;
    }
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await.expect("dogmaIM.AddTarget");
    for _ in 0..10 {
        if sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) {
            return;
        }
        sys.step_manual(5.0).await.unwrap();
    }
    assert!(sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked));
}

fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push(((v as u8) & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn encode_varint_field(field: u32, v: u64, out: &mut Vec<u8>) {
    encode_varint(u64::from(field) << 3, out);
    encode_varint(v, out);
}

fn encode_bytes_field(field: u32, bytes: &[u8], out: &mut Vec<u8>) {
    encode_varint((u64::from(field) << 3) | 2, out);
    encode_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn idm(v: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, v, &mut b);
    b
}

async fn fuel_and_online_ansiblex_hub(
    ctx: &ServerContext,
    sess: &mut SessionState,
    username: &str,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    hub_id: i64,
) {
    // Anchor Orbital Skyhooks on resource planets (mint items once while before positioning ship).
    let planets: Vec<_> = ctx.sde.celestials_in_system(sys_id)
        .into_iter()
        .filter(|c| c.group_id == 7 && eve_server::services::quasar_sov_hub::planet_yield(c.type_id) != (0, 0))
        .collect();
    if !planets.is_empty() {
        let sh_stack = create_item_as_gm(ctx, sess, username, char_id, 81_080, planets.len() as i32).await;
        settle_warp(ctx, sys_id, ship_id).await;
        for cel in &planets {
            ctx.systems.get_or_spawn(sys_id).set_ball_position(ship_id, cel.position + DVec3::new(150_000.0, 0.0, 0.0)).await;
            rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(sh_stack), PyRep::Integer(cel.celestial_id)])
                .await
                .expect("Anchor Orbital Skyhook");
        }
    }
    let ice_id = create_item_as_gm(ctx, sess, username, char_id, TYPE_SUPERIONIC_ICE, 5_000).await;
    let gas_id = create_item_as_gm(ctx, sess, username, char_id, TYPE_MAGMATIC_GAS, 5_000).await;
    let wf_mod = create_item_as_gm(ctx, sess, username, char_id, 88_229, 1).await;
    let jb_upg = create_item_as_gm(ctx, sess, username, char_id, UPGRADE_ADVANCED_LOGISTICS_NETWORK, 1).await;
    for (fuel_iid, amt) in [(gas_id as u64, 3_000u64), (ice_id as u64, 3_000u64)] {
        let mut req = Vec::new();
        encode_bytes_field(1, &idm(fuel_iid), &mut req);
        encode_varint_field(2, amt, &mut req);
        encode_bytes_field(3, &idm(hub_id as u64), &mut req);
        let resp = quasar_rpc(ctx, sess, "eve_public.sovereignty.hub.fuel.api.AddRequest", req).await;
        assert_eq!(resp.status_code, 200, "hub.fuel.api.AddRequest failed: {resp:?}");
    }
    let mut proc_req = Vec::new();
    for iid in [wf_mod as u64, jb_upg as u64] {
        encode_bytes_field(1, &idm(iid), &mut proc_req);
    }
    for tid in [88_229u64, UPGRADE_ADVANCED_LOGISTICS_NETWORK as u64] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut cfg);
        encode_varint_field(2, 1, &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc_req);
    }
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc_req);
    let resp = quasar_rpc(ctx, sess, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc_req).await;
    assert_eq!(resp.status_code, 200, "hub.upgrade.api.ProcessConfigurationRequest failed: {resp:?}");
}

async fn hack_board_to_win(ctx: &ServerContext, sess: &mut SessionState, target_id: i64, grid_size: u32) -> Option<PyRep> {
    use eve_server::services::pve_hacking_sites::are_hacking_tiles_adjacent;
    use std::collections::HashSet;

    let mut revealed: HashSet<u32> = HashSet::from([0]);
    let mut non_blocking: HashSet<u32> = HashSet::from([0]);
    let mut tile_dist: HashMap<u32, i64> = HashMap::from([(0, 3)]);
    let mut blockers: HashMap<u32, (String, i64)> = HashMap::new();
    let mut core_tile: Option<(u32, i64)> = None;
    let mut next_util_slot: i64 = 0;
    let mut held_slots: Vec<i64> = Vec::new();
    let mut last_eff_str: i64 = 20;

    for _step in 0..(grid_size * 12) {
        let suppressor = blockers.iter().find(|(_, (k, _))| k == "VirusSuppressor").map(|(&t, _)| t);
        let next_tile = if let Some((ct, _)) = core_tile.filter(|&(_, ccoh)| ccoh <= last_eff_str) {
            ct
        } else if let Some((ct, _)) = core_tile {
            suppressor.unwrap_or(ct)
        } else if let Some(t) = (0..grid_size)
            .filter(|t| !revealed.contains(t) && non_blocking.iter().any(|&adj| are_hacking_tiles_adjacent(adj, *t)))
            .min_by_key(|t| {
                let best_adj = non_blocking
                    .iter()
                    .filter(|&&adj| are_hacking_tiles_adjacent(adj, *t))
                    .filter_map(|adj| tile_dist.get(adj).copied())
                    .min()
                    .unwrap_or(5);
                (best_adj, *t)
            })
        {
            t
        } else if let Some(st) = suppressor {
            st
        } else if let Some((&t, _)) = blockers.iter().filter(|(_, (k, _))| k != "RestorationNode").min_by_key(|(&t, (_, coh))| (*coh, t)) {
            t
        } else if let Some((&t, _)) = blockers.iter().min_by_key(|(&t, (_, coh))| (*coh, t)) {
            t
        } else {
            break;
        };

        if let Some((ct, ccoh)) = core_tile {
            if ct == next_tile && ccoh > last_eff_str {
                if let Some(slot) = held_slots.pop() {
                    let _ = rpc(ctx, sess, "hackingMgr", "UsedUtilityElementOnVirus", &[PyRep::Integer(target_id), PyRep::Integer(slot), PyRep::Integer(i64::from(ct))]).await;
                }
            }
        } else if blockers.contains_key(&next_tile) && suppressor.is_none() {
            if let Some(slot) = held_slots.pop() {
                let _ = rpc(ctx, sess, "hackingMgr", "UsedUtilityElementOnVirus", &[PyRep::Integer(target_id), PyRep::Integer(slot), PyRep::Integer(i64::from(next_tile))]).await;
            }
        }

        let step = match rpc(ctx, sess, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(target_id), PyRep::Integer(i64::from(next_tile))]).await {
            Ok(s) => s,
            Err(e) if e.code == "InvalidTileIndex" => {
                revealed.insert(next_tile);
                continue;
            }
            Err(_) => break,
        };
        revealed.insert(next_tile);
        let status = step.dict_get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status == "Won" {
            return Some(step);
        }
        if status != "InProgress" {
            return None;
        }
        let kind = step.dict_get("nodeKind").and_then(|v| v.as_str()).unwrap_or("");
        let coh = step.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0);
        if let Some(es) = step.dict_get("virusStrength").and_then(|v| v.as_i64()) {
            last_eff_str = es.max(5);
        }
        if let Some(di) = step.dict_get("distanceIndicator").and_then(|v| v.as_i64()) {
            tile_dist.insert(next_tile, di);
        }
        if kind == "SystemCore" && coh > 0 {
            core_tile = Some((next_tile, coh));
        }
        if matches!(kind, "Firewall" | "Antivirus" | "RestorationNode" | "VirusSuppressor") && coh > 0 {
            blockers.insert(next_tile, (kind.to_string(), coh));
        } else {
            blockers.remove(&next_tile);
            non_blocking.insert(next_tile);
        }
        if matches!(kind, "UtilitySubsystem" | "DataCache") && next_util_slot < 3 {
            held_slots.push(next_util_slot);
            next_util_slot += 1;
        }
    }
    None
}

async fn setup_two_pilots_and_corp(ctx: &ServerContext) -> (SessionState, SessionState, i64) {
    let skills = &[
        3_300, 3_303, 3_306, 3_319, 3_320, 3_321, 3_327, 3_363, 3_368, 3_405, 3_413, 3_426,
        3_429, 3_432, 3_436, 3_439, 3_443, 3_453, 3_732, 11_584, 16_622, 21_718, 24_242,
    ];
    let mut s1 = SessionState::new(851);
    crate::common::login_pilot(ctx, &mut s1, "demo", "demo-secret").await;
    rpc(ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)]).await.unwrap();
    grant_skills_and_isk(ctx, &mut s1, "demo", P1, skills, 5_000_000_000).await;

    let mut s2 = SessionState::new(852);
    crate::common::login_pilot(ctx, &mut s2, "other", "other-secret").await;
    rpc(ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)]).await.unwrap();
    grant_skills_and_isk(ctx, &mut s2, "other", P2, skills, 5_000_000_000).await;

    let corp_id = rpc(ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Defense Corp".into()), PyRep::String("UDFC".into())])
        .await.unwrap().dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    refresh_session(ctx, &mut s1, P1).await;
    (s1, s2, corp_id)
}

#[tokio::test]
async fn test_structure_combat_state_ansiblex_acl_and_fitting_dropoff() {
    let ctx = crate::sovereignty_equinox_gameplay::fast_demo_ctx();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2, corp_id) = setup_two_pilots_and_corp(&ctx).await;

    rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(2_000_000_000.0), PyRep::String("Alliance fund".into())])
        .await.expect("fund corp");
    let _ally_id = rpc(
        &ctx, &mut s1, "allianceRegistry", "CreateAlliance",
        &[PyRep::String("Upwell Defense Coalition".into()), PyRep::String("UDFC".into()), PyRep::String("Coalition".into()), PyRep::String("https://udfc.eve".into())],
    ).await.expect("CreateAlliance").dict_get("allianceID").and_then(|v| v.as_i64()).unwrap();
    refresh_session(&ctx, &mut s1, P1).await;

    // Anchor Astrahus in Perimeter after 7-day corp age gate.
    ctx.advance_secs(7 * 86_400 + 30);
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P1");
    ctx.advance_secs(12);
    gm_tr(&ctx, &mut s1, "demo", P1, PERIMETER_SYS).await;
    let _ = create_item_as_gm(&ctx, &mut s1, "demo", P1, 35_832, 1).await;
    let st_id = rpc(&ctx, &mut s1, "structureDeployment", "Anchor", &[PyRep::Integer(35_832), PyRep::String("Citadel Alpha".into())])
        .await.expect("Anchor Citadel Alpha").dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    // AnchorVulnerable structure rejects TakeControl.
    assert!(rpc(&ctx, &mut s1, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.is_err());

    // Transition through Anchoring -> NoCore, dock inside, confirm NoCore rejects TakeControl.
    ctx.advance_secs(901);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await;
    ctx.advance_secs(86_401);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await;

    let st_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&st_id].position));
    ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(P1_SHIP, st_pos).await;
    rpc(&ctx, &mut s1, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect("Dock NoCore st_id");
    assert!(rpc(&ctx, &mut s1, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.is_err());

    // Install Quantum Core, fuel, transition to ShieldVulnerable, and fit Standup service module.
    let core_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, 56_201, 1).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(core_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::StructureDeedBay as i64)])
        .await.expect("Install st_id core");
    let fuel_item = create_item_as_gm(&ctx, &mut s1, "demo", P1, 4_051, 50).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::StructureFuel as i64)])
        .await.expect("Add structure fuel");
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let mod_item = create_item_as_gm(&ctx, &mut s1, "demo", P1, 35_894, 1).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(mod_item), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)])
        .await.expect("Fit ServiceSlot0 module");

    // Deploy Sov Hubs & Ansiblex Jump Gates in nearby sov-null pair VBPT-T <-> TTP-2B (< 5 LY)
    // and link them via V24 `structureDeployment.Anchor` `extraVars["destinationSolarsystemID"]`.
    rpc(&ctx, &mut s1, "structureDocking", "Undock", &[PyRep::Integer(st_id)]).await.expect("Undock st_id");
    ctx.advance_secs(12);
    let mut jb_ids = Vec::new();
    for (sys_id, dest_sys_id, hub_name, jb_name) in [
        (VBPT_T, TTP_2B, "VBPT Hub", "JB VBPT"),
        (TTP_2B, VBPT_T, "TTP Hub", "JB TTP"),
    ] {
        gm_tr(&ctx, &mut s1, "demo", P1, sys_id).await;
        let hub_pkg = create_item_as_gm(&ctx, &mut s1, "demo", P1, 32_458, 1).await;
        let hub_id = rpc(&ctx, &mut s1, "structureDeployment", "Anchor", &[PyRep::Integer(hub_pkg), PyRep::String(hub_name.into())])
            .await.expect("Anchor Sov Hub").dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
        ctx.advance_secs(905);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        fuel_and_online_ansiblex_hub(&ctx, &mut s1, "demo", P1, sys_id, P1_SHIP, hub_id).await;

        let jb_pkg = create_item_as_gm(&ctx, &mut s1, "demo", P1, 35_841, 1).await;
        let jb_fuel = create_item_as_gm(&ctx, &mut s1, "demo", P1, 4_051, 100).await;
        settle_warp(&ctx, sys_id, P1_SHIP).await;
        let deep = DVec3::new(50_000_000.0, 50_000_000.0, 50_000_000.0);
        ctx.systems.get_or_spawn(sys_id).set_ball_position(P1_SHIP, deep).await;
        let extra_vars = PyRep::Dict(vec![(
            PyRep::String("destinationSolarsystemID".into()),
            PyRep::Integer(i64::from(dest_sys_id)),
        )]);
        let jb_id = rpc(
            &ctx,
            &mut s1,
            "structureDeployment",
            "Anchor",
            &[
                PyRep::Integer(jb_pkg),
                PyRep::None,
                PyRep::None,
                PyRep::Real(0.0),
                PyRep::Integer(0),
                PyRep::String(jb_name.into()),
                PyRep::String("".into()),
                PyRep::Integer(2),
                PyRep::Integer(18),
                extra_vars,
            ],
        )
            .await.expect("Anchor Ansiblex").dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
        let jb_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&jb_id].position));
        ctx.systems.get_or_spawn(sys_id).set_ball_position(P1_SHIP, jb_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(jb_fuel), PyRep::Integer(jb_id), PyRep::Integer(InventoryFlag::StructureFuel as i64)])
            .await.expect("Load Ansiblex fuel");
        ctx.advance_secs(15 * 60 + 45 * 60 + 15 * 60 + 10);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        if ctx.db.with_snapshot(|s| s.structures[&jb_id].state == "NoCore") {
            let jb_core = create_item_as_gm(&ctx, &mut s1, "demo", P1, 56_201, 1).await;
            rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(jb_core), PyRep::Integer(jb_id), PyRep::Integer(InventoryFlag::StructureDeedBay as i64)])
                .await.expect("Install Ansiblex core");
            ctx.advance_secs(905);
            eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        }
        jb_ids.push(jb_id);
    }
    let (jb_a, jb_b) = (jb_ids[0], jb_ids[1]);
    let linked_b = rpc(&ctx, &mut s1, "structureJumpBridgeMgr", "GetLinkedStructure", &[PyRep::Integer(jb_a)])
        .await.expect("GetLinkedStructure jb_a");
    let linked_b_id = match linked_b {
        PyRep::Tuple(items) => items.first().and_then(|v| v.as_i64()),
        other => other.as_i64(),
    };
    assert_eq!(linked_b_id, Some(jb_b));

    // Block P2 via real ownerGroup + structureProfiles ACL rules.
    let g_blk = rpc(&ctx, &mut s1, "ownerGroupManager", "Create", &[PyRep::String("Blocked Pilots".into()), PyRep::String("".into())])
        .await.unwrap().dict_get("groupID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "ownerGroupManager", "AddMembers", &[
        PyRep::Integer(g_blk),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(P2), PyRep::Integer(3)])]),
    ]).await.expect("Add P2 EXCLUDED=3");
    rpc(&ctx, &mut s1, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(jb_a),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(g_blk), PyRep::Bool(true)]),
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(g_blk), PyRep::Bool(false)]),
        ]),
    ]).await.expect("Save JB ACL profile");
    rpc(&ctx, &mut s1, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(st_id),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(g_blk), PyRep::Bool(false)])]),
    ]).await.expect("Save Citadel housing block ACL");

    // P2 blocked from Ansiblex navigation queries.
    assert!(rpc(&ctx, &mut s2, "structureJumpBridgeMgr", "GetLinkedStructure", &[PyRep::Integer(jb_a)]).await.is_err());
    assert!(rpc(&ctx, &mut s2, "structureJumpBridgeMgr", "GetJbStructureDestination", &[PyRep::Integer(jb_a)]).await.is_err());

    // P1 docked at Jita cannot jettison st_id fuel; returns NotDockedInStructure.
    gm_tr(&ctx, &mut s1, "demo", P1, 30_000_142).await;
    let jita44_pos = ctx.sde.stations[&JITA_44].position;
    ctx.systems.get_or_spawn(30_000_142).set_ball_position(P1_SHIP, jita44_pos + DVec3::new(200.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(JITA_44)]).await.expect("Dock Jita");
    assert_eq!(
        rpc(&ctx, &mut s1, "structure", "JettisonStructureFuel", &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(fuel_item)])])
            .await.unwrap_err().code,
        "NotDockedInStructure"
    );

    // P2 in Perimeter outside st_id cannot drop off cargo when blocked by ACL.
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P2");
    ctx.advance_secs(12);
    gm_tr(&ctx, &mut s2, "other", P2, PERIMETER_SYS).await;
    let ore_item = create_item_as_gm(&ctx, &mut s2, "other", P2, 1230, 10).await;
    settle_warp(&ctx, PERIMETER_SYS, P2_SHIP).await;
    ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(P2_SHIP, st_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    assert!(rpc(&ctx, &mut s2, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(ore_item)])]).await.is_err());

    // P1 undocks from Jita, returns to Perimeter, docks in st_id, and jettisons fuel -> offlines Standup service module.
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P1");
    ctx.advance_secs(12);
    gm_tr(&ctx, &mut s1, "demo", P1, PERIMETER_SYS).await;
    ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(P1_SHIP, st_pos).await;
    rpc(&ctx, &mut s1, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect("Dock st_id");
    rpc(&ctx, &mut s1, "structure", "JettisonStructureFuel", &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(fuel_item)])])
        .await.expect("Docked JettisonStructureFuel");
    assert_eq!(ctx.db.with_snapshot(|s| s.module_online_states.get(&mod_item).copied()), Some(false));
}

#[tokio::test]
async fn test_ownergroup_v24_roles_and_pos_forcefield_acl_password() {
    let ctx = crate::sovereignty_equinox_gameplay::fast_demo_ctx();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2, corp_id) = setup_two_pilots_and_corp(&ctx).await;

    let p3_outsider = 90_000_093_i64;
    ctx.db.provision_gm_account("out93", "out93-secret", p3_outsider, "Outsider 93");
    crate::sovereignty_equinox_gameplay::preseed_fast_account(
        &ctx,
        "out93",
        "out93-secret",
        eve_db::service_roles::ROLE_GM_DEFAULT,
    );
    let mut s3 = SessionState::new(853);
    crate::common::login_pilot(&ctx, &mut s3, "out93", "out93-secret").await;
    rpc(&ctx, &mut s3, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(p3_outsider)]).await.unwrap();
    grant_skills_and_isk(&ctx, &mut s3, "out93", p3_outsider, &[3_327], 100_000_000).await;

    ctx.advance_secs(7 * 86_400 + 30);
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P1");
    ctx.advance_secs(12);
    gm_tr(&ctx, &mut s1, "demo", P1, PERIMETER_SYS).await;
    let _ = create_item_as_gm(&ctx, &mut s1, "demo", P1, 35_832, 1).await;
    let st_id = rpc(&ctx, &mut s1, "structureDeployment", "Anchor", &[PyRep::Integer(35_832), PyRep::String("ACL Citadel".into())])
        .await.expect("Anchor ACL Citadel").dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(901);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await;
    ctx.advance_secs(86_401);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await;

    let st_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&st_id].position));
    ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(P1_SHIP, st_pos).await;
    rpc(&ctx, &mut s1, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.expect("Dock ACL Citadel");
    let core_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, 56_201, 1).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(core_id), PyRep::Integer(st_id), PyRep::Integer(InventoryFlag::StructureDeedBay as i64)])
        .await.expect("Install ACL Citadel core");
    ctx.advance_secs(901);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(st_id)]).await;

    let g1 = rpc(&ctx, &mut s1, "ownerGroupManager", "Create", &[PyRep::String("Group 1".into()), PyRep::String("".into())])
        .await.unwrap().dict_get("groupID").and_then(|v| v.as_i64()).unwrap();
    let g2 = rpc(&ctx, &mut s1, "ownerGroupManager", "Create", &[PyRep::String("Group 2".into()), PyRep::String("".into())])
        .await.unwrap().dict_get("groupID").and_then(|v| v.as_i64()).unwrap();

    rpc(&ctx, &mut s1, "ownerGroupManager", "AddMembers", &[
        PyRep::Integer(g1),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(P2), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(p3_outsider), PyRep::Integer(3)])]),
    ]).await.expect("AddMembers with MANAGER=1 and EXCLUDED=3");
    rpc(&ctx, &mut s1, "ownerGroupManager", "AddMembers", &[
        PyRep::Integer(g2),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(p3_outsider), PyRep::Integer(3)])]),
    ]).await.unwrap();
    rpc(&ctx, &mut s1, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(st_id),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(g1), PyRep::Bool(true)]),
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(g2), PyRep::Bool(true)]),
        ]),
    ]).await.expect("Assign g1 + g2 docking ACL");
    assert!(ctx.db.with_snapshot(|s| {
        eve_server::services::structure_acl_defense_rules::verify_structure_docking_acl(&ctx, s, st_id, p3_outsider).is_err()
    }));

    assert!(rpc(&ctx, &mut s2, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(g1), PyRep::List(vec![PyRep::Integer(p3_outsider)]), PyRep::Integer(2)]).await.is_err());
    rpc(&ctx, &mut s2, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(g1), PyRep::List(vec![PyRep::Integer(p3_outsider)]), PyRep::Integer(3)])
        .await.expect("UpdateMembershipTypes EXCLUDED=3");

    rpc(&ctx, &mut s1, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(g1), PyRep::List(vec![PyRep::Integer(p3_outsider)])]).await.unwrap();
    assert!(ctx.db.with_snapshot(|s| {
        eve_server::services::structure_acl_defense_rules::verify_structure_docking_acl(&ctx, s, st_id, p3_outsider).is_err()
    }));

    // P2 joins corp_id cleanly with 0 roles; P1 creates Control Tower 12235 and sets password via posMgr.SetTowerPassword.
    let app = rpc(&ctx, &mut s2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("join".into())]).await.expect("InsertApplication");
    let app_id = app.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await.expect("Accept P2");
    refresh_session(&ctx, &mut s2, P2).await;

    let tower_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, 12_235, 1).await;
    rpc(&ctx, &mut s1, "posMgr", "SetTowerPassword", &[PyRep::Integer(tower_id), PyRep::String("secret42".into()), PyRep::Bool(false), PyRep::Bool(false)])
        .await.expect("posMgr.SetTowerPassword");

    assert!(!can_enter_forcefield(&ctx, P2, P2_SHIP, tower_id));
    rpc(&ctx, &mut s2, "ship", "SetShipPassword", &[PyRep::String("secret42".into())]).await.expect("SetShipPassword");
    assert!(can_enter_forcefield(&ctx, P2, P2_SHIP, tower_id));
    rpc(&ctx, &mut s2, "ship", "SetShipPassword", &[PyRep::String("".into())]).await.expect("Clear SetShipPassword");
    assert!(!can_enter_forcefield(&ctx, P2, P2_SHIP, tower_id));
    let _ = corp_roles::ROLE_STARBASE_CONFIG;
}

#[tokio::test]
async fn test_sov_hub_vuln_window_contested_entosis_decap_hacked_recovery_and_cyno_jammer() {
    let ctx = crate::sovereignty_equinox_gameplay::fast_demo_ctx();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2, corp_id) = setup_two_pilots_and_corp(&ctx).await;

    rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(2_000_000_000.0), PyRep::String("Def Alliance".into())])
        .await.expect("fund def corp");
    let _ally_def = rpc(
        &ctx, &mut s1, "allianceRegistry", "CreateAlliance",
        &[PyRep::String("Sov Defenders".into()), PyRep::String("SDEF".into()), PyRep::String("Def".into()), PyRep::String("https://sdef.eve".into())],
    ).await.expect("Create ally_def").dict_get("allianceID").and_then(|v| v.as_i64()).unwrap();
    refresh_session(&ctx, &mut s1, P1).await;

    let atk_corp = rpc(&ctx, &mut s2, "corpRegistry", "AddCorporation", &[PyRep::String("Sov Attackers Corp".into()), PyRep::String("SATC".into())])
        .await.expect("Create atk_corp").dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    refresh_session(&ctx, &mut s2, P2).await;
    rpc(&ctx, &mut s2, "account", "GiveCash", &[PyRep::Integer(atk_corp), PyRep::Real(2_000_000_000.0), PyRep::String("Atk Alliance".into())])
        .await.expect("fund atk corp");
    let _ally_atk = rpc(
        &ctx, &mut s2, "allianceRegistry", "CreateAlliance",
        &[PyRep::String("Sov Attackers".into()), PyRep::String("SATK".into()), PyRep::String("Atk".into()), PyRep::String("https://satk.eve".into())],
    ).await.expect("Create ally_atk").dict_get("allianceID").and_then(|v| v.as_i64()).unwrap();
    refresh_session(&ctx, &mut s2, P2).await;

    let null_sys = ctx.sde.solar_systems.values()
        .find(|s| eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .expect("claimable null-sec system");

    // Fit P1 and P2 while still docked at station (Entosis Link I + Strontium + Data Analyzer II).
    let entosis_def = create_item_as_gm(&ctx, &mut s1, "demo", P1, 34_593, 1).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(entosis_def), PyRep::Integer(P1_SHIP), PyRep::Integer(InventoryFlag::HiSlot1 as i64)])
        .await.expect("Fit Entosis Link I on P1");
    let stront_def = create_item_as_gm(&ctx, &mut s1, "demo", P1, 16_275, 40).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(stront_def), PyRep::Integer(P1_SHIP), PyRep::Integer(InventoryFlag::Cargo as i64)])
        .await.expect("Load Strontium on P1");
    let analyzer_id = create_item_as_gm(&ctx, &mut s2, "other", P2, 22_175, 1).await;
    rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(analyzer_id), PyRep::Integer(P2_SHIP), PyRep::Integer(InventoryFlag::MedSlot0 as i64)])
        .await.expect("Fit Data Analyzer II");
    let entosis_atk = create_item_as_gm(&ctx, &mut s2, "other", P2, 34_593, 1).await;
    rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(entosis_atk), PyRep::Integer(P2_SHIP), PyRep::Integer(InventoryFlag::HiSlot1 as i64)])
        .await.expect("Fit Entosis Link I on P2");
    let stront_atk = create_item_as_gm(&ctx, &mut s2, "other", P2, 16_275, 40).await;
    rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(stront_atk), PyRep::Integer(P2_SHIP), PyRep::Integer(InventoryFlag::Cargo as i64)])
        .await.expect("Load Strontium on P2");

    ctx.advance_secs(7 * 86_400 + 30);
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P1");
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P2");
    ctx.advance_secs(12);

    // P1 anchors real Sov Hub (32458) in null_sys -> ShieldVulnerable & claimed for ally_def.
    gm_tr(&ctx, &mut s1, "demo", P1, null_sys).await;
    let hub_pkg = create_item_as_gm(&ctx, &mut s1, "demo", P1, 32_458, 1).await;
    let hub_id = rpc(&ctx, &mut s1, "structureDeployment", "Anchor", &[PyRep::Integer(hub_pkg), PyRep::String("Sov Hub".into())])
        .await.expect("Anchor Sov Hub").dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let cur_hour = ((ctx.now_unix() % 86_400) / 3_600) as i64;
    rpc(&ctx, &mut s1, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(cur_hour), PyRep::Integer(2)])
        .await.expect("UpdateReinforceTimesFromClient");
    assert!(ctx.db.with_snapshot(|s| eve_server::services::sov_gameplay::is_hub_within_vulnerability_window_in_snap(s, hub_id, ctx.now_unix())));

    // P2 moves near hub_id, hacks hub_id via dogmaIM.Activate + hackingMgr, and tests recovery.
    gm_tr(&ctx, &mut s2, "other", P2, null_sys).await;
    let hub_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&hub_id].position));
    ctx.systems.get_or_spawn(null_sys).set_ball_position(P2_SHIP, hub_pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut s2, null_sys, P2_SHIP, hub_id).await;
    rpc(&ctx, &mut s2, "dogmaIM", "Activate", &[PyRep::Integer(analyzer_id), PyRep::String("dataHacking".into()), PyRep::Integer(hub_id)])
        .await.expect("Activate Data Analyzer II on hub_id");
    let mut hack_won = false;
    for _attempt in 0..5 {
        let start_hack = rpc(&ctx, &mut s2, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(hub_id)]).await.expect("StartNewGameInstance on Sov Hub");
        let grid_size = start_hack.dict_get("gridSize").and_then(|v| v.as_i64()).unwrap_or(20) as u32;
        if hack_board_to_win(&ctx, &mut s2, hub_id, grid_size).await.is_some() {
            hack_won = true;
            break;
        }
    }
    assert!(hack_won, "Data Analyzer hack must succeed on Sov Hub");
    assert!(ctx.db.with_snapshot(|s| s.sov_runtime.hacked_hubs.contains_key(&hub_id)));
    ctx.advance_secs(3_605);
    eve_server::services::sov_gameplay::tick_sov_gameplay(&ctx, ctx.now_unix()).await;
    assert!(!ctx.db.with_snapshot(|s| s.sov_runtime.hacked_hubs.contains_key(&hub_id)));

    // P2 activates Entosis Link I on hub_id to reinforce it into HullReinforced (300s warmup + 600s capture).
    let cur_h_ent = ((ctx.now_unix() % 86_400) / 3_600) as i64;
    rpc(&ctx, &mut s1, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(cur_h_ent), PyRep::Integer(2)])
        .await.expect("Keep vuln window active for hub entosis");
    lock_target(&ctx, &mut s2, null_sys, P2_SHIP, hub_id).await;
    rpc(&ctx, &mut s2, "dogmaIM", "Activate", &[PyRep::Integer(entosis_atk), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("Activate Entosis on hub_id");
    ctx.advance_secs(910);
    eve_server::services::sov_gameplay::tick_sov_gameplay(&ctx, ctx.now_unix()).await;
    let _ = rpc(&ctx, &mut s2, "dogmaIM", "Deactivate", &[PyRep::Integer(entosis_atk), PyRep::String("default".into())]).await;

    // Advance past reinforced timer into next vulnerability window so tick_sov_gameplay naturally spawns Command Nodes.
    let reinf_until = ctx.db.with_snapshot(|s| s.structures[&hub_id].reinforced_until_unix);
    let now_u = ctx.now_unix();
    if reinf_until > now_u {
        ctx.advance_secs(reinf_until - now_u + 10);
    }
    let cur_h_after = ((ctx.now_unix() % 86_400) / 3_600) as i64;
    rpc(&ctx, &mut s1, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(cur_h_after), PyRep::Integer(2)])
        .await.expect("Keep vuln window active for node spawn");
    eve_server::services::sov_gameplay::tick_sov_gameplay(&ctx, ctx.now_unix()).await;

    let (node_id, node_sys, node_pos) = ctx.db.with_snapshot(|s| {
        let (&nid, &(_, nsys, _)) = s.sov_runtime.command_nodes.iter().find(|(_, &(hid, _, _))| hid == hub_id).expect("naturally spawned command node");
        (nid, nsys, DVec3::from_array(s.item_positions[&nid]))
    });

    // Attacker builds progress on Command Node via real Entosis Link I activation (300s warmup + 120s capture < 240s).
    if node_sys != null_sys {
        gm_tr(&ctx, &mut s2, "other", P2, node_sys).await;
    }
    ctx.systems.get_or_spawn(node_sys).set_ball_position(P2_SHIP, node_pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut s2, node_sys, P2_SHIP, node_id).await;
    rpc(&ctx, &mut s2, "dogmaIM", "Activate", &[PyRep::Integer(entosis_atk), PyRep::String("default".into()), PyRep::Integer(node_id)])
        .await.expect("Attacker activate Entosis on Command Node");
    ctx.advance_secs(420);
    eve_server::services::sov_gameplay::tick_sov_gameplay(&ctx, ctx.now_unix()).await;

    // Defender P1 travels to Command Node and contests -> progress must stall.
    gm_tr(&ctx, &mut s1, "demo", P1, node_sys).await;
    ctx.systems.get_or_spawn(node_sys).set_ball_position(P1_SHIP, node_pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut s1, node_sys, P1_SHIP, node_id).await;
    rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(entosis_def), PyRep::String("default".into()), PyRep::Integer(node_id)])
        .await.expect("Defender activate Entosis on Command Node");

    let prog_contested_before = ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress[&node_id].3);
    assert!(prog_contested_before > 10.0);
    ctx.advance_secs(10);
    eve_server::services::sov_gameplay::tick_sov_gameplay(&ctx, ctx.now_unix()).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress[&node_id].3), prog_contested_before);

    // Attacker deactivates Entosis -> defender decap decays progress by 10s.
    rpc(&ctx, &mut s2, "dogmaIM", "Deactivate", &[PyRep::Integer(entosis_atk), PyRep::String("default".into())])
        .await.expect("Attacker deactivate Entosis");
    ctx.advance_secs(10);
    eve_server::services::sov_gameplay::tick_sov_gameplay(&ctx, ctx.now_unix()).await;
    ctx.db.with_snapshot(|s| {
        assert!(s.sov_runtime.command_nodes.contains_key(&node_id));
        assert!((s.sov_runtime.entosis_capture_progress[&node_id].3 - (prog_contested_before - 10.0)).abs() < 1e-6);
    });

    // Mobile Tractor Unit (33700) does not jam cynos; Mobile Cyno Inhibitor (33476) jams cynos.
    gm_tr(&ctx, &mut s1, "demo", P1, null_sys).await;
    ctx.systems.get_or_spawn(null_sys).set_ball_position(P1_SHIP, DVec3::new(20_000_000.0, 0.0, 0.0)).await;
    let mtu_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, 33_700, 1).await;
    rpc(&ctx, &mut s1, "posMgr", "AnchorStructure", &[PyRep::Integer(mtu_id)]).await.expect("Anchor MTU");
    let jam_mtu = rpc(&ctx, &mut s1, "beyonce", "GetCynoJammerState", &[]).await.unwrap();
    assert_eq!(jam_mtu, PyRep::None);

    let inhib_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, 33_476, 1).await;
    rpc(&ctx, &mut s1, "posMgr", "AnchorStructure", &[PyRep::Integer(inhib_id)]).await.expect("Anchor Cyno Inhibitor");
    let jam_inhib = rpc(&ctx, &mut s1, "beyonce", "GetCynoJammerState", &[]).await.unwrap();
    let now_ticks = ctx.now_blueos_filetime_ticks();
    assert!(matches!(jam_inhib, PyRep::Integer(ts) if ts > 0 && ts <= now_ticks), "expected active inhibitor timestamp <= {now_ticks}, got {jam_inhib:?}");
}

#[tokio::test]
async fn test_shipcaster_preserves_combat_damage_and_fw_militia_invariants() {
    let ctx = crate::sovereignty_equinox_gameplay::fast_demo_ctx();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2, corp_id) = setup_two_pilots_and_corp(&ctx).await;

    // Fit modules while docked: P1 fits 1MN Afterburner I; P2 loads EMP S ammo into pre-fitted 200mm AutoCannon II.
    let ab_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, 439, 1).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(ab_id), PyRep::Integer(P1_SHIP), PyRep::Integer(InventoryFlag::MedSlot0 as i64)])
        .await.expect("Fit 1MN AB I");
    let p2_gun = ctx.db.with_snapshot(|s| {
        s.items.values().find(|i| i.location_id == P2_SHIP && i.flag == InventoryFlag::HiSlot0).map(|i| i.item_id).expect("P2 fitted HiSlot0 gun")
    });
    let ammo_id = create_item_as_gm(&ctx, &mut s2, "other", P2, 185, 500).await;
    rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(ammo_id), PyRep::Integer(P2_SHIP), PyRep::Integer(InventoryFlag::HiSlot0 as i64)])
        .await.expect("Load EMP S");
    rpc(&ctx, &mut s2, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.expect("SetSafetyLevel Red");

    // Both pilots undock, teleport to Turnur (30002086) near Deathless Shipcaster launcher (105000011).
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P1");
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P2");
    ctx.advance_secs(12);
    gm_tr(&ctx, &mut s1, "demo", P1, TURNUR_SYS).await;
    gm_tr(&ctx, &mut s2, "other", P2, TURNUR_SYS).await;

    let launcher_pos = DVec3::new(250_000.0, 0.0, 0.0);
    let turnur = ctx.systems.get_or_spawn(TURNUR_SYS);
    turnur.set_ball_position(P1_SHIP, launcher_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    turnur.set_ball_position(P2_SHIP, launcher_pos + DVec3::new(1_800.0, 0.0, 0.0)).await;

    rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(ab_id), PyRep::String("default".into())])
        .await.expect("Activate AB");
    lock_target(&ctx, &mut s2, TURNUR_SYS, P2_SHIP, P1_SHIP).await;
    rpc(&ctx, &mut s2, "dogmaIM", "Activate", &[PyRep::Integer(p2_gun), PyRep::String("default".into()), PyRep::Integer(P1_SHIP)])
        .await.expect("Fire 200mm AC II on P1");

    for _ in 0..15 {
        turnur.step_manual(3.0).await.unwrap();
        let cs = turnur.get_combat_state(P1_SHIP).await.unwrap().expect("P1 combat state");
        if cs.shield_hp < cs.max_shield_hp * 0.9 && cs.armor_hp < cs.max_armor_hp * 0.9 && cs.capacitor < cs.max_capacitor * 0.95 {
            break;
        }
    }
    let _ = rpc(&ctx, &mut s1, "dogmaIM", "Deactivate", &[PyRep::Integer(ab_id), PyRep::String("default".into())]).await;
    let _ = rpc(&ctx, &mut s2, "dogmaIM", "Deactivate", &[PyRep::Integer(p2_gun), PyRep::String("default".into())]).await;

    let pre_cs = turnur.get_combat_state(P1_SHIP).await.unwrap().expect("pre-jump combat state");
    assert!(pre_cs.shield_hp < pre_cs.max_shield_hp && pre_cs.armor_hp < pre_cs.max_armor_hp && pre_cs.capacitor < pre_cs.max_capacitor);

    ctx.advance_secs(65);
    rpc(&ctx, &mut s1, "shipcasterTravelMgr", "CmdJumpThroughShipcaster", &[
        PyRep::Integer(ZARZAKH_LAUNCHER_ID), PyRep::Integer(ZARZAKH_SYS as i64), PyRep::Integer(ZARZAKH_PAD_ID),
    ]).await.expect("CmdJumpThroughShipcaster to Zarzakh");

    let post_cs = ctx.systems.get_or_spawn(ZARZAKH_SYS).get_combat_state(P1_SHIP).await.unwrap().expect("jumped ship combat state");
    assert!((post_cs.shield_hp - pre_cs.shield_hp).abs() < 5.0);
    assert!((post_cs.armor_hp - pre_cs.armor_hp).abs() < 1e-4);
    assert!((post_cs.structure_hp - pre_cs.structure_hp).abs() < 1e-4);
    assert!((post_cs.capacitor - pre_cs.capacitor).abs() < 5.0);
    assert_eq!(post_cs.entropic_spool_cycles, 0);
    assert!(post_cs.is_cloaked);

    // Return both pilots to Jita 4-4 cleanly for Faction Warfare militia invariants.
    gm_tr(&ctx, &mut s1, "demo", P1, 30_000_142).await;
    gm_tr(&ctx, &mut s2, "other", P2, 30_000_142).await;
    let jita44_pos = ctx.sde.stations[&JITA_44].position;
    let jita = ctx.systems.get_or_spawn(30_000_142);
    jita.set_ball_position(P1_SHIP, jita44_pos + DVec3::new(200.0, 0.0, 0.0)).await;
    jita.set_ball_position(P2_SHIP, jita44_pos + DVec3::new(200.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(JITA_44)]).await.expect("Dock P1 Jita");
    rpc(&ctx, &mut s2, "stationSvc", "Dock", &[PyRep::Integer(JITA_44)]).await.expect("Dock P2 Jita");

    let app = rpc(&ctx, &mut s2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("join".into())]).await.expect("InsertApplication");
    let app_id = app.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await.expect("Accept P2");
    refresh_session(&ctx, &mut s2, P2).await;

    let neutral_status = rpc(&ctx, &mut s1, "facWarMgr", "GetCorpFactionalWarStatus", &[PyRep::Integer(corp_id)]).await.unwrap();
    assert_eq!(neutral_status.dict_get("factionID"), Some(&PyRep::None));
    assert_eq!(neutral_status.dict_get("status"), Some(&PyRep::Integer(0)));

    rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)]).await.expect("P2 direct enlist Gallente");
    rpc(&ctx, &mut s1, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(500_001)]).await.expect("Corp enlist Caldari");
    refresh_session(&ctx, &mut s2, P2).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&P2].war_faction_id), Some(500_001));
    assert!(!ctx.db.with_snapshot(|s| s.fw_runtime.direct_enlisted_chars.contains_key(&P2)));

    assert_eq!(rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "RemoveMyDirectEnlistment", &[]).await.unwrap_err().code, "FwNotEnlisted");
    assert_eq!(s2.war_faction_id, Some(500_001));

    ctx.advance_secs(90_000);
    rpc(&ctx, &mut s1, "facWarMgr", "LeaveFactionAsCorporation", &[]).await.expect("LeaveFactionAsCorporation");
    assert_eq!(rpc(&ctx, &mut s1, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(500_001)]).await.unwrap_err().code, "MilitiaCooldownActive");
}
