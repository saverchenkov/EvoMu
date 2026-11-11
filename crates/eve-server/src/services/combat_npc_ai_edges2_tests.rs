#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{ExtendedNpcEntity, with_combat_runtime, with_mut_combat_runtime};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode, WarpPhase};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn clone_sde_type(
    ctx: &mut ServerContext,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    meta_group_id: Option<u32>,
) {
    let reg = Arc::make_mut(&mut ctx.sde);
    let mut rec: InvType = reg
        .types
        .get(&base_tid)
        .cloned()
        .unwrap_or_else(|| reg.types.values().next().unwrap().clone());
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = name.into();
    rec.meta_group_id = meta_group_id;
    reg.types.insert(new_tid, rec);
}

fn make_npc(ball_id: i64, sys_id: u32, type_id: u32, archetype: &str, pos: DVec3) -> ExtendedNpcEntity {
    ExtendedNpcEntity {
        ball_id,
        type_id,
        faction_id: 500_010,
        archetype: archetype.into(),
        solar_system_id: sys_id,
        spawn_group_id: 0,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 50_000.0,
        optimal_range: 30_000.0,
        orbit_range: 10_000.0,
        damage_vector: DamageVector {
            em: 10.0,
            thermal: 10.0,
            kinetic: 10.0,
            explosive: 10.0,
        },
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: 0.0,
        web_range: 0.0,
        scram_strength: 0.0,
        scram_range: 0.0,
        neut_amount: 0.0,
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: pos,
        sleeper_wh_class: 0,
        custom_cargo_loot: None,
    }
}

