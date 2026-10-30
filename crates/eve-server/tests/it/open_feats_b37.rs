//! Integration tests for batch 37 exclusive `feat:` parity rows:
//! - `feat:pve.operations-tutorial`
//! - `feat:pve.seasonal-login`
//! - `feat:structures.profiles`
//! - `feat:structures.structure-directory`

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use eve_db::{
    CharacterRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord, WorldRepository, corp_roles,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{SessionState, service_roles};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use eve_server::router::ServerContext;
use eve_server::services::account_services;
use eve_server::services::operations_manager_helpers::get_operation_defs;
use eve_server::services::station_structure::structure_upwell_ops::{
    PROFILE_CHANGE_DELAY_SECS, load_ext_state, save_ext_state,
};
use glam::DVec3;

use super::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYS: u32 = 30_000_142;
const AMARR_SYS: u32 = 30_002_187;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = Arc::new(SdeRegistry::open(&SdeRegistry::default_db_path()).expect("open SDE"));
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, m, args, &HashMap::new(), None).await
}

async fn rpc_kw(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep], kw: &HashMap<String, PyRep>) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, m, args, kw, None).await
}

async fn login_demo_pilot(ctx: &ServerContext, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    let (user, pass) = if char_id == PILOT_2 { ("other", "other-secret") } else { ("demo", "demo-secret") };
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    sess.solar_system_id = Some(JITA_SYS);
    sess.station_id = Some(JITA_STATION);
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = JITA_SYS;
            ch.station_id = Some(JITA_STATION);
            ch.structure_id = None;
            ch.home_station_id = JITA_STATION;
        }
    });
    sess
}

fn seed_corp_and_pilots(ctx: &ServerContext, corp_id: i64, ceo_id: i64, members_with_roles: &[(i64, u64, &str)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.get(&PILOT_1).expect("demo pilot 1").clone();
        let mut member_ids = HashSet::new();
        let mut role_map = HashMap::new();
        for &(cid, roles, name) in members_with_roles {
            member_ids.insert(cid);
            role_map.insert(cid, roles);
            let ship_id = 770_000_000 + cid;
            snap.items.entry(ship_id).or_insert(InventoryItemRecord {
                item_id: ship_id, type_id: 587, owner_id: cid, location_id: i64::from(JITA_SYS),
                flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            let ch = snap.characters.entry(cid).or_insert_with(|| CharacterRecord {
                character_id: cid, user_id: cid as u64, name: name.to_string(), corporation_id: corp_id,
                solar_system_id: JITA_SYS, constellation_id: 20_000_020, region_id: 10_000_002,
                active_ship_id: ship_id, home_station_id: JITA_STATION, balance: IskCents(100_000_00),
                race_id: 1, bloodline_id: 1, ancestry_id: 1, clone_grade: 2, ..base_char.clone()
            });
            ch.corporation_id = corp_id;
            ch.solar_system_id = JITA_SYS;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: format!("Corp {corp_id}"), ticker: "TST".into(),
            ceo_id, hq_station_id: Some(JITA_STATION), members: member_ids, member_roles: role_map, ..Default::default()
        });
    });
}

async fn make_space_session(ctx: &ServerContext, char_id: i64, corp_id: i64, corp_role: u64) -> SessionState {
    let ship_id = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id).unwrap());
    let _ = ctx.systems.get_or_spawn(JITA_SYS).spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = corp_role;
    sess.solar_system_id = Some(JITA_SYS);
    sess.constellation_id = Some(20_000_020);
    sess.region_id = Some(10_000_002);
    sess.ship_id = Some(ship_id);
    sess
}

