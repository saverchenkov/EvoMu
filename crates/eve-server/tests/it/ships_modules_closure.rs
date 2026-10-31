//! Integration tests closing `ships.*` and `modules.*` parity rows:
//! - `feat:ships.frigate-escape-bay`
//! - `feat:ships.blockade-runner-cargo-scan-immunity`
//! - `feat:ships.board-ship-skill-and-clone-check`
//! - `feat:ships.cynosural-beacon-spawning`
//! - `feat:ships.jump-drive-fatigue-accumulation`
//! - `feat:ships.titan-jump-portal-bridge`
//! - `feat:modules.stasis-grappler-mechanics`
//! - `feat:modules.emergency-hull-energizer`
//! - `feat:modules.mutadaptive-remote-armor-repairer`
//! - `feat:modules.entropic-disintegrator-damage-spool`

use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode, SlimItemMeta, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

async fn call_service(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
    kw: HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(ctx, sess, svc, method, &args, &kw)
        .await
}

fn make_session(
    sid: u64,
    user_id: u32,
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    sys_id: u32,
    ship_id: Option<i64>,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = station_id;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = ship_id;
    sess
}

fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton,
            version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
}

async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, type_id: u32, owner_id: i64, x: f64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.armor_hp = cs.max_armor_hp * 0.2;
    let b = Ball {
        id: ship_id, type_id, position: DVec3::new(x, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 150.0, inertia: 1.0, mass: 10_000_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        group_id: 26, category_id: 6, owner_id, char_id: Some(owner_id),
        corp_id: 1000009, alliance_id: -1, security_status: 0.0, is_cloaked: false,
    };
    let _ = sys_h.spawn_ball_with_meta(b, cs, meta).await;
}

fn grant_type_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id).cloned() else { return; };
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for &(sa, la) in &[(182_u16, 277_u16), (183, 278), (184, 279), (1285, 1286), (1289, 1287), (1290, 1288)] {
            if let Some(&sid_f) = t.default_attributes.get(&sa) {
                let sid = sid_f.round() as u32;
                let lvl = t.default_attributes.get(&la).copied().unwrap_or(1.0).round() as u8;
                if sid > 0 {
                    sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl.max(5), skill_points: 256_000.0 });
                }
            }
        }
    });
}

