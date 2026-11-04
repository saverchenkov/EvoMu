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

// 1. CRAB Deployment Restrictions, Capital Link, Drone Waves & Loot Window
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
    assert_eq!(with_pve_special_reg(&ctx, |reg| reg.crab_beacons[&beacon_id].phase), CrabBeaconPhase::Scanning, "Live Rogue Drones jam CRAB completion");
    for bid in with_pve_special_reg(&ctx, |reg| reg.crab_beacons[&beacon_id].wave_npc_ball_ids.clone()) {
        let _ = lowsec_handle.remove_ball(bid).await;
    }
    eve_server::services::pve_crab_beacons::tick_all_crab_beacons(&ctx);
    assert_eq!(with_pve_special_reg(&ctx, |reg| reg.crab_beacons[&beacon_id].phase), CrabBeaconPhase::LootRestricted);

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
            ctx.advance_secs(CRAB_LINK_DURATION_SECS);
        } else {
            assert_eq!(res.unwrap_err().code, "InsufficientEnergy", "4th CRAB activation must be refused");
        }
    }

    ctx.advance_secs(22 * 3_600);
    let energy_regen = ctx
        .dispatch_rpc(&mut sess1, "characterEnergyMgr", None, "GetMyEnergyState", &[], &kw, None)
        .await
        .expect("GetMyEnergyState after 22h");
    assert_eq!(energy_regen.dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(100));

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

