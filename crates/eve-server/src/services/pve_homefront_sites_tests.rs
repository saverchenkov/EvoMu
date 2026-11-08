//! Unit tests for `pve_homefront_sites`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn homefront_seeding_security_filtering_and_rpc_queries() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

    let hs_sys = HOMEFRONT_SYSTEMS
        .iter()
        .copied()
        .find(|id| sde.solar_systems.contains_key(id))
        .unwrap_or(30_000_142);

    let mut session = SessionState::new(501);
    session.user_id = Some(1);
    session.character_id = Some(90_001);
    session.solar_system_id = Some(hs_sys);
    session.character_id = Some(90_050_001);
    session.solar_system_id = Some(hs_sys);

    let res = handle_get_homefront_site_instances(&ctx, &session, &[])
        .expect("highsec homefront query succeeds");
    let PyRep::Dict(ref outer) = res else {
        panic!("expected outer Dict, got {res:?}");
    };
    let (_, PyRep::List(ref entries)) = outer[0] else {
        panic!("expected system list in outer dict, got {outer:?}");
    };
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert!(entry.dict_get("dungeonID").and_then(|v| v.as_i64()).is_some());
        assert!(entry.dict_get("instanceID").and_then(|v| v.as_i64()).is_some());
        assert_eq!(entry.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(hs_sys as i64));
    }
    with_pve_special_reg(&ctx, |reg| {
        let sites = reg.homefront_sites.get(&hs_sys).expect("seeded homefront list");
        assert_eq!(sites.len(), 2);
        assert_ne!(sites[0].instance_id, sites[1].instance_id);
        assert_eq!(sites[0].optimal_fleet_size, 5);
    });

    with_pve_special_reg(&ctx, |reg| {
        ensure_homefront_sites_for_system_locked(&ctx, reg, 31_000_001);
        assert!(reg.homefront_sites.get(&31_000_001).is_some_and(|v| v.is_empty()));
        ensure_homefront_sites_for_system_locked(&ctx, reg, hs_sys);
    });

    assert_eq!(
        handle_get_homefront_site_instances(&ctx, &session, &[PyRep::String("bad".into())])
            .expect_err("non-int arg")
            .code,
        "UnknownSolarSystem"
    );
    assert_eq!(
        handle_get_homefront_site_instances(&ctx, &session, &[PyRep::Integer(99_999_999)])
            .expect_err("unknown system")
            .code,
        "UnknownSolarSystem"
    );

    let explicit = handle_get_homefront_site_instances(&ctx, &session, &[PyRep::Integer(31_000_001)])
        .expect("explicit wormhole system returns empty dict entry");
    assert!(matches!(explicit, PyRep::Dict(_)));

    let mut no_sys_session = SessionState::new(502);
    no_sys_session.user_id = Some(2);
    no_sys_session.character_id = Some(90_050_002);
    assert_eq!(
        handle_get_homefront_site_instances(&ctx, &no_sys_session, &[])
            .expect_err("no active solar system")
            .code,
        "NotInSpace"
    );
}
