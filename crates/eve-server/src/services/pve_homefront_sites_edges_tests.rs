//! Edge-case tests for `pve_homefront_sites` seeding, tier/scaling, and RPC error paths.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_special_sites::with_pve_special_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DungeonTemplate, SdeRegistry, SolarSystem};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn dummy_system(sys_id: u32, sec: f64) -> SolarSystem {
    SolarSystem {
        solar_system_id: sys_id,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        name: format!("Sys-{sys_id}"),
        security_status: sec,
        radius: 1.0e11,
        galactic_pos_ly: [0.0; 3],
        planet_ids: Vec::new(),
        stations: Vec::new(),
        wormhole_class_id: None,
    }
}

#[test]
fn homefront_seeding_security_boundaries_templates_and_idempotency() {
    let mut base_sde = (*shared_sde()).clone();
    // Add boundary security systems: 0.44 (lowsec -> no homefront) and 0.45 (highsec -> 2 sites)
    base_sde.solar_systems.insert(30_900_044, dummy_system(30_900_044, 0.44));
    base_sde.solar_systems.insert(30_900_045, dummy_system(30_900_045, 0.45));
    // Ensure at least 2 archetype-70 dungeons exist in base_sde, and build a 1-template and 0-template SDE
    let mut single_tpl_sde = base_sde.clone();
    single_tpl_sde.dungeons.retain(|_, d| d.archetype_id != Some(70));
    single_tpl_sde.dungeons.insert(
        99_001,
        DungeonTemplate {
            dungeon_id: 99_001,
            name: "Single Homefront".into(),
            archetype_id: Some(70),
            faction_id: None,
            difficulty: Some(3),
        },
    );

    let mut zero_tpl_sde = single_tpl_sde.clone();
    zero_tpl_sde.dungeons.clear();

    // 0 templates -> inserts empty list
    let db0 = Arc::new(WorldRepository::new_empty());
    let ctx0 = ServerContext::with_sde_and_repository(Arc::new(zero_tpl_sde), db0);
    ctx0.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    with_pve_special_reg(&ctx0, |reg| {
        ensure_homefront_sites_for_system_locked(&ctx0, reg, 30_900_045);
        assert!(reg.homefront_sites[&30_900_045].is_empty());
    });

    // 1 template -> both sites share dungeon_id 99_001, distinct instance_ids, diff 3 ISK scaling
    let db1 = Arc::new(WorldRepository::new_empty());
    let ctx1 = ServerContext::with_sde_and_repository(Arc::new(single_tpl_sde), db1);
    ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    with_pve_special_reg(&ctx1, |reg| {
        ensure_homefront_sites_for_system_locked(&ctx1, reg, 30_900_045);
        let list = &reg.homefront_sites[&30_900_045];
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].dungeon_id, 99_001);
        assert_eq!(list[1].dungeon_id, 99_001);
        assert_eq!(list[0].instance_id, 30_900_045_i64 * 1000 + 1);
        assert_eq!(list[1].instance_id, 30_900_045_i64 * 1000 + 2);
        assert_eq!(list[0].base_pilot_isk, 15_000_000);
        assert_eq!(list[1].base_pilot_isk, 15_000_000);

        // Calling again is idempotent (early return on contains_key)
        reg.homefront_sites.get_mut(&30_900_045).unwrap()[0].completed = true;
        ensure_homefront_sites_for_system_locked(&ctx1, reg, 30_900_045);
        assert!(reg.homefront_sites[&30_900_045][0].completed);

        // Lowsec boundary 0.44 and unknown system ID -> empty
        ensure_homefront_sites_for_system_locked(&ctx1, reg, 30_900_044);
        assert!(reg.homefront_sites[&30_900_044].is_empty());
        ensure_homefront_sites_for_system_locked(&ctx1, reg, 30_999_999);
        assert!(reg.homefront_sites[&30_999_999].is_empty());

        // init_homefront_sites seeds all HOMEFRONT_SYSTEMS
        init_homefront_sites(&ctx1, reg);
        for &sys in HOMEFRONT_SYSTEMS {
            assert!(reg.homefront_sites.contains_key(&sys));
        }
    });

    with_pve_special_reg(&ctx1, |reg| {
        reg.homefront_sites.get_mut(&30_900_045).unwrap()[0].completed = false;
    });

    // RPC with 1 template groups both instance IDs under the same dungeon_id and sorts them;
    // marking one completed omits it from GetHomefrontSiteInstances.
    let mut sess = SessionState::new(701);
    sess.user_id = Some(10);
    sess.character_id = Some(90_060_001);
    sess.solar_system_id = Some(30_900_045);

    let rep = handle_get_homefront_site_instances(&ctx1, &sess, &[PyRep::None]).unwrap();
    let PyRep::Dict(outer) = rep else { panic!("expected dict") };
    assert_eq!(outer[0].0, PyRep::Integer(30_900_045));
    let PyRep::List(inner) = &outer[0].1 else { panic!("expected inner list") };
    assert_eq!(inner.len(), 2);
    assert_eq!(
        inner[0].dict_get("instanceID"),
        Some(&PyRep::Integer(30_900_045_i64 * 1000 + 1))
    );
    assert_eq!(
        inner[1].dict_get("instanceID"),
        Some(&PyRep::Integer(30_900_045_i64 * 1000 + 2))
    );

    with_pve_special_reg(&ctx1, |reg| {
        reg.homefront_sites.get_mut(&30_900_045).unwrap()[0].completed = true;
    });
    let rep2 = handle_get_homefront_site_instances(&ctx1, &sess, &[]).unwrap();
    let PyRep::Dict(outer2) = rep2 else { panic!("expected dict") };
    let PyRep::List(inner2) = &outer2[0].1 else { panic!("expected inner list") };
    assert_eq!(inner2.len(), 1);
    assert_eq!(
        inner2[0].dict_get("instanceID"),
        Some(&PyRep::Integer(30_900_045_i64 * 1000 + 2))
    );
}

