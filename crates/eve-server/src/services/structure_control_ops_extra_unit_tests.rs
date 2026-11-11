//! Extra unit and mutation-killing tests for `structure_control_ops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
};
use crate::services::structure_control_ops::*;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_sde::{InvType, SdeRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "structure_control_boarding_and_defense_unit_tests.rs"]
mod structure_control_boarding_and_defense_unit_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn clone_type_with(
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
    rec.name = format!("ExtraCtrlType_{new_tid}");
    rec.radius = 5_000.0;
    rec.mass = 1.0e11;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

pub(super) fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    ship_id: Option<i64>,
    structure_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(720);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = ship_id;
    s.structure_id = structure_id;
    s.corp_role = role;
    s
}

pub(super) fn seed_pilot_and_structure(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    ally_id: Option<i64>,
    sys_id: u32,
    ship_id: i64,
    sid: i64,
    st_tid: u32,
    is_ceo: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap
            .corporations
            .entry(corp_id)
            .or_insert_with(|| { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = format!("Corp {corp_id}");
                __corp.ticker = "CTL".into();
                __corp.ceo_id = if is_ceo { char_id } else { char_id + 500 };
                __corp.alliance_id = ally_id;
                __corp.hq_station_id = Some(60003760);
                __corp.tax_rate = 0.0;
                __corp.members = HashSet::from([char_id]);
                __corp.set_division_balance(1, IskCents(0));
                __corp });
        corp.members.insert(char_id);
        if is_ceo {
            corp.member_roles.insert(char_id, u64::MAX);
        }
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = ally_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = Some(sid);
        ch.active_ship_id = ship_id;
        ch.home_station_id = 60003760;
        ch.balance = IskCents(100_000);
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 600,
                owner_id: char_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.structures
            .entry(sid)
            .or_insert_with(|| UpwellStructureRecord {
                structure_id: sid,
                type_id: st_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Control Citadel".into(),
                position: [1_000_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            });
    });
}

