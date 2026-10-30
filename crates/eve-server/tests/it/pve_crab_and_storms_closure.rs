//! Integration tests for CRAB Beacons, Character CEQ, System Signal Interference,
//! and Metaliminal Storm Movement (`feat:pve.crab-link-activation`,
//! `feat:pve.crab-ceq-signal-interference`, `feat:pve.metaliminal-storm-movement`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_containers_ships::validate_single_item_can_be_opened;
use eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs;
use eve_server::services::pve_crab_beacons::{
    CrabBeaconPhase, CRAB_LINK_DURATION_SECS, CRAB_LOOT_EXCLUSIVE_SECS, CRAB_SCAN_DURATION_SECS,
    TYPE_CRAB_BEACON,
};
use eve_server::services::pve_metaliminal_storms::{compute_storm_footprint, is_nullsec_storm_system};
use eve_server::services::pve_special_sites::{
    advance_pve_special_clock, ensure_special_sites_spawned, with_pve_special_reg,
    AbyssalWeatherKind, RoamingStormRecord,
};
use eve_server::services::space_destiny::space_nav_travel::sync_tether;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const LOWSEC_SYS: u32 = 30_002_904; // Tama (Low-sec 0.28)
const HIGHSEC_SYS: u32 = 30_000_142; // Jita (High-sec 0.95)
const WH_SYS: u32 = 31_000_240; // Wormhole J-space (non-shattered C3)
const TYPE_ARCHON: u32 = 23_757; // Carrier
const TYPE_RIFTER: u32 = 587; // Frigate

async fn setup_space_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(username.into()), PyRep::String(password.into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            11_584,
            TrainedSkillRecord {
                type_id: 11_584,
                level: 3,
                skill_points: 40_000.0,
            },
        );
        let p1_template = snap.characters[&PILOT_1].clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| p1_template);
        ch.character_id = char_id;
        ch.clone_grade = 2; // Omega clone so Anchoring III is uncapped
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_200_000_000.0,
                radius: 450.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;
    sess
}

