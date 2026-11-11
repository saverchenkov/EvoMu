use super::*;
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, PersistedInsurgencyCampaign,
    SystemDynamicState, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{SdeRegistry, SolarSystem};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn add_test_type(
    sde: &mut SdeRegistry,
    type_id: u32,
    group_id: u32,
    cat_id: u32,
    name: &str,
    faction: Option<u32>,
    shield: f64,
) {
    let mut t = sde.types.values().next().unwrap().clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = cat_id;
    t.name = name.into();
    t.published = true;
    t.faction_id = faction;
    if shield > 0.0 {
        t.default_attributes
            .insert(eve_sde::attr_ids::SHIELD_CAPACITY, shield);
    }
    sde.types.insert(type_id, t);
}

fn build_insurgency_test_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    for (sid, name, pos) in [
        (30_002_056_u32, "Turnur", [1.0, 0.0, 0.0]),
        (30_002_811_u32, "Odamia", [2.0, 0.0, 0.0]),
        (30_002_974_u32, "Mehatoor", [3.0, 0.0, 0.0]),
        (30_045_324_u32, "Onnamon", [4.0, 0.0, 0.0]),
        (30_003_788_u32, "Intaki", [5.0, 0.0, 0.0]),
        (30_002_055_u32, "Amo", [6.0, 0.0, 0.0]),
    ] {
        sde.solar_systems.entry(sid).or_insert_with(|| SolarSystem {
            solar_system_id: sid,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            name: name.into(),
            security_status: 0.3,
            radius: 1.0e13,
            galactic_pos_ly: pos,
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        });
    }
    add_test_type(&mut sde, 587, 25, 6, "Rifter", None, 400.0);
    add_test_type(&mut sde, 16227, 419, 6, "Ferox", None, 2500.0);
    add_test_type(&mut sde, 16229, 1201, 6, "Talos", None, 2000.0);
    add_test_type(&mut sde, 671, 30, 6, "Erebus", None, 100_000.0);
    add_test_type(&mut sde, 670, 29, 6, "Capsule", None, 100.0);
    add_test_type(&mut sde, 10998, 315, 7, "Warp Core Stabilizer I", None, 0.0);
    add_test_type(&mut sde, 79_001, 551, 11, "Angel Defender Frigate", Some(500_011), 500.0);
    add_test_type(&mut sde, 79_002, 553, 11, "Guristas Defender Cruiser", Some(500_010), 1500.0);
    add_test_type(&mut sde, 79_003, 554, 11, "Angel Defender BC", Some(500_011), 2500.0);
    add_test_type(&mut sde, 79_004, 550, 11, "Guristas Defender BS", Some(500_010), 4000.0);
    // Named/regular roamers used by select_named_or_regular_roamer_type
    for &tid in &[79_329_u32, 79_330, 79_332, 79_334] {
        add_test_type(&mut sde, tid, 4637, 11, "Angel Commander", Some(500_011), 800.0);
    }
    for &tid in &[79_358_u32, 79_360, 79_362, 79_364] {
        add_test_type(&mut sde, tid, 4637, 11, "Guristas Commander", Some(500_010), 800.0);
    }
    add_test_type(&mut sde, 79_350, 4637, 11, "Regular Angel Roamer", Some(500_011), 400.0);
    for &tid in &[79_389_u32, 79_394, 79_395, 79_396] {
        add_test_type(&mut sde, tid, 4638, 11, "Empire Commander", Some(500_001), 900.0);
    }
    for tid in [78_867_u32, 78_872, 78_873, 78_874, 79_117, 79_118] {
        add_test_type(&mut sde, tid, 4748, 65, "Insurgency Crate", None, 0.0);
    }

    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1.0e6,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn seed_pilot(
    ctx: &ServerContext,
    cid: i64,
    sys_id: u32,
    wf: Option<i64>,
    ship_id: i64,
    ship_type: u32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.name = format!("Pilot {cid}");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.war_faction_id = wf;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents(0);
        ch.loyalty_points.clear();
        snap.characters.insert(cid, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type,
                owner_id: cid,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
    });
}