async fn seed_structure_with_services(ctx: &ServerContext, structure_id: i64, owner_corp_id: i64, name: &str, state: &str, modules: &[(i64, u32, InventoryFlag, bool)]) {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(structure_id, UpwellStructureRecord {
            structure_id, type_id: 35_832, owner_corp_id, solar_system_id: JITA_SYS, name: name.to_string(),
            position: [1_000.0, 0.0, 0.0], state: state.to_string(), fuel_blocks: 2_400, quantum_core_installed: true,
            shield_hp: 1_000_000.0, armor_hp: 1_000_000.0, hull_hp: 1_000_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut ext = load_ext_state(snap);
        let lf = ext.lifecycle.entry(structure_id).or_default();
        lf.last_fuel_tick_unix = Some(now);
        for &(item_id, type_id, flag, online) in modules {
            snap.items.insert(item_id, InventoryItemRecord {
                item_id, type_id, owner_id: owner_corp_id, location_id: structure_id, flag,
                quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.module_online_states.insert(item_id, online);
            if online && !lf.online_modules.contains(&item_id) {
                lf.online_modules.push(item_id);
            }
        }
        save_ext_state(snap, &ext);
    });
    let _ = ctx.systems.get_or_spawn(JITA_SYS).spawn_ball(Ball {
        id: structure_id, type_id: 35_832, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0e9, radius: 5_000.0, speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await;
}

#[tokio::test]
// parity: feat:pve.operations-tutorial
async fn test_operations_tutorial_tasks_categories_progress_and_rewards() {
    eve_testkit::attest(&["feat:pve.operations-tutorial"]);
    let ctx = new_ctx();
    common::pin_clock(&ctx);

    // Negative 1: unauthenticated / characterless session rejected
    let mut anon = SessionState::new(77);
    assert!(rpc(&ctx, &mut anon, "operationsManager", "get_character_progress", &[]).await.is_err());

    let mut sess = login_demo_pilot(&ctx, PILOT_1).await;

    // Negative 2: non-existent SDE operation ID & non-existent category ID rejected
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(999_999)]).await.unwrap_err().code, "CustomInfo");
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(999_999)]).await.unwrap_err().code, "CustomInfo");

    let defs = get_operation_defs(&ctx.sde);
    let (&op_id, op_tasks) = defs
        .op_tasks
        .iter()
        .filter(|(oid, ts)| {
            defs.sde_operations.contains(oid)
                && ts.len() >= 2
                && (defs.op_isk_rewards.get(oid).copied().unwrap_or(0) > 0
                    || defs.op_completion_rewards.get(oid).is_some_and(|r| !r.is_empty())
                    || defs.op_skill_rewards.get(oid).is_some_and(|r| !r.is_empty()))
        })
        .min_by_key(|(oid, _)| **oid)
        .expect("SDE multi-task operation with rewards");
    let op_tasks = op_tasks.clone();
    let cat_id = defs
        .cat_isk_rewards
        .iter()
        .filter(|(_, isk)| **isk > 0)
        .map(|(cid, _)| *cid)
        .min()
        .or_else(|| defs.sde_categories.iter().copied().min())
        .expect("SDE operation category");

    // Negative 3 & 4: wrong solar system rejected; cross-character tutorial query without GM rejected
    assert_eq!(
        rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op_id), PyRep::Integer(i64::from(AMARR_SYS)), PyRep::Integer(cat_id)]).await.unwrap_err().code,
        "CustomInfo"
    );
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "is_main_tutorial_finished", &[PyRep::Integer(PILOT_2)]).await.unwrap_err().code, "UserError");

    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(cat_id)]).await.unwrap(), PyRep::Integer(1));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "is_category_complete", &[PyRep::Integer(cat_id)]).await.unwrap(), PyRep::Bool(false));

    let wallet_before = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
    let items_before: i32 = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION).map(|i| i.quantity).sum());

    assert_eq!(
        rpc(&ctx, &mut sess, "operationsManager", "activate_operation_in_solar_system", &[PyRep::Integer(op_id), PyRep::Integer(i64::from(JITA_SYS)), PyRep::Integer(cat_id)]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_active_category_id", &[]).await.unwrap(), PyRep::Integer(cat_id));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(cat_id)]).await.unwrap(), PyRep::Integer(2));

    // Blocking first task prevents completion when client event fires
    let (t0, t1) = (op_tasks[0], op_tasks[1]);
    rpc(&ctx, &mut sess, "operationsManager", "block_task", &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(t0)]).await.unwrap();
    let mut kw_t0 = HashMap::new();
    kw_t0.insert("extra_data".into(), PyRep::Dict(vec![(PyRep::String("task_id".into()), PyRep::Integer(t0))]));
    rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_progress".into())], &kw_t0).await.unwrap();
    let PyRep::Dict(prog_b) = rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(prog_b.iter().find(|(k, _)| k.as_i64() == Some(t0)).and_then(|(_, v)| v.as_i64()), Some(0));

    // Unblock t0 and complete via SDE eventCondition -> partial op progress (1..100), not yet completed
    rpc(&ctx, &mut sess, "operationsManager", "unblock_task", &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(t0)]).await.unwrap();
    let ev_name = defs.task_event_conditions.get(&t0).and_then(|cs| cs.first().cloned()).unwrap_or_else(|| "task_progress".into());
    rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String(ev_name)], &kw_t0).await.unwrap();
    let PyRep::Dict(prog_partial) = rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(prog_partial.iter().find(|(k, _)| k.as_i64() == Some(t0)).and_then(|(_, v)| v.as_i64()), Some(100));
    let op_pct = prog_partial.iter().find(|(k, _)| k.as_i64() == Some(op_id)).and_then(|(_, v)| v.as_i64()).unwrap_or(0);
    assert!((1..100).contains(&op_pct), "expected partial progress in 1..100, got {op_pct}");
    let PyRep::List(completed_mid) = rpc(&ctx, &mut sess, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
    assert!(!completed_mid.contains(&PyRep::Integer(op_id)));

    // Skip t1 and finish remaining tasks -> operation reaches 100% and delivers SDE rewards once
    rpc(&ctx, &mut sess, "operationsManager", "skip_task", &[PyRep::Integer(cat_id), PyRep::Integer(op_id), PyRep::Integer(t1)]).await.unwrap();
    for &tn in op_tasks.iter().skip(2) {
        let mut kw_tn = HashMap::new();
        kw_tn.insert("extra_data".into(), PyRep::Dict(vec![(PyRep::String("task_id".into()), PyRep::Integer(tn))]));
        rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_progress".into())], &kw_tn).await.unwrap();
    }
    let PyRep::Dict(prog_done) = rpc(&ctx, &mut sess, "operationsManager", "get_character_progress", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(prog_done.iter().find(|(k, _)| k.as_i64() == Some(op_id)).and_then(|(_, v)| v.as_i64()), Some(100));
    let PyRep::List(completed_ops) = rpc(&ctx, &mut sess, "operationsManager", "get_operations_completed_at_least_once", &[]).await.unwrap() else { panic!("List") };
    assert!(completed_ops.contains(&PyRep::Integer(op_id)));

    let wallet_after_op = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
    let items_after_op: i32 = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION).map(|i| i.quantity).sum());
    assert!(wallet_after_op > wallet_before || items_after_op > items_before, "completing SDE operation {op_id} must grant real SDE rewards");

    // Idempotency: re-firing task event does not double-grant operation rewards
    rpc_kw(&ctx, &mut sess, "operationsManager", "process_client_event", &[PyRep::String("task_progress".into())], &kw_t0).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0), wallet_after_op);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION).map(|i| i.quantity).sum::<i32>()), items_after_op);

    // Complete category -> state 3 (COMPLETE), is_category_complete == true, SDE category ISK reward delivered once
    let expected_cat_isk_cents = (defs.cat_isk_rewards.get(&cat_id).copied().unwrap_or(0) as i64) * 100;
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(cat_id)]).await.unwrap(), PyRep::Integer(3));
    assert_eq!(rpc(&ctx, &mut sess, "operationsManager", "is_category_complete", &[PyRep::Integer(cat_id)]).await.unwrap(), PyRep::Bool(true));
    let wallet_after_cat = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0);
    assert_eq!(wallet_after_cat - wallet_after_op, expected_cat_isk_cents);
    rpc(&ctx, &mut sess, "operationsManager", "complete_category_for_character", &[PyRep::Integer(cat_id)]).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.0), wallet_after_cat);
}

