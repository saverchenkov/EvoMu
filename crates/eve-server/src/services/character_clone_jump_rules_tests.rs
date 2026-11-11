//! Unit tests for `character_clone_jump_rules` (Omega/Alpha checks, structure clone bay ACLs & fees, cross-station & same-station clone jumps, wormhole/Pochven restrictions, and Titan/Rorqual Clone Vat Bay activation).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{save_ext_state, StructureAccessProfile};
use eve_db::types::{TrainedSkillRecord, UpwellStructureRecord};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, WorldStateSnapshot};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

fn put_item(
    snap: &mut WorldStateSnapshot,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            ..Default::default()
        },
    );
    item_id
}

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
    rec.name = format!("TestType_{new_tid}");
    rec.volume = 10.0;
    rec.capacity = 500.0;
    rec.mass = 1_000_000.0;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_session(
    user_id: u64,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
    structure_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(760);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.station_id = station_id;
    s.structure_id = structure_id;
    s.ship_id = ship_id;
    s
}

#[test]
fn alpha_capacity_and_structure_clone_bay_fees_and_acls() {
    let mut ctx = test_ctx();
    let struct_id = 1_040_888_001;

    ctx.db.with_mut_snapshot(|snap| {
        let alpha_id = {
            let alpha = snap.characters.values_mut().next().expect("char 1");
            alpha.clone_grade = 1;
            alpha.character_id
        };

        assert_eq!(
            check_alpha_clone_install_capacity(snap, alpha_id).unwrap_err().code,
            "OmegaCapacityRequired"
        );
        snap.characters.get_mut(&alpha_id).unwrap().clone_grade = 2;
        assert!(check_alpha_clone_install_capacity(snap, alpha_id).is_ok());

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 98_999_002,
                solar_system_id: 30000142,
                name: "FeeStructure".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                position: [0.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        assert_eq!(compute_structure_clone_fee(snap, struct_id, 6.0).0, 100);
        assert_eq!(compute_structure_clone_fee(snap, struct_id, 0.0).0, 500);
        assert_eq!(compute_structure_clone_fee(snap, struct_id, -2.0).0, 1000);

        let mut ext = structure_upwell_ops::load_ext_state(snap);
        let mut blocked = HashSet::new();
        blocked.insert(alpha_id);
        let mut prof = StructureAccessProfile {
            profile_id: 7001,
            name: "StrictProfile".into(),
            owner_corp_id: 98_999_002,
            blocked_characters: blocked,
            ..Default::default()
        };
        prof.general_settings.insert(2, false);
        ext.access_profiles.insert(struct_id, prof);
        save_ext_state(snap, &ext);

        let err = check_structure_clone_bay_access(snap, struct_id, alpha_id).unwrap_err();
        assert_eq!(err.code, "StructureAccessDenied");
    });
    let _ = &mut ctx;
}

#[test]
fn clone_jump_station_to_station_and_same_station_swap() {
    let ctx = test_ctx();
    let clone_stations: Vec<(u32, i64)> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.solar_system_id < 31_000_000 && s.region_id != POCHVEN_REGION_ID)
        .flat_map(|s| {
            s.stations
                .iter()
                .filter(|st| st.has_clone_bay && st.station_id != 60_015_187)
                .map(move |st| (s.solar_system_id, st.station_id))
        })
        .take(2)
        .collect();
    assert_eq!(clone_stations.len(), 2);
    let (sys1, st1) = clone_stations[0];
    let (sys2, st2) = clone_stations[1];

    let (uid, cid, corp_id, cap1_id) = ctx.db.with_mut_snapshot(|snap| {
        let (uid, cid, corp) = {
            let ch = snap.characters.values_mut().next().expect("demo char");
            ch.solar_system_id = sys1;
            ch.station_id = Some(st1);
            ch.structure_id = None;
            ch.balance = IskCents::from_isk(10_000_000);
            ch.last_clone_jump_unix = 0;
            (ch.user_id, ch.character_id, ch.corporation_id)
        };

        let cap = put_item(
            snap,
            670,
            cid,
            st1,
            InventoryFlag::Hangar,
            1,
            true,
        );
        snap.characters.get_mut(&cid).unwrap().active_ship_id = cap;
        snap.skills.entry(cid).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 2,
                skill_points: 16_000.0,
            },
        );
        snap.active_implants.insert(cid, vec![9899]);
        snap.jump_clones.insert(
            cid,
            vec![
                JumpCloneRecord {
                    jump_clone_id: 9101,
                    character_id: cid,
                    location_id: st2,
                    implants: vec![9941],
                },
                JumpCloneRecord {
                    jump_clone_id: 9102,
                    character_id: cid,
                    location_id: st1,
                    implants: vec![9942],
                },
            ],
        );
        (uid, cid, corp, cap)
    });

    let mut sess = make_session(uid, cid, corp_id, sys1, Some(st1), None, Some(cap1_id));

    // Cross-station jump from st1 -> st2 debits 900,000 ISK and swaps implants
    let res1 = handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(9101)])
        .expect("handled")
        .expect("cross-station jump ok");
    let PyRep::KeyVal(kv1) = res1 else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv1.get("locationID").and_then(PyRep::as_i64), Some(st2));
    assert_eq!(sess.station_id, Some(st2));
    assert_eq!(sess.solar_system_id, Some(sys2));
    assert!(kv1.get("cooldownSeconds").and_then(PyRep::as_i64).unwrap_or(0) > 0);

    // Jumping back to st1 while cooldown is active fails
    crate::services::space_destiny::clear_session_change_timer(&ctx, cid);
    let cd_err = handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(9101)])
        .expect("handled")
        .expect_err("cooldown active");
    assert_eq!(cd_err.code, "CloneJumpCooldownActive");

    // Same-station clone swap (installing/placing a clone at st2 and swapping in st2) succeeds with 0s cooldown even while on cooldown
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(clones) = snap.jump_clones.get_mut(&cid) {
            clones.push(JumpCloneRecord {
                jump_clone_id: 9103,
                character_id: cid,
                location_id: st2,
                implants: vec![9950],
            });
        }
    });
    let swap_res = handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(9103)])
        .expect("handled")
        .expect("same-station swap ok");
    let PyRep::KeyVal(kv_swap) = swap_res else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv_swap.get("cooldownSeconds").and_then(PyRep::as_i64), Some(0));
}

