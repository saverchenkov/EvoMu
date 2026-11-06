//! Extra unit and mutation-killing tests for `structure_control_ops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
    StructureLifecycleState,
};
use crate::services::structure_acl_defense_rules::{save_defense_settings, StructureDefenseSettings};
use crate::services::structure_control_ops::*;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashSet;
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
    rec.name = format!("ExtraCtrlType_{new_tid}");
    rec.radius = 5_000.0;
    rec.mass = 1.0e11;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_session(
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

fn seed_pilot_and_structure(
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
            .or_insert_with(|| CorporationRecord {
                corporation_id: corp_id,
                name: format!("Corp {corp_id}"),
                ticker: "CTL".into(),
                ceo_id: if is_ceo { char_id } else { char_id + 500 },
                alliance_id: ally_id,
                hq_station_id: Some(60003760),
                tax_rate: 0.0,
                master_wallet: IskCents(0),
                members: HashSet::from([char_id]),
                ..Default::default()
            });
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
async fn take_control_board_and_release_edge_cases_and_fallback_ships() {
    let mut ctx = test_ctx();
    let st_tid = 930_101u32;
    let flex_tid = 930_102u32;
    clone_type_with(&mut ctx.sde, 35832, st_tid, 1657, 65, &[(1547, 2.0)]);
    clone_type_with(&mut ctx.sde, 35841, flex_tid, 1406, 65, &[(14, 2.0)]);

    let sid = 10_001_200_001i64;
    let flex_sid = 10_001_200_002i64;
    seed_pilot_and_structure(&ctx, 6101, 98_610, None, 30000142, 71_001, sid, st_tid, true);
    seed_pilot_and_structure(
        &ctx,
        6102,
        98_610,
        None,
        30000142,
        71_002,
        sid,
        st_tid,
        false,
    );
    seed_pilot_and_structure(
        &ctx,
        6103,
        98_610,
        None,
        30000142,
        71_003,
        flex_sid,
        flex_tid,
        true,
    );

    // Unauthenticated & MissingStructureId & StructureNotFound
    let mut unauth = SessionState::new(1);
    assert!(handle_take_control(&ctx, &mut unauth, &[PyRep::Integer(sid)]).await.is_err());
    let mut ceo = make_session(6101, 98_610, 30000142, Some(71_001), Some(sid), 1);
    let mut undocked_ceo = make_session(6101, 98_610, 30000142, Some(71_001), None, 1);
    assert_eq!(handle_take_control(&ctx, &mut undocked_ceo, &[]).await.unwrap_err().code, "MissingStructureId");
    assert_eq!(handle_take_control(&ctx, &mut ceo, &[PyRep::Integer(999_888_777)]).await.unwrap_err().code, "StructureNotFound");

    // StructureDestroyed (state == "Destroyed")
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&sid).unwrap().state = "Destroyed".into());
    assert_eq!(handle_take_control(&ctx, &mut ceo, &[PyRep::Integer(sid)]).await.unwrap_err().code, "StructureDestroyed");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&sid).unwrap().state = "ShieldVulnerable".into());

    // NotDockedInStructure for dockable structure when session.structure_id != Some(sid)
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&6101).unwrap().structure_id = None);
    assert_eq!(handle_take_control(&ctx, &mut undocked_ceo, &[PyRep::Integer(sid)]).await.unwrap_err().code, "NotDockedInStructure");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&6101).unwrap().structure_id = Some(sid));

    // FLEX boarding: NoActiveShip, wrong solar system, and item_positions fallback
    let mut flex_no_ship = make_session(6103, 98_610, 30000142, None, None, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&6103).unwrap().structure_id = None;
        snap.characters.get_mut(&6103).unwrap().active_ship_id = 0;
    });
    assert_eq!(handle_board_structure(&ctx, &mut flex_no_ship, &[PyRep::Integer(flex_sid)]).await.unwrap_err().code, "NoActiveShip");

    let mut flex_wrong_sys = make_session(6103, 98_610, 30002187, Some(71_003), None, 1);
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&6103).unwrap().active_ship_id = 71_003);
    assert_eq!(handle_board_structure(&ctx, &mut flex_wrong_sys, &[PyRep::Integer(flex_sid)]).await.unwrap_err().code, "OutOfBoardingRange");

    // FLEX item_positions fallback within range -> boards; then board_mode by another manager fails with StructureAlreadyControlled
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(71_003, [1_000_005_000.0, 0.0, 0.0]);
        snap.item_positions.insert(71_001, [1_000_005_000.0, 0.0, 0.0]);
        snap.characters.get_mut(&6101).unwrap().structure_id = None;
    });
    let mut flex_sess = make_session(6103, 98_610, 30000142, Some(71_003), None, 1);
    let _ = handle_board_structure(&ctx, &mut flex_sess, &[PyRep::Integer(flex_sid)]).await.expect("board via item_positions");
    assert_eq!(handle_board_structure(&ctx, &mut undocked_ceo, &[PyRep::Integer(flex_sid)]).await.unwrap_err().code, "StructureAlreadyControlled");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&6101).unwrap().structure_id = Some(sid));

    // Gunner immobilized blocks release control and eject (keyed by structure_id flex_sid)
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.gunner_immobilized_until_unix.insert(flex_sid, ctx.now_unix() + 120);
        save_ext_state(snap, &ext);
    });
    assert_eq!(handle_release_control(&ctx, &mut flex_sess, &[PyRep::Integer(flex_sid)]).unwrap_err().code, "GunnerImmobilized");
    flex_sess.role = service_roles::ROLE_GMH;
    assert_eq!(handle_eject_from_structure(&ctx, &mut flex_sess, &[PyRep::Integer(flex_sid)]).unwrap_err().code, "GunnerImmobilized");

    // Clear immobilization & remove original ship 71_003 so in-space release provisions a capsule & spawns ball
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.gunner_immobilized_until_unix.remove(&flex_sid);
        save_ext_state(snap, &ext);
        snap.items.remove(&71_003);
    });
    let _ = handle_release_control(&ctx, &mut flex_sess, &[PyRep::Integer(flex_sid)]).expect("release flex to capsule");
    let new_pod_id = flex_sess.ship_id.expect("provisioned pod");
    assert_ne!(new_pod_id, flex_sid);

    // Docked release fallback: remove 71_001, place a locked ship + pod + regular ship in hangar -> picks regular ship
    let _ = handle_take_control(&ctx, &mut ceo, &[PyRep::Integer(sid)])
        .await
        .expect("ceo take docked");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&71_001);
        for (id, tid, flag) in [
            (71_901, 600u32, InventoryFlag::Locked),
            (71_902, 670u32, InventoryFlag::Hangar),
            (71_903, 600u32, InventoryFlag::Hangar),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: 6101,
                    location_id: sid,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Seed due pending profile application to verify GetStructurePilot applies it
        let mut ext = load_ext_state(snap);
        let prof = StructureAccessProfile {
            profile_id: 888,
            owner_corp_id: 98_610,
            name: "AppliedProf".into(),
            ..Default::default()
        };
        ext.profiles_by_id.insert(888, prof);
        ext.pending_profile_applications.insert(sid, (888, 1));
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_get_structure_pilot(&ctx, &ceo, &[PyRep::Integer(sid)]).expect("pilot"),
        PyRep::Integer(6101)
    );
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert_eq!(ext.structure_profile_id.get(&sid), Some(&888));
    });
    let _ = handle_release_control(&ctx, &mut ceo, &[PyRep::Integer(sid)]).expect("release");
    assert_eq!(ceo.ship_id, Some(71_903));

    // Forced session ship sync + sync_character_session helpers
    notify_released_structure_controllers(&ctx, &[(sid, 6101, 30000142, 71_902)]);
    apply_forced_session_ship_sync(&ctx, &mut ceo);
    assert_eq!(ceo.ship_id, Some(71_902));
    sync_character_session(&ctx, &mut ceo);
    assert_eq!(ceo.ship_id, Some(71_903));
}

