//! Unit tests for `structure_lifecycle` (profiles, space classes, power states, reinforcement, sync state machine, damage caps, destruction, and packaging).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
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
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("LifeType_{new_tid}");
    rec.volume = 10_000.0;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn seed_structure_types(ctx: &mut ServerContext) -> (u32, u32, u32) {
    let med_tid = 920_001u32;
    let xl_tid = 920_002u32;
    let core_tid = 920_003u32;
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
            (ATTR_SHIELD_DAMAGE_LIMIT, 5_000.0),
            (ATTR_ARMOR_DAMAGE_LIMIT, 5_000.0),
            (ATTR_STRUCTURE_DAMAGE_LIMIT, 5_000.0),
            (ATTR_HI_SLOTS, 3.0),
            (ATTR_MED_SLOTS, 3.0),
            (ATTR_LOW_SLOTS, 3.0),
            (ATTR_RIG_SLOTS, 3.0),
            (ATTR_SERVICE_SLOTS, 3.0),
            (ATTR_STRUCTURE_FULL_POWER_HP_MULT, 1.5),
        ],
    );
    clone_type_with(
        &mut ctx.sde,
        35834,
        xl_tid,
        1657,
        CATEGORY_STRUCTURE,
        &[
            (ATTR_RIG_SIZE, 4.0),
            (ATTR_SHIELD_CAPACITY, 1_000_000.0),
            (ATTR_ARMOR_HP, 2_000_000.0),
            (ATTR_HP, 3_000_000.0),
            (ATTR_REQUIRES_DEED_TYPE, f64::from(core_tid)),
            (ATTR_STRUCTURE_ROLE_BONUS, 25.0),
            (ATTR_ENG_MAT_BONUS, -2.0),
        ],
    );
    clone_type_with(&mut ctx.sde, 35832, core_tid, 4086, 17, &[]);
    (med_tid, xl_tid, core_tid)
}

fn make_record(sid: i64, tid: u32, corp_id: i64, sys_id: u32, state: &str, core: bool, fuel: u32) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: sid,
        type_id: tid,
        owner_corp_id: corp_id,
        solar_system_id: sys_id,
        name: format!("Struct {sid}"),
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
fn profile_space_class_and_power_states_cover_all_tiers() {
    let mut ctx = test_ctx();
    let (med_tid, xl_tid, core_tid) = seed_structure_types(&mut ctx);
    let svc_tid = 920_004u32;
    clone_type_with(
        &mut ctx.sde,
        35832,
        svc_tid,
        1321,
        66,
        &[(ATTR_SERVICE_FUEL_PER_HOUR, 10.0), (ATTR_SERVICE_FUEL_ONLINE, 20.0)],
    );

    let med = structure_profile(&ctx, med_tid).expect("medium profile");
    assert_eq!(med.hull_class, HullClass::Medium);
    assert!(structure_profile(&ctx, 2233).is_some()); // Customs Office
    assert!(structure_profile(&ctx, 3962).is_some()); // Mercenary Den

    let xl = structure_profile(&ctx, xl_tid).expect("xl profile");
    assert_eq!(xl.hull_class, HullClass::XLarge);
    assert_eq!(xl.core_type_id, Some(core_tid));
    assert!((xl.fuel_bonus_pct - 25.0).abs() < 1e-6);

    // Space class: Wormhole vs HighSec
    let mut st = make_record(5001, med_tid, 98501, 31000001, STATE_SHIELD_VULNERABLE, true, 10);
    assert_eq!(space_class(&ctx, 31000001), SpaceClass::Wormhole);
    assert_eq!(space_class(&ctx, 30000142), SpaceClass::HighSec);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.war_hq_structures.insert(5001);
        ext.lifecycle.insert(
            5001,
            StructureLifecycleState {
                online_modules: vec![8001],
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
        snap.structures.insert(5001, st.clone());
        snap.items.insert(
            8001,
            InventoryItemRecord {
                item_id: 8001,
                type_id: svc_tid,
                owner_id: 98501,
                location_id: 5001,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(8001, true);
    });

    // Power states: NotOperational, FullPower, LowPower, Abandoned, FactionFortizar exempt
    let now = 1_700_000_000u64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        st.state = STATE_NO_CORE.into();
        assert_eq!(power_state(&ctx, snap, &ext, &st, now), PowerState::NotOperational);

        st.state = STATE_SHIELD_VULNERABLE.into();
        assert_eq!(power_state(&ctx, snap, &ext, &st, now), PowerState::FullPower);
        assert!(
            (effective_max_shield(&ctx, snap, &med, 5001, PowerState::FullPower) - 150_000.0).abs()
                < 1e-6
        );

        // Offline service module -> LowPower, then Abandoned after 7d
        snap.module_online_states.insert(8001, false);
        ext.lifecycle.get_mut(&5001).unwrap().online_modules.clear();
        assert_eq!(power_state(&ctx, snap, &ext, &st, now), PowerState::LowPower);

        ext.lifecycle.get_mut(&5001).unwrap().low_power_since_unix =
            Some(now - ABANDON_AFTER_LOW_POWER_SECS - 10);
        assert_eq!(power_state(&ctx, snap, &ext, &st, now), PowerState::Abandoned);
        assert!(
            (effective_max_shield(&ctx, snap, &med, 5001, PowerState::Abandoned) - 100_000.0).abs()
                < 1e-6
        );

        // Faction Fortizar never goes abandoned
        st.type_id = 47512;
        assert_eq!(power_state(&ctx, snap, &ext, &st, now), PowerState::LowPower);
    });
}