pub(crate) mod crab_negative_exploits_subcap_double {
    // Part 2 negative/regression integration tests for CRAB Beacon Exploits
    // (`11019` subcapital rejection, simultaneous double-link refusal, non-compounding
    // dogma resonance bonus/restore, Rogue Drone scan-jamming) and Overlapping Metaliminal
    // Storms with Cross-Instance Isolation (`feat:pve.crab-link-activation`,
    // `feat:pve.crab-ceq-signal-interference`, `feat:pve.metaliminal-storm-movement`).


    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
    use eve_destiny::{Ball, DamageVector, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::inventory_fitting::fitting_containers_ships::validate_single_item_can_be_opened;
    use eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs;
    use eve_server::services::pve_crab_beacons::{
        tick_all_crab_beacons, CrabBeaconPhase, CRAB_LINK_DURATION_SECS, CRAB_LOOT_EXCLUSIVE_SECS,
        CRAB_SCAN_DURATION_SECS, TYPE_CRAB_BEACON,
    };
    use eve_server::services::pve_metaliminal_storms::{
        compute_storm_footprint, sync_metaliminal_storm_effects_and_sites,
    };
    use eve_server::services::pve_special_sites::{
        with_pve_special_reg, AbyssalWeatherKind, RoamingStormRecord,
    };
    use glam::DVec3;
    use std::collections::HashMap;

    const PILOT_1: i64 = 90_000_001;
    const LOWSEC_SYS: u32 = 30_002_904; // Tama (Low-sec 0.28)
    const TYPE_ARCHON: u32 = 23_757; // Carrier
    const TYPE_COCKROACH: u32 = 11_019; // Subcapital QA frigate (must NOT bypass capital check)
    const TYPE_EM_SHIELD_AMP_I: u32 = 1_808; // Passive EM Shield Amplifier I (T1)

    async fn setup_pilot(
        ctx: &ServerContext,
        sess_id: u64,
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
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
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
            snap.skills.entry(char_id).or_default().insert(
                3_425,
                TrainedSkillRecord {
                    type_id: 3_425,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.clone_grade = 2;
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
            }
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
    // 1. CRAB Exploit Guards: 11019 Subcap, Simultaneous Link, Dogma Resonance & Drone Jam
    // ============================================================================
    #[tokio::test]
    async fn test_crab_negative_exploits_subcap_11019_double_link_resonance_and_drone_jam() {
        // parity: feat:pve.crab-link-activation, feat:pve.crab-ceq-signal-interference
        eve_testkit::attest(&[
            "feat:pve.crab-link-activation",
            "feat:pve.crab-ceq-signal-interference",
        ]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let pos = DVec3::new(320_000_000.0, 20_000_000.0, 120_000_000.0);

        // 1. Subcapital type_id 11019 (Cockroach) must be rejected with IneligibleShipForCrabLink
        let roach_id = 731_001_001i64;
        let mut sess_roach = setup_pilot(&ctx, 8501, PILOT_1, roach_id, TYPE_COCKROACH, LOWSEC_SYS, pos).await;
        let b1 = 731_100_001i64;
        let b2 = 731_100_002i64;
        for bid in [b1, b2] {
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.insert(
                    bid,
                    InventoryItemRecord {
                        item_id: bid,
                        type_id: TYPE_CRAB_BEACON,
                        owner_id: PILOT_1,
                        location_id: roach_id,
                        flag: InventoryFlag::Cargo,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            });
            ctx.dispatch_rpc(&mut sess_roach, "posMgr", None, "AnchorStructure", &[PyRep::Integer(bid)], &kw, None)
                .await
                .expect("Anchor CRAB");
        }

        let roach_err = ctx
            .dispatch_rpc(
                &mut sess_roach,
                "beyonce",
                None,
                "CallComponentFromClient",
                &[
                    PyRep::Integer(b1),
                    PyRep::String("linkWithShip".into()),
                    PyRep::String("InitiateLink".into()),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("11019 Cockroach must not bypass CRAB capital ship restriction");
        assert_eq!(roach_err.code, "IneligibleShipForCrabLink");

        // 2. Carrier links b1; attempting to link b2 while already linked to b1 fails with ShipAlreadyLinkedToCrab
        //    WITHOUT consuming CEQ or increasing signal interference!
        let carrier_id = 731_001_002i64;
        let mut sess_cap = setup_pilot(&ctx, 8502, PILOT_1, carrier_id, TYPE_ARCHON, LOWSEC_SYS, pos).await;
        let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
        let clean_base_res = sys_handle.get_combat_state(carrier_id).await.unwrap().unwrap().shield_resonance;

        let link1 = ctx
            .dispatch_rpc(
                &mut sess_cap,
                "beyonce",
                None,
                "CallComponentFromClient",
                &[
                    PyRep::Integer(b1),
                    PyRep::String("linkWithShip".into()),
                    PyRep::String("InitiateLink".into()),
                ],
                &kw,
                None,
            )
            .await
            .expect("Carrier links b1");
        assert_eq!(link1.dict_get("ceqRemaining").and_then(|v| v.as_i64()), Some(70));

        let double_err = ctx
            .dispatch_rpc(
                &mut sess_cap,
                "beyonce",
                None,
                "CallComponentFromClient",
                &[
                    PyRep::Integer(b2),
                    PyRep::String("linkWithShip".into()),
                    PyRep::String("InitiateLink".into()),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("Already-linked carrier cannot simultaneously link a 2nd CRAB");
        assert_eq!(double_err.code, "ShipAlreadyLinkedToCrab");
        let ceq_after_reject = ctx
            .dispatch_rpc(&mut sess_cap, "characterEnergyMgr", None, "GetMyEnergyState", &[], &kw, None)
            .await
            .unwrap();
        assert_eq!(
            ceq_after_reject.dict_get("currentEnergy").and_then(|v| v.as_i64()),
            Some(70),
            "Rejected double-link must not deduct CEQ"
        );

        // 3. Fit a passive shield resistance module DURING the active link; when the 4-minute link ends,
        //    clear_crab_link_ship_effects must rebuild dogma resonances (preserving the fitted module)
        //    rather than overwriting with pre-link snapshot resonances or compounding 0.90!
        let amp_id = 731_009_001i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                amp_id,
                InventoryItemRecord {
                    item_id: amp_id,
                    type_id: TYPE_EM_SHIELD_AMP_I,
                    owner_id: PILOT_1,
                    location_id: carrier_id,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(amp_id, true);
        });
        ctx.invalidate_ship_fitting(carrier_id);
        let expected_post_fit_em_res = ctx.build_ship_combat_state(PILOT_1, carrier_id, TYPE_ARCHON).shield_resonance[0];
        assert!(
            expected_post_fit_em_res < clean_base_res[0],
            "Fitted EM Shield Amplifier lowers EM damage resonance"
        );

        ctx.advance_secs(CRAB_LINK_DURATION_SECS);
        let post_unlink_res = sys_handle.get_combat_state(carrier_id).await.unwrap().unwrap().shield_resonance;
        assert!(
            (post_unlink_res[0] - expected_post_fit_em_res).abs() < 1e-6,
            "Unlinking must restore current dogma-fitted EM resonance ({expected_post_fit_em_res}), got {}",
            post_unlink_res[0]
        );

        // 4. Rogue Drone Jamming: advance 10m (600s) + extra 300s WITHOUT killing Rogue Drones ->
        //    CRAB beacon stays in Scanning phase (`CrabScanInProgress`), and ONLY transitions to
        //    LootRestricted with a full 120s deployer window once all 3 Rogue Drone waves are killed!
        ctx.advance_secs(CRAB_SCAN_DURATION_SECS + 300);
        let (phase_jammed, waves_jammed, live_drones) = with_pve_special_reg(&ctx, |reg| {
            let b = &reg.crab_beacons[&b1];
            (b.phase, b.waves_spawned, b.wave_npc_ball_ids.clone())
        });
        assert_eq!(waves_jammed, 3);
        assert!(!live_drones.is_empty());
        assert_eq!(
            phase_jammed,
            CrabBeaconPhase::Scanning,
            "CRAB scan must remain paused while Rogue Drone waves are alive"
        );
        assert_eq!(
            validate_single_item_can_be_opened(&ctx, PILOT_1, b1).await.unwrap_err().code,
            "CrabScanInProgress"
        );

        for bid in live_drones {
            let _ = sys_handle
                .apply_damage_with_attacker(
                    bid,
                    DamageVector {
                        em: 1.0e6,
                        thermal: 1.0e6,
                        kinetic: 1.0e6,
                        explosive: 1.0e6,
                    },
                    Some(PILOT_1),
                )
                .await;
        }
        tick_all_crab_beacons(&ctx);
        let (phase_cleared, sd_at) = with_pve_special_reg(&ctx, |reg| {
            let b = &reg.crab_beacons[&b1];
            (b.phase, b.self_destruct_at_sec)
        });
        assert_eq!(phase_cleared, CrabBeaconPhase::LootRestricted);
        assert_eq!(sd_at, ctx.now_unix() + CRAB_LOOT_EXCLUSIVE_SECS);
        assert!(validate_single_item_can_be_opened(&ctx, PILOT_1, b1).await.is_ok());
    }

    // ============================================================================
    // 2. Overlapping Storms Stack Both Weather Modifiers & Isolate Per-ServerContext
    // ============================================================================
    #[tokio::test]
    async fn test_overlapping_storms_stack_effects_without_cross_context_leakage() {
        // parity: feat:pve.metaliminal-storm-movement
        eve_testkit::attest(&["feat:pve.metaliminal-storm-movement"]);

        let ctx1 = ServerContext::new();
        let ctx2 = ServerContext::new();
        crate::common::pin_clock(&ctx1);
        crate::common::pin_clock(&ctx2);

        let overlap_sys = 30_000_208_u32;
        let ship_id = 731_050_001_i64;
        let _sess1 = setup_pilot(&ctx1, 8510, PILOT_1, ship_id, 587, overlap_sys, DVec3::ZERO).await;
        let _sess2 = setup_pilot(&ctx2, 8511, PILOT_1, ship_id, 587, overlap_sys, DVec3::ZERO).await;

        let (core_a, periph_a) = compute_storm_footprint(&ctx1, overlap_sys);
        let (core_b, periph_b) = compute_storm_footprint(&ctx1, overlap_sys);
        with_pve_special_reg(&ctx1, |reg| {
            reg.roaming_storms = vec![
                RoamingStormRecord {
                    storm_id: 201,
                    weather: AbyssalWeatherKind::Electrical,
                    center_solar_system_id: overlap_sys,
                    strong_core_systems: core_a,
                    weak_periphery_systems: periph_a,
                    step_count: 0,
                    next_move_at_sec: u64::MAX,
                    recent_centers: vec![overlap_sys],
                },
                RoamingStormRecord {
                    storm_id: 202,
                    weather: AbyssalWeatherKind::Firestorm,
                    center_solar_system_id: overlap_sys,
                    strong_core_systems: core_b,
                    weak_periphery_systems: periph_b,
                    step_count: 0,
                    next_move_at_sec: u64::MAX,
                    recent_centers: vec![overlap_sys],
                },
            ];
        });
        sync_metaliminal_storm_effects_and_sites(&ctx1);

        // Electrical lowers capacitor recharge time (attr 55: *0.50); Firestorm boosts armor HP (attr 265: *1.50)
        let mut attrs1 = HashMap::from([(55_u16, 100.0), (265_u16, 1_000.0)]);
        apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx1, ship_id, &mut attrs1);
        assert!((attrs1[&55] - 50.0).abs() < 1e-6, "Electrical storm modifier applied");
        assert!((attrs1[&265] - 1_500.0).abs() < 1e-6, "Firestorm modifier stacked simultaneously");

        // Separate ServerContext ctx2 in the same solar system has NO storm beacons and must stay unaffected
        let mut attrs2 = HashMap::from([(55_u16, 100.0), (265_u16, 1_000.0)]);
        apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx2, ship_id, &mut attrs2);
        assert_eq!(attrs2[&55], 100.0, "Storm effects in ctx1 must not leak into ctx2");
        assert_eq!(attrs2[&265], 1_000.0, "Storm effects in ctx1 must not leak into ctx2");
    }

    // ============================================================================
    // 3. Electrical Storm Hacking Virus Coherence Bonus (+25 Core / +10 Periphery)
    // ============================================================================
    #[tokio::test]
    async fn test_electrical_storm_grants_hacking_virus_coherence_per_context() {
        // parity: feat:pve.metaliminal-storms, feat:pve.hacking-node-types
        eve_testkit::attest(&["feat:pve.metaliminal-storms", "feat:pve.hacking-node-types"]);

        let ctx1 = ServerContext::new();
        let ctx2 = ServerContext::new();
        crate::common::pin_clock(&ctx1);
        crate::common::pin_clock(&ctx2);

        let center_sys = 30_000_208_u32;
        let (core_sys_list, periph_sys_list) = compute_storm_footprint(&ctx1, center_sys);
        let periph_sys = periph_sys_list
            .iter()
            .copied()
            .find(|s| !core_sys_list.contains(s))
            .expect("distinct periphery system");

        let ship_core = 731_060_001_i64;
        let mut sess_core = setup_pilot(&ctx1, 8520, PILOT_1, ship_core, 587, center_sys, DVec3::ZERO).await;
        let sess_iso = setup_pilot(&ctx2, 8522, PILOT_1, ship_core, 587, center_sys, DVec3::ZERO).await;

        for (ctx, char_id, ship_id, mod_id) in [
            (&ctx1, PILOT_1, ship_core, 731_060_101_i64),
            (&ctx2, PILOT_1, ship_core, 731_060_103_i64),
        ] {
            ctx.db.with_mut_snapshot(|snap| {
                snap.skills.entry(char_id).or_default().insert(
                    21718,
                    eve_db::TrainedSkillRecord { type_id: 21718, level: 5, skill_points: 256_000.0 },
                );
                snap.items.insert(
                    mod_id,
                    InventoryItemRecord {
                        item_id: mod_id,
                        type_id: 22175, // Data Analyzer I (40 base coherence, 20 strength)
                        owner_id: char_id,
                        location_id: ship_id,
                        flag: InventoryFlag::MedSlot0,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            });
        }

        let (base_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
            &ctx1, &sess_core, PILOT_1, false,
        )
        .expect("baseline analyzer stats");
        assert_eq!(base_coh, 90, "Baseline Data Analyzer I + Hacking V coherence = 90");

        with_pve_special_reg(&ctx1, |reg| {
            reg.roaming_storms = vec![RoamingStormRecord {
                storm_id: 301,
                weather: AbyssalWeatherKind::Electrical,
                center_solar_system_id: center_sys,
                strong_core_systems: core_sys_list,
                weak_periphery_systems: periph_sys_list,
                step_count: 0,
                next_move_at_sec: u64::MAX,
                recent_centers: vec![center_sys],
            }];
        });
        sync_metaliminal_storm_effects_and_sites(&ctx1);

        let (core_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
            &ctx1, &sess_core, PILOT_1, false,
        )
        .expect("core storm analyzer stats");
        let (iso_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
            &ctx2, &sess_iso, PILOT_1, false,
        )
        .expect("isolated context analyzer stats");

        assert_eq!(core_coh, 115, "Strong Electrical Storm core grants +25 virus coherence (90 -> 115)");
        assert_eq!(iso_coh, 90, "Electrical Storm in ctx1 must not leak coherence bonus into ctx2");

        // Move pilot in ctx1 into the weak storm periphery system and verify +10 coherence bonus.
        sess_core.solar_system_id = Some(periph_sys);
        ctx1.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
                ch.solar_system_id = periph_sys;
            }
            if let Some(ship) = snap.items.get_mut(&ship_core) {
                ship.location_id = i64::from(periph_sys);
            }
        });
        let (periph_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
            &ctx1, &sess_core, PILOT_1, false,
        )
        .expect("periphery storm analyzer stats");
        assert_eq!(periph_coh, 100, "Weak Electrical Storm periphery grants +10 virus coherence (90 -> 100)");
    }
}

pub(crate) mod metaliminal_storms_roaming_dogma_effects {
    // Comprehensive behavioral verification tests for Metaliminal Roaming Storms,
    // Roaming Weather Map RPC, and Homefront Operation Instance Caching
    // (`feat:pve.metaliminal-storms`, `feat:pve.homefront-ops`, `rpc:map.GetAllRoamingWeatherSystems`, `rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances`).

    use eve_destiny::DamageVector;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::pve_special_sites::{
        advance_pve_special_clock, ensure_special_sites_spawned, with_pve_special_reg,
    };
    use std::collections::HashMap;

    async fn setup_pilot_session(
        ctx: &ServerContext,
        session_id: u64,
        solar_system_id: u32,
        char_id: i64,
    ) -> SessionState {
        let kw = HashMap::new();
        let mut sess = SessionState::new(session_id);
        let (user, secret) = if char_id == 90_000_002 {
            ("other", "other-secret")
        } else {
            ("demo", "demo-secret")
        };
        ctx.dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[PyRep::String(user.into()), PyRep::String(secret.into())],
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

        sess.solar_system_id = Some(solar_system_id);
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(char_id + 10_000_000);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = solar_system_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = char_id + 10_000_000;
            }
        });

        sess
    }

    #[tokio::test]
    async fn test_metaliminal_storms_roaming_dogma_effects_and_sites() {
        // parity: feat:pve.metaliminal-storms, rpc:map.GetAllRoamingWeatherSystems
        eve_testkit::attest(&["feat:pve.metaliminal-storms"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;

        let kw = HashMap::new();
        let mut sess = setup_pilot_session(&ctx, 8911, 30_002_904, 90_000_001).await;

        // 1. Verify map.GetAllRoamingWeatherSystems returns V24 shape with locationID and sceneType
        let storms_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetAllRoamingWeatherSystems",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetAllRoamingWeatherSystems");
        let PyRep::List(storms) = &storms_rep else {
            panic!("Expected List from GetAllRoamingWeatherSystems");
        };
        assert!(!storms.is_empty(), "Must return active roaming weather systems");

        for entry in storms {
            let loc = entry.dict_get("locationID").and_then(|v| v.as_i64()).expect("locationID");
            let scene = entry.dict_get("sceneType").and_then(|v| v.as_i64()).expect("sceneType");
            assert!(loc > 0, "Location ID must be valid");
            assert!(scene == 1 || scene == 4, "sceneType must be 4 (core) or 1 (periphery)");
            let sec = ctx.sde.solar_systems.get(&(loc as u32)).map(|s| s.security_status).unwrap_or(1.0);
            assert!(sec < 0.45, "Metaliminal storm systems must only be lowsec/nullsec, got sec={sec}");
        }

        // 2. Verify initial storm centers are in nullsec/lowsec, not Jita (30000142)
        let initial_centers = with_pve_special_reg(&ctx, |reg| {
            reg.roaming_storms.iter().map(|s| (s.storm_id, s.center_solar_system_id, s.weather)).collect::<Vec<_>>()
        });
        assert_eq!(initial_centers.len(), 4, "Must have 4 roaming metaliminal storms");
        for (_, center, _) in &initial_centers {
            let sec = ctx.sde.solar_systems.get(center).map(|s| s.security_status).unwrap_or(1.0);
            assert!(sec < 0.45, "Storm center must be nullsec/lowsec, not highsec");
            assert_ne!(*center, 30_000_142, "Storm center must never be highsec Jita");
        }

        // 3. Verify system-wide environment dogma effects and bonus mining sites are applied
        let elec_center = initial_centers.iter().find(|(_, _, w)| *w == eve_server::services::pve_special_sites::AbyssalWeatherKind::Electrical).unwrap().1;
        let has_strong_beacon = ctx.db.with_snapshot(|snap| {
            snap.items.values().any(|it| it.location_id == elec_center as i64 && it.type_id == 47382)
        });
        assert!(has_strong_beacon, "Core electrical system must have strong weather beacon 47382");

        let has_volatile_ice = ctx.db.with_snapshot(|snap| {
            snap.cosmic_anomalies.get(&elec_center).into_iter().flatten().any(|a| a.anomaly_id.starts_with("DNG-9042-"))
        });
        assert!(has_volatile_ice, "Core storm system must spawn bonus Fresh Volatile Ice Field (9042)");

        // 4. Advance clock by 48h (172,800s): storms roam 1 jump along connected nullsec stargates
        advance_pve_special_clock(&ctx, 172_800);

        let storms_rep_after = ctx
            .dispatch_rpc(
                &mut sess,
                "map",
                None,
                "GetAllRoamingWeatherSystems",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetAllRoamingWeatherSystems after roam");
        let PyRep::List(storms_after) = &storms_rep_after else {
            panic!("Expected List");
        };
        assert!(!storms_after.is_empty());

        let moved_centers = with_pve_special_reg(&ctx, |reg| {
            reg.roaming_storms.iter().map(|s| (s.storm_id, s.center_solar_system_id)).collect::<Vec<_>>()
        });
        assert!(
            moved_centers.iter().any(|(id, new_c)| {
                let old_c = initial_centers.iter().find(|(sid, _, _)| sid == id).unwrap().1;
                *new_c != old_c
            }),
            "At least one storm center must have moved along stargates"
        );
    }

    #[tokio::test]
    async fn test_homefront_site_instances_v24_registry_and_completion() {
        // parity: feat:pve.homefront-ops, rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances
        eve_testkit::attest(&["feat:pve.homefront-ops"]);

        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ensure_special_sites_spawned(&ctx).await;

        let kw = HashMap::new();
        let jita = 30_000_142_u32;
        let mut sess = setup_pilot_session(&ctx, 8912, jita, 90_000_002).await;

        // 1. Query dungeonInstanceCacheMgr.GetHomefrontSiteInstances with V24 dict return shape
        let hf_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "dungeonInstanceCacheMgr",
                None,
                "GetHomefrontSiteInstances",
                &[PyRep::Integer(jita as i64)],
                &kw,
                None,
            )
            .await
            .expect("GetHomefrontSiteInstances for Jita");
        let PyRep::Dict(hf_dict) = &hf_rep else {
            panic!("Expected Dict from GetHomefrontSiteInstances");
        };

        let Some((_, PyRep::Dict(jita_dungeons))) = hf_dict.iter().find(|(k, _)| k.as_i64() == Some(jita as i64)) else {
            panic!("Dict must contain entry for queried solar system {jita}");
        };
        assert!(!jita_dungeons.is_empty(), "Jita must have seeded Homefront dungeons");

        let (did_rep, list_rep) = &jita_dungeons[0];
        let did = did_rep.as_i64().expect("dungeonID") as u32;
        let PyRep::List(inst_list) = list_rep else {
            panic!("Expected List of instance IDs");
        };
        assert!(!inst_list.is_empty(), "Dungeon must have active instance IDs");
        let target_instance_id = inst_list[0].as_i64().expect("instanceID");

        // SDE Archetype 70 verification: verify dungeon ID is from SDE archetype 70
        let d_archetype = ctx.sde.dungeons.get(&did).and_then(|d| d.archetype_id);
        assert_eq!(d_archetype, Some(70), "Homefront dungeon must belong to SDE archetype 70");

        // 2. Complete the Homefront site via in-space combat destruction of the SDE-backed boss NPC
        let (boss_bid, boss_tid) = with_pve_special_reg(&ctx, |reg| {
            let s = reg.homefront_sites[&jita]
                .iter()
                .find(|s| s.instance_id == target_instance_id)
                .unwrap();
            (s.boss_npc_ball_id, s.boss_type_id)
        });
        assert!(ctx.sde.types.contains_key(&boss_tid), "Homefront boss must use a real SDE NPC type");

        let jita_sys = ctx.systems.get_or_spawn(jita);
        let boss_snap = jita_sys
            .get_ball_snapshot(boss_bid)
            .await
            .expect("get_ball_snapshot")
            .expect("Homefront boss ball must be spawned");
        assert_eq!(boss_snap.1.type_id, boss_tid);

        let boss_combat = jita_sys
            .get_combat_state(boss_bid)
            .await
            .expect("get_combat_state")
            .expect("Homefront boss ball must have combat state");
        assert!(
            boss_combat.max_shield_hp > 0.0
                && boss_combat.max_armor_hp > 0.0
                && boss_combat.max_structure_hp > 0.0,
            "Homefront boss ball must have SDE-backed CombatState HP"
        );

        let init_balance = ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance.0);
        let init_journal_len = ctx.db.with_snapshot(|s| s.wallet_journal.len());
        let _ = ctx.drain_notifications_for_char(90_000_002);

        let kill_evt = jita_sys
            .apply_damage_with_attacker(
                boss_bid,
                DamageVector {
                    em: 50_000.0,
                    thermal: 50_000.0,
                    kinetic: 50_000.0,
                    explosive: 50_000.0,
                },
                Some(90_000_002),
            )
            .await
            .expect("apply_damage_with_attacker")
            .expect("Homefront boss hit result");
        assert!(kill_evt.1, "Homefront boss must be destroyed by combat damage");

        let final_balance = ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance.0);
        assert!(final_balance > init_balance, "Pilot must receive ISK payout from in-space Homefront completion");

        let journal_after = ctx.db.with_snapshot(|s| s.wallet_journal.clone());
        assert!(
            journal_after.len() > init_journal_len
                && journal_after.iter().any(|j| {
                    j.character_id == 90_000_002
                        && j.entry_type == "CorporateRewardPayout"
                        && j.reason.contains("Homefront Operation Payout")
                }),
            "Homefront completion must record a CorporateRewardPayout wallet journal entry"
        );

        let notifs = ctx.drain_notifications_for_char(90_000_002);
        assert!(
            notifs.iter().any(|n| n.event == "OnAccountChange"),
            "Homefront payout must emit OnAccountChange notification"
        );

        // 3. Query GetHomefrontSiteInstances again: the completed instance is no longer returned in the active registry list
        let hf_after_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "dungeonInstanceCacheMgr",
                None,
                "GetHomefrontSiteInstances",
                &[PyRep::Integer(jita as i64)],
                &kw,
                None,
            )
            .await
            .expect("GetHomefrontSiteInstances after completion");
        let PyRep::Dict(hf_after_dict) = &hf_after_rep else {
            panic!("Expected Dict");
        };
        let Some((_, PyRep::Dict(jita_after))) = hf_after_dict.iter().find(|(k, _)| k.as_i64() == Some(jita as i64)) else {
            panic!("Missing Jita in after dict");
        };
        let active_remaining: Vec<i64> = jita_after
            .iter()
            .flat_map(|(_, l)| match l {
                PyRep::List(ids) => ids.iter().filter_map(|i| i.as_i64()).collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        assert!(
            !active_remaining.contains(&target_instance_id),
            "Completed instance {target_instance_id} must not appear in active registry instances"
        );

        // 4. Lowsec/nullsec query returns empty (Homefront is highsec only)
        let nullsec_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "dungeonInstanceCacheMgr",
                None,
                "GetHomefrontSiteInstances",
                &[PyRep::Integer(30_002_904)],
                &kw,
                None,
            )
            .await
            .expect("GetHomefrontSiteInstances for nullsec");
        let PyRep::Dict(ns_dict) = &nullsec_rep else { panic!("Expected Dict") };
        let (_, PyRep::Dict(ns_dungeons)) = &ns_dict[0] else { panic!("Expected Dict") };
        assert!(ns_dungeons.is_empty(), "Nullsec system must not have Homefront operations");
    }
}
