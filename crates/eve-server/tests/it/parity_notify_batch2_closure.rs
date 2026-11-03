//! End-to-end integration tests for the 7 remediated `notify:*` parity rows:
//! - `notify:DoSimClockRebase`
//! - `notify:OnDisapprovalTimerUpdate`
//! - `notify:OnDungeonInstanceAdded`
//! - `notify:OnDungeonInstanceRemoved`
//! - `notify:OnFacWarBunkerVulnerable`
//! - `notify:OnInfluenceUpdate`
//! - `notify:OnInvalidateCacheForLocations`

#![forbid(unsafe_code)]

use crate::common;
use crate::notify_real_emitters_batch7::{login_in_space, make_item};
use eve_db::{InventoryFlag, TrainedSkillRecord};
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::system_actor::SystemHandle;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const SYS_AMARR: u32 = 30_002_187;
const SYS_ZARZAKH: u32 = 30_100_000;
const STATION_JITA: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

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

fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find_map(|(k, v)| if k == key { Some(v) } else { None }),
        PyRep::Object { args, .. } => kv_get(args, key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        }),
        _ => None,
    }
}

fn fit_guns_and_skills(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    gun_base: i64,
    gun_type: u32,
    ammo_type: u32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, flag) in [
            InventoryFlag::HiSlot0,
            InventoryFlag::HiSlot1,
            InventoryFlag::HiSlot2,
            InventoryFlag::HiSlot3,
            InventoryFlag::HiSlot4,
            InventoryFlag::HiSlot5,
            InventoryFlag::HiSlot6,
            InventoryFlag::HiSlot7,
        ]
        .into_iter()
        .enumerate()
        {
            let gid = gun_base + (idx as i64) * 2;
            snap.items
                .insert(gid, make_item(gid, gun_type, char_id, ship_id, flag, 1, true));
            snap.items
                .insert(gid + 1, make_item(gid + 1, ammo_type, char_id, ship_id, flag, 25_000, false));
            snap.module_online_states.insert(gid, true);
        }
        let shield_id = gun_base + 100;
        snap.items.insert(shield_id, make_item(shield_id, 3841, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.module_online_states.insert(shield_id, true);
        let dc_id = gun_base + 110;
        snap.items.insert(dc_id, make_item(dc_id, 2048, char_id, ship_id, InventoryFlag::LoSlot0, 1, true));
        snap.module_online_states.insert(dc_id, true);
        let low_mod = if gun_type == 47_272 || gun_type == 47_920 { 1244 } else if matches!(gun_type, 2404 | 2420 | 20_539 | 2929 | 2961 | 34288) { 22_291 } else if gun_type == 3074 || gun_type == 3_146 || gun_type == 3186 { 10_190 } else { 519 };
        for (idx, flag) in [
            InventoryFlag::LoSlot1,
            InventoryFlag::LoSlot2,
            InventoryFlag::LoSlot3,
            InventoryFlag::LoSlot4,
            InventoryFlag::LoSlot5,
            InventoryFlag::LoSlot6,
            InventoryFlag::LoSlot7,
        ]
        .into_iter()
        .enumerate()
        {
            let gyro_id = gun_base + 111 + idx as i64;
            snap.items.insert(gyro_id, make_item(gyro_id, low_mod, char_id, ship_id, flag, 1, true));
            snap.module_online_states.insert(gyro_id, true);
        }
        for (idx, flag) in [
            InventoryFlag::MedSlot1,
            InventoryFlag::MedSlot2,
            InventoryFlag::MedSlot3,
        ]
        .into_iter()
        .enumerate()
        {
            let cap_id = gun_base + 120 + idx as i64;
            snap.items.insert(cap_id, make_item(cap_id, 2032, char_id, ship_id, flag, 1, true));
            snap.module_online_states.insert(cap_id, true);
        }
        for &tid in &[
            3300_u32, 3301, 3302, 3303, 3307, 3308, 3310, 3311, 3312, 3315, 3317, 3319, 3320, 3321,
            3324, 3325, 3333, 3336, 3337, 11_082, 12_209, 12_212, 20_312, 20_327, 21_071, 25_719,
        ] {
            snap.skills.entry(char_id).or_default().insert(
                tid,
                TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
}

async fn destroy_npc_ball(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_handle: &SystemHandle,
    npc_id: i64,
    gun_base: i64,
    effect_name: &str,
    offset_dist: f64,
    step_secs: f64,
) {
    let Some((_, npc_ball)) = sys_handle.get_ball_snapshot(npc_id).await.unwrap() else {
        return;
    };
    if let Some(sys_id) = sess.solar_system_id {
        if let Ok(beyonce_oid) = ctx.bind_object(sess, "beyonce", i64::from(sys_id)) {
            let _ = ctx
                .dispatch_rpc(sess, "beyonce", Some(beyonce_oid), "CmdStop", &[], &HashMap::new(), None)
                .await;
        }
    }
    if let Some(ship_id) = sess.ship_id {
        let _ = sys_handle
            .set_ball_position(ship_id, npc_ball.position + DVec3::new(offset_dist, 0.0, 0.0))
            .await;
    }
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(npc_id)])
        .await
        .expect("AddTarget NPC");
    sys_handle.step_manual(5.0).await.unwrap();
    for idx in 0..8_i64 {
        let gid = gun_base + idx * 2;
        if ctx.db.with_snapshot(|s| s.items.contains_key(&gid)) {
            let _ = rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gid), PyRep::String(effect_name.into())]).await;
        }
    }
    sys_handle.step_manual(5.0).await.unwrap();
    let mut sync_weapons = false;
    for idx in 0..8_i64 {
        let gid = gun_base + idx * 2;
        if !ctx.db.with_snapshot(|s| s.items.contains_key(&gid)) {
            continue;
        }
        let res = rpc(
            ctx, sess, "dogmaIM", "Activate",
            &[PyRep::Integer(gid), PyRep::String(effect_name.into()), PyRep::Integer(npc_id), PyRep::Integer(1000)],
        ).await.unwrap_or_else(|e| panic!("Activate gun {idx} on NPC failed: {e:?}"));
        if kv_get(&res, "appliedDamage").is_some() {
            sync_weapons = true;
        }
    }
    let mut last_dmg = 0.0_f64;
    let mut last_hit = 0.0_f64;
    for _ in 0..800 {
        if sys_handle.get_ball_snapshot(npc_id).await.unwrap().is_none() {
            break;
        }
        sys_handle.step_manual(step_secs).await.unwrap();
        let Some((_, nb)) = sys_handle.get_ball_snapshot(npc_id).await.unwrap() else {
            break;
        };
        if let Some(sid) = sess.ship_id {
            let _ = sys_handle.set_ball_position(sid, nb.position + DVec3::new(offset_dist, 0.0, 0.0)).await;
        }
        if sync_weapons {
            let _ = rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(npc_id)]).await;
            for idx in 0..6_i64 {
                let gid = gun_base + idx * 2;
                let _ = rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gid), PyRep::String(effect_name.into())]).await;
                let res = rpc(
                    ctx, sess, "dogmaIM", "Activate",
                    &[PyRep::Integer(gid), PyRep::String(effect_name.into()), PyRep::Integer(npc_id), PyRep::Integer(1)],
                ).await.expect("Activate gun in loop");
                if let Some(d) = kv_get(&res, "appliedDamage").and_then(|v| v.as_f64()) {
                    last_dmg = d;
                }
                if let Some(h) = kv_get(&res, "hitChance").and_then(|v| v.as_f64()) {
                    last_hit = h;
                }
            }
        }
    }
    let rem_cs = sys_handle.get_combat_state(npc_id).await.ok().flatten();
    assert!(
        sys_handle.get_ball_snapshot(npc_id).await.unwrap().is_none(),
        "NPC {npc_id} must be destroyed via live combat (last_dmg={last_dmg}, last_hit={last_hit}, rem_cs={rem_cs:?})"
    );
    for idx in 0..8_i64 {
        let gid = gun_base + idx * 2;
        if ctx.db.with_snapshot(|s| s.items.contains_key(&gid)) {
            let _ = rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gid), PyRep::String(effect_name.into())]).await;
        }
    }
}