#[test]
fn reinforce_timings_jitter_and_nearest_npc_station_work() {
    let mut ctx = test_ctx();
    let _ = seed_structure_types(&mut ctx);
    let now = 1_700_000_000u64;
    let exit = next_reinforce_exit(now + 24 * 3_600, 18);
    assert!(exit >= now + 24 * 3_600);
    let max_j = reinforce_jitter_window_secs(HullClass::Medium, true);
    let jitter = apply_reinforce_jitter(exit, 1_800, max_j);
    assert!((jitter as i128 - exit as i128).abs() <= i128::from(max_j));

    let armor_secs =
        armor_reinforcement_secs(HullClass::Medium, SpaceClass::HighSec, PowerState::FullPower)
            .expect("armor reinforce secs");
    assert!(armor_secs >= 24 * 3_600);
    let hull_secs =
        hull_reinforcement_secs(HullClass::XLarge, SpaceClass::HighSec, PowerState::FullPower)
            .expect("hull reinforce secs");
    assert!(hull_secs >= armor_secs);

    let nearest = nearest_npc_station_from_pos(&ctx, 30000142, glam::DVec3::ZERO);
    assert!(nearest.is_some());
}

#[test]
fn sync_structure_advances_lifecycle_burn_fuel_and_repair_timers() {
    let mut ctx = test_ctx();
    let (med_tid, xl_tid, _) = seed_structure_types(&mut ctx);
    let base_now = ctx.now_unix();

    // AnchorVulnerable (unpaused) -> Anchoring
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            6001,
            make_record(6001, med_tid, 98601, 30000142, STATE_ANCHOR_VULNERABLE, false, 100),
        );
        snap.structures.get_mut(&6001).unwrap().reinforced_until_unix = base_now + 10;
        let mut ext = load_ext_state(snap);
        assert!(sync_structure(&ctx, snap, &mut ext, 6001, base_now + 60).is_none());
        save_ext_state(snap, &ext);
        assert_eq!(snap.structures[&6001].state, STATE_ANCHORING);
    });

    // Core-requiring hull stops in NoCore after Anchoring expires
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            6002,
            make_record(6002, xl_tid, 98601, 30000142, STATE_ANCHORING, false, 100),
        );
        snap.structures.get_mut(&6002).unwrap().reinforced_until_unix = base_now + 20;
        let mut ext = load_ext_state(snap);
        assert!(sync_structure(&ctx, snap, &mut ext, 6002, base_now + 60).is_none());
        save_ext_state(snap, &ext);
        assert_eq!(snap.structures[&6002].state, STATE_NO_CORE);
    });

    // Onlining -> ShieldVulnerable; repair timer heals shield & armor
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = make_record(6003, med_tid, 98601, 30000142, STATE_ONLINING, true, 50);
        st.shield_hp = 10_000.0;
        st.armor_hp = 20_000.0;
        st.reinforced_until_unix = base_now + 30;
        snap.structures.insert(6003, st);
        snap.items.insert(
            90001,
            InventoryItemRecord {
                item_id: 90001,
                type_id: 35892,
                owner_id: 98601,
                location_id: 6003,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.lifecycle.insert(
            6003,
            StructureLifecycleState {
                online_modules: vec![90001],
                repair_until_unix: Some(base_now + 40),
                last_fuel_tick_unix: Some(base_now.saturating_sub(7_200)),
                ..Default::default()
            },
        );
        assert!(sync_structure(&ctx, snap, &mut ext, 6003, base_now + 60).is_none());
        save_ext_state(snap, &ext);
        let st = &snap.structures[&6003];
        assert_eq!(st.state, STATE_SHIELD_VULNERABLE);
        assert!((st.armor_hp - 200_000.0).abs() < 1e-3);
    });
}

#[test]
fn damage_cycle_transitions_shield_armor_hull_and_destroy_drops_loot() {
    let mut ctx = test_ctx();
    let (med_tid, _, core_tid) = seed_structure_types(&mut ctx);

    let sid = 7001i64;
    let attacker_id = 8801i64;
    let corp_id = 98701i64;
    let owner_char_id = 8802i64;
    let now = ctx.now_unix();
    if let Some(t) = Arc::make_mut(&mut ctx.sde).types.get_mut(&med_tid) {
        t.default_attributes.insert(ATTR_REQUIRES_DEED_TYPE, f64::from(core_tid));
    }
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = owner_char_id;
        ch.user_id = owner_char_id as u64;
        ch.name = "HomeOwner".into();
        ch.corporation_id = corp_id;
        ch.solar_system_id = 30000142;
        ch.active_ship_id = 99001;
        ch.home_station_id = sid;
        ch.balance = IskCents(10_000);
        snap.characters.insert(owner_char_id, ch);
        snap.structures.insert(
            sid,
            make_record(sid, med_tid, corp_id, 30000142, STATE_SHIELD_VULNERABLE, true, 12),
        );
        snap.items.insert(
            91001,
            InventoryItemRecord {
                item_id: 91001,
                type_id: 2454,
                owner_id: owner_char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        // 1. Deplete shield -> ArmorReinforced (cap is 5_000/s * 30s = 150_000)
        let r1 = apply_structure_damage(&ctx, snap, &mut ext, sid, 150_000.0, 30.0, attacker_id, now)
            .expect("shield dmg");
        assert_eq!(r1.state, STATE_ARMOR_REINFORCED);

        // 2. While reinforced, damage rejected
        assert_eq!(
            apply_structure_damage(&ctx, snap, &mut ext, sid, 10_000.0, 10.0, attacker_id, now)
                .unwrap_err()
                .code,
            "StructureReinforcedInvulnerable"
        );

        // 3. Move to ArmorVulnerable & deplete armor -> HullVulnerable (Medium hull skips hull reinforcement)
        snap.structures.get_mut(&sid).unwrap().state = STATE_ARMOR_VULNERABLE.into();
        let r2 = apply_structure_damage_typed(
            &ctx,
            snap,
            &mut ext,
            sid,
            100_000.0,
            100_000.0,
            50_000.0,
            0.0,
            60.0,
            attacker_id,
            now,
        )
        .expect("armor dmg");
        assert_eq!(r2.state, STATE_HULL_VULNERABLE);

        // 4. Destroy in HullVulnerable -> Killmail + Wreck + Core drop + Asset safety
        let r3 = apply_structure_damage(&ctx, snap, &mut ext, sid, 400_000.0, 100.0, attacker_id, now)
            .expect("hull destroy");
        assert!(r3.destruction.is_some());
        save_ext_state(snap, &ext);
    });

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&sid].state, STATE_DESTROYED);
        assert!(!snap.killmails.is_empty());
        let ext = load_ext_state(snap);
        assert!(!ext.asset_safety_wraps.is_empty());
    });
}

