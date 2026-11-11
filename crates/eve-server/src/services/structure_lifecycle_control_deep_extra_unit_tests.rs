//! Deep branch-coverage and mutation-killing unit tests for Upwell structure
//! lifecycle state transitions, reinforcement/jitter/damage caps, fitting/onlining,
//! and structure piloting / control operations.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle::{
    apply_reinforce_jitter, apply_structure_damage, apply_structure_damage_typed,
    armor_reinforcement_secs, check_anchor_position, destroy_structure, effective_max_shield,
    hull_reinforcement_secs, is_operational, nearest_npc_station, next_reinforce_exit,
    online_service_type_ids, package_structure, power_state, record_structure_hack_reveal,
    reinforce_jitter_window_secs, resolve_char_school_or_nearest_station, service_family_for_module,
    service_fuel_groups, space_class, structure_family, structure_profile, sync_all_structures,
    sync_structure, sync_structure_with_released, tether_range_m, validate_home_station_structure,
    validate_structure_refit, HullClass, PowerState, ServiceFamily, SpaceClass,
    ABANDON_AFTER_LOW_POWER_SECS, ANCHORING_SECS, ANCHOR_VULNERABLE_SECS, REPAIR_HULL_SECS,
    REPAIR_SHIELD_ARMOR_SECS, STATE_ANCHOR_VULNERABLE, STATE_ARMOR_REINFORCED,
    STATE_ARMOR_VULNERABLE, STATE_DESTROYED, STATE_HULL_VULNERABLE,
    STATE_NO_CORE, STATE_ONLINING, STATE_SHIELD_VULNERABLE,
};
use crate::services::station_structure::structure_lifecycle_rpc::{
    handle_jettison_structure_fuel, handle_structure_fit_move, handle_structure_module_online,
};
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureAccessProfile, StructureExtendedState,
};
use crate::services::structure_control_ops::{
    handle_board_structure, handle_eject_from_structure, handle_get_structure_pilot,
    handle_release_control, handle_take_control, is_structure_vessel,
    prepare_ship_for_undock_while_controlling, reject_if_controlling_structure,
    release_all_structure_control_for_character, release_unauthorized_structure_controllers_in_snap,
    rollback_ship_after_failed_undock, step_structure_auto_defense,
};
use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    let sde = SHARED_SDE
        .get_or_init(|| Arc::new(ServerContext::new().sde.as_ref().clone()))
        .clone();
    ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    state: &str,
    pos: [f64; 3],
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: format!("Citadel-{structure_id}"),
        position: pos,
        state: state.into(),
        quantum_core_installed: false,
        fuel_blocks: 0,
        shield_hp: 100_000.0,
        armor_hp: 100_000.0,
        hull_hp: 100_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        ..Default::default()
    }
}

fn kv_i64(rep: &PyRep, key: &str) -> Option<i64> {
    if let PyRep::KeyVal(map) = rep {
        map.get(key).and_then(|v| v.as_i64())
    } else {
        None
    }
}

fn kv_bool(rep: &PyRep, key: &str) -> Option<bool> {
    if let PyRep::KeyVal(map) = rep {
        if let Some(PyRep::Bool(b)) = map.get(key) {
            return Some(*b);
        }
    }
    None
}

