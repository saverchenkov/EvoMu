//! End-to-end acceptance tests for `#1540` Track 1 (`ScenarioBuilder` presets,
//! legal-fixture checks, deterministic SDE/RNG picks, and `TestContext`).

use std::collections::HashMap;

use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::router::TestContext;
use eve_testkit::legal_fixture::{
    assert_legal_fixture, check_legal_fixture, is_npc_corporation, LegalFixtureViolation,
};
use eve_testkit::scenario::{
    DeterministicRng, ScenarioBuilder, ScenarioWorldBackend, DEFAULT_TEST_SEED,
};
use glam::DVec3;

use crate::scenario_common::ServerScenarioBackend;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preset_combat_pilot_in_space_locks_and_activates_weapon() {
    let ctx = TestContext::new();
    crate::common::pin_clock(&ctx);
    let mut backend = ServerScenarioBackend::new(&ctx);
    let mut scenario = ScenarioBuilder::combat_pilot_in_space().build(&mut backend).await;
    let actor = scenario.actor_mut(0);
    let ship_id = actor.ship_id.expect("combat ship");
    let mod_id = actor.module_item_ids[0].1;
    assert!(actor.station_id.is_none());

    let sys = ctx.systems.get_or_spawn(actor.solar_system_id);
    let (_, attacker_ball) = sys.get_ball_snapshot(ship_id).await.expect("ball query").expect("ship in ballpark");
    let target_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord {
            item_id: id, type_id: 587, owner_id: 1_000_125, location_id: i64::from(actor.solar_system_id),
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
        id
    });
    sys.spawn_ball(Ball {
        id: target_id, type_id: 587, position: attacker_ball.position + DVec3::new(2_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.expect("spawn target ball");

    let empty = HashMap::new();
    let _lock_rep = actor.harness.call_ok("dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &empty).await;

    let mut locked = false;
    for _ in 0..90 {
        if sys.get_target_locks(ship_id).await.expect("locks").iter().any(|l| l.target_id == target_id && l.locked) {
            locked = true;
            break;
        }
        sys.step_manual(1.0).await.expect("step_manual");
    }
    assert!(locked, "combat pilot must finish locking nearby target");

    let _act_rep = actor.harness.call_ok("dogmaIM", None, "Activate", &[PyRep::Integer(mod_id), PyRep::Integer(target_id)], &empty).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preset_industrialist_docked_installs_manufacturing_job() {
    let ctx = TestContext::new();
    crate::common::pin_clock(&ctx);
    let mut backend = ServerScenarioBackend::new(&ctx);
    let mut scenario = ScenarioBuilder::industrialist_docked(&ctx.sde).build(&mut backend).await;
    let actor = scenario.actor_mut(0);
    assert!(actor.station_id.is_some());

    let bp_type_id = actor.hangar_item_ids.keys().copied()
        .find(|tid| ctx.sde.blueprints.contains_key(tid))
        .expect("blueprint in industrialist hangar");
    let job_rep = actor.harness.call_ok(
        "industryManager", None, "InstallJob",
        &[PyRep::Integer(i64::from(bp_type_id)), PyRep::Integer(1), PyRep::Integer(1)], &HashMap::new(),
    ).await;
    let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("InstallJob jobID");
    assert!(job_id > 0);
    assert!(ctx.db.with_snapshot(|snap| snap.industry_jobs.contains_key(&job_id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preset_corporation_director_performs_director_actions() {
    let ctx = TestContext::new();
    crate::common::pin_clock(&ctx);
    let mut backend = ServerScenarioBackend::from_test_ctx(&ctx);
    let mut scenario = ScenarioBuilder::corporation_director().build(&mut backend).await;
    let actor = scenario.actor_mut(0);
    assert!(!is_npc_corporation(actor.corporation_id, &ctx.sde));

    let bul_rep = actor.harness.call_ok(
        "corpRegistry", None, "AddBulletin",
        &[PyRep::String("Ops".into()), PyRep::String("Fleet ready".into())], &HashMap::new(),
    ).await;
    assert!(bul_rep.dict_get("bulletinID").and_then(|v| v.as_i64()).is_some());
    ctx.db.with_snapshot(|snap| {
        let corp = snap.corporations.get(&actor.corporation_id).expect("player corp");
        assert_ne!(corp.member_roles.get(&actor.character_id).copied().unwrap_or(0) & corp_roles::ROLE_DIRECTOR, 0);
        assert!(snap.corp_governance.get(&actor.corporation_id).is_some_and(|g| g.bulletins.iter().any(|(_, title, ..)| title == "Ops")));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preset_two_pilots_in_fleet_coordinates_broadcast() {
    let ctx = TestContext::new();
    crate::common::pin_clock(&ctx);
    let mut backend = ServerScenarioBackend::with_rpc_rate_budget_disabled(&ctx);
    let mut scenario = ScenarioBuilder::two_pilots_in_fleet().build(&mut backend).await;
    let [fc, wing] = &mut scenario.actors[..] else { panic!("expected 2 fleet actors"); };
    let fleet_id = fc.fleet_id.expect("commander fleet_id");
    assert_eq!(wing.fleet_id, Some(fleet_id));
    let wing_ship = wing.ship_id.expect("wingman ship");
    let wing_ship_type = ctx.db.with_snapshot(|s| s.items.get(&wing_ship).map(|it| it.type_id as i64).unwrap_or(587));

    let bcast = fc.harness.call_ok(
        "fleetObjectHandler", None, "SendBroadcast",
        &[PyRep::String("TargetPrimary".into()), PyRep::Integer(1), PyRep::Integer(wing_ship), PyRep::Integer(wing_ship_type)], &HashMap::new(),
    ).await;
    assert!(matches!(bcast, PyRep::KeyVal(_)));

    let init = wing.harness.call_ok("fleetObjectHandler", None, "GetInitState", &[], &HashMap::new()).await;
    assert_eq!(init.dict_get("fleetID").and_then(|v| v.as_i64()), Some(fleet_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negative_legal_fixture_checks_live_snapshot() {
    let ctx = TestContext::new();
    crate::common::pin_clock(&ctx);
    let mut backend = ServerScenarioBackend::new(&ctx);
    let mut scenario = ScenarioBuilder::combat_pilot_in_space().build(&mut backend).await;
    let actor = scenario.actor_mut(0);
    let ship_id = actor.ship_id.expect("combat ship");
    let view = backend.actor_session_view(actor.harness.transport());
    let mut exp = actor.expectation.clone();
    let snap = ctx.db.with_snapshot(|s| s.clone());
    assert_legal_fixture("baseline", &snap, &ctx.sde, &view, &exp);

    let mut missing_skill = snap.clone();
    missing_skill.skills.get_mut(&actor.character_id).expect("skills").remove(&3302);
    let errs = check_legal_fixture(&missing_skill, &ctx.sde, &view, &exp).unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, LegalFixtureViolation::MissingRequiredSkill { skill_type_id: 3302, .. })));

    let mut bad_ammo = snap.clone();
    let charge = bad_ammo.items.values_mut()
        .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8))
        .expect("loaded charge");
    charge.type_id = 234;
    let errs = check_legal_fixture(&bad_ammo, &ctx.sde, &view, &exp).unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, LegalFixtureViolation::IncompatibleLoadedCharge { charge_type_id: 234, .. })));

    let mut npc_snap = snap.clone();
    npc_snap.characters.get_mut(&actor.character_id).expect("char").corporation_id = 1_000_167;
    let mut npc_view = view.clone();
    npc_view.corporation_id = Some(1_000_167);
    exp.require_player_corp = true;
    let errs = check_legal_fixture(&npc_snap, &ctx.sde, &npc_view, &exp).unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, LegalFixtureViolation::NpcCorpUsedAsPlayerCorp { corporation_id: 1_000_167, .. })));

    let mut docked_ship = snap;
    docked_ship.items.get_mut(&ship_id).expect("ship").location_id = 60_015_157;
    docked_ship.items.get_mut(&ship_id).expect("ship").flag = InventoryFlag::Hangar;
    exp.require_player_corp = false;
    let errs = check_legal_fixture(&docked_ship, &ctx.sde, &view, &exp).unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, LegalFixtureViolation::ShipLocationMismatch { .. })));
}

#[test]
fn deterministic_seed_and_sde_pick_reproducibility() {
    let ctx = TestContext::new();
    let p1 = ctx.sde.lowest_matching_type(|t| t.published && t.category_id == 6 && t.group_id == 25).expect("frigate");
    let p2 = ctx.sde.lowest_matching_type(|t| t.published && t.category_id == 6 && t.group_id == 25).expect("frigate");
    assert_eq!(p1.type_id, p2.type_id);

    let b1 = ScenarioBuilder::industrialist_docked(&ctx.sde);
    let b2 = ScenarioBuilder::industrialist_docked(&ctx.sde);
    assert_eq!(b1.seed, DEFAULT_TEST_SEED);
    assert_eq!(b1.actors[0].station_hangar_items, b2.actors[0].station_hangar_items);

    let mut r1 = DeterministicRng::new(DEFAULT_TEST_SEED);
    let mut r2 = DeterministicRng::new(DEFAULT_TEST_SEED);
    let seq1: Vec<u64> = (0..8).map(|_| r1.next_u64()).collect();
    let seq2: Vec<u64> = (0..8).map(|_| r2.next_u64()).collect();
    assert_eq!(seq1, seq2);
}
