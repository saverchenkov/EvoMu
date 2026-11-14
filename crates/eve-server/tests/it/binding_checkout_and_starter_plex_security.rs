//! Integration tests for bound moniker resolution (`#473`, `#476`), session transition
//! evictions, FastCheckout/NES purchase invariants (`#474`), and starter PLEX grants (`#517`).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_testkit::client_harness::extract_bound_id_from_wire_rep;

use crate::common;

#[tokio::test]
async fn bound_object_monikers_preserve_service_targets_and_evict_on_session_transitions() {
    let ctx = eve_server::router::ServerContext::new();
    common::pin_clock(&ctx);
    let uid = ctx.db.provision_player_account("pilot1", "pass1");
    let char_id = ctx
        .db
        .create_character(uid, "Bound Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create char")
        .character_id;
    let corp_id = 98_000_001_i64;
    let agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.agent_type_id == 2
                && a.solar_system_id.is_some()
                && a.station_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .or_else(|| ctx.sde.agents.values().min_by_key(|a| a.agent_id))
        .expect("seeded agent in SDE");
    let jita_agent = agent.agent_id;
    let jita_sys = i64::from(agent.solar_system_id.unwrap_or(30_000_142));
    let jita_station = agent.station_id.unwrap_or(60_003_760);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys as u32;
            ch.station_id = Some(jita_station);
            ch.corporation_id = corp_id;
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
        }
        snap.skills.entry(char_id).or_default().insert(
            3363,
            eve_db::TrainedSkillRecord {
                type_id: 3363,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    let mut sess = SessionState::new(501);
    common::login_pilot(&ctx, &mut sess, "pilot1", "pass1").await;
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(jita_sys as u32);
    sess.station_id = Some(jita_station);

    // 1. Bind agentMgr to an SDE agent and call DoAction with an unrelated arg[1] (missionID)

    let agent_bind_rep = common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok(
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Integer(jita_agent), PyRep::None],
            &HashMap::new(),
        )
        .await;
    let agent_bound_id =
        extract_bound_id_from_wire_rep(&agent_bind_rep).expect("bind agentMgr");
    let solar_sys_rep = common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok(
            "agentMgr",
            Some(agent_bound_id),
            "GetSolarSystemOfAgent",
            &[PyRep::Integer(999_999_999)],
            &HashMap::new(),
        )
        .await;
    assert_eq!(solar_sys_rep, PyRep::Integer(jita_sys));

    // DoAction(actionID=1, missionID=888_888_888) must keep bound agent_id rather than treating missionID as agent_id
    let action_res = common::borrow_wire_harness(&ctx, &mut sess)
        .call(
            "agentMgr",
            Some(agent_bound_id),
            "DoAction",
            &[PyRep::Integer(1), PyRep::Integer(888_888_888)],
            &HashMap::new(),
        )
        .await;
    assert!(action_res.is_ok(), "DoAction failed on bound agentMgr: {action_res:?}");

    // 2. Bind planetOrbitalRegistryBroker to solarSystemID and verify explicit planetID takes precedence
    let jita_planet = 40_009_077_i64;
    let pi_bind_rep = common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok(
            "planetOrbitalRegistryBroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(jita_sys), PyRep::None],
            &HashMap::new(),
        )
        .await;
    let pi_bound_id =
        extract_bound_id_from_wire_rep(&pi_bind_rep).expect("bind planetOrbitalRegistryBroker");
    assert!(common::borrow_wire_harness(&ctx, &mut sess)
        .call(
            "planetOrbitalRegistryBroker",
            Some(pi_bound_id),
            "GetTaxRate",
            &[],
            &HashMap::new(),
        )
        .await
        .is_err());
    let tax_res = common::borrow_wire_harness(&ctx, &mut sess)
        .call(
            "planetOrbitalRegistryBroker",
            Some(pi_bound_id),
            "GetTaxRate",
            &[PyRep::Integer(jita_planet)],
            &HashMap::new(),
        )
        .await;
    assert!(tax_res.is_ok(), "GetTaxRate with bound solarSystemID failed: {tax_res:?}");

    // 3. Bind officeManager to corp_id when docked, and verify corp change invalidates the binding
    let office_bind_rep = common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok(
            "officeManager",
            None,
            "MachoBindObject",
            &[PyRep::Integer(corp_id), PyRep::None],
            &HashMap::new(),
        )
        .await;
    let office_bound_id =
        extract_bound_id_from_wire_rep(&office_bind_rep).expect("bind officeManager");
    assert!(ctx.verify_bound_object(&sess, office_bound_id).is_ok());

    let create_corp_res = common::borrow_wire_harness(&ctx, &mut sess)
        .call(
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String("BoundTransitionTestCorp".into()),
                PyRep::String("BTTC".into()),
                PyRep::Real(0.05),
            ],
            &HashMap::new(),
        )
        .await;
    assert!(create_corp_res.is_ok(), "AddCorporation failed: {create_corp_res:?}");
    assert!(
        ctx.verify_bound_object(&sess, office_bound_id).is_err(),
        "officeManager binding must be evicted upon corporation change"
    );
}