#[test]
fn homefront_rpc_auth_db_fallback_and_negative_system_errors() {
    let sde = shared_sde();
    let template_ch = WorldRepository::with_demo_seed()
        .with_snapshot(|s| s.characters[&90_000_001].clone());
    let db = Arc::new(WorldRepository::new_empty());
    db.with_mut_snapshot(|snap| {
        let mut ch = template_ch;
        ch.character_id = 90_070_001;
        ch.user_id = 70;
        ch.name = "HF Pilot".into();
        ch.corporation_id = 1_000_009;
        ch.solar_system_id = 30_000_142;
        ch.standings.clear();
        snap.characters.insert(90_070_001, ch);
    });
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

    // Unauthenticated session fails require_character
    let anon = SessionState::new(800);
    assert!(handle_get_homefront_site_instances(&ctx, &anon, &[]).is_err());

    // Negative system ID fails u32::try_from
    let mut sess = SessionState::new(801);
    sess.user_id = Some(70);
    sess.character_id = Some(90_070_001);
    assert_eq!(
        handle_get_homefront_site_instances(&ctx, &sess, &[PyRep::Integer(-5)])
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );

    // Session with solar_system_id = None falls back to character's DB solar_system_id
    crate::services::pve_special_sites::ensure_special_sites_initialized(&ctx);
    let rep = handle_get_homefront_site_instances(&ctx, &sess, &[]).unwrap();
    let PyRep::Dict(outer) = rep else { panic!("expected dict") };
    assert_eq!(outer[0].0, PyRep::Integer(30_000_142));
}

mod multi_role_edge_tests {
    use super::*;
    use crate::services::pve_homefront_sites::{
        activate_homefront_acceleration_gate, dread_assault_advance_siege,
        dread_assault_transfer_capacitor, emergency_aid_advance_defense,
        get_homefront_runtime_state, metaliminal_mine_mutanite, on_homefront_cap_warfare_event,
        on_homefront_combat_damage_event, on_homefront_remote_repair_event,
        on_homefront_warp_landed, raid_deliver_cargo_to_freighter,
        raid_tackle_and_destroy_hauler, record_homefront_combat_and_logistics,
        salvage_research_process_wreck, spawn_homefront_site_for_dungeon,
        stabilize_rift_neutralize_arrays, suspicious_signal_attack_relay,
        suspicious_signal_hack_array, traffic_stop_scan_and_intercept,
    };
    use crate::services::pve_special_sites_ops::resolve_homefront_archetype_blueprint;
    use eve_db::clock::VirtualServerClock;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
    use eve_destiny::{Ball, MovementMode};
    use eve_sde::{SolarSystemId, TypeId};
    use glam::DVec3;
    use std::sync::Arc;

    fn test_ctx_with_pilot(
        char_id: i64,
        ship_id: i64,
        ship_type_id: TypeId,
        sys_id: SolarSystemId,
    ) -> (ServerContext, SessionState) {
        let sde = super::shared_sde();
        let template_ch = WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters[&90_000_001].clone());
        let db = Arc::new(WorldRepository::new_empty());
        db.with_mut_snapshot(|snap| {
            let mut ch = template_ch;
            ch.character_id = char_id;
            ch.user_id = 80;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000);
            snap.characters.insert(char_id, ch);
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: char_id,
                    location_id: char_id,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
        let mut sess = SessionState::new(80);
        sess.user_id = Some(80);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        (ctx, sess)
    }