#[tokio::test]
async fn test_ships_frigate_escape_bay_blockade_runner_scan_and_board_skill_clone_checks() {
    // parity: feat:ships.board-ship-skill-and-clone-check
    eve_testkit::attest(&[
        "feat:ships.board-ship-skill-and-clone-check",
    ]);

    let ctx = new_ctx();
    ctx.set_clock_paused(true);
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    grant_type_skills(&ctx, c1, 638);
    grant_type_skills(&ctx, c1, 587);
    ctx.db.with_mut_snapshot(|s| if let Some(ch) = s.characters.get_mut(&c1) { ch.clone_grade = 2; });

    // 1. Frigate Escape Bay (`flag 179` on Raven `638`, `frigateEscapeBayCapacity` attr 3020 > 0)
    let raven_id = 940_001;
    let packaged_rifter = 940_002;
    let rifter_with_cargo = 940_003;
    let cargo_trit = 940_004;
    let valid_rifter = 940_005;
    let second_rifter = 940_006;
    let cruiser_feb_id = 940_007;

    seed_item(&ctx, raven_id, 638, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, packaged_rifter, 587, c1, station_id, InventoryFlag::Hangar, 1, false);
    seed_item(&ctx, rifter_with_cargo, 587, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, cargo_trit, 34, c1, rifter_with_cargo, InventoryFlag::Cargo, 100, false);
    seed_item(&ctx, valid_rifter, 587, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, second_rifter, 587, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, cruiser_feb_id, 626, c1, station_id, InventoryFlag::Hangar, 1, true);

    let mut sess1 = make_session(1, 1, c1, 1000009, Some(station_id), sys_id, Some(raven_id));

    let cruiser_feb_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(cruiser_feb_id), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Cruiser into FEB must fail");
    assert_eq!(cruiser_feb_err.code, "InvalidItemTypeForHold");

    let pkg_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(packaged_rifter), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Packaged frigate must be rejected");
    assert_eq!(pkg_err.code, "ShipMustBeAssembled");

    let cargo_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(rifter_with_cargo), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Frigate with non-charge cargo must be rejected");
    assert_eq!(cargo_err.code, "ShipHasItemsInCargo");

    let _ = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(valid_rifter), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect("Assembled empty frigate must fit in Frigate Escape Bay");

    let second_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(second_rifter), PyRep::Integer(raven_id), PyRep::Integer(179)], HashMap::new()).await.expect_err("Frigate Escape Bay must reject a second frigate");
    assert_eq!(second_err.code, "CapacityExceeded");

    // Undock Raven into space and destroy it via execute_death_pipeline -> pilot boards `valid_rifter`!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.station_id = None;
            ch.active_ship_id = raven_id;
        }
        if let Some(sh) = snap.items.get_mut(&raven_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
    });
    spawn_ship_ball(&ctx, sys_id, raven_id, 638, c1, 1_000.0).await;
    let outcome = execute_death_pipeline(&ctx, sys_id, c1, raven_id, c2, DVec3::new(1_000.0, 0.0, 0.0), ctx.now_unix_secs()).await.expect("execute_death_pipeline succeeds");
    assert_eq!(outcome.ejected_capsule_id, Some(valid_rifter));
    let active_after_death = ctx.db.with_snapshot(|s| s.characters.get(&c1).map(|c| c.active_ship_id).unwrap_or(0));
    assert_eq!(active_after_death, valid_rifter, "Pilot must be placed directly into escape frigate");
    assert!(!outcome.loot_dropped_item_ids.contains(&valid_rifter), "Frigate must not drop as loot");
    assert!(!outcome.loot_destroyed_item_ids.contains(&valid_rifter), "Frigate must not be destroyed in wreck");
    let dkm = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.detailed_killmails.get(&outcome.killmail_id).cloned()).expect("Detailed killmail must exist");
    assert!(dkm.dropped_items.iter().any(|it| it.type_id == 587 && it.flag == 179 && it.qty_dropped == 0 && it.qty_destroyed == 0), "Killmail must record surviving frigate at flag 179");
    ctx.advance_virtual_clock(15);

    // 2. Blockade Runner (`Crane` 12729, group 1202) cargo scan immunity vs NPC customs contraband scan
    let crane_id = 940_020;
    let contraband_id = 940_021; // Slaves (type 3721)
    let scanner_mod = 940_022; // Cargo Scanner I (type 442)
    seed_item(&ctx, crane_id, 12729, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, contraband_id, 3721, c2, crane_id, InventoryFlag::Cargo, 5, false);
    seed_item(&ctx, scanner_mod, 442, c1, valid_rifter, InventoryFlag::MedSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(scanner_mod, true);
        if let Some(ch) = snap.characters.get_mut(&c2) {
            ch.balance = IskCents::from_isk(10_000_000);
            ch.active_ship_id = crane_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
    });
    spawn_ship_ball(&ctx, sys_id, crane_id, 12729, c2, 2_000.0).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.add_target_lock(valid_rifter, TargetLock { target_id: crane_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(25_000.0) }).await;

    sess1.station_id = None;
    sess1.ship_id = Some(valid_rifter);
    let scan_rep = call_service(&ctx, &mut sess1, "dogmaIM", "Activate", vec![PyRep::Integer(scanner_mod), PyRep::String("cargoScan".into()), PyRep::Integer(crane_id)], HashMap::new()).await.expect("Cargo Scanner activation on Blockade Runner");
    assert_eq!(scan_rep.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(scan_rep.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

    let spawned_police = eve_server::services::combat_npc_police::check_and_spawn_faction_police(&ctx, sys_id).await;
    assert!(!spawned_police.is_empty(), "High-sec customs police must spawn when contraband is detected");
    let (cargo_has_contraband, c2_bal, has_standing_loss) = ctx.db.with_snapshot(|s| {
        let in_cargo = s.items.get(&contraband_id).map(|i| i.location_id == crane_id).unwrap_or(false);
        let bal = s.characters.get(&c2).map(|c| c.balance).unwrap_or(IskCents(0));
        let standing = s.standing_transactions.values().flatten().any(|st| st.to_id == c2 && st.modification < 0.0);
        (in_cargo, bal, standing)
    });
    assert!(!cargo_has_contraband, "Contraband must be confiscated from Blockade Runner cargo");
    assert!(c2_bal < IskCents::from_isk(10_000_000), "Customs fine must be deducted from pilot wallet");
    assert!(has_standing_loss, "Standing penalty transaction must be recorded for contraband");

    // R-CUSTOMS-SCOPE negative tests:
    // Wrecks and jetcans with contraband are NOT confiscated:
    let (wreck_id, wreck_cb, jetcan_id, jetcan_cb) = (940_080, 940_081, 940_082, 940_083);
    seed_item(&ctx, wreck_id, 26561, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, wreck_cb, 3721, c2, wreck_id, InventoryFlag::Cargo, 5, false);
    seed_item(&ctx, jetcan_id, 23, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, jetcan_cb, 3721, c2, jetcan_id, InventoryFlag::Cargo, 5, false);
    spawn_ship_ball(&ctx, sys_id, wreck_id, 26561, c2, 3_000.0).await;
    spawn_ship_ball(&ctx, sys_id, jetcan_id, 23, c2, 4_000.0).await;
    assert!(eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, sys_id, wreck_id).is_empty());
    assert!(eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, sys_id, jetcan_id).is_empty());
    assert!(ctx.db.get_item(wreck_cb).is_some() && ctx.db.get_item(jetcan_cb).is_some());

    // System below confiscateMinSec: Tama (0.28) is below SDE confiscateMinSec -> contraband NOT confiscated:
    let (tama_sys, tama_ship, tama_cb) = (30_002_813, 940_084, 940_085);
    seed_item(&ctx, tama_ship, 587, c2, tama_sys as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, tama_cb, 3721, c2, tama_ship, InventoryFlag::Cargo, 5, false);
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&c2) { ch.solar_system_id = tama_sys; ch.active_ship_id = tama_ship; } });
    spawn_ship_ball(&ctx, tama_sys, tama_ship, 587, c2, 1_000.0).await;
    assert!(eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, tama_sys, tama_ship).is_empty());
    assert!(ctx.db.get_item(tama_cb).is_some(), "Contraband below confiscateMinSec must not be confiscated");
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&c2) { ch.solar_system_id = sys_id; ch.active_ship_id = crane_id; } });

    // 3. Board ship skill and Alpha clone grade checks (`ship.Board` and `ship.BoardStoredShip`)
    sess1.station_id = Some(station_id);
    let bustard_id = 940_030;
    seed_item(&ctx, bustard_id, 12731, c1, station_id, InventoryFlag::Hangar, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.station_id = Some(station_id);
        }
        snap.skills.entry(c1).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
    let board_no_skill = call_service(&ctx, &mut sess1, "ship", "Board", vec![PyRep::Integer(bustard_id)], HashMap::new()).await.expect_err("Boarding Bustard without Transport Ships skill must fail");
    assert_eq!(board_no_skill.code, "ShipHasSkillPrerequisites");

    grant_type_skills(&ctx, c1, 12731);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.clone_grade = 0; } });
    let board_alpha_err = call_service(&ctx, &mut sess1, "ship", "Board", vec![PyRep::Integer(bustard_id)], HashMap::new()).await.expect_err("Alpha clone must be blocked from boarding Omega-only Bustard");
    assert_eq!(board_alpha_err.code, "TrialAccountRestriction");

    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.clone_grade = 2; } });
    let board_ok = call_service(&ctx, &mut sess1, "ship", "Board", vec![PyRep::Integer(bustard_id)], HashMap::new()).await.expect("Omega clone with skills must board Bustard");
    assert_eq!(board_ok.dict_get("shipID").and_then(|v| v.as_i64()), Some(bustard_id));

    // In space boarding (`ship.Board`):
    let unpiloted_bustard = 940_040;
    let c2_ship = 940_050;
    seed_item(&ctx, unpiloted_bustard, 12731, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, c2_ship, 587, c2, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ship_ball(&ctx, sys_id, unpiloted_bustard, 12731, c2, 11_000.0).await;
    spawn_ship_ball(&ctx, sys_id, c2_ship, 587, c2, 11_500.0).await;
    let mut sess_space = make_session(2, 2, c2, 1000009, None, sys_id, Some(c2_ship));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c2) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = c2_ship;
            ch.station_id = None;
            ch.clone_grade = 0;
        }
        snap.skills.entry(c2).or_default().clear();
    });
    let space_no_skill = call_service(&ctx, &mut sess_space, "ship", "Board", vec![PyRep::Integer(unpiloted_bustard)], HashMap::new()).await.expect_err("Missing skills in space");
    assert_eq!(space_no_skill.code, "ShipHasSkillPrerequisites");

    grant_type_skills(&ctx, c2, 12731);
    let space_alpha_err = call_service(&ctx, &mut sess_space, "ship", "Board", vec![PyRep::Integer(unpiloted_bustard)], HashMap::new()).await.expect_err("Alpha in space");
    assert_eq!(space_alpha_err.code, "TrialAccountRestriction");

    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c2) { ch.clone_grade = 2; } });
    let space_board_ok = call_service(&ctx, &mut sess_space, "ship", "Board", vec![PyRep::Integer(unpiloted_bustard)], HashMap::new()).await.expect("Omega in space boards");
    assert_eq!(space_board_ok.dict_get("shipID").and_then(|v| v.as_i64()), Some(unpiloted_bustard));
}

