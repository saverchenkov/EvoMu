//! Extra unit and mutation-killing tests for `services::pve_incursion_sites`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_incursion_lifecycle::{
    eligible_incursion_constellations, INCURSION_EFFECT_BEACON_ASSAULT,
    INCURSION_EFFECT_BEACON_HQ, INCURSION_EFFECT_BEACON_VANGUARD,
};
use crate::services::pve_incursion_sites::*;
use crate::services::pve_special_sites::with_pve_special_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone()
}

fn build_test_ctx() -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db))
}

fn insert_pilot(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    fleet_id: Option<i64>,
    pos: DVec3,
) -> i64 {
    let sys_rec = ctx.sde.solar_systems.get(&sys_id).unwrap();
    let ship_id = 600_000 + char_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            CharacterRecord {
                activity_status: None,
                character_id: char_id,
                user_id: char_id as u64,
                name: format!("IncExtra-{char_id}"),
                corporation_id: 1_000_006,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: sys_rec.constellation_id,
                region_id: sys_rec.region_id,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(1_000_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 1.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 11,
                school_id: 17,
                neural: NeuralAttributes::default(),
                skill_points: 10_000_000.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1_700_000_000,
                fleet_id,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            },
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    ship_id
}

#[test]
fn site_ball_serde_defaults_all_four_directions_and_snap_effects() {
    let ctx = build_test_ctx();
    let ball = make_site_npc_ball(42, 2192, DVec3::new(10.0, 20.0, 30.0));
    assert_eq!(ball.id, 42);
    assert_eq!(ball.type_id, 2192);
    assert_eq!(ball.max_velocity, 380.0);
    assert_eq!(ball.mass, 4_000_000.0);
    assert_eq!(ball.radius, 220.0);

    // Exercise default_boss_type_id() via serde JSON deserialization when boss_type_id is omitted
    let json = r#"{
        "site_id": "INC-1-5165",
        "dungeon_id": 5165,
        "dungeon_name": "Colony",
        "solar_system_id": 30000142,
        "tier": "Vanguard",
        "min_pilots": 5,
        "max_optimal_pilots": 10,
        "base_isk": 10395000,
        "base_concord_lp": 1400,
        "influence_delta": 0.05,
        "locked_until_low_influence": false,
        "completed_count": 0,
        "boss_npc_ball_id": 999
    }"#;
    let parsed: IncursionSiteEntry = serde_json::from_str(json).expect("deserialize site entry");
    assert_eq!(parsed.boss_type_id, 2933);

    // Exercise all 4 direction branches (site_idx % 4 == 0, 1, 2, 3) in derive_incursion_site_position
    let p0 = derive_incursion_site_position(&ctx.sde, 30_000_142, 0);
    let p1 = derive_incursion_site_position(&ctx.sde, 30_000_142, 1);
    let p2 = derive_incursion_site_position(&ctx.sde, 30_000_142, 2);
    let p3 = derive_incursion_site_position(&ctx.sde, 30_000_142, 3);
    assert_ne!(p0, p2);
    assert_ne!(p1, p3);
    assert_ne!(p2, p3);

    // Seed incursion and verify get_incursion_effects_for_system_in_snap
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let staging_sys = with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.incursions[&cid].staging_solar_system_id
    });
    ctx.db.with_snapshot(|snap| {
        let eff = get_incursion_effects_for_system_in_snap(&ctx.sde, snap, staging_sys).unwrap();
        assert_eq!(eff.constellation_id, cid);
        assert!((eff.influence - 1.0).abs() < 1e-6);
    });
}