#[tokio::test]
// parity: feat:pve.seasonal-login
async fn test_seasonal_login_campaign_active_claim_and_history() {
    eve_testkit::attest(&["feat:pve.seasonal-login"]);
    let ctx = new_ctx();
    // 1_700_044_800 = 2023-11-15 10:40:00 UTC (20 minutes before 11:00:00 UTC daily downtime turnover)
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_700_044_800)));

    // Negative 1: unauthenticated session rejected
    let mut anon = SessionState::new(88);
    assert!(rpc(&ctx, &mut anon, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.is_err());

    let mut sess = login_demo_pilot(&ctx, PILOT_1).await;
    let uid = sess.user_id.expect("authenticated user_id");

    // Unscheduled campaign returns exact V24 4-tuple (None, None, None, None) and rejects claim_reward
    assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap(), PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None]));
    let e_unsched = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap_err();
    assert!(e_unsched.message.to_ascii_lowercase().contains("no seasonal campaign"));

    // Negative 2 & 3: non-GM cannot schedule /campaign; GM scheduling unknown SDE campaign ID fails
    assert_eq!(rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())]).await.unwrap_err().code, "InsufficientRole");
    let mut gm_sess = login_demo_pilot(&ctx, PILOT_2).await;
    gm_sess.role = service_roles::ROLE_GML | service_roles::ROLE_ADMIN;
    assert_eq!(rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 99999999 30 11:00:00".into())]).await.unwrap_err().code, "CampaignNotFound");

    // GM schedules real SDE campaign 2323 for 30 days with 11:00:00 UTC turnover
    rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/campaign seasonal 2323 30 11:00:00".into())]).await.expect("GM schedule 2323");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&PILOT_1).unwrap().clone_grade = 1);

    let PyRep::Tuple(active_fields) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap() else { panic!("Tuple") };
    assert_eq!(active_fields.len(), 4);
    assert_eq!(active_fields[0], PyRep::Integer(2323));
    assert!(active_fields[1].as_i64().unwrap_or(0) > 730_000);
    assert_eq!(active_fields[2], PyRep::String("11:00:00".into()));
    let initial_rem = active_fields[3].as_i64().expect("num_remaining_rewards");
    assert!(initial_rem > 0);

    // Alpha Day 0 claim: delivers SDE Day 0 Alpha reward (87900) to hangar, withholds Omega reward (49809), no redeem token notification while docked
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap(), PyRep::Bool(true));
    assert!(ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION && i.type_id == 87_900)));
    assert!(!ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == PILOT_1 && i.type_id == 49_809)));
    assert!(!ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));

    let PyRep::Dict(hist_day0) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(hist_day0.len(), 1);
    let ord0 = hist_day0[0].0.as_i64().unwrap();
    assert_eq!(hist_day0[0].1, PyRep::Integer(0), "Alpha claim state must be CLONE_STATE_ALPHA (0)");
    let PyRep::Tuple(after_d0) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap() else { panic!("Tuple") };
    assert_eq!(after_d0[3], PyRep::Integer(initial_rem - 1));

    // Negative 4: duplicate claim before 11:00:00 UTC turnover (10:50:00 UTC) is rejected
    ctx.advance_secs(600);
    let e_dup = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap_err();
    assert!(e_dup.message.to_ascii_lowercase().contains("already claimed"));

    // Advance across 11:00:00 UTC turnover (11:10:00 UTC = Day 1), upgrade pilot to Omega (clone_grade = 2) and undock to space
    ctx.advance_secs(1_200);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&PILOT_1).unwrap();
        ch.clone_grade = 2;
        ch.station_id = None;
    });
    let tokens_before = ctx.db.with_snapshot(|s| account_services::load_store(s).redeem_tokens.get(&uid).map_or(0, Vec::len));
    let _ = ctx.drain_notifications_for_char(PILOT_1);

    assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[PyRep::Bool(true)]).await.unwrap(), PyRep::Bool(true));
    let has_retro_omega_token = ctx.db.with_snapshot(|s| {
        let toks = account_services::load_store(s).redeem_tokens.get(&uid).cloned().unwrap_or_default();
        toks.len() > tokens_before && toks.iter().any(|t| t.type_id == 49_809)
    });
    assert!(has_retro_omega_token, "Undocked Omega claim with retro_claim=true must enqueue Day 0 Omega reward 49809 into redeeming queue");
    assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnRedeemingTokensUpdated"));

    let PyRep::Dict(hist_day1) = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(hist_day1.len(), 2);
    assert_eq!(hist_day1.iter().find(|(k, _)| k.as_i64() == Some(ord0)).and_then(|(_, v)| v.as_i64()), Some(1));
    assert_eq!(hist_day1.iter().find(|(k, _)| k.as_i64() == Some(ord0 + 1)).and_then(|(_, v)| v.as_i64()), Some(1));

    // Advance past 30-day campaign end -> campaign expires cleanly
    ctx.advance_secs(31 * 86_400);
    assert_eq!(rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await.unwrap(), PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None]));
    let e_exp = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[]).await.unwrap_err();
    assert!(e_exp.message.to_ascii_lowercase().contains("no seasonal campaign"));
}