#[test]
fn can_character_control_structure_with_profile_acl_and_block_matrix() {
    let ctx = test_ctx();
    let sid = 10_001_100_001i64;
    let corp_id = 98_601i64;
    let ally_id = 99_001_601i64;
    let char_id = 6001i64;
    seed_pilot_and_structure(
        &ctx,
        char_id,
        corp_id,
        Some(ally_id),
        30000142,
        70_001,
        sid,
        35832,
        false,
    );

    // Non-owning corp without defense ACL returns false
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let st = &snap.structures[&sid];
        assert!(!can_character_control_structure_with_profile(
            snap,
            &ext,
            st,
            None,
            char_id + 999
        ));
    });

    // Profile explicit character / corp / alliance blocks
    for mode in 0..4 {
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let mut prof = StructureAccessProfile {
                profile_id: 700,
                owner_corp_id: corp_id,
                defense_allowed_characters: HashSet::from([char_id]),
                ..Default::default()
            };
            match mode {
                0 => {
                    prof.defense_blocked_characters.insert(char_id);
                }
                1 => {
                    prof.blocked_characters.insert(char_id);
                }
                2 => {
                    prof.blocked_corporations.insert(corp_id);
                }
                _ => {
                    prof.blocked_alliances.insert(ally_id);
                }
            }
            ext.access_profiles.insert(sid, prof);
            save_ext_state(snap, &ext);
            let st = &snap.structures[&sid];
            assert!(!can_character_control_structure_with_profile(
                snap,
                &ext,
                st,
                ext.access_profiles.get(&sid),
                char_id,
            ));
        });
    }

    // Defense group -1 block for character, corp, or alliance overrides defense_allowed_characters
    for blocked_entity in [char_id, corp_id, ally_id] {
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let mut grp = AccessGroupRecord {
                group_id: 55,
                owner_corp_id: corp_id,
                ..Default::default()
            };
            grp.members.insert(blocked_entity, -1);
            ext.owner_groups.insert(55, grp);
            let prof = StructureAccessProfile {
                profile_id: 701,
                owner_corp_id: corp_id,
                defense_group_ids: HashSet::from([55]),
                defense_allowed_characters: HashSet::from([char_id]),
                ..Default::default()
            };
            ext.access_profiles.insert(sid, prof);
            save_ext_state(snap, &ext);
            let st = &snap.structures[&sid];
            assert!(!can_character_control_structure_with_profile(
                snap,
                &ext,
                st,
                ext.access_profiles.get(&sid),
                char_id,
            ));
        });
    }

    // Positive defense group grants via public(0), char, corp, and alliance even for non-owning corp
    for granted_entity in [0i64, char_id, corp_id, ally_id] {
        ctx.db.with_mut_snapshot(|snap| {
            let mut ext = load_ext_state(snap);
            let mut grp = AccessGroupRecord {
                group_id: 56,
                owner_corp_id: corp_id,
                ..Default::default()
            };
            grp.members.insert(granted_entity, 0);
            ext.owner_groups.insert(56, grp);
            let prof = StructureAccessProfile {
                profile_id: 702,
                owner_corp_id: corp_id,
                defense_group_ids: HashSet::from([56]),
                ..Default::default()
            };
            ext.access_profiles.insert(sid, prof);
            save_ext_state(snap, &ext);
            let mut other_st = snap.structures[&sid].clone();
            other_st.owner_corp_id = corp_id + 999;
            assert!(can_character_control_structure_with_profile(
                snap,
                &ext,
                &other_st,
                ext.access_profiles.get(&sid),
                char_id,
            ));
        });
    }

    // Non-empty defense_group_ids without match denies pilot without corp roles
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let grp = AccessGroupRecord {
            group_id: 57,
            owner_corp_id: corp_id,
            ..Default::default()
        };
        ext.owner_groups.insert(57, grp);
        let prof = StructureAccessProfile {
            profile_id: 703,
            owner_corp_id: corp_id,
            defense_group_ids: HashSet::from([57]),
            ..Default::default()
        };
        ext.access_profiles.insert(sid, prof);
        save_ext_state(snap, &ext);
        let st = &snap.structures[&sid];
        assert!(!can_character_control_structure_with_profile(
            snap,
            &ext,
            st,
            ext.access_profiles.get(&sid),
            char_id,
        ));
    });
}

