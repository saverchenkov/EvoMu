//! Additional planet-SDE, boundary, and mutation-killing unit tests for `quasar_sov_mercden` (`Q-8`).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::{
    InventoryItemRecord, IskCents, PersistedMercenaryActivity, PersistedMercenaryDen,
    UpwellStructureRecord, WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireMessage, WireValue,
};
use eve_sde::{CelestialInfo, SdeRegistry};
use glam::DVec3;
use std::sync::{Arc, RwLock};

fn sde_with_planets(_tag: &str, planets: &[(i64, u32, f64)]) -> (Arc<SdeRegistry>, ()) {
    let mut reg = SdeRegistry::with_canonical_seed();
    for &(pid, sys_id, radius_m) in planets {
        reg.insert_celestial(CelestialInfo {
            celestial_id: pid,
            solar_system_id: sys_id,
            type_id: 11,
            group_id: 7,
            name: "Temperate Planet".into(),
            radius: radius_m,
            position: DVec3::ZERO,
            orbit_id: None,
            orbit_index: None,
            celestial_index: None,
        });
    }
    (Arc::new(reg), ())
}

fn id_req(field: u32, id: i64) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Int64(id));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

fn start_req(uuid: Vec<u8>) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("ActId");
    inner.fields.insert(1, DynamicValue::Bytes(uuid));
    let mut outer = DynamicQuasarMessage::new("Start");
    outer.fields.insert(1, DynamicValue::Message(Box::new(inner)));
    outer
}

fn call_rpc(router: &PublicGatewayRouter, name: &str, ctx: &AuthoritativeContext, msg: &DynamicQuasarMessage) -> Result<GoogleAny, (u32, String)> {
    let f = router.handlers.read().unwrap()[name].clone();
    f(ctx, &GoogleAny::default(), msg)
}

fn mk_den(den_id: i64, owner: i64, sys: u32, planet: i64, enabled: bool, extraction: bool, dev: u32, anarchy: u32, sim_at: u64, next_gen: u64) -> PersistedMercenaryDen {
    PersistedMercenaryDen {
        den_id,
        owner_char_id: owner,
        skyhook_id: 79_999_999,
        solar_system_id: sys,
        planet_id: planet,
        type_id: 85_230,
        enabled,
        cargo_extraction_enabled: extraction,
        development_level: dev,
        anarchy_level: anarchy,
        infomorphs: 0,
        simulated_at_unix: sim_at,
        next_activity_generation_unix: next_gen,
    }
}

fn mk_act(uuid: Vec<u8>, den_id: i64, sys: u32, started: bool, started_by: i64, expiry_unix: u64, dev_imp: i32, an_imp: i32, bonus: u32) -> PersistedMercenaryActivity {
    PersistedMercenaryActivity {
        uuid,
        den_id,
        solar_system_id: sys,
        dungeon_id: 0,
        started,
        started_by,
        expiry_unix,
        development_impact: dev_imp,
        anarchy_impact: an_imp,
        infomorph_bonus: bonus,
    }
}

