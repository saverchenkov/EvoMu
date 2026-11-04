//! Live Sleeper behaviour by wormhole class with zero direct snapshot seeding.
//!
//! Sleepers are spawned by the production combat-anomaly seeding path (`scanSvc.GetCombatAnomalyInstances`)
//! after a GM pilot travels into a real w-space system; behaviour is observed through the live NPC AI tick.

#![forbid(unsafe_code)]

use crate::esi_fw_sov_incursions_live::{rpc, select_gm_pilot, slash};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;

fn wh_class_of(ctx: &ServerContext, sys_id: u32) -> Option<u8> {
    let s = ctx.sde.solar_systems.get(&sys_id)?;
    ctx.sde
        .constellations
        .get(&s.constellation_id)
        .and_then(|c| c.wormhole_class_id)
        .or_else(|| ctx.sde.regions.get(&s.region_id).and_then(|r| r.wormhole_class_id))
        .map(|c| c as u8)
}

fn first_system_of_class(ctx: &ServerContext, class: u8) -> u32 {
    ctx.sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&id| {
            id > 31_000_006
                && wh_class_of(ctx, id) == Some(class)
                && eve_server::services::pve_exploration::wormhole_class_for_system(id) == Some(class)
                && !eve_server::services::wormhole_lifecycle_ops::system_wormhole_profile(id).2
        })
        .min()
        .unwrap_or_else(|| panic!("SDE C{class} wormhole system"))
}

/// Travels into `sys_id`, triggers live anomaly seeding, parks the ship 5 km from the first Sleeper,
/// runs NPC AI ticks, and returns `(sleeper_entities, ship_scramble_strength)`.
async fn enter_and_engage(ctx: &ServerContext, sid: u64, sys_id: u32) -> (Vec<eve_server::services::combat_crimewatch::ExtendedNpcEntity>, i32) {
    let mut sess = select_gm_pilot(ctx, sid, "demo", "demo-secret", PILOT_1).await;
    slash(ctx, &mut sess, &format!("/tr {sys_id}")).await;
    rpc(ctx, &mut sess, "scanSvc", "GetCombatAnomalyInstances", &[PyRep::Integer(i64::from(sys_id))])
        .await
        .expect("GetCombatAnomalyInstances");
    let sleepers: Vec<eve_server::services::combat_crimewatch::ExtendedNpcEntity> = eve_server::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        let mut v: Vec<_> = rt.npc_entities.values().filter(|n| n.solar_system_id == sys_id && n.archetype == "Sleeper").cloned().collect();
        v.sort_by_key(|n| n.ball_id);
        v
    });
    assert!(!sleepers.is_empty(), "live seeding spawns Sleepers in C{:?}", wh_class_of(ctx, sys_id));
    let ship = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let anchor = sys.get_ball_snapshot(sleepers[0].ball_id).await.unwrap().expect("sleeper ball").1.position;
    sys.set_ball_position(ship, anchor + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let mut scram = 0;
    for _ in 0..6 {
        eve_server::sp6_lifecycle::tick_background_cluster(ctx).await;
        let npc_tackled = eve_server::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.npc_scrambled_ships.get(&sys_id).is_some_and(|s| s.contains(&ship))
        });
        let cs_scram = sys.get_combat_state(ship).await.unwrap().map(|cs| cs.warp_scramble_strength).unwrap_or(0);
        if npc_tackled {
            scram = scram.max(cs_scram.max(1));
        }
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }
    (sleepers, scram)
}

#[tokio::test]
async fn test_sleeper_reps_from_sde_and_scram_only_from_class_three_up() {
    // parity: feat:npc.sleeper-ewar-by-wormhole-class
    eve_testkit::attest(&["feat:npc.sleeper-ewar-by-wormhole-class"]);

    // C1 negative case: Sleepers in class 1 w-space never warp scramble.
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let c1 = first_system_of_class(&ctx, 1);
    let (c1_sleepers, c1_scram) = enter_and_engage(&ctx, 77_001, c1).await;
    assert!(c1_sleepers.iter().all(|n| n.sleeper_wh_class == 1), "class comes from the SDE, not a C3 fallback");
    assert_eq!(c1_scram, 0, "C1 Sleepers must not scramble");

    // C3 positive case on a fresh cluster: Sleepers scramble a ship inside scram range.
    let ctx3 = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx3);
    let c3 = first_system_of_class(&ctx3, 3);
    let (c3_sleepers, c3_scram) = enter_and_engage(&ctx3, 77_002, c3).await;
    assert!(c3_sleepers.iter().all(|n| n.sleeper_wh_class == 3));
    assert!(c3_scram > 0, "C3+ Sleepers scramble targets inside scram range");

    // Remote repair amounts equal each Sleeper type's SDE shieldBonus (68) / armorDamageAmount (84).
    for n in c1_sleepers.iter().chain(c3_sleepers.iter()) {
        let attr = |id: u16| ctx.sde.types.get(&n.type_id).and_then(|t| t.default_attributes.get(&id).copied()).unwrap_or(0.0);
        assert_eq!(n.remote_shield_rep, attr(68), "Sleeper {} shield rep from SDE attr 68", n.type_id);
        assert_eq!(n.remote_armor_rep, attr(84), "Sleeper {} armor rep from SDE attr 84", n.type_id);
    }
}