#[test]
fn clone_jump_rejects_wormhole_pochven_and_non_capsule() {
    let ctx = test_ctx();
    let hs_station = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.solar_system_id < 31_000_000 && s.region_id != POCHVEN_REGION_ID)
        .flat_map(|s| s.stations.iter().map(move |st| (s.solar_system_id, st)))
        .find(|(_, st)| st.has_clone_bay && st.station_id != 60_015_187)
        .map(|(sys_id, st)| (sys_id, st.station_id))
        .expect("hs station with clone bay");

    let pochven_station = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == POCHVEN_REGION_ID)
        .flat_map(|s| s.stations.iter())
        .find(|st| st.has_clone_bay)
        .map(|st| st.station_id);

    let wh_struct_id = 1_040_999_001_000;
    let (uid, cid, corp_id, frigate_id, cap_id) = ctx.db.with_mut_snapshot(|snap| {
        let (uid, cid, corp) = {
            let ch = snap.characters.values_mut().next().expect("demo char");
            ch.solar_system_id = hs_station.0;
            ch.station_id = Some(hs_station.1);
            ch.balance = IskCents::from_isk(10_000_000);
            ch.last_clone_jump_unix = 0;
            (ch.user_id, ch.character_id, ch.corporation_id)
        };

        let frig = put_item(
            snap,
            588,
            cid,
            hs_station.1,
            InventoryFlag::Hangar,
            1,
            true,
        );
        let cap = put_item(
            snap,
            670,
            cid,
            hs_station.1,
            InventoryFlag::Hangar,
            1,
            true,
        );
        snap.characters.get_mut(&cid).unwrap().active_ship_id = frig;
        snap.skills.entry(cid).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 2,
                skill_points: 16_000.0,
            },
        );

        snap.structures.insert(
            wh_struct_id,
            UpwellStructureRecord {
                structure_id: wh_struct_id,
                type_id: 35832,
                owner_corp_id: corp,
                solar_system_id: 31_000_001, // non-Thera J-space
                name: "JSpaceStructure".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                position: [0.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        snap.items.insert(
            99_101,
            InventoryItemRecord {
                item_id: 99_101,
                type_id: 35894,
                owner_id: corp,
                location_id: wh_struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(99_101, true);
        let mut clones = vec![JumpCloneRecord {
            jump_clone_id: 9201,
            character_id: cid,
            location_id: wh_struct_id,
            implants: vec![],
        }];
        if let Some(pst) = pochven_station {
            clones.push(JumpCloneRecord {
                jump_clone_id: 9202,
                character_id: cid,
                location_id: pst,
                implants: vec![],
            });
        }
        snap.jump_clones.insert(cid, clones);
        (uid, cid, corp, frig, cap)
    });

    let mut sess = make_session(uid, cid, corp_id, hs_station.0, Some(hs_station.1), None, Some(frigate_id));
    let non_cap = handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(9201)])
        .expect("handled")
        .expect_err("must be in capsule");
    assert_eq!(non_cap.code, "MustBeInCapsule");

    // Switch into capsule -> J-space destination refused
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.active_ship_id = cap_id;
        }
    });
    sess.ship_id = Some(cap_id);
    let wh_err = handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(9201)])
        .expect("handled")
        .expect_err("wormhole clone jump refused");
    assert_eq!(wh_err.code, "CannotCloneJumpInWormholeSpace");

    if pochven_station.is_some() {
        let poch_err = handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(9202)])
            .expect("handled")
            .expect_err("pochven destination refused");
        assert_eq!(poch_err.code, "CannotCloneJumpToPochven");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clone_vat_bay_activation_and_in_space_ship_clone_jump() {
    let mut ctx = test_ctx();
    let titan_tid = 99_910;
    let vat_mod_tid = 99_911;
    let ozone_tid = 16_273;
    clone_type_with(&mut ctx.sde, 11_567, titan_tid, 30, 6, &[]);
    clone_type_with(
        &mut ctx.sde,
        24_576,
        vat_mod_tid,
        815,
        7,
        &[(73, 300_000.0), (713, f64::from(ozone_tid)), (714, 250.0)],
    );
    clone_type_with(&mut ctx.sde, 34, ozone_tid, 423, 4, &[]);

    let sys_id = 30000142;
    let host_station = 60_003_760;
    let (host_cid, jumper_uid, jumper_cid, corp_id, titan_id, vat_id, cap_id) =
        ctx.db.with_mut_snapshot(|snap| {
            let mut it = snap.characters.values_mut();
            let c1 = it.next().expect("host char");
            c1.solar_system_id = sys_id;
            c1.station_id = None;
            c1.structure_id = None;
            let host_cid = c1.character_id;
            let corp = c1.corporation_id;

            let c2 = it.next().expect("jumper char");
            c2.solar_system_id = sys_id;
            c2.station_id = Some(host_station);
            c2.structure_id = None;
            c2.corporation_id = corp;
            c2.balance = IskCents::from_isk(10_000_000);
            c2.last_clone_jump_unix = 0;
            let j_cid = c2.character_id;
            let j_uid = c2.user_id;

            let titan = put_item(
                snap,
                titan_tid,
                host_cid,
                sys_id as i64,
                InventoryFlag::None,
                1,
                true,
            );
            let vat = put_item(
                snap,
                vat_mod_tid,
                host_cid,
                titan,
                InventoryFlag::HiSlot0,
                1,
                true,
            );
            put_item(
                snap,
                ozone_tid,
                host_cid,
                titan,
                InventoryFlag::SpecializedFuelBay,
                300,
                false,
            );
            let cap = put_item(
                snap,
                670,
                j_cid,
                host_station,
                InventoryFlag::Hangar,
                1,
                true,
            );

            if let Some(h) = snap.characters.get_mut(&host_cid) {
                h.active_ship_id = titan;
            }
            if let Some(j) = snap.characters.get_mut(&j_cid) {
                j.active_ship_id = cap;
            }
            for id in [host_cid, j_cid] {
                snap.skills.entry(id).or_default().insert(
                    24242,
                    TrainedSkillRecord {
                        type_id: 24242,
                        level: 2,
                        skill_points: 16_000.0,
                    },
                );
            }
            snap.jump_clones.insert(
                host_cid,
                vec![JumpCloneRecord {
                    jump_clone_id: 9301,
                    character_id: host_cid,
                    location_id: titan,
                    implants: vec![],
                }],
            );
            (host_cid, j_uid, j_cid, corp, titan, vat, cap)
        });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: titan_id,
                type_id: titan_tid,
                position: DVec3::new(50_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 75.0,
                inertia: 40.0,
                mass: 2_400_000_000.0,
                radius: 6_500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                owner_id: host_cid,
                char_id: Some(host_cid),
                corp_id,
                group_id: 30,
                category_id: 6,
                ..Default::default()
            },
        )
        .await;

    let res = activate_clone_vat_bay(&ctx, &sys_h, host_cid, titan_id, vat_id)
        .await
        .expect("vat bay activated");
    let PyRep::KeyVal(kv) = res else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv.get("cloneVatBayActive").and_then(PyRep::as_bool), Some(true));
    assert_eq!(kv.get("fuelConsumed").and_then(PyRep::as_i64), Some(250));

    let active_now = ctx.db.with_snapshot(|s| is_ship_clone_vat_bay_active(&ctx, s, titan_id, ctx.now_unix()));
    assert!(active_now);

    // Host owner jumps into their Titan's Clone Vat Bay
    let mut host_sess = make_session(jumper_uid, host_cid, corp_id, sys_id, Some(host_station), None, Some(cap_id));
    ctx.db.with_mut_snapshot(|s| {
        let host_cap = put_item(
            s,
            670,
            host_cid,
            host_station,
            InventoryFlag::Hangar,
            1,
            true,
        );
        if let Some(h) = s.characters.get_mut(&host_cid) {
            h.station_id = Some(host_station);
            h.active_ship_id = host_cap;
        }
        host_sess.ship_id = Some(host_cap);
    });

    // Wait: host pilot must be in space piloting titan while jumper_cid jumps in!
    let mut jumper_sess = make_session(jumper_uid, jumper_cid, corp_id, sys_id, Some(host_station), None, Some(cap_id));
    ctx.db.with_mut_snapshot(|s| {
        if let Some(h) = s.characters.get_mut(&host_cid) {
            h.station_id = None;
            h.active_ship_id = titan_id;
        }
        s.fleets.insert(
            8801,
            eve_db::types::FleetRecord {
                fleet_id: 8801,
                boss_id: host_cid,
                members: vec![host_cid, jumper_cid],
                ..Default::default()
            },
        );
        s.jump_clones.insert(
            jumper_cid,
            vec![JumpCloneRecord {
                jump_clone_id: 9302,
                character_id: jumper_cid,
                location_id: titan_id,
                implants: vec![],
            }],
        );
    });
    let space_jump = handle_clone_jump(&ctx, &mut jumper_sess, &[PyRep::Integer(9302)])
        .expect("handled")
        .expect("vat bay jump ok");
    assert!(matches!(space_jump, PyRep::KeyVal(_)));
    assert!(jumper_sess.station_id.is_none());
}

