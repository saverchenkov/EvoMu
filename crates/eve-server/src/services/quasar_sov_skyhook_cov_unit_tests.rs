//! Focused coverage and mutation-killing unit tests for `quasar_sov_skyhook`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_BAD_REQUEST};
use crate::services::quasar_sov_hub;
use crate::services::sov_services::{TYPE_MAGMATIC_GAS, TYPE_SUPERIONIC_ICE};
use eve_db::{corp_roles, UpwellStructureRecord};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::CelestialInfo;
use glam::DVec3;

struct TempCelestialSqlite {
    reg: SdeRegistry,
}

impl TempCelestialSqlite {
    fn new(rows: &[(i64, u32, u32, u32, f64)]) -> Self {
        let mut reg = SdeRegistry::default();
        for &(cid, sys_id, tid, gid, radius) in rows {
            reg.insert_celestial(CelestialInfo {
                celestial_id: cid,
                solar_system_id: sys_id,
                type_id: tid,
                group_id: gid,
                name: format!("Planet-{cid}"),
                radius,
                position: DVec3::ZERO,
                orbit_id: None,
                orbit_index: None,
                celestial_index: None,
            });
        }
        Self { reg }
    }

    fn sde(&self) -> SdeRegistry {
        self.reg.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WireField {
    Varint(u64),
    Bytes(Vec<u8>),
}

fn read_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut out = 0_u64;
    let mut shift = 0_u32;
    loop {
        let b = buf[*pos];
        *pos += 1;
        out |= u64::from(b & 0x7F) << shift;
        if (b & 0x80) == 0 {
            return out;
        }
        shift += 7;
    }
}

fn decode_wire(buf: &[u8]) -> Vec<(u32, WireField)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let tag = read_varint(buf, &mut pos);
        let field_no = (tag >> 3) as u32;
        let wt = tag & 0x07;
        match wt {
            0 => out.push((field_no, WireField::Varint(read_varint(buf, &mut pos)))),
            2 => {
                let len = read_varint(buf, &mut pos) as usize;
                out.push((field_no, WireField::Bytes(buf[pos..pos + len].to_vec())));
                pos += len;
            }
            other => panic!("unexpected wire type {other}"),
        }
    }
    out
}

fn extract_varint(fields: &[(u32, WireField)], fno: u32) -> Option<u64> {
    fields.iter().find_map(|(n, v)| match v {
        WireField::Varint(x) if *n == fno => Some(*x),
        _ => None,
    })
}

fn extract_bytes_all(fields: &[(u32, WireField)], fno: u32) -> Vec<Vec<u8>> {
    fields
        .iter()
        .filter_map(|(n, v)| match v {
            WireField::Bytes(b) if *n == fno => Some(b.clone()),
            _ => None,
        })
        .collect()
}

fn decode_id_submsg(raw: &[u8]) -> u64 {
    let inner = decode_wire(raw);
    extract_varint(&inner, 1).expect("id varint field 1")
}

fn decode_ts_submsg(raw: &[u8]) -> u64 {
    let inner = decode_wire(raw);
    extract_varint(&inner, 1).unwrap_or(0)
}

fn id_msg(field: u32, id: i64) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Int64(id));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

fn mk_struct(id: i64, type_id: u32, corp_id: i64, sys_id: u32) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: id,
        type_id,
        owner_corp_id: corp_id,
        solar_system_id: sys_id,
        name: format!("Struct-{id}"),
        state: "anchor_vulnerable".into(),
        ..Default::default()
    }
}