#[tokio::test]
// parity: feat:structures.profiles
async fn test_structure_profiles_acl_mapping_and_delayed_apply() {
    eve_testkit::attest(&["feat:structures.profiles"]);
    let ctx = new_ctx();
    common::pin_clock(&ctx);

    const CORP_OWNER: i64 = 98_600_001;
    const CORP_GUEST: i64 = 98_600_002;
    const DIR_PILOT: i64 = 95_600_001;
    const GRUNT_PILOT: i64 = 95_600_002;
    const VIP_PILOT: i64 = 95_600_003;
    const PUB_PILOT: i64 = 95_600_004;
    const BLOCKED_CORP_PILOT: i64 = 95_600_005;
    const OUTSIDER_PILOT: i64 = 95_600_006;
    const ST_ID: i64 = 1_045_000_000_001;

    seed_corp_and_pilots(&ctx, CORP_OWNER, DIR_PILOT, &[(DIR_PILOT, corp_roles::ROLE_DIRECTOR, "Owner Dir"), (GRUNT_PILOT, 0, "Owner Grunt")]);
    seed_corp_and_pilots(&ctx, CORP_GUEST, VIP_PILOT, &[(VIP_PILOT, corp_roles::ROLE_DIRECTOR, "VIP"), (PUB_PILOT, 0, "Pub"), (BLOCKED_CORP_PILOT, 0, "Blocked")]);
    seed_corp_and_pilots(&ctx, 98_600_003, OUTSIDER_PILOT, &[(OUTSIDER_PILOT, corp_roles::ROLE_DIRECTOR, "Outsider")]);
    seed_structure_with_services(&ctx, ST_ID, CORP_OWNER, "Profile Citadel", "ShieldVulnerable", &[
        (880_001, 35_892, InventoryFlag::ServiceSlot0, true), // Market (svc 1)
        (880_002, 35_894, InventoryFlag::ServiceSlot1, true), // Clone Bay (svc 2)
    ]).await;

    let mut dir_sess = make_space_session(&ctx, DIR_PILOT, CORP_OWNER, corp_roles::ROLE_DIRECTOR).await;
    let mut grunt_sess = make_space_session(&ctx, GRUNT_PILOT, CORP_OWNER, 0).await;
    let mut vip_sess = make_space_session(&ctx, VIP_PILOT, CORP_GUEST, corp_roles::ROLE_DIRECTOR).await;
    let mut pub_sess = make_space_session(&ctx, PUB_PILOT, CORP_GUEST, 0).await;
    let mut blk_sess = make_space_session(&ctx, BLOCKED_CORP_PILOT, CORP_GUEST, 0).await;
    let mut out_sess = make_space_session(&ctx, OUTSIDER_PILOT, 98_600_003, corp_roles::ROLE_DIRECTOR).await;

    // Negative 1: non-director corp member cannot create profiles
    assert_eq!(rpc(&ctx, &mut grunt_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Unauthorized".into()), PyRep::String("".into())]).await.unwrap_err().code, "InsufficientCorpRoles");

    let vip_acl = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP ACL".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
    let pub_acl = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Public ACL".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
    let def_acl = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("Gunners ACL".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();

    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(vip_acl), PyRep::List(vec![PyRep::Integer(VIP_PILOT)])]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(pub_acl), PyRep::List(vec![PyRep::Integer(CORP_GUEST)]), PyRep::Bool(false)]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(pub_acl), PyRep::List(vec![PyRep::Integer(BLOCKED_CORP_PILOT)]), PyRep::Bool(true)]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_acl), PyRep::List(vec![PyRep::Integer(VIP_PILOT)])]).await.unwrap();

    let prof_1 = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Tiered Trade".into()), PyRep::String("ACL tiered".into())]).await.unwrap().as_i64().unwrap();

    // Negative 2 & 3: cross-corp SaveProfileSettings rejected; invalid tax > 1.0 rejected
    assert_eq!(rpc(&ctx, &mut vip_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), PyRep::Dict(vec![])]).await.unwrap_err().code, "StructureOwnershipViolation");
    let bad_tax = PyRep::Dict(vec![(PyRep::Integer(1), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(1.5))])]))]);
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), bad_tax]).await.unwrap_err().code, "InvalidStructureTaxRate");

    // Save V24 Dict<settingID, List<KeyVal(groupID, value)>> mapping docking (19), multi-ACL market tax (1), VIP clone bay (23 -> svc 2), and defense (17)
    let v24_settings = PyRep::Dict(vec![
        (PyRep::Integer(19), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Integer(1))]), PyRep::keyval([("groupID", PyRep::Integer(pub_acl)), ("value", PyRep::Integer(1))])])),
        (PyRep::Integer(1), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(0.01))]), PyRep::keyval([("groupID", PyRep::Integer(pub_acl)), ("value", PyRep::Real(0.05))])])),
        (PyRep::Integer(23), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(vip_acl)), ("value", PyRep::Real(0.02))])])),
        (PyRep::Integer(17), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(def_acl)), ("value", PyRep::Integer(1))])])),
    ]);
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_1), v24_settings]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "SetDefaultProfile", &[PyRep::Integer(prof_1)]).await.unwrap(), PyRep::Bool(true));

    let dup_id = rpc(&ctx, &mut dir_sess, "structureProfiles", "DuplicateProfile", &[PyRep::Integer(prof_1)]).await.unwrap().as_i64().unwrap();
    let PyRep::List(profs) = rpc(&ctx, &mut dir_sess, "structureProfiles", "GetProfiles", &[]).await.unwrap() else { panic!("List") };
    assert_eq!(profs.len(), 2);
    let PyRep::Dict(saved_cfg) = rpc(&ctx, &mut dir_sess, "structureProfiles", "GetProfileSettings", &[PyRep::Integer(prof_1)]).await.unwrap() else { panic!("Dict") };
    assert_eq!(saved_cfg.len(), 4);
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(dup_id)]).await.unwrap(), PyRep::Bool(true));

    // Initial profile assignment applies immediately; in-use profile cannot be deleted
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(ST_ID)]), PyRep::Integer(prof_1)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureProfiles", "DeleteProfile", &[PyRep::Integer(prof_1)]).await.unwrap_err().code, "ProfileInUse");

    // Verify Capsuleer > Corporation specificity on docking, tiered Market tax (1% vs 5%), VIP-only Clone Bay (2%), and Defense control (17)
    assert_eq!(rpc(&ctx, &mut blk_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "StructureDockingAccessDenied");
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "StructureDockingAccessDenied");
    rpc(&ctx, &mut vip_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.expect("VIP docks");
    rpc(&ctx, &mut pub_sess, "structureDocking", "Dock", &[PyRep::Integer(ST_ID)]).await.expect("Public corp member docks");

    assert!((rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(1)]).await.unwrap().as_f64().unwrap() - 0.01).abs() < 1e-6);
    assert!((rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(1)]).await.unwrap().as_f64().unwrap() - 0.05).abs() < 1e-6);
    assert!((rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(2)]).await.unwrap().as_f64().unwrap() - 0.02).abs() < 1e-6);
    assert_eq!(rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(ST_ID), PyRep::Integer(2)]).await.unwrap(), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterCheckService", &[PyRep::Integer(ST_ID), PyRep::Integer(2)]).await.unwrap_err().code, "StructureServiceAccessDenied");
    assert_eq!(rpc(&ctx, &mut pub_sess, "structureControl", "TakeControl", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "DefenseAccessDenied");
    rpc(&ctx, &mut vip_sess, "structureControl", "TakeControl", &[PyRep::Integer(ST_ID)]).await.expect("VIP gunner TakeControl");
    rpc(&ctx, &mut vip_sess, "structureControl", "ReleaseControl", &[PyRep::Integer(ST_ID)]).await.expect("VIP ReleaseControl");

    // Switch active structure to restrictive corp-only profile -> delayed apply (PROFILE_CHANGE_DELAY_SECS = 300s)
    let prof_locked = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Lockdown".into()), PyRep::String("Corp only".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(prof_locked), PyRep::Dict(vec![(PyRep::String("corpOnly".into()), PyRep::Bool(true))])]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(ST_ID)]), PyRep::Integer(prof_locked)]).await.unwrap();

    // At +120s (< 300s), old profile still governs services, taxes, docking, and defense
    ctx.advance_secs(PROFILE_CHANGE_DELAY_SECS - 180);
    assert!((rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterGetService", &[PyRep::Integer(ST_ID), PyRep::Integer(1)]).await.unwrap().as_f64().unwrap() - 0.05).abs() < 1e-6);
    assert_eq!(rpc(&ctx, &mut vip_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(ST_ID)])]).await.unwrap(), PyRep::List(vec![PyRep::Integer(ST_ID)]));

    // At +305s (>= 300s), lockdown profile applies automatically across docking, services, and defense
    ctx.advance_secs(185);
    assert_eq!(rpc(&ctx, &mut pub_sess, "structureSettings", "CharacterHasService", &[PyRep::Integer(ST_ID), PyRep::Integer(1)]).await.unwrap(), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut vip_sess, "structureSettings", "CharacterGetServices", &[PyRep::Integer(ST_ID)]).await.unwrap(), PyRep::Dict(vec![]));
    assert_eq!(rpc(&ctx, &mut vip_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(ST_ID)])]).await.unwrap(), PyRep::List(vec![]));
    assert_eq!(rpc(&ctx, &mut vip_sess, "structureControl", "TakeControl", &[PyRep::Integer(ST_ID)]).await.unwrap_err().code, "DefenseAccessDenied");
}

