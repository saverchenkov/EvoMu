//! Behavioral edge-case tests for `structure_lifecycle.rs`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureExtendedState, StructureLifecycleState,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
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
    rec.name = format!("EdgeLifeType_{new_tid}");
    rec.volume = 10_000.0;
    rec.radius = 2_500.0;
    rec.base_price = 50_000_000.0;
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
        name: format!("EdgeStruct {sid}"),
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
fn apply_damage_errors_deploy_pause_unanchor_abort_and_low_power_skip_to_hull() {
    let mut ctx = test_ctx();
    let base_tid = ctx.sde.types.keys().next().copied().unwrap_or(34);
    let astra_tid = 9_935_832_u32;
    let non_struct_tid = 9_900_034_u32;

    clone_type_with(
        &mut ctx.sde,
        base_tid,
        astra_tid,
        1657, // Citadel (Medium)
        65,
        &[
            (263, 100_000.0), // shield
            (265, 200_000.0), // armor
            (9, 300_000.0),   // hull
            (271, 0.5),       // shield EM resonance
            (274, 0.8),       // shield thermal resonance
            (273, 1.0),       // shield kinetic resonance
            (272, 1.0),       // shield explosive resonance
            (267, 0.25),      // armor EM resonance
            (270, 0.5),
            (269, 0.5),
            (268, 0.5),
            (113, 0.1),       // hull EM resonance
            (110, 0.2),
            (109, 0.3),
            (111, 0.4),
            (3356, 500.0),    // ATTR_PAUSE_HULL_REPAIR_DPS
        ],
    );
    clone_type_with(&mut ctx.sde, base_tid, non_struct_tid, 18, 4, &[]);

    let sys_id = *ctx.sde.solar_systems.keys().next().unwrap_or(&30_000_142);
    let now = 1_750_000_000_u64;
    let mut ext = StructureExtendedState::default();

    // 1. StructureNotFound, InvalidStructureType, StructureAlreadyDestroyed, StructureReinforcedInvulnerable
    let err_missing = ctx.db.with_mut_snapshot(|snap| {
        apply_structure_damage(&ctx, snap, &mut ext, 888_777_666, 1_000.0, 10.0, 90_000_001, now)
    });
    assert_eq!(err_missing.unwrap_err().code, "StructureNotFound");

    let err_typed_missing = ctx.db.with_mut_snapshot(|snap| {
        apply_structure_damage_typed(
            &ctx,
            snap,
            &mut ext,
            888_777_666,
            100.0,
            100.0,
            100.0,
            100.0,
            10.0,
            90_000_001,
            now,
        )
    });
    assert_eq!(err_typed_missing.unwrap_err().code, "StructureNotFound");

    let sid_bad_type = 888_001_001_i64;
    let sid_destroyed = 888_001_002_i64;
    let sid_reinf = 888_001_003_i64;
    let sid_anchor = 888_001_004_i64;
    let sid_low_power = 888_001_005_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            sid_bad_type,
            make_record(sid_bad_type, non_struct_tid, 98_000_001, sys_id, STATE_SHIELD_VULNERABLE, true, 0),
        );
        snap.structures.insert(
            sid_destroyed,
            make_record(sid_destroyed, astra_tid, 98_000_001, sys_id, STATE_DESTROYED, true, 0),
        );
        let mut r_st = make_record(sid_reinf, astra_tid, 98_000_001, sys_id, STATE_ARMOR_REINFORCED, true, 0);
        r_st.reinforced_until_unix = now + 3_600;
        snap.structures.insert(sid_reinf, r_st);

        let mut anc = make_record(sid_anchor, astra_tid, 98_000_001, sys_id, STATE_ANCHOR_VULNERABLE, true, 0);
        anc.reinforced_until_unix = now + 900;
        snap.structures.insert(sid_anchor, anc);

        let mut lp = make_record(sid_low_power, astra_tid, 98_000_001, sys_id, STATE_SHIELD_VULNERABLE, true, 0);
        lp.shield_hp = 5_000.0;
        lp.armor_hp = 5_000.0;
        lp.hull_hp = 20_000.0;
        snap.structures.insert(sid_low_power, lp);
        ext.unanchoring_until_unix.insert(sid_low_power, now + 86_400);
        ext.lifecycle.entry(sid_low_power).or_default().low_power_since_unix =
            Some(now - super::ABANDON_AFTER_LOW_POWER_SECS - 10);
    });

    assert_eq!(
        ctx.db
            .with_mut_snapshot(|snap| apply_structure_damage(&ctx, snap, &mut ext, sid_bad_type, 1_000.0, 10.0, 90_000_001, now))
            .unwrap_err()
            .code,
        "InvalidStructureType"
    );
    assert_eq!(
        ctx.db
            .with_mut_snapshot(|snap| apply_structure_damage(&ctx, snap, &mut ext, sid_destroyed, 1_000.0, 10.0, 90_000_001, now))
            .unwrap_err()
            .code,
        "StructureAlreadyDestroyed"
    );
    assert_eq!(
        ctx.db
            .with_mut_snapshot(|snap| apply_structure_damage(&ctx, snap, &mut ext, sid_reinf, 1_000.0, 10.0, 90_000_001, now))
            .unwrap_err()
            .code,
        "StructureReinforcedInvulnerable"
    );

    // 2. AnchorVulnerable: sub-threshold fire does NOT pause anchor timer; qualifying fire DOES pause anchor timer
    let sub_out = ctx
        .db
        .with_mut_snapshot(|snap| apply_structure_damage(&ctx, snap, &mut ext, sid_anchor, 10.0, 10.0, 90_000_001, now))
        .unwrap();
    assert!(!sub_out.repair_paused);
    assert_eq!(sub_out.reinforced_until_unix, now + 900);
    assert!(ext.lifecycle.get(&sid_anchor).unwrap().repair_until_unix.is_none());

    let qual_out = ctx
        .db
        .with_mut_snapshot(|snap| apply_structure_damage(&ctx, snap, &mut ext, sid_anchor, 50_000.0, 10.0, 90_000_001, now))
        .unwrap();
    assert!(qual_out.repair_paused);
    assert_eq!(qual_out.reinforced_until_unix, now + 910);

    // 3. LowPower structure: typed EM damage uses shield EM resonance (0.5) & negative channels clamped to 0.0;
    // depleting shield skips ArmorReinforced directly into ArmorVulnerable and cancels unanchoring;
    // depleting armor skips HullReinforced directly into HullVulnerable!
    let out1 = ctx
        .db
        .with_mut_snapshot(|snap| {
            apply_structure_damage_typed(
                &ctx,
                snap,
                &mut ext,
                sid_low_power,
                100_000.0, // * 0.5 = 50_000 -> exceeds remaining 5_000 shield
                -500.0,    // clamped to 0
                0.0,
                0.0,
                10.0,
                90_000_001,
                now,
            )
        })
        .unwrap();
    assert_eq!(out1.layer, "shield");
    assert_eq!(out1.state, STATE_ARMOR_VULNERABLE);
    assert_eq!(out1.shield_hp, 0.0);
    assert!(!ext.unanchoring_until_unix.contains_key(&sid_low_power));

    // Deplete armor in LowPower -> transitions directly to HullVulnerable (uses armor EM resonance 0.25)
    let out2 = ctx
        .db
        .with_mut_snapshot(|snap| {
            apply_structure_damage_typed(
                &ctx,
                snap,
                &mut ext,
                sid_low_power,
                100_000.0, // * 0.25 = 25_000 -> exceeds 5_000 armor
                0.0,
                0.0,
                0.0,
                10.0,
                90_000_001,
                now + 10,
            )
        })
        .unwrap();
    assert_eq!(out2.layer, "armor");
    assert_eq!(out2.state, STATE_HULL_VULNERABLE);
    assert_eq!(out2.armor_hp, 0.0);
}