#[tokio::test]
async fn test_batch2_sim_clock_rebase_disapproval_and_cache_invalidation() {
    // parity: notify:DoSimClockRebase, notify:OnDisapprovalTimerUpdate, notify:OnInvalidateCacheForLocations
    eve_testkit::attest(&[
        "notify:DoSimClockRebase",
        "notify:OnDisapprovalTimerUpdate",
        "notify:OnInvalidateCacheForLocations",
    ]);

    let ctx = new_ctx();

    // 1. DoSimClockRebase via GM /tidi slash command in Jita space
    let mut sess_jita = login_in_space(
        &ctx, 92_001, "demo", "demo-secret", PILOT_A, 720_000_001, 587, SYS_JITA, DVec3::ZERO,
    ).await;
    sess_jita.role |= ROLE_GM_DEFAULT;
    let _sess_amarr = login_in_space(
        &ctx, 92_002, "other", "other-secret", PILOT_B, 720_000_002, 587, SYS_AMARR, DVec3::ZERO,
    ).await;
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    rpc(&ctx, &mut sess_jita, "slash", "SlashCmd", &[PyRep::String("/tidi 0.50".into())])
        .await
        .expect("/tidi 0.50");

    let jita_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let rebase = jita_notifs
        .iter()
        .find(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase emitted to Jita pilot");
    assert_eq!(rebase.args.len(), 1);
    let PyRep::Tuple(pair) = &rebase.args[0] else {
        panic!("expected 2-tuple in DoSimClockRebase, got {:?}", rebase.args);
    };
    assert_eq!(pair.len(), 2);
    assert!(pair[0].as_i64().expect("oldSimTime") < pair[1].as_i64().expect("newSimTime"));
    assert!(
        ctx.drain_notifications_for_char(PILOT_B).iter().all(|n| n.event != "DoSimClockRebase"),
        "Amarr pilot must not receive Jita DoSimClockRebase"
    );

    // 2. OnDisapprovalTimerUpdate via live weapon aggression in Zarzakh + cluster tick
    let (ship_z1, ship_z2, gun_z) = (720_000_101_i64, 720_000_102_i64, 720_000_110_i64);
    let mut sess_z1 = login_in_space(
        &ctx, 92_003, "demo", "demo-secret", PILOT_A, ship_z1, 587, SYS_ZARZAKH, DVec3::ZERO,
    ).await;
    let _sess_z2 = login_in_space(
        &ctx, 92_004, "other", "other-secret", PILOT_B, ship_z2, 587, SYS_ZARZAKH, DVec3::new(1_000.0, 0.0, 0.0),
    ).await;
    fit_guns_and_skills(&ctx, PILOT_A, ship_z1, gun_z, 3074, 12_625);
    let sys_z = ctx.systems.get_or_spawn(SYS_ZARZAKH);
    rpc(&ctx, &mut sess_z1, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_z2)]).await.expect("AddTarget");
    sys_z.step_manual(5.0).await.unwrap();
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    rpc(
        &ctx, &mut sess_z1, "dogmaIM", "Activate",
        &[PyRep::Integer(gun_z), PyRep::String("projectileFired".into()), PyRep::Integer(ship_z2)],
    ).await.expect("Activate weapon in Zarzakh");

    let dis_active = ctx.drain_notifications_for_char(PILOT_A)
        .into_iter()
        .rfind(|n| n.event == "OnDisapprovalTimerUpdate")
        .expect("OnDisapprovalTimerUpdate active (501)");
    assert_eq!(dis_active.args.len(), 2);
    assert_eq!(dis_active.args[0], PyRep::Integer(501));
    assert!(dis_active.args[1].as_i64().unwrap_or(0) > 0);

    // Advance 2s + cluster tick -> countdown state (502)
    ctx.advance_secs(2);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let dis_cd = ctx.drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnDisapprovalTimerUpdate")
        .expect("OnDisapprovalTimerUpdate countdown (502)");
    assert_eq!(dis_cd.args[0], PyRep::Integer(502));
    assert!(dis_cd.args[1].as_i64().unwrap_or(0) > 0);

    // Advance past 900s expiry + cluster tick -> idle state (500, None)
    ctx.advance_secs(905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let dis_idle = ctx.drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnDisapprovalTimerUpdate")
        .expect("OnDisapprovalTimerUpdate idle (500)");
    assert_eq!(dis_idle.args, vec![PyRep::Integer(500), PyRep::None]);

    // 3. OnInvalidateCacheForLocations via bound invbroker.SetPassword
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(STATION_JITA);
            ch.solar_system_id = SYS_JITA;
        }
    });
    let mut sess_st = SessionState::new(92_005);
    common::login_pilot(&ctx, &mut sess_st, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_st, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)]).await.expect("SelectCharacterID");
    sess_st.station_id = Some(STATION_JITA);
    sess_st.solar_system_id = None;
    ctx.sync_session(&mut sess_st);
    let container_id: i64 = 720_300_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            make_item(container_id, 3465, PILOT_A, STATION_JITA, InventoryFlag::Hangar, 1, true),
        );
    });
    let bind = rpc(
        &ctx, &mut sess_st, "invbroker", "MachoBindObject",
        &[PyRep::Tuple(vec![PyRep::Integer(STATION_JITA), PyRep::Integer(15)]), PyRep::None],
    ).await.expect("MachoBindObject");
    let mgr_oid = match &bind {
        PyRep::Tuple(parts) => parts[0].as_i64().expect("mgr_oid") as u64,
        _ => panic!("expected tuple"),
    };
    let cont_oid = ctx.dispatch_rpc(
        &mut sess_st, "invbroker", Some(mgr_oid), "GetInventoryFromId",
        &[PyRep::Integer(container_id), PyRep::Integer(0)], &HashMap::new(), None,
    ).await.expect("GetInventoryFromId").as_i64().expect("cont_oid") as u64;
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    ctx.dispatch_rpc(
        &mut sess_st, "invbroker", Some(cont_oid), "SetPassword",
        &[PyRep::Integer(2), PyRep::String("alpha123".into()), PyRep::String(String::new())],
        &HashMap::new(), None,
    ).await.expect("SetPassword");

    let inv = ctx.drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnInvalidateCacheForLocations")
        .expect("OnInvalidateCacheForLocations");
    assert_eq!(inv.args, vec![PyRep::List(vec![PyRep::Integer(container_id)])]);
}