#[test]
fn derive_archetype_all_branches_and_edencom_drone_hostility() {
    let mut ctx = test_ctx();

    assert_eq!(derive_npc_archetype(&ctx, 41_589), "TransmissionRelay");

    let samples = [
        (951_001_u32, 999, 11, "Sleeper Emergent Patroller", None, "Sleeper"),
        (951_002, 287, 11, "Rogue Drone Alpha", None, "RogueDrone"),
        (951_003, 1956, 11, "Drifter Battleship", None, "Drifter"),
        (951_004, 100, 11, "Triglavian Damavik", None, "Triglavian"),
        (951_005, 4637, 11, "Guristas Insurgency Roamer", None, "InsurgencyPirateRoamer"),
        (951_006, 100, 11, "Commander Sansha", Some(4), "Officer"),
        (951_007, 100, 11, "Outuni Mesmerizer", None, "IncursionSansha"),
        (951_008, 4034, 11, "EDENCOM Patrol", None, "EDENCOM"),
    ];
    for &(tid, gid, cid, name, meta, expected) in &samples {
        clone_sde_type(&mut ctx, 587, tid, gid, cid, name, meta);
        assert_eq!(derive_npc_archetype(&ctx, tid), expected, "type {tid} {name}");
    }

    let cid = 90_000_001_i64;
    let drone_id = 9_700_001_i64;
    let mut eden_npc = make_npc(9_700_002, 30_000_142, 951_008, "EDENCOM", DVec3::ZERO);
    eden_npc.faction_id = 500_027;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454,
                owner_id: cid,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.standings.insert(500_027, -2.0);
        }
    });
    assert!(is_candidate_hostile_to_edencom(&ctx, &eden_npc, drone_id, 0));
    assert!(!is_candidate_hostile_to_edencom(&ctx, &eden_npc, 999_888_777, 0));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.standings.insert(500_027, 1.0);
        }
    });
    assert!(!is_candidate_hostile_to_edencom(&ctx, &eden_npc, drone_id, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_ai_sync_target_pruning_ecm_capsule_sleepers_zarzakh_and_threat_events() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let cid = 90_000_001_i64;
    let ship_id = 9_710_001_i64;
    let cap_id = 9_710_002_i64;

    step_extended_npc_ai_sync(&ctx, 30_002_187);

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, pos) in [
        (ship_id, 587_u32, DVec3::new(5_000.0, 0.0, 0.0)),
        (cap_id, 670_u32, DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: tid,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        let _ = sys_h
            .spawn_ball(Ball {
                id: bid,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let missing_ball_npc = 9_720_001_i64;
    let warp_id = 9_720_002_i64;
    let relay_id = 9_720_003_i64;
    let police_id = 9_720_004_i64;
    let gate_gun_id = 9_720_005_i64;
    let drifter_id = 9_720_006_i64;

    for (nid, pos) in [
        (warp_id, DVec3::new(2_000.0, 0.0, 0.0)),
        (relay_id, DVec3::new(3_000.0, 0.0, 0.0)),
        (police_id, DVec3::new(1_200.0, 0.0, 0.0)),
        (gate_gun_id, DVec3::new(4_500.0, 0.0, 0.0)),
        (drifter_id, DVec3::new(5_500.0, 0.0, 0.0)),
    ] {
        let mode = if nid == warp_id {
            MovementMode::Warp {
                origin: pos,
                destination: DVec3::new(1_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 1_000_000.0,
                phase: WarpPhase::Cruising,
                elapsed_warp_time: 1.0,
            }
        } else {
            MovementMode::Stop
        };
        let _ = sys_h
            .spawn_ball(Ball {
                id: nid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode,
            })
            .await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.security_status = -5.0;
        }
    });

    with_mut_combat_runtime(&ctx, |rt| {
        // missing_ball_npc is NOT spawned in ballpark, so step_extended_npc_ai removes it
        rt.npc_entities.insert(
            missing_ball_npc,
            make_npc(missing_ball_npc, sys_id, 587, "Pirate", DVec3::ZERO),
        );

        let mut warp = make_npc(warp_id, sys_id, 587, "Pirate", DVec3::new(2_000.0, 0.0, 0.0));
        warp.target_id = Some(ship_id);
        rt.npc_entities.insert(warp_id, warp);

        rt.npc_entities.insert(
            relay_id,
            make_npc(relay_id, sys_id, 41_589, "TransmissionRelay", DVec3::new(3_000.0, 0.0, 0.0)),
        );

        let mut pol = make_npc(police_id, sys_id, 587, "Police", DVec3::new(1_200.0, 0.0, 0.0));
        pol.target_id = Some(cap_id);
        rt.npc_entities.insert(police_id, pol);

        let mut gg = make_npc(gate_gun_id, sys_id, 587, "ZarzakhGateGun", DVec3::new(4_500.0, 0.0, 0.0));
        gg.ecm_jammed = true;
        gg.target_id = Some(ship_id);
        rt.npc_entities.insert(gate_gun_id, gg);

        let mut dr = make_npc(drifter_id, sys_id, 587, "Drifter", DVec3::new(5_500.0, 0.0, 0.0));
        dr.drifter_overshield_hp = 500.0;
        dr.threat_table.insert(ship_id, 50.0);
        rt.npc_entities.insert(drifter_id, dr);
    });

    step_extended_npc_ai(&ctx, sys_id).await;

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.npc_entities.contains_key(&missing_ball_npc));
        assert_eq!(rt.npc_entities[&warp_id].target_id, None);
        assert_ne!(rt.npc_entities[&police_id].target_id, Some(cap_id));
    });

    record_npc_threat_event(&ctx, drifter_id, ship_id, 100.0, false, false);
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.npc_entities[&drifter_id].drifter_overshield_hp < 500.0);
    });

    let hint_res = destroy_specialized_npc_with_hint(
        &ctx,
        sys_id,
        9_799_999,
        cid,
        Some((587, DVec3::new(1_000.0, 0.0, 0.0))),
    )
    .await;
    assert!(hint_res.0 > 0);

    let clone_id = 9_720_010_i64;
    let _ = sys_h
        .spawn_ball(Ball {
            id: clone_id,
            type_id: 587,
            position: DVec3::new(6_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    with_mut_combat_runtime(&ctx, |rt| {
        let mut cs = make_npc(clone_id, sys_id, 587, "CloneSoldier", DVec3::new(6_000.0, 0.0, 0.0));
        cs.custom_cargo_loot = Some((34, 10));
        rt.npc_entities.insert(clone_id, cs);
    });
    let (wreck_id, bounty, _) = destroy_specialized_npc_and_queue_bounty(&ctx, sys_id, clone_id, cid).await;
    assert!(wreck_id > 0);
    assert!(bounty > 0.0);
}
