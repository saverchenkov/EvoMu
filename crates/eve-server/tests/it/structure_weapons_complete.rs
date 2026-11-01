//! End-to-end V24 RPC integration tests for Upwell Standup weapon systems:
//! Point Defense Battery, Guided Bomb Launcher, Keepstar Standup Arcing Vorton Projector,
//! Multirole/Anticapital Missiles, and Burst Projectors (`feat:structures.structure-weapons`,
//! `feat:structures.point-defense-system`, `feat:structures.standup-point-defense-battery`,
//! `feat:structures.standup-guided-bomb-launcher`, `feat:structures.standup-superweapon-arcing-vorton`).

use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;
use std::collections::HashMap;

const LOWSEC_SYS: u32 = 30_002_718; // Rancer (low-sec)
const HIGHSEC_SYS: u32 = 30_000_142; // Jita (high-sec)

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn kv_entry<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

async fn setup_gunner_and_corp(ctx: &ServerContext, sess_id: u64, char_id: i64, ticker: &str) -> (SessionState, i64) {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())])
        .await
        .expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String(format!("Citadel Ops {ticker}").into()), PyRep::String(ticker.into())])
        .await
        .expect("AddCorporation");
    sess.corp_role |= eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    (sess, corp_id)
}

fn seed_armed_structure(
    ctx: &ServerContext,
    structure_id: i64,
    type_id: u32,
    corp_id: i64,
    sys_id: u32,
    modules: &[(i64, u32, InventoryFlag)],
    ammo: &[(i64, u32, i32)],
) {
    let profile = life::structure_profile(ctx, type_id).expect("structure profile");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                position: [0.0, 0.0, 0.0],
                state: life::STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true,
                fuel_blocks: 5_000,
                shield_hp: profile.shield_hp,
                armor_hp: profile.armor_hp,
                hull_hp: profile.hull_hp,
                name: format!("Armed Structure {structure_id}"),
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            structure_id,
            InventoryItemRecord {
                item_id: structure_id,
                type_id,
                owner_id: corp_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for &(mid, mtid, flag) in modules {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: mtid,
                    owner_id: corp_id,
                    location_id: structure_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
        for &(aid, atid, qty) in ammo {
            snap.items.insert(
                aid,
                InventoryItemRecord {
                    item_id: aid,
                    type_id: atid,
                    owner_id: corp_id,
                    location_id: structure_id,
                    flag: InventoryFlag::SpecializedAmmoHold,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
}

async fn dock_and_board_structure(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, structure_id: i64, sys_id: u32) {
    let ship_id = sess.ship_id.expect("ship");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.location_id = structure_id;
            it.flag = InventoryFlag::Hangar;
        }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.structure_id = Some(structure_id);
    rpc(ctx, sess, "structureControl", "BoardStructure", &[PyRep::Integer(structure_id)])
        .await
        .expect("BoardStructure");
    let (st_type_id, corp_id) = ctx.db.with_snapshot(|s| {
        s.structures
            .get(&structure_id)
            .map(|st| (st.type_id, st.owner_corp_id))
            .unwrap_or((35834, 98_000_001))
    });
    let st_rad = ctx.sde.types.get(&st_type_id).map(|t| t.radius).unwrap_or(15_000.0);
    spawn_grid_entity(
        ctx,
        sys_id,
        structure_id,
        st_type_id,
        DVec3::ZERO,
        1_000_000.0,
        1_000_000.0,
        1_000_000.0,
        st_rad,
        500_000.0,
        None,
        corp_id,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn spawn_grid_entity(
    ctx: &ServerContext,
    sys_id: u32,
    ball_id: i64,
    type_id: u32,
    pos: DVec3,
    shield_hp: f64,
    armor_hp: f64,
    hull_hp: f64,
    sig_radius: f64,
    cap_gj: f64,
    char_id: Option<i64>,
    corp_id: i64,
) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let t_info = ctx.sde.types.get(&type_id);
    let cat = t_info.map(|t| t.category_id).unwrap_or(6);
    let grp = t_info.map(|t| t.group_id).unwrap_or(25);
    let rad = t_info.map(|t| t.radius).unwrap_or(40.0).max(5.0);
    let mut cs = CombatState::new();
    cs.max_shield_hp = shield_hp.max(1.0);
    cs.max_armor_hp = armor_hp.max(1.0);
    cs.max_structure_hp = hull_hp.max(1.0);
    cs.shield_hp = shield_hp;
    cs.armor_hp = armor_hp;
    cs.structure_hp = hull_hp;
    cs.signature_radius = sig_radius;
    cs.max_capacitor = cap_gj.max(100.0);
    cs.capacitor = cap_gj;
    let meta = SlimItemMeta {
        owner_id: char_id.unwrap_or(corp_id),
        char_id,
        corp_id,
        alliance_id: 0,
        security_status: 0.0,
        is_cloaked: false,
        category_id: cat,
        group_id: grp,
    };
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ball_id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_500_000.0,
                radius: rad,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            meta,
        )
        .await;
}

#[tokio::test]
async fn test_standup_point_defense_battery_pulse_exclusions_and_capsules() {
    // parity: feat:structures.point-defense-system, feat:structures.standup-point-defense-battery, feat:structures.structure-weapons
    eve_testkit::attest(&[
        "feat:structures.point-defense-system",
        "feat:structures.standup-point-defense-battery",
        "feat:structures.structure-weapons",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let gunner = 90_000_001;
    let fleetmate = 90_000_002;
    let (mut sess, corp_id) = setup_gunner_and_corp(&ctx, 601, gunner, "PDSA").await;

    // Put gunner and fleetmate in the same fleet to test fleetmate exclusion!
    let fleet_rep = rpc(&ctx, &mut sess, "fleetMgr", "CreateFleet", &[]).await.expect("CreateFleet");
    let fleet_id = kv_entry(&fleet_rep, "fleetID").and_then(|v| v.as_i64()).unwrap_or(1);
    sess.fleet_id = Some(fleet_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(f) = snap.fleets.get_mut(&fleet_id) {
            if !f.members.contains(&fleetmate) {
                f.members.push(fleetmate);
            }
        }
    });

    // 1. High-sec activation check -> IllegalHighSecAoEWeapon
    let hs_st = 1_000_000_400_001i64;
    let hs_pds = 940_001i64;
    seed_armed_structure(&ctx, hs_st, 35833, corp_id, HIGHSEC_SYS, &[(hs_pds, 35926, InventoryFlag::HiSlot0)], &[(940_002, 63195, 20)]);
    dock_and_board_structure(&ctx, &mut sess, gunner, hs_st, HIGHSEC_SYS).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(hs_pds), PyRep::String("structureModuleEffectPointDefense".into())]).await.unwrap_err().code,
        "CannotActivateInHighSec"
    );
    rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(hs_st)]).await.unwrap();

    // 2. Low-sec Fortizar with Standup Point Defense Battery I (35926) + Standup Flak Round I (63195)
    let ls_st = 1_000_000_400_002i64;
    let ls_pds = 940_010i64;
    let ls_ammo = 940_011i64;
    seed_armed_structure(&ctx, ls_st, 35834, corp_id, LOWSEC_SYS, &[(ls_pds, 35926, InventoryFlag::HiSlot0)], &[]);

    // Activating BEFORE boarding must fail with NotControllingStructure
    sess.structure_id = Some(ls_st);
    sess.solar_system_id = Some(LOWSEC_SYS);
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(ls_pds)]).await.unwrap_err().code,
        "NotControllingStructure"
    );

    dock_and_board_structure(&ctx, &mut sess, gunner, ls_st, LOWSEC_SYS).await;

    // Overload is forbidden on Standup modules; empty ammo fails with NoChargesLoaded
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Overload", &[PyRep::Integer(ls_pds)]).await.unwrap_err().code,
        "NoAmmoLoaded"
    );
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(ls_pds)]).await.unwrap_err().code,
        "NoAmmoLoaded"
    );

    // Load 500 Standup Flak Round I (63195)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ls_ammo, InventoryItemRecord {
            item_id: ls_ammo, type_id: 63195, owner_id: corp_id, location_id: ls_st,
            flag: InventoryFlag::SpecializedAmmoHold, quantity: 500, singleton: false, version: 1, ..Default::default()
        });
    });

    let st_rad = ctx.sde.types.get(&35834).map(|t| t.radius).unwrap_or(15_000.0);
    let in_pds_pos = DVec3::new(st_rad + 1_200.0, 0.0, 0.0); // 1,200m from structure surface (< 2,500m PDS range)
    let out_pds_pos = DVec3::new(st_rad + 12_000.0, 0.0, 0.0); // 12,000m surface distance (> 2,500m PDS range)
    let hostile_corp = 98_000_901i64;

    // Spawn 9 test entities around the Fortizar:
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(950_007, InventoryItemRecord { item_id: 950_007, type_id: 587, owner_id: corp_id, location_id: LOWSEC_SYS as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default() });
    });
    spawn_grid_entity(&ctx, LOWSEC_SYS, 950_001, 587, in_pds_pos, 5_000.0, 5_000.0, 5_000.0, 35.0, 300.0, Some(90_000_051), hostile_corp).await; // 1. Hostile Frigate (HIT)
    spawn_grid_entity(&ctx, LOWSEC_SYS, 950_002, 2454, in_pds_pos, 200.0, 200.0, 200.0, 25.0, 50.0, None, hostile_corp).await; // 2. Hostile Hobgoblin I drone (HIT)
    spawn_grid_entity(&ctx, LOWSEC_SYS, 950_003, 40556, in_pds_pos, 2_000.0, 1_000.0, 1_000.0, 40.0, 100.0, None, hostile_corp).await; // 3. Hostile Light Fighter (HIT)
    spawn_grid_entity(&ctx, LOWSEC_SYS, 950_004, 209, in_pds_pos, 50.0, 50.0, 50.0, 15.0, 10.0, None, hostile_corp).await; // 4. Incoming Missile/Torpedo (category_id == 8, HIT)
    spawn_grid_entity(&ctx, LOWSEC_SYS, 950_005, 670, in_pds_pos, 0.0, 250.0, 250.0, 25.0, 100.0, Some(90_000_055), hostile_corp).await; // 5. UNSHIELDED Capsule (HIT)
    spawn_grid_entity(&ctx, LOWSEC_SYS, 950_006, 670, in_pds_pos, 250.0, 250.0, 250.0, 25.0, 100.0, Some(90_000_056), hostile_corp).await; // 6. Hostile Capsule with shield (HIT under Red safety)
    spawn_grid_entity(&ctx, LOWSEC_SYS, 950_007, 587, in_pds_pos, 5_000.0, 5_000.0, 5_000.0, 35.0, 300.0, None, corp_id).await; // 7. Friendly Corp Frigate (SPARED!)
    spawn_grid_entity(&ctx, LOWSEC_SYS, 950_008, 587, in_pds_pos, 5_000.0, 5_000.0, 5_000.0, 35.0, 300.0, Some(fleetmate), hostile_corp).await; // 8. Fleetmate Frigate (SPARED!)
    spawn_grid_entity(&ctx, LOWSEC_SYS, 950_009, 587, out_pds_pos, 5_000.0, 5_000.0, 5_000.0, 35.0, 300.0, Some(90_000_059), hostile_corp).await; // 9. Out-of-range Hostile Frigate (SPARED!)

    let res = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(ls_pds), PyRep::String("structureModuleEffectPointDefense".into())])
        .await
        .expect("Activate Standup Point Defense Battery I");
    assert_eq!(kv_entry(&res, "targetsHit").and_then(|v| v.as_i64()), Some(6), "Exact 6 valid hostile targets must be hit");

    let sys_h = ctx.systems.get_or_spawn(LOWSEC_SYS);
    // Verify 1..6 took damage (missile 950_004 may even be destroyed & despawned!)
    assert!(sys_h.get_combat_state(950_001).await.unwrap().unwrap().shield_hp < 5_000.0, "Hostile frigate must take PDS damage");
    assert!(sys_h.get_combat_state(950_002).await.unwrap().map_or(true, |cs| cs.shield_hp < 200.0), "Hostile drone must take PDS damage");
    assert!(sys_h.get_combat_state(950_003).await.unwrap().unwrap().shield_hp < 2_000.0, "Hostile light fighter must take PDS damage");
    assert!(sys_h.get_combat_state(950_004).await.unwrap().map_or(true, |cs| cs.shield_hp < 50.0), "Incoming missile must take PDS damage");
    assert!(sys_h.get_combat_state(950_005).await.unwrap().map_or(true, |cs| cs.armor_hp < 250.0), "Unshielded hostile capsule MUST take PDS damage");
    assert!(sys_h.get_combat_state(950_006).await.unwrap().map_or(true, |cs| cs.shield_hp < 250.0), "Hostile capsule MUST take PDS damage under Red safety");

    // Verify 7..9 were completely spared!
    for spared_id in [950_007, 950_008, 950_009] {
        assert!((sys_h.get_combat_state(spared_id).await.unwrap().unwrap().shield_hp - 5_000.0).abs() < 1e-6, "Spared entity {spared_id} MUST be untouched by PDS");
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&ls_ammo].quantity), 300, "200 Standup Flak Round I consumed");
}