    fn fit_module_in_ctx(
        ctx: &ServerContext,
        char_id: i64,
        ship_id: i64,
        mod_id: i64,
        mod_type_id: TypeId,
        flag: InventoryFlag,
        online: bool,
    ) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: mod_type_id,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mod_id, online);
        });
    }

    #[tokio::test]
    async fn homefront_runtime_rejects_invalid_gates_roles_and_timeouts() {
        let jita: SolarSystemId = 30_000_142;

        // 1. Spawn validation: low-sec (Tama) & unknown/non-Homefront dungeon rejected
        let (ctx, mut sess_caracal) = test_ctx_with_pilot(930_001, 830_001, 621, jita);
        assert_eq!(
            spawn_homefront_site_for_dungeon(&ctx, 30_002_813, 10_347).unwrap_err().code,
            "InvalidHomefrontSecurity"
        );
        assert_eq!(
            spawn_homefront_site_for_dungeon(&ctx, jita, 999_999).unwrap_err().code,
            "UnknownDungeon"
        );

        let raid = spawn_homefront_site_for_dungeon(&ctx, jita, 10_347).unwrap();

        // Outer gate vs pocket separation + gate proximity enforcement (> 2500m rejected)
        assert!(!raid.pocket_spawned, "pocket must NOT spawn at site creation");
        assert!(raid.spawned_ball_ids.is_empty(), "no pocket balls before gate entry");
        let sys_h = ctx.systems.get_or_spawn(jita);
        assert!(sys_h.get_ball_snapshot(raid.outer_gate_ball_id).await.unwrap().is_some());
        assert!(on_homefront_warp_landed(&ctx, 930_001, jita, raid.site_position).await);
        assert!(!get_homefront_runtime_state(&ctx, raid.instance_id).unwrap().pocket_spawned);

        let _ = sys_h
            .spawn_ball(Ball {
                id: 830_001,
                type_id: 621,
                position: raid.site_position + DVec3::new(10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        assert_eq!(
            activate_homefront_acceleration_gate(&ctx, &sess_caracal, raid.instance_id)
                .await
                .unwrap_err()
                .code,
            "OutsideGateActivationRange"
        );
        let _ = sys_h
            .set_ball_position(830_001, raid.site_position + DVec3::new(400.0, 0.0, 0.0))
            .await;

        // 2. Gate rejections: docked, wrong system, restricted ships (T2 Cerberus 11993, BC Drake 24698, Trig Vedmak 47270, Capsule 670, Praxis 47466 / Rattlesnake)
        sess_caracal.station_id = Some(60_003_760);
        assert_eq!(
            activate_homefront_acceleration_gate(&ctx, &sess_caracal, raid.instance_id).await.unwrap_err().code,
            "ShipDocked"
        );
        sess_caracal.station_id = None;
        assert_eq!(
            record_homefront_combat_and_logistics(&ctx, &sess_caracal, raid.instance_id, 1_500.0, 0.0).unwrap_err().code,
            "GateNotEntered"
        );

        for bad_tid in [11_993_u32, 24_698, 47_270, 670, 11_134] {
            let (bad_ctx, bad_sess) = test_ctx_with_pilot(930_002, 830_002, bad_tid, jita);
            let bad_raid = spawn_homefront_site_for_dungeon(&bad_ctx, jita, 10_347).unwrap();
            let bad_h = bad_ctx.systems.get_or_spawn(jita);
            let _ = bad_h
                .spawn_ball(Ball {
                    id: 830_002,
                    type_id: bad_tid,
                    position: bad_raid.site_position + DVec3::new(400.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                })
                .await;
            assert_eq!(
                activate_homefront_acceleration_gate(&bad_ctx, &bad_sess, bad_raid.instance_id)
                    .await
                    .unwrap_err()
                    .code,
                "ShipRestrictedByGate",
                "ship type {bad_tid} must be rejected by 5-pilot gate"
            );
        }

        // 3-pilot site rejects Coercer Navy Issue (73789) and Caracal (621)
        let sal = spawn_homefront_site_for_dungeon(&ctx, jita, 10_713).unwrap();
        let _ = sys_h
            .set_ball_position(830_001, sal.site_position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        assert_eq!(
            activate_homefront_acceleration_gate(&ctx, &sess_caracal, sal.instance_id).await.unwrap_err().code,
            "ShipRestrictedByGate"
        );

        // 3. Raid edge failures: untackled hauler escapes; Caracal (<3500 m3) blocked from cargo delivery; incomplete objectives blocked
        let _ = sys_h
            .set_ball_position(830_001, raid.site_position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, raid.instance_id).await.unwrap();
        let ship_post_gate = sys_h.get_ball_snapshot(830_001).await.unwrap().unwrap().1.position;
        assert!(
            ship_post_gate.distance(raid.pocket_position) < 5_000.0,
            "gate activation must relocate pilot into Homefront pocket"
        );
        assert_eq!(
            raid_tackle_and_destroy_hauler(&ctx, &sess_caracal, raid.instance_id, false, 3_000.0)
                .unwrap_err()
                .code,
            "TargetEscaped"
        );
        raid_tackle_and_destroy_hauler(&ctx, &sess_caracal, raid.instance_id, true, 3_000.0).unwrap();
        assert_eq!(
            raid_deliver_cargo_to_freighter(&ctx, &sess_caracal, raid.instance_id).unwrap_err().code,
            "CargoCapacityTooSmall"
        );
        assert_eq!(
            complete_homefront_operation_and_pay(&ctx, &sess_caracal, raid.instance_id)
                .await
                .unwrap_err()
                .code,
            "ObjectivesIncomplete"
        );

        // 4. Metaliminal requires ONLINE Miner I / mining drone (offline module rejected)
        let met = spawn_homefront_site_for_dungeon(&ctx, jita, 10_312).unwrap();
        let _ = sys_h
            .set_ball_position(830_001, met.site_position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, met.instance_id).await.unwrap();
        assert_eq!(
            metaliminal_mine_mutanite(&ctx, &sess_caracal, met.instance_id, 100, 0).unwrap_err().code,
            "MiningModuleRequired"
        );
        fit_module_in_ctx(&ctx, 930_001, 830_001, 830_001_01, 483, InventoryFlag::HiSlot0, false);
        assert_eq!(
            metaliminal_mine_mutanite(&ctx, &sess_caracal, met.instance_id, 100, 0).unwrap_err().code,
            "MiningModuleRequired"
        );

        // 4b. Salvage Research rejects Salvager I with <20% bonus, missing/offline Salvager, or missing/offline Data Analyzer
        let (ctx_sal, sess_sal) = test_ctx_with_pilot(930_010, 830_010, 16_236, jita);
        let sal2 = spawn_homefront_site_for_dungeon(&ctx_sal, jita, 10_713).unwrap();
        let _ = ctx_sal
            .systems
            .get_or_spawn(jita)
            .spawn_ball(Ball {
                id: 830_010,
                type_id: 16_236,
                position: sal2.site_position + DVec3::new(400.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        activate_homefront_acceleration_gate(&ctx_sal, &sess_sal, sal2.instance_id).await.unwrap();
        assert_eq!(
            salvage_research_process_wreck(&ctx_sal, &sess_sal, sal2.instance_id, 1, false).unwrap_err().code,
            "SalvagerModuleRequired"
        );
        fit_module_in_ctx(&ctx_sal, 930_010, 830_010, 830_010_01, 25_861, InventoryFlag::HiSlot0, true);
        assert_eq!(
            salvage_research_process_wreck(&ctx_sal, &sess_sal, sal2.instance_id, 1, false).unwrap_err().code,
            "InsufficientSalvageBonus"
        );
        ctx_sal.db.with_mut_snapshot(|snap| {
            snap.skills.entry(930_010).or_default().insert(
                25_863,
                eve_db::TrainedSkillRecord { type_id: 25_863, level: 4, skill_points: 45_255.0 },
            );
        });
        fit_module_in_ctx(&ctx_sal, 930_010, 830_010, 830_010_02, 22_175, InventoryFlag::MedSlot0, false);
        assert_eq!(
            salvage_research_process_wreck(&ctx_sal, &sess_sal, sal2.instance_id, 1, true).unwrap_err().code,
            "AnalyzerModuleOffline"
        );

        // 5. Dread Assault rejects early Siege & cap transfer during Siege
        let dread = spawn_homefront_site_for_dungeon(&ctx, jita, 10_320).unwrap();
        let _ = sys_h
            .set_ball_position(830_001, dread.site_position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, dread.instance_id).await.unwrap();
        assert_eq!(
            dread_assault_advance_siege(&ctx, &sess_caracal, dread.instance_id, 30).unwrap_err().code,
            "DreadNotInSiegeMode"
        );
        dread_assault_transfer_capacitor(&ctx, &sess_caracal, dread.instance_id, 30_000.0, 0.0).unwrap();
        assert_eq!(
            dread_assault_transfer_capacitor(&ctx, &sess_caracal, dread.instance_id, 5_000.0, 0.0).unwrap_err().code,
            "DreadInSiegeMode"
        );

        // 6. Suspicious Signal rejects attacking relay before all 3 arrays are hacked (requires fitted online Data Analyzer)
        fit_module_in_ctx(&ctx, 930_001, 830_001, 830_001_02, 22_175, InventoryFlag::MedSlot0, true);
        let sig = spawn_homefront_site_for_dungeon(&ctx, jita, 10_309).unwrap();
        let _ = sys_h
            .set_ball_position(830_001, sig.site_position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, sig.instance_id).await.unwrap();
        suspicious_signal_hack_array(&ctx, &sess_caracal, sig.instance_id, 0).unwrap();
        assert_eq!(
            suspicious_signal_attack_relay(&ctx, &sess_caracal, sig.instance_id, 50_000.0, 10).unwrap_err().code,
            "StructureInvulnerable"
        );

        // 7. Failure persistence across Err(OperationFailed): Emergency Aid protectee death & Traffic Stop collateral
        let aid = spawn_homefront_site_for_dungeon(&ctx, jita, 10_238).unwrap();
        let _ = sys_h
            .set_ball_position(830_001, aid.site_position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, aid.instance_id).await.unwrap();
        assert_eq!(
            emergency_aid_advance_defense(&ctx, &sess_caracal, aid.instance_id, 0.0, 500_000.0, 10).unwrap_err().code,
            "OperationFailed"
        );
        assert!(
            get_homefront_runtime_state(&ctx, aid.instance_id).unwrap().failed,
            "Emergency Aid failure MUST persist in DB after Err(OperationFailed)"
        );
        assert_eq!(
            emergency_aid_advance_defense(&ctx, &sess_caracal, aid.instance_id, 1_000.0, 0.0, 10).unwrap_err().code,
            "OperationFailed"
        );

        let (ctx3, sess_frigate) = test_ctx_with_pilot(930_003, 830_003, 597, jita);
        let traf = spawn_homefront_site_for_dungeon(&ctx3, jita, 10_705).unwrap();
        let _ = ctx3
            .systems
            .get_or_spawn(jita)
            .spawn_ball(Ball {
                id: 830_003,
                type_id: 597,
                position: traf.site_position + DVec3::new(400.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        activate_homefront_acceleration_gate(&ctx3, &sess_frigate, traf.instance_id).await.unwrap();
        assert_eq!(
            traffic_stop_scan_and_intercept(&ctx3, &sess_frigate, traf.instance_id, 1, 3, 2_000.0).unwrap_err().code,
            "OperationFailed"
        );
        assert!(
            get_homefront_runtime_state(&ctx3, traf.instance_id).unwrap().failed,
            "Traffic Stop collateral failure MUST persist in DB after Err(OperationFailed)"
        );

        // NaN / Inf / out-of-range floats rejected across runtime & live hooks
        assert_eq!(
            record_homefront_combat_and_logistics(&ctx, &sess_caracal, sig.instance_id, f64::NAN, 0.0).unwrap_err().code,
            "InvalidArgument"
        );
        assert_eq!(
            stabilize_rift_neutralize_arrays(&ctx3, &sess_frigate, traf.instance_id, [f64::INFINITY, 0.0, 0.0, 0.0, 0.0], 10).unwrap_err().code,
            "InvalidArgument"
        );
        // Live event handlers silently sanitize NaN/negative inputs without mutating contributions
        on_homefront_combat_damage_event(&ctx, Some(930_001), 830_001, 999_999, f64::NAN).await;
        on_homefront_cap_warfare_event(&ctx, 930_001, jita, 999_999, f64::INFINITY, true).await;
        on_homefront_remote_repair_event(&ctx, 930_001, jita, 999_999, f64::NEG_INFINITY).await;

        let timed = spawn_homefront_site_for_dungeon(&ctx, jita, 10_347).unwrap();
        let _ = sys_h
            .set_ball_position(830_001, timed.site_position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, timed.instance_id).await.unwrap();
        ctx.advance_secs(timed.blueprint.time_limit_secs + 5);
        assert_eq!(
            record_homefront_combat_and_logistics(&ctx, &sess_caracal, timed.instance_id, 1_000.0, 0.0)
                .unwrap_err()
                .code,
            "OperationTimedOut"
        );

        // 8. Exordium halving verified against SDE starter school system (if present)
        if let Some(ex_sys) = ctx
            .sde
            .schools
            .values()
            .map(|s| s.solar_system_id)
            .find(|&sid| sid != jita)
        {
            let ex_bp = resolve_homefront_archetype_blueprint(&ctx.sde, 10_347, "Raid", ex_sys, 2);
            if ex_bp.is_exordium_system {
                assert_eq!(ex_bp.base_pilot_isk, 7_500_000);
            }
        }
    }

    #[tokio::test]
    async fn homefront_gate_fail_closed_inventory_cycle_rollover_and_unentered_guards() {
        use crate::services::inventory_fitting::fitting_mining_lasers::handle_mining_and_deactivate;
        use crate::services::inventory_fitting::fitting_module_cycles::{
            execute_pending_cycle_actions, handle_module_cycles,
        };
        use crate::services::pve_homefront_sites::{
            on_homefront_cap_warfare_event_sync, on_homefront_cargo_scanned,
            on_homefront_combat_damage_event_sync, on_homefront_entity_destroyed_core,
            on_homefront_inventory_event, on_homefront_item_moved, on_homefront_ore_mined,
            on_homefront_remote_repair_event_sync, on_homefront_structure_hacked,
            on_homefront_wreck_salvaged, tick_homefront_operations_sync,
            validate_homefront_salvage_attempt,
        };
        use eve_db::{HomefrontProgressRecord, TrainedSkillRecord};
        use eve_destiny::{CombatState, TargetLock};
        use eve_sde::attr_ids;
        use std::collections::HashMap;

        let sys_id = 30_000_142_u32;

        // 1. Fail-closed gate checks: NotInSpace, ShipNotInSpace, GateNotFound
        let (ctx, mut gate_sess) = test_ctx_with_pilot(970_001, 870_001, 620, sys_id);
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let raid_gate = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_347).expect("spawn Raid gate check");
        let saved_sys = gate_sess.solar_system_id;
        gate_sess.solar_system_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&970_001) {
                ch.solar_system_id = 0;
            }
        });
        let err_not_in_space = activate_homefront_acceleration_gate(&ctx, &gate_sess, raid_gate.instance_id)
            .await
            .expect_err("solar_system_id=None must fail closed with NotInSpace");
        assert_eq!(err_not_in_space.code, "NotInSpace");

        gate_sess.solar_system_id = saved_sys;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&970_001) {
                ch.solar_system_id = sys_id;
            }
        });
        let err_ship_missing = activate_homefront_acceleration_gate(&ctx, &gate_sess, raid_gate.instance_id)
            .await
            .expect_err("missing ship ball must fail closed with ShipNotInSpace");
        assert_eq!(err_ship_missing.code, "ShipNotInSpace");

        sys_h
            .spawn_ball(Ball {
                id: 870_001,
                type_id: 620,
                position: raid_gate.site_position + DVec3::new(300.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 0.5,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
        let _ = sys_h.remove_ball(raid_gate.outer_gate_ball_id).await.unwrap();
        let err_gate_missing = activate_homefront_acceleration_gate(&ctx, &gate_sess, raid_gate.instance_id)
            .await
            .expect_err("missing outer gate ball must fail closed with GateNotFound");
        assert_eq!(err_gate_missing.code, "GateNotFound");

        // Helper to seed an extra pilot with full skills & ship ball in Jita
        let spawn_pilot = |cid: i64, sid: i64, ship_tid: TypeId, user_id: u64, pos: DVec3| {
            let mut s = gate_sess.clone();
            s.user_id = Some(user_id);
            s.character_id = Some(cid);
            s.ship_id = Some(sid);
            s.solar_system_id = Some(sys_id);
            let template_ch = ctx.db.with_snapshot(|snap| snap.characters[&970_001].clone());
            ctx.db.with_mut_snapshot(|snap| {
                let mut ch = template_ch;
                ch.character_id = cid;
                ch.user_id = user_id;
                ch.solar_system_id = sys_id;
                ch.active_ship_id = sid;
                snap.characters.insert(cid, ch);
                snap.items.insert(
                    sid,
                    InventoryItemRecord {
                        item_id: sid,
                        type_id: ship_tid,
                        owner_id: cid,
                        location_id: sys_id as i64,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            });
            let sh_ref = sys_h.clone();
            let cs = ctx.build_ship_combat_state(cid, sid, ship_tid);
            async move {
                sh_ref
                    .spawn_ball_with_combat(
                        Ball {
                            id: sid,
                            type_id: ship_tid,
                            position: pos,
                            velocity: DVec3::ZERO,
                            max_velocity: 200.0,
                            inertia: 0.5,
                            mass: 10_000_000.0,
                            radius: 100.0,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        },
                        cs,
                    )
                    .await
                    .unwrap();
                s
            }
        };

        // 2. Inventory & Raid withdrawal rollback + outsider/wrong-type/wrong-container guards
        let raid = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_347).expect("spawn Raid #2");
        let hauler_sess = spawn_pilot(
            970_002,
            870_002,
            649,
            81,
            raid.site_position + DVec3::new(300.0, 0.0, 0.0),
        )
        .await;
        let _outsider_sess = spawn_pilot(
            970_099,
            870_099,
            620,
            89,
            raid.site_position + DVec3::new(300.0, 0.0, 0.0),
        )
        .await;
        activate_homefront_acceleration_gate(&ctx, &hauler_sess, raid.instance_id)
            .await
            .expect("Tayra enters Raid");
        let (depot_id, loot_tid) = ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&raid.instance_id];
            match &rec.progress {
                HomefrontProgressRecord::Raid { delivery_depot_ball_id, .. } => {
                    (
                        *delivery_depot_ball_id,
                        rec.blueprint.loot_commodity_type_id.unwrap_or(77_473),
                    )
                }
                other => panic!("expected Raid, got {other:?}"),
            }
        });
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                9_700_001,
                InventoryItemRecord {
                    item_id: 9_700_001,
                    type_id: loot_tid,
                    owner_id: 970_002,
                    location_id: depot_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        on_homefront_inventory_event(&ctx, 970_002, Some(sys_id), depot_id, loot_tid).await;
        assert!(matches!(
            get_homefront_runtime_state(&ctx, raid.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::Raid { cargo_delivered: 1, .. }
        ));

        // Wrong item type + wrong container + outsider depot deposit must NOT increment cargo_delivered
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                9_700_002,
                InventoryItemRecord {
                    item_id: 9_700_002,
                    type_id: 34,
                    owner_id: 970_002,
                    location_id: depot_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 10,
                    ..Default::default()
                },
            );
            snap.items.insert(
                9_700_003,
                InventoryItemRecord {
                    item_id: 9_700_003,
                    type_id: loot_tid,
                    owner_id: 970_002,
                    location_id: depot_id + 9_999,
                    flag: InventoryFlag::Cargo,
                    quantity: 5,
                    ..Default::default()
                },
            );
            snap.items.insert(
                9_700_004,
                InventoryItemRecord {
                    item_id: 9_700_004,
                    type_id: loot_tid,
                    owner_id: 970_099,
                    location_id: depot_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        on_homefront_inventory_event(&ctx, 970_002, Some(sys_id), depot_id, 34).await;
        on_homefront_item_moved(&ctx, 970_002, depot_id, 34, 10).await;
        on_homefront_inventory_event(&ctx, 970_002, Some(sys_id), depot_id + 9_999, loot_tid).await;
        on_homefront_inventory_event(&ctx, 970_099, Some(sys_id), depot_id, loot_tid).await;
        tick_homefront_operations_sync(&ctx);
        assert!(matches!(
            get_homefront_runtime_state(&ctx, raid.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::Raid { cargo_delivered: 1, .. }
        ));

        // Removing entered pilot's loot item from depot must roll cargo_delivered back to 0 (not clamp >= 1)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&9_700_001);
        });
        on_homefront_inventory_event(&ctx, 970_002, Some(sys_id), depot_id, loot_tid).await;
        assert!(matches!(
            get_homefront_runtime_state(&ctx, raid.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::Raid { cargo_delivered: 0, .. }
        ));

        // 3. Unentered-pilot guards on ALL live hooks + Live cycle rollover (cycles.rs:1900-1938)
        let mut anchor_sess = spawn_pilot(
            970_003,
            870_003,
            16_236, // Coercer T1 Destroyer (allowed on both 3-pilot and 5-pilot sites)
            83,
            DVec3::ZERO,
        )
        .await;
        let mod_rep_id = 9_800_001_i64;
        let mod_cap_id = 9_800_002_i64;
        let mod_neut_id = 9_800_003_i64;
        fit_module_in_ctx(&ctx, 970_003, 870_003, mod_rep_id, 3586, InventoryFlag::HiSlot0, true);
        fit_module_in_ctx(&ctx, 970_003, 870_003, mod_cap_id, 529, InventoryFlag::HiSlot1, true);
        fit_module_in_ctx(&ctx, 970_003, 870_003, mod_neut_id, 533, InventoryFlag::HiSlot2, true);
        // Grant required skills for Coercer (16236) & remote/neut modules + core cap/fitting skills V
        ctx.db.with_mut_snapshot(|snap| {
            let entry = snap.skills.entry(970_003).or_default();
            for tid in [16_236_u32, 3586, 529, 533] {
                if let Some(t) = ctx.sde.types.get(&tid) {
                    for (s_a, l_a) in [
                        (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
                        (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
                        (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
                    ] {
                        if let Some(&sid_f) = t.default_attributes.get(&s_a) {
                            let sid = sid_f as u32;
                            if sid > 0 {
                                let lvl = t.default_attributes.get(&l_a).copied().unwrap_or(1.0) as u8;
                                entry.insert(
                                    sid,
                                    TrainedSkillRecord {
                                        type_id: sid,
                                        level: lvl.max(5),
                                        skill_points: 256_000.0,
                                    },
                                );
                            }
                        }
                    }
                }
            }
            for sid in [3413_u32, 3417, 3418, 3422, 3423, 3424, 3426, 3427, 3428, 3429, 3431, 3432, 3433] {
                entry.insert(
                    sid,
                    TrainedSkillRecord {
                        type_id: sid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        });
        ctx.invalidate_ship_fitting(870_003);

        let aid = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_238).expect("spawn EmergencyAid");
        let met = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_312).expect("spawn Metaliminal");
        let dread = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_320).expect("spawn DreadAssault");
        let sig = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_309).expect("spawn SuspiciousSignal");
        let sal = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_713).expect("spawn SalvageResearch");
        let rift = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_714).expect("spawn StabilizeRift");
        let traf = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_705).expect("spawn TrafficStop");

        for site in [&aid, &met, &dread, &sig, &sal, &rift, &traf] {
            sys_h
                .set_ball_position(870_003, site.site_position + DVec3::new(300.0, 0.0, 0.0))
                .await;
            activate_homefront_acceleration_gate(&ctx, &anchor_sess, site.instance_id)
                .await
                .unwrap_or_else(|e| panic!("anchor enter {:?}: {e:?}", site.blueprint.archetype));
        }

        let (aid_protectee, met_rock, dread_id, sig_relay, sig_arr0, sal_wreck0, rift_arr0, traf_smuggler0) =
            ctx.db.with_snapshot(|snap| {
                let hr = &snap.dungeon_runtime.homefront_runtimes;
                let aid_p = match &hr[&aid.instance_id].progress {
                    HomefrontProgressRecord::EmergencyAid { protectee_ball_id, .. } => *protectee_ball_id,
                    _ => unreachable!(),
                };
                let met_r = match &hr[&met.instance_id].progress {
                    HomefrontProgressRecord::MetaliminalMeteoroid { meteoroid_ball_id, .. } => *meteoroid_ball_id,
                    _ => unreachable!(),
                };
                let dread_d = match &hr[&dread.instance_id].progress {
                    HomefrontProgressRecord::DreadAssault { dread_ball_id, .. } => *dread_ball_id,
                    _ => unreachable!(),
                };
                let (sig_rel, sig_a0) = match &hr[&sig.instance_id].progress {
                    HomefrontProgressRecord::SuspiciousSignal { relay_ball_id, array_ball_ids, .. } => {
                        (*relay_ball_id, array_ball_ids[0])
                    }
                    _ => unreachable!(),
                };
                let sal_w0 = match &hr[&sal.instance_id].progress {
                    HomefrontProgressRecord::SalvageResearch { wreck_ball_ids, .. } => wreck_ball_ids[0],
                    _ => unreachable!(),
                };
                let rift_a0 = match &hr[&rift.instance_id].progress {
                    HomefrontProgressRecord::StabilizeRift { array_ball_ids, .. } => array_ball_ids[0],
                    _ => unreachable!(),
                };
                let traf_s0 = match &hr[&traf.instance_id].progress {
                    HomefrontProgressRecord::TrafficStop { smuggler_ball_ids, .. } => smuggler_ball_ids[0],
                    _ => unreachable!(),
                };
                (aid_p, met_r, dread_d, sig_rel, sig_a0, sal_w0, rift_a0, traf_s0)
            });

        // Unentered pilot (970_099) must be rejected by EVERY Homefront live hook
        on_homefront_combat_damage_event_sync(&ctx, Some(970_099), 870_099, sig_relay, 5_000.0);
        on_homefront_ore_mined(&ctx, 970_099, 870_099, sys_id, met_rock, 77_524, 500, 0, 4_500).await;
        assert!(!on_homefront_structure_hacked(&ctx, 970_099, sys_id, sig_arr0).await);
        assert_eq!(
            validate_homefront_salvage_attempt(&ctx, 970_099, sys_id, sal_wreck0, 30.0, 0.0, 0.0)
                .unwrap_err()
                .code,
            "GateNotEntered"
        );
        assert!(!on_homefront_wreck_salvaged(&ctx, 970_099, sys_id, sal_wreck0).await);
        on_homefront_cargo_scanned(&ctx, 970_099, sys_id, traf_smuggler0, &[(81_383, 1)]).await;
        on_homefront_item_moved(&ctx, 970_099, depot_id, loot_tid, 1).await;
        on_homefront_cap_warfare_event_sync(&ctx, 970_099, Some(sys_id), dread_id, 1_000.0, true);
        on_homefront_cap_warfare_event_sync(&ctx, 970_099, Some(sys_id), rift_arr0, 1_000.0, false);
        on_homefront_remote_repair_event_sync(&ctx, 970_099, Some(sys_id), aid_protectee, 1_000.0);
        assert!(
            on_homefront_entity_destroyed_core(
                &ctx.systems,
                &ctx.db,
                &ctx.sde,
                sys_id,
                aid_protectee,
                Some(970_099),
            )
            .await
            .is_none(),
            "unentered killer must not fail EmergencyAid"
        );
        ctx.db.with_snapshot(|snap| {
            let hr = &snap.dungeon_runtime.homefront_runtimes;
            for sid in [aid.instance_id, met.instance_id, dread.instance_id, sig.instance_id, sal.instance_id, rift.instance_id, traf.instance_id, raid.instance_id] {
                assert!(
                    !hr[&sid].contributions.contains_key(&970_099),
                    "outsider must have 0 contributions on instance {sid}"
                );
            }
        });
        assert!(matches!(
            get_homefront_runtime_state(&ctx, met.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::MetaliminalMeteoroid { ore_mined_by_fleet: 0, .. }
        ));
        assert!(matches!(
            get_homefront_runtime_state(&ctx, sig.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::SuspiciousSignal { arrays_hacked: [false, false, false], .. }
        ));
        assert!(matches!(
            get_homefront_runtime_state(&ctx, sal.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::SalvageResearch { wrecks_salvaged: 0, .. }
        ));
        assert!(matches!(
            get_homefront_runtime_state(&ctx, dread.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::DreadAssault { capacitor_gj, .. } if capacitor_gj == 0.0
        ));
        assert!(matches!(
            get_homefront_runtime_state(&ctx, rift.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::StabilizeRift { arrays_cap_pct, .. } if (arrays_cap_pct[0] - 1.0).abs() < 1e-6
        ));

        // 4. Live dogmaIM.Activate + sys_h.step_manual + execute_pending_cycle_actions cycle rollover
        let kw = HashMap::new();
        let mut ship_cs = ctx.build_ship_combat_state(970_003, 870_003, 16_236);
        ship_cs.max_capacitor = 25_000.0;
        ship_cs.capacitor = 25_000.0;
        sys_h.set_combat_state(870_003, ship_cs).await.unwrap();

        // 4a. EmergencyAid: shipModuleRemoteShieldBooster rollover increments repair_hp
        let mut prot_cs = CombatState::default();
        prot_cs.max_shield_hp = 5_000.0;
        prot_cs.shield_hp = 100.0;
        prot_cs.max_armor_hp = 5_000.0;
        prot_cs.armor_hp = 5_000.0;
        prot_cs.max_structure_hp = 5_000.0;
        prot_cs.structure_hp = 5_000.0;
        sys_h.set_combat_state(aid_protectee, prot_cs).await.unwrap();
        let (_, prot_ball) = sys_h.get_ball_snapshot(aid_protectee).await.unwrap().unwrap();
        sys_h
            .set_ball_position(870_003, prot_ball.position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        sys_h
            .add_target_lock(
                870_003,
                TargetLock {
                    target_id: aid_protectee,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await
            .unwrap();
        let rep_res = handle_module_cycles(
            &ctx,
            &mut anchor_sess,
            None,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(mod_rep_id),
                PyRep::String("shipModuleRemoteShieldBooster".into()),
                PyRep::Integer(aid_protectee),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("Activate Remote Shield Booster on EmergencyAid protectee");
        let rep_dur = rep_res.dict_get("duration").and_then(PyRep::as_f64).unwrap_or(8.0);
        let rep_before_rollover = ctx.db.with_snapshot(|snap| {
            snap.dungeon_runtime.homefront_runtimes[&aid.instance_id]
                .contributions
                .get(&970_003)
                .map_or(0.0, |c| c.repair_hp)
        });
        sys_h.step_manual(rep_dur + 0.25).await.unwrap();
        execute_pending_cycle_actions(&ctx, sys_id).await;
        let rep_after_rollover = ctx.db.with_snapshot(|snap| {
            snap.dungeon_runtime.homefront_runtimes[&aid.instance_id]
                .contributions
                .get(&970_003)
                .map_or(0.0, |c| c.repair_hp)
        });
        assert!(
            rep_after_rollover > rep_before_rollover,
            "shieldTransfer rollover must credit EmergencyAid repair_hp ({rep_after_rollover} > {rep_before_rollover})"
        );
        let _ = handle_mining_and_deactivate(
            &ctx,
            &mut anchor_sess,
            None,
            "dogmaIM",
            "Deactivate",
            &[PyRep::Integer(mod_rep_id)],
            &kw,
        )
        .await;

        // 4b. DreadAssault: energyTransfer rollover increments cap_transferred + dread capacitor_gj
        let mut dread_cs = CombatState::default();
        dread_cs.max_shield_hp = 50_000.0;
        dread_cs.shield_hp = 50_000.0;
        dread_cs.max_capacitor = 50_000.0;
        dread_cs.capacitor = 100.0;
        sys_h.set_combat_state(dread_id, dread_cs).await.unwrap();
        let (_, dread_ball) = sys_h.get_ball_snapshot(dread_id).await.unwrap().unwrap();
        sys_h
            .set_ball_position(870_003, dread_ball.position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        sys_h
            .add_target_lock(
                870_003,
                TargetLock {
                    target_id: dread_id,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await
            .unwrap();
        let cap_res = handle_module_cycles(
            &ctx,
            &mut anchor_sess,
            None,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(mod_cap_id),
                PyRep::String("energyTransfer".into()),
                PyRep::Integer(dread_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("Activate Remote Cap Transmitter on DreadAssault dreadnought");
        let cap_dur = cap_res.dict_get("duration").and_then(PyRep::as_f64).unwrap_or(6.0);
        let (dread_cap_pre, contrib_cap_pre) = ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&dread.instance_id];
            let gj = match &rec.progress {
                HomefrontProgressRecord::DreadAssault { capacitor_gj, .. } => *capacitor_gj,
                _ => 0.0,
            };
            let cc = rec.contributions.get(&970_003).map_or(0.0, |c| c.cap_transferred);
            (gj, cc)
        });
        sys_h.step_manual(cap_dur + 0.25).await.unwrap();
        execute_pending_cycle_actions(&ctx, sys_id).await;
        let (dread_cap_post, contrib_cap_post) = ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&dread.instance_id];
            let gj = match &rec.progress {
                HomefrontProgressRecord::DreadAssault { capacitor_gj, .. } => *capacitor_gj,
                _ => 0.0,
            };
            let cc = rec.contributions.get(&970_003).map_or(0.0, |c| c.cap_transferred);
            (gj, cc)
        });
        assert!(
            dread_cap_post > dread_cap_pre && contrib_cap_post > contrib_cap_pre,
            "energyTransfer rollover must advance DreadAssault capacitor ({dread_cap_post} > {dread_cap_pre})"
        );
        let _ = handle_mining_and_deactivate(
            &ctx,
            &mut anchor_sess,
            None,
            "dogmaIM",
            "Deactivate",
            &[PyRep::Integer(mod_cap_id)],
            &kw,
        )
        .await;

        // 4c. StabilizeRift: energyNeutralizerFalloff rollover drains StabilizeRift array cap
        let mut rift_cs = CombatState::default();
        rift_cs.max_shield_hp = 50_000.0;
        rift_cs.shield_hp = 50_000.0;
        rift_cs.max_capacitor = 5_000.0;
        rift_cs.capacitor = 2_000.0;
        sys_h.set_combat_state(rift_arr0, rift_cs).await.unwrap();
        let (_, rift_ball) = sys_h.get_ball_snapshot(rift_arr0).await.unwrap().unwrap();
        sys_h
            .set_ball_position(870_003, rift_ball.position + DVec3::new(400.0, 0.0, 0.0))
            .await;
        sys_h
            .add_target_lock(
                870_003,
                TargetLock {
                    target_id: rift_arr0,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(50_000.0),
                },
            )
            .await
            .unwrap();
        let neut_res = handle_module_cycles(
            &ctx,
            &mut anchor_sess,
            None,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(mod_neut_id),
                PyRep::String("energyNeutralizerFalloff".into()),
                PyRep::Integer(rift_arr0),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("Activate Small Energy Neutralizer on StabilizeRift array");
        let neut_dur = neut_res.dict_get("duration").and_then(PyRep::as_f64).unwrap_or(6.0);
        let (arr_pct_pre, rift_contrib_pre) = ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&rift.instance_id];
            let pct = match &rec.progress {
                HomefrontProgressRecord::StabilizeRift { arrays_cap_pct, .. } => arrays_cap_pct[0],
                _ => 1.0,
            };
            let cc = rec.contributions.get(&970_003).map_or(0.0, |c| c.cap_transferred);
            (pct, cc)
        });
        sys_h.step_manual(neut_dur + 0.25).await.unwrap();
        execute_pending_cycle_actions(&ctx, sys_id).await;
        let (arr_pct_post, rift_contrib_post) = ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&rift.instance_id];
            let pct = match &rec.progress {
                HomefrontProgressRecord::StabilizeRift { arrays_cap_pct, .. } => arrays_cap_pct[0],
                _ => 1.0,
            };
            let cc = rec.contributions.get(&970_003).map_or(0.0, |c| c.cap_transferred);
            (pct, cc)
        });
        assert!(
            arr_pct_post < arr_pct_pre && rift_contrib_post > rift_contrib_pre,
            "energyNeutralizerFalloff rollover must drain StabilizeRift array ({arr_pct_post} < {arr_pct_pre})"
        );
    }
}