#[test]
fn deep_structure_lifecycle_anchoring_core_power_burn_and_self_destruct() {
    let ctx = test_ctx();
    let prof = structure_profile(&ctx, 35832).expect("Astrahus profile");
    assert_eq!(prof.hull_class, HullClass::Medium);
    assert!(tether_range_m(&ctx, 35832) >= 10_000.0);
    assert!(!service_fuel_groups(&ctx).is_empty());
    assert_ne!(service_family_for_module(&ctx, 35899), ServiceFamily::Other);
    assert_eq!(structure_family(1657), ServiceFamily::Citadel);
    assert!(is_operational(STATE_SHIELD_VULNERABLE));
    assert!(!is_operational(STATE_DESTROYED));
    assert_eq!(space_class(&ctx, 30_000_142), SpaceClass::HighSec);
    assert_eq!(space_class(&ctx, 31_000_005), SpaceClass::Wormhole);
    assert!(nearest_npc_station(&ctx, 30_000_142).is_some());

    let sys_id = 30_000_142u32;
    let pochven_sys = 30_001_413u32;
    let corp_id = 98_000_001i64;
    let st_kspace = 1_030_000_001i64;
    let st_pochven = 1_030_000_002i64;
    let st_pkg = 1_030_000_003i64;
    let base_t = 1_750_000_000u64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut s_k = make_structure(st_kspace, 35832, corp_id, sys_id, STATE_ANCHOR_VULNERABLE, [10_000.0, 0.0, 0.0]);
        s_k.reinforced_until_unix = base_t + ANCHOR_VULNERABLE_SECS;
        snap.structures.insert(st_kspace, s_k);

        let mut s_p = make_structure(st_pochven, 35832, corp_id, pochven_sys, STATE_ANCHOR_VULNERABLE, [5_000_000.0, 0.0, 0.0]);
        s_p.reinforced_until_unix = base_t + ANCHOR_VULNERABLE_SECS;
        snap.structures.insert(st_pochven, s_p);

        // Stack of 2 quantum cores in structure deed bay so 1 is consumed and stack decrements to 1
        if let Some(core_tid) = prof.core_type_id {
            snap.items.insert(
                940_000_001,
                InventoryItemRecord {
                    item_id: 940_000_001,
                    type_id: core_tid,
                    owner_id: corp_id,
                    location_id: st_kspace,
                    flag: InventoryFlag::StructureDeedBay,
                    quantity: 2,
                    ..Default::default()
                },
            );
            snap.items.insert(
                940_000_002,
                InventoryItemRecord {
                    item_id: 940_000_002,
                    type_id: core_tid,
                    owner_id: corp_id,
                    location_id: st_pochven,
                    flag: InventoryFlag::StructureDeedBay,
                    quantity: 1,
                    ..Default::default()
                },
            );
        }

        // Trade-hub anchoring ban + minimum anchor spacing enforcement in non-trade-hub system
        assert!(check_anchor_position(&ctx, snap, &prof, sys_id, DVec3::new(2_500_000.0, 0.0, 0.0)).is_err());
        let open_sys = 30_004_759u32;
        snap.structures.insert(st_pkg, make_structure(st_pkg, 35832, corp_id, open_sys, STATE_SHIELD_VULNERABLE, [1.0e11, 0.0, 0.0]));
        assert!(check_anchor_position(&ctx, snap, &prof, open_sys, DVec3::new(1.0e11 + 10_000.0, 0.0, 0.0)).is_err());
        assert!(check_anchor_position(&ctx, snap, &prof, open_sys, DVec3::new(1.0e11 + 2_500_000.0, 0.0, 0.0)).is_ok());
        snap.structures.remove(&st_pkg);

        let mut ext = StructureExtendedState::default();
        let after_vuln_end = base_t + ANCHOR_VULNERABLE_SECS + 5;
        let _ = sync_structure(&ctx, snap, &mut ext, st_kspace, after_vuln_end);
        assert_eq!(snap.structures[&st_kspace].state, "Anchoring");

        let after_anchor_end = base_t + ANCHOR_VULNERABLE_SECS + ANCHORING_SECS + 10;
        let mut released_k = Vec::new();
        let _ = sync_structure_with_released(&ctx, snap, &mut ext, st_kspace, after_anchor_end, &mut released_k);
        assert!(released_k.is_empty());
        let s_k_reinf_until = {
            let s_k_now = &snap.structures[&st_kspace];
            if prof.core_type_id.is_some() {
                assert!(s_k_now.quantum_core_installed);
                assert_eq!(s_k_now.state, STATE_ONLINING);
                assert_eq!(snap.items[&940_000_001].quantity, 1);
            }
            s_k_now.reinforced_until_unix
        };

        // Pochven transitions AnchorVulnerable -> Anchoring -> NoCore even when core item is present
        let _ = sync_structure(&ctx, snap, &mut ext, st_pochven, base_t + ANCHOR_VULNERABLE_SECS + 5);
        assert_eq!(snap.structures[&st_pochven].state, "Anchoring");
        let _ = sync_structure(&ctx, snap, &mut ext, st_pochven, after_anchor_end + 10);
        if prof.core_type_id.is_some() {
            assert_eq!(snap.structures[&st_pochven].state, STATE_NO_CORE);
        }

        // Advance past onlining into ShieldVulnerable, then fit a service module with 0 fuel -> auto offlines on burn_fuel
        let online_t = s_k_reinf_until + 5;
        let _ = sync_structure(&ctx, snap, &mut ext, st_kspace, online_t);
        assert_eq!(snap.structures[&st_kspace].state, STATE_SHIELD_VULNERABLE);

        let svc_id = 940_000_010i64;
        snap.items.insert(
            svc_id,
            InventoryItemRecord {
                item_id: svc_id,
                type_id: 35899,
                owner_id: corp_id,
                location_id: st_kspace,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_id, true);
        let _ = sync_structure(&ctx, snap, &mut ext, st_kspace, online_t + 1);
        assert!(!snap.module_online_states[&svc_id]);
        assert_eq!(power_state(&ctx, snap, &ext, &snap.structures[&st_kspace], online_t + 1), PowerState::LowPower);

        // Refuel (720 online cost + 40 hourly fuel), online service module, then run 5 hours of burn_fuel until depleted
        if let Some(s) = snap.structures.get_mut(&st_kspace) {
            s.fuel_blocks = 760;
        }
        snap.module_online_states.insert(svc_id, true);
        let _ = sync_structure(&ctx, snap, &mut ext, st_kspace, online_t + 2);
        assert_eq!(power_state(&ctx, snap, &ext, &snap.structures[&st_kspace], online_t + 2), PowerState::FullPower);
        assert!(online_service_type_ids(&ctx, snap, st_kspace).contains(&35899));

        let _ = sync_structure(&ctx, snap, &mut ext, st_kspace, online_t + 5 * 3_600);
        assert!(!snap.module_online_states[&svc_id]);
        let abandoned_t = online_t + 5 * 3_600 + ABANDON_AFTER_LOW_POWER_SECS + 10;
        assert_eq!(power_state(&ctx, snap, &ext, &snap.structures[&st_kspace], abandoned_t), PowerState::Abandoned);

        // Self-destruct timer completes disposal -> transitions to STATE_DESTROYED
        if let Some(s) = snap.structures.get_mut(&st_kspace) {
            s.self_destruct_at_unix = Some(abandoned_t + 100);
        }
        let outcome = sync_structure(&ctx, snap, &mut ext, st_kspace, abandoned_t + 100).expect("self destruct");
        assert!(outcome.packaged_item_id.is_none());
        assert_eq!(snap.structures[&st_kspace].state, STATE_DESTROYED);

        // Unanchoring completes package_structure
        snap.structures.insert(st_pkg, make_structure(st_pkg, 35832, corp_id, sys_id, STATE_SHIELD_VULNERABLE, [8_000_000.0, 0.0, 0.0]));
        ext.unanchoring_until_unix.insert(st_pkg, abandoned_t + 200);
        let pkg = sync_structure(&ctx, snap, &mut ext, st_pkg, abandoned_t + 200).expect("packaged");
        assert!(pkg.packaged_item_id.is_some());
        assert!(!snap.structures.contains_key(&st_pkg));
        save_ext_state(snap, &ext);
    });
    let _ = sync_all_structures(&ctx);
    assert!(resolve_char_school_or_nearest_station(&ctx, &ctx.db.with_snapshot(|s| s.clone()), 90_000_001, sys_id) > 0);
}

