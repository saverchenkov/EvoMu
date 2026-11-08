//! Additional unit and mutation-killing tests for `structure_lifecycle.rs`.

#![forbid(unsafe_code)]

use super::station_structure::structure_lifecycle::*;
use super::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureLifecycleState,
};
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord,
    UpwellStructureRecord,
};
use eve_sde::{InvType, SdeRegistry};
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

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg
        .types
        .get(&base_tid)
        .cloned()
        .or_else(|| reg.types.values().next().cloned())
        .expect("base sde type");
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("ExtraLifeType_{new_tid}");
    rec.volume = 10_000.0;
    rec.radius = 2_500.0;
    rec.base_price = 100_000_000.0;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_record(
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
        name: format!("ExtraStruct {sid}"),
        position: [1_000_000_000.0, 2_000_000.0, -500_000.0],
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
fn power_state_strings_hull_profiles_and_service_families_cover_all_variants() {
    assert_eq!(PowerState::NotOperational.as_str(), "NotOperational");
    assert_eq!(PowerState::FullPower.as_str(), "FullPower");
    assert_eq!(PowerState::LowPower.as_str(), "LowPower");
    assert_eq!(PowerState::Abandoned.as_str(), "Abandoned");

    assert_eq!(structure_family(1657), ServiceFamily::Citadel);
    assert_eq!(structure_family(1404), ServiceFamily::Engineering);
    assert_eq!(structure_family(1876), ServiceFamily::Engineering);
    assert_eq!(structure_family(1406), ServiceFamily::Refinery);
    assert_eq!(structure_family(9999), ServiceFamily::Other);

    let mut ctx = test_ctx();
    let lg_tid = 930_001u32;
    let flex_tid = 930_002u32;
    let g4744_tid = 930_003u32;
    let non_struct_tid = 930_004u32;
    let mod_cit_tid = 930_005u32;
    let mod_eng_tid = 930_006u32;
    let mod_ref_tid = 930_007u32;
    let mod_oth_tid = 930_008u32;

    clone_type_with(
        &mut ctx.sde,
        35833,
        lg_tid,
        1404,
        CATEGORY_STRUCTURE,
        &[
            (ATTR_RIG_SIZE, 3.0),
            (ATTR_DISALLOW_IN_HIGHSEC, 1.0),
            (ATTR_SERVICE_ROLE_BONUS, -20.0),
            (ATTR_STRUCTURE_ROLE_BONUS, -10.0),
            (ATTR_ENG_MAT_BONUS, 0.99),
            (ATTR_ENG_COST_BONUS, 0.96),
            (ATTR_ENG_TIME_BONUS, 0.80),
            (ATTR_REACTION_TIME_MULT, 0.75),
            (ATTR_REFINING_YIELD_BONUS, 0.04),
            (ATTR_TETHERING_RANGE, 15_000.0),
        ],
    );
    clone_type_with(&mut ctx.sde, 35832, flex_tid, 1408, CATEGORY_STRUCTURE, &[]);
    clone_type_with(&mut ctx.sde, 35832, g4744_tid, 4744, CATEGORY_STRUCTURE, &[]);
    clone_type_with(&mut ctx.sde, 35832, non_struct_tid, 1657, 6, &[]);
    clone_type_with(
        &mut ctx.sde,
        35832,
        mod_cit_tid,
        1321,
        66,
        &[(ATTR_SERVICE_FUEL_GROUP, 1136.0)],
    );
    clone_type_with(&mut ctx.sde, 35832, mod_eng_tid, 1415, 66, &[]);
    clone_type_with(&mut ctx.sde, 35832, mod_ref_tid, 1887, 66, &[]);
    clone_type_with(&mut ctx.sde, 35832, mod_oth_tid, 9999, 66, &[]);

    let lg = structure_profile(&ctx, lg_tid).expect("large profile");
    assert_eq!(lg.hull_class, HullClass::Large);
    assert!(lg.disallow_in_highsec);
    assert!((lg.fuel_bonus_pct - (-20.0)).abs() < 1e-6);
    assert!((lg.eng_mat_bonus_pct - (-1.0)).abs() < 1e-6);
    assert!((lg.eng_cost_bonus_pct - (-4.0)).abs() < 1e-6);
    assert!((lg.eng_time_bonus_pct - (-20.0)).abs() < 1e-6);
    assert!((lg.reaction_time_mult - 0.75).abs() < 1e-6);
    assert!((lg.refining_yield_bonus - 0.04).abs() < 1e-6);

    let flex = structure_profile(&ctx, flex_tid).expect("flex profile");
    assert_eq!(flex.hull_class, HullClass::Flex);
    assert!(!flex.disallow_in_highsec);

    let g4744 = structure_profile(&ctx, g4744_tid).expect("group 4744 profile");
    assert_eq!(g4744.hull_class, HullClass::Flex);
    assert!(structure_profile(&ctx, non_struct_tid).is_none());

    assert!((tether_range_m(&ctx, lg_tid) - 17_500.0).abs() < 1e-6);
    assert!((tether_range_m(&ctx, flex_tid) - 12_500.0).abs() < 1e-6);

    assert!(service_fuel_groups(&ctx).contains(&1136));
    assert_eq!(service_family_for_module(&ctx, mod_cit_tid), ServiceFamily::Citadel);
    assert_eq!(service_family_for_module(&ctx, mod_eng_tid), ServiceFamily::Engineering);
    assert_eq!(service_family_for_module(&ctx, mod_ref_tid), ServiceFamily::Refinery);
    assert_eq!(service_family_for_module(&ctx, mod_oth_tid), ServiceFamily::Other);

    assert_eq!(space_class(&ctx, 31_000_005), SpaceClass::Wormhole);
    assert_eq!(space_class(&ctx, 30000142), SpaceClass::HighSec);
    assert_eq!(space_class(&ctx, 30_002_537), SpaceClass::LowNullSec);
}

#[test]
fn reinforcement_tables_and_jitter_clamping_cover_all_hull_space_and_power_combinations() {
    assert_eq!(reinforce_jitter_window_secs(HullClass::Medium, true), 5_400);
    assert_eq!(reinforce_jitter_window_secs(HullClass::Medium, false), 0);
    assert_eq!(reinforce_jitter_window_secs(HullClass::Large, true), 10_800);
    assert_eq!(reinforce_jitter_window_secs(HullClass::Large, false), 10_800);
    assert_eq!(reinforce_jitter_window_secs(HullClass::XLarge, true), 10_800);
    assert_eq!(reinforce_jitter_window_secs(HullClass::XLarge, false), 10_800);
    assert_eq!(reinforce_jitter_window_secs(HullClass::Flex, false), 1_800);
    assert_eq!(reinforce_jitter_window_secs(HullClass::Flex, true), 0);

    let day_start = 20_000 * 86_400;
    assert_eq!(apply_reinforce_jitter(day_start + 3_600, 0, 5_400), day_start + 3_600);
    assert_eq!(apply_reinforce_jitter(day_start + 3_600, 1_200, 0), day_start + 3_600);
    // Clamped to same UTC day start and end
    assert_eq!(apply_reinforce_jitter(day_start + 600, -5_400, 5_400), day_start);
    assert_eq!(apply_reinforce_jitter(day_start + 86_000, 5_400, 5_400), day_start + 86_399);
    // Candidate before earliest advances by +86_400
    assert_eq!(next_reinforce_exit(day_start + 10 * 3_600, 8), day_start + 86_400 + 8 * 3_600);
    assert_eq!(next_reinforce_exit(day_start + 6 * 3_600, 8), day_start + 8 * 3_600);

    // Abandoned skips all reinforcement
    for class in [HullClass::Medium, HullClass::Large, HullClass::XLarge, HullClass::Flex] {
        assert_eq!(armor_reinforcement_secs(class, SpaceClass::HighSec, PowerState::Abandoned), None);
        assert_eq!(hull_reinforcement_secs(class, SpaceClass::HighSec, PowerState::Abandoned), None);
    }

    // Flex skips armor, has 30m hull reinforcement
    assert_eq!(armor_reinforcement_secs(HullClass::Flex, SpaceClass::LowNullSec, PowerState::FullPower), None);
    assert_eq!(hull_reinforcement_secs(HullClass::Flex, SpaceClass::LowNullSec, PowerState::FullPower), Some(1_800));

    // Medium armor reinforcement by SpaceClass; no hull reinforcement
    assert_eq!(armor_reinforcement_secs(HullClass::Medium, SpaceClass::WarHq, PowerState::FullPower), Some(24 * 3_600));
    assert_eq!(armor_reinforcement_secs(HullClass::Medium, SpaceClass::Wormhole, PowerState::FullPower), Some(60 * 3_600));
    assert_eq!(armor_reinforcement_secs(HullClass::Medium, SpaceClass::LowNullSec, PowerState::FullPower), Some(84 * 3_600));
    assert_eq!(armor_reinforcement_secs(HullClass::Medium, SpaceClass::HighSec, PowerState::FullPower), Some(108 * 3_600));
    assert_eq!(hull_reinforcement_secs(HullClass::Medium, SpaceClass::HighSec, PowerState::FullPower), None);

    // Large/XLarge armor reinforcement requires FullPower; hull reinforcement scales with SpaceClass
    assert_eq!(armor_reinforcement_secs(HullClass::Large, SpaceClass::HighSec, PowerState::LowPower), None);
    assert_eq!(armor_reinforcement_secs(HullClass::Large, SpaceClass::HighSec, PowerState::FullPower), Some(24 * 3_600));
    assert_eq!(hull_reinforcement_secs(HullClass::Large, SpaceClass::WarHq, PowerState::LowPower), Some(24 * 3_600));
    assert_eq!(hull_reinforcement_secs(HullClass::Large, SpaceClass::Wormhole, PowerState::LowPower), Some(36 * 3_600));
    assert_eq!(hull_reinforcement_secs(HullClass::Large, SpaceClass::LowNullSec, PowerState::LowPower), Some(60 * 3_600));
    assert_eq!(hull_reinforcement_secs(HullClass::Large, SpaceClass::HighSec, PowerState::LowPower), Some(108 * 3_600));
}

#[test]
fn validate_refit_and_home_station_structure_enforce_all_guards() {
    let mut ctx = test_ctx();
    let med_tid = 930_010u32;
    clone_type_with(
        &mut ctx.sde,
        35832,
        med_tid,
        1657,
        CATEGORY_STRUCTURE,
        &[
            (ATTR_RIG_SIZE, 2.0),
            (ATTR_SHIELD_CAPACITY, 100_000.0),
            (ATTR_ARMOR_HP, 200_000.0),
            (ATTR_HP, 300_000.0),
        ],
    );
    let sid = 1_000_000_930_001i64;
    let corp_id = 98901i64;
    let owner_char = 8901i64;
    let outsider_char = 8902i64;
    let now = 1_700_000_000u64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = owner_char;
        ch.corporation_id = corp_id;
        ch.home_station_id = sid;
        snap.characters.insert(owner_char, ch.clone());
        ch.character_id = outsider_char;
        ch.corporation_id = 999_999;
        snap.characters.insert(outsider_char, ch);
        snap.structures.insert(
            sid,
            make_record(sid, med_tid, corp_id, 30000142, STATE_ARMOR_REINFORCED, true, 10),
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        assert_eq!(
            validate_structure_refit(&ctx, snap, &ext, 999_999, now).unwrap_err().code,
            "StructureNotFound"
        );
        assert_eq!(
            validate_structure_refit(&ctx, snap, &ext, sid, now).unwrap_err().code,
            "StructureFittingLockedInState"
        );
        snap.structures.get_mut(&sid).unwrap().state = STATE_SHIELD_VULNERABLE.into();
        ext.lifecycle.insert(
            sid,
            StructureLifecycleState {
                repair_until_unix: Some(now + 300),
                ..Default::default()
            },
        );
        assert_eq!(
            validate_structure_refit(&ctx, snap, &ext, sid, now).unwrap_err().code,
            "StructureRepairingCannotRefit"
        );
        ext.lifecycle.get_mut(&sid).unwrap().repair_until_unix = None;
        snap.structures.get_mut(&sid).unwrap().hull_hp = 250_000.0;
        assert_eq!(
            validate_structure_refit(&ctx, snap, &ext, sid, now).unwrap_err().code,
            "StructureDamagedCannotRefit"
        );
        snap.structures.get_mut(&sid).unwrap().hull_hp = 300_000.0;
        assert!(validate_structure_refit(&ctx, snap, &ext, sid, now).is_ok());

        // Home station checks: missing,CloneBayServiceOffline, Pochven
        assert_eq!(
            validate_home_station_structure(&ctx, snap, owner_char, 999_999).unwrap_err().code,
            "StationNotFound"
        );
        assert_eq!(
            validate_home_station_structure(&ctx, snap, owner_char, sid).unwrap_err().code,
            "CloneBayServiceOffline"
        );
        if let Some((&pochven_sys, _)) = ctx
            .sde
            .solar_systems
            .iter()
            .find(|(_, s)| s.region_id == POCHVEN_REGION_ID)
        {
            snap.structures.get_mut(&sid).unwrap().solar_system_id = pochven_sys;
            assert_eq!(
                validate_home_station_structure(&ctx, snap, owner_char, sid).unwrap_err().code,
                "CannotSetMedicalCloneInPochven"
            );
            snap.structures.get_mut(&sid).unwrap().solar_system_id = 30000142;
        }
    });

    // prepare_home_station_respawn clears invalid Upwell home_station_id
    prepare_home_station_respawn(&ctx, owner_char);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&owner_char].home_station_id, 0);
    });
}