#[tokio::test]
async fn test_batch2_dungeon_instance_added_and_removed_via_live_combat() {
    // parity: notify:OnDungeonInstanceAdded, notify:OnDungeonInstanceRemoved
    eve_testkit::attest(&[
        "notify:OnDungeonInstanceAdded",
        "notify:OnDungeonInstanceRemoved",
    ]);

    let ctx = new_ctx();
    let ship_a = 720_000_201_i64;
    let gun_a = 720_000_210_i64;
    let mut sess_a = login_in_space(
        &ctx, 92_010, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO,
    ).await;
    fit_guns_and_skills(&ctx, PILOT_A, ship_a, gun_a, 3074, 12_625);

    let dest_sys = eve_server::services::pve_expeditions::find_escalation_destination_system(&ctx, SYS_JITA);
    let mut sess_obs = login_in_space(
        &ctx, 92_011, "other", "other-secret", PILOT_B, 720_000_202, 587, dest_sys, DVec3::ZERO,
    ).await;
    let _ = rpc(&ctx, &mut sess_obs, "scanSvc", "GetCombatAnomalyInstances", &[]).await.unwrap();
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    let _ = rpc(&ctx, &mut sess_a, "scanSvc", "GetCombatAnomalyInstances", &[]).await.unwrap();
    let site_id = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&SYS_JITA)
            .and_then(|l| l.iter().find(|s| s.site_category == "CombatAnomaly" && !s.cleared))
            .map(|s| s.site_id.clone())
            .expect("seeded CombatAnomaly in Jita")
    });

    ctx.set_mechanic_rng_seed(Some(3));
    let jita_handle = ctx.systems.get_or_spawn(SYS_JITA);

    for _wave in 0..6 {
        let npcs = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&SYS_JITA)
                .and_then(|l| l.iter().find(|s| s.site_id == site_id))
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        if npcs.is_empty() {
            break;
        }
        for nid in npcs {
            destroy_npc_ball(&ctx, &mut sess_a, &jita_handle, nid, gun_a, "projectileFired", 2_500.0, 6.5).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    let obs_add = ctx.drain_notifications_for_char(PILOT_B);
    let di_add = obs_add
        .iter()
        .find(|n| n.event == "OnDungeonInstanceAdded")
        .expect("OnDungeonInstanceAdded emitted on escalation");
    assert_eq!(di_add.args.len(), 2);
    assert_eq!(di_add.args[0], PyRep::Integer(i64::from(dest_sys)));
    let inst_id = kv_get(&di_add.args[1], "instanceID").and_then(|v| v.as_i64()).expect("instanceID");
    let sig_id = kv_get(&di_add.args[1], "signatureID").and_then(|v| v.as_str()).expect("signatureID").to_string();
    let exp_did = kv_get(&di_add.args[1], "dungeonID").and_then(|v| v.as_i64()).expect("dungeonID");
    assert!(kv_get(&di_add.args[1], "scanStrengthValue").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // Move Pilot A to dest_sys, enter expedition step 1, and destroy its NPCs in real combat
    let _ = ctx.db.set_character_solar_system(PILOT_A, dest_sys, &ctx.sde);
    sess_a.solar_system_id = Some(dest_sys);
    ctx.sync_session(&mut sess_a);
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    let ship_ball = jita_handle.get_ball_snapshot(ship_a).await.unwrap().unwrap().1;
    let _ = dest_handle.spawn_ball(ship_ball).await;

    let exp_site_id = format!("EXP-{inst_id}-1");
    let _ = ctx.drain_notifications_for_char(PILOT_B);
    for _wave in 0..6 {
        let npcs = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&dest_sys)
                .and_then(|l| l.iter().find(|s| s.site_id == exp_site_id))
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        if npcs.is_empty() {
            break;
        }
        for nid in npcs {
            destroy_npc_ball(&ctx, &mut sess_a, &dest_handle, nid, gun_a, "projectileFired", 2_500.0, 6.5).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let obs_rem = ctx.drain_notifications_for_char(PILOT_B);
    let di_rem = obs_rem
        .iter()
        .find(|n| n.event == "OnDungeonInstanceRemoved")
        .expect("OnDungeonInstanceRemoved emitted after destroying expedition NPCs");
    assert_eq!(
        di_rem.args,
        vec![
            PyRep::Integer(i64::from(dest_sys)),
            PyRep::Integer(inst_id),
            PyRep::String(sig_id),
            PyRep::Integer(exp_did),
        ]
    );
}

#[tokio::test]
async fn test_batch2_facwar_bunker_vulnerable_and_incursion_influence_update() {
    // parity: notify:OnFacWarBunkerVulnerable, notify:OnInfluenceUpdate
    eve_testkit::attest(&[
        "notify:OnFacWarBunkerVulnerable",
        "notify:OnInfluenceUpdate",
    ]);

    // 1. OnFacWarBunkerVulnerable via live FW complex capture reaching 100% contested
    let fw_ctx = new_ctx();
    let (_enemy_sys, warzone_sys, _cmd_sys, _rear_sys) =
        crate::fw_warzone_insurgency_closure::setup_warzone_chain(&fw_ctx);
    let (fw_ship, fw_gun) = (720_000_301_i64, 720_000_310_i64);
    fw_ctx.db.with_mut_snapshot(|snap| {
        if let Some(occ) = snap.fw_system_occupations.get_mut(&warzone_sys) {
            occ.victory_points = 0;
            occ.victory_points_threshold = 100;
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = 1_000_181;
            ch.station_id = None;
            ch.solar_system_id = warzone_sys;
        }
    });
    fit_guns_and_skills(&fw_ctx, PILOT_A, fw_ship, fw_gun, 3074, 12_625);
    let mut sess_fw = login_in_space(
        &fw_ctx, 92_020, "demo", "demo-secret", PILOT_A, fw_ship, 587, warzone_sys, DVec3::ZERO,
    ).await;
    sess_fw.role |= ROLE_GM_DEFAULT;
    rpc(&fw_ctx, &mut sess_fw, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)])
        .await
        .expect("CreateMyDirectEnlistment");
    let _ = rpc(&fw_ctx, &mut sess_fw, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.unwrap();
    let _ = rpc(&fw_ctx, &mut sess_fw, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();

    let wz_space = fw_ctx.systems.get_or_spawn(warzone_sys);
    let wz_beyonce = fw_ctx.bind_object(&sess_fw, "beyonce", i64::from(warzone_sys)).unwrap();
    let _ = fw_ctx.drain_notifications_for_char(PILOT_A);

    let mut bunker_vuln_seen = false;
    for _attempt in 0..35 {
        let gate_opt = fw_ctx.db.with_snapshot(|s| {
            s.fw_runtime.fw_complexes.iter().find_map(|(&gid, &(csys, ref tier, _, _, done))| {
                (csys == warzone_sys && tier == "Novice" && !done).then_some(gid)
            })
        });
        let Some(gate_id) = gate_opt else {
            let _ = rpc(&fw_ctx, &mut sess_fw, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await;
            continue;
        };
        fw_ctx.dispatch_rpc(
            &mut sess_fw, "beyonce", Some(wz_beyonce), "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)],
            &HashMap::new(), None,
        ).await.expect("CmdWarpToStuff FW gate");
        for _ in 0..4 {
            wz_space.step_manual(15.0).await.unwrap();
        }
        let _ = rpc(&fw_ctx, &mut sess_fw, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)])
            .await
            .expect("ActivateAccelerationGate FW");
        let guard_id = 500_000_000 + gate_id;
        rpc(&fw_ctx, &mut sess_fw, "dogmaIM", "AddTarget", &[PyRep::Integer(guard_id)])
            .await
            .expect("AddTarget FW guard");
        wz_space.step_manual(5.0).await.unwrap();
        for idx in 0..3_i64 {
            let gid = fw_gun + idx * 2;
            rpc(
                &fw_ctx,
                &mut sess_fw,
                "dogmaIM",
                "Activate",
                &[
                    PyRep::Integer(gid),
                    PyRep::String("projectileFired".into()),
                    PyRep::Integer(guard_id),
                    PyRep::Integer(100),
                ],
            )
            .await
            .expect("Activate gun on FW guard");
        }
        for _ in 0..90 {
            if wz_space.get_ball_snapshot(guard_id).await.unwrap().is_none() {
                break;
            }
            wz_space.step_manual(1.0).await.unwrap();
        }
        assert!(
            wz_space.get_ball_snapshot(guard_id).await.unwrap().is_none(),
            "FW guard NPC {guard_id} must be destroyed via live combat"
        );

        for _ in 0..10 {
            if fw_ctx.db.with_snapshot(|s| s.fw_runtime.fw_complexes.get(&gate_id).is_none_or(|c| c.4)) {
                break;
            }
            rpc(&fw_ctx, &mut sess_fw, "timeControl", "AdvanceClock", &[PyRep::Integer(150)]).await.expect("AdvanceClock");
            eve_server::services::fw_gameplay::tick_fw_gameplay(&fw_ctx, fw_ctx.now_unix()).await;
        }
        if let Some(ev) = fw_ctx.drain_notifications_for_char(PILOT_A).into_iter().find(|n| n.event == "OnFacWarBunkerVulnerable") {
            assert_eq!(ev.args.len(), 1);
            assert!(ev.args[0].as_i64().unwrap_or(0) > 0);
            bunker_vuln_seen = true;
            break;
        }
    }
    assert!(bunker_vuln_seen, "OnFacWarBunkerVulnerable must be emitted when FW complex captures reach 100% contested");

    // 2. OnInfluenceUpdate via live incursion site combat
    let inc_ctx = new_ctx();
    let mut sess_probe = SessionState {
        user_id: Some(2),
        character_id: Some(PILOT_B),
        ..Default::default()
    };
    rpc(&inc_ctx, &mut sess_probe, "taleMgr", "GetGlobalWorldEventTales", &[])
        .await
        .expect("GetGlobalWorldEventTales");

    let (tale_id, vg_sys, vg_site_id) =
        eve_server::services::pve_special_sites::with_pve_special_reg(&inc_ctx, |reg| {
            let inc = reg.incursions.values().find(|i| i.state != "Ended").expect("seeded incursion");
            let vg = inc.sites.iter().find(|s| s.tier == "Vanguard" && s.completed_count == 0).expect("Vanguard site");
            (inc.tale_id, vg.solar_system_id, vg.site_id.clone())
        });

    let (inc_ship, inc_gun) = (720_000_401_i64, 720_000_410_i64);
    inc_ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = None;
            ch.solar_system_id = vg_sys;
            ch.active_ship_id = inc_ship;
        }
    });
    let mut sess_inc = login_in_space(
        &inc_ctx, 92_030, "demo", "demo-secret", PILOT_A, inc_ship, 26_842, vg_sys, DVec3::ZERO,
    ).await;
    fit_guns_and_skills(&inc_ctx, PILOT_A, inc_ship, inc_gun, 34_288, 12_779);
    sess_inc.role |= ROLE_GM_DEFAULT;
    inc_ctx.sync_session(&mut sess_inc);
    let vg_space = inc_ctx.systems.get_or_spawn(vg_sys);
    let _ = inc_ctx.drain_notifications_for_char(PILOT_A);

    for _wave in 0..5 {
        let _ = rpc(&inc_ctx, &mut sess_inc, "keeper", "EnterNextDungeonRoom", &[PyRep::String(vg_site_id.clone())]).await;
        let (escorts, boss_id) = eve_server::services::pve_special_sites::with_pve_special_reg(&inc_ctx, |reg| {
            let vg = reg.incursions.values().flat_map(|i| i.sites.iter()).find(|s| s.site_id == vg_site_id).expect("vg site");
            let esc: Vec<i64> = vg.escort_spawns.iter().filter(|e| !e.destroyed).map(|e| e.npc_ball_id).collect();
            (esc, (!vg.boss_destroyed).then_some(vg.boss_npc_ball_id))
        });
        for eid in escorts {
            destroy_npc_ball(&inc_ctx, &mut sess_inc, &vg_space, eid, inc_gun, "projectileFired", 5_000.0, 2.0).await;
        }
        if let Some(bid) = boss_id {
            destroy_npc_ball(&inc_ctx, &mut sess_inc, &vg_space, bid, inc_gun, "projectileFired", 5_000.0, 2.0).await;
        }
        let done = eve_server::services::pve_special_sites::with_pve_special_reg(&inc_ctx, |reg| {
            reg.incursions.values().flat_map(|i| i.sites.iter()).find(|s| s.site_id == vg_site_id).is_some_and(|s| s.completed_count > 0)
        });
        if done {
            break;
        }
    }

    let inf_ev = inc_ctx.drain_notifications_for_char(PILOT_A)
        .into_iter()
        .find(|n| n.event == "OnInfluenceUpdate")
        .expect("OnInfluenceUpdate emitted on incursion site completion");
    assert_eq!(inf_ev.args.len(), 2);
    assert_eq!(inf_ev.args[0], PyRep::Integer(tale_id));
    let inf_val = kv_get(&inf_ev.args[1], "influence").and_then(|v| v.as_f64()).expect("influence");
    assert!((0.0..1.0).contains(&inf_val), "influence {inf_val} must decrease below 1.0");
    assert!(kv_get(&inf_ev.args[1], "lastUpdated").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
}
