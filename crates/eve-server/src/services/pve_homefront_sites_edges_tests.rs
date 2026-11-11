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
        assert!(list[0].base_pilot_isk > list[1].base_pilot_isk);

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