fn seed_campaign(
    ctx: &ServerContext,
    cid: i64,
    pirate_fac: i64,
    target_fac: i64,
    sys_id: u32,
    now: u64,
    c_stage: u8,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            cid,
            PersistedInsurgencyCampaign {
                campaign_id: cid,
                pirate_faction_id: pirate_fac,
                target_faction_id: target_fac,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now.saturating_sub(1000),
                forecasting_end_unix: now.saturating_sub(500),
                active_end_unix: now + 86_400,
                winddown_end_unix: now + 100_000,
                corruption_points: HashMap::from([(sys_id, i64::from(c_stage) * 200)]),
                suppression_points: HashMap::new(),
            },
        );
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                insurgency_corruption_stage: c_stage,
                insurgency_suppression_stage: 0,
                ..SystemDynamicState::default()
            },
        );
    });
}

fn save_ops_to_ctx(ctx: &ServerContext, st: &InsurgencyComplexOpsState) {
    ctx.db.with_mut_snapshot(|snap| save_ops_state(snap, st));
}

fn load_ops_from_ctx(ctx: &ServerContext) -> InsurgencyComplexOpsState {
    ctx.db.with_snapshot(|snap| load_ops_state(snap))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helpers_roamers_loot_laundering_and_pocket_dbuffs_cover_all_variants() {
    let ctx = build_insurgency_test_ctx();
    let sys_id = 30_002_056;
    let now = ctx.now_unix();

    // 1. OutpostRaidSize & is_warzone_opposing_empire & is_battlecruiser_group
    for size in OutpostRaidSize::ALL {
        assert!(size.slot_index() >= 1);
        assert!(size.beacon_type_id() > 0);
        assert!(size.base_capture_duration_secs() >= 600);
        assert!(size.contribution_percent() > 0.0);
        assert!(size.contribution_subpoints() > 0);
        assert!(size.base_lp_reward() > 0);
        assert!(size.respawn_delay_secs() > 0);
        let _ = size.mordus_defender_type_id();
        let _ = size.pirate_defender_type_id(500_010);
        let _ = size.pirate_defender_type_id(500_011);
    }
    assert!(OutpostRaidSize::Small.allows_ship_group(25));
    assert!(!OutpostRaidSize::Small.allows_ship_group(419));
    assert!(OutpostRaidSize::Moderate.allows_ship_group(419));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(27));

    assert!(is_warzone_opposing_empire(500_010, 500_001));
    assert!(is_warzone_opposing_empire(500_010, 500_004));
    assert!(!is_warzone_opposing_empire(500_010, 500_002));
    assert!(is_warzone_opposing_empire(500_011, 500_002));
    assert!(is_warzone_opposing_empire(500_011, 500_003));
    assert!(!is_warzone_opposing_empire(500_011, 500_001));
    assert!(!is_warzone_opposing_empire(500_001, 500_002));
    assert!(is_battlecruiser_group(419));
    assert!(is_battlecruiser_group(1201));
    assert!(!is_battlecruiser_group(25));

    // 2. is_warp_into_outpost_raid_pocket_blocked & active_insurgency_pocket_dbuffs
    let mod_pocket = OutpostRaidSize::Moderate.pocket_position();
    let outside = DVec3::new(0.0, 0.0, 0.0);
    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        sys_id,
        outside.to_array(),
        mod_pocket.to_array(),
        mod_pocket.to_array(),
    ));

    let cid = 94_000_001;
    let ship_id = 94_100_001;
    let mut st = InsurgencyComplexOpsState::default();
    st.outpost_raids.insert(
        44_001,
        OutpostRaidInstance {
            gate_id: 44_001,
            beacon_id: 44_002,
            campaign_id: 1,
            solar_system_id: sys_id,
            pirate_faction_id: 500_011,
            target_faction_id: 500_002,
            size: OutpostRaidSize::Moderate,
            is_static: false,
            occupants: vec![(cid, ship_id)],
            mordus_defender_ball_id: None,
            pirate_defender_ball_id: None,
            mordus_defender_cleared: false,
            pirate_defender_cleared: false,
            weighted_elapsed_secs: 0.0,
            last_step_unix: now,
            completed_on_cooldown: false,
            respawn_at_unix: 0,
        },
    );
    st.outpost_raids.insert(
        44_003,
        OutpostRaidInstance {
            gate_id: 44_003,
            beacon_id: 44_004,
            campaign_id: 1,
            solar_system_id: sys_id,
            pirate_faction_id: 500_011,
            target_faction_id: 500_002,
            size: OutpostRaidSize::Small,
            is_static: false,
            occupants: vec![],
            mordus_defender_ball_id: None,
            pirate_defender_ball_id: None,
            mordus_defender_cleared: false,
            pirate_defender_cleared: false,
            weighted_elapsed_secs: 0.0,
            last_step_unix: now,
            completed_on_cooldown: true,
            respawn_at_unix: now + 600,
        },
    );
    save_ops_to_ctx(&ctx, &st);
    assert!(ctx.db.with_snapshot(|s| is_outpost_raid_gate(s, 44_001)));
    assert!(!ctx.db.with_snapshot(|s| is_outpost_raid_gate(s, 999_999)));

    // From outside -> pocket anchor blocked; from inside pocket -> allowed; cooldown Small -> allowed; other sys -> allowed
    assert!(is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        sys_id,
        outside.to_array(),
        mod_pocket.to_array(),
        mod_pocket.to_array(),
    ));
    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        sys_id,
        mod_pocket.to_array(),
        mod_pocket.to_array(),
        mod_pocket.to_array(),
    ));
    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        sys_id,
        outside.to_array(),
        OutpostRaidSize::Small.pocket_position().to_array(),
        OutpostRaidSize::Small.pocket_position().to_array(),
    ));
    assert!(!is_warp_into_outpost_raid_pocket_blocked(
        &ctx,
        30_002_811,
        outside.to_array(),
        mod_pocket.to_array(),
        mod_pocket.to_array(),
    ));

    // active_insurgency_pocket_dbuffs: non-BC -> empty; BC occupant inside -> dbuff; BC outside -> empty
    assert!(active_insurgency_pocket_dbuffs(&ctx, cid, ship_id, 587).is_empty());
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions
            .insert(ship_id, (mod_pocket + DVec3::new(100.0, 0.0, 0.0)).to_array());
    });
    let dbuffs = active_insurgency_pocket_dbuffs(&ctx, cid, ship_id, 16227);
    assert_eq!(dbuffs.len(), 1);
    assert_eq!(dbuffs[0].dbuff_id, Some(MODERATE_OUTPOST_BC_SKILL_DBUFF_ID));

    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, outside.to_array());
    });
    assert!(active_insurgency_pocket_dbuffs(&ctx, cid, ship_id, 16227).is_empty());

    // 3. Roamers & Loot
    seed_campaign(&ctx, 1, 500_011, 500_002, sys_id, now, 3);
    assert!(select_named_or_regular_roamer_type(&ctx, sys_id, 4637, 500_011, 1, 0).is_none());
    let mut ops = load_ops_from_ctx(&ctx);
    ops.named_roamer_rolls.insert(format!("{sys_id}:4637"), true);
    ops.named_roamer_rolls.insert(format!("{sys_id}:4638"), true);
    save_ops_to_ctx(&ctx, &ops);
    assert!(select_named_or_regular_roamer_type(&ctx, sys_id, 4637, 500_011, 3, 0).is_some());
    assert!(select_named_or_regular_roamer_type(&ctx, sys_id, 4637, 500_010, 3, 0).is_some());
    assert!(select_named_or_regular_roamer_type(&ctx, sys_id, 4637, 500_011, 2, 0).is_some());
    assert!(select_named_or_regular_roamer_type(&ctx, sys_id, 4638, 500_011, 0, 3).is_some());
    assert!(select_named_or_regular_roamer_type(&ctx, sys_id, 999999, 500_010, 3, 3).is_none());

    let mut loot: Vec<(u32, i32, bool)> = Vec::new();
    append_insurgency_npc_wreck_loot(79_329, &mut loot);
    append_insurgency_npc_wreck_loot(79_358, &mut loot);
    append_insurgency_npc_wreck_loot(79_389, &mut loot);
    append_insurgency_npc_wreck_loot(TYPE_ORE_RORQUAL, &mut loot);
    append_insurgency_npc_wreck_loot(1, &mut loot);
    assert!(loot.iter().any(|&(tid, _, _)| tid == 78_865));
    assert!(loot.iter().any(|&(tid, _, _)| tid == 78_859));
    assert!(loot.iter().any(|&(tid, qty, _)| tid == 17_867 && qty == 5));

    // 4. Laundering across all 4 Empire CI Outposts + Pirate FOB facilities
    seed_campaign(&ctx, 2, 500_010, 500_001, 30_002_811, now, 2);
    let c_launder = 94_001_001;
    seed_pilot(&ctx, c_launder, sys_id, None, 94_101_001, 587);
    assert!(
        check_insurgency_laundering_access(
            &ctx,
            c_launder,
            HQ_SYSTEM_INTAKI,
            TYPE_FEDERATION_CI_OUTPOST_HUB
        )
        .is_err()
    );
    on_insurgency_laundering_trade_completed(
        &ctx,
        c_launder,
        HQ_SYSTEM_INTAKI,
        TYPE_FEDERATION_CI_OUTPOST_HUB,
        2,
    );

    for (wf, hq_sys, trader) in [
        (500_004, HQ_SYSTEM_INTAKI, TYPE_FEDERATION_CI_OUTPOST_HUB),
        (500_003, HQ_SYSTEM_MEHATOOR, TYPE_IMPERIAL_CI_OUTPOST_HUB),
        (500_001, HQ_SYSTEM_ONNAMON, TYPE_STATE_CI_OUTPOST_HUB),
        (500_002, HQ_SYSTEM_AMO, TYPE_REPUBLIC_CI_OUTPOST_HUB),
        (500_011, sys_id, TYPE_ANGEL_LAUNDERING_FACILITY),
        (500_010, 30_002_811, TYPE_GURISTAS_LAUNDERING_FACILITY),
    ] {
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&c_launder).unwrap().war_faction_id = Some(wf));
        assert!(check_insurgency_laundering_access(&ctx, c_launder, hq_sys, trader).is_ok());
        on_insurgency_laundering_trade_completed(&ctx, c_launder, hq_sys, trader, 1);
    }
    assert!(check_insurgency_laundering_access(&ctx, c_launder, sys_id, 999_999).is_ok());
    on_insurgency_laundering_trade_completed(&ctx, c_launder, sys_id, 999_999, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_outpost_raid_gate_covers_errors_reentry_and_moderate_bc_fit_invalidation() {
    let ctx = build_insurgency_test_ctx();
    let sys_id = 30_002_056;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let gate_id = 45_001_i64;
    let beacon_id = 45_002_i64;
    let cid = 94_002_001;
    let ship_id = 94_102_001;
    let gate_pos = OutpostRaidSize::Moderate.gate_position();

    seed_pilot(&ctx, cid, sys_id, None, ship_id, 16227);
    assert!(matches!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id).await,
        Err(ref e) if e.code == "GateNotFound"
    ));

    let mut st = InsurgencyComplexOpsState::default();
    st.outpost_raids.insert(
        gate_id,
        OutpostRaidInstance {
            gate_id,
            beacon_id,
            campaign_id: 1,
            solar_system_id: sys_id,
            pirate_faction_id: 500_011,
            target_faction_id: 500_002,
            size: OutpostRaidSize::Moderate,
            is_static: false,
            occupants: vec![],
            mordus_defender_ball_id: None,
            pirate_defender_ball_id: Some(45_099),
            mordus_defender_cleared: false,
            pirate_defender_cleared: false,
            weighted_elapsed_secs: 0.0,
            last_step_unix: ctx.now_unix(),
            completed_on_cooldown: false,
            respawn_at_unix: 0,
        },
    );
    save_ops_to_ctx(&ctx, &st);

    // 2. Faction not aligned
    assert!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .is_err()
    );

    // Set valid militia faction (Angel 500_011)
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&cid).unwrap().war_faction_id = Some(500_011));

    // 3. WarpCoreStabilizerRestricted
    let wcs_id = 94_202_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wcs_id,
            InventoryItemRecord {
                item_id: wcs_id,
                type_id: 10998,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
    });
    assert!(matches!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id).await,
        Err(ref e) if e.code == "WarpCoreStabilizerRestricted"
    ));
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&wcs_id);
    });

    // 4. Ship restrictions (capsule 670, Titan 671)
    for bad_ship in [670_u32, 671] {
        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = bad_ship);
        assert!(
            activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
                .await
                .is_err()
        );
    }
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 16227);

    // 5. ShipNotInSpace
    assert!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .is_err()
    );

    // 6. TooFarFromGate
    let _ = sys_handle
        .spawn_ball(make_ball(
            ship_id,
            16227,
            gate_pos + DVec3::new(20_000.0, 0.0, 0.0),
        ))
        .await;
    assert!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .is_err()
    );

    // 7. Success for Pirate in Moderate BC
    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle
        .spawn_ball(make_ball(
            ship_id,
            16227,
            gate_pos + DVec3::new(100.0, 0.0, 0.0),
        ))
        .await;
    let fit = ctx.get_or_rebuild_ship_fitting(cid, ship_id).unwrap();
    ctx.store_ship_fitting(ship_id, fit);

    assert!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .is_ok()
    );
    assert!(
        !ctx.ship_fittings.read().unwrap().contains_key(&ship_id),
        "Moderate BC fitting invalidated on entry"
    );

    // Move ship back to gate for re-entry branch
    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle
        .spawn_ball(make_ball(
            ship_id,
            16227,
            gate_pos + DVec3::new(100.0, 0.0, 0.0),
        ))
        .await;
    assert!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .is_ok()
    );

    // Empire pilot entry (500_002)
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&cid).unwrap().war_faction_id = Some(500_002));
    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle
        .spawn_ball(make_ball(
            ship_id,
            16227,
            gate_pos + DVec3::new(100.0, 0.0, 0.0),
        ))
        .await;
    assert!(
        activate_outpost_raid_gate_in_space(&ctx, cid, ship_id, sys_id, gate_id)
            .await
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_npc_destroyed_and_tick_cover_mining_ambush_outpost_capture_and_cleanup() {
    let ctx = build_insurgency_test_ctx();
    let sys_angel = 30_002_056;
    let sys_gur = 30_002_811;
    let now = ctx.now_unix();

    seed_campaign(&ctx, 10, 500_011, 500_002, sys_angel, now, 2);
    seed_campaign(&ctx, 11, 500_010, 500_001, sys_gur, now, 0);

    sync_insurgency_complex_sites_and_facilities(&ctx, now).await;
    let ops = load_ops_from_ctx(&ctx);
    assert!(ops.outpost_raids.values().any(|r| r.solar_system_id == sys_angel));
    assert!(ops.outpost_raids.values().any(|r| r.solar_system_id == sys_gur));
    assert!(ops.mining_ambushes.values().any(|a| a.solar_system_id == sys_angel));
    assert!(!ops.mining_ambushes.values().any(|a| a.solar_system_id == sys_gur));

    // Drop Angel corruption to 0 -> sync despawns Mining Ambush
    ctx.db.with_mut_snapshot(|s| {
        s.system_states
            .get_mut(&sys_angel)
            .unwrap()
            .insurgency_corruption_stage = 0;
    });
    sync_insurgency_complex_sites_and_facilities(&ctx, now + 5).await;
    assert!(load_ops_from_ctx(&ctx).mining_ambushes.is_empty());

    let c_pirate = 94_003_001;
    let c_empire = 94_003_002;
    let sh_pirate = 94_103_001;
    let sh_empire = 94_103_002;
    seed_pilot(&ctx, c_pirate, sys_angel, Some(500_011), sh_pirate, 16227);
    seed_pilot(&ctx, c_empire, sys_angel, Some(500_002), sh_empire, 587);

    let sys_h = ctx.systems.get_or_spawn(sys_angel);
    let pocket = OutpostRaidSize::Moderate.pocket_position();
    let _ = sys_h
        .spawn_ball(make_ball(sh_empire, 587, pocket + DVec3::new(1000.0, 0.0, 0.0)))
        .await;
    let _ = sys_h
        .spawn_ball(make_ball(sh_pirate, 16227, pocket + DVec3::new(1000.0, 0.0, 0.0)))
        .await;

    for bid in [51_001_i64, 51_002, 51_003, 51_004, 51_101, 51_102, 51_103, 51_104, 52_001, 52_002] {
        let _ = sys_h.spawn_ball(make_ball(bid, 79_001, pocket)).await;
    }

    let mut custom = load_ops_from_ctx(&ctx);
    custom
        .named_roamer_rolls
        .insert(format!("{sys_angel}:4637"), true);
    custom.mining_ambushes.insert(
        sys_angel,
        MiningAmbushInstance {
            campaign_id: 10,
            solar_system_id: sys_angel,
            pirate_faction_id: 500_011,
            target_faction_id: 500_002,
            beacon_id: 51_001,
            rorqual_ball_id: 51_002,
            pirate_tackler_ids: vec![51_004],
            ore_defender_ids: vec![51_003],
            ransom_elapsed_secs: 0,
            last_step_unix: now,
            resolved: false,
            rorqual_warping_off_until: None,
            cooldown_until_unix: 0,
        },
    );
    custom.mining_ambushes.insert(
        sys_gur,
        MiningAmbushInstance {
            campaign_id: 11,
            solar_system_id: sys_gur,
            pirate_faction_id: 500_010,
            target_faction_id: 500_001,
            beacon_id: 51_101,
            rorqual_ball_id: 51_102,
            pirate_tackler_ids: vec![51_104],
            ore_defender_ids: vec![51_103],
            ransom_elapsed_secs: 0,
            last_step_unix: now,
            resolved: false,
            rorqual_warping_off_until: None,
            cooldown_until_unix: 0,
        },
    );
    let outpost_gate = *custom.outpost_raids.keys().next().unwrap();
    if let Some(or) = custom.outpost_raids.get_mut(&outpost_gate) {
        or.pirate_defender_ball_id = Some(52_001);
        or.mordus_defender_ball_id = Some(52_002);
    }
    save_ops_to_ctx(&ctx, &custom);

    // Roamer destroyed -> clears named_roamer_rolls for group 4637
    on_insurgency_npc_destroyed(&ctx, sys_angel, 59_001, 79_329, 0, c_empire).await;
    assert!(!load_ops_from_ctx(&ctx)
        .named_roamer_rolls
        .contains_key(&format!("{sys_angel}:4637")));

    // Outpost pirate & Mordu defenders destroyed
    on_insurgency_npc_destroyed(&ctx, sys_angel, 52_001, 79_001, 0, c_empire).await;
    on_insurgency_npc_destroyed(&ctx, sys_angel, 52_002, 79_003, 0, c_pirate).await;
    assert!(load_ops_from_ctx(&ctx).outpost_raids[&outpost_gate]
        .pirate_defender_ball_id
        .is_none());
    assert!(load_ops_from_ctx(&ctx).outpost_raids[&outpost_gate]
        .mordus_defender_ball_id
        .is_none());

    // Mining Ambush 1: ORE defender destroyed, then last pirate tackler destroyed -> resolved + warping off
    on_insurgency_npc_destroyed(&ctx, sys_angel, 51_003, 79_001, 0, c_pirate).await;
    on_insurgency_npc_destroyed(&ctx, sys_angel, 51_004, 79_001, 0, c_empire).await;
    let amb1 = load_ops_from_ctx(&ctx).mining_ambushes[&sys_angel].clone();
    assert!(amb1.resolved);
    assert!(amb1.rorqual_warping_off_until.is_some());

    // Mining Ambush 2: Rorqual destroyed -> resolved
    on_insurgency_npc_destroyed(&ctx, sys_gur, 51_102, TYPE_ORE_RORQUAL, 0, c_pirate).await;
    assert!(load_ops_from_ctx(&ctx).mining_ambushes[&sys_gur].resolved);

    // 3. tick_insurgency_complex_ops with active raid occupant
    let mut tick_st = load_ops_from_ctx(&ctx);
    if let Some(or) = tick_st
        .outpost_raids
        .values_mut()
        .find(|r| r.solar_system_id == sys_angel)
    {
        or.size = OutpostRaidSize::Moderate;
        or.occupants = vec![(c_pirate, sh_pirate), (999_999, 999_999)];
        or.pirate_defender_ball_id = Some(54_999);
        or.mordus_defender_cleared = true;
    }
    save_ops_to_ctx(&ctx, &tick_st);

    tick_insurgency_complex_ops(&ctx, now + 10).await;

    // 4. cleanup_orphan_insurgency_complex_ops removes raids and ambushes for orphan systems
    cleanup_orphan_insurgency_complex_ops(&ctx, &[sys_angel, sys_gur]).await;
    let cleaned = load_ops_from_ctx(&ctx);
    assert!(cleaned
        .outpost_raids
        .values()
        .all(|r| r.solar_system_id != sys_angel && r.solar_system_id != sys_gur));
    assert!(cleaned.mining_ambushes.is_empty());
}
