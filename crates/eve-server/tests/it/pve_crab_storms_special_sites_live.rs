//! Clean end-to-end live integration tests for CRAB Beacons, CEQ & Signal Interference,
//! Metaliminal Storms, Homefront Operations, Drifter Hives, and Triglavian Sites (`feat:pve.*`).

#![forbid(unsafe_code)]

use crate::common::{self, LivePilot};
use eve_destiny::DVec3;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_SYS: u32 = 30_000_142;
const NULL_SYS: u32 = 30_004_759; // 1DQ1-A
const TYPE_CRAB_BEACON: u32 = 60_244;
const HIGH_SLOT_0: i64 = 27;
const HIGH_SLOT_1: i64 = 28;
const HIGH_SLOT_2: i64 = 29;
const HIGH_SLOT_3: i64 = 30;

fn paused_ctx() -> ServerContext {
    static BASE: std::sync::OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> = std::sync::OnceLock::new();
    let (sde, base_snap) = BASE.get_or_init(|| {
        let base_ctx = ServerContext::new();
        (base_ctx.sde.clone(), base_ctx.db.with_snapshot(Clone::clone))
    });
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = base_snap.clone());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    common::pin_clock(&ctx);
    ctx
}

fn int(v: i64) -> PyRep {
    PyRep::Integer(v)
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn ok(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    rpc(ctx, sess, svc, method, args).await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn gm_pilot_fitted_in_space(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    extra_skills: &[(u32, u8)],
    modules: &[(u32, i64, Option<(u32, i32)>)],
) -> (LivePilot, Vec<i64>) {
    let user = format!("live{char_id}");
    let pass = format!("{user}-secret");
    ctx.db.provision_gm_account(&user, &pass, char_id, &format!("Live Pilot {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    ok(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[int(char_id)]).await;

    let ship_id = common::slash_cmd(ctx, &mut sess, &format!("/createitem {ship_type_id} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    let mut stack = vec![ship_type_id];
    for &(mtid, _, c_opt) in modules {
        stack.push(mtid);
        if let Some((ctid, _)) = c_opt {
            stack.push(ctid);
        }
    }
    let mut visited = std::collections::HashSet::new();
    let mut ordered_skills = Vec::new();
    fn visit_prereqs(
        sde: &eve_sde::SdeRegistry,
        tid: u32,
        visited: &mut std::collections::HashSet<u32>,
        out: &mut Vec<u32>,
    ) {
        if !visited.insert(tid) {
            return;
        }
        if let Some(t) = sde.types.get(&tid) {
            for a in [182_u16, 183, 184, 1285, 1289, 1290] {
                if let Some(&s) = t.default_attributes.get(&a) {
                    let sid = s.round() as u32;
                    if sid > 0 {
                        visit_prereqs(sde, sid, visited, out);
                    }
                }
            }
            if t.category_id == 16 {
                out.push(tid);
            }
        }
    }
    for tid in stack {
        visit_prereqs(&ctx.sde, tid, &mut visited, &mut ordered_skills);
    }
    for sid in ordered_skills {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {sid} 5")).await;
    }
    for &(sid, lvl) in extra_skills {
        common::slash_cmd(ctx, &mut sess, &format!("/giveskill {sid} {lvl}")).await;
    }
    ctx.advance_secs(31);
    ok(ctx, &mut sess, "ship", "Board", &[int(ship_id)]).await;

    let mut fitted_ids = Vec::with_capacity(modules.len());
    for &(mtid, flag, charge_opt) in modules {
        let mid = common::slash_cmd(ctx, &mut sess, &format!("/createitem {mtid} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        ok(ctx, &mut sess, "invbroker", "Add", &[int(mid), int(ship_id), int(flag)]).await;
        if let Some((ctid, cqty)) = charge_opt {
            let cid = common::slash_cmd(ctx, &mut sess, &format!("/createitem {ctid} {cqty}")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
            ok(ctx, &mut sess, "dogmaIM", "LoadAmmo", &[int(ship_id), PyRep::List(vec![int(mid)]), PyRep::List(vec![int(cid)])]).await;
        }
        fitted_ids.push(mid);
    }
    if !fitted_ids.is_empty() {
        ok(ctx, &mut sess, "dogmaIM", "ShipOnlineModules", &[int(ship_id)]).await;
    }
    ctx.advance_secs(31);
    ok(ctx, &mut sess, "stationSvc", "Undock", &[]).await;
    common::slash_cmd(ctx, &mut sess, &format!("/tr {sys_id}")).await;
    sess.ship_id = Some(ship_id);
    let PyRep::Tuple(bound) = ok(ctx, &mut sess, "beyonce", "MachoBindObject", &[int(i64::from(sys_id)), PyRep::None]).await else { panic!("bind") };
    let beyonce = bound[0].as_i64().unwrap() as u64;
    ctx.advance_secs(70);
    let h = ctx.systems.get_or_spawn(sys_id);
    h.step_manual(10.0).await.unwrap();
    common::slash_cmd(ctx, &mut sess, "/heal").await;
    (LivePilot { sess, char_id, ship_id, beyonce }, fitted_ids)
}

fn dict_get_int<'a>(rep: &'a PyRep, key: i64) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(pairs) => pairs.iter().find(|(k, _)| k.as_i64() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

async fn list_bound_inv(ctx: &ServerContext, sess: &mut SessionState, loc_id: i64, flag: Option<i64>) -> Result<Vec<PyRep>, BridgeRpcError> {
    let list_args: Vec<PyRep> = flag.into_iter().map(int).collect();
    let rep = if sess.ship_id == Some(loc_id) || sess.station_id == Some(loc_id) {
        rpc(ctx, sess, "invbroker", "List", &list_args).await?
    } else {
        let mgr_loc = sess.station_id.or(sess.structure_id).or_else(|| sess.solar_system_id.map(i64::from)).unwrap();
        let PyRep::Tuple(m) = ok(ctx, sess, "invbroker", "MachoBindObject", &[int(mgr_loc), PyRep::None]).await else { return Ok(Vec::new()); };
        let inv_mgr = m[0].as_i64().unwrap() as u64;
        let cont_id = ctx.dispatch_rpc(sess, "invbroker", Some(inv_mgr), "GetInventoryFromId", &[int(loc_id), int(0)], &HashMap::new(), None).await?.as_i64().unwrap() as u64;
        ctx.dispatch_rpc(sess, "invbroker", Some(cont_id), "List", &list_args, &HashMap::new(), None).await?
    };
    Ok(rep.crowset_rows().unwrap_or_default().to_vec())
}

async fn qty_of(ctx: &ServerContext, sess: &mut SessionState, loc_id: i64, type_id: u32, flag: Option<i64>) -> i64 {
    let Ok(rows) = list_bound_inv(ctx, sess, loc_id, flag).await else { return 0; };
    rows.iter()
        .filter(|r| r.dbrow_get("typeID").and_then(|v| v.as_i64()) == Some(i64::from(type_id)))
        .map(|r| {
            let q = r.dbrow_get("quantity").and_then(|v| v.as_i64()).unwrap_or(0);
            if q < 0 { 1 } else { q }
        })
        .sum()
}

async fn kill_npc_with_weapons(ctx: &ServerContext, p: &mut LivePilot, weapon_ids: &[i64], npc_id: i64, pos: DVec3) {
    let sys_id = p.sess.solar_system_id.unwrap();
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = ctx.dispatch_rpc(&mut p.sess, "beyonce", Some(p.beyonce), "CmdStop", &[], &HashMap::new(), None).await;
    let mut pending_secs = 0_u64;
    let tick_step = |ctx: &ServerContext, pending: &mut u64, dt: u64| {
        *pending += dt;
        if *pending >= 50 {
            ctx.advance_secs(*pending);
            *pending = 0;
        }
    };
    for _ in 0..10 {
        let Some((_, sb)) = sys_h.get_ball_snapshot(p.ship_id).await.unwrap() else {
            break;
        };
        if !matches!(sb.mode, eve_destiny::MovementMode::Warp { .. }) && sb.velocity.length() <= 10.0 {
            break;
        }
        tick_step(ctx, &mut pending_secs, 5);
        sys_h.step_manual(5.0).await.unwrap();
    }
    let standoff = DVec3::new(2_500.0, 0.0, 0.0);
    let cur = sys_h.get_ball_snapshot(npc_id).await.unwrap().map(|(_, b)| b.position).unwrap_or(pos);
    sys_h.set_ball_position(p.ship_id, cur + standoff).await;
    ok(ctx, &mut p.sess, "dogmaIM", "AddTarget", &[int(npc_id)]).await;
    for step in 0..90 {
        let locks = sys_h.get_target_locks(p.ship_id).await.unwrap();
        if locks.iter().any(|l| l.target_id == npc_id && l.locked) {
            break;
        }
        if let Some((_, b)) = sys_h.get_ball_snapshot(npc_id).await.unwrap() {
            sys_h.set_ball_position(p.ship_id, b.position + standoff).await;
        } else {
            if pending_secs > 0 {
                ctx.advance_secs(pending_secs);
            }
            return;
        }
        if step > 0 && !locks.iter().any(|l| l.target_id == npc_id) {
            ok(ctx, &mut p.sess, "dogmaIM", "AddTarget", &[int(npc_id)]).await;
        }
        tick_step(ctx, &mut pending_secs, 1);
        sys_h.step_manual(1.0).await.unwrap();
    }
    for _ in 0..2_000 {
        let Some((_, b)) = sys_h.get_ball_snapshot(npc_id).await.unwrap() else {
            if pending_secs > 0 {
                ctx.advance_secs(pending_secs);
            }
            return;
        };
        sys_h.set_ball_position(p.ship_id, b.position + standoff).await;
        let active_set: std::collections::HashSet<i64> = ctx.db.with_snapshot(|snap| {
            weapon_ids
                .iter()
                .copied()
                .filter(|wid| snap.module_active_states.contains(wid))
                .collect()
        });
        for &wid in weapon_ids {
            if active_set.contains(&wid) {
                continue;
            }
            match rpc(ctx, &mut p.sess, "dogmaIM", "Activate", &[int(wid), PyRep::String("targetAttack".into()), int(npc_id)]).await {
                Ok(rep) if rep.dict_get("destroyed") == Some(&PyRep::Bool(true)) => {
                    if pending_secs > 0 {
                        ctx.advance_secs(pending_secs);
                    }
                    return;
                }
                Ok(_) => {}
                Err(e) if e.code == "ModuleAlreadyActive" => {}
                Err(e) if e.code == "NoAmmoLoaded" => {
                    let ammo_tid = ctx.db.with_snapshot(|snap| {
                        snap.items
                            .get(&wid)
                            .and_then(|it| ctx.sde.types.get(&it.type_id))
                            .and_then(|t| t.default_attributes.get(&128).copied())
                            .map(|sz| match sz.round() as i64 {
                                1 => 23_071_u32,
                                2 => 23_089,
                                4 => 20_911,
                                _ => 23_105,
                            })
                            .unwrap_or(23_105)
                    });
                    let cid = common::slash_cmd(ctx, &mut p.sess, &format!("/createitem {ammo_tid} 10")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
                    ok(ctx, &mut p.sess, "dogmaIM", "LoadAmmo", &[int(p.ship_id), PyRep::List(weapon_ids.iter().map(|&w| int(w)).collect()), PyRep::List(vec![int(cid)])]).await;
                    tick_step(ctx, &mut pending_secs, 11);
                    sys_h.step_manual(11.0).await.unwrap();
                    break;
                }
                Err(e) if e.code == "TargetNotLocked" => {
                    let _ = rpc(ctx, &mut p.sess, "dogmaIM", "AddTarget", &[int(npc_id)]).await;
                    tick_step(ctx, &mut pending_secs, 5);
                    sys_h.step_manual(5.0).await.unwrap();
                    break;
                }
                Err(_) if sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none() => {
                    if pending_secs > 0 {
                        ctx.advance_secs(pending_secs);
                    }
                    return;
                }
                Err(e) => panic!("dogmaIM.Activate weapon on NPC {npc_id} failed: {e:?}"),
            }
        }
        tick_step(ctx, &mut pending_secs, 5);
        sys_h.step_manual(5.0).await.unwrap();
    }
    if pending_secs > 0 {
        ctx.advance_secs(pending_secs);
    }
    assert!(sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none(), "NPC {npc_id} must be destroyed by fitted weapon");
}

#[tokio::test]
async fn test_crab_beacon_lifecycle_ceq_interference_and_cyno_jammer_live() {
    let ctx = paused_ctx();
    let (mut cap_hs, _) = gm_pilot_fitted_in_space(&ctx, 72_001, 92_100_001, 23_757 /* Archon */, JITA_SYS, &[(11_584, 5)], &[]).await;
    let hs_beacon = common::slash_cmd(&ctx, &mut cap_hs.sess, &format!("/createitem {TYPE_CRAB_BEACON} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(rpc(&ctx, &mut cap_hs.sess, "posMgr", "AnchorStructure", &[int(hs_beacon)]).await.unwrap_err().code, "CantDeployInHighSec");

    let (mut cap, cap_mods) = gm_pilot_fitted_in_space(
        &ctx, 72_002, 92_100_002, 19_720 /* Revelation */, NULL_SYS,
        &[(11_584, 5), (3300, 5), (3301, 5), (3302, 5), (3304, 5), (3309, 5), (12_215, 5), (20_525, 5), (20_533, 5), (21_668, 5), (24_563, 5)],
        &[
            (37_298 /* Dual Giga Pulse Laser II */, HIGH_SLOT_0, Some((20_911 /* True Sanshas Multifrequency XL */, 5))),
            (37_298, HIGH_SLOT_1, Some((20_911, 5))),
            (37_298, HIGH_SLOT_2, Some((20_911, 5))),
            (1952 /* Sensor Booster II */, 19 /* midSlot0 */, None),
            (2032 /* Cap Recharger II */, 20 /* midSlot1 */, None),
            (2364 /* Heat Sink II */, 11, None),
            (2364, 12, None),
            (2364, 13, None),
            (2364, 14, None),
        ],
    ).await;
    let ceq0 = ok(&ctx, &mut cap.sess, "characterEnergyMgr", "GetMyEnergyState", &[]).await;
    assert_eq!(ceq0.dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(100));
    assert_eq!(ceq0.dict_get("maxEnergy").and_then(|v| v.as_i64()), Some(100));
    let interf0 = ok(&ctx, &mut cap.sess, "solarsystemInterferenceMgr", "GetLocalInterferenceState", &[int(i64::from(NULL_SYS))]).await;
    assert_eq!(interf0.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()), Some(12));

    let beacon_id = common::slash_cmd(&ctx, &mut cap.sess, &format!("/createitem {TYPE_CRAB_BEACON} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ok(&ctx, &mut cap.sess, "posMgr", "AnchorStructure", &[int(beacon_id)]).await;

    let (mut subcap, subcap_mods) = gm_pilot_fitted_in_space(
        &ctx,
        72_003,
        92_100_003,
        28_659, /* Paladin (Marauder — subcapital, ineligible for Capital CRAB link) */
        NULL_SYS,
        &[(3300, 5), (3301, 5), (3302, 5), (3304, 5), (3309, 5), (3339, 5), (3416, 5), (3428, 5), (11_207, 5), (12_214, 5), (12_215, 5), (28_667, 5)],
        &[
            (3057 /* Mega Pulse Laser II */, HIGH_SLOT_0, Some((23_105 /* Imperial Navy Multifrequency L */, 2))),
            (3057, HIGH_SLOT_1, Some((23_105, 2))),
            (3057, HIGH_SLOT_2, Some((23_105, 2))),
            (3057, HIGH_SLOT_3, Some((23_105, 2))),
            (2032 /* Cap Recharger II */, 19 /* midSlot0 */, None),
            (2032, 20 /* midSlot1 */, None),
            (2032, 21 /* midSlot2 */, None),
            (2364 /* Heat Sink II */, 11, None),
            (2364, 12, None),
            (2364, 13, None),
            (2364, 14, None),
        ],
    ).await;
    subcap.sess.role = eve_machonet::service_roles::ROLE_PLAYER;
    let null_h = ctx.systems.get_or_spawn(NULL_SYS);
    let b_pos = null_h.get_ball_snapshot(beacon_id).await.unwrap().unwrap().1.position;
    null_h.set_ball_position(subcap.ship_id, b_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let subcap_err = rpc(&ctx, &mut subcap.sess, "beyonce", "CallComponentFromClient", &[int(beacon_id), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into()), PyRep::List(vec![])]).await.unwrap_err();
    assert_eq!(subcap_err.code, "IneligibleShipForCrabLink");

    null_h.set_ball_position(cap.ship_id, b_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let base_res = null_h.get_combat_state(cap.ship_id).await.unwrap().unwrap().shield_resonance;
    ok(&ctx, &mut cap.sess, "beyonce", "CallComponentFromClient", &[int(beacon_id), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into()), PyRep::List(vec![])]).await;

    let linked_res = null_h.get_combat_state(cap.ship_id).await.unwrap().unwrap().shield_resonance;
    assert!((linked_res[0] - base_res[0] * 0.90).abs() < 1e-4, "Active CRAB link must multiply shield resonances by 0.90 (+10% resistance)");
    let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == NULL_SYS).unwrap();
    assert_eq!(rpc(&ctx, &mut cap.sess, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), int(gate.stargate_id)]).await.unwrap_err().code, "CrabLinkActive");
    assert_eq!(ok(&ctx, &mut cap.sess, "characterEnergyMgr", "GetMyEnergyState", &[]).await.dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(70));
    assert_eq!(ok(&ctx, &mut cap.sess, "solarsystemInterferenceMgr", "GetLocalInterferenceState", &[int(i64::from(NULL_SYS))]).await.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()), Some(11));

    ctx.advance_secs(241);
    let restored_res = null_h.get_combat_state(cap.ship_id).await.unwrap().unwrap().shield_resonance;
    assert!((restored_res[0] - base_res[0]).abs() < 1e-4, "Ship resonances must restore after 4-minute CRAB link");
    let link_state = ok(&ctx, &mut cap.sess, "beyonce", "CallComponentFromClient", &[int(beacon_id), PyRep::String("linkWithShip".into()), PyRep::String("GetLinkState".into()), PyRep::List(vec![])]).await;
    assert_eq!(link_state.dict_get("phase").and_then(|v| v.as_str()), Some("scanning"));

    for wave in 0..3_usize {
        let recorded_wave_ids: std::collections::HashSet<i64> = ctx.db.with_snapshot(|s| {
            s.crab_beacons
                .beacons
                .get(&beacon_id)
                .map(|r| r.wave_npc_ball_ids.iter().copied().collect())
                .unwrap_or_default()
        });
        let wave_drones: Vec<i64> = null_h
            .list_balls()
            .await
            .unwrap()
            .into_iter()
            .filter(|b| recorded_wave_ids.contains(&b.id))
            .map(|b| b.id)
            .collect();
        for did in wave_drones {
            let is_capital_target = null_h.get_ball_snapshot(did).await.unwrap().is_some_and(|(_, b)| b.type_id == 60_333);
            if is_capital_target {
                kill_npc_with_weapons(&ctx, &mut cap, &cap_mods[..3], did, b_pos).await;
            } else {
                kill_npc_with_weapons(&ctx, &mut subcap, &subcap_mods[..4], did, b_pos).await;
            }
        }
        if wave < 2 {
            ctx.advance_secs(2);
        } else {
            let st = ok(&ctx, &mut cap.sess, "beyonce", "CallComponentFromClient", &[int(beacon_id), PyRep::String("linkWithShip".into()), PyRep::String("GetLinkState".into()), PyRep::List(vec![])]).await;
            let rem = st.dict_get("scanRemainingSeconds").and_then(|v| v.as_i64()).unwrap_or(0).max(0) as u64;
            ctx.advance_secs(rem + 5);
        }
    }
    let state_loot = ok(&ctx, &mut cap.sess, "beyonce", "CallComponentFromClient", &[int(beacon_id), PyRep::String("linkWithShip".into()), PyRep::String("GetLinkState".into()), PyRep::List(vec![])]).await;
    assert_eq!(state_loot.dict_get("phase").and_then(|v| v.as_str()), Some("loot_restricted"));

    assert_eq!(list_bound_inv(&ctx, &mut subcap.sess, beacon_id, None).await.unwrap_err().code, "LootOwnershipRestricted");
    assert_eq!(qty_of(&ctx, &mut cap.sess, beacon_id, 60_459, None).await, 15, "CRAB beacon must award 15x Rogue Drone Survey Data (60459)");

    ctx.advance_secs(125);
    let state_done = ok(&ctx, &mut cap.sess, "beyonce", "CallComponentFromClient", &[int(beacon_id), PyRep::String("linkWithShip".into()), PyRep::String("GetLinkState".into()), PyRep::List(vec![])]).await;
    assert_eq!(state_done.dict_get("phase").and_then(|v| v.as_str()), Some("self_destructed"));
    let can_id = state_done.dict_get("openContainerID").and_then(|v| v.as_i64()).expect("open container after self-destruct");
    null_h.set_ball_position(subcap.ship_id, b_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let can_items = list_bound_inv(&ctx, &mut subcap.sess, can_id, None).await.expect("open container loot");
    assert!(!can_items.is_empty(), "Open container after self-destruct must be accessible to any pilot");

    for _ in 0..2 {
        let bid = common::slash_cmd(&ctx, &mut cap.sess, &format!("/createitem {TYPE_CRAB_BEACON} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        ok(&ctx, &mut cap.sess, "posMgr", "AnchorStructure", &[int(bid)]).await;
        let bp = null_h.get_ball_snapshot(bid).await.unwrap().unwrap().1.position;
        null_h.set_ball_position(cap.ship_id, bp + DVec3::new(1_000.0, 0.0, 0.0)).await;
        ok(&ctx, &mut cap.sess, "beyonce", "CallComponentFromClient", &[int(bid), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLinkQuick".into()), PyRep::List(vec![])]).await;
        ctx.advance_secs(241);
    }
    let energy_after_3 = ok(&ctx, &mut cap.sess, "characterEnergyMgr", "GetMyEnergyState", &[]).await.dict_get("currentEnergy").and_then(|v| v.as_i64()).unwrap();
    assert!((10..=15).contains(&energy_after_3), "3x CRAB links must consume 90 CEQ (~10 remaining + natural regen), got {energy_after_3}");
    let bid4 = common::slash_cmd(&ctx, &mut cap.sess, &format!("/createitem {TYPE_CRAB_BEACON} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ok(&ctx, &mut cap.sess, "posMgr", "AnchorStructure", &[int(bid4)]).await;
    let bp4 = null_h.get_ball_snapshot(bid4).await.unwrap().unwrap().1.position;
    null_h.set_ball_position(cap.ship_id, bp4 + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(rpc(&ctx, &mut cap.sess, "beyonce", "CallComponentFromClient", &[int(bid4), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into()), PyRep::List(vec![])]).await.unwrap_err().code, "InsufficientEnergy");

    for idx in 0..3_usize {
        let (mut alt, _) = gm_pilot_fitted_in_space(&ctx, 72_010 + idx as u64, 92_100_010 + idx as i64, 23_757, NULL_SYS, &[(11_584, 5)], &[]).await;
        for _ in 0..3 {
            let b = common::slash_cmd(&ctx, &mut alt.sess, &format!("/createitem {TYPE_CRAB_BEACON} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
            ok(&ctx, &mut alt.sess, "posMgr", "AnchorStructure", &[int(b)]).await;
            let p = null_h.get_ball_snapshot(b).await.unwrap().unwrap().1.position;
            null_h.set_ball_position(alt.ship_id, p + DVec3::new(1_000.0, 0.0, 0.0)).await;
            ok(&ctx, &mut alt.sess, "beyonce", "CallComponentFromClient", &[int(b), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLinkQuick".into()), PyRep::List(vec![])]).await;
            ctx.advance_secs(241);
        }
    }
    assert_eq!(ok(&ctx, &mut cap.sess, "solarsystemInterferenceMgr", "GetLocalInterferenceState", &[int(i64::from(NULL_SYS))]).await.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()), Some(0));
    let (mut cap_excess, _) = gm_pilot_fitted_in_space(&ctx, 72_020, 92_100_020, 23_757, NULL_SYS, &[(11_584, 5)], &[]).await;
    let b_ex = common::slash_cmd(&ctx, &mut cap_excess.sess, &format!("/createitem {TYPE_CRAB_BEACON} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ok(&ctx, &mut cap_excess.sess, "posMgr", "AnchorStructure", &[int(b_ex)]).await;
    let bp_ex = null_h.get_ball_snapshot(b_ex).await.unwrap().unwrap().1.position;
    null_h.set_ball_position(cap_excess.ship_id, bp_ex + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(rpc(&ctx, &mut cap_excess.sess, "beyonce", "CallComponentFromClient", &[int(b_ex), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into()), PyRep::List(vec![])]).await.unwrap_err().code, "ExcessInterference");

    ctx.advance_secs(25 * 3_600);
    assert_eq!(ok(&ctx, &mut cap.sess, "characterEnergyMgr", "GetMyEnergyState", &[]).await.dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(100));
    assert_eq!(ok(&ctx, &mut cap.sess, "solarsystemInterferenceMgr", "GetLocalInterferenceState", &[int(i64::from(NULL_SYS))]).await.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()), Some(12));

    let (mut jammer_tester, _) = gm_pilot_fitted_in_space(&ctx, 72_021, 92_100_021, 23_757, 30_000_192 /* Pochven cyno-jammed */, &[(11_584, 5)], &[]).await;
    let j_beacon = common::slash_cmd(&ctx, &mut jammer_tester.sess, &format!("/createitem {TYPE_CRAB_BEACON} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(rpc(&ctx, &mut jammer_tester.sess, "posMgr", "AnchorStructure", &[int(j_beacon)]).await.unwrap_err().code, "CynoJammed");
}

#[tokio::test]
async fn test_metaliminal_storm_footprint_dogma_overlap_and_movement_live() {
    let ctx = paused_ctx();
    let (mut pilot, _) = gm_pilot_fitted_in_space(&ctx, 72_030, 92_100_030, 626 /* Vexor */, 30_000_208 /* Storm 1 Core */, &[], &[]).await;

    let PyRep::List(weather_rows) = ok(&ctx, &mut pilot.sess, "map", "GetAllRoamingWeatherSystems", &[]).await else { panic!("weather rows") };
    assert!(!weather_rows.is_empty());

    // Group rows by stormID -> 8 roaming storms in null-sec (first isCore == true row per storm is its center)
    let mut centers_by_storm: HashMap<i64, u32> = HashMap::new();
    let mut storm_count_per_sys: HashMap<u32, usize> = HashMap::new();
    for r in &weather_rows {
        let storm_id = r.dict_get("stormID").and_then(|v| v.as_i64()).unwrap();
        let sid = r.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
        if r.dict_get("isCore") == Some(&PyRep::Bool(true)) {
            centers_by_storm.entry(storm_id).or_insert(sid);
        }
        *storm_count_per_sys.entry(sid).or_default() += 1;
        assert!(ctx.sde.solar_systems[&sid].security_status < 0.0, "Storm system {sid} must be in null-sec");
    }
    assert_eq!(centers_by_storm.len(), 8, "Cluster must maintain 8 roaming Metaliminal Storms");

    let gamma_core = weather_rows.iter().find(|r| r.dict_get("weather").and_then(|v| v.as_str()) == Some("Gamma") && r.dict_get("isCore") == Some(&PyRep::Bool(true))).and_then(|r| r.dict_get("solarSystemID").and_then(|v| v.as_i64())).unwrap() as u32;
    let gamma_periph = weather_rows.iter().find(|r| r.dict_get("weather").and_then(|v| v.as_str()) == Some("Gamma") && r.dict_get("isCore") == Some(&PyRep::Bool(false))).and_then(|r| r.dict_get("solarSystemID").and_then(|v| v.as_i64())).unwrap() as u32;
    let fire_core = weather_rows.iter().find(|r| r.dict_get("weather").and_then(|v| v.as_str()) == Some("Firestorm") && r.dict_get("isCore") == Some(&PyRep::Bool(true))).and_then(|r| r.dict_get("solarSystemID").and_then(|v| v.as_i64())).unwrap() as u32;
    let core_sys = gamma_core;

    let base_sh = ctx.sde.types[&626].default_attributes.get(&263).copied().unwrap_or(1_200.0);
    let (p_out, _) = gm_pilot_fitted_in_space(&ctx, 72_031, 92_100_031, 626, NULL_SYS, &[], &[]).await;
    let (p_periph, _) = gm_pilot_fitted_in_space(&ctx, 72_032, 92_100_032, 626, gamma_periph, &[], &[]).await;
    let (p_core, _) = gm_pilot_fitted_in_space(&ctx, 72_033, 92_100_033, 626, gamma_core, &[], &[]).await;
    let (p_fire, _) = gm_pilot_fitted_in_space(&ctx, 72_034, 92_100_034, 626, fire_core, &[], &[]).await;

    let cs_out = ctx.systems.get_or_spawn(NULL_SYS).get_combat_state(p_out.ship_id).await.unwrap().unwrap();
    let cs_periph = ctx.systems.get_or_spawn(gamma_periph).get_combat_state(p_periph.ship_id).await.unwrap().unwrap();
    let cs_core = ctx.systems.get_or_spawn(gamma_core).get_combat_state(p_core.ship_id).await.unwrap().unwrap();
    let cs_fire = ctx.systems.get_or_spawn(fire_core).get_combat_state(p_fire.ship_id).await.unwrap().unwrap();

    assert!((cs_out.max_shield_hp - base_sh).abs() < 1e-2);
    assert!(cs_periph.max_shield_hp > cs_out.max_shield_hp, "Weak Gamma periphery must boost max_shield_hp");
    assert!((cs_core.max_shield_hp - cs_periph.max_shield_hp).abs() < 1e-2, "SDE Gamma core & periphery share +50% shield bonus");
    assert!(cs_periph.shield_resonance[3] > cs_out.shield_resonance[3], "Weak Gamma periphery must penalize explosive resonance (+30%)");
    assert!(cs_core.shield_resonance[3] > cs_periph.shield_resonance[3], "Strong Gamma core must penalize explosive resonance more than periphery (+50% vs +30%)");
    assert!(cs_fire.max_armor_hp > cs_out.max_armor_hp, "Strong Firestorm core must boost max_armor_hp");
    assert!(storm_count_per_sys.len() >= 16, "4 storms with core+periphery footprints cover distinct solar systems");

    // Verify storm anomalies exist in core systems and NOT in periphery-only systems
    let mut core_pilot = p_core;
    let PyRep::Dict(core_anoms) = ok(&ctx, &mut core_pilot.sess, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[]).await else { panic!("core anoms") };
    assert!(core_anoms.iter().any(|(k, _)| k.as_i64() == Some(i64::from(core_sys))));

    // Step in 12h increments across 48h window (max initial interval 46h) -> each storm takes first 1-jump step
    let extract_snapshot_centers = |rows: &[PyRep]| -> HashMap<i64, u32> {
        let mut out = HashMap::new();
        for r in rows {
            if r.dict_get("isCore") == Some(&PyRep::Bool(true)) {
                let sid_storm = r.dict_get("stormID").and_then(|v| v.as_i64()).unwrap();
                let sid = r.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
                out.entry(sid_storm).or_insert(sid);
            }
        }
        out
    };
    let mut mid_centers: HashMap<i64, u32> = HashMap::new();
    for _ in 0..4 {
        ctx.advance_secs(12 * 3_600);
        let PyRep::List(rows) = ok(&ctx, &mut pilot.sess, "map", "GetAllRoamingWeatherSystems", &[]).await else { panic!("mid rows") };
        for (sid_storm, sid) in extract_snapshot_centers(&rows) {
            if sid != centers_by_storm[&sid_storm] {
                mid_centers.entry(sid_storm).or_insert(sid);
            }
        }
        if mid_centers.len() == 8 { break; }
    }
    assert_eq!(mid_centers.len(), 8, "All 8 storms must move 1 jump within 24-48h window");

    let mut final_centers: HashMap<i64, u32> = HashMap::new();
    for _ in 0..4 {
        ctx.advance_secs(12 * 3_600);
        let PyRep::List(rows) = ok(&ctx, &mut pilot.sess, "map", "GetAllRoamingWeatherSystems", &[]).await else { panic!("moved rows") };
        for (sid_storm, sid) in extract_snapshot_centers(&rows) {
            if sid != mid_centers[&sid_storm] {
                final_centers.entry(sid_storm).or_insert(sid);
            }
        }
        if final_centers.len() == 8 { break; }
    }
    assert_eq!(final_centers.len(), 8, "All 8 storms must take a second 1-jump step within next 24-48h window");

    for (&storm_id, &old_c) in &centers_by_storm {
        let mid_c = mid_centers[&storm_id];
        let new_c = final_centers[&storm_id];
        assert_ne!(mid_c, old_c, "Storm {storm_id} must move within 24-48h window");
        assert!(ctx.sde.jumps.values().any(|j| j.from_solar_system_id == old_c && j.to_solar_system_id == mid_c));
        assert!(ctx.sde.jumps.values().any(|j| j.from_solar_system_id == mid_c && j.to_solar_system_id == new_c));
        assert_ne!(new_c, old_c, "Storm {storm_id} must not retrace its immediate previous center");
        assert!(ctx.sde.solar_systems[&new_c].security_status < 0.0);
    }
}

#[tokio::test]
async fn test_homefront_drifter_hives_and_triglavian_sites_live() {
    let ctx = paused_ctx();
    let gun_skill = [
        (3300_u32, 5_u8),
        (3301, 5),
        (3302, 5),
        (3304, 5),
        (3309, 5),
        (3339, 5),
        (12_215, 5),
        (28_667, 5),
    ];
    let gun_mod = [
        (3057 /* Mega Pulse Laser II */, HIGH_SLOT_0, Some((23_105 /* Imperial Navy Multifrequency L */, 2))),
        (3057, HIGH_SLOT_1, Some((23_105, 2))),
        (3057, HIGH_SLOT_2, Some((23_105, 2))),
        (3057, HIGH_SLOT_3, Some((23_105, 2))),
        (33_400 /* Bastion Module I */, 31 /* highSlot4 */, None),
        (2364 /* Heat Sink II */, 11 /* lowSlot0 */, None),
        (2364, 12 /* lowSlot1 */, None),
        (2364, 13 /* lowSlot2 */, None),
        (2364, 14 /* lowSlot3 */, None),
    ];

    let (mut hf1, hf1_mods) = gm_pilot_fitted_in_space(&ctx, 72_040, 92_100_040, 28_659 /* Paladin */, JITA_SYS, &gun_skill, &gun_mod).await;
    let (mut hf2, _) = gm_pilot_fitted_in_space(&ctx, 72_041, 92_100_041, 626, JITA_SYS, &[], &[]).await;
    let fid = ok(&ctx, &mut hf1.sess, "fleetObjectHandler", "CreateFleet", &[]).await.as_i64().unwrap();
    hf1.sess.fleet_id = Some(fid);
    ok(&ctx, &mut hf1.sess, "fleetObjectHandler", "Invite", &[int(hf2.char_id)]).await;
    ok(&ctx, &mut hf2.sess, "fleetObjectHandler", "AcceptInvite", &[int(fid)]).await;
    hf2.sess.fleet_id = Some(fid);

    let hf_jita = ok(&ctx, &mut hf1.sess, "dungeonInstanceCacheMgr", "GetHomefrontSiteInstances", &[]).await;
    let jita_site_count: usize = dict_get_int(&hf_jita, i64::from(JITA_SYS)).and_then(|d| match d {
        PyRep::List(v) => Some(v.len()),
        _ => None,
    }).unwrap_or(0);
    assert!(jita_site_count > 0, "High-sec Jita must have archetype-70 Homefront sites");

    let jita_h = ctx.systems.get_or_spawn(JITA_SYS);
    let hf_boss_id = 1_897_700_000 + (i64::from(JITA_SYS) * 1000 + 1);
    let hf_boss = jita_h.get_ball_snapshot(hf_boss_id).await.unwrap().expect("Homefront boss ball").1;
    let isk1_0 = ok(&ctx, &mut hf1.sess, "account", "GetCashBalance", &[]).await.as_f64().unwrap();
    let isk2_0 = ok(&ctx, &mut hf2.sess, "account", "GetCashBalance", &[]).await.as_f64().unwrap();
    kill_npc_with_weapons(&ctx, &mut hf1, &hf1_mods[..4], hf_boss.id, hf_boss.position).await;
    let isk1_delta = ok(&ctx, &mut hf1.sess, "account", "GetCashBalance", &[]).await.as_f64().unwrap() - isk1_0;
    let isk2_delta = ok(&ctx, &mut hf2.sess, "account", "GetCashBalance", &[]).await.as_f64().unwrap() - isk2_0;
    assert_eq!(isk1_delta, isk2_delta);
    assert!(isk1_delta > 0.0, "Homefront completion must pay equal fleet-scaled ISK to both fleetmates");

    let vidette_sys = 31_000_002_u32;
    let (mut cap_hive, _) = gm_pilot_fitted_in_space(&ctx, 72_042, 92_100_042, 23_757 /* Archon */, vidette_sys, &[], &[]).await;
    ok(&ctx, &mut cap_hive.sess, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[]).await;
    let gate1_id = 1_898_000_000 + (5649 * 100) + 10;
    let cap_gate_err = rpc(&ctx, &mut cap_hive.sess, "beyonce", "ActivateAccelerationGate", &[int(gate1_id)]).await.unwrap_err();
    assert_eq!(cap_gate_err.code, "ShipTooLargeForGate", "Capital ship cannot activate Drifter Hive acceleration gate");

    let (mut sub_hive, sub_mods) = gm_pilot_fitted_in_space(&ctx, 72_043, 92_100_043, 28_659 /* Paladin */, vidette_sys, &gun_skill, &gun_mod).await;
    let early_gate_err = rpc(&ctx, &mut sub_hive.sess, "beyonce", "ActivateAccelerationGate", &[int(gate1_id)]).await.unwrap_err();
    assert_eq!(early_gate_err.code, "RoomNotCleared", "Room 1 gate must stay locked until Room 1 Drifter NPCs are destroyed");

    let vh = ctx.systems.get_or_spawn(vidette_sys);
    for (r_npc_a, r_npc_b, gid) in [
        (1_898_000_000 + 5649 * 100 + 11, 1_898_000_000 + 5649 * 100 + 12, gate1_id),
        (1_898_000_000 + 5649 * 100 + 21, 1_898_000_000 + 5649 * 100 + 22, 1_898_000_000 + 5649 * 100 + 20),
    ] {
        for nid in [r_npc_a, r_npc_b] {
            let pos = vh.get_ball_snapshot(nid).await.unwrap().unwrap().1.position;
            kill_npc_with_weapons(&ctx, &mut sub_hive, &sub_mods[..4], nid, pos).await;
        }
        let gpos = vh.get_ball_snapshot(gid).await.unwrap().unwrap().1.position;
        vh.set_ball_position(sub_hive.ship_id, gpos).await;
        assert_eq!(ok(&ctx, &mut sub_hive.sess, "beyonce", "ActivateAccelerationGate", &[int(gid)]).await.dict_get("success"), Some(&PyRep::Bool(true)));
    }
    let warden_id = 1_898_000_000 + 5649 * 100 + 30;
    let wpos = vh.get_ball_snapshot(warden_id).await.unwrap().unwrap().1.position;
    let _ = rpc(&ctx, &mut sub_hive.sess, "dogmaIM", "Activate", &[int(sub_mods[4]), PyRep::String("moduleBonusBastion".into()), PyRep::None, int(1)]).await;
    kill_npc_with_weapons(&ctx, &mut sub_hive, &sub_mods[..4], warden_id, wpos).await;
    let elem_qty = qty_of(&ctx, &mut sub_hive.sess, sub_hive.ship_id, 34_558 /* Vidette Element */, None).await;
    let blue_qty = qty_of(&ctx, &mut sub_hive.sess, sub_hive.ship_id, 30_745 /* Antikythera Element */, None).await;
    assert_eq!((elem_qty, blue_qty), (1, 5), "Clearing Vidette Hive Warden must drop 1x Vidette Element + 5x Blue Loot");

    let trig_sys = 30_003_076_u32;
    let (mut trig_p, trig_mods) = gm_pilot_fitted_in_space(&ctx, 72_044, 92_100_044, 28_659 /* Paladin */, trig_sys, &gun_skill, &gun_mod).await;
    ok(&ctx, &mut trig_p.sess, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[]).await;

    let trig_boss_id = 1_897_700_000 + (i64::from(trig_sys) * 100 + 1);
    let tpos = ctx.systems.get_or_spawn(trig_sys).get_ball_snapshot(trig_boss_id).await.unwrap().unwrap().1.position;
    kill_npc_with_weapons(&ctx, &mut trig_p, &trig_mods[..4], trig_boss_id, tpos).await;
    assert_eq!(qty_of(&ctx, &mut trig_p.sess, trig_p.ship_id, 48_121 /* Triglavian Survey Database */, None).await, 150);

    common::slash_cmd(&ctx, &mut trig_p.sess, "/tr 30000012").await;
    ok(&ctx, &mut trig_p.sess, "beyonce", "MachoBindObject", &[int(30_000_012), PyRep::None]).await;
    ok(&ctx, &mut trig_p.sess, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[]).await;
    let eden_boss_id = 1_897_700_000 + (30_000_012 * 100 + 1);
    let epos = ctx.systems.get_or_spawn(30_000_012).get_ball_snapshot(eden_boss_id).await.unwrap().unwrap().1.position;
    kill_npc_with_weapons(&ctx, &mut trig_p, &trig_mods[..4], eden_boss_id, epos).await;
    assert_eq!(qty_of(&ctx, &mut trig_p.sess, trig_p.ship_id, 48_121, None).await, 300);
}
