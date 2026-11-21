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
    let target_cs = ctx.build_type_combat_state(587);
    let init_hp = target_cs.shield_hp + target_cs.armor_hp + target_cs.structure_hp;
    sys.spawn_ball_with_combat(Ball {
        id: target_id, type_id: 587, position: attacker_ball.position + DVec3::new(2_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }, target_cs).await.expect("spawn target ball");

    let empty = HashMap::new();
    let lock_rep = actor.harness.call_ok("dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &empty).await;
    assert!(!matches!(lock_rep, PyRep::None), "AddTarget must return lock acknowledgment");

    let mut locked = false;
    for _ in 0..90 {
        if sys.get_target_locks(ship_id).await.expect("locks").iter().any(|l| l.target_id == target_id && l.locked) {
            locked = true;
            break;
        }
        sys.step_manual(1.0).await.expect("step_manual");
    }
    assert!(locked, "combat pilot must finish locking nearby target");

    let charge_id_before = ctx.db.with_snapshot(|snap| {
        snap.items.values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8))
            .map(|i| (i.item_id, i.quantity))
            .expect("loaded charge before Activate")
    });
    assert_eq!(charge_id_before.1, 40);

    let act_rep = actor.harness.call_ok(
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(mod_id), PyRep::String("projectileFired".into()), PyRep::Integer(target_id)],
        &empty,
    ).await;
    assert_eq!(act_rep.dict_get("moduleID").and_then(|v| v.as_i64()), Some(mod_id));
    assert_eq!(act_rep.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    assert!(act_rep.dict_get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0, "expected positive cycle duration in {act_rep:?}");
    assert!(sys.has_module_cycle(ship_id, mod_id).await, "SystemActor must track active weapon cycle");
    assert!(ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod_id)), "module_active_states must mark weapon active");
    let after_cs = sys.get_combat_state(target_id).await.expect("combat state query").expect("target combat state");
    let after_hp = after_cs.shield_hp + after_cs.armor_hp + after_cs.structure_hp;
    assert!(after_hp < init_hp, "first weapon volley must damage locked target (init={init_hp}, after={after_hp})");
    let qty_after = ctx.db.with_snapshot(|snap| snap.items.get(&charge_id_before.0).map(|i| i.quantity));
    assert_eq!(qty_after, Some(39), "firing loaded weapon once must consume 1 round");
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

    let mut docked_ship = snap.clone();
    docked_ship.items.get_mut(&ship_id).expect("ship").location_id = 60_015_157;
    docked_ship.items.get_mut(&ship_id).expect("ship").flag = InventoryFlag::Hangar;
    exp.require_player_corp = false;
    let errs = check_legal_fixture(&docked_ship, &ctx.sde, &view, &exp).unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, LegalFixtureViolation::ShipLocationMismatch { .. })));

    // BF-4: Unspawned solar system ballpark must return Some(false) and emit ShipMissingFromSpaceBallpark.
    let unspawned_sys = if actor.solar_system_id == 30_000_002 { 30_000_003 } else { 30_000_002 };
    let missing_ballpark = backend.ship_in_space_ballpark(unspawned_sys, ship_id).await;
    assert_eq!(missing_ballpark, Some(false));
    let mut missing_sys_snap = snap.clone();
    missing_sys_snap.characters.get_mut(&actor.character_id).expect("char").solar_system_id = unspawned_sys;
    missing_sys_snap.items.get_mut(&ship_id).expect("ship").location_id = i64::from(unspawned_sys);
    let mut missing_sys_view = view.clone();
    missing_sys_view.solar_system_id = Some(unspawned_sys);
    let mut missing_sys_exp = actor.expectation.clone();
    missing_sys_exp.ship_in_space_ballpark = missing_ballpark;
    let errs = check_legal_fixture(&missing_sys_snap, &ctx.sde, &missing_sys_view, &missing_sys_exp).unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, LegalFixtureViolation::ShipMissingFromSpaceBallpark { ship_id: sid, solar_system_id } if *sid == ship_id && *solar_system_id == unspawned_sys)), "{errs:?}");

    // BF-2 + ban timestamp observation on live snapshot.
    let mut over_cap = snap.clone();
    over_cap.items.values_mut()
        .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8))
        .expect("loaded charge")
        .quantity = 500;
    let errs = check_legal_fixture(&over_cap, &ctx.sde, &view, &actor.expectation).unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, LegalFixtureViolation::ChargeExceedsModuleCapacity { quantity: 500, .. })), "{errs:?}");

    let now = view.now_unix.expect("ScenarioBackend must populate now_unix");
    let mut expired_ban = snap.clone();
    expired_ban.accounts.values_mut().find(|a| a.user_id == actor.user_id).expect("acc").banned_until_unix = Some(now.saturating_sub(60));
    assert_legal_fixture("expired-ban", &expired_ban, &ctx.sde, &view, &actor.expectation);
    let mut active_ban = snap;
    active_ban.accounts.values_mut().find(|a| a.user_id == actor.user_id).expect("acc").banned_until_unix = Some(now.saturating_add(60));
    let errs = check_legal_fixture(&active_ban, &ctx.sde, &view, &actor.expectation).unwrap_err();
    assert!(errs.iter().any(|e| matches!(e, LegalFixtureViolation::MissingOrBannedAccount { user_id } if *user_id == actor.user_id)), "{errs:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn omega_actor_trains_multiple_omega_skills_and_skill_gated_charges() {
    use eve_testkit::scenario::{ActorSpec, CorpSetup, ShipLoadoutSpec};

    let ctx = TestContext::new();
    crate::common::pin_clock(&ctx);
    let mut backend = ServerScenarioBackend::with_rpc_rate_budget_disabled(&ctx);

    let builder = ScenarioBuilder::with_seed(DEFAULT_TEST_SEED)
        .add_actor(ActorSpec {
            label: "omega-miner".into(),
            username: "omega_miner_1".into(),
            character_name: "Omega Miner One".into(),
            starting_isk: 200_000_000.0,
            require_omega: true,
            corp: CorpSetup::NpcStarter,
            ship: Some(ShipLoadoutSpec {
                hull_type_id: 17476, // Covetor (requires Mining Barge Omega-only)
                modules: vec![(InventoryFlag::HiSlot0, 17912)], // Modulated Strip Miner II
                charges: vec![(InventoryFlag::HiSlot0, 46355, 1)], // Ubiquitous Moon Mining Crystal Type A I (requires 46152)
                ..Default::default()
            }),
            extra_skills: vec![22544], // Hulk prerequisites (Exhumers + Mining Barge V + Science/Industry prereqs)
            in_space: false,
            ..Default::default()
        })
        .add_actor(ActorSpec {
            label: "omega-wing".into(),
            username: "omega_miner_2".into(),
            character_name: "Omega Miner Two".into(),
            starting_isk: 100_000_000.0,
            require_omega: true,
            corp: CorpSetup::NpcStarter,
            ship: Some(ShipLoadoutSpec {
                hull_type_id: 17476,
                modules: vec![(InventoryFlag::HiSlot0, 17912)],
                charges: vec![(InventoryFlag::HiSlot0, 46355, 1)],
                ..Default::default()
            }),
            in_space: false,
            ..Default::default()
        });

    let scenario = builder.build(&mut backend).await;
    let miner = scenario.actor(0);
    let wing = scenario.actor(1);
    assert!(miner.expectation.expect_omega);
    assert!(wing.expectation.expect_omega);
    ctx.db.with_snapshot(|snap| {
        let ch0 = snap.characters.get(&miner.character_id).expect("omega-miner char");
        let ch1 = snap.characters.get(&wing.character_id).expect("omega-wing char");
        assert_eq!(ch0.clone_grade, 2, "earlier Omega actor must retain clone_grade == 2 after multi-actor training");
        assert_eq!(ch1.clone_grade, 2, "second Omega actor must have clone_grade == 2");
        let s0 = snap.skills.get(&miner.character_id).expect("miner skills");
        assert!(s0.get(&17940).is_some_and(|r| r.level >= 1), "Omega-gated Mining Barge skill must train");
        assert!(s0.get(&22551).is_some_and(|r| r.level >= 1), "Omega-gated Exhumers skill must train");
        assert!(s0.get(&46152).is_some_and(|r| r.level >= 1), "charge-gated Ubiquitous Moon Ore Processing skill must train");
        let ship_id = miner.ship_id.expect("miner ship");
        assert!(
            snap.items.values().any(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 46355 && i.quantity == 1),
            "skill-gated Ubiquitous Moon Mining Crystal Type A I must be loaded in HiSlot0"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corp_join_actor_promotes_delayed_division_roles_after_stasis() {
    use eve_db::corp_roles;
    use eve_testkit::scenario::{ActorSpec, CorpSetup};

    let ctx = TestContext::new();
    crate::common::pin_clock(&ctx);
    let mut backend = ServerScenarioBackend::with_rpc_rate_budget_disabled(&ctx);

    let delayed_roles = corp_roles::ROLE_HANGAR_CAN_TAKE[0] | corp_roles::ROLE_HANGAR_CAN_QUERY[0];
    let builder = ScenarioBuilder::with_seed(DEFAULT_TEST_SEED)
        .add_actor(ActorSpec {
            label: "corp-ceo".into(),
            username: "corp_join_ceo".into(),
            character_name: "Corp Join CEO".into(),
            starting_isk: 50_000_000.0,
            require_omega: true,
            corp: CorpSetup::FoundPlayerCorp {
                name: "Scenario Corp".into(),
                ticker: "SCNP".into(),
                tax_rate: 0.05,
                required_roles: corp_roles::ROLE_DIRECTOR,
            },
            in_space: false,
            ..Default::default()
        })
        .add_actor(ActorSpec {
            label: "corp-quartermaster".into(),
            username: "corp_join_qm".into(),
            character_name: "Corp Join QM".into(),
            starting_isk: 10_000_000.0,
            corp: CorpSetup::JoinActorCorp {
                founder_actor_index: 0,
                required_roles: delayed_roles,
            },
            in_space: false,
            ..Default::default()
        });

    let scenario = builder.build(&mut backend).await;
    let ceo = scenario.actor(0);
    let qm = scenario.actor(1);
    assert_eq!(qm.corporation_id, ceo.corporation_id);
    let view = backend.actor_session_view(qm.harness.transport());
    assert_eq!(view.corp_role & delayed_roles, delayed_roles, "session corp_role must reflect matured delayed division roles");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters.get(&ceo.character_id).expect("ceo").clone_grade, 2, "Omega CEO must not lapse during 24h stasis");
        let corp = snap.corporations.get(&ceo.corporation_id).expect("player corp");
        let base_roles = corp.member_base_roles(qm.character_id);
        assert!(corp_roles::has_role(base_roles, delayed_roles), "delayed roles must be promoted into member_base_roles: {base_roles:#x}");
    });
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