#[tokio::test]
async fn test_ships_cyno_beacon_jump_fatigue_and_titan_portal_bridge() {

    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    let cyno_char = 90_000_001;
    let titan_char = 90_000_002;
    let subcap_char = 90_000_003;
    let low_origin = 30_002_538;
    let low_dest = 30_002_539;

    let falcon_id = 950_001;
    let cyno_mod = 950_002;
    let lo_id = 950_003;
    let cloak_mod = 950_004;
    let avatar_id = 950_010;
    let portal_mod = 950_011;
    let stront_id = 950_012;
    let helium_id = 950_013;
    let rifter_id = 950_020;

    seed_item(&ctx, falcon_id, 11957, cyno_char, low_dest as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, cyno_mod, 21096, cyno_char, falcon_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, cloak_mod, 11578, cyno_char, falcon_id, InventoryFlag::HiSlot1, 1, true);
    seed_item(&ctx, lo_id, 16273, cyno_char, falcon_id, InventoryFlag::Cargo, 2_000, false);

    seed_item(&ctx, avatar_id, 11567, titan_char, low_origin as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, portal_mod, 23953, titan_char, avatar_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, stront_id, 16275, titan_char, avatar_id, InventoryFlag::Cargo, 5_000, false);
    seed_item(&ctx, helium_id, 16274, titan_char, avatar_id, InventoryFlag::Cargo, 500_000, false);
    seed_item(&ctx, rifter_id, 587, subcap_char, low_origin as i64, InventoryFlag::None, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cyno_mod, true);
        snap.module_online_states.insert(cloak_mod, true);
        snap.module_online_states.insert(portal_mod, true);
        snap.skills.entry(cyno_char).or_default().insert(21611, TrainedSkillRecord { type_id: 21611, level: 5, skill_points: 256_000.0 });
        snap.skills.entry(titan_char).or_default().insert(3456, TrainedSkillRecord { type_id: 3456, level: 5, skill_points: 256_000.0 });
        let fleet_id = 77_001;
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: titan_char,
                members: vec![cyno_char, titan_char, subcap_char],
                ..Default::default()
            },
        );
        if let Some(ch_base) = snap.characters.get(&cyno_char).cloned() {
            let mut sub_ch = ch_base;
            sub_ch.character_id = subcap_char;
            sub_ch.user_id = 3;
            sub_ch.active_ship_id = rifter_id;
            sub_ch.solar_system_id = low_origin;
            snap.characters.insert(subcap_char, sub_ch);
        }
        for (cid, sid, sys) in [(cyno_char, falcon_id, low_dest), (titan_char, avatar_id, low_origin), (subcap_char, rifter_id, low_origin)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys;
                ch.active_ship_id = sid;
                ch.station_id = None;
            }
        }
        snap.accounts.insert(
            "sub".to_string(),
            eve_db::AccountRecord {
                user_id: 3,
                username: "sub".to_string(),
                password_token: "dummy".to_string(),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    spawn_ship_ball(&ctx, low_dest, falcon_id, 11957, cyno_char, 0.0).await;
    spawn_ship_ball(&ctx, low_origin, avatar_id, 11567, titan_char, 0.0).await;
    spawn_ship_ball(&ctx, low_origin, rifter_id, 587, subcap_char, 1_000.0).await;

    let mut s_cyno = make_session(1, 1, cyno_char, 1000009, None, low_dest, Some(falcon_id));
    let mut s_titan = make_session(2, 2, titan_char, 1000009, None, low_origin, Some(avatar_id));
    let mut s_sub = make_session(3, 3, subcap_char, 1000009, None, low_origin, Some(rifter_id));

    let now_ts = ctx.now_unix();
    let _ = ctx.db.acquire_online_lock(1, s_cyno.session_id, Some(cyno_char), true, now_ts);
    let _ = ctx.db.acquire_online_lock(2, s_titan.session_id, Some(titan_char), true, now_ts);
    let _ = ctx.db.acquire_online_lock(3, s_sub.session_id, Some(subcap_char), true, now_ts);

    let cyno_rep = call_service(&ctx, &mut s_cyno, "dogmaIM", "Activate", vec![PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())], HashMap::new()).await.expect("Falcon lights cynosural field");
    let beacon_id = cyno_rep.dict_get("beaconID").and_then(|v| v.as_i64()).expect("beaconID");
    assert!(beacon_id > 0);

    let lo_after = ctx.db.with_snapshot(|s| s.items.get(&lo_id).map(|i| i.quantity).unwrap_or(0));
    assert!(lo_after < 2_000, "Liquid Ozone must be consumed scaled by Cynosural Field Theory");
    let b_ball = ctx.systems.get_or_spawn(low_dest).get_ball_snapshot(beacon_id).await.ok().flatten();
    assert!(b_ball.is_some(), "Beacon ball must be spawned in space");
    let f_ball = ctx.systems.get_or_spawn(low_dest).get_ball_snapshot(falcon_id).await.ok().flatten().unwrap().1;
    assert_eq!(f_ball.max_velocity, 0.0, "Cyno ship must be immobilized (0 m/s)");

    let dock_err = call_service(&ctx, &mut s_cyno, "stationSvc", "Dock", vec![PyRep::Integer(60003760)], HashMap::new()).await.expect_err("Dock blocked");
    assert_eq!(dock_err.code, "CynosuralFieldActive");
    let b_cyno = ctx.bind_object(&s_cyno, "beyonce", low_dest as i64).expect("bind beyonce");
    let jump_err = ctx.dispatch_rpc(&mut s_cyno, "beyonce", Some(b_cyno), "CmdStargateJump", &[PyRep::Integer(50000001)], &HashMap::new(), None).await.expect_err("Gate jump blocked");
    assert_eq!(jump_err.code, "CynosuralFieldActive");
    let cloak_err = call_service(&ctx, &mut s_cyno, "dogmaIM", "Activate", vec![PyRep::Integer(cloak_mod), PyRep::String("cloaking".into())], HashMap::new()).await.expect_err("Cloak blocked");
    assert_eq!(cloak_err.code, "CynosuralFieldActive");

    let beacon_notifs = ctx.drain_notifications_for_char(titan_char);
    let beacon_notif = beacon_notifs.iter().find(|n| n.event == "OnFleetJumpBeaconModuleChange").expect("Beacon notification sent to fleet members");
    assert_eq!(beacon_notif.args.get(2).and_then(|v| v.as_i64()), Some(beacon_id), "Beacon must appear in fleet jump target list");

    let bridge_rep = call_service(&ctx, &mut s_titan, "beyonce", "CmdBridgeToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(low_dest as i64), PyRep::Integer(beacon_id)], HashMap::new()).await.expect("Titan opens jump portal bridge");
    assert_eq!(bridge_rep.dict_get("beaconID").and_then(|v| v.as_i64()), Some(beacon_id));

    let _ = ctx.systems.get_or_spawn(low_origin).set_ball_position(rifter_id, DVec3::new(3_500.0, 0.0, 0.0)).await;
    let dist_err = call_service(&ctx, &mut s_sub, "beyonce", "CmdJumpThroughFleet", vec![PyRep::Integer(titan_char), PyRep::Integer(avatar_id)], HashMap::new()).await.expect_err("Passenger > 2500m must be refused");
    assert_eq!(dist_err.code, "OutOfPortalRange");
    let _ = ctx.systems.get_or_spawn(low_origin).set_ball_position(rifter_id, DVec3::new(1_000.0, 0.0, 0.0)).await;

    let titan_h_before = ctx.db.with_snapshot(|s| s.items.get(&helium_id).map(|i| i.quantity).unwrap_or(0));
    let jump_through = call_service(&ctx, &mut s_sub, "beyonce", "CmdJumpThroughFleet", vec![PyRep::Integer(titan_char), PyRep::Integer(avatar_id)], HashMap::new()).await.expect("Passenger jumps through portal");
    assert_eq!(jump_through.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(low_dest as i64));
    let titan_h_after = ctx.db.with_snapshot(|s| s.items.get(&helium_id).map(|i| i.quantity).unwrap_or(0));
    assert!(titan_h_after < titan_h_before, "Titan isotopes consumed scaled by passenger mass and distance");
    let fatigue_min = jump_through.dict_get("jumpFatigueMinutes").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(fatigue_min > 0.0, "Passenger must accumulate jump fatigue");

    let far_err = call_service(&ctx, &mut s_titan, "beyonce", "CmdJumpToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(30000142), PyRep::Integer(beacon_id)], HashMap::new()).await.expect_err("Jump distance out of range");
    assert_eq!(far_err.code, "JumpDistanceOutOfRange");

    let titan_jump = call_service(&ctx, &mut s_titan, "beyonce", "CmdJumpToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(low_dest as i64), PyRep::Integer(beacon_id)], HashMap::new()).await.expect("Titan jumps to fleet cyno beacon");
    assert_eq!(titan_jump.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(low_dest as i64));
    let titan_h_final = ctx.db.with_snapshot(|s| s.items.get(&helium_id).map(|i| i.quantity).unwrap_or(0));
    assert!(titan_h_final < titan_h_after, "Titan isotopes consumed scaled by Jump Fuel Conservation");
    let cd_err = call_service(&ctx, &mut s_titan, "beyonce", "CmdJumpToFleetModuleBeacon", vec![PyRep::Integer(cyno_char), PyRep::Integer(low_dest as i64), PyRep::Integer(beacon_id)], HashMap::new()).await.expect_err("Immediate jump cooldown");
    assert_eq!(cd_err.code, "JumpReactivationCooldown");
}

#[tokio::test]
async fn test_modules_grappler_ehe_mutadaptive_repairer_and_entropic_disintegrator() {
    // parity: feat:modules.entropic-disintegrator-damage-spool
    eve_testkit::attest(&[
        "feat:modules.entropic-disintegrator-damage-spool",
    ]);

    let ctx = new_ctx();
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let station_id = 60_003_760;
    let sys_id = 30_004_759;
    grant_type_skills(&ctx, c1, 626);
    grant_type_skills(&ctx, c1, 638);
    grant_type_skills(&ctx, c1, 24483);
    grant_type_skills(&ctx, c1, 47272);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.clone_grade = 2; } });

    // 1. Heavy Stasis Grappler (group 1672): rejected on Cruiser (`626`), accepted on Battleship (`638`), max 1 per ship
    let grappler_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1672 && t.category_id == 7)
        .map(|t| t.type_id)
        .expect("Heavy Stasis Grappler in SDE");
    let ehe_tid = 40714_u32;
    let vexor_id = 960_001;
    let raven_id = 960_002;
    let grappler_1 = 960_003;
    let grappler_2 = 960_004;
    seed_item(&ctx, vexor_id, 626, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, raven_id, 638, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, grappler_1, grappler_tid, c1, station_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, grappler_2, grappler_tid, c1, station_id, InventoryFlag::Hangar, 1, true);
    grant_type_skills(&ctx, c1, grappler_tid);

    let mut sess = make_session(1, 1, c1, 1000009, Some(station_id), sys_id, Some(vexor_id));

    let cruiser_err = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(grappler_1), PyRep::Integer(vexor_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)], HashMap::new()).await.expect_err("Cruiser must not fit Heavy Stasis Grappler");
    assert_eq!(cruiser_err.code, "CanNotFitModuleToShip");

    sess.ship_id = Some(raven_id);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&c1) { ch.active_ship_id = raven_id; } });
    let _ = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(grappler_1), PyRep::Integer(raven_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)], HashMap::new()).await.expect("Battleship must fit 1 Heavy Stasis Grappler");
    let max_group_err = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(grappler_2), PyRep::Integer(raven_id), PyRep::Integer(InventoryFlag::MedSlot1 as i64)], HashMap::new()).await.expect_err("Max 1 Heavy Stasis Grappler per ship");
    assert_eq!(max_group_err.code, "MaxGroupFitted");

    // Undock Raven to activate Stasis Grappler on target at 1km and falloff
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(grappler_1, true);
        if let Some(ch) = snap.characters.get_mut(&c1) { ch.station_id = None; ch.solar_system_id = sys_id; ch.active_ship_id = raven_id; }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(raven_id);
    spawn_ship_ball(&ctx, sys_id, raven_id, 638, c1, 0.0).await;
    let grappler_target = 960_005;
    seed_item(&ctx, grappler_target, 626, c2, sys_id as i64, InventoryFlag::None, 1, true);
    spawn_ship_ball(&ctx, sys_id, grappler_target, 626, c2, 1_000.0).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.add_target_lock(raven_id, TargetLock { target_id: grappler_target, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let base_t_vel = sys_h.get_ball_snapshot(grappler_target).await.unwrap().unwrap().1.max_velocity;

    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(grappler_1), PyRep::String("stasisGrappler".into()), PyRep::Integer(grappler_target)], HashMap::new()).await.expect("Grappler activates at 1km");
    let grap_1km_vel = sys_h.get_ball_snapshot(grappler_target).await.unwrap().unwrap().1.max_velocity;
    assert!(grap_1km_vel < base_t_vel * 0.20, "Point blank 1km grappler must reduce speed by 85%");
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(grappler_1)], HashMap::new()).await.expect("Grappler deactivates");

    let _ = sys_h.set_ball_position(grappler_target, DVec3::new(8_000.0, 0.0, 0.0)).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(grappler_1), PyRep::String("stasisGrappler".into()), PyRep::Integer(grappler_target)], HashMap::new()).await.expect("Grappler activates at 8km falloff");
    let grap_8km_vel = sys_h.get_ball_snapshot(grappler_target).await.unwrap().unwrap().1.max_velocity;
    assert!(grap_8km_vel > grap_1km_vel && grap_8km_vel < base_t_vel, "Grappler speed reduction must decay over falloff curve");
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(grappler_1)], HashMap::new()).await.expect("Grappler deactivates");

    // 2. Capital Emergency Hull Energizer (`ehe_tid`): 95% hull resist (0.05 resonance) for 1 cycle, burns out, refuses nanite repair
    let mutadaptive_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 2018 && t.category_id == 7)
        .map(|t| t.type_id)
        .expect("Mutadaptive Remote Armor Repairer in SDE");
    let archon_id = 960_010;
    let ehe_id = 960_011;
    let mutadaptive_id = 960_012;
    let target_ship_a = 960_020;
    let target_ship_b = 960_021;
    seed_item(&ctx, archon_id, 24483, c1, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, ehe_id, ehe_tid, c1, archon_id, InventoryFlag::LoSlot0, 1, true);
    seed_item(&ctx, mutadaptive_id, mutadaptive_tid, c1, archon_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, target_ship_a, 638, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, target_ship_b, 638, c2, sys_id as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(ehe_id, true);
        snap.module_online_states.insert(mutadaptive_id, true);
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = archon_id;
        }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(archon_id);

    spawn_ship_ball(&ctx, sys_id, archon_id, 24483, c1, 0.0).await;
    spawn_ship_ball(&ctx, sys_id, target_ship_a, 638, c2, 2_000.0).await;
    spawn_ship_ball(&ctx, sys_id, target_ship_b, 638, c2, 3_000.0).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;

    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(ehe_id), PyRep::String("emergencyHullEnergizer".into())], HashMap::new()).await.expect("EHE must activate on capital hull");
    let active_cs = sys_h.get_combat_state(archon_id).await.unwrap().unwrap();
    assert_eq!(active_cs.structure_resonance, [0.05, 0.05, 0.05, 0.05], "EHE must grant 95% hull resistance across EM/Therm/Kin/Exp");
    let snap_res = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&archon_id).map(|t| (t.get(&109).copied(), t.get(&110).copied())).unwrap());
    assert_eq!(snap_res, (Some(0.05), Some(0.05)));

    let _ = sys_h.step_manual(25.0).await;
    let burned_dmg = ctx.db.with_snapshot(|s| s.items.get(&ehe_id).and_then(|i| i.dynamic_attributes.get(&3).copied()).unwrap_or(0.0));
    assert!(burned_dmg >= 39.9, "EHE must burn out after 1 cycle");
    let nanite_err = call_service(&ctx, &mut sess, "dogmaIM", "RepairModule", vec![PyRep::Integer(archon_id), PyRep::Integer(ehe_id)], HashMap::new()).await.expect_err("Burned-out EHE must refuse Nanite Repair Paste");
    assert_eq!(nanite_err.code, "ModuleBurnedOut");

    // 3. Mutadaptive Remote Armor Repairer (`mutadaptive_id`) spools up per cycle on `target_ship_a` and resets on `target_ship_b`
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mutadaptive_id), PyRep::String("remoteArmorRepair".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Mutadaptive RAR activates on target A");
    let _ = sys_h.step_manual(12.0).await;
    let rep_after_1 = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(0.0));
    let _ = sys_h.step_manual(12.0).await;
    let rep_after_2 = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(0.0));
    assert!(rep_after_2 > rep_after_1, "Mutadaptive RAR must spool up repair amount across consecutive cycles");

    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_b, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mutadaptive_id), PyRep::String("remoteArmorRepair".into()), PyRep::Integer(target_ship_b)], HashMap::new()).await.expect("Mutadaptive RAR retargets to target B");
    let _ = sys_h.step_manual(12.0).await;
    let rep_after_switch = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(0.0));
    assert!((rep_after_switch - rep_after_1).abs() < 1e-4, "Switching target on Mutadaptive RAR must reset spool to cycle 0");
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(mutadaptive_id)], HashMap::new()).await.expect("Deactivate Mutadaptive RAR");
    let rep_after_deact = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mutadaptive_id).and_then(|t| t.get(&84).copied()).unwrap_or(rep_after_1));
    assert!((rep_after_deact - rep_after_1).abs() < 1e-4, "Deactivating Mutadaptive RAR must reset transient attr 84 to base");
    let _ = sys_h.remove_target_lock(archon_id, target_ship_b).await;

    // 4. Entropic Disintegrator (`47272` Light Entropic Disintegrator I) spools up per cycle, reaches damageMultiplierBonusMax, resets on target change / deactivation / lock loss
    let dis_mod_id = 960_030;
    let dis_ammo_id = 960_031;
    seed_item(&ctx, dis_mod_id, 47272, c1, archon_id, InventoryFlag::HiSlot1, 1, true);
    seed_item(&ctx, dis_ammo_id, 47885, c1, archon_id, InventoryFlag::HiSlot1, 100, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(dis_mod_id, true);
    });

    // Give target ship structure-only HP pool to test continuous structure damage ramp
    let mut cs_a = ctx.build_type_combat_state(638);
    cs_a.shield_hp = 0.0;
    cs_a.max_shield_hp = 0.0;
    cs_a.armor_hp = 0.0;
    cs_a.max_armor_hp = 0.0;
    cs_a.structure_hp = 100_000.0;
    cs_a.max_structure_hp = 100_000.0;
    let _ = sys_h.set_combat_state(target_ship_a, cs_a.clone()).await;
    let _ = sys_h.set_combat_state(target_ship_b, cs_a).await;
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    ctx.set_invention_roll_override(Some(0.50));

    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Initial entropic activation on target A");
    let m0 = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
    assert_eq!(m0, 1.0);

    // Advance spool cycle 1 and record damage
    let hp0 = sys_h.get_combat_state(target_ship_a).await.unwrap().unwrap().structure_hp;
    sys_h.step_manual(4.5).await.unwrap();
    let hp1 = sys_h.get_combat_state(target_ship_a).await.unwrap().unwrap().structure_hp;
    let dmg_cycle1 = hp0 - hp1;
    let m1 = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
    assert!(m1 > 1.0, "Entropic Disintegrator must ramp damage multiplier per cycle");

    // Advance spool cycle 2 and verify continuous damage ramps: cycle 2 damage > cycle 1 damage
    sys_h.step_manual(4.5).await.unwrap();
    let hp2 = sys_h.get_combat_state(target_ship_a).await.unwrap().unwrap().structure_hp;
    let dmg_cycle2 = hp1 - hp2;
    assert!(dmg_cycle2 > dmg_cycle1, "Entropic disintegrator damage ramping must make cycle 2 damage ({dmg_cycle2}) > cycle 1 damage ({dmg_cycle1})");

    // Advance 31 more cycles up to damageMultiplierBonusMax (1.0 + attr 2734 == 3.125)
    for _ in 0..31 {
        sys_h.step_manual(4.5).await.unwrap();
    }
    let m_max = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
    assert!((m_max - 3.125).abs() < 1e-4, "Damage multiplier must reach damageMultiplierBonusMax (3.125)");

    // Target switch: lock target B and switch target: resets multiplier to 1.0
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_b, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_b)], HashMap::new()).await.expect("Target switch to target B");
    let m_switch = eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id);
    assert_eq!(m_switch, 1.0, "Switching target must reset Entropic Disintegrator spool to 1.0");

    // Deactivation via dogmaIM.Deactivate: clears spool
    let deact_rep = call_service(&ctx, &mut sess, "dogmaIM", "Deactivate", vec![PyRep::Integer(dis_mod_id)], HashMap::new()).await.expect("dogmaIM.Deactivate must succeed");
    assert_eq!(deact_rep.dict_get("spoolMultiplier").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0);
    // Remove target B lock so only target A is locked
    let _ = sys_h.remove_target_lock(archon_id, target_ship_b).await;
    // Reactivate on target A and remove target lock via dogmaIM.RemoveTarget: module must deactivate and spool stop immediately
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Reactivating on target A");
    sys_h.step_manual(4.5).await.unwrap();
    assert!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id) > 1.0);
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "RemoveTarget", vec![PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("RemoveTarget on sole locked target");
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "RemoveTarget must stop entropic spool");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "RemoveTarget must deactivate entropic disintegrator");

    // Reactivate on target A, then clear all locks in space (empty locks list) during step: module deactivates
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Reactivating on target A");
    let _ = sys_h.clear_target_locks(archon_id).await;
    sys_h.step_manual(4.5).await.unwrap();
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "Empty locks list must stop entropic spool on step");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "Empty locks list must deactivate module on step");

    // a) Target moves beyond maximum range: spool multiplier resets to 1.0 and module deactivates
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Activate target A");
    sys_h.step_manual(4.5).await.unwrap();
    assert!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id) > 1.0);
    let _ = sys_h.set_ball_position(target_ship_a, DVec3::new(100_000.0, 0.0, 0.0)).await;
    sys_h.step_manual(4.5).await.unwrap();
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "Out of range must reset spool to 1.0");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "Out of range must deactivate module");

    // b) Ship capacitor runs dry / insufficient capacitor: spool multiplier resets to 1.0 and module deactivates
    let _ = sys_h.set_ball_position(target_ship_a, DVec3::new(2_000.0, 0.0, 0.0)).await;
    let _ = sys_h.add_target_lock(archon_id, TargetLock { target_id: target_ship_a, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    let _ = call_service(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(dis_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_a)], HashMap::new()).await.expect("Activate in range");
    sys_h.step_manual(4.5).await.unwrap();
    assert!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id) > 1.0);
    if let Ok(Some(mut cs)) = sys_h.get_combat_state(archon_id).await {
        cs.capacitor = 0.0;
        cs.max_capacitor = 0.0;
        let _ = sys_h.set_combat_state(archon_id, cs).await;
    }
    sys_h.step_manual(4.5).await.unwrap();
    assert_eq!(eve_server::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, archon_id, dis_mod_id), 1.0, "Capacitor dry must reset spool to 1.0");
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&dis_mod_id)), "Capacitor dry must deactivate module");
}