#[test]
fn deep_structure_reinforce_timers_damage_caps_repair_pause_and_typed_resists() {
    let ctx = test_ctx();
    let now = 1_750_100_000u64;
    // Timer & jitter helper invariants across hull classes, space classes, and power states
    let next_exit = next_reinforce_exit(now + 10_000, 18);
    assert!(next_exit >= now + 10_000);
    assert_eq!((next_exit / 3_600) % 24, 18);
    assert_eq!(reinforce_jitter_window_secs(HullClass::Medium, false), 0);
    assert_eq!(reinforce_jitter_window_secs(HullClass::Medium, true), 5_400);
    assert_eq!(reinforce_jitter_window_secs(HullClass::Large, false), 10_800);
    assert_eq!(reinforce_jitter_window_secs(HullClass::XLarge, false), 10_800);
    assert_eq!(reinforce_jitter_window_secs(HullClass::Flex, false), 1_800);
    assert_eq!(apply_reinforce_jitter(500, -900, 1_800), 0);
    assert_eq!(apply_reinforce_jitter(5_000, 600, 1_800), 5_600);
    assert_eq!(armor_reinforcement_secs(HullClass::Medium, SpaceClass::LowNullSec, PowerState::LowPower), Some(84 * 3_600));
    assert_eq!(armor_reinforcement_secs(HullClass::Medium, SpaceClass::HighSec, PowerState::LowPower), Some(108 * 3_600));
    assert!(armor_reinforcement_secs(HullClass::Large, SpaceClass::HighSec, PowerState::LowPower).is_none());
    assert!(armor_reinforcement_secs(HullClass::Flex, SpaceClass::LowNullSec, PowerState::FullPower).is_none());
    assert!(armor_reinforcement_secs(HullClass::Medium, SpaceClass::LowNullSec, PowerState::Abandoned).is_none());
    assert!(hull_reinforcement_secs(HullClass::Medium, SpaceClass::LowNullSec, PowerState::FullPower).is_none());
    assert_eq!(hull_reinforcement_secs(HullClass::Large, SpaceClass::Wormhole, PowerState::FullPower), Some(36 * 3_600));

    let sys_null = 30_004_759u32;
    let sys_high = 30_000_142u32;
    let corp_id = 98_000_001i64;
    let attacker = 90_000_002i64;
    let sid_full = 1_030_000_101i64;
    let sid_low = 1_030_000_102i64;

    ctx.db.with_mut_snapshot(|snap| {
        assert!(apply_structure_damage(&ctx, snap, &mut StructureExtendedState::default(), 999_999_999, 5_000.0, 5.0, attacker, now).is_err());

        let mut ext = StructureExtendedState::default();
        // 1. Deploying structure: below-threshold damage does NOT extend timer; qualifying fire DOES extend timer
        let mut s_dep = make_structure(sid_full, 35832, corp_id, sys_null, STATE_ONLINING, [1_000_000.0, 0.0, 0.0]);
        s_dep.quantum_core_installed = true;
        s_dep.fuel_blocks = 100;
        s_dep.reinforced_until_unix = now + 500;
        snap.structures.insert(sid_full, s_dep);

        let weak = apply_structure_damage(&ctx, snap, &mut ext, sid_full, 50.0, 5.0, attacker, now).unwrap();
        assert!(!weak.repair_paused);
        assert_eq!(weak.reinforced_until_unix, now + 500);

        let strong = apply_structure_damage(&ctx, snap, &mut ext, sid_full, 80_000.0, 5.0, attacker, now).unwrap();
        assert!(strong.repair_paused);
        assert_eq!(strong.reinforced_until_unix, now + 505);

        // 2. Full-power Nullsec Astrahus with online service: Shield -> ArmorReinforced -> ArmorVulnerable -> HullReinforced -> HullVulnerable -> Destroyed
        snap.items.insert(
            940_000_110,
            InventoryItemRecord {
                item_id: 940_000_110,
                type_id: 35899,
                owner_id: corp_id,
                location_id: sid_full,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(940_000_110, true);
        if let Some(s) = snap.structures.get_mut(&sid_full) {
            s.state = STATE_SHIELD_VULNERABLE.into();
            s.shield_hp = 10_000.0;
            s.armor_hp = 10_000.0;
            s.hull_hp = 500.0;
            s.reinforced_until_unix = 0;
        }
        ext.lifecycle.clear();

        // Typed shield volley depletes shield (damage cap = 5,000 DPS * 5s = 25,000 cap) -> ArmorReinforced
        let out_sh = apply_structure_damage_typed(&ctx, snap, &mut ext, sid_full, 40_000.0, 0.0, 0.0, 0.0, 5.0, attacker, now).unwrap();
        assert_eq!(out_sh.layer, "shield");
        assert_eq!(out_sh.state, STATE_ARMOR_REINFORCED);
        assert!(out_sh.shield_hp <= 0.0);
        // Invulnerable while reinforced
        assert!(apply_structure_damage(&ctx, snap, &mut ext, sid_full, 10_000.0, 5.0, attacker, now + 10).is_err());

        // Exit ArmorReinforced -> ArmorVulnerable, then typed armor volley -> HullReinforced
        let ar_exit = snap.structures[&sid_full].reinforced_until_unix;
        let _ = sync_structure(&ctx, snap, &mut ext, sid_full, ar_exit);
        assert_eq!(snap.structures[&sid_full].state, STATE_ARMOR_VULNERABLE);
        assert_eq!(ext.lifecycle[&sid_full].repair_until_unix, Some(ar_exit + REPAIR_SHIELD_ARMOR_SECS));

        let out_ar = apply_structure_damage_typed(&ctx, snap, &mut ext, sid_full, 0.0, 40_000.0, 0.0, 0.0, 5.0, attacker, ar_exit + 5).unwrap();
        assert_eq!(out_ar.layer, "armor");
        assert_eq!(out_ar.state, STATE_HULL_VULNERABLE);
        assert_eq!(ext.lifecycle[&sid_full].repair_until_unix, Some(ar_exit + 5 + REPAIR_HULL_SECS));

        let out_hu = apply_structure_damage_typed(&ctx, snap, &mut ext, sid_full, 0.0, 0.0, 100_000.0, 100_000.0, 5.0, attacker, ar_exit + 10).unwrap();
        assert_eq!(out_hu.layer, "hull");
        assert_eq!(out_hu.state, STATE_DESTROYED);
        assert!(out_hu.destruction.is_some());
        assert_eq!(snap.structures[&sid_full].state, STATE_DESTROYED);

        // 3. LowPower Highsec Astrahus enters ArmorReinforced (4.5d); Abandoned Astrahus skips ArmorReinforced directly into ArmorVulnerable
        let mut s_low = make_structure(sid_low, 35832, corp_id, sys_high, STATE_SHIELD_VULNERABLE, [3_000_000.0, 0.0, 0.0]);
        s_low.shield_hp = 10_000.0;
        let max_low_sh = effective_max_shield(&ctx, snap, &structure_profile(&ctx, 35832).unwrap(), sid_low, PowerState::LowPower);
        assert!(max_low_sh > 0.0);
        snap.structures.insert(sid_low, s_low);
        let low_out = apply_structure_damage(&ctx, snap, &mut ext, sid_low, 40_000.0, 5.0, attacker, now).unwrap();
        assert_eq!(low_out.state, STATE_ARMOR_REINFORCED);

        let sid_abn = 1_030_000_103i64;
        let mut s_abn = make_structure(sid_abn, 35832, corp_id, sys_high, STATE_SHIELD_VULNERABLE, [6_000_000.0, 0.0, 0.0]);
        s_abn.shield_hp = 10_000.0;
        ext.lifecycle.entry(sid_abn).or_default().low_power_since_unix = Some(1);
        snap.structures.insert(sid_abn, s_abn);
        let abn_out = apply_structure_damage(&ctx, snap, &mut ext, sid_abn, 40_000.0, 5.0, attacker, now).unwrap();
        assert_eq!(abn_out.state, STATE_ARMOR_VULNERABLE);
        save_ext_state(snap, &ext);
    });

    assert!(record_structure_hack_reveal(&ctx, attacker, sid_low).is_some());
    assert!(record_structure_hack_reveal(&ctx, attacker, sid_low).is_some());
    assert!(record_structure_hack_reveal(&ctx, attacker, sid_low).is_some());
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert_eq!(validate_structure_refit(&ctx, snap, &ext, sid_low, now + 100).unwrap_err().code, "StructureFittingLockedInState");
        assert!(validate_home_station_structure(&ctx, snap, attacker, sid_low).is_err());
    });
    let mut destroyed_ext = StructureExtendedState::default();
    let dest = ctx.db.with_mut_snapshot(|snap| destroy_structure(&ctx, snap, &mut destroyed_ext, sid_low, attacker, now + 200));
    assert!(dest.packaged_item_id.is_none());
    let _ = package_structure;
}

