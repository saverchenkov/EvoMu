#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

pub mod b36 {


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const NULLSEC_SYSTEM: u32 = 30_004_759;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &args,
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

fn grant_all_combat_skills(ctx: &ServerContext, snap: &mut eve_db::WorldStateSnapshot, cid: i64, type_ids: &[u32]) {
    let sk = snap.skills.entry(cid).or_default();
    for sid in [
        3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3319, 3320, 3334, 3413,
        3418, 3426, 20327, 21071, 21667,
    ] {
        sk.insert(
            sid,
            TrainedSkillRecord {
                type_id: sid,
                level: 5,
                skill_points: 1_280_000.0,
            },
        );
    }
    for &tid in type_ids {
        if let Some(t) = ctx.sde.types.get(&tid) {
            for sa in [182_u16, 183, 184] {
                if let Some(&req_f) = t.default_attributes.get(&sa) {
                    let rsid = req_f as u32;
                    if rsid > 0 {
                        sk.insert(
                            rsid,
                            TrainedSkillRecord {
                                type_id: rsid,
                                level: 5,
                                skill_points: 1_280_000.0,
                            },
                        );
                    }
                }
            }
        }
    }
}


#[tokio::test]
async fn test_b36_combat_message_live_weapon_cycle() {
    // parity: notify:OnCombatMessage
    eve_testkit::attest(&["notify:OnCombatMessage"]);

    let ctx = new_ctx();
    let mut s1 = pilot(&ctx, 914, "demo", "demo-secret", P1).await;
    let _s2 = pilot(&ctx, 915, "other", "other-secret", P2).await;
    let kw = HashMap::new();

    let ship_p1 = 970_001_i64;
    let ship_p2 = 970_002_i64;
    let gun_p1 = 970_010_i64;
    let ammo_p1 = 970_011_i64;
    let lml_p1 = 970_012_i64;
    let missile_p1 = 970_013_i64;

    let light_missile_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 384 && t.published)
        .map(|t| t.type_id)
        .unwrap_or(213);

