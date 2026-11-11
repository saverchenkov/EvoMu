//! Deep branch-coverage and mutation-killing unit tests for `services::space_destiny::space_nav_travel`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_destiny::space_nav_travel::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository,
    WorldStateSnapshot,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

pub(super) fn clone_type_with(
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
    rec.name = format!("NavExtraType_{new_tid}");
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

pub(super) fn put_item(
    snap: &mut WorldStateSnapshot,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton: quantity == 1,
            version: 1,
            ..Default::default()
        },
    );
}

pub(super) fn bound_sys(sys_id: u32, char_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 745,
        user_id: 50,
        character_id: char_id,
        service: "beyonce".to_string(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    }
}

pub(super) fn ship_ball(ship_id: i64, pos: DVec3) -> Ball {
    Ball {
        id: ship_id,
        type_id: 588,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.5,
        mode: MovementMode::Stop,
    }
}

#[test]
fn jump_drive_skills_pochven_multistack_fuel_fatigue_decay_and_timer_errors() {
    let mut ctx = test_ctx();
    let char_id = 96_801_i64;
    let ship_id = 78_801_i64;
    let iso_tid = 17_887_u32;
    let cap_tid = 99_850_u32;
    let mult_tid = 99_851_u32;

    let can_jump = attr_id_by_name(&ctx, "canJump").unwrap_or(866);
    let jd_range = attr_id_by_name(&ctx, "jumpDriveRange").unwrap_or(868);
    let jd_amount = attr_id_by_name(&ctx, "jumpDriveConsumptionAmount").unwrap_or(869);
    let jd_type = attr_id_by_name(&ctx, "jumpDriveConsumptionType").unwrap_or(867);
    let jf_mult = attr_id_by_name(&ctx, "jumpFatigueMultiplier").unwrap_or(1971);

    assert!(attr_id_by_name(&ctx, "nonExistentDogmaAttr").is_none());
    assert!(type_attr(&ctx, 999_999_999, "canJump").is_none());

    // Find two low-sec systems and a Pochven system (region 10_000_070)
    let mut low_sys: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .collect();
    low_sys.sort_unstable();
    let cur_sys = low_sys[0];
    let dst_sys = low_sys
        .iter()
        .copied()
        .find(|&sid| sid != cur_sys && distance_ly(&ctx, cur_sys, sid) > 1.0 && distance_ly(&ctx, cur_sys, sid) <= 8.0)
        .unwrap();
    let dist = distance_ly(&ctx, cur_sys, dst_sys);

    // Configure cap_tid with base range = dist * 0.6 (so level 0 fails JumpDistanceOutOfRange, level 5 = 1.2 * dist succeeds!)
    clone_type_with(
        &mut ctx.sde,
        23757,
        cap_tid,
        547,
        6,
        &[
            (can_jump, 1.0),
            (jd_range, dist * 0.6),
            (jd_amount, 100.0),
            (jd_type, f64::from(iso_tid)),
        ],
    );
    clone_type_with(
        &mut ctx.sde,
        23757,
        mult_tid,
        547,
        6,
        &[(jf_mult, 0.20)], // 1.0 - 0.20 = 0.80 reduction bonus
    );
    assert!((ship_fatigue_reduction_bonus(&ctx, mult_tid) - 0.80).abs() < 1e-6);

    // Group ID fallbacks for ship_fatigue_reduction_bonus
    for (idx, grp) in [883, 28, 380, 1202, 513, 29, 31, 941].into_iter().enumerate() {
        let tid = 99_860 + idx as u32;
        clone_type_with(&mut ctx.sde, 588, tid, grp, 6, &[]);
        assert!((ship_fatigue_reduction_bonus(&ctx, tid) - 0.90).abs() < 1e-6);
    }

    // Wormhole source and Zarzakh source checks
    assert_eq!(
        plan_jump_drive(&ctx, char_id, cap_tid, 31_000_005, dst_sys)
            .unwrap_err()
            .code,
        "JumpDriveForbiddenInWormholeSpace"
    );
    assert_eq!(
        plan_jump_drive(&ctx, char_id, cap_tid, 30_100_000, dst_sys)
            .unwrap_err()
            .code,
        "JumpDriveForbiddenInZarzakh"
    );

    // Pochven destination check
    if let Some(poch_sys) = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
    {
        assert_eq!(
            plan_jump_drive(&ctx, char_id, cap_tid, cur_sys, poch_sys)
                .unwrap_err()
                .code,
            "CannotJumpToPochven"
        );
    }

    // Without Jump Drive Calibration skill -> JumpDistanceOutOfRange
    assert_eq!(
        plan_jump_drive(&ctx, char_id, cap_tid, cur_sys, dst_sys)
            .unwrap_err()
            .code,
        "JumpDistanceOutOfRange"
    );

    // Grant Jump Drive Calibration V and Jump Fuel Conservation V
    let jdc_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.name == "Jump Drive Calibration")
        .map(|t| t.type_id)
        .unwrap();
    let jfc_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.name == "Jump Fuel Conservation")
        .map(|t| t.type_id)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        let smap = snap.skills.entry(char_id).or_default();
        smap.insert(
            jdc_id,
            TrainedSkillRecord {
                type_id: jdc_id,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let plan_no_jfc = plan_jump_drive(&ctx, char_id, cap_tid, cur_sys, dst_sys).expect("jdc V ok");
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            jfc_id,
            TrainedSkillRecord {
                type_id: jfc_id,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let plan_with_jfc = plan_jump_drive(&ctx, char_id, cap_tid, cur_sys, dst_sys).expect("jfc V ok");
    assert!(plan_with_jfc.fuel_units < plan_no_jfc.fuel_units);

    // InsufficientFuel when fuel bay is empty, then multi-stack consumption across FuelBay + Cargo
    assert_eq!(
        consume_jump_fuel(&ctx, ship_id, &plan_with_jfc)
            .unwrap_err()
            .code,
        "InsufficientFuel"
    );
    let half = (plan_with_jfc.fuel_units / 2).max(1);
    let rest = plan_with_jfc.fuel_units - half + 15;
    ctx.db.with_mut_snapshot(|snap| {
        put_item(
            snap,
            78_810,
            iso_tid,
            char_id,
            ship_id,
            InventoryFlag::SpecializedFuelBay,
            half,
        );
        put_item(
            snap,
            78_811,
            iso_tid,
            char_id,
            ship_id,
            InventoryFlag::Cargo,
            rest,
        );
    });
    consume_jump_fuel(&ctx, ship_id, &plan_with_jfc).expect("multi-stack fuel ok");
    ctx.db.with_snapshot(|snap| {
        let remaining: Vec<i32> = [78_810, 78_811]
            .into_iter()
            .filter_map(|id| snap.items.get(&id).map(|i| i.quantity))
            .collect();
        assert_eq!(remaining, vec![15]);
    });

    // current_fatigue_min decay & handle_get_jump_timers error branches
    let now = ctx.now_unix();
    let (_, fat) = apply_jump_fatigue(&ctx, char_id, dist);
    let decayed = current_fatigue_min(&ctx, char_id, now + 120);
    assert!((decayed - (fat - 2.0).max(0.0)).abs() < 1e-6);

    let mut sess = SessionState::new(745);
    sess.user_id = Some(50);
    sess.character_id = Some(char_id);
    assert!(handle_get_jump_timers(&ctx, &sess, &[]).is_ok());
    assert_eq!(
        handle_get_jump_timers(&ctx, &sess, &[PyRep::String("bad".into())])
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_get_jump_timers(&ctx, &sess, &[PyRep::Integer(char_id + 1)])
            .unwrap_err()
            .code,
        "CharacterNotInSession"
    );
}

mod w4_ops_pve_mut_extra_tests {
    use super::{attr_id_by_name, bound_sys, clone_type_with, put_item, ship_ball, test_ctx};
    use crate::services::pve_crab_beacons::{
        handle_crab_component_call, register_crab_beacon, TYPE_CRAB_BEACON,
    };
    use crate::services::space_destiny::space_nav_travel::*;
    use eve_db::types::UpwellStructureRecord;
    use eve_db::{BookmarkRecord, InventoryFlag};
    use eve_destiny::{Ball, MovementMode, WarpPhase};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use glam::DVec3;
    use std::collections::HashMap;

    fn make_session(char_id: i64, corp_id: i64, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
        let mut s = SessionState::new(745);
        s.user_id = Some(50);
        s.character_id = Some(char_id);
        s.corporation_id = Some(corp_id);
        s.solar_system_id = sys_id;
        s.ship_id = ship_id;
        s
    }

    fn make_ship_ball(ship_id: i64, pos: DVec3) -> Ball {
        ship_ball(ship_id, pos)
    }

    #[tokio::test]
    async fn structure_tether_access_weapons_locks_fighters_disallow_warp_and_crab_immobile() {
        let mut ctx = test_ctx();
        let sys_id = 30_000_142_u32;
        let corp_owner = 98_500_001_i64;
        let corp_other = 98_500_002_i64;
        let char_id = 96_901_i64;
        let ship_id = 78_901_i64;
        let struct_id = 1_049_901_001_i64;
        let struct_tid = 35_832_u32;
        let disallow_mod_tid = 99_910_u32;

        if let Some(tid) = attr_id_by_name(&ctx, "tetheringRange") {
            clone_type_with(&mut ctx.sde, struct_tid, struct_tid, 1657, 65, &[(tid, 15_000.0)]);
        }
        let dis_attr = attr_id_by_name(&ctx, "disallowTethering").unwrap_or(1990);
        clone_type_with(&mut ctx.sde, 21096, disallow_mod_tid, 515, 7, &[(dis_attr, 1.0)]);

        let mod_item_id = 78_902_i64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = snap.characters.values().next().cloned().unwrap();
            ch.character_id = char_id;
            ch.corporation_id = corp_other;
            ch.alliance_id = None;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            snap.characters.insert(char_id, ch);
            put_item(snap, ship_id, 588, char_id, i64::from(sys_id), InventoryFlag::Hangar, 1);
            put_item(snap, mod_item_id, disallow_mod_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1);
            snap.structures.insert(
                struct_id,
                UpwellStructureRecord {
                    structure_id: struct_id,
                    type_id: struct_tid,
                    owner_corp_id: corp_owner,
                    solar_system_id: sys_id,
                    name: "Corp Citadel".into(),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    position: [0.0, 0.0, 0.0],
                    shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    hull_hp: 100_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        });

        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h.spawn_ball(make_ship_ball(ship_id, DVec3::new(2_000.0, 0.0, 0.0))).await;

        // 1. Different corp without alliance or access -> structure_access_allowed false -> not tethered
        sync_tether(&ctx, char_id, sys_id, ship_id).await;
        assert!(!sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);

        // Share same corporation -> tethered & OnDbuffUpdated emitted!
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().corporation_id = corp_owner;
        });
        let _ = ctx.drain_notifications_for_char(char_id);
        sync_tether(&ctx, char_id, sys_id, ship_id).await;
        assert!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);

        // 2. Break by weapons timer
        let now = ctx.now_unix();
        ctx.crimewatch.write().unwrap().trigger_weapon_timer(char_id, now);
        sync_tether(&ctx, char_id, sys_id, ship_id).await;
        assert!(!sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
        ctx.crimewatch.write().unwrap().states.remove(&char_id);

        // Re-tether, then break by disallowTethering active module
        sync_tether(&ctx, char_id, sys_id, ship_id).await;
        assert!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.insert(mod_item_id);
        });
        sync_tether(&ctx, char_id, sys_id, ship_id).await;
        assert!(!sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&mod_item_id);
        });

        // Break by active fighter in ctx.active_fighters
        sync_tether(&ctx, char_id, sys_id, ship_id).await;
        assert!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
        ctx.active_fighters.write().unwrap().insert(
            888_001,
            crate::router::ActiveFighterSquadron {
                ball_id: 888_001,
                item_id: 888_001,
                type_id: 23055,
                owner_char_id: char_id,
                parent_ship_id: ship_id,
                tube_index: 0,
                squadron_size: 9,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
        sync_tether(&ctx, char_id, sys_id, ship_id).await;
        assert!(!sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
        ctx.active_fighters.write().unwrap().clear();

        // Break by Abandoned structure / missing core
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.get_mut(&struct_id).unwrap().quantum_core_installed = false;
        });
        sync_tether(&ctx, char_id, sys_id, ship_id).await;
        assert!(!sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);

        // Break by Warp mode
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.get_mut(&struct_id).unwrap().quantum_core_installed = true;
        });
        let _ = sys_h
            .spawn_ball(eve_destiny::Ball {
                id: ship_id,
                type_id: 588,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Warp {
                    origin: DVec3::ZERO,
                    destination: DVec3::new(1e9, 0.0, 0.0),
                    phase: WarpPhase::Cruising,
                    warp_speed_au_s: 3.0,
                    total_distance: 1e9,
                    elapsed_warp_time: 0.0,
                },
            })
            .await;
        sync_tether(&ctx, char_id, sys_id, ship_id).await;
        assert!(!sys_h.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    }

    #[tokio::test]
    async fn crab_immobilize_orbit_follow_align_bookmarks_and_jump_boundaries() {
        let mut ctx = test_ctx();
        ctx.db.set_clock(std::sync::Arc::new(
            eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
        ));
        let mut low_systems: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.security_status > 0.0 && s.security_status < 0.445 && s.solar_system_id < 31_000_000 && s.region_id != 10_000_070)
            .map(|s| s.solar_system_id)
            .collect();
        low_systems.sort_unstable();
        let lowsec = low_systems[0];
        let nearby_low = low_systems
            .iter()
            .copied()
            .find(|&id| id != lowsec && distance_ly(&ctx, lowsec, id) <= 6.0)
            .expect("nearby lowsec");

        // Prepare dread linked to CRAB beacon in lowsec -> immobile!
        let dread_tid = ctx
            .sde
            .types
            .values()
            .filter(|t| {
                t.category_id == 6
                    && t.group_id == 485
                    && crate::services::pve_crab_beacons::is_eligible_crab_ship(
                        &ctx.sde,
                        TYPE_CRAB_BEACON,
                        t.type_id,
                    )
            })
            .map(|t| t.type_id)
            .min()
            .unwrap();
        let char_id = 96_910_i64;
        let ship_id = 78_910_i64;
        let target_id = 78_911_i64;
        let bm_other = 6_901_i64;
        let bm_other_sys = 6_902_i64;
        ctx.db.provision_gm_account("nav_w4_pilot", "pw", char_id, "NavW4Pilot");
        let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
        let _ = ctx.db.acquire_online_lock_with_computer_hash(uid, char_id as u64, Some(char_id), None, true, ctx.now_unix());

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = lowsec;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            put_item(snap, ship_id, dread_tid, char_id, i64::from(lowsec), InventoryFlag::Hangar, 1);
            let beacon_id = 959_001_i64;
            put_item(snap, beacon_id, TYPE_CRAB_BEACON, char_id, i64::from(lowsec), InventoryFlag::None, 1);
            snap.item_positions.insert(beacon_id, [0.0, 0.0, 0.0]);
            // Bookmark owned by another char + bookmark in another system
            snap.bookmarks.insert(
                bm_other,
                BookmarkRecord {
                    bookmark_id: bm_other,
                    owner_id: char_id + 1,
                    label: "NotMine".into(),
                    solar_system_id: lowsec,
                    x: 1000.0,
                    y: 0.0,
                    z: 0.0,
                    item_id: None,
                    memo: String::new(),
                    folder_id: None,
                    is_shared: false,
                },
            );
            snap.bookmarks.insert(
                bm_other_sys,
                BookmarkRecord {
                    bookmark_id: bm_other_sys,
                    owner_id: char_id,
                    label: "WrongSys".into(),
                    solar_system_id: nearby_low,
                    x: 1000.0,
                    y: 0.0,
                    z: 0.0,
                    item_id: None,
                    memo: String::new(),
                    folder_id: None,
                    is_shared: false,
                },
            );
        });

        let sys_h = ctx.systems.get_or_spawn(lowsec);
        let cs = ctx.build_ship_combat_state(char_id, ship_id, dread_tid);
        let mut dread_ball = make_ship_ball(ship_id, DVec3::ZERO);
        dread_ball.type_id = dread_tid;
        let _ = sys_h.spawn_ball_with_combat(dread_ball, cs).await;
        let _ = sys_h.spawn_ball(make_ship_ball(target_id, DVec3::new(30_000.0, 0.0, 0.0))).await;

        let mut sess = make_session(char_id, 98_000_001, Some(lowsec), Some(ship_id));
        sess.user_id = Some(uid);
        let bound = bound_sys(lowsec, char_id);

        // Movement commands succeed before CRAB link: CmdFollowBall + CmdOrbit default ranges + AlignTo kw dstID/coords + other-owner bookmark error
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdFollowBall", &[PyRep::Integer(target_id)])
            .await
            .expect("follow ball default range");
        handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(target_id)])
            .await
            .expect("orbit default range");
        let mut kw_dst = HashMap::new();
        kw_dst.insert("dstID".to_string(), PyRep::Integer(target_id));
        handle_cmd_align_to(&ctx, &sess, Some(&bound), &[], &kw_dst).await.expect("align dstID");
        handle_cmd_align_to(&ctx, &sess, Some(&bound), &[PyRep::Integer(target_id)], &HashMap::new())
            .await
            .expect("align dst arg");
        let mut kw_bm_other = HashMap::new();
        kw_bm_other.insert("bookmarkID".to_string(), PyRep::Integer(bm_other));
        assert!(handle_cmd_align_to(&ctx, &sess, Some(&bound), &[], &kw_bm_other).await.is_err());
        let mut kw_bm_sys = HashMap::new();
        kw_bm_sys.insert("bookmarkID".to_string(), PyRep::Integer(bm_other_sys));
        assert!(handle_cmd_align_to(&ctx, &sess, Some(&bound), &[], &kw_bm_sys).await.is_err());

        // Link ship to CRAB beacon -> check immobilization across Linking and Scanning phases
        register_crab_beacon(&ctx, 959_001, TYPE_CRAB_BEACON, char_id, lowsec, DVec3::ZERO);
        let link_st = handle_crab_component_call(&ctx, &sess, lowsec, char_id, ship_id, 959_001, "linkWithShip", "InitiateLink", &[])
            .await
            .unwrap();
        let link_checks = (
            handle_cmd_align_to(&ctx, &sess, Some(&bound), &[PyRep::Integer(target_id)], &HashMap::new()).await,
            handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdFollowBall", &[PyRep::Integer(target_id)]).await,
            handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(target_id)]).await,
            handle_cmd_goto_direction(&ctx, &sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await,
            handle_cmd_goto_point(&ctx, &sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await,
        );
        ctx.advance_secs(crate::services::pve_crab_beacons::CRAB_LINK_DURATION_SECS + 1);
        crate::services::pve_crab_beacons::tick_crab_beacons_in_system(&ctx, lowsec).await;
        let scan_st = handle_crab_component_call(&ctx, &sess, lowsec, char_id, ship_id, 959_001, "linkWithShip", "GetLinkState", &[])
            .await
            .unwrap();
        let scan_checks = (
            handle_cmd_align_to(&ctx, &sess, Some(&bound), &[PyRep::Integer(target_id)], &HashMap::new()).await,
            handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdFollowBall", &[PyRep::Integer(target_id)]).await,
            handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(target_id)]).await,
            handle_cmd_goto_direction(&ctx, &sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await,
            handle_cmd_goto_point(&ctx, &sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await,
        );
        assert_eq!(link_st.dict_get("phase").and_then(|v| v.as_str()), Some("linking"));
        assert_eq!(link_st.dict_get("linked").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(scan_st.dict_get("phase").and_then(|v| v.as_str()), Some("scanning"));
        assert!(link_checks.0.is_ok() && scan_checks.0.is_ok());

        // Jump planning out-of-range + abyssal destination + skill 3456 cap discount + insufficient cap!
        let iso_tid = 17887_u32;
        let blops_tid = 99_921_u32;
        let can_jump = attr_id_by_name(&ctx, "canJump").unwrap_or(866);
        let jd_range = attr_id_by_name(&ctx, "jumpDriveRange").unwrap_or(868);
        let jd_amt = attr_id_by_name(&ctx, "jumpDriveConsumptionAmount").unwrap_or(869);
        let jd_type = attr_id_by_name(&ctx, "jumpDriveConsumptionType").unwrap_or(867);
        clone_type_with(
            &mut ctx.sde,
            22474,
            blops_tid,
            898,
            6,
            &[(can_jump, 1.0), (jd_range, 0.001), (jd_amt, 100.0), (jd_type, f64::from(iso_tid))],
        );
        assert_eq!(
            plan_jump_drive(&ctx, char_id, blops_tid, lowsec, nearby_low).unwrap_err().code,
            "JumpDistanceOutOfRange"
        );
        assert_eq!(
            plan_jump_drive(&ctx, char_id, blops_tid, lowsec, 32_000_005).unwrap_err().code,
            "JumpDriveForbiddenInAbyssalSpace"
        );
        assert_eq!(
            plan_jump_drive(&ctx, char_id, blops_tid, lowsec, 99_999_999).unwrap_err().code,
            "JumpDistanceOutOfRange"
        );
    }

    #[tokio::test]
    async fn batch3_jump_capacitor_fatigue_cloak_decloak_mjd_and_nav_validation() {
        let mut ctx = test_ctx();
        let clock = std::sync::Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000));
        ctx.db.set_clock(clock);

        let mut low_systems: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.security_status > 0.0 && s.security_status < 0.44 && s.solar_system_id < 31_000_000 && s.region_id != 10_000_070)
            .map(|s| s.solar_system_id)
            .collect();
        low_systems.sort_unstable();
        let lowsec = low_systems[0];

        // Create two nearby synthetic systems at exact 0.44 (allowed) vs 0.45 (forbidden highsec) boundary
        let template_sys = ctx.sde.solar_systems[&lowsec].clone();
        let sec44_sys = 30_999_044_u32;
        let sec45_sys = 30_999_045_u32;
        let mut s44 = template_sys.clone();
        s44.solar_system_id = sec44_sys;
        s44.name = "Sec044Sys".into();
        s44.security_status = 0.44;
        s44.galactic_pos_ly[0] += 1.5;
        let mut s45 = template_sys.clone();
        s45.solar_system_id = sec45_sys;
        s45.name = "Sec045Sys".into();
        s45.security_status = 0.45;
        s45.galactic_pos_ly[0] += 1.5;
        let far_sys = 30_999_099_u32;
        let mut sfar = template_sys.clone();
        sfar.solar_system_id = far_sys;
        sfar.name = "FarSys".into();
        sfar.security_status = 0.2;
        sfar.galactic_pos_ly[0] += 200.0;

        let sde_mut = std::sync::Arc::make_mut(&mut ctx.sde);
        sde_mut.solar_systems.insert(sec44_sys, s44);
        sde_mut.solar_systems.insert(sec45_sys, s45);
        sde_mut.solar_systems.insert(far_sys, sfar);

        let iso_tid = 17887_u32;
        let blops_tid = 99_931_u32;
        let cloak_tid = 11_578_u32;
        let can_jump = attr_id_by_name(&ctx, "canJump").unwrap_or(866);
        let jd_range = attr_id_by_name(&ctx, "jumpDriveRange").unwrap_or(868);
        let jd_amt = attr_id_by_name(&ctx, "jumpDriveConsumptionAmount").unwrap_or(869);
        let jd_type = attr_id_by_name(&ctx, "jumpDriveConsumptionType").unwrap_or(867);
        let jd_cap_need = attr_id_by_name(&ctx, "jumpDriveCapacitorNeed").unwrap_or(897);
        let disallow_hs = attr_id_by_name(&ctx, "disallowInHighSec").unwrap_or(1077);
        clone_type_with(
            &mut ctx.sde,
            22474,
            blops_tid,
            898,
            6,
            &[
                (can_jump, 1.0),
                (jd_range, 100.0),
                (jd_amt, 100.0),
                (jd_type, f64::from(iso_tid)),
                (jd_cap_need, 0.90),
                (disallow_hs, 1.0),
            ],
        );

        let char_id = 96_931_i64;
        let ship_id = 78_931_i64;
        let target_id = 78_932_i64;
        let cloak_id = 78_933_i64;
        ctx.db.provision_gm_account("nav_b3_pilot", "pw", char_id, "NavB3Pilot");
        let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
        let _ = ctx.db.acquire_online_lock_with_computer_hash(uid, char_id as u64, Some(char_id), None, true, ctx.now_unix());

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = lowsec;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            put_item(snap, ship_id, blops_tid, char_id, i64::from(lowsec), InventoryFlag::Hangar, 1);
            put_item(snap, cloak_id, cloak_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        });

        let sys_h = ctx.systems.get_or_spawn(lowsec);
        let mut cs = ctx.build_ship_combat_state(char_id, ship_id, blops_tid);
        cs.max_capacitor = 1_000.0;
        cs.capacitor = 100.0;
        cs.is_cloaked = false;
        cs.base_max_velocity = Some(300.0);
        let mut ball = ship_ball(ship_id, DVec3::ZERO);
        ball.type_id = blops_tid;
        ball.max_velocity = 300.0;
        let _ = sys_h.spawn_ball_with_combat(ball, cs).await;
        let _ = sys_h.spawn_ball(ship_ball(target_id, DVec3::new(15_000.0, 0.0, 0.0))).await;

        let mut sess = make_session(char_id, 98_000_002, Some(lowsec), Some(ship_id));
        sess.user_id = Some(uid);
        let bound = bound_sys(lowsec, char_id);

        // 1. Highsec 0.44 allowed vs 0.45 forbidden + out-of-range error + unknown target distance fallback
        assert!(plan_jump_drive(&ctx, char_id, blops_tid, lowsec, sec44_sys).is_ok());
        assert_eq!(
            plan_jump_drive(&ctx, char_id, blops_tid, lowsec, sec45_sys).unwrap_err().code,
            "JumpDriveHighSecForbidden"
        );
        assert!(plan_jump_drive(&ctx, char_id, blops_tid, lowsec, far_sys).is_err());
        let _ = distance_ly(&ctx, lowsec, 99_999_999);

        // 2. validate_and_prepare_jump_capacitor: NotEnoughCapacitor when cap=100, need=900;
        //    then add cap & skill 3456 (Jump Drive Operation lv 5 = 25% discount) -> succeeds!
        let err_cap = validate_and_prepare_jump_capacitor(&ctx, char_id, ship_id, lowsec, blops_tid)
            .await
            .unwrap_err();
        assert_eq!(err_cap.code, "NotEnoughCapacitor");

        sys_h.try_update_combat_state(ship_id, |c| {
            c.capacitor = 700.0; // enough with lv 5 skill (675), NOT enough without (900)
        });
        assert_eq!(
            validate_and_prepare_jump_capacitor(&ctx, char_id, ship_id, lowsec, blops_tid)
                .await
                .unwrap_err()
                .code,
            "NotEnoughCapacitor"
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                3456,
                eve_db::TrainedSkillRecord {
                    type_id: 3456,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        });
        let cap_ok = validate_and_prepare_jump_capacitor(&ctx, char_id, ship_id, lowsec, blops_tid)
            .await
            .expect("skill 3456 25% reduction allows jump")
            .expect("updated CombatState");
        assert!(cap_ok.capacitor < 100.0);

        // 3. consume_jump_fuel shortage vs exact consumption
        let plan = plan_jump_drive(&ctx, char_id, blops_tid, lowsec, sec44_sys).unwrap();
        assert!(consume_jump_fuel(&ctx, ship_id, &plan).is_err());

        // 4. apply_jump_fatigue capping & handle_get_jump_timers + check_jump_activation + ship_fatigue_reduction_bonus
        let _ = ship_fatigue_reduction_bonus(&ctx, 20_185); // Freighter/JF vs dread
        let _ = ship_fatigue_reduction_bonus(&ctx, blops_tid);
        let _ = ctx.drain_notifications_for_char(char_id);
        let (cd1, fat1) = apply_jump_fatigue(&ctx, char_id, 5.0);
        assert!(cd1 > 0.0 && fat1 > 0.0);
        assert!(check_jump_activation(&ctx, char_id).is_err());
        let notifs_jf = ctx.drain_notifications_for_char(char_id);
        assert!(notifs_jf.iter().any(|n| n.event == "OnJumpTimersUpdated"));
        let timers_rep = handle_get_jump_timers(&ctx, &sess, &[]).expect("jump timers");
        assert!(!matches!(timers_rep, PyRep::None));
        ctx.advance_secs(4_000);
        assert!(check_jump_activation(&ctx, char_id).is_ok());

        // 5. sync_ship_cloak_and_mjfg_from_fitting cloak activation then decloak sets delay & OnSpecialFX
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.insert(cloak_id);
        });
        sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, lowsec, ship_id).await;
        let cs_cloaked = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs_cloaked.is_cloaked);
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&cloak_id);
        });
        let _ = ctx.drain_notifications_for_char(char_id);
        sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, lowsec, ship_id).await;
        let cs_decloaked = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(!cs_decloaked.is_cloaked);
        assert!(cs_decloaked.decloak_targeting_delay_remaining_s > 0.0);

        assert!(ensure_not_mjd_spooling(&sys_h, ship_id).await.is_ok());

        // 6. lance caster lock allows Stop, blocks SetSpeedFraction, AlignTo, Orbit/Follow, GotoDirection, GotoPoint
        ctx.tactical_state
            .write()
            .unwrap()
            .lance_caster_lock_until
            .insert((ctx.instance_id as usize, ship_id), ctx.now_unix_secs() + 60);
        assert!(handle_cmd_stop(&ctx, &sess, Some(&bound)).await.is_ok());
        for code in [
            handle_cmd_set_speed_fraction(&ctx, &sess, Some(&bound), &[PyRep::Real(0.5)]).await.unwrap_err().code,
            handle_cmd_align_to(&ctx, &sess, Some(&bound), &[PyRep::Integer(target_id)], &HashMap::new()).await.unwrap_err().code,
            handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(target_id)]).await.unwrap_err().code,
            handle_cmd_goto_direction(&ctx, &sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await.unwrap_err().code,
            handle_cmd_goto_point(&ctx, &sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await.unwrap_err().code,
        ] {
            assert_eq!(code, "CantMoveWhileModuleActive");
        }
        ctx.tactical_state.write().unwrap().lance_caster_lock_until.clear();

        // Command validation negative paths (short args, zero vec, self target, missing target)
        assert!(handle_cmd_goto_direction(&ctx, &sess, Some(&bound), &[]).await.is_err());
        assert!(handle_cmd_goto_point(&ctx, &sess, Some(&bound), &[]).await.is_err());
        assert!(handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(ship_id)]).await.is_err());
        assert!(handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdFollowBall", &[PyRep::Integer(999_888_777)]).await.is_err());
        assert!(handle_cmd_align_to(&ctx, &sess, Some(&bound), &[], &HashMap::new()).await.is_err());
        assert!(handle_cmd_set_speed_fraction(&ctx, &sess, Some(&bound), &[PyRep::Real(2.0)]).await.is_ok());
        assert!(handle_cmd_stop(&ctx, &sess, Some(&bound)).await.is_ok());

        // 7. MJD spooling blocks movement commands and fails ensure_not_mjd_spooling
        let _ = sys_h
            .add_module_cycle(crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id: 78_939,
                module_type_id: 4383,
                duration_s: 10.0,
                repeat: 0,
                mjd_jump_distance_m: Some(100_000.0),
                ..Default::default()
            })
            .await;
        assert!(ensure_not_mjd_spooling(&sys_h, ship_id).await.is_err());
        assert!(handle_cmd_set_speed_fraction(&ctx, &sess, Some(&bound), &[PyRep::Real(0.5)]).await.is_err());
    }

    #[tokio::test]
    async fn batch4_align_warp_cancel_abyssal_cloak_celestials_and_session_guards() {
        let mut ctx = test_ctx();
        let mut low_systems: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.security_status > 0.0 && s.security_status < 0.44 && s.solar_system_id < 31_000_000)
            .map(|s| s.solar_system_id)
            .collect();
        low_systems.sort_unstable();
        let lowsec = low_systems[0];

        let char_id = 96_941_i64;
        let ship_id = 78_941_i64;
        let cloak_id = 78_942_i64;
        let cloak_tid = 11_578_u32;
        let blops_tid = 22_474_u32;
        ctx.db.provision_gm_account("nav_b4_pilot", "pw", char_id, "NavB4Pilot");
        let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);

        // Clone an industrial/JF type without jumpFatigueMultiplier to test fallback bonus
        let jf_no_attr_tid = 99_941_u32;
        clone_type_with(&mut ctx.sde, blops_tid, jf_no_attr_tid, 902, 6, &[]);
        assert!((ship_fatigue_reduction_bonus(&ctx, jf_no_attr_tid) - 0.90).abs() < 1e-6);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = lowsec;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
            put_item(snap, ship_id, blops_tid, char_id, i64::from(lowsec), InventoryFlag::Hangar, 1);
            put_item(snap, cloak_id, cloak_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1);
            snap.character_last_jump_unix.insert(char_id, ctx.now_unix().saturating_sub(20));
        });

        let sys_h = ctx.systems.get_or_spawn(lowsec);
        let mut cs = ctx.build_ship_combat_state(char_id, ship_id, blops_tid);
        cs.capacitor = 800.0;
        cs.max_capacitor = 1_000.0;
        let _ = sys_h.spawn_ball_with_combat(ship_ball(ship_id, DVec3::ZERO), cs).await;
        let bound = bound_sys(lowsec, char_id);

        // Passive cap recharge during validate_and_prepare_jump_capacitor
        let _ = validate_and_prepare_jump_capacitor(&ctx, char_id, ship_id, lowsec, blops_tid).await;

        // Docked NotInSpace, NoActiveShip, and ShipNotInSpace guards across all movement handlers
        let mut docked_sess = make_session(char_id, uid as i64, Some(lowsec), Some(ship_id));
        docked_sess.station_id = Some(60_003_760);
        for res in [
            handle_cmd_goto_point(&ctx, &docked_sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await,
            handle_cmd_goto_direction(&ctx, &docked_sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await,
            handle_cmd_stop(&ctx, &docked_sess, Some(&bound)).await,
            handle_cmd_set_speed_fraction(&ctx, &docked_sess, Some(&bound), &[PyRep::Real(0.5)]).await,
            handle_cmd_orbit_or_follow(&ctx, &docked_sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(1)]).await,
            handle_cmd_align_to(&ctx, &docked_sess, Some(&bound), &[PyRep::Integer(1)], &HashMap::new()).await,
        ] {
            assert_eq!(res.unwrap_err().code, "NotInSpace");
        }

        let no_ship_sess = make_session(char_id, uid as i64, Some(lowsec), None);
        for res in [
            handle_cmd_goto_direction(&ctx, &no_ship_sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await,
            handle_cmd_stop(&ctx, &no_ship_sess, Some(&bound)).await,
            handle_cmd_set_speed_fraction(&ctx, &no_ship_sess, Some(&bound), &[PyRep::Real(0.5)]).await,
            handle_cmd_orbit_or_follow(&ctx, &no_ship_sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(1)]).await,
            handle_cmd_align_to(&ctx, &no_ship_sess, Some(&bound), &[PyRep::Integer(1)], &HashMap::new()).await,
        ] {
            assert_eq!(res.unwrap_err().code, "NoActiveShip");
        }

        let unspawned_sess = make_session(char_id, uid as i64, Some(lowsec), Some(78_999_999));
        for res in [
            handle_cmd_goto_point(&ctx, &unspawned_sess, Some(&bound), &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await,
            handle_cmd_set_speed_fraction(&ctx, &unspawned_sess, Some(&bound), &[PyRep::Real(0.5)]).await,
            handle_cmd_orbit_or_follow(&ctx, &unspawned_sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(1)]).await,
            handle_cmd_align_to(&ctx, &unspawned_sess, Some(&bound), &[PyRep::Integer(1)], &HashMap::new()).await,
        ] {
            assert_eq!(res.unwrap_err().code, "ShipNotInSpace");
        }

        // Cancel pending warp align at speed=0.5 (switches to GotoDirection) and speed=0.0 (Stop)
        let sess = make_session(char_id, uid as i64, Some(lowsec), Some(ship_id));
        let warp_align = MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(10_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 10_000_000.0,
            elapsed_warp_time: 0.0,
            phase: eve_destiny::WarpPhase::Aligning,
        };
        let _ = sys_h.set_movement_mode(ship_id, warp_align.clone()).await;
        assert!(handle_cmd_set_speed_fraction(&ctx, &sess, Some(&bound), &[PyRep::Real(0.5)]).await.is_ok());
        let _ = sys_h.set_movement_mode(ship_id, warp_align).await;
        assert!(handle_cmd_set_speed_fraction(&ctx, &sess, Some(&bound), &[PyRep::Real(0.0)]).await.is_ok());

        // resolve_system_target_position on SDE stations, jumps, and celestials
        if let Some(st) = ctx.sde.stations.values().find(|s| s.solar_system_id == lowsec) {
            assert!(resolve_system_target_position(&ctx, lowsec, char_id, st.station_id).await.is_some());
        }
        if let Some(j) = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == lowsec) {
            assert!(resolve_system_target_position(&ctx, lowsec, char_id, j.stargate_id).await.is_some());
        }
        if let Some(c) = ctx.sde.celestials_in_system(lowsec).into_iter().next() {
            assert!(resolve_system_target_position(&ctx, lowsec, char_id, c.celestial_id).await.is_some());
        }

        // Already cloaked field sync + Abyssal forced decloak & velocity restore
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.insert(cloak_id);
        });
        sys_h.try_update_combat_state(ship_id, |c| {
            c.is_cloaked = true;
            c.is_covert_cloak = true;
            c.cloak_stabilization_remaining_s = 0.0;
            c.cloak_configured_targeting_delay_s = 0.0;
        });
        sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, lowsec, ship_id).await;

        let ab_sys = 32_000_001_u32;
        let ab_ship = 78_945_i64;
        let ab_h = ctx.systems.get_or_spawn(ab_sys);
        let mut ab_cs = ctx.build_ship_combat_state(char_id, ab_ship, blops_tid);
        ab_cs.is_cloaked = false;
        ab_cs.cloak_velocity_multiplier = Some(0.25);
        let _ = ab_h.spawn_ball_with_combat(ship_ball(ab_ship, DVec3::ZERO), ab_cs).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&cloak_id).unwrap().location_id = ab_ship;
            snap.module_active_states.remove(&cloak_id);
        });
        crate::services::mobile_deployables_ext::record_ship_cloak_activated(&ctx, ab_ship);
        sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, ab_sys, ab_ship).await;
        let ab_after = ab_h.get_combat_state(ab_ship).await.unwrap().unwrap();
        assert!(ab_after.cloak_velocity_multiplier.is_none());

        // Forced decloak while cloaked with active cloak module in Abyssal
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.insert(cloak_id);
        });
        ab_h.try_update_combat_state(ab_ship, |c| {
            c.is_cloaked = true;
            c.gate_cloak_remaining_ticks = 0;
        });
        sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, ab_sys, ab_ship).await;
        assert!(!ab_h.get_combat_state(ab_ship).await.unwrap().unwrap().is_cloaked);
    }
}