#[test]
fn package_structure_hack_reveal_and_validate_refit_work() {
    let mut ctx = test_ctx();
    let (med_tid, _, core_tid) = seed_structure_types(&mut ctx);
    let sid = 8001i64;
    let corp_id = 98801i64;
    let now = ctx.now_unix();
    if let Some(t) = Arc::make_mut(&mut ctx.sde).types.get_mut(&med_tid) {
        t.default_attributes.insert(ATTR_REQUIRES_DEED_TYPE, f64::from(core_tid));
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Unanchor Corp".into();
                __corp.ticker = "UAC".into();
                __corp.ceo_id = 8810;
                __corp.hq_station_id = Some(60003760);
                __corp.tax_rate = 0.0;
                __corp.set_division_balance(1, IskCents(0));
                __corp },
        );
        let mut st = make_record(sid, med_tid, corp_id, 30000142, STATE_SHIELD_VULNERABLE, true, 5);
        st.armor_hp = 50.0;
        snap.structures.insert(sid, st);
    });

    // Damaged armor blocks refit, then repair allows it
    ctx.db.with_mut_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert_eq!(
            validate_structure_refit(&ctx, snap, &ext, sid, now).unwrap_err().code,
            "StructureDamagedCannotRefit"
        );
        snap.structures.get_mut(&sid).unwrap().armor_hp = 200_000.0;
        assert!(validate_structure_refit(&ctx, snap, &ext, sid, now).is_ok());
    });

    // Data hack reveal records reinforce exit window for non-owning character
    let reveal = record_structure_hack_reveal(&ctx, 8899, sid);
    assert!(reveal.is_some());

    // Package structure drops hull + core into space
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let _ = package_structure(&ctx, snap, &mut ext, sid, now);
        save_ext_state(snap, &ext);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.structures.contains_key(&sid));
        let in_space: Vec<u32> = snap
            .items
            .values()
            .filter(|i| i.location_id == 30000142 && i.owner_id == corp_id)
            .map(|i| i.type_id)
            .collect();
        assert!(in_space.contains(&med_tid));
        assert!(in_space.contains(&core_tid));
    });
}

#[test]
fn metenox_is_flex_hull_class_with_integrated_core_and_single_reinforce() {
    let mut ctx = test_ctx();
    if !ctx.sde.types.contains_key(&81_826) {
        clone_type_with(&mut ctx.sde, 35_832, 81_826, 4744, CATEGORY_STRUCTURE, &[(ATTR_DISALLOW_IN_HIGHSEC, 1.0)]);
    }
    let prof = structure_profile(&ctx, 81_826).expect("Metenox profile");
    assert_eq!(prof.hull_class, HullClass::Flex);
    assert_eq!(prof.core_type_id, Some(81_920));
    assert_eq!(armor_reinforcement_secs(prof.hull_class, SpaceClass::LowNullSec, PowerState::FullPower), None);
    assert_eq!(hull_reinforcement_secs(prof.hull_class, SpaceClass::LowNullSec, PowerState::FullPower), Some(30 * 60));
}