    let fw_sys = ctx
        .db
        .with_snapshot(|s| s.fw_system_occupations.keys().copied().min().unwrap_or(NULLSEC_SYSTEM));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.solar_system_id = fw_sys;
            c1.station_id = None;
            c1.active_ship_id = ship_p1;
            c1.war_faction_id = Some(500_001);
        }
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.solar_system_id = fw_sys;
            c2.station_id = None;
            c2.active_ship_id = ship_p2;
            c2.war_faction_id = Some(500_004);
        }
        for (sid, owner) in [(ship_p1, P1), (ship_p2, P2)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 639,
                    owner_id: owner,
                    location_id: i64::from(fw_sys),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            gun_p1,
            InventoryItemRecord {
                item_id: gun_p1,
                type_id: 2961,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ammo_p1,
            InventoryItemRecord {
                item_id: ammo_p1,
                type_id: 21894,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot0,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            lml_p1,
            InventoryItemRecord {
                item_id: lml_p1,
                type_id: 2404,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            missile_p1,
            InventoryItemRecord {
                item_id: missile_p1,
                type_id: light_missile_tid,
                owner_id: P1,
                location_id: ship_p1,
                flag: InventoryFlag::HiSlot1,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(gun_p1, true);
        snap.module_online_states.insert(lml_p1, true);
        grant_all_combat_skills(&ctx, snap, P1, &[639, 2961, 21894, 2404, light_missile_tid]);
    });
    ctx.invalidate_ship_fitting(ship_p1);
    ctx.invalidate_ship_fitting(ship_p2);

    s1.solar_system_id = Some(fw_sys);
    s1.station_id = None;
    s1.ship_id = Some(ship_p1);

    let sys_h = ctx.systems.get_or_spawn(fw_sys);
    for (sid, pos) in [(ship_p1, DVec3::ZERO), (ship_p2, DVec3::new(3_000.0, 0.0, 0.0))] {
        let mut cs = ctx.build_type_combat_state(639);
        cs.signature_radius = 450.0;
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 639,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 100_000_000.0,
                    radius: 250.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    // Negative check: activating weapon before target lock fails and emits no OnCombatMessage
    assert!(ctx
        .dispatch_rpc(
            &mut s1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(gun_p1), PyRep::Integer(ship_p2)],
            &kw,
            None,
        )
        .await
        .is_err());
    assert!(events(&ctx, P1, "OnCombatMessage").is_empty());
    assert!(events(&ctx, P2, "OnCombatMessage").is_empty());

    // Lock target via dogmaIM.AddTarget + SystemActor ticks
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ship_p2)], &kw, None)
        .await
        .expect("AddTarget ship_p2");
    for _ in 0..30 {
        if sys_h.get_target_locks(ship_p1).await.unwrap().iter().any(|l| l.target_id == ship_p2 && l.locked) {
            break;
        }
        let _ = sys_h.tick().await;
    }

    // 1. Live turret cycle -> TacticalSvc.OnCombatMessage(messageName, messageArgs) on P1 and P2
    ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_p1), PyRep::Integer(ship_p2)], &kw, None)
        .await
        .expect("Activate weapon on locked ship_p2");
    let _ = sys_h.tick().await;

    let atk_ev = events(&ctx, P1, "OnCombatMessage");
    assert!(!atk_ev.is_empty(), "Attacker P1 must receive OnCombatMessage from live weapon cycle");
    assert_eq!(atk_ev[0].len(), 2);
    assert!(matches!(atk_ev[0][0].as_str(), Some("AttackHit" | "AttackMiss")));
    assert_eq!(atk_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p2)));
    assert_eq!(atk_ev[0][1].dict_get("type"), Some(&PyRep::Integer(2961)));
    assert_eq!(atk_ev[0][1].dict_get("weapon"), Some(&PyRep::Integer(2961)));
    assert!(atk_ev[0][1].dict_get("damage").is_some());
    assert!(atk_ev[0][1].dict_get("hitQualityText").and_then(|v| v.as_str()).is_some());

    let vic_ev = events(&ctx, P2, "OnCombatMessage");
    assert!(!vic_ev.is_empty(), "Victim P2 must receive OnCombatMessage from live weapon cycle");
    assert_eq!(vic_ev[0].len(), 2);
    assert!(matches!(vic_ev[0][0].as_str(), Some("AttackedBy" | "AttackMissBy")));
    assert_eq!(vic_ev[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p1)));
    assert_eq!(vic_ev[0][1].dict_get("type"), Some(&PyRep::Integer(2961)));

    // 2. `F-01` regression: live missile launcher impact (`combat_missiles_aoe.rs` passes `victim_char_opt = None`!)
    // Verify `resolved_victim` delivers `OnCombatMessage`, `OnDamageMessage`, and `OnDamageMessages` to victim P2!
    let _ = ctx.drain_notifications_for_char(P1);
    let _ = ctx.drain_notifications_for_char(P2);

    let m_res = ctx
        .dispatch_rpc(&mut s1, "dogmaIM", None, "Activate", &[PyRep::Integer(lml_p1), PyRep::Integer(ship_p2)], &kw, None)
        .await
        .expect("Activate Light Missile Launcher II on locked ship_p2");
    assert_eq!(m_res.dict_get("impacted"), Some(&PyRep::Bool(true)), "In-flight missile must impact ship_p2");

    let vic_missile_combat = events(&ctx, P2, "OnCombatMessage");
    assert_eq!(vic_missile_combat.len(), 1, "Victim P2 must receive OnCombatMessage when caller passes victim_char_opt = None");
    assert_eq!(vic_missile_combat[0][0].as_str(), Some("AttackedBy"));
    assert_eq!(vic_missile_combat[0][1].dict_get("specialObject"), Some(&PyRep::Integer(ship_p1)));
    assert_eq!(vic_missile_combat[0][1].dict_get("type"), Some(&PyRep::Integer(i64::from(light_missile_tid))));
    assert!(!events(&ctx, P2, "OnDamageMessage").is_empty(), "Victim P2 must receive OnDamageMessage when victim_char_opt = None");
    assert!(!events(&ctx, P2, "OnDamageMessages").is_empty(), "Victim P2 must receive OnDamageMessages when victim_char_opt = None");
}

}