#[tokio::test]
async fn nav_commands_happy_paths_and_cloak_mjfg_sync_exact_branch_mutants() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 96_991_i64;
    let ship_id = 78_991_i64;
    let target_id = 78_992_i64;
    let cloak_id = 78_993_i64;
    let mjfg_id = 78_994_i64;
    let cloak_tid = 11_578_u32; // Covert Ops Cloaking Device II
    let hull_tid = 588_u32;

    ctx.db.provision_gm_account("nav_mutant_pilot", "pw", char_id, "NavMutPilot");
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        put_item(snap, ship_id, hull_tid, char_id, i64::from(sys_id), InventoryFlag::Hangar, 1);
        put_item(snap, cloak_id, cloak_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        put_item(snap, mjfg_id, 83_485, char_id, ship_id, InventoryFlag::HiSlot1, 1);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let cs_init = ctx.build_ship_combat_state(char_id, ship_id, hull_tid);
    let _ = sys_h
        .spawn_ball_with_combat(ship_ball(ship_id, DVec3::ZERO), cs_init)
        .await;
    let mut tgt_ball = ship_ball(target_id, DVec3::new(15_000.0, 0.0, 0.0));
    tgt_ball.id = target_id;
    let _ = sys_h
        .spawn_ball_with_combat(tgt_ball, ctx.build_ship_combat_state(char_id, target_id, hull_tid))
        .await;

    let bound = bound_sys(sys_id, char_id);
    let mut sess = SessionState::new(745);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. Movement handler happy paths return valid integer ticks and transition ballpark state (L800, 856, 920, 1000)
    let dir_rep = handle_cmd_goto_direction(
        &ctx,
        &sess,
        Some(&bound),
        &[PyRep::Real(1.0), PyRep::Real(2.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap();
    assert!(dir_rep.as_i64().unwrap_or(-1) >= 0);

    let spd_rep = handle_cmd_set_speed_fraction(&ctx, &sess, Some(&bound), &[PyRep::Real(0.42)])
        .await
        .unwrap();
    assert!(spd_rep.as_i64().unwrap_or(-1) >= 0);
    let (_, b_spd) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((b_spd.speed_fraction - 0.42).abs() < 1e-6);

    let orb_rep = handle_cmd_orbit_or_follow(
        &ctx,
        &sess,
        Some(&bound),
        "CmdOrbit",
        &[PyRep::Integer(target_id), PyRep::Real(3_500.0)],
    )
    .await
    .unwrap();
    assert!(orb_rep.as_i64().unwrap_or(-1) >= 0);
    let (_, b_orb) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(b_orb.mode, MovementMode::Orbit { .. }));

    let fol_rep = handle_cmd_orbit_or_follow(
        &ctx,
        &sess,
        Some(&bound),
        "CmdFollowBall",
        &[PyRep::Integer(target_id)],
    )
    .await
    .unwrap();
    assert!(fol_rep.as_i64().unwrap_or(-1) >= 0);

    let stop_rep = handle_cmd_stop(&ctx, &sess, Some(&bound)).await.unwrap();
    assert!(stop_rep.as_i64().unwrap_or(-1) >= 0);
    let (_, b_stop) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(b_stop.mode, MovementMode::Stop));

    // 2. sync_ship_cloak_and_mjfg_from_fitting cloak branches (L1197, L1296-L1340)
    //    (a) `!is_cloaked` + `!cloak_recorded` activates cloak, records timestamp, sets delay & 900.0 stab
    let expected_covert = ctx
        .sde
        .types
        .get(&cloak_tid)
        .map(|t| {
            t.effect_ids.iter().any(|&eid| {
                ctx.sde
                    .effects
                    .get(&eid)
                    .is_some_and(|e| e.name == "cloakingWarpSafe" || e.is_warp_safe)
            })
        })
        .unwrap_or(false);
    let cloaking_skill = ctx.character_skill_levels(char_id).get(&11579).copied().unwrap_or(0) as u8;
    let expected_delay = crate::services::space_navigation_advanced::compute_decloak_targeting_delay_for_module(
        &ctx,
        char_id,
        ship_id,
        hull_tid,
        Some(cloak_id),
        cloak_tid,
        cloaking_skill,
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(cloak_id);
    });
    crate::services::mobile_deployables_ext::clear_ship_cloak_activated(&ctx, ship_id);
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs_after_init = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_init.is_cloaked);
    assert_eq!(cs_after_init.is_covert_cloak, expected_covert);
    assert!((cs_after_init.cloak_stabilization_remaining_s - 900.0).abs() < 1e-6);
    assert!((cs_after_init.cloak_configured_targeting_delay_s - expected_delay).abs() < 1e-6);
    assert!(ctx.db.with_snapshot(|s| {
        crate::services::mobile_deployables::read_deployable_state_snap(s)
            .cloak_activated_unix
            .contains_key(&ship_id)
    }));

    //    (b) Already cloaked with wrong `is_covert_cloak`, stabilization=500.0, `!cloak_recorded`:
    //        `stabilization` MUST stay 500.0 (kills L1314 `<=` -> `>` and `&&` -> `||`),
    //        `is_covert_cloak` updates (kills L1310 `!=` -> `==`),
    //        `cloak_activated_unix` gets recorded (kills L1318 `!` deletion).
    crate::services::mobile_deployables_ext::clear_ship_cloak_activated(&ctx, ship_id);
    sys_h.try_update_combat_state(ship_id, |c| {
        c.is_cloaked = true;
        c.is_covert_cloak = !expected_covert;
        c.cloak_stabilization_remaining_s = 500.0;
        c.cloak_configured_targeting_delay_s = 0.0; // tests `(0.0 - delay).abs()` vs `(0.0 / delay).abs()`
    });
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs_b = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_b.is_covert_cloak, expected_covert);
    assert!((cs_b.cloak_stabilization_remaining_s - 500.0).abs() < 1e-6);
    assert!((cs_b.cloak_configured_targeting_delay_s - expected_delay).abs() < 1e-6);
    assert!(ctx.db.with_snapshot(|s| {
        crate::services::mobile_deployables::read_deployable_state_snap(s)
            .cloak_activated_unix
            .contains_key(&ship_id)
    }));

    //    (c) Already cloaked with stabilization=0.0 AND cloak_recorded=true MUST keep 0.0 (kills L1314 `&&` -> `||` and `!` deletion),
    //        and negative configured delay `-expected_delay` updates to `+expected_delay` (kills L1321 `-` -> `+`).
    sys_h.try_update_combat_state(ship_id, |c| {
        c.cloak_stabilization_remaining_s = 0.0;
        c.cloak_configured_targeting_delay_s = -expected_delay;
    });
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs_c = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_c.cloak_stabilization_remaining_s - 0.0).abs() < 1e-6);
    assert!((cs_c.cloak_configured_targeting_delay_s - expected_delay).abs() < 1e-6);

    //    (d) Non-Abyssal `!cs.is_cloaked` preserves `cloak_velocity_multiplier` (kills L1339 `!` and L1340 `&&` -> `||`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_id);
    });
    sys_h.try_update_combat_state(ship_id, |c| {
        c.is_cloaked = false;
        c.cloak_velocity_multiplier = Some(0.25);
    });
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    assert_eq!(
        sys_h
            .get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .cloak_velocity_multiplier,
        Some(0.25)
    );

    // 3. Cap-MJFG spool checks (L1355 `active_cap_mjfg && spool <= 0.0 && sig.is_none()`)
    //    Inactive MJFG -> does not arm (kills 1st `&&` -> `||`)
    sys_h.try_update_combat_state(ship_id, |c| {
        c.mjfg_spool_remaining_s = 0.0;
        c.mjfg_sig_multiplier = None;
    });
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs_mj0 = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_mj0.mjfg_sig_multiplier, None);

    //    Active MJFG with spool=4.0 > 0.0 -> does not overwrite (kills `<=` -> `>` and 2nd `&&` -> `||`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mjfg_id);
    });
    sys_h.try_update_combat_state(ship_id, |c| {
        c.mjfg_spool_remaining_s = 4.0;
        c.mjfg_sig_multiplier = None;
    });
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs_mj1 = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_mj1.mjfg_spool_remaining_s - 4.0).abs() < 1e-6);
    assert_eq!(cs_mj1.mjfg_sig_multiplier, None);

    //    Active MJFG with spool=0.0 and existing sig_multiplier=Some(1.75) -> does not overwrite
    sys_h.try_update_combat_state(ship_id, |c| {
        c.mjfg_spool_remaining_s = 0.0;
        c.mjfg_sig_multiplier = Some(1.75);
    });
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs_mj2 = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_mj2.mjfg_sig_multiplier, Some(1.75));

    //    Active MJFG with spool=0.0 and sig=None -> arms spool=9.0 and sig=Some(2.5)
    sys_h.try_update_combat_state(ship_id, |c| {
        c.mjfg_spool_remaining_s = 0.0;
        c.mjfg_sig_multiplier = None;
    });
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs_mj3 = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_mj3.mjfg_spool_remaining_s - 9.0).abs() < 1e-6);
    assert_eq!(cs_mj3.mjfg_sig_multiplier, Some(2.5));
}
