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
            CorporationRecord {
                corporation_id: corp_id,
                name: "Unanchor Corp".into(),
                ticker: "UAC".into(),
                ceo_id: 8810,
                hq_station_id: Some(60003760),
                tax_rate: 0.0,
                master_wallet: IskCents(0),
                ..Default::default()
            },
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