#[tokio::test]
async fn destroy_and_package_structure_evacuate_gunner_docked_pilots_and_reset_home() {
    let mut ctx = test_ctx();
    let (med_tid, _, _) = seed_structure_types(&mut ctx);
    let corp_id = 98_701i64;
    let sys_id = 30_000_142u32;
    let now = ctx.now_unix();

    for (run_idx, do_destroy) in [true, false].into_iter().enumerate() {
        let base_id = 1_000_070_000_000i64 + (run_idx as i64) * 100_000;
        let sid = base_id + 1;
        let gunner_cid = 70_101 + (run_idx as i64) * 10;
        let gunner_ship_id = base_id + 2;
        let pilot_cid = 70_102 + (run_idx as i64) * 10;
        let pilot_ship_id = base_id + 3;
        let spare_ship_id = base_id + 4;
        let podless_cid = 70_103 + (run_idx as i64) * 10;
        let base_char = ctx.db.with_snapshot(|s| s.characters.values().next().cloned().expect("demo char"));

        ctx.db.with_mut_snapshot(|snap| {
            let mut corp = CorporationRecord::default();
            corp.corporation_id = corp_id;
            corp.name = "Evac Corp".into();
            corp.ticker = "EVAC".into();
            corp.ceo_id = gunner_cid;
            corp.hq_station_id = Some(60_003_760);
            snap.corporations.insert(corp_id, corp);
            snap.structures.insert(
                sid,
                make_record(sid, med_tid, corp_id, sys_id, STATE_SHIELD_VULNERABLE, true, 5),
            );
            snap.structures.get_mut(&sid).unwrap().position = [1_000.0, 2_000.0, 3_000.0];

            // Gunner controlling structure (`active_ship_id == sid`), docked inside structure
            let mut g_ch = base_char.clone();
            g_ch.character_id = gunner_cid;
            g_ch.corporation_id = corp_id;
            g_ch.solar_system_id = sys_id;
            g_ch.station_id = None;
            g_ch.structure_id = Some(sid);
            g_ch.active_ship_id = sid;
            g_ch.home_station_id = sid;
            snap.characters.insert(gunner_cid, g_ch);
            snap.items.insert(
                gunner_ship_id,
                InventoryItemRecord {
                    item_id: gunner_ship_id,
                    type_id: 587,
                    owner_id: gunner_cid,
                    location_id: sid,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );

            // Docked pilot with active ship + spare hangar ship (spare must go to Asset Safety)
            let mut p_ch = base_char.clone();
            p_ch.character_id = pilot_cid;
            p_ch.corporation_id = corp_id;
            p_ch.solar_system_id = sys_id;
            p_ch.station_id = None;
            p_ch.structure_id = Some(sid);
            p_ch.active_ship_id = pilot_ship_id;
            p_ch.home_station_id = sid;
            snap.characters.insert(pilot_cid, p_ch);
            snap.items.insert(
                pilot_ship_id,
                InventoryItemRecord {
                    item_id: pilot_ship_id,
                    type_id: 587,
                    owner_id: pilot_cid,
                    location_id: sid,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                spare_ship_id,
                InventoryItemRecord {
                    item_id: spare_ship_id,
                    type_id: 587,
                    owner_id: pilot_cid,
                    location_id: sid,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );

            // Docked pilot with no valid ship (`active_ship_id == 0`) -> Capsule 670 spawned
            let mut n_ch = base_char;
            n_ch.character_id = podless_cid;
            n_ch.corporation_id = corp_id;
            n_ch.solar_system_id = sys_id;
            n_ch.station_id = None;
            n_ch.structure_id = Some(sid);
            n_ch.active_ship_id = 0;
            n_ch.home_station_id = sid;
            snap.characters.insert(podless_cid, n_ch);

            let mut ext = load_ext_state(snap);
            ext.active_pilot_by_structure.insert(sid, gunner_cid);
            ext.previous_ship_by_structure.insert(sid, gunner_ship_id);
            save_ext_state(snap, &ext);
        });

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let outcome = ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let out = if do_destroy {
                destroy_structure(&ctx, snap, &mut ext, sid, 9999, now)
            } else {
                package_structure(&ctx, snap, &mut ext, sid, now)
            };
            save_ext_state(snap, &ext);
            out
        });
        assert!(!outcome.asset_safety_wrap_ids.is_empty(), "spare ship must enter asset safety");

        ctx.db.with_snapshot(|snap| {
            let ext = load_ext_state(snap);
            assert!(!ext.active_pilot_by_structure.contains_key(&sid));

            // Gunner restored to gunner_ship_id and evacuated into space
            let g = snap.characters.get(&gunner_cid).unwrap();
            assert_eq!(g.structure_id, None);
            assert_eq!(g.active_ship_id, gunner_ship_id);
            assert_ne!(g.home_station_id, sid);
            assert!(g.home_station_id > 0);
            let g_ship = snap.items.get(&gunner_ship_id).expect("gunner ship intact");
            assert_eq!(g_ship.location_id, i64::from(sys_id));
            assert_eq!(g_ship.flag, InventoryFlag::None);

            // Normal docked pilot evacuated with pilot_ship_id intact, spare_ship wrapped in AS
            let p = snap.characters.get(&pilot_cid).unwrap();
            assert_eq!(p.structure_id, None);
            assert_eq!(p.active_ship_id, pilot_ship_id);
            assert_ne!(p.home_station_id, sid);
            let p_ship = snap.items.get(&pilot_ship_id).expect("pilot active ship intact");
            assert_eq!(p_ship.location_id, i64::from(sys_id));
            let spare = snap.items.get(&spare_ship_id).expect("spare ship preserved in wrap");
            assert_ne!(spare.location_id, i64::from(sys_id));
            assert_ne!(spare.location_id, sid);

            // Podless docked pilot spawned Capsule 670 in space
            let n = snap.characters.get(&podless_cid).unwrap();
            assert_eq!(n.structure_id, None);
            assert_ne!(n.home_station_id, sid);
            let pod = snap.items.get(&n.active_ship_id).expect("spawned capsule");
            assert_eq!(pod.type_id, 670);
            assert_eq!(pod.location_id, i64::from(sys_id));
            assert_eq!(pod.flag, InventoryFlag::None);
        });

        assert!(sys_handle.get_ball_snapshot(gunner_ship_id).await.unwrap().is_some());
        assert!(sys_handle.get_ball_snapshot(pilot_ship_id).await.unwrap().is_some());
        let pod_id = ctx.db.with_snapshot(|s| s.characters[&podless_cid].active_ship_id);
        assert!(sys_handle.get_ball_snapshot(pod_id).await.unwrap().is_some());
    }
}

#[test]
fn sync_all_structures_pauses_industry_jobs_when_fuel_depletes_and_marks_ready() {
    let ctx = test_ctx();
    let base_now = 1_750_000_000_u64;
    let clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_now));
    ctx.set_clock(clock);

    let sid = 1_000_000_999_001_i64;
    let corp_id = 98_000_001_i64;
    let char_id = 90_000_001_i64;
    let active_jid = 7_700_001_i64;
    let ready_jid = 7_700_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35_825, // Raitaru
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Test Raitaru".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
        let svc_mod_id = 900_999_i64;
        snap.items.insert(
            svc_mod_id,
            InventoryItemRecord {
                item_id: svc_mod_id,
                type_id: 35_878, // Standup Manufacturing Plant I
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_mod_id, true);
        let mut ext = load_ext_state(snap);
        ext.lifecycle.insert(
            sid,
            crate::services::station_structure::structure_upwell_ops::StructureLifecycleState {
                last_fuel_tick_unix: Some(base_now - 3_600),
                online_modules: vec![svc_mod_id],
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);

        snap.industry_jobs.insert(
            active_jid,
            eve_db::IndustryJobRecord {
                job_id: active_jid,
                installer_id: char_id,
                facility_id: sid,
                blueprint_item_id: 900_001,
                blueprint_type_id: 587,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 7_200,
                start_unix: base_now - 600,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            ready_jid,
            eve_db::IndustryJobRecord {
                job_id: ready_jid,
                installer_id: char_id,
                facility_id: sid,
                blueprint_item_id: 900_002,
                blueprint_type_id: 587,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 300,
                start_unix: base_now - 600,
                ..Default::default()
            },
        );
    });

    // Running sync_all_structures burns last fuel block -> low_power_since_unix set & jobs paused!
    let _ = sync_all_structures(&ctx);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&sid].fuel_blocks, 0);
        let ext = load_ext_state(snap);
        assert!(
            ext.lifecycle
                .get(&sid)
                .and_then(|l| l.low_power_since_unix)
                .unwrap_or(0)
                > 0
        );
        assert_eq!(snap.industry_jobs[&active_jid].status, "Paused");
        let active_meta = crate::services::industry_pi::industry_facilities_bps::load_job_cost_meta(
            snap, active_jid,
        )
        .expect("in-progress job gets paused meta immediately on fuel outage");
        assert!(active_meta.paused_at_unix.is_some());
        assert!(!active_meta.ready_before_offline);

        let ready_meta = crate::services::industry_pi::industry_facilities_bps::load_job_cost_meta(
            snap, ready_jid,
        )
        .expect("already-completed job gets ready_before_offline meta");
        assert!(ready_meta.ready_before_offline);
        assert!(!any_structure_or_industry_needs_sync(&ctx, snap, base_now));
    });
}