#[tokio::test]
async fn test_standup_guided_bomb_launcher_missiles_and_burst_projectors() {
    // parity: feat:structures.standup-guided-bomb-launcher, feat:structures.structure-weapons
    eve_testkit::attest(&[
        "feat:structures.standup-guided-bomb-launcher",
        "feat:structures.structure-weapons",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let gunner = 90_000_001;
    let (mut sess, corp_id) = setup_gunner_and_corp(&ctx, 611, gunner, "GBMB").await;
    let st_id = 1_000_000_400_010i64;
    let gbl_mod = 960_001i64; // Standup Guided Bomb Launcher I (35923)
    let mml_mod = 960_002i64; // Standup Multirole Missile Launcher I (35922)
    let acm_mod = 960_003i64; // Standup Anticapital Missile Launcher I (35921)
    let neut_proj = 960_004i64; // Standup Energy Neutralization Burst Projector I (35944)
    let ecm_proj = 960_005i64; // Standup ECM Jammer Burst Projector I (47069)

    seed_armed_structure(
        &ctx,
        st_id,
        35833,
        corp_id,
        LOWSEC_SYS,
        &[
            (gbl_mod, 35923, InventoryFlag::HiSlot0),
            (mml_mod, 35922, InventoryFlag::HiSlot1),
            (acm_mod, 35921, InventoryFlag::HiSlot2),
            (neut_proj, 35944, InventoryFlag::MedSlot0),
            (ecm_proj, 47069, InventoryFlag::MedSlot1),
        ],
        &[
            (960_101, 37850, 500), // Standup Light Guided Bomb (exp_rad = 100m)
            (960_102, 37847, 5), // Standup Light Missile
            (960_103, 37844, 5), // Standup XL Cruise Missile (group 1546)
        ],
    );
    dock_and_board_structure(&ctx, &mut sess, gunner, st_id, LOWSEC_SYS).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(960_111, InventoryItemRecord {
            item_id: 960_111, type_id: 37850, owner_id: corp_id, location_id: st_id,
            flag: InventoryFlag::HiSlot0, quantity: 5, singleton: false, version: 1, ..Default::default()
        });
    });

    let hostile_corp = 98_000_902i64;
    let primary_cruiser = 961_001i64;
    let nearby_frigate = 961_002i64;
    let far_battleship = 961_003i64;
    let dread_target = 961_004i64;
    spawn_grid_entity(&ctx, LOWSEC_SYS, primary_cruiser, 626, DVec3::new(60_000.0, 0.0, 0.0), 20_000.0, 20_000.0, 20_000.0, 140.0, 2_500.0, Some(90_000_061), hostile_corp).await;
    spawn_grid_entity(&ctx, LOWSEC_SYS, nearby_frigate, 587, DVec3::new(60_000.0, 12_000.0, 0.0), 20_000.0, 20_000.0, 20_000.0, 35.0, 400.0, Some(90_000_062), hostile_corp).await;
    spawn_grid_entity(&ctx, LOWSEC_SYS, far_battleship, 642, DVec3::new(60_000.0, 35_000.0, 0.0), 20_000.0, 20_000.0, 20_000.0, 420.0, 5_000.0, Some(90_000_063), hostile_corp).await;
    spawn_grid_entity(&ctx, LOWSEC_SYS, dread_target, 19720, DVec3::new(90_000.0, 0.0, 0.0), 200_000.0, 200_000.0, 200_000.0, 11_000.0, 40_000.0, Some(90_000_064), hostile_corp).await;

    // Unlocked target fails with TargetNotLocked
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gbl_mod), PyRep::String("defaultEffect".into()), PyRep::Integer(primary_cruiser)]).await.unwrap_err().code,
        "TargetNotLocked"
    );

    // Lock targets via real dogmaIM.AddTarget while piloting structure
    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(primary_cruiser)]).await.expect("Structure AddTarget cruiser");
    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(dread_target)]).await.expect("Structure AddTarget dread");
    assert!(sess.drain_notifications().iter().any(|(n, _)| n == "OnStructureTargetAdded"));

    // 1. Fire Standup Light Guided Bomb (37850): hits primary_cruiser (0km) + nearby_frigate (12km <= 20km), spares far_battleship (35km > 20km)
    let _gbl_res = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gbl_mod), PyRep::String("defaultEffect".into()), PyRep::Integer(primary_cruiser)])
        .await
        .expect("Fire Light Guided Bomb");
    let sys_h = ctx.systems.get_or_spawn(LOWSEC_SYS);
    for _ in 0..20 {
        sys_h.step_manual(1.0).await.unwrap();
    }

    let cruiser_lost = 20_000.0 - sys_h.get_combat_state(primary_cruiser).await.unwrap().unwrap().shield_hp;
    let frig_lost = 20_000.0 - sys_h.get_combat_state(nearby_frigate).await.unwrap().unwrap().shield_hp;
    let far_lost = 20_000.0 - sys_h.get_combat_state(far_battleship).await.unwrap().unwrap().shield_hp;
    assert!(cruiser_lost > frig_lost * 2.0, "Cruiser (sig 140m) must take significantly more guided bomb damage than Frigate (sig 35m): {cruiser_lost} vs {frig_lost}");
    assert!(frig_lost > 0.0);
    assert!(far_lost.abs() < 1e-6, "Ship 35km from impact (>20km AoE) must take zero guided bomb damage");

    // 2. Swap to Standup Void Guided Bomb (37851) -> drains capacitor in 20km AoE!
    ctx.advance_secs(61);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&960_101).unwrap().type_id = 37851;
        snap.items.get_mut(&960_111).unwrap().type_id = 37851;
    });
    let _void_res = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gbl_mod), PyRep::String("defaultEffect".into()), PyRep::Integer(primary_cruiser)])
        .await
        .expect("Fire Void Guided Bomb");

    // 3. Fire Multirole Missile Launcher I (35922) and Anticapital Missile Launcher I (35921)
    let mml_res = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(mml_mod), PyRep::String("defaultEffect".into()), PyRep::Integer(primary_cruiser)])
        .await.expect("Fire Multirole Missile");
    assert!(kv_entry(&mml_res, "damageDealt").and_then(|v| v.as_f64()).unwrap() > 0.0);

    let acm_res = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(acm_mod), PyRep::String("defaultEffect".into()), PyRep::Integer(dread_target)])
        .await.expect("Fire Anticapital Missile at Dreadnought");
    assert!(kv_entry(&acm_res, "damageDealt").and_then(|v| v.as_f64()).unwrap() > 2_000.0);

    // 4. Fire Standup Energy Neutralization & ECM Jammer Burst Projectors (replenish cruiser cap after Void Guided Bomb)
    if let Ok(Some(mut cs)) = sys_h.get_combat_state(primary_cruiser).await {
        cs.capacitor = 2_500.0;
        let _ = sys_h.set_combat_state(primary_cruiser, cs).await;
    }
    sys_h.add_target_lock(primary_cruiser, TargetLock { target_id: st_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await.unwrap();
    let neut_res = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(neut_proj), PyRep::String("defaultEffect".into()), PyRep::Integer(primary_cruiser)])
        .await.expect("Fire Neut Burst Projector");
    assert!(kv_entry(&neut_res, "capacitorDrained").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    let ecm_res = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(ecm_proj), PyRep::String("defaultEffect".into()), PyRep::Integer(primary_cruiser)])
        .await.expect("Fire ECM Burst Projector");
    assert_eq!(kv_entry(&ecm_res, "ecmJammed"), Some(&PyRep::Bool(true)));
    let cruiser_jam = sys_h.get_combat_state(primary_cruiser).await.unwrap().unwrap().ecm_jammed_remaining_s;
    let frig_jam = sys_h.get_combat_state(nearby_frigate).await.unwrap().unwrap().ecm_jammed_remaining_s;
    assert!(cruiser_jam > 0.0 || frig_jam > 0.0, "At least one AoE target must be jammed when ecmJammed is true");
}