#[test]
fn sync_structure_core_consumption_self_destruct_unanchor_and_fuel_exhaustion() {
    let mut ctx = test_ctx();
    let core_tid = 930_020u32;
    let st_tid = 930_021u32;
    let svc_tid = 930_022u32;
    clone_type_with(&mut ctx.sde, 35832, core_tid, 4086, 17, &[]);
    clone_type_with(
        &mut ctx.sde,
        35832,
        st_tid,
        1657,
        CATEGORY_STRUCTURE,
        &[
            (ATTR_RIG_SIZE, 2.0),
            (ATTR_SHIELD_CAPACITY, 100_000.0),
            (ATTR_ARMOR_HP, 200_000.0),
            (ATTR_HP, 300_000.0),
            (ATTR_REQUIRES_DEED_TYPE, f64::from(core_tid)),
        ],
    );
    clone_type_with(
        &mut ctx.sde,
        35832,
        svc_tid,
        1321,
        66,
        &[(ATTR_SERVICE_FUEL_PER_HOUR, 25.0), (ATTR_SERVICE_FUEL_ONLINE, 50.0)],
    );

    let now = 1_700_000_000u64;
    let corp_id = 98902i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        // 1) STATE_NO_CORE consumes core from StructureDeedBay and transitions through STATE_ONLINING
        snap.structures.insert(
            9301,
            make_record(9301, st_tid, corp_id, 30000142, STATE_NO_CORE, false, 0),
        );
        snap.items.insert(
            93_101,
            InventoryItemRecord {
                item_id: 93_101,
                type_id: core_tid,
                owner_id: corp_id,
                location_id: 9301,
                flag: InventoryFlag::StructureDeedBay,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        assert!(sync_structure(&ctx, snap, &mut ext, 9301, now).is_none());
        assert_eq!(snap.structures[&9301].state, STATE_ONLINING);
        assert!(snap.structures[&9301].quantum_core_installed);
        assert_eq!(snap.items[&93_101].quantity, 1);

        // 2) Self-destruct elapsed destroys structure
        let mut sd = make_record(9302, st_tid, corp_id, 30000142, STATE_SHIELD_VULNERABLE, true, 0);
        sd.self_destruct_at_unix = Some(now - 5);
        snap.structures.insert(9302, sd);
        let out_sd = sync_structure(&ctx, snap, &mut ext, 9302, now).expect("self destruct outcome");
        assert!(out_sd.killmail_id.is_some());
        assert_eq!(snap.structures[&9302].state, STATE_DESTROYED);

        // 3) Unanchoring timer elapsed packages structure
        let un = make_record(9303, st_tid, corp_id, 30000142, STATE_SHIELD_VULNERABLE, true, 0);
        snap.structures.insert(9303, un);
        ext.unanchoring_until_unix.insert(9303, now - 1);
        let out_un = sync_structure(&ctx, snap, &mut ext, 9303, now).expect("unanchor outcome");
        assert!(out_un.packaged_item_id.is_some());
        assert!(!snap.structures.contains_key(&9303));

        // 4) Hourly fuel burn with empty fuel bay offlines service module
        let fb = make_record(9304, st_tid, corp_id, 30000142, STATE_SHIELD_VULNERABLE, true, 0);
        snap.structures.insert(9304, fb);
        snap.items.insert(
            93_104,
            InventoryItemRecord {
                item_id: 93_104,
                type_id: svc_tid,
                owner_id: corp_id,
                location_id: 9304,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(93_104, true);
        ext.lifecycle.insert(
            9304,
            StructureLifecycleState {
                online_modules: vec![93_104],
                last_fuel_tick_unix: Some(now - 3_600),
                ..Default::default()
            },
        );
        assert!(sync_structure(&ctx, snap, &mut ext, 9304, now).is_none());
        assert!(!snap.module_online_states[&93_104]);
        assert!(online_service_type_ids(&ctx, snap, 9304).is_empty());
    });
}

#[test]
fn abandoned_destruction_drops_hangar_items_relocates_clones_and_hack_reveal_respects_corp() {
    let mut ctx = test_ctx();
    let st_tid = 930_030u32;
    clone_type_with(
        &mut ctx.sde,
        35832,
        st_tid,
        1657,
        CATEGORY_STRUCTURE,
        &[
            (ATTR_RIG_SIZE, 2.0),
            (ATTR_SHIELD_CAPACITY, 50_000.0),
            (ATTR_ARMOR_HP, 50_000.0),
            (ATTR_HP, 50_000.0),
            (ATTR_PAUSE_HULL_REPAIR_DPS, 100.0),
        ],
    );
    let sid = 9401i64;
    let corp_id = 98903i64;
    let owner_char = 8931i64;
    let hacker_char = 8932i64;
    let now = 1_700_000_000u64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = owner_char;
        ch.corporation_id = corp_id;
        ch.home_station_id = 60003760;
        snap.characters.insert(owner_char, ch.clone());
        ch.character_id = hacker_char;
        ch.corporation_id = 999_888;
        snap.characters.insert(hacker_char, ch);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "AbandonCorp".into(),
                ticker: "ABC".into(),
                ceo_id: owner_char,
                hq_station_id: Some(60003760),
                tax_rate: 0.0,
                master_wallet: IskCents(0),
                ..Default::default()
            },
        );
        snap.structures.insert(
            sid,
            make_record(sid, st_tid, corp_id, 30000142, STATE_SHIELD_VULNERABLE, true, 0),
        );
        // Rig slot item (destroyed) + Hangar item (dropped in abandoned state)
        snap.items.insert(
            94_101,
            InventoryItemRecord {
                item_id: 94_101,
                type_id: 2454,
                owner_id: owner_char,
                location_id: sid,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            94_102,
            InventoryItemRecord {
                item_id: 94_102,
                type_id: 2454,
                owner_id: owner_char,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.jump_clones.insert(
            owner_char,
            vec![JumpCloneRecord {
                jump_clone_id: 1,
                character_id: owner_char,
                location_id: sid,
                implants: vec![9899],
            }],
        );
        let mut ext = load_ext_state(snap);
        ext.pending_reinforce_schedule.insert(sid, (20, 3, now + 86_400));
        ext.lifecycle.insert(
            sid,
            StructureLifecycleState {
                low_power_since_unix: Some(now - ABANDON_AFTER_LOW_POWER_SECS - 100),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });

    // Owning corp member cannot hack-reveal own structure; outsider reveals pending schedule
    assert!(record_structure_hack_reveal(&ctx, owner_char, sid).is_none());
    let rev = record_structure_hack_reveal(&ctx, hacker_char, sid).expect("outsider reveal");
    assert_eq!(rev.2, Some((20, 3, now + 86_400)));

    // Abandoned structure skips armor & hull reinforcement and drops hangar items into space on destruction
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let d1 = apply_structure_damage(&ctx, snap, &mut ext, sid, 350_000.0, 10.0, hacker_char, now)
            .expect("abandoned shield");
        assert_eq!(d1.state, STATE_ARMOR_VULNERABLE);
        let d2 = apply_structure_damage(&ctx, snap, &mut ext, sid, 350_000.0, 10.0, hacker_char, now)
            .expect("abandoned armor");
        assert_eq!(d2.state, STATE_HULL_VULNERABLE);
        let d3 = apply_structure_damage(&ctx, snap, &mut ext, sid, 350_000.0, 10.0, hacker_char, now)
            .expect("abandoned hull");
        let disp = d3.destruction.expect("destroyed outcome");
        assert!(disp.destroyed_item_ids.contains(&94_101));
        assert!(disp.dropped_item_ids.contains(&94_102));
        assert!(disp.asset_safety_wrap_ids.is_empty());
        assert_eq!(snap.jump_clones[&owner_char][0].location_id, 60003760);
        assert!(snap.jump_clones[&owner_char][0].implants.is_empty());
    });
}