#[test]
fn planet_hourly_reagent_yield_planet_types_radius_clamps_and_fallbacks() {
    let sys = 30_000_142_u32;
    let tmp = TempCelestialSqlite::new(&[
        (40_100, sys, 12, 7, 0.0),            // Ice zero radius -> nominal 36
        (40_101, sys, 12, 7, -10.0),          // Ice negative radius -> nominal 36
        (40_102, sys, 12, 7, 1_000_000.0),    // Ice clamp min t=0 -> 22
        (40_103, sys, 12, 7, 5_000_000.0),    // Ice mid radius -> >22, <46
        (40_104, sys, 12, 7, 6_000_000.0),    // Ice larger radius -> >= 5M
        (40_105, sys, 12, 7, 60_000_000.0),   // Ice clamp max t=1.35 -> 46
        (40_200, sys, 2015, 7, 1_000_000.0),  // Lava clamp min t=0 -> 520
        (40_201, sys, 2015, 7, 2_500_000.0),  // Lava mid radius -> >520, <946
        (40_202, sys, 2015, 7, 4_160_000.0),  // Lava max cutoff t=1.0 -> 946
        (40_203, sys, 2015, 7, 15_000_000.0), // Lava clamp max t=1.4 -> 1116
        (40_300, sys, 2014, 7, 1_000_000.0),  // Oceanic -> 0 reagents (workforce only)
        (40_301, sys, 2014, 7, 5_000_000.0),
        (40_302, sys, 2014, 7, 15_000_000.0),
        (40_303, sys, 2063, 7, 5_000_000.0),  // Plasma -> 0
        (40_400, sys, 2017, 7, 1_000_000.0),  // Storm -> 0
        (40_401, sys, 2017, 7, 5_000_000.0),
        (40_402, sys, 2017, 7, 15_000_000.0),
        (40_403, sys, 13, 7, 5_000_000.0),    // Gas -> 0
        (40_500, sys, 11, 7, 5_000_000.0),    // Temperate -> 0
    ]);
    let sde = tmp.sde();

    assert_eq!(planet_hourly_reagent_yield(&sde, None, Some(sys), TYPE_SUPERIONIC_ICE), 0);
    assert_eq!(planet_hourly_reagent_yield(&sde, Some(40_103), None, TYPE_SUPERIONIC_ICE), 0);
    assert_eq!(planet_hourly_reagent_yield(&sde, Some(999_999), Some(sys), TYPE_SUPERIONIC_ICE), 0);
    assert_eq!(planet_hourly_reagent_yield(&sde, Some(40_500), Some(sys), TYPE_SUPERIONIC_ICE), 0);

    assert_eq!(planet_hourly_reagent_yield(&sde, Some(40_100), Some(sys), TYPE_SUPERIONIC_ICE), 36);
    assert_eq!(planet_hourly_reagent_yield(&sde, Some(40_101), Some(sys), TYPE_SUPERIONIC_ICE), 36);
    assert_eq!(planet_hourly_reagent_yield(&sde, Some(40_102), Some(sys), TYPE_SUPERIONIC_ICE), 22);
    let ice_5m = planet_hourly_reagent_yield(&sde, Some(40_103), Some(sys), TYPE_SUPERIONIC_ICE);
    let ice_6m = planet_hourly_reagent_yield(&sde, Some(40_104), Some(sys), TYPE_SUPERIONIC_ICE);
    assert_eq!(ice_5m, planet_hourly_reagent_yield_for_planet(12, 5_000_000.0));
    assert!(ice_5m > 22 && ice_5m <= ice_6m && ice_6m < 46);
    assert_eq!(planet_hourly_reagent_yield(&sde, Some(40_105), Some(sys), TYPE_SUPERIONIC_ICE), 46);

    assert_eq!(planet_hourly_reagent_yield(&sde, Some(40_200), Some(sys), TYPE_MAGMATIC_GAS), 520);
    let lava_mid = planet_hourly_reagent_yield(&sde, Some(40_201), Some(sys), TYPE_MAGMATIC_GAS);
    assert!(lava_mid > 520 && lava_mid < 946);
    assert_eq!(planet_hourly_reagent_yield(&sde, Some(40_202), Some(sys), TYPE_MAGMATIC_GAS), 946);
    assert_eq!(planet_hourly_reagent_yield(&sde, Some(40_203), Some(sys), TYPE_MAGMATIC_GAS), 1_116);

    for pid in [40_300, 40_301, 40_302, 40_303, 40_400, 40_401, 40_402, 40_403] {
        assert_eq!(planet_hourly_reagent_yield(&sde, Some(pid), Some(sys), TYPE_SUPERIONIC_ICE), 0);
    }
}