#[test]
fn destroy_skyhook_and_package_sov_hub_clean_runtime_reconcile_network_and_queue_quasar_notices() {
    use crate::services::quasar_sov_hub::{planet_yield_for_planet, star_power_for_system};
    use crate::services::quasar_sov_workforce::systems_within_jumps;
    use crate::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
    use eve_db::{PersistedHubUpgrade, PersistedSovHubData, PersistedWorkforceMode};

    let ctx = test_ctx();
    let now = 1_750_100_000_u64;
    let sde = &ctx.sde;
    let (sys_a, planet_id, _planet_wf, sys_b) = [30_000_669_u32, 30_000_142_u32]
        .into_iter()
        .flat_map(|seed| systems_within_jumps(sde, seed, 8).into_keys())
        .filter(|&sa| star_power_for_system(sde, sa) >= 250)
        .find_map(|sa| {
            let planet = sde
                .celestials_in_system_slice(sa)
                .iter()
                .filter(|c| c.group_id == 7)
                .find_map(|c| {
                    let (_, wf) = planet_yield_for_planet(c.type_id, c.radius);
                    (wf >= 3_000).then_some((c.celestial_id, wf))
                })?;
            let nb = systems_within_jumps(sde, sa, 1)
                .into_iter()
                .find(|&(sb, hops)| {
                    hops == 1 && sb != sa && star_power_for_system(sde, sb) >= 250
                })?
                .0;
            Some((sa, planet.0, planet.1, nb))
        })
        .expect("system pair with >=3000 workforce planet and neighbor system");

    let corp_id = 98_920_001_i64;
    let attacker_id = 90_000_002_i64;
    let hub_a = 88_920_001_i64;
    let hub_b = 88_920_002_i64;
    let skyhook_id = 88_920_101_i64;
    let export_amt = 1_500_u64;

    let out_skyhook = ctx.db.with_mut_snapshot(|snap| {
        let mut c = CorporationRecord::default();
        c.corporation_id = corp_id;
        c.ceo_id = 90_000_001;
        snap.corporations.insert(corp_id, c);

        for (id, ty, sys) in [
            (hub_a, TYPE_SOVEREIGNTY_HUB, sys_a),
            (hub_b, TYPE_SOVEREIGNTY_HUB, sys_b),
            (skyhook_id, TYPE_ORBITAL_SKYHOOK, sys_a),
        ] {
            snap.structures.insert(
                id,
                UpwellStructureRecord {
                    structure_id: id,
                    type_id: ty,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("LifecycleSov {id}"),
                    state: STATE_SHIELD_VULNERABLE.into(),
                    ..Default::default()
                },
            );
        }
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, planet_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
        snap.sov_runtime.skyhook_reagent_bays.insert(skyhook_id, (81_143, 200, 100, now));
        snap.sov_runtime.skyhook_reagents.insert(skyhook_id, (200, 100));
        snap.sov_runtime.skyhook_maturity_rem.insert(skyhook_id, (100, 500));
        snap.sov_runtime.skyhook_last_harvest_unix.insert(skyhook_id, now - 60);
        snap.sov_runtime.skyhook_theft_windows.insert(skyhook_id, (now + 100, now + 7_300));
        snap.sov_runtime.skyhook_hackable.insert(skyhook_id, true);
        snap.sov_runtime
            .skyhook_silo_links
            .insert(skyhook_id, (999_001, attacker_id, now + 60, 1_000.0));

        snap.sov_runtime.sov_hubs.insert(
            hub_a,
            PersistedSovHubData {
                hub_id: hub_a,
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(sys_b),
                    amount: export_amt,
                },
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81_615,
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_b,
            PersistedSovHubData {
                hub_id: hub_b,
                workforce: PersistedWorkforceMode::Import {
                    sources: vec![sys_a],
                },
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81_615,
                    online: true,
                    low_power: false,
                }],
                ..Default::default()
            },
        );

        let mut ext = load_ext_state(snap);
        let out = destroy_structure(&ctx, snap, &mut ext, skyhook_id, attacker_id, now);
        save_ext_state(snap, &ext);
        out
    });

    flush_disposal_quasar_notices(&ctx, &out_skyhook, now);

    let skyhook_notice_urls: Vec<&str> = out_skyhook
        .pending_quasar_notices
        .iter()
        .map(|(_, a)| a.type_url.as_str())
        .collect();
    assert!(skyhook_notice_urls.iter().any(|u| u.ends_with("ResourcesSimulatedNotice")));
    assert!(skyhook_notice_urls.iter().any(|u| u.ends_with("StateChangedNotice")));
    assert!(skyhook_notice_urls.iter().any(|u| u.ends_with("HubUpgradesSimulatedNotice")));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.skyhook_planets.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_active.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_reagent_bays.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_reagents.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_maturity_rem.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_last_harvest_unix.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_theft_windows.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_hackable.contains_key(&skyhook_id));
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&skyhook_id));

        let ha = &snap.sov_runtime.sov_hubs[&hub_a];
        assert!(ha.upgrades[0].low_power);
        assert_eq!(
            ha.workforce,
            PersistedWorkforceMode::Export {
                destination: Some(sys_b),
                amount: 0,
            }
        );
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
    });

    // Re-enable Skyhook, reset both hubs online, then package Hub A -> Hub B must overload
    // and emit Quasar notices when Hub A leaves space.
    let out_pkg_hub = ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&skyhook_id).unwrap().state = STATE_SHIELD_VULNERABLE.into();
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, planet_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
        if let Some(ha) = snap.sov_runtime.sov_hubs.get_mut(&hub_a) {
            ha.workforce = PersistedWorkforceMode::Export {
                destination: Some(sys_b),
                amount: export_amt,
            };
            ha.upgrades[0].low_power = false;
        }
        if let Some(hb) = snap.sov_runtime.sov_hubs.get_mut(&hub_b) {
            hb.upgrades[0].low_power = false;
        }
        let mut ext = load_ext_state(snap);
        let out = package_structure(&ctx, snap, &mut ext, hub_a, now + 10);
        save_ext_state(snap, &ext);
        out
    });

    flush_disposal_quasar_notices(&ctx, &out_pkg_hub, now + 10);
    let hub_notice_urls: Vec<&str> = out_pkg_hub
        .pending_quasar_notices
        .iter()
        .map(|(_, a)| a.type_url.as_str())
        .collect();
    assert!(hub_notice_urls.iter().any(|u| u.ends_with("ResourcesSimulatedNotice")));
    assert!(hub_notice_urls.iter().any(|u| u.ends_with("HubUpgradesSimulatedNotice")));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.sov_hubs.contains_key(&hub_a));
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
    });
}

