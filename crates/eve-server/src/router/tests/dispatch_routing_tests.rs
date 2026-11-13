//! Unit tests for RPC dispatch gates, `MachoBindObject`, Quasar gateway routing, and Sov/mining seeding.

use crate::router::dispatch::{seed_system_mining_sites_from_sde, tick_sov_prospecting_deposit_respawn};
use crate::router::ServerContext;
use crate::services::mining_sites::SOV_PROSPECTING_DEPOSITS;
use eve_db::{
    CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
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

#[tokio::test]
async fn dispatch_gates_timecontrol_macho_bind_and_quasar_domains() {
    let ctx = test_ctx();
    let mut session = SessionState::default();
    session.session_id = 777;
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    session.solar_system_id = Some(30_000_142);

    // Feature flag disabled gates
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .enable_living_world = false;
    let tale_err = ctx
        .dispatch_rpc(&mut session, "taleMgr", None, "", &[], &HashMap::new(), None)
        .await
        .unwrap_err();
    assert!(tale_err.message.contains("disabled"));
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .enable_living_world = true;

    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .enable_quasar_gateway = false;
    let err = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "FeatureDisabled");
    ctx.feature_flags
        .write()
        .expect("feature_flags lock poisoned")
        .enable_quasar_gateway = true;

    // Non-GM denied on timeControl & slash
    let err = ctx
        .dispatch_rpc(
            &mut session,
            "timeControl",
            None,
            "AdvanceClock",
            &[PyRep::Integer(10)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "InsufficientRole");

    let err = ctx
        .dispatch_rpc(&mut session, "slash", None, "SlashCmd", &[], &HashMap::new(), None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "InsufficientRole");

    // Grant GM role and test AdvanceClock (negative -> InvalidArgument, positive -> true) & FastForwardSessionTimer
    session.role = service_roles::ROLE_GMH | service_roles::ROLE_QA;
    let err = ctx
        .dispatch_rpc(
            &mut session,
            "timeControl",
            None,
            "AdvanceClock",
            &[PyRep::Integer(-5)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidArgument");

    let res = ctx
        .dispatch_rpc(
            &mut session,
            "timeControl",
            None,
            "AdvanceClock",
            &[PyRep::Integer(15)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(res, PyRep::Bool(true));

    let ff = ctx
        .dispatch_rpc(
            &mut session,
            "timeControl",
            None,
            "FastForwardSessionTimer",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(ff, PyRep::Bool(true));

    // PublicGateway unknown domain check
    let err = ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::String("nonexistent.domain".into())],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "UnknownQuasarDomain");

    // MachoResolveObject & MachoBindObject (with string moniker and invalid arg)
    let node = ctx
        .dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "MachoResolveObject",
            &[PyRep::Integer(60_003_760)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(node, PyRep::Integer(eve_machonet::MACHO_PROXY_NODE_ID as i64));

    assert!(ctx
        .dispatch_rpc(&mut session, "invbroker", None, "MachoBindObject", &[], &HashMap::new(), None)
        .await
        .is_err());

    session.corporation_id = Some(98_000_001);
    let bound = ctx
        .dispatch_rpc(
            &mut session,
            "corpRegistry",
            None,
            "MachoBindObject",
            &[PyRep::Integer(98_000_001)],
            &HashMap::new(),
            Some("machoNet:26000"),
        )
        .await
        .unwrap();
    assert!(matches!(bound, PyRep::Tuple(_)));
    assert!(ctx
        .dispatch_rpc(
            &mut session,
            "corpRegistry",
            None,
            "MachoBindObject",
            &[PyRep::Integer(98_000_002)],
            &HashMap::new(),
            Some("machoNet:26000"),
        )
        .await
        .is_err());

    // Unimplemented RPC on an internal non-client service returns UnimplementedRPC
    let err = ctx
        .dispatch_rpc(
            &mut session,
            "timeControl",
            None,
            "",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "UnimplementedRPC");
}

#[tokio::test]
async fn sov_prospecting_respawn_and_system_mining_site_seeding() {
    let ctx = test_ctx();
    let sys_id = 30_000_144; // Low-sec system
    let hub_id = 1_090_000_001;
    let upgrade_id = 1_090_000_002;
    let upgrade_type = SOV_PROSPECTING_DEPOSITS[0].upgrade_type_id;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1_000_000.0,
                armor_hp: 1_000_000.0,
                hull_hp: 1_000_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            upgrade_id,
            InventoryItemRecord {
                item_id: upgrade_id,
                type_id: upgrade_type,
                owner_id: 98_000_001,
                location_id: hub_id,
                flag: InventoryFlag::SubSystemSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "DNG-SOV-OLD".into(),
            solar_system_id: sys_id,
            name: "Old Sov Ore".into(),
            anomaly_type: "SovOre".into(),
            position: [10_000.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![1230],
            remaining_volume: 0.0,
            created_unix: now.saturating_sub(100_000),
            lifetime_seconds: 100,
            cleared: true,
        });
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: "DNG-SOV-OLD".into(),
            solar_system_id: sys_id,
            sig_group: "SovOre".into(),
            type_name: "Old Sov Ore".into(),
            position: [10_000.0, 0.0, 0.0],
            signal_strength: 0.0,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: now.saturating_sub(100_000),
            lifetime_seconds: 100,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: true,
        });
    });

    tick_sov_prospecting_deposit_respawn(&ctx, sys_id).await;
    ctx.db.with_snapshot(|snap| {
        let anoms = &snap.cosmic_anomalies[&sys_id];
        assert!(anoms.iter().any(|a| a.anomaly_id != "DNG-SOV-OLD" && !a.cleared));
        assert!(snap.asteroids.values().any(|a| a.solar_system_id == sys_id && a.belt_id == 0));
    });

    // Also test Wormhole C3 system mining site seeding & idempotency
    let wh_sys = 31_000_005;
    seed_system_mining_sites_from_sde(&ctx, wh_sys).await;
    let first_ast_count = ctx.db.with_snapshot(|s| {
        s.asteroids.values().filter(|a| a.solar_system_id == wh_sys).count()
    });
    assert!(first_ast_count > 0);
    seed_system_mining_sites_from_sde(&ctx, wh_sys).await;
    let second_ast_count = ctx.db.with_snapshot(|s| {
        s.asteroids.values().filter(|a| a.solar_system_id == wh_sys).count()
    });
    assert_eq!(first_ast_count, second_ast_count);

    // MachoBindObject forms (string objectID, tuple objectID, invalid arg, piggybacked call, machoNet:26000)
    let mut sess = eve_machonet::SessionState::default();
    sess.session_id = 88_001;
    sess.user_id = Some(101);
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(30_000_142);

    let bound_tuple_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "MachoBindObject",
            &[
                eve_proto::PyRep::Tuple(vec![eve_proto::PyRep::Integer(30_000_142)]),
                eve_proto::PyRep::Tuple(vec![
                    eve_proto::PyRep::String("UpdateStateRequest".into()),
                    eve_proto::PyRep::Tuple(vec![]),
                    eve_proto::PyRep::Dict(vec![(
                        eve_proto::PyRep::String("extra".into()),
                        eve_proto::PyRep::Integer(1),
                    )]),
                ]),
            ],
            &Default::default(),
            Some("machoNet:26000"),
        )
        .await;
    assert!(bound_tuple_res.is_ok());

    let bad_bind = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "MachoBindObject",
            &[eve_proto::PyRep::None],
            &Default::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bad_bind.code, "InvalidArgument");

    let scan_mgr_res = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetSystemScanMgr",
            &[eve_proto::PyRep::Integer(30_000_142)],
            &Default::default(),
            Some("machoNet:26000"),
        )
        .await;
    assert!(scan_mgr_res.is_ok());
}