#[test]
fn skyhook_window_boundaries_and_wire_encoding_variants() {
    let sys = 30_000_142_u32;
    let tmp = TempCelestialSqlite::new(&[
        (50_001, sys, 2014, 7, 5_000_000.0), // Oceanic: workforce via curve, reagent = 0
        (50_002, sys, 12, 7, 5_000_000.0),   // Ice: workforce = 0, reagent via curve
    ]);
    let sde = tmp.sde();
    let db = WorldRepository::with_demo_seed();

    let sh_default_bay = 81_001_i64;
    let sh_zero_ts_inactive = 81_002_i64;
    let sh_no_reagent_no_planet = 81_003_i64;
    let not_skyhook = 81_004_i64;

    db.with_mut_snapshot(|snap| {
        snap.structures
            .insert(sh_default_bay, mk_struct(sh_default_bay, TYPE_ORBITAL_SKYHOOK, 10, sys));
        snap.structures.insert(
            sh_zero_ts_inactive,
            mk_struct(sh_zero_ts_inactive, TYPE_ORBITAL_SKYHOOK, 10, sys),
        );
        snap.structures.insert(
            sh_no_reagent_no_planet,
            mk_struct(sh_no_reagent_no_planet, TYPE_ORBITAL_SKYHOOK, 10, sys),
        );
        snap.structures
            .insert(not_skyhook, mk_struct(not_skyhook, 35_832, 10, sys));

        // 1) Reagents without bay entry -> fallback (stock, 0, now); active missing -> true; Oceanic planet -> radius-scaled workforce
        snap.sov_runtime
            .skyhook_reagents
            .insert(sh_default_bay, (TYPE_SUPERIONIC_ICE, 900));
        snap.sov_runtime.skyhook_planets.insert(sh_default_bay, 50_001);
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(sh_default_bay, (1_000, 2_000));

        // 2) Bay entry with ts == 0 uses `now`, inactive skyhook suppresses workforce -> field 2 = 1
        snap.sov_runtime
            .skyhook_reagents
            .insert(sh_zero_ts_inactive, (TYPE_SUPERIONIC_ICE, 900));
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(sh_zero_ts_inactive, (TYPE_SUPERIONIC_ICE, 700, 250, 0));
        snap.sov_runtime
            .skyhook_planets
            .insert(sh_zero_ts_inactive, 50_002);
        snap.sov_runtime
            .skyhook_active
            .insert(sh_zero_ts_inactive, false);
    });

    db.with_snapshot(|snap| {
        assert!(is_skyhook(snap, sh_default_bay));
        assert!(!is_skyhook(snap, not_skyhook));
        assert!(!is_skyhook(snap, 999_888_777));

        // Exact theft window boundaries: [start, end)
        assert_eq!(open_window(snap, sh_default_bay, 999), None);
        assert_eq!(open_window(snap, sh_default_bay, 1_000), Some((1_000, 2_000)));
        assert_eq!(open_window(snap, sh_default_bay, 1_999), Some((1_000, 2_000)));
        assert_eq!(open_window(snap, sh_default_bay, 2_000), None);
        assert_eq!(open_window(snap, sh_no_reagent_no_planet, 1_500), None);

        // Verify Case 1 at boundary now == 1_000 (vulnerable = 1, version field 8 = 1)
        let f1 = decode_wire(&encode_skyhook(snap, &sde, sh_default_bay, 1_000));
        assert_eq!(decode_id_submsg(&extract_bytes_all(&f1, 1)[0]), sh_default_bay as u64);
        assert_eq!(extract_varint(&f1, 3), Some(1));
        assert!(extract_bytes_all(&f1, 6).is_empty(), "field 6 collides with unknown vulnerability bool and must stay unset");
        assert_eq!(decode_id_submsg(&extract_bytes_all(&f1, 8)[0]), 1);

        let r_msg = decode_wire(&extract_bytes_all(&f1, 2)[0]);
        assert_eq!(decode_id_submsg(&extract_bytes_all(&r_msg, 1)[0]), u64::from(TYPE_SUPERIONIC_ICE));
        let sim = decode_wire(&extract_bytes_all(&r_msg, 4)[0]);
        assert_eq!(extract_varint(&sim, 1), Some(900));
        assert_eq!(extract_varint(&sim, 2), Some(0)); // explicit bay missing -> unsecured 0
        assert_eq!(decode_ts_submsg(&extract_bytes_all(&sim, 3)[0]), 1_000);

        let r_def = decode_wire(&extract_bytes_all(&f1, 4)[0]);
        let amt = decode_wire(&extract_bytes_all(&r_def, 2)[0]);
        assert_eq!(extract_varint(&amt, 1), Some(0)); // Oceanic produces 0 reagents
        assert_eq!(extract_varint(&amt, 2), Some(3_600));
        assert_eq!(extract_varint(&amt, 3), Some(50));
        assert_eq!(extract_varint(&amt, 4), Some(25_000));
        assert_eq!(extract_varint(&amt, 5), Some(25_000));

        let theft_open = decode_wire(&extract_bytes_all(&f1, 5)[0]);
        assert_eq!(extract_varint(&theft_open, 1), Some(1));
        assert_eq!(decode_ts_submsg(&extract_bytes_all(&theft_open, 2)[0]), 1_000);
        assert_eq!(decode_ts_submsg(&extract_bytes_all(&theft_open, 3)[0]), 2_000);

        let expected_wf = quasar_sov_hub::planet_yield_for_planet(2014, 5_000_000.0).1;
        let wf_active = decode_wire(&extract_bytes_all(&f1, 7)[0]);
        assert_eq!(extract_varint(&wf_active, 1), Some(expected_wf));
        assert_eq!(extract_varint(&wf_active, 2), None);

        // At now == 2_000 (end boundary), theft_vulnerability field 1 flips to 0
        let f1_closed = decode_wire(&encode_skyhook(snap, &sde, sh_default_bay, 2_000));
        let theft_closed = decode_wire(&extract_bytes_all(&f1_closed, 5)[0]);
        assert_eq!(extract_varint(&theft_closed, 1), Some(0));

        // Verify Case 2: ts == 0 falls back to `now`, Ice planet produces reagent yield, active == false encodes workforce disabled (field 2 = 1)
        let f2 = decode_wire(&encode_skyhook(snap, &sde, sh_zero_ts_inactive, 4_321));
        assert_eq!(extract_varint(&f2, 3), Some(0));
        let r_msg2 = decode_wire(&extract_bytes_all(&f2, 2)[0]);
        let sim2 = decode_wire(&extract_bytes_all(&r_msg2, 4)[0]);
        assert_eq!(extract_varint(&sim2, 1), Some(700));
        assert_eq!(extract_varint(&sim2, 2), Some(250));
        assert_eq!(decode_ts_submsg(&extract_bytes_all(&sim2, 3)[0]), 4_321);
        let r_def2 = decode_wire(&extract_bytes_all(&f2, 4)[0]);
        let amt2 = decode_wire(&extract_bytes_all(&r_def2, 2)[0]);
        assert_eq!(extract_varint(&amt2, 1), Some(planet_hourly_reagent_yield_for_planet(12, 5_000_000.0)));
        let wf_inactive = decode_wire(&extract_bytes_all(&f2, 7)[0]);
        assert_eq!(extract_varint(&wf_inactive, 1), None);
        assert_eq!(extract_varint(&wf_inactive, 2), Some(1));

        // Verify Case 3: no reagents, no planet, no theft window
        let f3 = decode_wire(&encode_skyhook(snap, &sde, sh_no_reagent_no_planet, 1_500));
        assert!(extract_bytes_all(&f3, 2).is_empty());
        assert!(extract_bytes_all(&f3, 4).is_empty());
        assert!(extract_bytes_all(&f3, 6).is_empty());
        assert!(extract_bytes_all(&f3, 8).is_empty());
        let theft_none = decode_wire(&extract_bytes_all(&f3, 5)[0]);
        assert_eq!(extract_varint(&theft_none, 1), Some(0));
        assert!(extract_bytes_all(&theft_none, 2).is_empty());
    });
}