#[test]
fn clone_bay_rejects_empty_slots_offline_and_non_operational_structures() {
    let ctx = test_ctx();
    let cid = 97_600_001i64;
    let owner_corp = 98_600_001i64;
    let struct_dep = 1_060_000_000_001i64;
    let struct_dest = 1_060_000_000_002i64;
    let kw = std::collections::HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = cid;
        ch.user_id = cid as u64;
        ch.corporation_id = owner_corp;
        ch.clone_grade = 2;
        ch.solar_system_id = 30_000_142;
        ch.station_id = None;
        ch.structure_id = Some(struct_dep);
        ch.balance = IskCents::from_isk(10_000_000);
        ch.last_clone_jump_unix = 0;
        let cap_id = put_item(snap, 670, cid, struct_dep, InventoryFlag::Hangar, 1, true);
        ch.active_ship_id = cap_id;
        snap.characters.insert(cid, ch);

        snap.skills.entry(cid).or_default().insert(
            24242,
            TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 },
        );

        for (sid, sys) in [(struct_dep, 30_000_142), (struct_dest, 30_000_142)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35832,
                    owner_corp_id: owner_corp,
                    solar_system_id: sys,
                    name: format!("Citadel {sid}"),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    ..Default::default()
                },
            );
        }
        snap.jump_clones.insert(
            cid,
            vec![JumpCloneRecord {
                jump_clone_id: 901,
                character_id: cid,
                location_id: struct_dest,
                implants: Vec::new(),
            }],
        );
    });

    let cap_id = ctx.db.with_snapshot(|snap| snap.characters[&cid].active_ship_id);
    let mut sess = make_session(cid as u64, cid, owner_corp, 30_000_142, None, Some(struct_dep), Some(cap_id));

    // 1. Empty service slots on departure structure -> StationHasNoCloneBay + GetPriceForClone -> StationHasNoCloneBay + InstallCloneInStructure -> NoCloneBayService
    assert_eq!(
        handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(901)])
            .unwrap().unwrap_err().code,
        "StationHasNoCloneBay"
    );
    assert_eq!(
        crate::services::character_skills::character_clones_implants::handle_clones_and_sheet(
            &ctx, &mut sess, "jumpCloneSvc", "GetPriceForClone", &[], &kw
        ).unwrap().unwrap_err().code,
        "StationHasNoCloneBay"
    );
    assert_eq!(
        crate::services::character_skills::character_clones_implants::handle_clones_and_sheet(
            &ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStructure", &[], &kw
        ).unwrap().unwrap_err().code,
        "NoCloneBayService"
    );

    // 2. Fit & online 35894 on departure, keep destination empty -> StructureHasNoCloneBay
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            97_600_101,
            InventoryItemRecord {
                item_id: 97_600_101,
                type_id: 35894,
                owner_id: owner_corp,
                location_id: struct_dep,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(97_600_101, true);
    });
    assert_eq!(
        crate::services::character_skills::character_clones_implants::handle_clones_and_sheet(
            &ctx, &mut sess, "jumpCloneSvc", "GetPriceForClone", &[], &kw
        ).unwrap().unwrap().as_f64(),
        Some(900_000.0)
    );
    assert_eq!(
        handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(901)])
            .unwrap().unwrap_err().code,
        "StructureHasNoCloneBay"
    );

    // 3. Fit & online 35894 on destination, test HullReinforce / Unanchored / NoCore / Anchoring rejection
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            97_600_102,
            InventoryItemRecord {
                item_id: 97_600_102,
                type_id: 35894,
                owner_id: owner_corp,
                location_id: struct_dest,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(97_600_102, true);
    });
    for bad_state in ["HullReinforce", "HullReinforced", "Unanchored", "NoCore", "Anchoring"] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.get_mut(&struct_dest).unwrap().state = bad_state.into();
        });
        assert_eq!(
            handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(901)])
                .unwrap().unwrap_err().code,
            "StructureHasNoCloneBay"
        );
    }

    // 4. Operational both sides -> CloneJump succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_dest).unwrap().state = "ShieldVulnerable".into();
    });
    let ok = handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(901)])
        .unwrap()
        .expect("clone jump with operational structures and online 35894 modules must succeed");
    assert_eq!(ok.dict_get("locationID"), Some(&PyRep::Integer(struct_dest)));
}

#[cfg(test)]
#[path = "character_clone_jump_rules_extra_unit_tests.rs"]
mod character_clone_jump_rules_extra_unit_tests;
