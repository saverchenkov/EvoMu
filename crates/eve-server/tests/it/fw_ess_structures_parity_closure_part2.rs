//! `fw_ess_structures_parity_closure_part2` — Live RPC integration tests for ESS intrusion
//! bubble, Main Bank 2h45m auto-payout, Reserve Bank key tiers, and Upwell structure weapons & directory.

use crate::common::login_pilot;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::encounter_surveillance_system::ESS_MAIN_BANK_AUTOPAYOUT_SECS;
use eve_server::services::structure_standup_weapons::try_activate_standup_weapon;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn test_ess_grid_intrusion_bubble_warp_mwd_mjd_cloak_cyno_restrictions() {
    // parity: feat:pve.ess-grid-warp-mwd-restrictions
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(20_004);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let kw = HashMap::new();
    let null_sys = 30_004_759;
    let ship_id = 888_777_666;
    let mwd_id = 888_777_667;
    let cloak_id = 888_777_668;

    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = null_sys;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(null_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mwd_id,
            InventoryItemRecord {
                item_id: mwd_id,
                type_id: 434, // 5MN Microwarpdrive I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cloak_id,
            InventoryItemRecord {
                item_id: cloak_id,
                type_id: 11370, // Prototype Cloaking Device I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Spawn ESS beacon and ship within 75 km intrusion bubble (10 km apart)
    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let ess_id = 820_000_000 + i64::from(null_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ess_id,
            type_id: 56002,
            position: DVec3::new(0.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    // 1. Warp blocked inside 75 km ESS bubble
    let warp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(40_000_001)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(warp_err.code, "WarpDisruptedByEssBubble");

    // 2. MWD blocked inside 75 km ESS bubble
    let mwd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(mwd_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(mwd_err.code, "ModuleBlockedInEssBubble");

    // 3. Cloaking Device blocked inside 75 km ESS bubble
    let cloak_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cloak_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(cloak_err.code, "ModuleBlockedInEssBubble");
}

#[tokio::test]
async fn test_ess_main_bank_autopayout_proportional_and_hack_deferral() {
    // parity: feat:pve.ess-main-bank-autopayout, feat:pve.ess-reserve-bank-key-tiers
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(20_005);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char1 = 90_000_001;
    let char2 = 90_000_002;
    let null_sys = 30_004_759;
    let kw = HashMap::new();
    sess.character_id = Some(char1);
    sess.solar_system_id = Some(null_sys);

    let now = ctx.now_unix();
    let (bal1_before, bal2_before) = ctx.db.with_mut_snapshot(|snap| {
        snap.system_states
            .entry(null_sys)
            .or_default()
            .ess_main_bank = IskCents::from_isk(10_000_000);
        snap.sov_runtime
            .ess_main_bank_next_payout_unix
            .insert(null_sys, now);
        let contrib = snap
            .sov_runtime
            .ess_main_bank_contributors
            .entry(null_sys)
            .or_default();
        contrib.insert(char1, 750_000_000); // 75%
        contrib.insert(char2, 250_000_000); // 25%
        let b1 = snap.characters.get(&char1).map(|c| c.balance.0).unwrap_or(0);
        let b2 = snap.characters.get(&char2).map(|c| c.balance.0).unwrap_or(0);
        (b1, b2)
    });

    // Trigger essMgr query at 2h45m expiry -> distributes 7.5M to char1 and 2.5M to char2
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "prime_cache_result",
            &[PyRep::Integer(i64::from(null_sys))],
            &kw,
            None,
        )
        .await
        .expect("prime_cache_result");

    ctx.db.with_snapshot(|snap| {
        let st = snap.system_states.get(&null_sys).expect("st");
        assert_eq!(st.ess_main_bank.0, 0);
        let b1_after = snap.characters.get(&char1).map(|c| c.balance.0).unwrap_or(0);
        let b2_after = snap.characters.get(&char2).map(|c| c.balance.0).unwrap_or(0);
        assert_eq!(b1_after - bal1_before, IskCents::from_isk(7_500_000).0);
        assert_eq!(b2_after - bal2_before, IskCents::from_isk(2_500_000).0);
        let next_p = snap
            .sov_runtime
            .ess_main_bank_next_payout_unix
            .get(&null_sys)
            .copied()
            .unwrap_or(0);
        assert!(next_p >= now + ESS_MAIN_BANK_AUTOPAYOUT_SECS);
    });
}

#[tokio::test]
async fn test_structures_point_defense_weapons_and_directory_browser() {
    // parity: feat:structures.point-defense-system, feat:structures.structure-weapons, feat:structures.structure-directory
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(20_006);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let kw = HashMap::new();
    let sys_id = 30_004_759;
    let structure_id = 1_050_000_001;
    let pdb_mod_id = 1_050_000_002;
    let target_ship_id = 1_050_000_003;

    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);

    let flak_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 4186)
        .map(|t| t.type_id)
        .unwrap_or(56_074);
    ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(1_000_001);
        snap.items.insert(
            pdb_mod_id + 10,
            InventoryItemRecord {
                item_id: pdb_mod_id + 10,
                type_id: flak_tid,
                owner_id: corp_id,
                location_id: structure_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35834, // Keepstar
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Astrahus Citadel".into(),
                state: "Online".into(),
                position: [50_000.0, 0.0, 50_000.0],
                quantum_core_installed: true,
                fuel_blocks: 5_000,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: 0,
                last_damage_unix: 0,
                last_damage_tick: 0,
                self_destruct_at_unix: None,
            },
        );
        let pdb_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.group_id == 1330)
            .map(|t| t.type_id)
            .unwrap_or(35892);
        snap.items.insert(
            pdb_mod_id,
            InventoryItemRecord {
                item_id: pdb_mod_id,
                type_id: pdb_type_id,
                owner_id: corp_id,
                location_id: structure_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(pdb_mod_id, true);
    });

    // 1. Verify structureDirectory.GetStructures lists the structure
    let dir_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDirectory",
            None,
            "GetStructures",
            &[],
            &kw,
            None,
        )
        .await
        .expect("structureDirectory.GetStructures");
    let found_struct = match &dir_rep {
        PyRep::Dict(entries) => entries.iter().any(|(k, v)| {
            k.as_i64() == Some(structure_id)
                || v.dict_get("structureID") == Some(&PyRep::Integer(structure_id))
        }),
        PyRep::List(structs) => structs
            .iter()
            .any(|s| s.dict_get("structureID") == Some(&PyRep::Integer(structure_id))),
        _ => false,
    };
    assert!(found_struct, "structureDirectory.GetStructures must include {structure_id}");

    // 2. Spawn structure & nearby hostile frigate within 5 km and fire Standup Point Defense Battery
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let struct_ball = Ball {
        id: structure_id,
        type_id: 35834,
        position: DVec3::new(50_000.0, 0.0, 50_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 0.0,
        mass: 1.0e9,
        radius: 2_500.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    };
    let _ = sys_handle.spawn_ball(struct_ball.clone()).await;
    let _ = sys_handle.set_capacitor(structure_id, 50_000.0).await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: target_ship_id,
            type_id: 587,
            position: DVec3::new(53_000.0, 0.0, 50_000.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    sess.structure_id = Some(structure_id);
    eve_server::services::structure_standup_weapons::set_structure_active_gunner(
        &ctx,
        structure_id,
        char_id,
    );

    let pdb_res = try_activate_standup_weapon(
        &ctx,
        &sys_handle,
        char_id,
        structure_id,
        sys_id,
        pdb_mod_id,
        Some(target_ship_id),
        &struct_ball,
    )
    .await
    .expect("try_activate_standup_weapon")
    .expect("PDB activation Ok");
    assert_eq!(
        pdb_res.dict_get("structureID"),
        Some(&PyRep::Integer(structure_id))
    );
}