#[test]
fn planet_scaled_rates_stage_bands_and_long_simulation_arithmetic() {
    for (st, expected) in [(1, (65, 100)), (2, (68, 105)), (3, (71, 110)), (4, (74, 115)), (5, (78, 120)), (99, (78, 120))] {
        assert_eq!(stage_infomorph_band(st), expected);
    }
    assert_eq!((infomorphs_per_hour(99), anarchy_workforce_disruption_pct(99)), (78, 40));
    for st in 1..=5 {
        assert_eq!(activity_capacity(st), 2 + st);
    }

    let sys = 30_000_142_u32;
    let (sde, _guard) = sde_with_planets(
        "bands",
        &[
            (40_000_001, sys, 1_500_000.0),
            (40_000_002, sys, 2_000_000.0),
            (40_000_003, sys, 7_000_000.0),
            (40_000_004, sys, 12_000_000.0),
            (40_000_005, sys, 18_000_000.0),
        ],
    );

    for (st, min_r, mid_r, max_r) in [(1, 65, 83, 100), (2, 68, 87, 105), (3, 71, 91, 110), (4, 74, 95, 115), (5, 78, 99, 120)] {
        assert_eq!(planet_scaled_infomorphs_per_hour(None, sys, 40_000_003, st), min_r);
        assert_eq!(planet_scaled_infomorphs_per_hour(Some(&sde), sys, 40_999_999, st), min_r);
        assert_eq!(planet_scaled_infomorphs_per_hour(Some(&sde), sys, 40_000_001, st), min_r);
        assert_eq!(planet_scaled_infomorphs_per_hour(Some(&sde), sys, 40_000_002, st), min_r);
        assert_eq!(planet_scaled_infomorphs_per_hour(Some(&sde), sys, 40_000_003, st), mid_r);
        assert_eq!(planet_scaled_infomorphs_per_hour(Some(&sde), sys, 40_000_004, st), max_r);
        assert_eq!(planet_scaled_infomorphs_per_hour(Some(&sde), sys, 40_000_005, st), max_r);
    }

    let def_planet = WireMessage::decode(&encode_infomorphs_gen_definition_for_planet(INFOMORPH_CARGO_CAPACITY, Some(&sde), sys, 40_000_003)).unwrap();
    let def_none = WireMessage::decode(&encode_infomorphs_gen_definition_for_planet(INFOMORPH_CARGO_CAPACITY, None, sys, 40_000_003)).unwrap();
    let decode_band1 = |wm: &WireMessage| match &wm.fields.iter().find(|f| f.field_number == 3).unwrap().value {
        WireValue::LengthDelimited(b) => WireMessage::decode(b).unwrap(),
        other => panic!("expected bytes, got {other:?}"),
    };
    let (b_planet, b_none) = (decode_band1(&def_planet), decode_band1(&def_none));
    assert_eq!((&b_planet.fields[1].value, &b_planet.fields[2].value), (&WireValue::Varint(65), &WireValue::Varint(100)));
    assert_eq!((&b_none.fields[1].value, &b_none.fields[2].value), (&WireValue::Varint(65), &WireValue::Varint(65)));

    let mut den = mk_den(91_300_001, 90_000_001, sys, 40_000_004, false, true, 0, 0, 1_000, 1_000);
    let mut rem = 0u64;
    assert!(!simulate_den_for_planet(&mut den, &mut rem, Some(&sde), 9_000));
    assert_eq!(den.simulated_at_unix, 9_000);

    den.enabled = true;
    assert!(!simulate_den_for_planet(&mut den, &mut rem, Some(&sde), 9_000));
    assert_eq!(den.simulated_at_unix, 9_000);

    assert!(!simulate_den_for_planet(&mut den, &mut rem, Some(&sde), 9_000 + INFOMORPH_UNIT_SECS));
    assert_eq!((den.infomorphs, rem, den.simulated_at_unix), (100, INFOMORPH_UNIT_SECS, 9_000 + INFOMORPH_UNIT_SECS));

    let base_t = den.simulated_at_unix;
    assert!(simulate_den_for_planet(&mut den, &mut rem, Some(&sde), base_t + 750 * INFOMORPH_UNIT_SECS + 123));
    assert_eq!((den.simulated_at_unix, rem), (base_t + 750 * INFOMORPH_UNIT_SECS, 7 * INFOMORPH_UNIT_SECS));
    assert_eq!((den.development_level, den.anarchy_level, den.infomorphs), (100, 100, INFOMORPH_CARGO_CAPACITY));
}