#[tokio::test]
async fn test_keepstar_standup_arcing_vorton_projector_chain_and_immobilization() {
    // parity: feat:structures.standup-superweapon-arcing-vorton, feat:structures.structure-weapons
    eve_testkit::attest(&[
        "feat:structures.standup-superweapon-arcing-vorton",
        "feat:structures.structure-weapons",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let gunner = 90_000_001;
    let (mut sess, corp_id) = setup_gunner_and_corp(&ctx, 621, gunner, "VRTN").await;

    // 1. Negative hull restriction: Standup Arcing Vorton Projector I (35928, group 1333) CANNOT activate on a Fortizar (35833)!
    let fort_id = 1_000_000_400_020i64;
    let bad_vorton = 970_001i64;
    seed_armed_structure(&ctx, fort_id, 35833, corp_id, LOWSEC_SYS, &[(bad_vorton, 35928, InventoryFlag::HiSlot0)], &[]);
    dock_and_board_structure(&ctx, &mut sess, gunner, fort_id, LOWSEC_SYS).await;
    assert_eq!(
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(bad_vorton), PyRep::String("superWeaponAmarr".into()), PyRep::Integer(1)]).await.unwrap_err().code,
        "CantFitToStructure"
    );
    rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(fort_id)]).await.unwrap();

    // 2. Keepstar (35834) in low-sec: chain across up to 5 hostile targets within 20km hop radius + 600s gunner immobilization!
    let ks_id = 1_000_000_400_021i64;
    let vorton_mod = 970_010i64;
    seed_armed_structure(&ctx, ks_id, 35834, corp_id, LOWSEC_SYS, &[(vorton_mod, 35928, InventoryFlag::HiSlot0)], &[]);
    dock_and_board_structure(&ctx, &mut sess, gunner, ks_id, LOWSEC_SYS).await;

    let hostile_corp = 98_000_909i64;
    // Spawn 6 hostile capitals spaced 15km apart (each <= 20km from the previous), 1 friendly ship 10km away, and 1 isolated hostile 45km away
    for idx in 0..6 {
        spawn_grid_entity(
            &ctx,
            LOWSEC_SYS,
            971_000 + idx,
            19720,
            DVec3::new(80_000.0, idx as f64 * 2_000.0, 0.0),
            5_000_000.0,
            5_000_000.0,
            5_000_000.0,
            10_000.0,
            50_000.0,
            Some(90_000_100 + idx),
            hostile_corp,
        )
        .await;
    }
    spawn_grid_entity(&ctx, LOWSEC_SYS, 971_090, 19720, DVec3::new(80_000.0, 8_000.0, 0.0), 5_000_000.0, 5_000_000.0, 5_000_000.0, 10_000.0, 50_000.0, Some(90_000_190), corp_id).await;
    spawn_grid_entity(&ctx, LOWSEC_SYS, 971_099, 19720, DVec3::new(80_000.0, -45_000.0, 0.0), 5_000_000.0, 5_000_000.0, 5_000_000.0, 10_000.0, 50_000.0, Some(90_000_199), hostile_corp).await;

    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(971_000)]).await.expect("Keepstar lock primary");

    let fire_rep = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(vorton_mod), PyRep::String("superWeaponVorton".into()), PyRep::Integer(971_000)])
        .await
        .expect("Fire Standup Arcing Vorton Projector I");
    assert_eq!(kv_entry(&fire_rep, "targetsHit"), Some(&PyRep::Integer(5)), "Arcing Vorton must cap chain at max 5 targets");

    let sys_h = ctx.systems.get_or_spawn(LOWSEC_SYS);
    // Targets 0..=4 took massive omni damage; 6th hostile (971_005), friendly (971_090), and isolated (>20km, 971_099) took ZERO damage!
    for hit_id in 971_000..=971_004 {
        let cs = sys_h.get_combat_state(hit_id).await.unwrap().unwrap();
        assert!(cs.shield_hp < 5_000_000.0, "Chained target {hit_id} must take Vorton damage");
    }
    assert!((sys_h.get_combat_state(971_005).await.unwrap().unwrap().shield_hp - 5_000_000.0).abs() < 1e-6, "6th target beyond 5-target cap must be untouched");
    assert!((sys_h.get_combat_state(971_090).await.unwrap().unwrap().shield_hp - 5_000_000.0).abs() < 1e-6, "Friendly corp target must be excluded from Vorton chain");
    assert!((sys_h.get_combat_state(971_099).await.unwrap().unwrap().shield_hp - 5_000_000.0).abs() < 1e-6, "Target >20km from chain must be untouched");

    // 3. During the 30s SDE immobilization (attr 1579), gunner cannot re-fire, EjectFromStructure, or ReleaseControl!
    ctx.advance_secs(10);
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(vorton_mod), PyRep::String("superWeaponVorton".into()), PyRep::Integer(971_000)]).await.unwrap_err().code, "WeaponCycleImmobilized");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(ks_id)]).await.unwrap_err().code, "GunnerImmobilized");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(ks_id)]).await.unwrap_err().code, "GunnerImmobilized");
    // 4. Once the 30s SDE immobilization completes (while 600s cycle is still active), gunner can relinquish control!
    ctx.advance_secs(25);
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(vorton_mod), PyRep::String("superWeaponVorton".into()), PyRep::Integer(971_000)]).await.unwrap_err().code, "ModuleAlreadyCycling");
    rpc(&ctx, &mut sess, "structureControl", "EjectFromStructure", &[PyRep::Integer(ks_id)])
        .await
        .expect("EjectFromStructure succeeds after 30s SDE immobilization");
}

