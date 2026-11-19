//! Edge-case tests for `pve_homefront_sites` seeding, tier/scaling, and RPC error paths.

use super::*;
use crate::router::ServerContext;
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
    let rep = handle_get_homefront_site_instances(&ctx, &sess, &[]).unwrap();
    let PyRep::Dict(outer) = rep else { panic!("expected dict") };
    assert_eq!(outer[0].0, PyRep::Integer(30_000_142));
}

mod multi_role_edge_tests {
    use super::*;
    use crate::services::pve_homefront_sites::{
        activate_homefront_acceleration_gate, complete_homefront_operation_and_pay,
        dread_assault_advance_siege, dread_assault_transfer_capacitor, emergency_aid_advance_defense,
        metaliminal_mine_mutanite, raid_deliver_cargo_to_freighter,
        raid_tackle_and_destroy_hauler, record_homefront_combat_and_logistics,
        spawn_homefront_site_for_dungeon, suspicious_signal_attack_relay,
        suspicious_signal_hack_array, traffic_stop_scan_and_intercept,
    };
    use crate::services::pve_special_sites_ops::resolve_homefront_archetype_blueprint;
    use eve_db::clock::VirtualServerClock;
    use eve_db::{InventoryItemRecord, IskCents, WorldRepository};
    use eve_sde::{SolarSystemId, TypeId};
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

        // 2. Gate rejections: docked, wrong system, restricted ships (T2 Cerberus 11993, BC Drake 24698, Trig Vedmak 47270, Capsule 670)
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
        assert_eq!(
            activate_homefront_acceleration_gate(&ctx, &sess_caracal, sal.instance_id).await.unwrap_err().code,
            "ShipRestrictedByGate"
        );

        // 3. Raid edge failures: untackled hauler escapes; Caracal (<3500 m3) blocked from cargo delivery; incomplete objectives blocked
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, raid.instance_id).await.unwrap();
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

        // 4. Metaliminal requires Miner I / mining hull
        let met = spawn_homefront_site_for_dungeon(&ctx, jita, 10_312).unwrap();
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, met.instance_id).await.unwrap();
        assert_eq!(
            metaliminal_mine_mutanite(&ctx, &sess_caracal, met.instance_id, 100, 0).unwrap_err().code,
            "MiningModuleRequired"
        );

        // 5. Dread Assault rejects early Siege & cap transfer during Siege
        let dread = spawn_homefront_site_for_dungeon(&ctx, jita, 10_320).unwrap();
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

        // 6. Suspicious Signal rejects attacking relay before all 3 arrays are hacked
        let sig = spawn_homefront_site_for_dungeon(&ctx, jita, 10_309).unwrap();
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, sig.instance_id).await.unwrap();
        suspicious_signal_hack_array(&ctx, &sess_caracal, sig.instance_id, 0).unwrap();
        assert_eq!(
            suspicious_signal_attack_relay(&ctx, &sess_caracal, sig.instance_id, 50_000.0, 10).unwrap_err().code,
            "StructureInvulnerable"
        );

        // 7. Failure states & timeout: Emergency Aid protectee death, Traffic Stop civilian collateral, time limit expiration
        let aid = spawn_homefront_site_for_dungeon(&ctx, jita, 10_238).unwrap();
        activate_homefront_acceleration_gate(&ctx, &sess_caracal, aid.instance_id).await.unwrap();
        assert_eq!(
            emergency_aid_advance_defense(&ctx, &sess_caracal, aid.instance_id, 0.0, 50_000.0, 10).unwrap_err().code,
            "OperationFailed"
        );
        assert_eq!(
            emergency_aid_advance_defense(&ctx, &sess_caracal, aid.instance_id, 1_000.0, 0.0, 10).unwrap_err().code,
            "OperationFailed"
        );

        let (ctx3, sess_frigate) = test_ctx_with_pilot(930_003, 830_003, 597, jita);
        let traf = spawn_homefront_site_for_dungeon(&ctx3, jita, 10_705).unwrap();
        activate_homefront_acceleration_gate(&ctx3, &sess_frigate, traf.instance_id).await.unwrap();
        assert_eq!(
            traffic_stop_scan_and_intercept(&ctx3, &sess_frigate, traf.instance_id, 1, 3, 2_000.0).unwrap_err().code,
            "OperationFailed"
        );

        let timed = spawn_homefront_site_for_dungeon(&ctx, jita, 10_347).unwrap();
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
}