#[test]
fn owned_den_disabled_reinforcement_activity_completion_and_partial_refresh() {
    let db = WorldRepository::with_demo_seed();
    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    let (cid, den_id, sys) = (90_000_001_i64, 91_300_010_i64, 30_000_142_u32);

    db.with_mut_snapshot(|snap| {
        let mut d = mk_den(den_id, cid, sys, 40_009_080, false, true, 10, 10, 1_000, 1_000);
        d.infomorphs = 50;
        snap.sov_runtime.mercenary_dens.insert(den_id, d);
        snap.sov_runtime.mercenary_den_reinforcement.insert(den_id, (500, 1_500, 80));
        assert_eq!(owned_den(snap, cid, den_id).unwrap_err().0, STATUS_CONFLICT);

        let exp_u = vec![0xE1; 16];
        snap.sov_runtime.mercenary_activities.insert(hex(&exp_u), mk_act(exp_u, den_id, sys, false, 0, 2_000, 10, -10, 50));
        refresh_activities(snap, &pending, den_id, 2_000);
        assert!(snap.sov_runtime.mercenary_activities.is_empty());
        assert_eq!(snap.sov_runtime.mercenary_dens[&den_id].next_activity_generation_unix, 1_000);

        snap.sov_runtime.mercenary_dens.get_mut(&den_id).unwrap().enabled = true;
        for b in [0xF1u8, 0xF2u8] {
            let u = vec![b; 16];
            snap.sov_runtime.mercenary_activities.insert(hex(&u), mk_act(u, den_id, sys, false, 0, 99_000, 10, -10, 50));
        }
        refresh_activities(snap, &pending, den_id, 3_000);
        assert_eq!(snap.sov_runtime.mercenary_activities.len(), 3);
        assert_eq!(snap.sov_runtime.mercenary_dens[&den_id].next_activity_generation_unix, 3_000 + ACTIVITY_GENERATION_SECS);

        let act_sub100 = mk_act(vec![0xA9; 16], den_id, sys, true, cid, 9_000, 15, -25, 10);
        complete_mercenary_activity_in_snap(snap, &pending, &act_sub100, den_id, sys);
        let d = &snap.sov_runtime.mercenary_dens[&den_id];
        assert_eq!((d.development_level, d.anarchy_level, d.infomorphs), (25, 0, 100));

        let before_bal = snap.characters[&cid].balance;
        complete_mercenary_activity_in_snap(snap, &pending, &act_sub100, 999_999_999, sys);
        assert_eq!(snap.characters[&cid].balance, before_bal.checked_add(IskCents::from_isk(7_500_000)).unwrap());
    });
}