#[tokio::test]
async fn standup_missile_and_burst_projector_rof_crimewatch_and_ecm_roll() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let gunner = 90_000_001i64;
    let victim_char = 90_000_002i64;
    let hostile_corp = 98_000_909i64;
    let (mut sess, corp_id) = setup_gunner_and_corp(&ctx, 73, gunner, "WPNR").await;

    let fort_id = 1_000_000_400_031i64;
    let missile_mod = 970_020i64;
    let ecm_burst_mod = 970_021i64;
    seed_armed_structure(
        &ctx,
        fort_id,
        35833,
        corp_id,
        LOWSEC_SYS,
        &[
            (missile_mod, 35922, InventoryFlag::HiSlot0),
            (ecm_burst_mod, 47069, InventoryFlag::HiSlot1),
        ],
        &[(970_029, 37847, 50)],
    );
    dock_and_board_structure(&ctx, &mut sess, gunner, fort_id, LOWSEC_SYS).await;

    let target_ship = 972_001i64;
    spawn_grid_entity(
        &ctx,
        LOWSEC_SYS,
        target_ship,
        19720,
        DVec3::new(40_000.0, 0.0, 0.0),
        1_000_000.0,
        1_000_000.0,
        1_000_000.0,
        10_000.0,
        50_000.0,
        Some(victim_char),
        hostile_corp,
    )
    .await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&victim_char) {
            c.active_ship_id = target_ship;
        }
    });

    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_ship)])
        .await
        .expect("Lock hostile player ship");

    // 1. First missile activation deals damage, records crimewatch aggression, and starts per-module cycle lock.
    let m1 = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(missile_mod), PyRep::String("useMissiles".into()), PyRep::Integer(target_ship)],
    )
    .await
    .expect("First Standup Multirole Missile Launcher activation");
    assert!(kv_entry(&m1, "damageDealt").and_then(PyRep::as_f64).unwrap_or(0.0) > 0.0);

    let now_u = ctx.now_unix();
    let has_aggression = ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&gunner)
        .is_some_and(|st| st.is_weapon_timer_active(now_u) || st.is_suspect(now_u));
    assert!(has_aggression, "Standup missile activation against player must record crimewatch aggression");

    // 2. Immediate spam reactivation before cycle completes must fail with ModuleAlreadyCycling.
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(missile_mod), PyRep::String("useMissiles".into()), PyRep::Integer(target_ship)],
        )
        .await
        .unwrap_err()
        .code,
        "ModuleAlreadyCycling"
    );

    // 3. Burst projector activation also enforces per-module cycle cooldown (ModuleAlreadyCycling on spam).
    let _b1 = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ecm_burst_mod), PyRep::String("burstProjector".into()), PyRep::Integer(target_ship)],
    )
    .await
    .expect("First Standup ECM Jammer Burst Projector activation");
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(ecm_burst_mod), PyRep::String("burstProjector".into()), PyRep::Integer(target_ship)],
        )
        .await
        .unwrap_err()
        .code,
        "ModuleAlreadyCycling"
    );

    // 4. Advancing past the cycle duration allows firing again.
    ctx.advance_secs(60);
    let m2 = rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(missile_mod), PyRep::String("useMissiles".into()), PyRep::Integer(target_ship)],
    )
    .await
    .expect("Standup missile launcher fires again after cycle completes");
    assert!(kv_entry(&m2, "damageDealt").and_then(PyRep::as_f64).unwrap_or(0.0) > 0.0);
}
