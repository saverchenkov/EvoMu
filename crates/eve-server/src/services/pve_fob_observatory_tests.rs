//! Unit and integration tests for `pve_fob_observatory`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::types::FleetRecord;
use eve_db::WorldRepository;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pirate_fob_spawn_defense_live_damage_and_payout() {
    let sys_id: SolarSystemId = 30_000_142;
    let pilot1: i64 = 90_040_301;
    let pilot2: i64 = 90_040_302;
    let ship1: i64 = 1_040_000_301;
    let ship2: i64 = 1_040_000_302;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("fob_p1", "pw", pilot1, "FobP1");
    db.provision_gm_account("fob_p2", "pw", pilot2, "FobP2");
    db.with_mut_snapshot(|snap| {
        for (cid, sid, bal) in [(pilot1, ship1, 100.0), (pilot2, ship2, 200.0)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.active_ship_id = sid;
                ch.balance = eve_db::IskCents::from_f64_isk(bal);
            }
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_500_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Spawn Pirate FOB (Blood Raiders 500_012)
    let spawned = spawn_pirate_fob(&ctx, sys_id, 500_012)
        .await
        .expect("spawn_pirate_fob succeeds");
    assert!(!matches!(spawned, PyRep::None));

    let (fob_id, fob_pos) = ctx.db.with_snapshot(|snap| {
        let st = snap
            .structures
            .values()
            .find(|s| s.solar_system_id == sys_id)
            .expect("FOB structure persisted");
        (st.structure_id, DVec3::from_array(st.position))
    });

    for (cid, sid) in [(pilot1, ship1), (pilot2, ship2)] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: fob_pos + DVec3::new(15_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        record_fob_combat_participant(&ctx, fob_id, cid);
    }

    // Trigger defense fleet response
    let def_res = trigger_fob_defense_response(&ctx, fob_id, &[ship1, ship2])
        .await
        .expect("defense response succeeds");
    assert!(!matches!(def_res, PyRep::None));

    // Apply partial live combat damage
    let (dmg_applied, destroyed, _, _, _, _) =
        apply_live_combat_damage_to_pirate_fob(&ctx, sys_id, &sys_handle, pilot1, fob_id, 10_000.0)
            .await;
    assert_eq!(dmg_applied, 10_000.0);
    assert!(!destroyed);

    // Apply massive lethal damage -> destroys FOB and pays bounties to both participants
    let (_, lethal_destroyed, s_r, a_r, h_r, _) = apply_live_combat_damage_to_pirate_fob(
        &ctx,
        sys_id,
        &sys_handle,
        pilot2,
        fob_id,
        500_000_000.0,
    )
    .await;
    assert!(lethal_destroyed);
    assert_eq!(s_r, 0.0);
    assert_eq!(a_r, 0.0);
    assert_eq!(h_r, 0.0);

    let (bal1, bal2) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters[&pilot1].balance,
            snap.characters[&pilot2].balance,
        )
    });
    assert!(bal1 > eve_db::IskCents::from_f64_isk(100.0));
    assert!(bal2 > eve_db::IskCents::from_f64_isk(200.0));

    // Step living world spawner across non-empty universe
    step_living_world_fob_and_observatory_spawns(&ctx).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observatory_flashpoint_access_gate_and_boss_completion() {
    let mut sde = (*shared_sde()).clone();
    let subcap_tid: u32 = 626;
    if let Some(mut ship_t) = sde.types.values().next().cloned() {
        ship_t.type_id = subcap_tid;
        ship_t.category_id = 6;
        ship_t.group_id = 26;
        ship_t.name = "Caracal".into();
        ship_t.published = true;
        sde.types.insert(subcap_tid, ship_t);
    }
    let sde = Arc::new(sde);
    let sys_id: SolarSystemId = sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE has Pochven solar system");
    let p1: i64 = 90_040_401;
    let p2: i64 = 90_040_402;
    let p3: i64 = 90_040_403;
    let ship1: i64 = 1_040_000_401;

    let ship2: i64 = 1_040_000_402;
    let ship3: i64 = 1_040_000_403;

    let db = Arc::new(WorldRepository::new_empty());
    for (i, (cid, sid)) in [(p1, ship1), (p2, ship2), (p3, ship3)].into_iter().enumerate() {
        db.provision_gm_account(&format!("obs_{i}"), "pw", cid, &format!("ObsPilot{i}"));
        db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
                ch.balance = eve_db::IskCents::from_f64_isk(500.0);
            }
            snap.items.insert(
                sid,
                InventoryItem {
                    item_id: sid,
                    type_id: subcap_tid,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
    }
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            7701,
            FleetRecord {
                fleet_id: 7701,
                boss_id: p1,
                members: vec![p1, p2, p3],
                wings: Default::default(),
                squads: Default::default(),
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
    });
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_550_000)));
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Spawn Observatory Flashpoint
    let spawned = spawn_observatory_flashpoint(&ctx, sys_id)
        .await
        .expect("spawn_observatory_flashpoint succeeds");
    assert!(!matches!(spawned, PyRep::None));

    let obs = get_observatory_site_state(&ctx, sys_id).expect("observatory persisted");
    let gate_pos = DVec3::from_array(obs.gate_pos);

    // Spawn all 3 fleet member subcapital ships right at the acceleration gate
    for sid in [ship1, ship2, ship3] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: subcap_tid,
                position: gate_pos + DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_100_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    // Validate fleet size check
    assert!(validate_observatory_access(&ctx, p1, &[p1, p2]).is_err());
    validate_observatory_access(&ctx, p1, &[p1, p2, p3]).expect("3+ fleet members accepted");

    let mut session = SessionState::new(404);
    session.user_id = Some(1);
    session.character_id = Some(p1);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship1);

    // Gate mismatch returns None; matching gate validates fleet & warps
    assert!(try_activate_observatory_gate(&ctx, &session, sys_id, 123).await.is_none());
    let gate_res = try_activate_observatory_gate(&ctx, &session, sys_id, obs.site_id + 2)
        .await
        .expect("gate recognized");
    assert!(gate_res.is_ok(), "gate activation failed: {:?}", gate_res);

    // World Ark boss kill & collective completion payout
    on_world_ark_boss_killed(&ctx, sys_id, p1).await;
    let comp = complete_observatory_flashpoint(&ctx, obs.site_id, &[p1, p2, p3])
        .expect("complete_observatory_flashpoint succeeds");
    assert!(!matches!(comp, PyRep::None));
    assert!(
        ctx.db.with_snapshot(|snap| snap.characters[&p1].balance)
            > eve_db::IskCents::from_f64_isk(500.0)
    );
}
