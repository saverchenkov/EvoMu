//! Deep branch-coverage and mutation-killing unit tests for Upwell structure lifecycle,
//! quantum core drops, asset safety wrap & delivery fees, POS/orbital timers & restrictions,
//! and Orbital Skyhook 1 Hz reagent maturity & silo theft/siphon mechanics.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::sov_gameplay::sov_gameplay_skyhook::{
    advance_skyhook_reagents_in_snap, apply_skyhook_weapon_damage, handle_skyhook_component_call,
    planet_reagent_type, tick_skyhooks, SKYHOOK_SILO_LINK_DURATION_SECS,
    SKYHOOK_SILO_LINK_MAX_SPEED_MS,
};
use crate::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SUPERIONIC_ICE,
};
use crate::services::station_structure;
use crate::services::station_structure::structure_lifecycle::*;
use crate::services::station_structure::structure_upwell_ops::{
    self, load_ext_state, save_ext_state, sync_auto_delivered_asset_safety_wraps,
    AssetSafetyWrapRecord, StructureLifecycleState, ASSET_SAFETY_AUTO_DELIVERY_SECS,
    ASSET_SAFETY_DELIVERY_DELAY_SECS,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    corp_roles, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    let sde = SHARED_SDE
        .get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone();
    ServerContext::with_sde_and_repository(sde, Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn insert_custom_type(
    sde: &mut Arc<SdeRegistry>,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    base_price: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&35_832)
        .cloned()
        .or_else(|| reg.types.values().next().cloned())
        .expect("seed type");
    rec.type_id = type_id;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("DeepStructType_{type_id}");
    rec.volume = 10_000.0;
    rec.radius = 2_000.0;
    rec.base_price = base_price;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(type_id, rec);
}

fn make_structure_rec(
    sid: i64,
    tid: u32,
    corp_id: i64,
    sys_id: u32,
    state: &str,
    core: bool,
    fuel: u32,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: sid,
        type_id: tid,
        owner_corp_id: corp_id,
        solar_system_id: sys_id,
        name: format!("DeepStructure_{sid}"),
        position: [250_000.0, 0.0, 0.0],
        state: state.to_string(),
        quantum_core_installed: core,
        fuel_blocks: fuel,
        shield_hp: 100_000.0,
        armor_hp: 200_000.0,
        hull_hp: 300_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

#[test]
fn reinforcement_matrices_jitter_boundaries_power_states_and_shield_multipliers() {
    let hr = 3_600u64;

    // Abandoned structures always skip armor and hull reinforcement across all classes and spaces
    for class in [HullClass::Medium, HullClass::Large, HullClass::XLarge, HullClass::Flex] {
        for space in [SpaceClass::HighSec, SpaceClass::LowNullSec, SpaceClass::Wormhole, SpaceClass::WarHq] {
            assert_eq!(armor_reinforcement_secs(class, space, PowerState::Abandoned), None);
            assert_eq!(hull_reinforcement_secs(class, space, PowerState::Abandoned), None);
        }
    }

    for (class, space, power, exp_armor, exp_hull) in [
        (HullClass::Medium, SpaceClass::HighSec, PowerState::FullPower, Some(108 * hr), None),
        (HullClass::Medium, SpaceClass::LowNullSec, PowerState::LowPower, Some(84 * hr), None),
        (HullClass::Medium, SpaceClass::Wormhole, PowerState::FullPower, Some(60 * hr), None),
        (HullClass::Medium, SpaceClass::WarHq, PowerState::FullPower, Some(24 * hr), None),
        (HullClass::Large, SpaceClass::HighSec, PowerState::FullPower, Some(24 * hr), Some(108 * hr)),
        (HullClass::Large, SpaceClass::LowNullSec, PowerState::LowPower, None, Some(60 * hr)),
        (HullClass::XLarge, SpaceClass::Wormhole, PowerState::FullPower, Some(24 * hr), Some(36 * hr)),
        (HullClass::XLarge, SpaceClass::WarHq, PowerState::FullPower, Some(24 * hr), Some(24 * hr)),
        (HullClass::Flex, SpaceClass::LowNullSec, PowerState::FullPower, None, Some(1_800)),
    ] {
        assert_eq!(armor_reinforcement_secs(class, space, power), exp_armor);
        assert_eq!(hull_reinforcement_secs(class, space, power), exp_hull);
    }

    // Jitter window half-width per HullClass and is_armor
    for (class, is_armor, exp_j) in [
        (HullClass::Medium, true, 5_400),
        (HullClass::Medium, false, 0),
        (HullClass::Large, true, 10_800),
        (HullClass::XLarge, false, 10_800),
        (HullClass::Flex, false, 1_800),
        (HullClass::Flex, true, 0),
    ] {
        assert_eq!(reinforce_jitter_window_secs(class, is_armor), exp_j);
    }

    // apply_reinforce_jitter clamps offset to [-max_j, max_j] and stays within same UTC day
    let day_start = 20_000 * 86_400;
    assert_eq!(apply_reinforce_jitter(day_start + 100, -50_000, 5_400), day_start);
    assert_eq!(apply_reinforce_jitter(day_start + 86_300, 50_000, 5_400), day_start + 86_399);
    assert_eq!(apply_reinforce_jitter(day_start + 43_200, 99_999, 5_400), day_start + 43_200 + 5_400);

    // next_reinforce_exit: same day vs next day when earliest_exit > today's exit_hour
    assert_eq!(next_reinforce_exit(day_start + 10 * 3_600, 18), day_start + 18 * 3_600);
    assert_eq!(next_reinforce_exit(day_start + 18 * 3_600 + 1, 18), day_start + 86_400 + 18 * 3_600);

    // Power state transitions & Faction Fortizar (47512..=47516) never abandoning
    let mut ctx = test_ctx();
    let (st_tid, svc_tid) = (945_001u32, 945_002u32);
    insert_custom_type(
        &mut ctx.sde,
        st_tid,
        1657,
        CATEGORY_STRUCTURE,
        100_000_000.0,
        &[
            (ATTR_RIG_SIZE, 2.0),
            (ATTR_SHIELD_CAPACITY, 100_000.0),
            (ATTR_ARMOR_HP, 200_000.0),
            (ATTR_HP, 300_000.0),
            (ATTR_STRUCTURE_FULL_POWER_HP_MULT, 2.5),
        ],
    );
    insert_custom_type(
        &mut ctx.sde,
        svc_tid,
        1321,
        66,
        5_000_000.0,
        &[
            (ATTR_SERVICE_FUEL_PER_HOUR, 5.0),
            (ATTR_SERVICE_FULL_POWER_HP_MULT, 1.5),
        ],
    );

    let prof = structure_profile(&ctx, st_tid).expect("profile");
    let now = 1_800_000_000u64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut s = make_structure_rec(88_001, st_tid, 98_001, 30_000_142, STATE_ONLINING, true, 100);
        snap.structures.insert(88_001, s.clone());
        assert_eq!(power_state(&ctx, snap, &ext, &s, now), PowerState::NotOperational);

        s.state = STATE_SHIELD_VULNERABLE.to_string();
        snap.structures.insert(88_001, s.clone());
        assert_eq!(power_state(&ctx, snap, &ext, &s, now), PowerState::LowPower);
        assert!((effective_max_shield(&ctx, snap, &prof, 88_001, PowerState::LowPower) - 100_000.0).abs() < 1e-6);

        snap.items.insert(
            880_101,
            InventoryItemRecord {
                item_id: 880_101,
                type_id: svc_tid,
                owner_id: 98_001,
                location_id: 88_001,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(880_101, true);
        assert_eq!(power_state(&ctx, snap, &ext, &s, now), PowerState::FullPower);
        assert!((effective_max_shield(&ctx, snap, &prof, 88_001, PowerState::FullPower) - 375_000.0).abs() < 1e-6);

        snap.module_online_states.insert(880_101, false);
        ext.lifecycle.insert(
            88_001,
            StructureLifecycleState {
                low_power_since_unix: Some(now - ABANDON_AFTER_LOW_POWER_SECS + 1),
                ..Default::default()
            },
        );
        assert_eq!(power_state(&ctx, snap, &ext, &s, now), PowerState::LowPower);
        ext.lifecycle.get_mut(&88_001).unwrap().low_power_since_unix = Some(now - ABANDON_AFTER_LOW_POWER_SECS);
        assert_eq!(power_state(&ctx, snap, &ext, &s, now), PowerState::Abandoned);
        s.type_id = 47_512;
        assert_eq!(power_state(&ctx, snap, &ext, &s, now), PowerState::LowPower);
    });
}

#[test]
fn upwell_sync_core_insertion_damage_pause_and_destruction_quantum_core_drop() {
    let mut ctx = test_ctx();
    let (st_tid, core_tid, mod1_tid, mod2_tid, rig_tid) = (945_010u32, 56_201u32, 945_011u32, 945_012u32, 945_013u32);
    insert_custom_type(
        &mut ctx.sde,
        st_tid,
        1657,
        CATEGORY_STRUCTURE,
        200_000_000.0,
        &[
            (ATTR_RIG_SIZE, 3.0),
            (ATTR_SHIELD_CAPACITY, 10_000.0),
            (ATTR_ARMOR_HP, 10_000.0),
            (ATTR_HP, 10_000.0),
            (ATTR_ARMOR_DAMAGE_LIMIT, 5_000.0),
            (ATTR_PAUSE_SHIELD_REPAIR_DPS, 100.0),
            (ATTR_PAUSE_ARMOR_REPAIR_DPS, 100.0),
            (ATTR_PAUSE_HULL_REPAIR_DPS, 100.0),
            (ATTR_REQUIRES_DEED_TYPE, core_tid as f64),
        ],
    );
    insert_custom_type(&mut ctx.sde, core_tid, 4093, 66, 80_000_000.0, &[]);
    insert_custom_type(&mut ctx.sde, mod1_tid, 1321, 66, 5_000_000.0, &[(ATTR_SERVICE_FUEL_PER_HOUR, 5.0)]);
    insert_custom_type(&mut ctx.sde, mod2_tid, 1321, 66, 5_000_000.0, &[(ATTR_SERVICE_FUEL_PER_HOUR, 5.0)]);
    insert_custom_type(&mut ctx.sde, rig_tid, 1500, 66, 5_000_000.0, &[]);

    let (sid, corp_id, char_id, now) = (88_100i64, 98_100i64, 90_100i64, 1_800_000_000u64);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.home_station_id = 60_003_760;
        snap.characters.insert(char_id, ch);

        let mut s = make_structure_rec(sid, st_tid, corp_id, 30_000_142, STATE_ANCHOR_VULNERABLE, false, 50_000);
        s.reinforced_until_unix = now - 1;
        snap.structures.insert(sid, s);
        snap.items.insert(
            881_001,
            InventoryItemRecord {
                item_id: 881_001,
                type_id: core_tid,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::StructureDeedBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        sync_structure(&ctx, snap, &mut ext, sid, now);
        assert_eq!(snap.structures[&sid].state, STATE_ANCHORING);

        snap.structures.get_mut(&sid).unwrap().reinforced_until_unix = now;
        sync_structure(&ctx, snap, &mut ext, sid, now);
        assert_eq!(snap.structures[&sid].state, STATE_ONLINING);
        assert!(snap.structures[&sid].quantum_core_installed);
        assert!(!snap.items.contains_key(&881_001));

        snap.structures.get_mut(&sid).unwrap().reinforced_until_unix = now;
        sync_structure(&ctx, snap, &mut ext, sid, now);
        assert_eq!(snap.structures[&sid].state, STATE_SHIELD_VULNERABLE);

        for (iid, tid, owner, flag) in [
            (881_010, mod1_tid, corp_id, InventoryFlag::ServiceSlot0),
            (881_011, mod2_tid, corp_id, InventoryFlag::ServiceSlot1),
            (881_012, rig_tid, corp_id, InventoryFlag::RigSlot0),
            (881_013, 34, char_id, InventoryFlag::Hangar),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: sid,
                    flag,
                    quantity: 100,
                    singleton: flag != InventoryFlag::Hangar,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(881_010, true);

        let d_shield = apply_structure_damage(&ctx, snap, &mut ext, sid, 50_000.0, 1.0, char_id, now).unwrap();
        assert_eq!(d_shield.state, STATE_ARMOR_REINFORCED);

        let t_armor = snap.structures[&sid].reinforced_until_unix;
        sync_structure(&ctx, snap, &mut ext, sid, t_armor);
        assert_eq!(snap.structures[&sid].state, STATE_ARMOR_VULNERABLE);
        assert_eq!(ext.lifecycle[&sid].repair_until_unix, Some(t_armor + 900));

        // Sub-pause DPS (< 100 DPS) does NOT pause repair_until_unix; >= 100 DPS pauses by cycle_whole (+10s)
        let _ = apply_structure_damage(&ctx, snap, &mut ext, sid, 500.0, 10.0, char_id, t_armor + 50);
        assert_eq!(ext.lifecycle[&sid].repair_until_unix, Some(t_armor + 900));
        let _ = apply_structure_damage(&ctx, snap, &mut ext, sid, 1_000.0, 10.0, char_id, t_armor + 50);
        assert_eq!(ext.lifecycle[&sid].repair_until_unix, Some(t_armor + 910));

        let d_armor = apply_structure_damage(&ctx, snap, &mut ext, sid, 50_000.0, 2.0, char_id, t_armor + 60).unwrap();
        assert_eq!(d_armor.state, STATE_HULL_REINFORCED);
        let t_hull = snap.structures[&sid].reinforced_until_unix;
        sync_structure(&ctx, snap, &mut ext, sid, t_hull);
        assert_eq!(snap.structures[&sid].state, STATE_HULL_VULNERABLE);

        let d_hull = apply_structure_damage(&ctx, snap, &mut ext, sid, 50_000.0, 1.0, char_id, t_hull + 10).unwrap();
        let disp = d_hull.destruction.expect("structure destroyed");

        let core_id = disp.dropped_core_item_id.expect("quantum core dropped");
        assert_eq!(snap.items[&core_id].type_id, core_tid);
        assert_eq!(Some(snap.items[&core_id].location_id), disp.wreck_item_id);
        assert!(disp.dropped_item_ids.contains(&881_010) ^ disp.dropped_item_ids.contains(&881_011));
        assert!(disp.destroyed_item_ids.contains(&881_012));
        assert_eq!(disp.asset_safety_wrap_ids.len(), 2);
    });
}

#[tokio::test]
async fn asset_safety_wrap_auto_delivery_and_05_pct_vs_15_pct_fee_math() {
    let mut ctx = test_ctx();
    let now = 1_800_000_000u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));
    insert_custom_type(&mut ctx.sde, 945_020, 1657, 65, 1_000_000.0, &[]);

    let (char_id, corp_id, sys_id) = (90_201i64, 98_201i64, 30_000_142u32);
    let (sid_src, sid_dst, npc_station) = (88_201i64, 88_202i64, 60_003_760i64);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.balance = IskCents::from_isk(100_000);
        snap.characters.insert(char_id, ch);

        for sid in [sid_src, sid_dst] {
            snap.structures.insert(
                sid,
                make_structure_rec(sid, 35_832, corp_id, sys_id, STATE_SHIELD_VULNERABLE, true, 100),
            );
        }

        let mut ext = load_ext_state(snap);
        for (wid, item_id, created_at) in [
            (770_001i64, 771_001i64, now - ASSET_SAFETY_DELIVERY_DELAY_SECS - 10),
            (770_002i64, 771_002i64, now - ASSET_SAFETY_AUTO_DELIVERY_SECS - 10),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 945_020,
                    owner_id: char_id,
                    location_id: wid,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            ext.asset_safety_wraps.insert(
                wid,
                AssetSafetyWrapRecord {
                    wrap_id: wid,
                    owner_id: char_id,
                    is_corp: false,
                    source_structure_id: sid_src,
                    source_structure_name: "SrcStruct".into(),
                    solar_system_id: sys_id,
                    created_unix: created_at,
                    deliverable_after_unix: created_at + ASSET_SAFETY_DELIVERY_DELAY_SECS,
                    delivered_to_id: None,
                    item_ids: vec![item_id],
                    estimated_value_isk: 1_000_000.0,
                    nearest_npc_station_id: Some(npc_station),
                    auto_delivered: false,
                    retrieval_fee_paid: false,
                },
            );
        }
        save_ext_state(snap, &ext);
    });

    sync_auto_delivered_asset_safety_wraps(&ctx);
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert!(!ext.asset_safety_wraps[&770_001].auto_delivered);
        assert!(ext.asset_safety_wraps[&770_002].auto_delivered);
        assert_eq!(ext.asset_safety_wraps[&770_002].delivered_to_id, Some(npc_station));
    });

    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    let kw = HashMap::new();

    // 1) Same-system manual delivery of wrap 770_001 to sid_dst charges 0.5% = 5,000 ISK
    let rep1 = structure_upwell_ops::handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "MoveSafetyWrapToStructure",
        &[PyRep::Integer(770_001), PyRep::Integer(sid_dst)],
        &kw,
    )
    .await
    .unwrap()
    .expect("0.5% same-system manual delivery");
    assert_eq!(rep1.dict_get("feePaidISK"), Some(&PyRep::Real(5_000.0)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(95_000));
        assert_eq!(snap.items[&771_001].location_id, sid_dst);
    });

    // 2) Auto-delivered wrap 770_002 requires 15% = 150,000 ISK; pilot has 95,000 ISK -> InsufficientFunds
    assert_eq!(
        structure_upwell_ops::handle(
            &ctx,
            &mut sess,
            "structureAssetSafety",
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(770_002), PyRep::Integer(npc_station)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(200_000);
    });
    let rep2 = structure_upwell_ops::handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "MoveSafetyWrapToStructure",
        &[PyRep::Integer(770_002), PyRep::Integer(npc_station)],
        &kw,
    )
    .await
    .unwrap()
    .expect("15% auto-delivered recovery");
    assert_eq!(rep2.dict_get("feePaidISK"), Some(&PyRep::Real(150_000.0)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(50_000));
    });
}

