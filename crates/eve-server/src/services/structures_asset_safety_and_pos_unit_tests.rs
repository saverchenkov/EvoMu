//! Semantic unit tests split from `structures_pos_deep_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::structures_pos_deep_extra_unit_tests::*;
use crate::services::sov_gameplay::sov_gameplay_skyhook::{
    advance_skyhook_reagents_in_snap, apply_skyhook_weapon_damage, handle_skyhook_component_call,
    planet_reagent_type, tick_skyhooks, SKYHOOK_SILO_LINK_DURATION_SECS,
    SKYHOOK_SILO_LINK_MAX_SPEED_MS,
};
use crate::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SUPERIONIC_ICE,
};
use crate::services::station_structure;
use crate::services::station_structure::structure_lifecycle::*;
use crate::services::station_structure::structure_upwell_ops::{
    self, load_ext_state, save_ext_state, sync_auto_delivered_asset_safety_wraps,
    AssetSafetyWrapRecord, ASSET_SAFETY_AUTO_DELIVERY_SECS,
    ASSET_SAFETY_DELIVERY_DELAY_SECS,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    corp_roles, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[tokio::test]
async fn asset_safety_wrap_auto_delivery_and_05_pct_vs_15_pct_fee_math() {
    let mut ctx = test_ctx();
    let now = 1_800_000_000u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));
    insert_custom_type(&mut ctx.sde, 945_020, 1657, 65, 1_000_000.0, &[]);

    let (char_id, corp_id, sys_id) = (90_201i64, 98_201i64, 30_000_142u32);
    let (sid_src, sid_dst, npc_station) = (88_201i64, 88_202i64, 60_003_760i64);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.balance = IskCents::from_isk(100_000);
        snap.characters.insert(char_id, ch);

        for sid in [sid_src, sid_dst] {
            snap.structures.insert(
                sid,
                make_structure_rec(sid, 35_832, corp_id, sys_id, STATE_SHIELD_VULNERABLE, true, 100),
            );
        }

        let mut ext = load_ext_state(snap);
        for (wid, item_id, created_at) in [
            (770_001i64, 771_001i64, now - ASSET_SAFETY_DELIVERY_DELAY_SECS - 10),
            (770_002i64, 771_002i64, now - ASSET_SAFETY_AUTO_DELIVERY_SECS - 10),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 945_020,
                    owner_id: char_id,
                    location_id: wid,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            ext.asset_safety_wraps.insert(
                wid,
                AssetSafetyWrapRecord {
                    wrap_id: wid,
                    owner_id: char_id,
                    is_corp: false,
                    source_structure_id: sid_src,
                    source_structure_name: "SrcStruct".into(),
                    solar_system_id: sys_id,
                    created_unix: created_at,
                    deliverable_after_unix: created_at + ASSET_SAFETY_DELIVERY_DELAY_SECS,
                    delivered_to_id: None,
                    item_ids: vec![item_id],
                    estimated_value_isk: 1_000_000.0,
                    nearest_npc_station_id: Some(npc_station),
                    auto_delivered: false,
                    retrieval_fee_paid: false,
                },
            );
        }
        save_ext_state(snap, &ext);
    });

    sync_auto_delivered_asset_safety_wraps(&ctx);
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        assert!(!ext.asset_safety_wraps[&770_001].auto_delivered);
        assert!(ext.asset_safety_wraps[&770_002].auto_delivered);
        assert_eq!(ext.asset_safety_wraps[&770_002].delivered_to_id, Some(npc_station));
    });

    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    let kw = HashMap::new();

    // 1) Same-system manual delivery of wrap 770_001 to sid_dst charges 0.5% = 5,000 ISK
    let rep1 = structure_upwell_ops::handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "MoveSafetyWrapToStructure",
        &[PyRep::Integer(770_001), PyRep::Integer(sid_dst)],
        &kw,
    )
    .await
    .unwrap()
    .expect("0.5% same-system manual delivery");
    assert_eq!(rep1.dict_get("feePaidISK"), Some(&PyRep::Real(5_000.0)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(95_000));
        assert_eq!(snap.items[&771_001].location_id, sid_dst);
    });

    // 2) Auto-delivered wrap 770_002 requires 15% = 150,000 ISK; pilot has 95,000 ISK -> InsufficientFunds
    assert_eq!(
        structure_upwell_ops::handle(
            &ctx,
            &mut sess,
            "structureAssetSafety",
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(770_002), PyRep::Integer(npc_station)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(200_000);
    });
    let rep2 = structure_upwell_ops::handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "MoveSafetyWrapToStructure",
        &[PyRep::Integer(770_002), PyRep::Integer(npc_station)],
        &kw,
    )
    .await
    .unwrap()
    .expect("15% auto-delivered recovery");
    assert_eq!(rep2.dict_get("feePaidISK"), Some(&PyRep::Real(150_000.0)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(50_000));
    });
}