#[tokio::test]
async fn combat_skyhook_hub_and_mercden_destruction_cleans_dens_tax_balls_notifs_and_workforce() {
    use crate::services::quasar_sov_hub::{all_upgrade_definitions, planet_yield_for_planet, system_yield};
    use crate::services::quasar_sov_workforce::{exported_amount, imports};
    use crate::services::sov_gameplay::TYPE_MERCENARY_DEN;
    use crate::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
    use crate::services::structure_upwell_ops::load_ext_state;
    use eve_db::durable_domain_types::{
        PersistedHubUpgrade, PersistedMercenaryActivity, PersistedSovHubData,
    };
    use eve_db::{
        CharacterRecord, OnlineSessionRecord, PersistedMercenaryDen, PersistedWorkforceMode,
    };
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;

    let ctx = test_ctx();
    let sys_a: u32 = 30_000_669; // N3-JBX
    let sys_b: u32 = 30_000_671; // GN-PDU (1 jump)
    let corp_a: i64 = 98_780_001;
    let corp_raider: i64 = 98_780_002;
    let observer_cid: i64 = 2_117_800_001;
    let den_owner_cid: i64 = 2_117_800_002;

    let hub_a: i64 = 1_097_800_001;
    let hub_b: i64 = 1_097_800_002;
    let skyhook_1: i64 = 1_097_800_010;
    let skyhook_2: i64 = 1_097_800_011;
    let den_1: i64 = 1_097_800_020;
    let den_2: i64 = 1_097_800_021;
    let mto_ball_1: i64 = 1_997_800_030;
    let now = ctx.now_unix();

    let (planet_1, wf_1) = ctx
        .sde
        .celestials_in_system_slice(sys_a)
        .iter()
        .filter(|c| c.group_id == 7)
        .map(|c| (c.celestial_id, planet_yield_for_planet(c.type_id, c.radius).1))
        .max_by_key(|&(_, wf)| wf)
        .expect("SYS_A workforce planet");
    assert!(wf_1 > 0);
    let planet_2 = ctx
        .sde
        .celestials_in_system_slice(sys_a)
        .iter()
        .filter(|c| c.group_id == 7 && c.celestial_id != planet_1)
        .map(|c| (c.celestial_id, planet_yield_for_planet(c.type_id, c.radius).1))
        .min_by_key(|&(_, wf)| wf)
        .map(|(id, _)| id)
        .unwrap_or(planet_1 + 1);

    let sys_b_power: u64 = {
        let y = system_yield(&ctx.sde, sys_b);
        y.stars.iter().map(|(_, p)| *p).sum::<u64>() + y.planet_power
    };
    let hub_b_upg = all_upgrade_definitions(&ctx.sde)
        .into_iter()
        .find(|d| {
            d.workforce_produced == 0
                && d.workforce_required > 0
                && d.workforce_required <= wf_1
                && d.power_required <= sys_b_power
        })
        .expect("SYS_B upgrade");

    let sys_handle = ctx.systems.get_or_spawn(sys_a);
    for (bid, tid) in [
        (hub_a, TYPE_SOVEREIGNTY_HUB),
        (skyhook_1, TYPE_ORBITAL_SKYHOOK),
        (skyhook_2, TYPE_ORBITAL_SKYHOOK),
        (den_1, TYPE_MERCENARY_DEN),
        (den_2, TYPE_MERCENARY_DEN),
        (mto_ball_1, 12_345),
    ] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: bid,
                type_id: tid,
                position: DVec3::new(100_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1.0e8,
                radius: 1_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, corp) in [(observer_cid, corp_a), (den_owner_cid, corp_raider)] {
            snap.characters.insert(
                cid,
                CharacterRecord {
                    character_id: cid,
                    user_id: cid as u64,
                    name: format!("Char {cid}"),
                    corporation_id: corp,
                    solar_system_id: sys_a,
                    ..Default::default()
                },
            );
            snap.sessions.insert(
                cid as u64,
                OnlineSessionRecord {
                    user_id: cid as u64,
                    username: format!("user_{cid}"),
                    session_id: cid as u64,
                    character_id: Some(cid),
                    connected_at_unix: now,
                    computer_hash: None,
                },
            );
        }
        snap.corporations.insert(corp_a, CorporationRecord::new(corp_a, "Sov Corp", "SOV", observer_cid));
        snap.corporations.insert(corp_raider, CorporationRecord::new(corp_raider, "Raider Corp", "RAD", den_owner_cid));

        for (sid, hid, name) in [(sys_a, hub_a, "Hub A"), (sys_b, hub_b, "Hub B")] {
            snap.system_states.entry(sid).or_default().sov_alliance_id = Some(99_780_001);
            snap.structures.insert(
                hid,
                UpwellStructureRecord {
                    structure_id: hid,
                    type_id: TYPE_SOVEREIGNTY_HUB,
                    owner_corp_id: corp_a,
                    solar_system_id: sid,
                    name: name.into(),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }
        snap.sov_runtime.sov_claimed_since_unix.insert(sys_a, now.saturating_sub(1_000));
        snap.sov_runtime.hacked_hubs.insert(hub_a, now + 600);
        snap.sov_runtime.fuel_access_groups.insert(hub_a, 77);

        for (sid, corp, name) in [
            (skyhook_1, corp_a, "Skyhook 1"),
            (skyhook_2, corp_a, "Skyhook 2"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: TYPE_ORBITAL_SKYHOOK,
                    owner_corp_id: corp,
                    solar_system_id: sys_a,
                    name: name.into(),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
            snap.sov_runtime.skyhook_active.insert(sid, true);
        }
        snap.sov_runtime.skyhook_planets.insert(skyhook_1, planet_1);
        snap.sov_runtime.skyhook_planets.insert(skyhook_2, planet_2);
        snap.sov_runtime.mercenary_den_tax_pct.insert(skyhook_1, 25);
        snap.sov_runtime.mercenary_den_tax_pct.insert(skyhook_2, 15);

        let mut fuel_b = std::collections::BTreeMap::new();
        if hub_b_upg.fuel_type > 0 {
            fuel_b.insert(hub_b_upg.fuel_type, hub_b_upg.fuel_startup + hub_b_upg.fuel_per_hour * 100);
        }
        snap.sov_runtime.sov_hubs.insert(
            hub_a,
            PersistedSovHubData {
                hub_id: hub_a,
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(sys_b),
                    amount: wf_1,
                },
                last_updated_unix: now,
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_b,
            PersistedSovHubData {
                hub_id: hub_b,
                upgrades: vec![PersistedHubUpgrade {
                    type_id: hub_b_upg.type_id,
                    online: true,
                    low_power: false,
                }],
                fuel: fuel_b,
                workforce: PersistedWorkforceMode::Import {
                    sources: vec![sys_a],
                },
                last_updated_unix: now,
                ..Default::default()
            },
        );

        for (did, sid, pid, anarchy) in [
            (den_1, skyhook_1, planet_1, 10),
            (den_2, skyhook_2, planet_2, 100),
        ] {
            snap.structures.insert(
                did,
                UpwellStructureRecord {
                    structure_id: did,
                    type_id: TYPE_MERCENARY_DEN,
                    owner_corp_id: corp_raider,
                    solar_system_id: sys_a,
                    name: format!("MercDen {did}"),
                    state: "ShieldVulnerable".into(),
                    ..Default::default()
                },
            );
            snap.sov_runtime.mercenary_dens.insert(
                did,
                PersistedMercenaryDen {
                    den_id: did,
                    type_id: TYPE_MERCENARY_DEN,
                    owner_char_id: den_owner_cid,
                    skyhook_id: sid,
                    solar_system_id: sys_a,
                    planet_id: pid,
                    enabled: true,
                    cargo_extraction_enabled: true,
                    development_level: 10,
                    anarchy_level: anarchy,
                    infomorphs: 50,
                    simulated_at_unix: now,
                    next_activity_generation_unix: now + 3_600,
                },
            );
        }

        let act_key = "combat_skyhook_mto".to_string();
        snap.sov_runtime.mto_site_balls.insert(mto_ball_1, act_key.clone());
        snap.sov_runtime.mercenary_activities.insert(
            act_key,
            PersistedMercenaryActivity {
                uuid: vec![0xC3; 16],
                den_id: den_1,
                solar_system_id: sys_a,
                dungeon_id: 12_367,
                started: true,
                started_by: den_owner_cid,
                expiry_unix: now + 3_600,
                development_impact: 5,
                anarchy_impact: -10,
                infomorph_bonus: 0,
            },
        );
        snap.items.insert(
            mto_ball_1,
            InventoryItemRecord {
                item_id: mto_ball_1,
                type_id: 12_345,
                owner_id: 1,
                location_id: i64::from(sys_a),
                flag: InventoryFlag::None,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(mto_ball_1, [100_000.0, 0.0, 0.0]);
    });

    // 1. Combat destruction of Skyhook 1 cleans tax, attached Den 1, MTO/balls, emits
    //    OnMyMercenaryDenRemoved & OnSolarSystemSovStructuresUpdated, and reconciles workforce
    //    (if planet_1 == planet_2, Skyhook 2 keeps planet_2 wf; else export drops to Skyhook 2 wf or 0).
    let out_sky = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let out = destroy_structure(&ctx, snap, &mut ext, skyhook_1, observer_cid, now);
        save_ext_state(snap, &ext);
        out
    });
    let sky_urls: Vec<&str> = out_sky
        .pending_quasar_notices
        .iter()
        .map(|(_, a)| a.type_url.as_str())
        .collect();
    assert!(sky_urls.iter().any(|u| u.ends_with("StateChangedNotice")));
    assert!(sky_urls.iter().any(|u| u.ends_with("ResourcesSimulatedNotice")));
    assert!(sky_urls.iter().any(|u| u.ends_with("HubUpgradesSimulatedNotice")));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&skyhook_1].state, STATE_DESTROYED);
        assert_eq!(snap.structures[&den_1].state, STATE_DESTROYED);
        assert!(!snap.sov_runtime.mercenary_den_tax_pct.contains_key(&skyhook_1));
        assert!(!snap.sov_runtime.mercenary_dens.contains_key(&den_1));
        assert!(!snap.sov_runtime.mercenary_activities.contains_key("combat_skyhook_mto"));
        assert!(!snap.sov_runtime.mto_site_balls.contains_key(&mto_ball_1));
        assert!(!snap.items.contains_key(&mto_ball_1));
        assert!(!snap.item_positions.contains_key(&mto_ball_1));
        assert_eq!(exported_amount(snap, hub_a), 0);
        assert_eq!(imports(snap, &ctx.sde, hub_b), vec![(sys_a, 0)]);
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
        // Third-party Den 2 & tax on Skyhook 2 remain intact.
        assert_eq!(snap.sov_runtime.mercenary_den_tax_pct.get(&skyhook_2), Some(&15));
        assert!(snap.sov_runtime.mercenary_dens.contains_key(&den_2));
    });
    {
        let bp = sys_handle.ballpark.read().unwrap();
        assert!(!bp.balls.contains_key(&skyhook_1));
        assert!(!bp.balls.contains_key(&den_1));
        assert!(!bp.balls.contains_key(&mto_ball_1));
        assert!(bp.balls.contains_key(&den_2));
    }
    {
        let q = ctx.pending_notifications.read().unwrap();
        assert!(
            q.get(&den_owner_cid)
                .into_iter()
                .flatten()
                .any(|n| n.event == "OnMyMercenaryDenRemoved"),
            "expected OnMyMercenaryDenRemoved for attached den owner"
        );
        assert!(
            q.get(&observer_cid)
                .into_iter()
                .flatten()
                .any(|n| n.event == "OnSolarSystemSovStructuresUpdated"),
            "expected OnSolarSystemSovStructuresUpdated on skyhook combat destruction"
        );
    }

    // 2. Restore active export from Hub A -> Hub B (via an intact Skyhook on planet_1) while
    //    Skyhook 2 + Anarchy Den 2 remain anchored, then destroy Sovereignty Hub A in combat.
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_2, planet_1);
        if let Some(d2) = snap.sov_runtime.mercenary_dens.get_mut(&den_2) {
            d2.planet_id = planet_1;
            d2.anarchy_level = 10; // keep full wf_1 available until Hub A dies
        }
        if let Some(ha) = snap.sov_runtime.sov_hubs.get_mut(&hub_a) {
            ha.workforce = PersistedWorkforceMode::Export {
                destination: Some(sys_b),
                amount: wf_1,
            };
        }
        if let Some(hb) = snap.sov_runtime.sov_hubs.get_mut(&hub_b) {
            hb.upgrades[0].low_power = false;
        }
    });
    ctx.pending_notifications.write().unwrap().clear();
    let out_hub = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let out = destroy_structure(&ctx, snap, &mut ext, hub_a, observer_cid, now);
        save_ext_state(snap, &ext);
        out
    });
    let hub_urls: Vec<&str> = out_hub
        .pending_quasar_notices
        .iter()
        .map(|(_, a)| a.type_url.as_str())
        .collect();
    assert!(hub_urls.iter().any(|u| u.ends_with("StateChangedNotice")));
    assert!(hub_urls.iter().any(|u| u.ends_with("ResourcesSimulatedNotice")));
    assert!(hub_urls.iter().any(|u| u.ends_with("HubUpgradesSimulatedNotice")));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&hub_a].state, STATE_DESTROYED);
        assert!(!snap.sov_runtime.sov_hubs.contains_key(&hub_a));
        assert!(!snap.sov_runtime.hacked_hubs.contains_key(&hub_a));
        assert!(!snap.sov_runtime.fuel_access_groups.contains_key(&hub_a));
        assert!(!snap.sov_runtime.sov_claimed_since_unix.contains_key(&sys_a));
        assert_eq!(snap.system_states[&sys_a].sov_alliance_id, None);
        assert_eq!(imports(snap, &ctx.sde, hub_b), vec![(sys_a, 0)]);
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
        assert!(snap.sov_runtime.mercenary_dens.contains_key(&den_2));
    });
    assert!(ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&observer_cid)
        .into_iter()
        .flatten()
        .any(|n| n.event == "OnSovereigntyChanged"));

    // 3. Direct packaging of Mercenary Den 2 cleans den runtime, Ballpark ball, and notifies owner.
    ctx.pending_notifications.write().unwrap().clear();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let _ = package_structure(&ctx, snap, &mut ext, den_2, now);
        save_ext_state(snap, &ext);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.structures.contains_key(&den_2));
        assert!(!snap.sov_runtime.mercenary_dens.contains_key(&den_2));
    });
    assert!(!sys_handle.ballpark.read().unwrap().balls.contains_key(&den_2));
    assert!(ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&den_owner_cid)
        .into_iter()
        .flatten()
        .any(|n| n.event == "OnMyMercenaryDenRemoved"));
}