#[tokio::test]
async fn staging_payout_solo_ball_offgrid_zero_payout_lockout_expiry_and_beacon_cleanup() {
    let ctx = build_test_ctx();
    let cid = eligible_incursion_constellations(&ctx)[0].0;
    let inc = with_pve_special_reg(&ctx, |reg| {
        reg.initialized = true;
        seed_incursion_for_constellation(&ctx, reg, cid);
        reg.incursions[&cid].clone()
    });
    let stg_site = inc.sites.iter().find(|s| s.tier == "Staging").unwrap().clone();
    let vg_site = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap().clone();
    let ms_site = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap().clone();

    // 1. mark_incursion_boss_destroyed false branches: locked mothership, pending waves, unknown ball
    assert!(!mark_incursion_boss_destroyed(&ctx, ms_site.solar_system_id, ms_site.boss_npc_ball_id));
    assert!(!mark_incursion_boss_destroyed(&ctx, vg_site.solar_system_id, vg_site.boss_npc_ball_id));
    assert!(!mark_incursion_boss_destroyed(&ctx, 0, 999_999_999));

    // 2. Vanguard site error branches: escorts alive vs boss not destroyed vs boss ball alive in space
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves.get_mut(&vg_site.site_id).unwrap().current_wave = 3;
    });
    assert_eq!(
        process_incursion_site_completion_and_payout(
            &ctx,
            Some(92_100_001),
            None,
            vg_site.solar_system_id,
            &vg_site.site_id,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "IncursionSiteNpcsRemaining"
    );

    with_pve_special_reg(&ctx, |reg| {
        let s = reg
            .incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == vg_site.site_id)
            .unwrap();
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
    });
    // Boss not marked destroyed yet -> IncursionSiteNpcsRemaining
    assert_eq!(
        process_incursion_site_completion_and_payout(
            &ctx,
            Some(92_100_001),
            None,
            vg_site.solar_system_id,
            &vg_site.site_id,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "IncursionSiteNpcsRemaining"
    );

    // Wildcard system_id == 0 marks boss destroyed
    assert!(mark_incursion_boss_destroyed(&ctx, 0, vg_site.boss_npc_ball_id));

    // Spawn boss ball in space -> completion blocked because ball is still alive in system!
    let vg_handle = ctx.systems.get_or_spawn(vg_site.solar_system_id);
    vg_handle
        .spawn_ball(make_site_npc_ball(vg_site.boss_npc_ball_id, vg_site.boss_type_id, DVec3::from_array(vg_site.position)))
        .await
        .unwrap();
    assert_eq!(
        process_incursion_site_completion_and_payout(
            &ctx,
            Some(92_100_001),
            None,
            vg_site.solar_system_id,
            &vg_site.site_id,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "IncursionSiteNpcsRemaining"
    );
    let _ = vg_handle.remove_ball(vg_site.boss_npc_ball_id).await;

    // 3. NF-INC-OFFGRID-KILL-01: no verified pilots (caller_char_id = None) -> completes with 0 ISK / 0 LP
    // Also remove wave tracker entry for vg_site so `!has_wave_tracker` (`spawn_boss_now = true`) is exercised!
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves.remove(&vg_site.site_id);
    });
    let zero_res = process_incursion_site_completion_and_payout(
        &ctx,
        None,
        None,
        vg_site.solar_system_id,
        &vg_site.site_id,
        None,
    )
    .await
    .unwrap();
    assert_eq!(zero_res.fleet_size, 0);
    assert_eq!(zero_res.isk_per_pilot, 0);
    assert_eq!(zero_res.concord_lp_per_pilot, 0);

    // 4. Staging site completion by solo pilot (not in fleet) with ship ball in sys_handle and candidate_pilots = Some(&[])
    let solo_pid = 92_100_010i64;
    let stg_pos = DVec3::from_array(stg_site.position);
    let solo_ship = insert_pilot(&ctx, solo_pid, stg_site.solar_system_id, None, stg_pos);
    let stg_handle = ctx.systems.get_or_spawn(stg_site.solar_system_id);
    stg_handle
        .spawn_ball(Ball {
            id: solo_ship,
            type_id: 587,
            position: stg_pos + DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves.get_mut(&stg_site.site_id).unwrap().current_wave = 2;
        let s = reg
            .incursions
            .get_mut(&cid)
            .unwrap()
            .sites
            .iter_mut()
            .find(|s| s.site_id == stg_site.site_id)
            .unwrap();
        for e in &mut s.escort_spawns {
            e.destroyed = true;
        }
        s.boss_destroyed = true;
    });
    let stg_res = process_incursion_site_completion_and_payout(
        &ctx,
        Some(solo_pid),
        None,
        stg_site.solar_system_id,
        &stg_site.site_id,
        Some(&[]),
    )
    .await
    .unwrap();
    assert_eq!(stg_res.tier, "Staging");
    assert_eq!(stg_res.fleet_size, 1);
    assert_eq!(stg_res.fleet_scale_factor, 1.0);
    assert_eq!(stg_res.isk_per_pilot, stg_site.base_isk);

    // 5. Mothership lockout expiry -> sets state = "Withdrawing" and returns MothershipSiteLocked
    with_pve_special_reg(&ctx, |reg| {
        let inc_mut = reg.incursions.get_mut(&cid).unwrap();
        inc_mut.influence = 0.0;
        inc_mut.mothership_spawned = true;
        inc_mut.lockout_until_filetime = Some(100); // expired!
    });
    assert_eq!(
        check_and_complete_incursion_wave_server_side(
            &ctx,
            ms_site.solar_system_id,
            &ms_site.site_id,
            Some(solo_pid),
        )
        .await
        .unwrap_err()
        .code,
        "MothershipSiteLocked"
    );

    // 6. Reset state to Mobilizing, seed effect beacons in snap.items, and complete Mothership when last escort dies after boss!
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, btype, sys) in [
            (770_001i64, INCURSION_EFFECT_BEACON_VANGUARD, vg_site.solar_system_id),
            (770_002i64, INCURSION_EFFECT_BEACON_ASSAULT, inc.assault_solar_system_ids[0]),
            (770_003i64, INCURSION_EFFECT_BEACON_HQ, ms_site.solar_system_id),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: btype,
                    owner_id: 1_000_125,
                    location_id: sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(iid, [0.0, 0.0, 0.0]);
        }
    });
    let last_escort_bid = with_pve_special_reg(&ctx, |reg| {
        let inc_mut = reg.incursions.get_mut(&cid).unwrap();
        inc_mut.state = "Mobilizing".into();
        inc_mut.lockout_until_filetime = None;
        let ms = inc_mut.sites.iter_mut().find(|s| s.site_id == ms_site.site_id).unwrap();
        ms.boss_destroyed = true;
        let len = ms.escort_spawns.len();
        for e in &mut ms.escort_spawns[..len - 1] {
            e.destroyed = true;
        }
        ms.escort_spawns[len - 1].npc_ball_id
    });

    // Unknown ball returns None; killing the final escort when boss_destroyed == true completes the Mothership site!
    assert!(on_incursion_boss_destroyed(&ctx, ms_site.solar_system_id, 999_888_777, Some(solo_pid))
        .await
        .is_none());
    let fin = on_incursion_boss_destroyed(&ctx, ms_site.solar_system_id, last_escort_bid, Some(solo_pid))
        .await
        .unwrap()
        .unwrap();
    assert!(fin.incursion_ended);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&770_001));
        assert!(!snap.items.contains_key(&770_002));
        assert!(!snap.items.contains_key(&770_003));
    });

    // 7. Unauthenticated RPC error branches + unmatched UpdateIncursionLPLog filter -> PyRep::None
    let mut unauth = SessionState::new(10);
    for (svc, m) in [
        ("incursion", "UpdateIncursionLPLog"),
        ("taleMgr", "GetGlobalWorldEventTales"),
        ("taleMgr", "get_active_tales_by_template"),
    ] {
        assert!(handle_incursion_rpc(&ctx, &mut unauth, svc, m, &[], &HashMap::new())
            .await
            .unwrap()
            .is_err());
    }
    let mut auth_sess = SessionState::new(11);
    auth_sess.user_id = Some(solo_pid as u64);
    auth_sess.character_id = Some(solo_pid);
    assert_eq!(
        handle_incursion_rpc(
            &ctx,
            &mut auth_sess,
            "incursion",
            "UpdateIncursionLPLog",
            &[PyRep::String("NON-EXISTENT-SITE".into())],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
}