#[test]
fn rpc_get_as_owner_evolution_notice_get_all_expiry_capacity_and_start_positions() {
    let (sys, planet_id) = (30_000_142_u32, 40_009_080_i64);
    let (sde, _guard) = sde_with_planets("rpc", &[(planet_id, sys, 7_000_000.0)]);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(Arc::new(QuasarDescriptorRegistry::empty_with_builtins()), Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db));

    let (owner_id, other_id, corp_id) = (90_000_001_i64, 90_000_002_i64, 1_000_009_i64);
    let (den_pos_item, den_pos_struct, den_pos_default) = (91_400_001_i64, 91_400_002, 91_400_003);

    db.with_mut_snapshot(|snap| {
        snap.skills.remove(&owner_id);
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.solar_system_id = sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.item_positions.insert(den_pos_item, [10_000.0, 20_000.0, 30_000.0]);
        snap.sov_runtime.mercenary_dens.insert(den_pos_item, mk_den(den_pos_item, owner_id, sys, planet_id, true, false, 18, 10, 1_000, 500_000));
        snap.structures.insert(
            den_pos_struct,
            UpwellStructureRecord {
                structure_id: den_pos_struct,
                type_id: 85_230,
                owner_corp_id: corp_id,
                solar_system_id: sys,
                position: [50_000.0, 60_000.0, -10_000.0],
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(den_pos_struct, mk_den(den_pos_struct, owner_id, sys, planet_id, true, true, 75, 10, 1_000, 500_000));
        snap.sov_runtime.mercenary_dens.insert(den_pos_default, mk_den(den_pos_default, owner_id, sys, planet_id, true, true, 5, 5, 1_000, 500_000));
    });

    let now = 1_000 + EVOLUTION_UNIT_SECS;
    let owner_ctx = AuthoritativeContext::for_session_at(Some(1), Some(owner_id as u32), Some(corp_id as u32), 0, "tranquility", vec![1], now);
    let other_ctx = AuthoritativeContext::for_session_at(Some(2), Some(other_id as u32), Some(corp_id as u32), 0, "tranquility", vec![1], now);
    let empty = DynamicQuasarMessage::new("E");

    let max_rsp = call_rpc(&router, "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest", &owner_ctx, &empty).unwrap();
    assert!(WireMessage::decode(&max_rsp.value).unwrap().fields.iter().any(|f| f.field_number == 2 && f.value == WireValue::Varint(5)));

    let cap_zero = call_rpc(&router, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", &other_ctx, &empty).unwrap();
    assert!(WireMessage::decode(&cap_zero.value).unwrap().fields.iter().all(|f| f.value == WireValue::Varint(0)));
    let cap_six = call_rpc(&router, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", &owner_ctx, &empty).unwrap();
    assert_eq!(WireMessage::decode(&cap_six.value).unwrap().fields[0].value, WireValue::Varint(6));

    router.pending_notices.write().unwrap().clear();
    let owner_rsp = call_rpc(&router, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", &owner_ctx, &id_req(1, den_pos_item)).unwrap();
    assert!(!WireMessage::decode(&owner_rsp.value).unwrap().fields.iter().any(|f| f.field_number == 6));
    assert!(router.pending_notices.read().unwrap().iter().any(|(_, a)| a.type_url.ends_with("EvolutionStateChangedNotice")));

    let (u_eq, u_gt1, u_gt2, u_gt3) = (vec![0x11; 16], vec![0x22; 16], vec![0x33; 16], vec![0x44; 16]);
    db.with_mut_snapshot(|snap| {
        for (u, did, exp) in [
            (u_eq.clone(), den_pos_item, now),
            (u_gt1.clone(), den_pos_item, now + 1),
            (u_gt2.clone(), den_pos_struct, now + 500),
            (u_gt3.clone(), den_pos_default, now + 500),
        ] {
            snap.sov_runtime.mercenary_activities.insert(hex(&u), mk_act(u, did, sys, false, 0, exp, 10, -10, 350));
        }
    });
    let all_rsp = call_rpc(&router, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", &owner_ctx, &empty).unwrap();
    assert_eq!(WireMessage::decode(&all_rsp.value).unwrap().fields.len(), 3);

    assert_eq!(call_rpc(&router, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", &owner_ctx, &start_req(Vec::new())).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(call_rpc(&router, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", &owner_ctx, &start_req(u_eq)).unwrap_err().0, STATUS_CONFLICT);

    let ghost_cid = 99_888_777_u32;
    let ghost_ctx = AuthoritativeContext::for_session_at(Some(9), Some(ghost_cid), Some(corp_id as u32), 0, "tranquility", vec![1], now);
    db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.get_mut(&den_pos_item).unwrap().owner_char_id = i64::from(ghost_cid);
    });
    assert_eq!(call_rpc(&router, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", &ghost_ctx, &start_req(u_gt1.clone())).unwrap_err().0, STATUS_NOT_FOUND);

    db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.get_mut(&den_pos_item).unwrap().owner_char_id = owner_id;
        snap.characters.get_mut(&owner_id).unwrap().solar_system_id = 30_002_187;
    });
    assert_eq!(call_rpc(&router, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", &owner_ctx, &start_req(u_gt1.clone())).unwrap_err().0, STATUS_FORBIDDEN);

    db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&owner_id).unwrap();
        ch.solar_system_id = sys;
        ch.structure_id = Some(den_pos_struct);
    });
    assert_eq!(call_rpc(&router, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", &owner_ctx, &start_req(u_gt1.clone())).unwrap_err().0, STATUS_FORBIDDEN);
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&owner_id).unwrap().structure_id = None;
    });

    for (u, expected_pos) in [
        (u_gt1, [25_000.0, 25_000.0, 30_000.0]),
        (u_gt2, [65_000.0, 65_000.0, -10_000.0]),
        (u_gt3, [265_000.0, 5_000.0, 0.0]),
    ] {
        let h = hex(&u);
        call_rpc(&router, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", &owner_ctx, &start_req(u)).unwrap();
        let pos = db.with_snapshot(|snap| {
            let (&bid, _) = snap.sov_runtime.mto_site_balls.iter().find(|(_, v)| *v == &h).unwrap();
            let _: &InventoryItemRecord = &snap.items[&bid];
            snap.item_positions[&bid]
        });
        assert_eq!(pos, expected_pos);
    }
}