#[test]
fn sov_hub_cleanup_wormhole_loot_drop_online_service_ids_and_home_station_respawn() {
    let mut ctx = test_ctx();
    let base_tid = ctx.sde.types.keys().next().copied().unwrap_or(34);
    let hub_tid = crate::services::sov_services::TYPE_SOVEREIGNTY_HUB;
    let astra_tid = 9_935_833_u32;
    let svc_tid = 9_935_890_u32;
    let rig_tid = 9_935_891_u32;
    let cargo_tid = 9_900_035_u32;

    clone_type_with(
        &mut ctx.sde,
        base_tid,
        hub_tid,
        1657,
        65,
        &[(263, 100_000.0), (265, 100_000.0), (9, 100_000.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        base_tid,
        astra_tid,
        1657,
        65,
        &[(263, 100_000.0), (265, 100_000.0), (9, 100_000.0)],
    );
    clone_type_with(&mut ctx.sde, base_tid, svc_tid, 1321, 66, &[(2109, 5.0)]);
    clone_type_with(&mut ctx.sde, base_tid, rig_tid, 1308, 66, &[]);
    clone_type_with(&mut ctx.sde, base_tid, cargo_tid, 18, 4, &[]);

    let kspace_sys = *ctx.sde.solar_systems.keys().next().unwrap_or(&30_000_142);
    // Add a wormhole system (class 3, ID in 31_000_000..32_000_000) to SDE
    let wh_sys = 31_000_777_u32;
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        let mut sys = reg.solar_systems.get(&kspace_sys).unwrap().clone();
        sys.solar_system_id = wh_sys;
        sys.region_id = 11_000_001;
        sys.security_status = -1.0;
        reg.solar_systems.insert(wh_sys, sys);
    }

    let now = 1_750_100_000_u64;
    let hub_sid = 889_001_001_i64;
    let wh_sid = 889_001_002_i64;
    let svc_online_iid = 889_010_001_i64;
    let svc_offline_iid = 889_010_002_i64;
    let wh_rig_iid = 889_010_003_i64;
    let wh_hangar_iid = 889_010_004_i64;
    let char_id = 90_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // 1. Sov hub + runtime metadata
        snap.structures.insert(
            hub_sid,
            make_record(hub_sid, hub_tid, 98_000_001, kspace_sys, STATE_SHIELD_VULNERABLE, true, 100),
        );
        snap.sov_runtime.sov_hubs.insert(hub_sid, Default::default());
        snap.sov_runtime.sov_claimed_since_unix.insert(kspace_sys, now);
        snap.system_states.entry(kspace_sys).or_default().sov_alliance_id = Some(99_000_001);
        snap.system_states.entry(kspace_sys).or_default().strategic_index = 3;

        // 2. Service modules (1 online, 1 offline) on hub_sid
        snap.items.insert(
            svc_online_iid,
            InventoryItemRecord {
                item_id: svc_online_iid,
                type_id: svc_tid,
                owner_id: 98_000_001,
                location_id: hub_sid,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            svc_offline_iid,
            InventoryItemRecord {
                item_id: svc_offline_iid,
                type_id: svc_tid,
                owner_id: 98_000_001,
                location_id: hub_sid,
                flag: InventoryFlag::ServiceSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_online_iid, true);
        snap.module_online_states.insert(svc_offline_iid, false);

        // 3. Wormhole structure with rig + hangar item
        snap.structures.insert(
            wh_sid,
            make_record(wh_sid, astra_tid, 98_000_001, wh_sys, STATE_HULL_VULNERABLE, true, 100),
        );
        snap.items.insert(
            wh_rig_iid,
            InventoryItemRecord {
                item_id: wh_rig_iid,
                type_id: rig_tid,
                owner_id: 98_000_001,
                location_id: wh_sid,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            wh_hangar_iid,
            InventoryItemRecord {
                item_id: wh_hangar_iid,
                type_id: cargo_tid,
                owner_id: char_id,
                location_id: wh_sid,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        // Set character home station to hub_sid
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.home_station_id = hub_sid;
        }
    });

    // Verify online_service_type_ids returns only the online service slot
    let online_tids = ctx
        .db
        .with_snapshot(|snap| online_service_type_ids(&ctx, snap, hub_sid));
    assert_eq!(online_tids, vec![svc_tid]);
    assert!(tether_range_m(&ctx, hub_tid) >= 10_000.0);

    // Destroy sov hub -> sov_runtime entries cleaned up
    let mut ext = ctx.db.with_snapshot(load_ext_state);
    let hub_out = ctx
        .db
        .with_mut_snapshot(|snap| destroy_structure(&ctx, snap, &mut ext, hub_sid, 90_000_002, now));
    assert!(hub_out.killmail_id.is_some());
    ctx.db.with_mut_snapshot(|snap| save_ext_state(snap, &ext));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.sov_hubs.contains_key(&hub_sid));
        assert!(!snap.sov_runtime.sov_claimed_since_unix.contains_key(&kspace_sys));
        let sys_st = snap.system_states.get(&kspace_sys).unwrap();
        assert_eq!(sys_st.sov_alliance_id, None);
        assert_eq!(sys_st.strategic_index, 0);
    });

    // prepare_home_station_respawn resets char_id's home station away from destroyed hub_sid
    prepare_home_station_respawn(&ctx, char_id);
    let new_home = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.home_station_id));
    assert!(new_home.is_some());
    assert_ne!(new_home, Some(hub_sid));

    // Destroy wormhole structure -> no asset safety wraps; hangar item drops in space, rig destroyed
    assert_eq!(
        ctx.db
            .with_snapshot(|snap| space_class(&ctx, snap.structures.get(&wh_sid).unwrap().solar_system_id)),
        SpaceClass::Wormhole
    );
    let wh_out = ctx.db.with_mut_snapshot(|snap| {
        let mut ext_local = load_ext_state(snap);
        let res = destroy_structure(&ctx, snap, &mut ext_local, wh_sid, 90_000_002, now);
        save_ext_state(snap, &ext_local);
        res
    });
    assert!(wh_out.asset_safety_wrap_ids.is_empty());
    assert!(wh_out.destroyed_item_ids.contains(&wh_rig_iid));
    assert!(wh_out.dropped_item_ids.contains(&wh_hangar_iid));
    ctx.db.with_snapshot(|snap| {
        let dropped = snap.items.get(&wh_hangar_iid).unwrap();
        assert_eq!(dropped.location_id, i64::from(wh_sys));
        assert_eq!(dropped.flag, InventoryFlag::None);
    });

    // Also test sync_structure_with_released for no-op on unknown structure
    let mut released_out = Vec::new();
    let none_res = ctx.db.with_mut_snapshot(|snap| {
        let mut ext_local = StructureExtendedState::default();
        ext_local.lifecycle.insert(999_999, StructureLifecycleState::default());
        sync_structure_with_released(&ctx, snap, &mut ext_local, 999_999, now, &mut released_out)
    });
    assert!(none_res.is_none());
    assert!(released_out.is_empty());
}