#[tokio::test]
async fn pos_mgr_orbital_anchoring_gantry_upgrade_and_skyhook_reagent_siphon_math() {
    let ctx = test_ctx();
    let now = 1_800_000_000u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));

    assert_eq!(planet_reagent_type(12), Some(TYPE_SUPERIONIC_ICE));
    assert_eq!(planet_reagent_type(2014), Some(TYPE_SUPERIONIC_ICE));
    assert_eq!(planet_reagent_type(2015), Some(TYPE_MAGMATIC_GAS));
    assert_eq!(planet_reagent_type(11), None);

    let (null_sys, planet_id, planet_pos) = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status <= 0.0 && !s.planet_ids.is_empty())
        .and_then(|s| {
            let pid = s.planet_ids[0];
            let ppos = ctx.sde.celestials_in_system(s.solar_system_id).into_iter().find(|c| c.celestial_id == pid).map(|c| c.position).unwrap_or(DVec3::ZERO);
            Some((s.solar_system_id, pid, ppos))
        })
        .expect("nullsec planet");

    let (char_id, thief_id, corp_id, ally_id) = (90_301i64, 90_302i64, 98_300_001i64, 99_300_001i64);
    let (gantry_id, skyhook_id, ship_id, cruiser_id) = (88_301i64, 88_302i64, 88_310i64, 88_311i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(ally_id, AllianceRecord {
            alliance_id: ally_id, name: "Skyhook Ally".into(), short_name: "SKYA".into(),
            executor_corp_id: corp_id, member_corp_ids: HashSet::from([corp_id]), ..Default::default()
        });
        snap.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Skyhook Corp".into();
            __corp.ticker = "SKYC".into();
            __corp.ceo_id = char_id;
            __corp.alliance_id = Some(ally_id);
            __corp.members = HashSet::from([char_id]);
            __corp });
        let base_ch = snap.characters.values().next().cloned().expect("demo char");
        for (cid, c_corp, s_id) in [(char_id, corp_id, ship_id), (thief_id, 98_999, cruiser_id)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid; ch.corporation_id = c_corp; ch.solar_system_id = null_sys; ch.active_ship_id = s_id;
            snap.characters.insert(cid, ch);
        }
        for (iid, tid, owner, loc, flag) in [
            (gantry_id, 3_962u32, char_id, ship_id, InventoryFlag::Cargo),
            (88_399, 2_233u32, char_id, ship_id, InventoryFlag::Cargo),
            (cruiser_id, 621u32, thief_id, null_sys as i64, InventoryFlag::Hangar),
        ] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: owner, location_id: loc, flag,
                quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
    });

    let sys_h = ctx.systems.get_or_spawn(null_sys);
    for (id, tid) in [(ship_id, 587u32), (cruiser_id, 621u32)] {
        let _ = sys_h.spawn_ball(Ball {
            id, type_id: tid, position: planet_pos + DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 2_500.0, inertia: 1.0, mass: 10_000_000.0, radius: 80.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
    }

    let mut dir_sess = SessionState::new(1);
    dir_sess.user_id = Some(char_id as u64);
    dir_sess.character_id = Some(char_id);
    dir_sess.corporation_id = Some(corp_id);
    dir_sess.solar_system_id = Some(null_sys);
    dir_sess.ship_id = Some(ship_id);
    dir_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    let kw = HashMap::new();

    let anchor_rep = station_structure::handle(&ctx, &mut dir_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(planet_id)], &kw)
        .await.unwrap().expect("AnchorOrbital");
    assert_eq!(anchor_rep.dict_get("planetID"), Some(&PyRep::Integer(planet_id)));

    let upg_rep = crate::services::station_structure_handlers::handle_upgrade_gantry(&ctx, &dir_sess, &[PyRep::Integer(gantry_id)])
        .expect("handle_upgrade_gantry");
    assert_eq!(upg_rep.dict_get("typeID"), Some(&PyRep::Integer(2_233)));

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(skyhook_id, UpwellStructureRecord {
            structure_id: skyhook_id, type_id: TYPE_ORBITAL_SKYHOOK, owner_corp_id: corp_id, solar_system_id: null_sys,
            name: "Deep Skyhook".into(), position: (planet_pos + DVec3::new(2_000.0, 0.0, 0.0)).to_array(),
            state: STATE_SHIELD_VULNERABLE.into(), quantum_core_installed: true, fuel_blocks: 100,
            shield_hp: 10_000.0, armor_hp: 10_000.0, hull_hp: 10_000.0, ..Default::default()
        });
        snap.sov_runtime.skyhook_reagent_bays.insert(skyhook_id, (TYPE_MAGMATIC_GAS, 100, 400, now));
        snap.sov_runtime.skyhook_theft_windows.insert(skyhook_id, (now, now + 7_200));

        for step in 1..=3_600 {
            advance_skyhook_reagents_in_snap(snap, &ctx.sde, skyhook_id, now + step);
        }
        let (_, sec_qty, insec_qty, _) = snap.sov_runtime.skyhook_reagent_bays[&skyhook_id];
        assert!(sec_qty > 125);
        assert_eq!(sec_qty + insec_qty, 550);
    });

    let dmg_rep = apply_skyhook_weapon_damage(&ctx, skyhook_id, 11_875.0).expect("damage inside window");
    assert_eq!(dmg_rep.dict_get("state"), Some(&PyRep::String("ShieldVulnerable".into())));
    assert_eq!(dmg_rep.dict_get("hackable"), Some(&PyRep::Bool(true)));

    let mut thief_sess = SessionState::new(2);
    thief_sess.user_id = Some(thief_id as u64);
    thief_sess.character_id = Some(thief_id);
    thief_sess.corporation_id = Some(98_999);
    thief_sess.solar_system_id = Some(null_sys);
    thief_sess.ship_id = Some(cruiser_id);

    let link_rep = handle_skyhook_component_call(&ctx, &thief_sess, null_sys, thief_id, cruiser_id, skyhook_id, "linkWithShip", "InitiateLink", &[])
        .await.expect("InitiateLink ok");
    assert_eq!(link_rep.dict_get("linkDurationSeconds"), Some(&PyRep::Integer(SKYHOOK_SILO_LINK_DURATION_SECS as i64)));
    let (_, cruiser_ball) = sys_h.get_ball_snapshot(cruiser_id).await.unwrap().unwrap();
    assert!((cruiser_ball.max_velocity - SKYHOOK_SILO_LINK_MAX_SPEED_MS).abs() < 1e-6);

    let secure_before = ctx.db.with_snapshot(|snap| snap.sov_runtime.skyhook_reagent_bays[&skyhook_id].1);
    tick_skyhooks(&ctx, now + SKYHOOK_SILO_LINK_DURATION_SECS + 10).await;
    ctx.db.with_snapshot(|snap| {
        let (_, sec_after, insec_after, _) = snap.sov_runtime.skyhook_reagent_bays[&skyhook_id];
        assert_eq!(insec_after, 0);
        assert!(sec_after >= secure_before);
        assert!(!snap.sov_runtime.skyhook_silo_links.contains_key(&skyhook_id));
    });
}