// ============================================================================
// 1. CRAB Deployment Restrictions, Capital Link, Drone Waves & Loot Window
// ============================================================================
#[tokio::test]
async fn test_crab_deployment_capital_link_drone_waves_loot_and_self_destruct() {
    // parity: feat:pve.crab-link-activation
    eve_testkit::attest(&["feat:pve.crab-link-activation"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let safe_pos = DVec3::new(350_000_000.0, 120_000_000.0, -240_000_000.0);

    // 1. Deployment in High-Sec and Wormhole must fail
    let mut sess_hs = setup_space_pilot(&ctx, 8301, "demo", "demo-secret", PILOT_1, 730_001_001, TYPE_ARCHON, HIGHSEC_SYS, safe_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            730_100_001,
            InventoryItemRecord {
                item_id: 730_100_001,
                type_id: TYPE_CRAB_BEACON,
                owner_id: PILOT_1,
                location_id: 730_001_001,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let hs_err = ctx
        .dispatch_rpc(&mut sess_hs, "posMgr", None, "AnchorStructure", &[PyRep::Integer(730_100_001)], &kw, None)
        .await
        .expect_err("CRAB deploy in High-Sec must fail");
    assert_eq!(hs_err.code, "CantDeployInHighSec");

    let mut sess_wh = setup_space_pilot(&ctx, 8302, "demo", "demo-secret", PILOT_1, 730_001_002, TYPE_ARCHON, WH_SYS, safe_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            730_100_002,
            InventoryItemRecord {
                item_id: 730_100_002,
                type_id: TYPE_CRAB_BEACON,
                owner_id: PILOT_1,
                location_id: 730_001_002,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let wh_err = ctx
        .dispatch_rpc(&mut sess_wh, "posMgr", None, "AnchorStructure", &[PyRep::Integer(730_100_002)], &kw, None)
        .await
        .expect_err("CRAB deploy in Wormhole must fail");
    assert_eq!(wh_err.code, "CantDeployInWormhole");

    // 2. Deployment in Low-Sec succeeds; Subcapital link refused; Capital link succeeds
    let carrier_id = 730_001_003i64;
    let subcap_id = 730_001_004i64;
    let beacon_id = 730_100_003i64;
    let cloak_mod_id = 730_200_001i64;
    let mut sess_cap = setup_space_pilot(&ctx, 8303, "demo", "demo-secret", PILOT_1, carrier_id, TYPE_ARCHON, LOWSEC_SYS, safe_pos).await;
    let mut sess_sub = setup_space_pilot(
        &ctx,
        8304,
        "other",
        "other-secret",
        PILOT_2,
        subcap_id,
        TYPE_RIFTER,
        LOWSEC_SYS,
        safe_pos + DVec3::new(1_200.0, 0.0, 0.0),
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            beacon_id,
            InventoryItemRecord {
                item_id: beacon_id,
                type_id: TYPE_CRAB_BEACON,
                owner_id: PILOT_1,
                location_id: carrier_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: 11_578, // Prototype Cloaking Device I
                owner_id: PILOT_1,
                location_id: carrier_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    ctx.dispatch_rpc(&mut sess_cap, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None)
        .await
        .expect("Deploy CRAB in Low-Sec");

    // Subcapital link refused
    let sub_err = ctx
        .dispatch_rpc(
            &mut sess_sub,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(beacon_id),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Subcapital cannot link to CRAB");
    assert_eq!(sub_err.code, "IneligibleShipForCrabLink");

    // Anchor a cored Astrahus right at safe_pos so we verify tether before, during, and after CRAB link
    ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters.get(&PILOT_1).map(|c| c.corporation_id).unwrap_or(1_000_009);
        snap.structures.insert(
            1_030_000_001,
            UpwellStructureRecord {
                structure_id: 1_030_000_001,
                type_id: 35_832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: LOWSEC_SYS,
                name: "Lowsec Staging Astrahus".into(),
                position: safe_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
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
    let lowsec_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    sync_tether(&ctx, PILOT_1, LOWSEC_SYS, carrier_id).await;
    assert!(
        lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap().is_tethered,
        "Carrier tethers at cored friendly Astrahus before CRAB link"
    );

    // Record baseline carrier shield resonance before link
    let orig_sh_res = lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap().shield_resonance;

    // Capital ship links to CRAB beacon
    let link_rep = ctx
        .dispatch_rpc(
            &mut sess_cap,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(beacon_id),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Capital link to CRAB");
    assert_eq!(link_rep.dict_get("linked").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(link_rep.dict_get("linkDurationSeconds").and_then(|v| v.as_i64()), Some(240));
    assert_eq!(link_rep.dict_get("resistanceBonusPct").and_then(|v| v.as_f64()), Some(10.0));

    // 3. During the 4-minute link: cannot warp, cloak, or tether, and gets +10% universal resistance bonus
    let warp_err = ctx
        .dispatch_rpc(
            &mut sess_cap,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[
                PyRep::String("point".into()),
                PyRep::Real(safe_pos.x + 500_000_000.0),
                PyRep::Real(safe_pos.y),
                PyRep::Real(safe_pos.z),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Carrier cannot warp while linked to CRAB");
    assert_eq!(warp_err.code, "CrabLinkActive");

    let cloak_err = ctx
        .dispatch_rpc(&mut sess_cap, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_mod_id)], &kw, None)
        .await
        .expect_err("Carrier cannot cloak while linked to CRAB");
    assert_eq!(cloak_err.code, "CrabLinkActive");

    sync_tether(&ctx, PILOT_1, LOWSEC_SYS, carrier_id).await;
    assert!(
        !lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap().is_tethered,
        "Carrier cannot tether while linked to CRAB"
    );

    let linked_cs = lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap();
    assert!(
        (linked_cs.shield_resonance[0] - orig_sh_res[0] * 0.90).abs() < 1e-6,
        "10% universal resistance bonus applied during CRAB link"
    );

    // 4. Advance 4 minutes (240s) -> link ends, resistance reverts, tether restored, 10-minute scan begins & Wave 1 spawns
    ctx.advance_secs(CRAB_LINK_DURATION_SECS);
    sync_tether(&ctx, PILOT_1, LOWSEC_SYS, carrier_id).await;
    let post_link_cs = lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap();
    assert!(post_link_cs.is_tethered, "Carrier can tether again once 4m CRAB link completes");
    assert!(
        (post_link_cs.shield_resonance[0] - orig_sh_res[0]).abs() < 1e-6,
        "Resistance bonus cleared after 4m link completes"
    );
    let (phase_w1, waves_w1, drones_w1) = with_pve_special_reg(&ctx, |reg| {
        let b = &reg.crab_beacons[&beacon_id];
        (b.phase, b.waves_spawned, b.wave_npc_ball_ids.len())
    });
    assert_eq!(phase_w1, CrabBeaconPhase::Scanning);
    assert_eq!(waves_w1, 1);
    assert!(!drones_w1.eq(&0), "Wave 1 rogue drones must spawn");

    // Advance through Wave 2 (+200s), Wave 3 (+200s), and scan completion (+200s = 600s total)
    ctx.advance_secs(200);
    assert_eq!(with_pve_special_reg(&ctx, |reg| reg.crab_beacons[&beacon_id].waves_spawned), 2);
    ctx.advance_secs(200);
    assert_eq!(with_pve_special_reg(&ctx, |reg| reg.crab_beacons[&beacon_id].waves_spawned), 3);
    ctx.advance_secs(CRAB_SCAN_DURATION_SECS - 400);

    let phase_loot = with_pve_special_reg(&ctx, |reg| reg.crab_beacons[&beacon_id].phase);
    assert_eq!(phase_loot, CrabBeaconPhase::LootRestricted);

    // 5. During 2-minute exclusive loot window: only deployer can open; non-deployer is refused
    let non_deployer_open = validate_single_item_can_be_opened(&ctx, PILOT_2, beacon_id).await;
    assert_eq!(non_deployer_open.unwrap_err().code, "LootOwnershipRestricted");
    assert!(validate_single_item_can_be_opened(&ctx, PILOT_1, beacon_id).await.is_ok());

    // 6. After 2 minutes (120s), beacon self-destructs and drops open-to-all loot container
    ctx.advance_secs(CRAB_LOOT_EXCLUSIVE_SECS);
    let (phase_end, open_can_opt) = with_pve_special_reg(&ctx, |reg| {
        let b = &reg.crab_beacons[&beacon_id];
        (b.phase, b.open_container_id)
    });
    assert_eq!(phase_end, CrabBeaconPhase::SelfDestructed);
    assert!(lowsec_handle.get_ball_snapshot(beacon_id).await.unwrap().is_none(), "Beacon ball despawned");
    let open_can_id = open_can_opt.expect("Open loot container spawned on self-destruct");
    assert!(
        validate_single_item_can_be_opened(&ctx, PILOT_2, open_can_id).await.is_ok(),
        "Non-deployer can open loot container after 2m exclusivity expires"
    );
}

// ============================================================================
// 2. Character CEQ Pool & System Signal Interference
// ============================================================================
#[tokio::test]
async fn test_crab_ceq_pool_regeneration_and_system_signal_interference() {
    // parity: feat:pve.crab-ceq-signal-interference
    eve_testkit::attest(&["feat:pve.crab-ceq-signal-interference"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let base_pos = DVec3::new(400_000_000.0, 50_000_000.0, 100_000_000.0);

    let carrier_id = 730_002_001i64;
    let mut sess1 = setup_space_pilot(&ctx, 8311, "demo", "demo-secret", PILOT_1, carrier_id, TYPE_ARCHON, LOWSEC_SYS, base_pos).await;

    // 1. Verify initial CEQ is capped at 100
    let energy0 = ctx
        .dispatch_rpc(&mut sess1, "characterEnergyMgr", None, "GetMyEnergyState", &[], &kw, None)
        .await
        .expect("GetMyEnergyState");
    assert_eq!(energy0.dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(100));
    assert_eq!(energy0.dict_get("maxEnergy").and_then(|v| v.as_i64()), Some(100));

    // 2. Link 3 CRAB beacons (3 * 30 = 90 CEQ); 4th activation refused with InsufficientEnergy
    for idx in 0..4i64 {
        let bid = 730_110_000 + idx;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: TYPE_CRAB_BEACON,
                    owner_id: PILOT_1,
                    location_id: carrier_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(bid)], &kw, None)
            .await
            .expect("Anchor CRAB");
        let res = ctx
            .dispatch_rpc(
                &mut sess1,
                "beyonce",
                None,
                "CallComponentFromClient",
                &[
                    PyRep::Integer(bid),
                    PyRep::String("linkWithShip".into()),
                    PyRep::String("InitiateLink".into()),
                ],
                &kw,
                None,
            )
            .await;
        if idx < 3 {
            assert!(res.is_ok(), "Activation {} within 100 CEQ cap must succeed: {res:?}", idx + 1);
        } else {
            assert_eq!(res.unwrap_err().code, "InsufficientEnergy", "4th CRAB activation must be refused");
        }
    }

    // 3. Advance 22 hours -> CEQ regenerates back to 100
    ctx.advance_secs(22 * 3_600);
    let energy_regen = ctx
        .dispatch_rpc(&mut sess1, "characterEnergyMgr", None, "GetMyEnergyState", &[], &kw, None)
        .await
        .expect("GetMyEnergyState after 22h");
    assert_eq!(energy_regen.dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(100));

    // 4. Signal Interference rises per CRAB across pilots until linking is refused at 12 CRABs,
    // and decays back over 25 hours
    let test_sys = 30_002_905u32; // Kedama (Low-sec)
    let mut pilots: Vec<(String, i64)> = Vec::new();
    for idx in 1..=4 {
        let uname = format!("crabp{idx}");
        let uid = ctx.db.provision_player_account(&uname, "crab-secret");
        let cid = ctx
            .db
            .create_character(uid, &format!("CRAB Pilot {idx}"), 1, 1, 11, 17, &ctx.sde)
            .expect("create CRAB pilot")
            .character_id;
        pilots.push((uname, cid));
    }
    for p_idx in 0..13usize {
        let (uname, pid) = &pilots[p_idx % pilots.len()];
        let pid = *pid;
        let sid = 730_020_000 + (p_idx as i64);
        let bid = 730_120_000 + (p_idx as i64);
        let p_pos = base_pos + DVec3::new((p_idx as f64) * 50_000.0, 0.0, 0.0);
        let mut p_sess = setup_space_pilot(&ctx, 8320 + (p_idx as u64), uname, "crab-secret", pid, sid, TYPE_ARCHON, test_sys, p_pos).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: TYPE_CRAB_BEACON,
                    owner_id: pid,
                    location_id: sid,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        ctx.dispatch_rpc(&mut p_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(bid)], &kw, None)
            .await
            .expect("Anchor CRAB");
        let link_res = ctx
            .dispatch_rpc(
                &mut p_sess,
                "beyonce",
                None,
                "CallComponentFromClient",
                &[
                    PyRep::Integer(bid),
                    PyRep::String("linkWithShip".into()),
                    PyRep::String("InitiateLink".into()),
                ],
                &kw,
                None,
            )
            .await;
        if p_idx < 12 {
            assert!(link_res.is_ok(), "CRAB #{} in system must succeed: {link_res:?}", p_idx + 1);
        } else {
            assert_eq!(link_res.unwrap_err().code, "ExcessInterference", "13th CRAB in system must be refused");
        }
    }

    let int_full = ctx
        .dispatch_rpc(
            &mut sess1,
            "solarsystemInterferenceMgr",
            None,
            "GetLocalInterferenceState",
            &[PyRep::Integer(i64::from(test_sys))],
            &kw,
            None,
        )
        .await
        .expect("GetLocalInterferenceState");
    assert_eq!(int_full.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()), Some(0));

    // Advance 25 hours -> interference decays to 0 and 12 activations are available again
    ctx.advance_secs(25 * 3_600);
    let int_decayed = ctx
        .dispatch_rpc(
            &mut sess1,
            "solarsystemInterferenceMgr",
            None,
            "GetLocalInterferenceState",
            &[PyRep::Integer(i64::from(test_sys))],
            &kw,
            None,
        )
        .await
        .expect("GetLocalInterferenceState after 25h");
    assert_eq!(int_decayed.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()), Some(12));
    assert!((int_decayed.dict_get("interferenceLevel").and_then(|v| v.as_f64()).unwrap_or(1.0)).abs() < 1e-6);
}

// ============================================================================
// 3. Metaliminal Storm Core/Periphery, Movement Cadence & Overlapping Storms
// ============================================================================
#[tokio::test]
async fn test_metaliminal_storm_core_periphery_movement_and_overlap() {
    // parity: feat:pve.metaliminal-storm-movement
    eve_testkit::attest(&["feat:pve.metaliminal-storm-movement"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    let (storm_id, center_0, core_0, periph_0, all_core_systems) = with_pve_special_reg(&ctx, |reg| {
        let all_cores: HashSet<u32> = reg
            .roaming_storms
            .iter()
            .flat_map(|st| st.strong_core_systems.iter().copied())
            .collect();
        let s = &reg.roaming_storms[0];
        (
            s.storm_id,
            s.center_solar_system_id,
            s.strong_core_systems.clone(),
            s.weak_periphery_systems.clone(),
            all_cores,
        )
    });

    assert!(is_nullsec_storm_system(&ctx.sde, center_0), "Storm center must be null-sec");
    assert!(core_0.contains(&center_0), "Core contains center system");
    assert!(core_0.len() > 1, "Core contains center + 1-jump neighbors");
    let periph_only = periph_0
        .iter()
        .copied()
        .find(|s| !all_core_systems.contains(s))
        .expect("Periphery system 2-3 jumps away outside all storm cores");

    // 1. Extra storm PvE anomalies spawn ONLY in the core, never in periphery-only systems
    let core_has_storm_site = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&center_0)
            .is_some_and(|l| l.iter().any(|a| a.anomaly_id.starts_with("DNG-9042-") || a.anomaly_id.starts_with("DNG-9043-")))
    });
    let periph_has_storm_site = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&periph_only)
            .is_some_and(|l| l.iter().any(|a| a.anomaly_id.starts_with("DNG-9042-") || a.anomaly_id.starts_with("DNG-9043-")))
    });
    assert!(core_has_storm_site, "Storm core must spawn extra storm PvE sites");
    assert!(!periph_has_storm_site, "Storm periphery must not spawn extra storm PvE sites");

    // 2. Storm center does NOT move before 24h, moves 1 jump every 24-48h, and avoids retracing
    advance_pve_special_clock(&ctx, 12 * 3_600);
    let center_at_12h = with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.iter().find(|s| s.storm_id == storm_id).unwrap().center_solar_system_id
    });
    assert_eq!(center_at_12h, center_0, "Storm must not move before 24h");

    advance_pve_special_clock(&ctx, 36 * 3_600); // 48h total
    let (center_1, recent_1) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.roaming_storms.iter().find(|s| s.storm_id == storm_id).unwrap();
        (s.center_solar_system_id, s.recent_centers.clone())
    });
    assert_ne!(center_1, center_0, "Storm center must move after 24-48h");
    assert!(core_0.contains(&center_1), "Storm moves exactly 1 jump to a neighbor in its former core");

    advance_pve_special_clock(&ctx, 48 * 3_600);
    let center_2 = with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.iter().find(|s| s.storm_id == storm_id).unwrap().center_solar_system_id
    });
    assert_ne!(center_2, center_1);
    assert_ne!(center_2, center_0, "Storm movement must avoid retracing its previous center");
    assert!(recent_1.contains(&center_0));

    // 3. Overlapping storms apply BOTH effect sets simultaneously
    let overlap_sys = 30_000_208u32;
    let ship_id = 730_030_001i64;
    let _sess = setup_space_pilot(&ctx, 8331, "demo", "demo-secret", PILOT_1, ship_id, TYPE_RIFTER, overlap_sys, DVec3::ZERO).await;

    let (elec_core, elec_periph) = compute_storm_footprint(&ctx, overlap_sys);
    let (gamma_core, gamma_periph) = compute_storm_footprint(&ctx, overlap_sys);
    with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms = vec![
            RoamingStormRecord {
                storm_id: 91,
                weather: AbyssalWeatherKind::Electrical,
                center_solar_system_id: overlap_sys,
                strong_core_systems: elec_core,
                weak_periphery_systems: elec_periph,
                step_count: 0,
                next_move_at_sec: u64::MAX,
                recent_centers: vec![overlap_sys],
            },
            RoamingStormRecord {
                storm_id: 92,
                weather: AbyssalWeatherKind::Gamma,
                center_solar_system_id: overlap_sys,
                strong_core_systems: gamma_core,
                weak_periphery_systems: gamma_periph,
                step_count: 0,
                next_move_at_sec: u64::MAX,
                recent_centers: vec![overlap_sys],
            },
        ];
    });
    eve_server::services::pve_metaliminal_storms::sync_metaliminal_storm_effects_and_sites(&ctx);

    // Electrical boosts capacitor recharge (attr 55: *0.50); Gamma boosts shield HP (attr 263: *1.50)
    let mut attrs = HashMap::from([(55, 100.0), (263, 1_000.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, ship_id, &mut attrs);
    assert!(attrs[&55] < 100.0, "Electrical storm effect set applied on overlap");
    assert!(attrs[&263] > 1_000.0, "Gamma storm effect set applied on overlap");
}