#[tokio::test(flavor = "current_thread")]
async fn controller_release_session_sync_capsule_reuse_and_auto_defense_weapon_branches() {
    let mut ctx = test_ctx();
    let sys_id = 30_002_537_u32; // Lowsec so standing/ACL auto-defense triggers
    let st_tid = 930_201_u32;
    let pdb_tid = 930_202_u32;
    let missile_mod_tid = 930_203_u32;
    let missile_charge_tid = 930_204_u32;
    let turret_mod_tid = 930_205_u32;

    clone_type_with(&mut ctx.sde, 35832, st_tid, 1657, 65, &[(76, 350_000.0), (564, 40.0), (1547, 2.0)]);
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let base_mod = sde.types.values().find(|t| t.category_id == 66).cloned().unwrap();
        let mut pdb = base_mod.clone();
        pdb.type_id = pdb_tid;
        pdb.group_id = 1330; // Standup Point Defense Battery
        pdb.category_id = 66;
        pdb.default_attributes = HashMap::from([(51_u16, 1000.0), (64, 1.0), (114, 100.0)]);
        sde.types.insert(pdb_tid, pdb);

        let mut mmod = base_mod.clone();
        mmod.type_id = missile_mod_tid;
        mmod.group_id = 1327;
        mmod.category_id = 66;
        mmod.default_attributes = HashMap::from([(51_u16, 2000.0), (64, 1.0)]);
        sde.types.insert(missile_mod_tid, mmod);

        let mut tmod = base_mod;
        tmod.type_id = turret_mod_tid;
        tmod.group_id = 1328;
        tmod.category_id = 66;
        tmod.default_attributes = HashMap::from([
            (51_u16, 1000.0),
            (64, 1.0),
            (114, 5_000.0),
            (160, 0.5),
            (620, 40_000.0),
            (54, 150_000.0),
            (158, 50_000.0),
        ]);
        sde.types.insert(turret_mod_tid, tmod);

        let mut mcharge = sde.types.values().find(|t| t.category_id == 8).cloned().unwrap();
        mcharge.type_id = missile_charge_tid;
        mcharge.category_id = 8;
        mcharge.default_attributes = HashMap::from([
            (114_u16, 200.0),
            (653, 500.0),
            (654, 100.0),
            (1353, 0.944),
            (37, 1000.0),
            (281, 5000.0),
        ]);
        sde.types.insert(missile_charge_tid, mcharge);
    }

    let sid = 10_001_300_001_i64;
    let ceo_id = 6201_i64;
    let hostile1 = 6202_i64;
    let hostile2 = 6203_i64;
    let corp_id = 98_620_i64;
    let h_ship1 = 72_001_i64;
    let h_ship2 = 72_002_i64;
    let existing_pod = 72_099_i64;

    seed_pilot_and_structure(&ctx, ceo_id, corp_id, None, sys_id, 72_000, sid, st_tid, true);
    ctx.db.with_mut_snapshot(|snap| {
        // Seed existing capsule in structure Hangar for docked reuse check + unauthorized controller
        snap.items.insert(
            existing_pod,
            InventoryItemRecord {
                item_id: existing_pod,
                type_id: 670,
                owner_id: ceo_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.get_mut(&sid).unwrap().position = [0.0, 0.0, 0.0];
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, ceo_id);
        save_ext_state(snap, &ext);
    });

    let mut ceo_sess = make_session(ceo_id, corp_id, sys_id, Some(sid), Some(sid), 1);
    // Test forced corp / ship session sync
    crate::services::fleet_social::fleet_social_persistence::with_social_state(&ctx, |s| {
        s.forced_session_corp_sync.insert(ceo_id, (corp_id, Some(99_620), 1));
        s.forced_session_ship_sync.insert(ceo_id, (sid, None));
    });
    sync_character_session(&ctx, &mut ceo_sess);
    assert_eq!(ceo_sess.alliance_id, Some(99_620));

    // release_all_structure_control_for_character restores previous ship when present; then without previous ship reuses existing_pod!
    let released = release_all_structure_control_for_character(&ctx, Some(&mut ceo_sess), ceo_id);
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].2, 72_000);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, ceo_id);
        ext.previous_ship_by_structure.remove(&sid);
        snap.items.remove(&72_000);
        save_ext_state(snap, &ext);
    });
    let released_pod = release_all_structure_control_for_character(&ctx, Some(&mut ceo_sess), ceo_id);
    assert_eq!(released_pod.len(), 1);
    assert_eq!(released_pod[0].2, existing_pod);

    // Unauthorized controller revocation in snap when pilot loses corp membership
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, ceo_id);
        save_ext_state(snap, &ext);
        snap.characters.get_mut(&ceo_id).unwrap().corporation_id = 98_999;
    });
    let revoked = ctx.db.with_mut_snapshot(release_unauthorized_structure_controllers_in_snap);
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].0, sid);

    // Seed auto-defense weapons (PDB, Missile with 1 charge, Turret) + 2 hostile ships (one fragile, one far/tanky)
    ctx.db.with_mut_snapshot(|snap| {
        let mut def = crate::services::structure_acl_defense_rules::load_defense_settings(snap, sid);
        def.auto_defense_enabled = true;
        def.standing_threshold = 0.5;
        def.target_blocked_acl = true;
        crate::services::structure_acl_defense_rules::save_defense_settings(snap, &def);

        for (cid, ship, tid) in [(hostile1, h_ship1, 587_u32), (hostile2, h_ship2, 587)] {
            let mut ch = snap.characters[&ceo_id].clone();
            ch.character_id = cid;
            ch.corporation_id = 98_777;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                ship,
                InventoryItemRecord {
                    item_id: ship,
                    type_id: tid,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        // Weapons: PDB (HiSlot0), Missile (HiSlot1 + 1 charge), Turret (HiSlot2)
        for (iid, tid, flag, qty) in [
            (72_101, pdb_tid, InventoryFlag::HiSlot0, 1),
            (72_102, missile_mod_tid, InventoryFlag::HiSlot1, 1),
            (72_103, missile_charge_tid, InventoryFlag::HiSlot1, 1),
            (72_104, turret_mod_tid, InventoryFlag::HiSlot2, 1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: sid,
                    flag,
                    quantity: qty,
                    singleton: qty == 1 && tid != missile_charge_tid,
                    ..Default::default()
                },
            );
            if tid != missile_charge_tid {
                snap.module_online_states.insert(iid, true);
            }
        }
        let mut ext = load_ext_state(snap);
        ext.weapon_cooldowns.insert(72_101, 0.5);
        save_ext_state(snap, &ext);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    // Stale lock for ship 72_999 that is no longer on grid -> pruned
    let _ = sys
        .add_target_lock(
            sid,
            eve_destiny::TargetLock {
                target_id: 72_999,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    // Fragile hostile1 at 20 km (destroyed on shot!), hostile2 at 2 km
    for (bid, x, hp) in [(h_ship1, 20_000.0, 5.0), (h_ship2, 2_000.0, 50_000.0)] {
        let _ = sys
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: 587,
                    position: DVec3::new(x, 0.0, 0.0),
                    velocity: DVec3::new(0.0, 100.0, 0.0),
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1e6,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                eve_destiny::CombatState {
                    shield_hp: hp,
                    max_shield_hp: hp,
                    armor_hp: hp,
                    max_armor_hp: hp,
                    structure_hp: hp,
                    max_structure_hp: hp,
                    signature_radius: 100.0,
                    ..eve_destiny::CombatState::new()
                },
            )
            .await;
    }

    // Step 1: acquires lock on closest (h_ship2) and fires PDB (no lock needed)
    step_structure_auto_defense(&ctx, sys_id, 2.0).await;
    // Step 2: lock h_ship1 by moving h_ship2 out of range so h_ship1 is primary and gets destroyed
    let _ = sys.remove_ball(h_ship2).await;
    let _ = sys
        .add_target_lock(
            sid,
            eve_destiny::TargetLock {
                target_id: h_ship1,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    step_structure_auto_defense(&ctx, sys_id, 30.0).await;
    assert!(sys.get_combat_state(h_ship1).await.unwrap().is_none(), "fragile ship destroyed by auto-defense");

    // Step 3: no hostiles left -> weapon cooldowns decay
    step_structure_auto_defense(&ctx, sys_id, 5.0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flex_take_control_manager_takeover_and_destroyed_structure_error_branches() {
    let ctx = test_ctx();
    let sys_id = 30_002_537_u32;
    let ceo_id = 6211_i64;
    let mgr_id = 6212_i64;
    let corp_id = 98_621_i64;
    let flex_tid = 35_841_u32; // Ansiblex Jump Gate (HullClass::Flex)
    let st_tid = 35_832_u32;
    let flex_sid = 10_001_301_001_i64;
    let dead_sid = 10_001_301_002_i64;
    let nocore_sid = 10_001_301_003_i64;
    let ship_id = 72_110_i64;

    seed_pilot_and_structure(&ctx, ceo_id, corp_id, None, sys_id, ship_id, flex_sid, flex_tid, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            dead_sid,
            UpwellStructureRecord {
                structure_id: dead_sid,
                type_id: st_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                state: "Destroyed".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                ..Default::default()
            },
        );
        snap.structures.insert(
            nocore_sid,
            UpwellStructureRecord {
                structure_id: nocore_sid,
                type_id: st_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                state: "Unanchored".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                ..Default::default()
            },
        );
        let mut mgr_ch = snap.characters[&ceo_id].clone();
        mgr_ch.character_id = mgr_id;
        mgr_ch.structure_id = Some(flex_sid);
        snap.characters.insert(mgr_id, mgr_ch);
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(mgr_id, eve_db::corp_roles::ROLE_STATION_MANAGER);
        snap.characters.get_mut(&ceo_id).unwrap().structure_id = None;
        snap.characters.get_mut(&ceo_id).unwrap().station_id = None;
    });

    // Destroyed & non-operational structure rejection
    let mut sess = make_session(ceo_id, corp_id, sys_id, Some(ship_id), None, eve_db::corp_roles::ROLE_DIRECTOR);
    assert_eq!(handle_take_control(&ctx, &mut sess, &[eve_proto::PyRep::Integer(dead_sid)]).await.unwrap_err().code, "StructureDestroyed");
    assert_eq!(handle_take_control(&ctx, &mut sess, &[eve_proto::PyRep::Integer(nocore_sid)]).await.unwrap_err().code, "StructureNotOperational");

    // FLEX structure checks: NoActiveShip, wrong solarSystemID, no position, too far, then within proximity
    let mut no_ship = make_session(ceo_id, corp_id, sys_id, None, None, 0);
    assert_eq!(handle_take_control(&ctx, &mut no_ship, &[eve_proto::PyRep::Integer(flex_sid)]).await.unwrap_err().code, "NoActiveShip");
    let mut wrong_sys = make_session(ceo_id, corp_id, 30_000_142, Some(ship_id), None, 0);
    assert_eq!(handle_take_control(&ctx, &mut wrong_sys, &[eve_proto::PyRep::Integer(flex_sid)]).await.unwrap_err().code, "OutOfBoardingRange");
    assert_eq!(handle_take_control(&ctx, &mut sess, &[eve_proto::PyRep::Integer(flex_sid)]).await.unwrap_err().code, "OutOfBoardingRange");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(handle_take_control(&ctx, &mut sess, &[eve_proto::PyRep::Integer(flex_sid)]).await.unwrap_err().code, "OutOfBoardingRange");

    // Move ship inside FLEX proximity (e.g. 500 m) and assume control; then another manager cannot steal from manager, while CEO/non-manager takeover rules apply
    let _ = sys.remove_ball(ship_id).await;
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(1_000_000_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let res = handle_take_control(&ctx, &mut sess, &[eve_proto::PyRep::Integer(flex_sid)]).await.unwrap();
    assert_ne!(res, eve_proto::PyRep::None);

    // Another manager blocked while CEO holds control (StructureAlreadyControlled)
    let mut mgr_sess = make_session(mgr_id, corp_id, sys_id, Some(ship_id), Some(flex_sid), eve_db::corp_roles::ROLE_STATION_MANAGER);
    assert_eq!(handle_take_control(&ctx, &mut mgr_sess, &[eve_proto::PyRep::Integer(flex_sid)]).await.unwrap_err().code, "StructureAlreadyControlled");

    // prepare_ship_for_undock_while_controlling & rollback_ship_after_failed_undock + fresh capsule creation when no pod in hangar
    let docked_sid = 10_001_301_009_i64;
    let non_mgr = 6213_i64;
    let fallback_ship = 72_111_i64;
    seed_pilot_and_structure(&ctx, non_mgr, corp_id, None, sys_id, fallback_ship, docked_sid, st_tid, false);
    assert_eq!(prepare_ship_for_undock_while_controlling(&ctx, docked_sid, non_mgr), None);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(docked_sid, non_mgr);
        save_ext_state(snap, &ext);
        snap.characters.get_mut(&non_mgr).unwrap().active_ship_id = docked_sid;
    });
    assert_eq!(prepare_ship_for_undock_while_controlling(&ctx, docked_sid, non_mgr), Some(fallback_ship));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&non_mgr].active_ship_id), fallback_ship);
    rollback_ship_after_failed_undock(&ctx, docked_sid, non_mgr);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&non_mgr].active_ship_id), docked_sid);

    // Release control with NO fallback ships/pods in hangar provisions a brand-new capsule item
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&fallback_ship);
    });
    let rel_new_pod = release_all_structure_control_for_character(&ctx, None, non_mgr);
    assert_eq!(rel_new_pod.len(), 1);
    assert!(ctx.db.with_snapshot(|s| s.items.get(&rel_new_pod[0].2).is_some_and(|i| i.type_id == 670)));

    // Manager displaces non-manager controller when ACL grants non-manager control
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile { profile_id: 621, owner_corp_id: corp_id, ..Default::default() };
        prof.allowed_characters.insert(non_mgr);
        ext.access_profiles.insert(docked_sid, prof);
        ext.active_pilot_by_structure.insert(docked_sid, non_mgr);
        save_ext_state(snap, &ext);
        snap.characters.get_mut(&mgr_id).unwrap().structure_id = Some(docked_sid);
    });
    mgr_sess.structure_id = Some(docked_sid);
    assert!(handle_take_control(&ctx, &mut mgr_sess, &[eve_proto::PyRep::Integer(docked_sid)]).await.is_ok());

    // step_structure_auto_defense auto-spawns missing structure ball & ignores cloaked/out-of-range targets
    let cloaked_ship = 72_112_i64;
    let far_ship = 72_113_i64;
    let mut def_sess = make_session(non_mgr, corp_id, sys_id, Some(cloaked_ship), None, 0);
    let _ = handle_eject_from_structure(&ctx, &mut def_sess, &[eve_proto::PyRep::Integer(999_999)]).unwrap_err();
    let _ = sys.spawn_ball_with_combat(
        Ball {
            id: cloaked_ship, type_id: 587, position: DVec3::new(1_000_005_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        eve_destiny::CombatState { is_cloaked: true, ..eve_destiny::CombatState::new() },
    ).await;
    let _ = sys.spawn_ball_with_combat(
        Ball {
            id: far_ship, type_id: 587, position: DVec3::new(0.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        eve_destiny::CombatState::new(),
    ).await;
    step_structure_auto_defense(&ctx, sys_id, 1.0).await;
}

#[tokio::test]
async fn control_ops_role_takeover_release_session_fallback_and_weapon_tick_mutants() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let st_tid = 931_101_u32;
    let pdb_tid = 931_201_u32;
    clone_type_with(
        &mut ctx.sde,
        35832,
        st_tid,
        1657,
        65,
        &[(76, 350_000.0), (564, 40.0), (1547, 2.0)],
    );
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let mut pdb = sde.types.values().find(|t| t.category_id == 66).cloned().unwrap();
        pdb.type_id = pdb_tid;
        pdb.group_id = 1330;
        pdb.category_id = 66;
        pdb.default_attributes = HashMap::from([(51_u16, 1000.0), (64, 1.0), (114, 500.0)]);
        sde.types.insert(pdb_tid, pdb);
    }

    let sid = 10_001_311_001_i64;
    let corp_id = 98_631_i64;
    let ceo_id = 6301_i64;
    let director_id = 6302_i64;
    let mgr_id = 6303_i64;
    let mgr2_id = 6304_i64;
    let acct_id = 6305_i64;
    let outsider_id = 6306_i64;
    let fallback_ship = 73_101_i64;
    let other_ship = 73_102_i64;

    seed_pilot_and_structure(&ctx, ceo_id, corp_id, None, sys_id, 73_000, sid, st_tid, true);
    for cid in [director_id, mgr_id, mgr2_id, acct_id] {
        seed_pilot_and_structure(&ctx, cid, corp_id, None, sys_id, fallback_ship, sid, st_tid, false);
    }

    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&outsider_id) {
            let mut ch = snap.characters[&ceo_id].clone();
            ch.character_id = outsider_id;
            ch.corporation_id = 98_998;
            snap.characters.insert(outsider_id, ch);
        }
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.member_roles.insert(director_id, eve_db::corp_roles::ROLE_DIRECTOR);
        corp.member_roles.insert(mgr_id, eve_db::corp_roles::ROLE_STATION_MANAGER);
        corp.member_roles.insert(mgr2_id, eve_db::corp_roles::ROLE_STATION_MANAGER);
        corp.member_roles.insert(acct_id, eve_db::corp_roles::ROLE_ACCOUNTANT);

        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile {
            profile_id: 631,
            owner_corp_id: corp_id,
            ..Default::default()
        };
        prof.defense_allowed_characters.insert(acct_id);
        ext.access_profiles.insert(sid, prof.clone());
        save_ext_state(snap, &ext);

        let st = snap.structures.get(&sid).unwrap().clone();
        // Direct check on verify_control_acl_or_role_with_profile: denied vs granted
        assert_eq!(
            super::super::verify_control_acl_or_role_with_profile(snap, &ext, &st, None, outsider_id)
                .unwrap_err()
                .code,
            "DefenseAccessDenied"
        );
        assert!(
            super::super::verify_control_acl_or_role_with_profile(snap, &ext, &st, Some(&prof), acct_id)
                .is_ok()
        );
    });

    // 1. Direct check on engage_structure_control returns exact non-0/non-1 solarSystemID (30_000_142)
    let mut dir_sess = make_session(
        director_id,
        corp_id,
        sys_id,
        Some(fallback_ship),
        Some(sid),
        eve_db::corp_roles::ROLE_DIRECTOR,
    );
    let engaged_sys = super::super::engage_structure_control(&ctx, &mut dir_sess, sid, director_id, false)
        .await
        .expect("director engages structure control");
    assert_eq!(engaged_sys, sys_id);

    // 2. Role holder protection:
    //    - holder with ROLE_DIRECTOR only blocks takeover by manager
    //    - holder with ROLE_STATION_MANAGER only blocks takeover by another manager
    //    - holder with ROLE_ACCOUNTANT only (ACL allowed) IS displaced by manager
    let mut mgr_sess = make_session(
        mgr_id,
        corp_id,
        sys_id,
        Some(fallback_ship),
        Some(sid),
        eve_db::corp_roles::ROLE_STATION_MANAGER,
    );
    assert_eq!(
        handle_take_control(&ctx, &mut mgr_sess, &[eve_proto::PyRep::Integer(sid)])
            .await
            .unwrap_err()
            .code,
        "StructureAlreadyControlled"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, mgr2_id);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_take_control(&ctx, &mut mgr_sess, &[eve_proto::PyRep::Integer(sid)])
            .await
            .unwrap_err()
            .code,
        "StructureAlreadyControlled"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, acct_id);
        save_ext_state(snap, &ext);
    });
    assert!(
        handle_take_control(&ctx, &mut mgr_sess, &[eve_proto::PyRep::Integer(sid)])
            .await
            .is_ok()
    );

    // 3. disengage_structure_control_in_snap exact tuple & sync_released_ship_on_session boundaries
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, acct_id);
        ext.previous_ship_by_structure.insert(sid, fallback_ship);
        save_ext_state(snap, &ext);
        let pair = super::super::disengage_structure_control_in_snap(Some(&ctx), snap, sid, acct_id, None);
        assert_eq!(pair, (sys_id, fallback_ship));
    });

    let mut rel_sess = make_session(acct_id, corp_id, sys_id, Some(sid), Some(sid), 0);
    super::super::sync_released_ship_on_session(&ctx, &mut rel_sess, acct_id, sid, 77_001);
    assert_eq!(rel_sess.ship_id, Some(77_001));

    rel_sess.ship_id = Some(sid);
    super::super::sync_released_ship_on_session(&ctx, &mut rel_sess, acct_id, sid, 0);
    assert_eq!(rel_sess.ship_id, None);

    rel_sess.ship_id = Some(other_ship);
    super::super::sync_released_ship_on_session(&ctx, &mut rel_sess, acct_id, sid, 0);
    assert_eq!(rel_sess.ship_id, Some(other_ship));

    // 4. prepare_ship_for_undock_while_controlling & rollback_ship_after_failed_undock boundaries
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(sid, acct_id);
        save_ext_state(snap, &ext);
        snap.items.insert(
            other_ship,
            InventoryItemRecord {
                item_id: other_ship,
                type_id: 587,
                owner_id: acct_id,
                location_id: sid,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // active_ship_id == 0 -> switched to fallback_ship
        snap.characters.get_mut(&acct_id).unwrap().active_ship_id = 0;
    });
    assert_eq!(
        prepare_ship_for_undock_while_controlling(&ctx, sid, acct_id),
        Some(fallback_ship)
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&acct_id].active_ship_id), fallback_ship);

    // active_ship_id < 0 -> switched to fallback_ship
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&acct_id).unwrap().active_ship_id = -5;
    });
    assert_eq!(
        prepare_ship_for_undock_while_controlling(&ctx, sid, acct_id),
        Some(fallback_ship)
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&acct_id].active_ship_id), fallback_ship);

    // active_ship_id == other_ship > 0 (not sid) -> preserved as other_ship
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&acct_id).unwrap().active_ship_id = other_ship;
    });
    assert_eq!(
        prepare_ship_for_undock_while_controlling(&ctx, sid, acct_id),
        Some(fallback_ship)
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&acct_id].active_ship_id), other_ship);

    // Non-controlling pilot rollback_ship_after_failed_undock -> no-op
    rollback_ship_after_failed_undock(&ctx, sid, outsider_id);
    assert_ne!(ctx.db.with_snapshot(|s| s.characters[&outsider_id].active_ship_id), sid);

    // fallback_ship == 0 (no hangar fallback ship) -> returns Some(0) and does not overwrite active_ship_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&fallback_ship);
        snap.items.remove(&other_ship);
        snap.characters.get_mut(&acct_id).unwrap().active_ship_id = 0;
    });
    assert_eq!(prepare_ship_for_undock_while_controlling(&ctx, sid, acct_id), Some(0));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&acct_id].active_ship_id), 0);

    // 5. sync_character_session: ch.active_ship_id == 0 with stale structure session.ship_id clears to None,
    //    while ch.active_ship_id != 0 overrides session.ship_id
    let mut drain_sess = make_session(acct_id, corp_id, sys_id, Some(sid), Some(sid), 0);
    super::super::apply_forced_session_ship_sync(&ctx, &mut drain_sess);
    let mut sync_sess = make_session(acct_id, corp_id, sys_id, Some(sid), Some(sid), 0);
    sync_character_session(&ctx, &mut sync_sess);
    assert_eq!(sync_sess.ship_id, None);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&acct_id).unwrap().active_ship_id = 77_002;
    });
    let mut sync_sess2 = make_session(acct_id, corp_id, sys_id, Some(sid), Some(sid), 0);
    sync_character_session(&ctx, &mut sync_sess2);
    assert_eq!(sync_sess2.ship_id, Some(77_002));

    // 6. Direct resolve_structure_weapon_tick returns exact non-empty DamageVector map for in-range target
    let target_ship_id = 73_999_i64;
    let dmg_map = ctx.db.with_mut_snapshot(|snap| {
        let pdb_item = InventoryItemRecord {
            item_id: 83_901,
            type_id: pdb_tid,
            owner_id: corp_id,
            location_id: sid,
            flag: InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        let hostiles = [super::super::HostileTargetInfo {
            ship_id: target_ship_id,
            pilot_id: outsider_id,
            position: DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            sig_radius: 40.0,
            dist_m: 5_000.0,
        }];
        super::super::resolve_structure_weapon_tick(
            &ctx,
            snap,
            sid,
            DVec3::ZERO,
            &[pdb_item],
            &hostiles,
            target_ship_id,
            false,
            1.0,
        )
    });
    let dealt = dmg_map.get(&target_ship_id).expect("PDB must record damage for hostile target");
    assert!((dealt.em - 500.0).abs() < 1e-6);
}