#[tokio::test]
async fn deep_structure_fit_move_online_and_control_lifecycle() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_000_001i64;
    let outsider_id = 90_000_002i64;
    let corp_id = 98_000_001i64;
    let sid = 1_030_000_201i64;
    let ship_id = 100_000_001i64;
    let rig_item_id = 940_000_201i64;
    let svc_item_id = 940_000_202i64;
    let launcher_tid = ctx.sde.types.values().find(|t| t.category_id == 66 && t.group_id != 1322).map(|t| t.type_id).unwrap_or(35899);
    let launcher_item_id = 940_000_203i64;
    let core_tid = structure_profile(&ctx, 35832).and_then(|p| p.core_type_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.structure_id = Some(sid);
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sid;
            sh.flag = InventoryFlag::Hangar;
        }
        let mut s = make_structure(sid, 35832, corp_id, sys_id, STATE_SHIELD_VULNERABLE, [1_000_000.0, 0.0, 0.0]);
        s.fuel_blocks = 0;
        snap.structures.insert(sid, s);
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            1,
            StructureAccessProfile {
                profile_id: 1,
                owner_corp_id: corp_id,
                ..Default::default()
            },
        );
        ext.structure_profile_id.insert(sid, 1);
        save_ext_state(snap, &ext);

        snap.items.insert(
            svc_item_id,
            InventoryItemRecord {
                item_id: svc_item_id,
                type_id: 35899,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                ..Default::default()
            },
        );
        if let Some(rig_t) = ctx.sde.types.values().find(|t| t.category_id == 66 && t.default_attributes.get(&1547).is_some_and(|&v| (v - 2.0).abs() < 0.2) && t.default_attributes.keys().any(|&k| k == 1137)) {
            snap.items.insert(
                rig_item_id,
                InventoryItemRecord {
                    item_id: rig_item_id,
                    type_id: rig_t.type_id,
                    owner_id: char_id,
                    location_id: sid,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            launcher_item_id,
            InventoryItemRecord {
                item_id: launcher_item_id,
                type_id: launcher_tid,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    // Outsider fails ownership check
    let mut bad_sess = SessionState::new(9);
    bad_sess.user_id = Some(9);
    bad_sess.character_id = Some(outsider_id);
    bad_sess.corporation_id = Some(98_000_999);
    bad_sess.solar_system_id = Some(sys_id);
    assert_eq!(handle_structure_fit_move(&ctx, &mut bad_sess, svc_item_id, sid, InventoryFlag::ServiceSlot0, Some(true)).unwrap_err().code, "StructureOwnershipViolation");

    // Director session docked at structure
    let mut sess = SessionState::new(1);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = corp_roles::ROLE_DIRECTOR;
    sess.solar_system_id = Some(sys_id);
    sess.structure_id = Some(sid);
    sess.ship_id = Some(ship_id);

    // Damaged structure rejects refit; once at 100% HP, checks QuantumCoreRequired -> InsufficientStructureFuelToOnline
    assert_eq!(handle_structure_fit_move(&ctx, &mut sess, svc_item_id, sid, InventoryFlag::ServiceSlot0, Some(true)).unwrap_err().code, "StructureDamagedCannotRefit");
    let full_prof = structure_profile(&ctx, 35832).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.structures.get_mut(&sid) {
            s.shield_hp = full_prof.shield_hp;
            s.armor_hp = full_prof.armor_hp;
            s.hull_hp = full_prof.hull_hp;
        }
    });
    if core_tid.is_some() {
        assert_eq!(handle_structure_fit_move(&ctx, &mut sess, svc_item_id, sid, InventoryFlag::ServiceSlot0, Some(true)).unwrap_err().code, "QuantumCoreRequired");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(s) = snap.structures.get_mut(&sid) {
                s.quantum_core_installed = true;
            }
        });
    }
    assert_eq!(handle_structure_fit_move(&ctx, &mut sess, svc_item_id, sid, InventoryFlag::ServiceSlot0, Some(true)).unwrap_err().code, "InsufficientStructureFuelToOnline");

    // Refuel (>= 1440 fuel blocks for fit+online then offline+reonline) and fit stack of 2 service modules
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.structures.get_mut(&sid) {
            s.fuel_blocks = 2_000;
        }
    });
    let fitted_rep = handle_structure_fit_move(&ctx, &mut sess, svc_item_id, sid, InventoryFlag::ServiceSlot0, Some(true)).unwrap();
    let fitted_svc_id = kv_i64(&fitted_rep, "itemID").unwrap();
    assert_ne!(fitted_svc_id, svc_item_id);
    assert!(handle_structure_module_online(&ctx, &mut sess, sid, fitted_svc_id, false).is_ok());
    assert!(handle_structure_module_online(&ctx, &mut sess, sid, fitted_svc_id, true).is_ok());

    // Rig fitting immovability and unfit destruction
    if ctx.db.with_snapshot(|s| s.items.contains_key(&rig_item_id)) {
        assert!(handle_structure_fit_move(&ctx, &mut sess, rig_item_id, sid, InventoryFlag::RigSlot0, Some(true)).is_ok());
        assert_eq!(handle_structure_fit_move(&ctx, &mut sess, rig_item_id, sid, InventoryFlag::RigSlot1, Some(true)).unwrap_err().code, "CannotMoveFittedRig");
        let trash_rep = handle_structure_fit_move(&ctx, &mut sess, rig_item_id, sid, InventoryFlag::Hangar, None).unwrap();
        assert_eq!(kv_bool(&trash_rep, "destroyed"), Some(true));
    }

    // Structure piloting / boarding / release / eject / auto-defense
    assert_eq!(handle_get_structure_pilot(&ctx, &sess, &[PyRep::Integer(sid)]).unwrap(), PyRep::None);
    assert!(handle_take_control(&ctx, &mut sess, &[PyRep::Integer(sid)]).await.is_ok());
    assert_eq!(sess.ship_id, Some(sid));
    assert!(is_structure_vessel(&ctx, sid));
    assert_eq!(reject_if_controlling_structure(&ctx, sid, "warp").unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(handle_get_structure_pilot(&ctx, &sess, &[PyRep::Integer(sid)]).unwrap(), PyRep::Integer(char_id));

    let pre_undock = prepare_ship_for_undock_while_controlling(&ctx, sid, char_id);
    assert_eq!(pre_undock, Some(ship_id));
    rollback_ship_after_failed_undock(&ctx, sid, char_id);
    assert_eq!(handle_get_structure_pilot(&ctx, &sess, &[PyRep::Integer(sid)]).unwrap(), PyRep::Integer(char_id));

    assert!(handle_release_control(&ctx, &mut sess, &[PyRep::Integer(sid), PyRep::Integer(0)]).is_ok());
    assert_eq!(handle_release_control(&ctx, &mut sess, &[PyRep::Integer(sid)]).unwrap_err().code, "NotControllingStructure");

    assert!(handle_board_structure(&ctx, &mut sess, &[PyRep::Integer(sid)]).await.is_ok());
    assert!(handle_eject_from_structure(&ctx, &mut sess, &[PyRep::Integer(sid)]).is_ok());
    assert_eq!(handle_get_structure_pilot(&ctx, &sess, &[PyRep::Integer(sid)]).unwrap(), PyRep::None);

    // Unauthorized controller cleanup when character leaves owning corp & auto-defense empty system early exit
    assert!(handle_take_control(&ctx, &mut sess, &[PyRep::Integer(sid)]).await.is_ok());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = 1_000_009;
        }
        let released = release_unauthorized_structure_controllers_in_snap(snap);
        assert_eq!(released.len(), 1);
    });
    assert!(release_all_structure_control_for_character(&ctx, None, char_id).is_empty());
    step_structure_auto_defense(&ctx, sys_id, 0.0).await;
    step_structure_auto_defense(&ctx, sys_id, 1.0).await;

    // Jettison fuel checks ownership first, then jettisons fuel into a cargo container
    assert_eq!(handle_jettison_structure_fuel(&ctx, &mut sess, &[PyRep::Integer(sid)]).await.unwrap_err().code, "StructureOwnershipViolation");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
    });
    let jet = handle_jettison_structure_fuel(&ctx, &mut sess, &[PyRep::Integer(sid)]).await.unwrap();
    assert_eq!(kv_i64(&jet, "remainingFuelBlocks"), Some(0));
    assert!(kv_i64(&jet, "containerID").unwrap_or(0) > 0);
}