#[tokio::test(flavor = "current_thread")]
async fn step_structure_auto_defense_directed_turret_and_missile_charges() {
    let mut ctx = test_ctx();
    let st_tid = 930_201u32;
    let turret_tid = 930_202u32;
    let missile_mod_tid = 930_203u32;
    let ammo_tid = 930_204u32;
    clone_type_with(&mut ctx.sde, 35832, st_tid, 1657, 65, &[]);
    // Directed Standup turret (non-PDB, non-vorton, non-missile)
    clone_type_with(
        &mut ctx.sde,
        35926,
        turret_tid,
        1320,
        66,
        &[
            (73, 1000.0),    // duration ms
            (54, 150_000.0), // maxRange
            (64, 20.0),      // damageMultiplier
            (114, 200.0),    // emDamage
        ],
    );
    // Standup missile launcher (group 511, chargeGroup1 = 384) + missile charge with flight range (37 * 281 / 1000)
    clone_type_with(
        &mut ctx.sde,
        35924,
        missile_mod_tid,
        511,
        66,
        &[(73, 1000.0), (54, 50_000.0), (64, 10.0), (604, 384.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        200,
        ammo_tid,
        384,
        8,
        &[
            (37, 5000.0),  // maxVelocity
            (281, 40_000.0), // explosionDelay ms -> 200km range
            (116, 500.0),  // explosiveDamage
        ],
    );

    let sid = 10_001_300_001i64;
    seed_pilot_and_structure(&ctx, 6301, 98_630, None, 30000142, 73_001, sid, st_tid, true);

    // dt <= 0.0 is a no-op
    step_structure_auto_defense(&ctx, 30000142, 0.0).await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut hostile = snap.characters[&6301].clone();
        hostile.character_id = 9951;
        hostile.corporation_id = 98_951;
        hostile.structure_id = None;
        hostile.active_ship_id = 73_099;
        snap.characters.insert(9951, hostile);
        for (id, tid, flag, qty) in [
            (85_001i64, turret_tid, InventoryFlag::HiSlot0, 1),
            (85_002i64, missile_mod_tid, InventoryFlag::HiSlot1, 1),
            (85_003i64, ammo_tid, InventoryFlag::HiSlot1, 1),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: 98_630,
                    location_id: sid,
                    flag,
                    quantity: qty,
                    singleton: id < 85_003,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        let mut ext = load_ext_state(snap);
        ext.lifecycle.insert(
            sid,
            StructureLifecycleState {
                online_modules: vec![85_001, 85_002],
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
        let mut ds = StructureDefenseSettings::default();
        ds.structure_id = sid;
        ds.auto_defense_enabled = true;
        ds.record_attacker(9951, ctx.now_unix());
        save_defense_settings(snap, &ds);
    });

    let sys = ctx.systems.get_or_spawn(30000142);
    let mut host_cs = CombatState::new();
    host_cs.shield_hp = 50_000.0;
    host_cs.max_shield_hp = 50_000.0;
    host_cs.armor_hp = 50_000.0;
    host_cs.max_armor_hp = 50_000.0;
    host_cs.structure_hp = 50_000.0;
    host_cs.max_structure_hp = 50_000.0;
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: 73_099,
                type_id: 600,
                position: DVec3::new(1_000_020_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            host_cs,
            SlimItemMeta {
                owner_id: 9951,
                char_id: Some(9951),
                corp_id: 98_951,
                alliance_id: -1,
                group_id: 25,
                category_id: 6,
                security_status: -5.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await;

    // Tick 1: directed weapons initiate target lock on 73_099 without firing
    step_structure_auto_defense(&ctx, 30000142, 1.5).await;
    let locks = sys.get_target_locks(sid).await.expect("locks");
    assert_eq!(locks.len(), 1);
    assert_eq!(locks[0].target_id, 73_099);

    // Complete target lock via step_manual and run Tick 2: locked directed weapons fire, damaging 73_099 and consuming the 1 missile charge (removing item 85_003)
    let _ = sys.step_manual(60.0).await;
    step_structure_auto_defense(&ctx, 30000142, 1.5).await;
    let after_cs = sys.get_combat_state(73_099).await.expect("ok").expect("cs");
    assert!(after_cs.shield_hp < 50_000.0);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&85_003));
    });
}