#[tokio::test]
async fn pos_mgr_orbital_anchoring_gantry_upgrade_and_skyhook_reagent_siphon_math() {
    let ctx = test_ctx();
    let now = 1_800_000_000u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));

    assert_eq!(planet_reagent_type(12), Some(TYPE_SUPERIONIC_ICE));
    assert_eq!(planet_reagent_type(2014), Some(TYPE_SUPERIONIC_ICE));
    assert_eq!(planet_reagent_type(2015), Some(TYPE_MAGMATIC_GAS));
    assert_eq!(planet_reagent_type(11), None);

    let (null_sys, planet_id, planet_pos) = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0 && !s.planet_ids.is_empty())
        .and_then(|s| {
            let pid = s.planet_ids[0];
            let ppos = ctx.sde.celestials_in_system(s.solar_system_id).into_iter().find(|c| c.celestial_id == pid).map(|c| c.position).unwrap_or(DVec3::ZERO);
            Some((s.solar_system_id, pid, ppos))
        })
        .expect("nullsec planet");

    let (char_id, thief_id, corp_id, ally_id) = (90_301i64, 90_302i64, 98_300_001i64, 99_300_001i64);
    let (gantry_id, skyhook_id, ship_id, cruiser_id) = (88_301i64, 88_302i64, 88_310i64, 88_311i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(ally_id, AllianceRecord {
            alliance_id: ally_id, name: "Skyhook Ally".into(), short_name: "SKYA".into(),
            executor_corp_id: corp_id, member_corp_ids: HashSet::from([corp_id]), ..Default::default()
        });
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: "Skyhook Corp".into(), ticker: "SKYC".into(),
            ceo_id: char_id, alliance_id: Some(ally_id), members: HashSet::from([char_id]), ..Default::default()
        });
        let base_ch = snap.characters.values().next().cloned().expect("demo char");
        for (cid, c_corp, s_id) in [(char_id, corp_id, ship_id), (thief_id, 98_999, cruiser_id)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid; ch.corporation_id = c_corp; ch.solar_system_id = null_sys; ch.active_ship_id = s_id;
            snap.characters.insert(cid, ch);
        }
        for (iid, tid, owner, loc, flag) in [
            (gantry_id, 3_962u32, char_id, ship_id, InventoryFlag::Cargo),
            (88_399, 2_233u32, char_id, ship_id, InventoryFlag::Cargo),
            (cruiser_id, 621u32, thief_id, null_sys as i64, InventoryFlag::Hangar),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: owner, location_id: loc, flag,
                quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
    });

    let sys_h = ctx.systems.get_or_spawn(null_sys);
    for (id, tid) in [(ship_id, 587u32), (cruiser_id, 621u32)] {
        let _ = sys_h.spawn_ball(Ball {
            id, type_id: tid, position: planet_pos + DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 2_500.0, inertia: 1.0, mass: 10_000_000.0, radius: 80.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
    }

    let mut dir_sess = SessionState::new(1);
    dir_sess.user_id = Some(char_id as u64);
    dir_sess.character_id = Some(char_id);
    dir_sess.corporation_id = Some(corp_id);
    dir_sess.solar_system_id = Some(null_sys);
    dir_sess.ship_id = Some(ship_id);
    dir_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    let kw = HashMap::new();

    let anchor_rep = station_structure::handle(&ctx, &mut dir_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet_id)], &kw)
        .await.unwrap().expect("AnchorOrbital");
    assert_eq!(anchor_rep.dict_get("planetID"), Some(&PyRep::Integer(planet_id)));

    let upg_rep = crate::services::station_structure_handlers::handle_upgrade_gantry(&ctx, &dir_sess, &[PyRep::Integer(gantry_id)])
        .expect("handle_upgrade_gantry");
    assert_eq!(upg_rep.dict_get("typeID"), Some(&PyRep::Integer(2_233)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(skyhook_id, UpwellStructureRecord {
            structure_id: skyhook_id, type_id: TYPE_ORBITAL_SKYHOOK, owner_corp_id: corp_id, solar_system_id: null_sys,
            name: "Deep Skyhook".into(), position: (planet_pos + DVec3::new(2_000.0, 0.0, 0.0)).to_array(),
            state: STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: 10_000.0, armor_hp: 10_000.0, hull_hp: 10_000.0, ..Default::default()
        });
        snap.sov_runtime.skyhook_reagent_bays.insert(skyhook_id, (TYPE_MAGMATIC_GAS, 100, 400, now));
        snap.sov_runtime.skyhook_theft_windows.insert(skyhook_id, (now, now + 7_200));

        for step in 1..=3_600 {
            advance_skyhook_reagents_in_snap(snap, &ctx.sde, skyhook_id, now + step);
        }
        let (_, sec_qty, insec_qty, _) = snap.sov_runtime.skyhook_reagent_bays[&skyhook_id];
        assert!(sec_qty > 125);
        assert_eq!(sec_qty + insec_qty, 550);
    });

    let dmg_rep = apply_skyhook_weapon_damage(&ctx, skyhook_id, 11_875.0).expect("damage inside window");
    assert_eq!(dmg_rep.dict_get("state"), Some(&PyRep::String("ShieldVulnerable".into())));
    assert_eq!(dmg_rep.dict_get("hackable"), Some(&PyRep::Bool(true)));

    let mut thief_sess = SessionState::new(2);
    thief_sess.user_id = Some(thief_id as u64);
    thief_sess.character_id = Some(thief_id);
    thief_sess.corporation_id = Some(98_999);
    thief_sess.solar_system_id = Some(null_sys);
    thief_sess.ship_id = Some(cruiser_id);

    let link_rep = handle_skyhook_component_call(&ctx, &thief_sess, null_sys, thief_id, cruiser_id, skyhook_id, "linkWithShip", "InitiateLink", &[])
        .await.expect("InitiateLink ok");
    assert_eq!(link_rep.dict_get("linkDurationSeconds"), Some(&PyRep::Integer(SKYHOOK_SILO_LINK_DURATION_SECS as i64)));
    let (_, cruiser_ball) = sys_h.get_ball_snapshot(cruiser_id).await.unwrap().unwrap();
    assert!((cruiser_ball.max_velocity - SKYHOOK_SILO_LINK_MAX_SPEED_MS).abs() < 1e-6);

    let secure_before = ctx.db.with_snapshot(|snap| snap.sov_runtime.skyhook_reagent_bays[&skyhook_id].1);
    tick_skyhooks(&ctx, now + SKYHOOK_SILO_LINK_DURATION_SECS + 10).await;
    ctx.db.with_snapshot(|snap| {
        let (_, sec_after, insec_after, _) = snap.sov_runtime.skyhook_reagent_bays[&skyhook_id];
        assert_eq!(insec_after, 0);
        assert!(sec_after >= secure_before);
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&skyhook_id));
    });
}