#[test]
fn query_handlers_sorting_filtering_and_error_paths() {
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let corp_a = 98_000_010_i64;
    let corp_b = 98_000_020_i64;
    let cid = 90_000_001_i64;

    let tmp = TempCelestialSqlite::new(&[(60_001, sys_a, 12, 7, 5_000_000.0)]);
    let sde = Arc::new(tmp.sde());
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_query_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let ctx = AuthoritativeContext::for_session_at(
        Some(1),
        Some(cid as u32),
        Some(corp_a as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
        1_000,
    );
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = sys_a;
            ch.corporation_id = corp_a;
        }
        snap.corporations.insert(
            corp_a,
            { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = corp_a;
                __corp.ceo_id = cid;
                __corp },
        );
        // Insert out-of-order IDs + a non-skyhook structure to verify filtering and sorting
        snap.structures
            .insert(90_020, mk_struct(90_020, TYPE_ORBITAL_SKYHOOK, corp_a, sys_a));
        snap.structures
            .insert(90_005, mk_struct(90_005, TYPE_ORBITAL_SKYHOOK, corp_a, sys_a));
        snap.structures
            .insert(90_015, mk_struct(90_015, TYPE_ORBITAL_SKYHOOK, corp_b, sys_b));
        snap.structures
            .insert(90_099, mk_struct(90_099, 35_832, corp_a, sys_a));

        // Open windows at now=1_000 on 90_020 and 90_005; expired at exact 1_000 on 90_015; open on non-skyhook 90_099
        snap.sov_runtime.skyhook_theft_windows.insert(90_020, (1_000, 9_000));
        snap.sov_runtime.skyhook_theft_windows.insert(90_005, (200, 5_000));
        snap.sov_runtime.skyhook_theft_windows.insert(90_015, (10, 1_000));
        snap.sov_runtime.skyhook_theft_windows.insert(90_099, (0, 9_000));
    });

    let handlers = router.handlers.read().unwrap();
    let any_empty = GoogleAny::default();
    let empty_req = DynamicQuasarMessage::new("Req");
    let ghost_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(777_888_999),
        Some(corp_a as u32),
        0,
        "tranquility",
        vec![1],
    );

    // GetRequest: missing ID -> 400, non-skyhook structure -> 404
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.GetRequest"](&ctx, &any_empty, &empty_req)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.GetRequest"](&ctx, &any_empty, &id_msg(1, 90_099))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // GetAllLocalRequest: missing character -> 404, valid character -> sorted [90_005, 90_020]
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.GetAllLocalRequest"](&ghost_ctx, &any_empty, &empty_req)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    let local = handlers["eve_public.sovereignty.skyhook.api.GetAllLocalRequest"](&ctx, &any_empty, &empty_req)
        .expect("local skyhooks");
    let local_fields = decode_wire(&local.value);
    assert_eq!(decode_id_submsg(&extract_bytes_all(&local_fields, 1)[0]), u64::from(sys_a));
    let local_ids: Vec<u64> = extract_bytes_all(&local_fields, 2)
        .iter()
        .map(|b| decode_id_submsg(&extract_bytes_all(&decode_wire(b), 1)[0]))
        .collect();
    assert_eq!(local_ids, vec![90_005, 90_020]);

    // GetAllByCorporationRequest: missing ID -> 400, foreign corp -> 403, corp_a -> [90_005, 90_020]
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest"](&ctx, &any_empty, &empty_req)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest"](
            &ctx,
            &any_empty,
            &id_msg(1, 555_555),
        )
        .unwrap_err()
        .0,
        crate::quasar_gateway::STATUS_FORBIDDEN
    );

    let corp_res = handlers["eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest"](
        &ctx,
        &any_empty,
        &id_msg(1, corp_a),
    )
    .expect("corp_a skyhooks");
    let corp_ids: Vec<u64> = extract_bytes_all(&decode_wire(&corp_res.value), 1)
        .iter()
        .map(|b| decode_id_submsg(&extract_bytes_all(&decode_wire(b), 1)[0]))
        .collect();
    assert_eq!(corp_ids, vec![90_005, 90_020]);

    // Vulnerable systems: deduplicates sys_a and excludes expired sys_b and non-skyhook 90_099
    let vuln_sys = handlers["eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest"](
        &ctx, &any_empty, &empty_req,
    )
    .expect("vuln systems");
    let sys_ids: Vec<u64> = extract_bytes_all(&decode_wire(&vuln_sys.value), 1)
        .iter()
        .map(|b| decode_id_submsg(b))
        .collect();
    assert_eq!(sys_ids, vec![u64::from(sys_a)]);

    // Vulnerable skyhooks in solar system: missing ID -> 400, sys_b -> empty, sys_a -> [90_005, 90_020] with exact start/end
    assert_eq!(
        handlers["eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest"](
            &ctx, &any_empty, &empty_req,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let in_sys_b = handlers["eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest"](
        &ctx,
        &any_empty,
        &id_msg(1, i64::from(sys_b)),
    )
    .expect("vuln in sys_b");
    assert!(in_sys_b.value.is_empty());

    let in_sys_a = handlers["eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest"](
        &ctx,
        &any_empty,
        &id_msg(1, i64::from(sys_a)),
    )
    .expect("vuln in sys_a");
    let entries = extract_bytes_all(&decode_wire(&in_sys_a.value), 1);
    assert_eq!(entries.len(), 2);
    let e0 = decode_wire(&entries[0]);
    assert_eq!(decode_id_submsg(&extract_bytes_all(&e0, 1)[0]), 90_005);
    assert_eq!(decode_ts_submsg(&extract_bytes_all(&e0, 3)[0]), 5_000);
    assert_eq!(decode_ts_submsg(&extract_bytes_all(&e0, 4)[0]), 200);
}

#[test]
fn planet_hourly_reagent_yield_planet_types_and_radius_scaling() {
    let mut sde = SdeRegistry::default();
    // Fallback without planet/system returns 0 (non-reagent / unknown)
    assert_eq!(planet_hourly_reagent_yield(&sde, None, None, 81_143), 0);
    assert_eq!(planet_hourly_reagent_yield(&sde, None, None, 81_144), 0);

    // Nominal average by planet type_id (Ice=36, Lava=763, others=0)
    assert_eq!(planet_type_base_hourly_reagent_yield(12), 36);
    assert_eq!(planet_type_base_hourly_reagent_yield(2015), 763);
    assert_eq!(planet_type_base_hourly_reagent_yield(2014), 0);
    assert_eq!(planet_type_base_hourly_reagent_yield(2017), 0);
    assert_eq!(planet_type_base_hourly_reagent_yield(11), 0);

    // Radius scaling for Lava (2015) and Ice (12) planets
    Arc::make_mut(&mut sde.celestials_by_system).insert(
        30_000_142,
        vec![
            eve_sde::CelestialInfo {
                celestial_id: 40_009_080,
                solar_system_id: 30_000_142,
                type_id: 2015,
                group_id: 7,
                name: "Lava I".into(),
                radius: 5_000_000.0,
                position: glam::DVec3::ZERO,
                orbit_id: None,
                orbit_index: None,
                celestial_index: None,
            },
            eve_sde::CelestialInfo {
                celestial_id: 40_009_081,
                solar_system_id: 30_000_142,
                type_id: 12,
                group_id: 7,
                name: "Ice II".into(),
                radius: 7_500_000.0,
                position: glam::DVec3::ZERO,
                orbit_id: None,
                orbit_index: None,
                celestial_index: None,
            },
        ],
    );
    assert_eq!(
        planet_hourly_reagent_yield(&sde, Some(40_009_080), Some(30_000_142), 81_143),
        planet_hourly_reagent_yield_for_planet(2015, 5_000_000.0)
    );
    assert_eq!(
        planet_hourly_reagent_yield(&sde, Some(40_009_081), Some(30_000_142), 81_144),
        planet_hourly_reagent_yield_for_planet(12, 7_500_000.0)
    );
}