#[tokio::test]
// parity: feat:structures.structure-directory
async fn test_structure_directory_accessible_structures_and_services() {
    eve_testkit::attest(&["feat:structures.structure-directory"]);
    let ctx = new_ctx();
    common::pin_clock(&ctx);

    const CORP_OWNER: i64 = 98_700_001;
    const CORP_ALLY: i64 = 98_700_002;
    const DIR_ID: i64 = 95_700_001;
    const GRUNT_ID: i64 = 95_700_002;
    const VIP_ID: i64 = 95_700_003;
    const MKT_ONLY_ID: i64 = 95_700_004;
    const OUT_ID: i64 = 95_700_005;
    const ST_HUB: i64 = 1_046_000_000_001;
    const ST_ANCHORING: i64 = 1_046_000_000_002;
    const ST_PRIVATE: i64 = 1_046_000_000_003;

    seed_corp_and_pilots(&ctx, CORP_OWNER, DIR_ID, &[(DIR_ID, corp_roles::ROLE_DIRECTOR, "Dir"), (GRUNT_ID, 0, "Grunt")]);
    seed_corp_and_pilots(&ctx, CORP_ALLY, VIP_ID, &[(VIP_ID, corp_roles::ROLE_DIRECTOR, "VIP"), (MKT_ONLY_ID, 0, "MarketOnly")]);
    seed_corp_and_pilots(&ctx, 98_700_003, OUT_ID, &[(OUT_ID, corp_roles::ROLE_DIRECTOR, "Outsider")]);

    seed_structure_with_services(&ctx, ST_HUB, CORP_OWNER, "Public Trade Hub", "ShieldVulnerable", &[
        (890_001, 35_892, InventoryFlag::ServiceSlot0, true),  // Market (1) online
        (890_002, 35_899, InventoryFlag::ServiceSlot1, true),  // Reprocessing (3) online
        (890_003, 35_894, InventoryFlag::ServiceSlot2, false), // Clone Bay (2) OFFLINE
    ]).await;
    seed_structure_with_services(&ctx, ST_ANCHORING, CORP_OWNER, "Anchoring Raitaru", "Anchoring", &[]).await;
    seed_structure_with_services(&ctx, ST_PRIVATE, CORP_OWNER, "Private Sotiyo", "ShieldVulnerable", &[
        (890_004, 35_878, InventoryFlag::ServiceSlot0, true), // Manufacturing (4) online
    ]).await;

    let mut dir_sess = make_space_session(&ctx, DIR_ID, CORP_OWNER, corp_roles::ROLE_DIRECTOR).await;
    let mut grunt_sess = make_space_session(&ctx, GRUNT_ID, CORP_OWNER, 0).await;
    let mut vip_sess = make_space_session(&ctx, VIP_ID, CORP_ALLY, corp_roles::ROLE_DIRECTOR).await;
    let mut mkt_sess = make_space_session(&ctx, MKT_ONLY_ID, CORP_ALLY, 0).await;
    let mut out_sess = make_space_session(&ctx, OUT_ID, 98_700_003, corp_roles::ROLE_DIRECTOR).await;

    // Negative 1: unauthenticated session rejected
    let mut anon = SessionState::new(99);
    assert!(rpc(&ctx, &mut anon, "structureDirectory", "GetMyCharacterStructures", &[]).await.is_err());

    let g_vip = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
    let g_mkt = rpc(&ctx, &mut dir_sess, "ownerGroupManager", "Create", &[PyRep::String("MktOnly".into()), PyRep::String("".into())]).await.unwrap().dict_get("groupID").unwrap().as_i64().unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(g_vip), PyRep::List(vec![PyRep::Integer(VIP_ID)])]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(g_mkt), PyRep::List(vec![PyRep::Integer(MKT_ONLY_ID)])]).await.unwrap();

    let p_hub = rpc(&ctx, &mut dir_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Hub ACL".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    let hub_cfg = PyRep::Dict(vec![
        (PyRep::Integer(19), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(g_vip)), ("value", PyRep::Integer(1))]), PyRep::keyval([("groupID", PyRep::Integer(g_mkt)), ("value", PyRep::Integer(1))])])),
        (PyRep::Integer(1), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(g_vip)), ("value", PyRep::Real(0.01))]), PyRep::keyval([("groupID", PyRep::Integer(g_mkt)), ("value", PyRep::Real(0.03))])])),
        (PyRep::Integer(3), PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(g_vip)), ("value", PyRep::Real(0.02))])])),
    ]);
    rpc(&ctx, &mut dir_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_hub), hub_cfg]).await.unwrap();
    rpc(&ctx, &mut dir_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(ST_HUB)]), PyRep::Integer(p_hub)]).await.unwrap();

    // Negative 2 & 3: remote outsider denied GetStructureInfo on private structure; unknown structure returns StructureNotFound
    out_sess.solar_system_id = Some(AMARR_SYS);
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&OUT_ID).unwrap().solar_system_id = AMARR_SYS);
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(ST_PRIVATE)]).await.unwrap_err().code, "StructureAccessDenied");
    assert_eq!(rpc(&ctx, &mut dir_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(999_999_999)]).await.unwrap_err().code, "StructureNotFound");

    // Director sees all 3 corp structures; non-operational ST_ANCHORING has empty services and is excluded from dockable lists
    let PyRep::Dict(corp_map) = rpc(&ctx, &mut dir_sess, "structureDirectory", "GetMyCorporationStructures", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(corp_map.len(), 3);
    let hub_dir = &corp_map.iter().find(|(k, _)| k.as_i64() == Some(ST_HUB)).unwrap().1;
    assert_eq!(hub_dir.dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(1)), (PyRep::Integer(3), PyRep::Integer(1))])));
    assert_eq!(hub_dir.dict_get("fuelBlocks"), Some(&PyRep::Integer(2_400)));
    assert_eq!(hub_dir.dict_get("profileID"), Some(&PyRep::Integer(p_hub)));
    let anch_dir = &corp_map.iter().find(|(k, _)| k.as_i64() == Some(ST_ANCHORING)).unwrap().1;
    assert_eq!(anch_dir.dict_get("services"), Some(&PyRep::Dict(vec![])));
    assert_eq!(
        rpc(&ctx, &mut dir_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap(),
        PyRep::List(vec![PyRep::Integer(ST_HUB), PyRep::Integer(ST_PRIVATE)])
    );

    // Non-director corp member has fuelBlocks masked
    assert_eq!(rpc(&ctx, &mut grunt_sess, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(ST_HUB)]).await.unwrap().dict_get("fuelBlocks"), Some(&PyRep::None));

    // VIP guest sees ONLY ST_HUB with online accessible services {1: 1, 3: 1} (offline Clone Bay 2 omitted)
    let PyRep::Dict(vip_map) = rpc(&ctx, &mut vip_sess, "structureDirectory", "GetMyCharacterStructures", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(vip_map.len(), 1);
    assert_eq!(vip_map[0].1.dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(1)), (PyRep::Integer(3), PyRep::Integer(1))])));
    assert_eq!(vip_map[0].1.dict_get("profileID"), Some(&PyRep::None));
    assert_eq!(rpc(&ctx, &mut vip_sess, "structureDirectory", "GetMyCorporationStructures", &[]).await.unwrap(), PyRep::Dict(vec![]));

    // Market-only guest sees ST_HUB in character directory & map data with ONLY service 1 (Reprocessing 3 filtered out by service ACL!)
    let PyRep::Dict(mkt_map) = rpc(&ctx, &mut mkt_sess, "structureDirectory", "GetMyCharacterStructures", &[]).await.unwrap() else { panic!("Dict") };
    assert_eq!(mkt_map.len(), 1);
    assert_eq!(mkt_map[0].1.dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(1))])));
    let PyRep::List(map_rows) = rpc(&ctx, &mut mkt_sess, "structureDirectory", "GetStructureMapData", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap() else { panic!("List") };
    assert_eq!(map_rows.len(), 1);
    assert_eq!(map_rows[0].dict_get("itemID"), Some(&PyRep::Integer(ST_HUB)));
    assert_eq!(map_rows[0].dict_get("services"), Some(&PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(1))])));

    // Outsider gets empty results across directory, dockable, docking check, and map data RPCs
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetMyCharacterStructures", &[]).await.unwrap(), PyRep::Dict(vec![]));
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetMyDockableStructures", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap(), PyRep::List(vec![]));
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "CheckMyDockingAccessToStructures", &[PyRep::List(vec![PyRep::Integer(ST_HUB), PyRep::Integer(ST_PRIVATE)])]).await.unwrap(), PyRep::List(vec![]));
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDirectory", "GetStructureMapData", &[PyRep::Integer(i64::from(JITA_SYS))]).await.unwrap(), PyRep::List(vec![]));
}