#[tokio::test]
async fn deep_structure_destroy_evacuate_clones_package_sync_and_home_station_validation() {
    use crate::services::station_structure::structure_lifecycle::{
        nearest_npc_station_from_pos, prepare_home_station_respawn, sync_asset_safety_wraps,
        STATE_ANCHORING,
    };

    let ctx = test_ctx();
    let sys_id = 30_000_142u32; // HighSec (Jita)
    let wh_sys_id = 31_000_005u32;
    let pochven_sys = 30_001_413u32;
    let corp_id = 98_000_050i64;
    let char_with_ship = 90_000_001i64;
    let char_no_ship = 90_000_002i64;
    let gunner_char = 90_000_003i64;
    let attacker_char = 90_000_004i64;
    let now = 1_760_000_000u64;

    // 1. Profiles, families, nearest_npc_station_from_pos, reinforce windows, power_state
    for tid in [35832u32, 35833, 35834, 35840, 2233, 3962, 81826] {
        let _ = structure_profile(&ctx, tid);
    }
    assert_eq!(structure_family(1657), ServiceFamily::Citadel);
    assert_eq!(structure_family(1404), ServiceFamily::Engineering);
    assert_eq!(structure_family(1406), ServiceFamily::Refinery);
    assert_eq!(structure_family(1408), ServiceFamily::Other);
    for m_tid in [35899u32, 35894, 35896, 35892] {
        let _ = service_family_for_module(&ctx, m_tid);
    }
    assert!(nearest_npc_station_from_pos(&ctx, sys_id, DVec3::new(1000.0, 2000.0, 3000.0)).is_some());
    assert!(nearest_npc_station_from_pos(&ctx, wh_sys_id, DVec3::ZERO).is_some());

    // Reinforce jitter and rollover bounds
    assert_eq!(next_reinforce_exit(86_400 + 3_600, 2), 86_400 + 2 * 3_600 + 86_400 - 86_400);
    assert!(reinforce_jitter_window_secs(HullClass::Flex, true) <= reinforce_jitter_window_secs(HullClass::XLarge, true));
    let _ = apply_reinforce_jitter(86_400 * 10, 50_000, 10_800);
    let _ = apply_reinforce_jitter(86_400 * 10, -50_000, 10_800);
    assert_eq!(apply_reinforce_jitter(86_400 * 10, 100, 0), 86_400 * 10);
    assert_eq!(armor_reinforcement_secs(HullClass::Medium, SpaceClass::Wormhole, PowerState::LowPower), Some(216_000));
    assert_eq!(armor_reinforcement_secs(HullClass::Medium, SpaceClass::Wormhole, PowerState::Abandoned), None);
    assert!(armor_reinforcement_secs(HullClass::Large, SpaceClass::HighSec, PowerState::FullPower).is_some());
    assert!(hull_reinforcement_secs(HullClass::XLarge, SpaceClass::WarHq, PowerState::FullPower).is_some());
    assert_eq!(hull_reinforcement_secs(HullClass::Medium, SpaceClass::LowNullSec, PowerState::Abandoned), None);

    // Faction Fortizar (47512) exempt from Abandoned even past 7d LowPower
    ctx.db.with_mut_snapshot(|snap| {
        let ff_id = 1_030_000_501i64;
        let reg_id = 1_030_000_502i64;
        snap.structures.insert(
            ff_id,
            make_structure(ff_id, 47512, corp_id, sys_id, STATE_SHIELD_VULNERABLE, [0.0, 0.0, 0.0]),
        );
        snap.structures.insert(
            reg_id,
            make_structure(reg_id, 35832, corp_id, sys_id, STATE_SHIELD_VULNERABLE, [2_000_000.0, 0.0, 0.0]),
        );
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(ff_id).or_default().low_power_since_unix = Some(now - ABANDON_AFTER_LOW_POWER_SECS - 100);
        ext.lifecycle.entry(reg_id).or_default().low_power_since_unix = Some(now - ABANDON_AFTER_LOW_POWER_SECS - 100);
        let ff = snap.structures.get(&ff_id).unwrap();
        let reg = snap.structures.get(&reg_id).unwrap();
        assert_eq!(power_state(&ctx, snap, &ext, ff, now), PowerState::LowPower);
        assert_eq!(power_state(&ctx, snap, &ext, reg, now), PowerState::Abandoned);
        let prof = structure_profile(&ctx, 35832).unwrap();
        assert!(effective_max_shield(&ctx, snap, &prof, reg_id, PowerState::LowPower) <= effective_max_shield(&ctx, snap, &prof, reg_id, PowerState::FullPower));
        save_ext_state(snap, &ext);
    });

    // Spawn active system ballpark so emit_structure_left_space_and_spawn_drops exercises space drops
    let handle = ctx.systems.get_or_spawn(sys_id);
    let sid_kspace = 1_030_000_601i64;
    let sid_wh = 1_030_000_602i64;
    let sid_pkg = 1_030_000_603i64;
    let sid_sov = 1_030_000_604i64;
    let sid_pochven = 1_030_000_605i64;
    let sid_nocore = 1_030_000_606i64;
    let sid_tenebrex = 1_030_000_607i64;

    // 2. Full K-space structure destruction: evacuates docked pilots (with valid ship & without ship->Capsule 670),
    // disengages active gunner, wipes & relocates jump clones + implants, resets home_station_id, destroys rigs,
    // alternates fitted drops, asset-safety wraps hangar container + child items!
    let mut kspace_outcome = Default::default();
    ctx.db.with_mut_snapshot(|snap| {
        let mut s = make_structure(sid_kspace, 35832, corp_id, sys_id, STATE_HULL_VULNERABLE, [10_000.0, 20_000.0, 0.0]);
        s.quantum_core_installed = true;
        s.hull_hp = 1.0;
        snap.structures.insert(sid_kspace, s);

        let ship_id = 100_000_001i64;
        if let Some(ch) = snap.characters.get_mut(&char_with_ship) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.structure_id = Some(sid_kspace);
            ch.station_id = None;
            ch.active_ship_id = ship_id;
            ch.home_station_id = sid_kspace;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sid_kspace;
            sh.flag = InventoryFlag::Hangar;
        }
        // Second docked pilot without an active ship -> spawns Capsule 670
        let template = snap.characters.get(&char_with_ship).cloned().unwrap();
        let mut ch2 = template.clone();
        ch2.character_id = char_no_ship;
        ch2.name = "CapsulePilot".into();
        ch2.active_ship_id = 0;
        ch2.structure_id = Some(sid_kspace);
        ch2.home_station_id = sid_kspace;
        snap.characters.insert(char_no_ship, ch2);

        // Gunner controlling structure + third character for attacker
        let mut ch3 = template;
        ch3.character_id = gunner_char;
        ch3.name = "Gunner".into();
        ch3.structure_id = Some(sid_kspace);
        ch3.active_ship_id = ship_id;
        snap.characters.insert(gunner_char, ch3);

        // Jump clone at destroyed structure with implants -> relocated & implants cleared
        snap.jump_clones.entry(char_with_ship).or_default().push(eve_db::JumpCloneRecord {
            jump_clone_id: 880_001,
            character_id: char_with_ship,
            location_id: sid_kspace,
            implants: vec![9941, 9942],
        });

        // Items inside structure: Rig (destroyed), 2 fitted modules (50% alternating drop), hangar box + child item
        snap.items.insert(950_001, InventoryItemRecord {
            item_id: 950_001,
            type_id: 26302,
            owner_id: corp_id,
            location_id: sid_kspace,
            flag: InventoryFlag::RigSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
        snap.items.insert(950_002, InventoryItemRecord {
            item_id: 950_002,
            type_id: 35899,
            owner_id: corp_id,
            location_id: sid_kspace,
            flag: InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
        snap.items.insert(950_003, InventoryItemRecord {
            item_id: 950_003,
            type_id: 35894,
            owner_id: corp_id,
            location_id: sid_kspace,
            flag: InventoryFlag::MedSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
        snap.items.insert(950_004, InventoryItemRecord {
            item_id: 950_004,
            type_id: 3293, // Station Container
            owner_id: char_with_ship,
            location_id: sid_kspace,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
        snap.items.insert(950_005, InventoryItemRecord {
            item_id: 950_005,
            type_id: 34, // Tritanium inside container
            owner_id: char_with_ship,
            location_id: 950_004,
            flag: InventoryFlag::Hangar,
            quantity: 1_000,
            singleton: false,
            ..Default::default()
        });

        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid_kspace, gunner_char);
        kspace_outcome = destroy_structure(&ctx, snap, &mut ext, sid_kspace, attacker_char, now);
        save_ext_state(snap, &ext);

        assert!(kspace_outcome.killmail_id.is_some());
        assert!(kspace_outcome.wreck_item_id.is_some());
        assert!(!kspace_outcome.asset_safety_wrap_ids.is_empty());
        assert!(kspace_outcome.destroyed_item_ids.contains(&950_001));
        assert!(snap.characters[&char_with_ship].structure_id.is_none());
        assert_ne!(snap.characters[&char_with_ship].home_station_id, sid_kspace);
        assert!(snap.characters[&char_no_ship].active_ship_id > 0);
        let clones = &snap.jump_clones[&char_with_ship];
        let moved = clones.iter().find(|c| c.jump_clone_id == 880_001).unwrap();
        assert_ne!(moved.location_id, sid_kspace);
        assert!(moved.implants.is_empty());
    });
    assert!(handle.list_balls().await.unwrap().iter().any(|b| Some(b.id) == kspace_outcome.wreck_item_id));

    // 3. Wormhole destruction drops hangar items directly into space (NO asset safety wraps)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid_wh,
            make_structure(sid_wh, 35832, corp_id, wh_sys_id, STATE_HULL_VULNERABLE, [5_000.0, 0.0, 0.0]),
        );
        snap.items.insert(950_101, InventoryItemRecord {
            item_id: 950_101,
            type_id: 34,
            owner_id: char_with_ship,
            location_id: sid_wh,
            flag: InventoryFlag::Hangar,
            quantity: 500,
            singleton: false,
            ..Default::default()
        });
        let mut ext = load_ext_state(snap);
        let wh_out = destroy_structure(&ctx, snap, &mut ext, sid_wh, attacker_char, now);
        assert!(wh_out.asset_safety_wrap_ids.is_empty());
        assert!(wh_out.dropped_item_ids.contains(&950_101));
        save_ext_state(snap, &ext);
    });

    // 4. Package (unanchor) + SovHub cleanup on leave space
    ctx.db.with_mut_snapshot(|snap| {
        let mut s_pkg = make_structure(sid_pkg, 35832, corp_id, sys_id, STATE_SHIELD_VULNERABLE, [15_000.0, 0.0, 0.0]);
        s_pkg.quantum_core_installed = true;
        snap.structures.insert(sid_pkg, s_pkg);
        snap.items.insert(950_201, InventoryItemRecord {
            item_id: 950_201,
            type_id: 34,
            owner_id: char_with_ship,
            location_id: sid_pkg,
            flag: InventoryFlag::Hangar,
            quantity: 250,
            singleton: false,
            ..Default::default()
        });
        let mut ext = load_ext_state(snap);
        let pkg_out = package_structure(&ctx, snap, &mut ext, sid_pkg, now);
        assert!(pkg_out.packaged_item_id.is_some());
        assert!(!pkg_out.asset_safety_wrap_ids.is_empty());
        assert!(!snap.structures.contains_key(&sid_pkg));
        save_ext_state(snap, &ext);
    });

    // 5. SovHub STATE_ONLINING completion + Pochven NoCore blocked + non-Pochven core install + Tenebrex spool-up
    ctx.db.with_mut_snapshot(|snap| {
        let sov_tid = crate::services::sov_services::TYPE_SOVEREIGNTY_HUB;
        let mut s_sov = make_structure(sid_sov, sov_tid, corp_id, sys_id, STATE_ONLINING, [30_000.0, 0.0, 0.0]);
        s_sov.reinforced_until_unix = now - 10;
        snap.structures.insert(sid_sov, s_sov);

        // Pochven structure remains NoCore even if core exists
        snap.structures.insert(
            sid_pochven,
            make_structure(sid_pochven, 35832, corp_id, pochven_sys, STATE_NO_CORE, [0.0, 0.0, 0.0]),
        );

        // Non-Pochven NoCore structure consumes core from StructureDeedBay (flag 172)
        snap.structures.insert(
            sid_nocore,
            make_structure(sid_nocore, 35832, corp_id, sys_id, STATE_NO_CORE, [40_000.0, 0.0, 0.0]),
        );
        if let Some(core_tid) = structure_profile(&ctx, 35832).and_then(|p| p.core_type_id) {
            snap.items.insert(950_301, InventoryItemRecord {
                item_id: 950_301,
                type_id: core_tid,
                owner_id: corp_id,
                location_id: sid_nocore,
                flag: InventoryFlag::StructureDeedBay,
                quantity: 1,
                singleton: false,
                ..Default::default()
            });
        }

        // Tenebrex Cyno Jammer spooling online
        let t_tid = crate::services::sov_gameplay::TYPE_TENEBREX_CYNO_JAMMER;
        let mut s_ten = make_structure(sid_tenebrex, t_tid, corp_id, sys_id, STATE_ANCHORING, [50_000.0, 0.0, 0.0]);
        s_ten.reinforced_until_unix = now - 1;
        snap.structures.insert(sid_tenebrex, s_ten);
    });
    let sync_results = sync_all_structures(&ctx);
    assert!(!sync_results.is_empty() || ctx.db.with_snapshot(|s| s.structures.get(&sid_pochven).map(|st| st.state.as_str() == STATE_NO_CORE).unwrap_or(false)));
    sync_asset_safety_wraps(&ctx);

    // 6. validate_home_station_structure, prepare_home_station_respawn, validate_structure_refit, hack reveal
    let mut sess_bad = SessionState::new(99);
    sess_bad.character_id = Some(char_with_ship);
    let destroyed_upwell_home = 1_030_000_000_601i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_with_ship) {
            ch.home_station_id = destroyed_upwell_home;
        }
        assert_eq!(validate_home_station_structure(&ctx, snap, char_with_ship, 999_999_999).unwrap_err().code, "StationNotFound");
        assert!(validate_home_station_structure(&ctx, snap, char_with_ship, sid_pochven).is_err());

        let mut ext = load_ext_state(snap);
        assert_eq!(validate_structure_refit(&ctx, snap, &ext, 999_999_999, now).unwrap_err().code, "StructureNotFound");
        assert_eq!(validate_structure_refit(&ctx, snap, &ext, sid_pochven, now).unwrap_err().code, "StructureFittingLockedInState");

        snap.structures.insert(
            1_030_000_699,
            make_structure(1_030_000_699, 35832, corp_id, sys_id, STATE_ARMOR_REINFORCED, [60_000.0, 0.0, 0.0]),
        );
        assert_eq!(validate_structure_refit(&ctx, snap, &ext, 1_030_000_699, now).unwrap_err().code, "StructureFittingLockedInState");

        snap.structures.insert(
            1_030_000_700,
            make_structure(1_030_000_700, 35832, corp_id, sys_id, STATE_SHIELD_VULNERABLE, [70_000.0, 0.0, 0.0]),
        );
        ext.lifecycle.entry(1_030_000_700).or_default().repair_until_unix = Some(now + 600);
        assert_eq!(validate_structure_refit(&ctx, snap, &ext, 1_030_000_700, now).unwrap_err().code, "StructureRepairingCannotRefit");
    });
    prepare_home_station_respawn(&ctx, char_with_ship);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_with_ship].home_station_id, 0);
    });

    // Empty fast-path on sync_all_structures when structures & industry_jobs are cleared
    let empty_ctx = ServerContext::with_sde_and_repository(
        ctx.sde.clone(),
        Arc::new(eve_db::WorldRepository::new_empty()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    assert!(sync_all_structures(&empty_ctx).is_empty());

    // Online service type ids & typed damage + hack reveal sanity
    let _ = online_service_type_ids(&ctx, &ctx.db.with_snapshot(|s| s.clone()), sid_nocore);
    let _ = resolve_char_school_or_nearest_station(&ctx, &ctx.db.with_snapshot(|s| s.clone()), char_with_ship, sys_id);
    let _ = record_structure_hack_reveal(&ctx, char_with_ship, 1_030_000_699);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let _ = apply_structure_damage_typed(&ctx, snap, &mut ext, sid_kspace, 100.0, 0.0, 0.0, 0.0, 10.0, attacker_char, now);
    });
}
