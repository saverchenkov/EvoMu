//! End-to-end integration tests closing Crimewatch parity rows:
//! - `feat:crimewatch.concord-response-scaling`
//! - `feat:crimewatch.criminal-wreck-auto-abandon`
//! - `feat:crimewatch.limited-engagement-duel`
//! - `feat:crimewatch.suspect-timer-looting-ess`
//! - `feat:crimewatch.safety-setting-action-block`

use eve_db::{AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx
}

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    bound: Option<u64>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, bound, method, args, &HashMap::new(), None).await
}

pub(crate) fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() });
    });
}

pub(crate) async fn spawn_pilot(
    ctx: &ServerContext, sid: u64, char_id: i64, corp_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, CharacterRecord {
            character_id: char_id, user_id: sid, name: format!("Pilot {char_id}"), corporation_id: corp_id,
            alliance_id: None, war_faction_id: None, station_id: None, structure_id: None, solar_system_id: sys_id,
            constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id,
            balance: IskCents::from_isk(500_000_000), plex_balance: 0, loyalty_points: HashMap::new(),
            standings: HashMap::new(), security_status: 2.0, clone_grade: 2, home_station_id: 60_003_760,
            race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1, neural: NeuralAttributes::default(),
            skill_points: 10_000.0 * 1_000.0, activity_status: None, free_skill_points: 0.0,
            last_clone_jump_unix: 0, jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
            description: String::new(), title: String::new(), gender: 1, create_date_unix: 1_700_000_000,
            fleet_id: None, autopilot_waypoints: Vec::new(), version: 1, system_epoch: 0,
        });
        snap.accounts.entry(format!("u_{sid}")).or_insert(AccountRecord {
            user_id: sid, username: format!("u_{sid}"), password_token: String::new(), role: 0, banned_until_unix: None, ban_reason: None,
        });
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [3429, 3436, 11579] {
            sk.insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
        }
    });
    seed_item(ctx, ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    let _ = ctx.db.acquire_online_lock(sid, 10_000 + sid, Some(char_id), true, ctx.now_unix());
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let grp = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(25);
    let _ = sys_h.spawn_ball_with_meta(
        Ball { id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 320.0, inertia: 2.0, mass: 1_200_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(ship_type_id),
        SlimItemMeta { group_id: grp, category_id: 6, owner_id: char_id, char_id: Some(char_id), corp_id, alliance_id: -1, security_status: 2.0, is_cloaked: false },
    ).await;
    let mut sess = SessionState::new(10_000 + sid);
    sess.user_id = Some(sid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess
}

pub(crate) fn fit_online(ctx: &ServerContext, char_id: i64, ship_id: i64, mod_id: i64, mod_tid: u32, flag: InventoryFlag, ammo: Option<(i64, u32, i32)>) {
    seed_item(ctx, mod_id, mod_tid, char_id, ship_id, flag, 1, true);
    if let Some((aid, atid, qty)) = ammo {
        seed_item(ctx, aid, atid, char_id, ship_id, flag, qty, false);
    }
    let req_skills: Vec<u32> = ctx.sde.types.get(&mod_tid).map(|t| {
        [182u16, 183, 184, 1285, 1289, 1290].into_iter()
            .filter_map(|a| t.default_attributes.get(&a).copied())
            .filter(|&v| v > 0.0).map(|v| v as u32).collect()
    }).unwrap_or_default();
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, true);
        let sk = snap.skills.entry(char_id).or_default();
        for tid in req_skills.into_iter().chain([3413, 3426]) {
            sk.insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
}

pub(crate) async fn lock_target(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, target_id: i64) {
    rpc(ctx, sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)]).await.expect("dogmaIM.AddTarget");
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..60 {
        let _ = sys_h.step_manual(1.0).await;
    }
}

fn find_wreck_of_owner(ctx: &ServerContext, owner_id: i64, sys_id: u32) -> i64 {
    ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| {
                i.owner_id == owner_id
                    && i.location_id == sys_id as i64
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186)
            })
            .map(|i| i.item_id)
            .expect("Expected real wreck spawned by death_pipeline")
    })
}

#[tokio::test]
async fn test_crimewatch_concord_response_scaling_and_invulnerable_enforcement() {
    // parity: feat:crimewatch.concord-response-scaling
    eve_testkit::attest(&["feat:crimewatch.concord-response-scaling"]);

    // 1. Live SDE high-sec solar systems across all 6 response tiers via real router `dogmaIM.Activate`
    let tiers: [(u32, u64); 6] = [
        (30_000_010, 19), // Chidah (~0.5)
        (30_000_009, 14), // Sooma (~0.6)
        (30_000_026, 11), // Ordize (~0.7)
        (30_000_002, 8),  // Lashesih (~0.8)
        (30_000_142, 6),  // Jita (~0.9)
        (30_100_032, 6),  // Manifest (1.0)
    ];
    for (idx, (sys_id, expected_delay_s)) in tiers.into_iter().enumerate() {
        let ctx = new_ctx();
        let atk_c = 91_000_100 + idx as i64;
        let vic_c = 91_000_200 + idx as i64;
        let atk_s = 81_000_100 + idx as i64;
        let vic_s = 81_000_200 + idx as i64;
        let gun_id = 81_000_300 + idx as i64;
        let ammo_id = 81_000_400 + idx as i64;
        let mut atk_sess = spawn_pilot(&ctx, 100 + idx as u64, atk_c, 1_000_009, atk_s, 587, sys_id, DVec3::ZERO).await;
        let _vic_sess = spawn_pilot(&ctx, 200 + idx as u64, vic_c, 1_000_010, vic_s, 587, sys_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
        fit_online(&ctx, atk_c, atk_s, gun_id, 486, InventoryFlag::HiSlot0, Some((ammo_id, 185, 50)));
        lock_target(&ctx, &mut atk_sess, sys_id, vic_s).await;
        rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
        let t0 = ctx.now_unix();
        rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(vic_s)]).await.expect("Unprovoked high-sec attack on Safety Red");
        let (_, arrival, _, _) = ctx.crimewatch.read().unwrap().states[&atk_c].pending_concord_arrival.expect("CONCORD schedule");
        assert_eq!(arrival, t0 + expected_delay_s, "Mismatch for system {sys_id} (expected {expected_delay_s}s)");
    }

    // 2. Low-sec (0.4, Onsooh 30_000_013) triggers Suspect Flag with zero CONCORD schedule
    {
        let ctx = new_ctx();
        let (atk_c, vic_c, atk_s, vic_s) = (91_000_501, 91_000_502, 81_000_501, 81_000_502);
        let mut atk_sess = spawn_pilot(&ctx, 301, atk_c, 1_000_009, atk_s, 587, 30_000_013, DVec3::ZERO).await;
        let _vic_sess = spawn_pilot(&ctx, 302, vic_c, 1_000_010, vic_s, 587, 30_000_013, DVec3::new(1_000.0, 0.0, 0.0)).await;
        fit_online(&ctx, atk_c, atk_s, 81_000_510, 486, InventoryFlag::HiSlot0, Some((81_000_511, 185, 50)));
        lock_target(&ctx, &mut atk_sess, 30_000_013, vic_s).await;
        rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
        rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(81_000_510), PyRep::Integer(vic_s)]).await.unwrap();
        let cw = ctx.crimewatch.read().unwrap();
        assert!(cw.states[&atk_c].pending_concord_arrival.is_none(), "Low-sec must never schedule CONCORD");
        assert!(cw.states[&atk_c].is_suspect(ctx.now_unix()));
    }

    // 3. Full CONCORD execution suite in Jita (0.9 -> 6s) driven by production `tick_background_cluster`
    let ctx = new_ctx();
    let jita = 30_000_142u32;
    let (atk_c, vic_c, atk_s, vic_s) = (91_000_901, 91_000_902, 81_000_901, 81_000_902);
    let mut atk_sess = spawn_pilot(&ctx, 401, atk_c, 1_000_009, atk_s, 621, jita, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let _vic_sess = spawn_pilot(&ctx, 402, vic_c, 1_000_010, vic_s, 587, jita, DVec3::new(6_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, atk_c, atk_s, 81_000_910, 486, InventoryFlag::HiSlot0, Some((81_000_911, 185, 50)));
    let sys_h = ctx.systems.get_or_spawn(jita);
    lock_target(&ctx, &mut atk_sess, jita, vic_s).await;
    let jita_b = ctx.bind_object(&atk_sess, "beyonce", jita as i64).unwrap();
    rpc(&ctx, &mut atk_sess, "beyonce", Some(jita_b), "CmdOrbit", &[PyRep::Integer(vic_s), PyRep::Real(1_000.0)]).await.unwrap();

    rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(81_000_910), PyRep::Integer(vic_s)]).await.unwrap();

    // At t0 + 3s (< 6s): CONCORD has not arrived yet; at t0 + 6s: `tick_background_cluster` executes CONCORD
    ctx.advance_virtual_clock(3);
    tick_background_cluster(&ctx).await;
    assert!(ctx.crimewatch.concord_reports().is_empty());
    assert!(sys_h.get_ball_snapshot(atk_s).await.unwrap().is_some(), "Criminal hull intact before 6s arrival");

    ctx.advance_virtual_clock(3);
    tick_background_cluster(&ctx).await;
    let reports = ctx.crimewatch.concord_reports();
    assert_eq!(reports.len(), 1);
    let rep = &reports[0];
    assert_eq!(rep.criminal_char_id, atk_c);
    assert_eq!(rep.webbed_speed_fraction, 0.0, "CONCORD must apply 100% stasis web");
    assert!(rep.warp_scramble_strength >= 100, "CONCORD must warp scramble criminal hull");
    assert_eq!(rep.capacitor_after_neut, 0.0, "CONCORD must completely drain capacitor");
    assert!(rep.ecm_jammed, "CONCORD must apply ECM jam");
    assert!(rep.locks_cleared >= 1, "CONCORD ECM jam must clear criminal target locks");

    // Verify CONCORD battleship on grid is completely invulnerable to incoming damage
    let concord_cs_before = sys_h.get_combat_state(rep.concord_ball_id).await.unwrap().expect("CONCORD ship on grid");
    assert_eq!(concord_cs_before.shield_resonance, [0.0, 0.0, 0.0, 0.0]);
    let (applied, destroyed, _) = sys_h
        .apply_damage(rep.concord_ball_id, DamageVector::new(250_000.0, 250_000.0, 250_000.0, 250_000.0))
        .await
        .unwrap()
        .expect("CONCORD ball present");
    assert_eq!(applied, 0.0, "CONCORD hulls must take 0.0 damage");
    assert!(!destroyed);

    // Criminal ship was destroyed and pilot is now in an intact Capsule (670)
    assert!(sys_h.get_ball_snapshot(atk_s).await.unwrap().is_none(), "Criminal hull must be destroyed");
    let pod_id = ctx.db.get_character(atk_c).unwrap().active_ship_id;
    assert_eq!(ctx.db.get_item(pod_id).unwrap().type_id, 670, "Criminal pilot must be ejected into an intact capsule");
}

#[tokio::test]
async fn test_crimewatch_criminal_wreck_and_jetcan_auto_abandon_all_sec_bands() {
    // parity: feat:crimewatch.criminal-wreck-auto-abandon
    eve_testkit::attest(&["feat:crimewatch.criminal-wreck-auto-abandon"]);

    let ctx = new_ctx();

    // 1. High-sec (`Jita` 30_000_142): pilot commits real criminal aggression (`dogmaIM.Activate`), jettisons a jetcan (`ship.Jettison`),
    //    and is destroyed by CONCORD via `tick_background_cluster` -> real wreck & jetcan are auto-abandoned even after Criminal Timer expires!
    let jita = 30_000_142u32;
    let (hs_crim, hs_vic, hs_loot) = (92_100_001, 92_100_002, 92_100_003);
    let (hs_crim_s, hs_vic_s, hs_loot_s) = (82_100_001, 82_100_002, 82_100_003);
    let mut hs_crim_sess = spawn_pilot(&ctx, 501, hs_crim, 1_000_009, hs_crim_s, 587, jita, DVec3::ZERO).await;
    let _hs_vic_sess = spawn_pilot(&ctx, 502, hs_vic, 1_000_010, hs_vic_s, 587, jita, DVec3::new(800.0, 0.0, 0.0)).await;
    let mut hs_loot_sess = spawn_pilot(&ctx, 503, hs_loot, 1_000_011, hs_loot_s, 587, jita, DVec3::new(300.0, 0.0, 0.0)).await;
    fit_online(&ctx, hs_crim, hs_crim_s, 82_100_101, 486, InventoryFlag::HiSlot0, Some((82_100_102, 185, 20)));
    seed_item(&ctx, 82_100_200, 34, hs_crim, hs_crim_s, InventoryFlag::Cargo, 40, false);
    for k in 1..=16 {
        seed_item(&ctx, 82_100_200 + k, 34, hs_crim, hs_crim_s, InventoryFlag::Cargo, 5, true);
    }
    lock_target(&ctx, &mut hs_crim_sess, jita, hs_vic_s).await;
    rpc(&ctx, &mut hs_crim_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut hs_crim_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_100_101), PyRep::Integer(hs_vic_s)]).await.unwrap();
    assert!(ctx.crimewatch.read().unwrap().states[&hs_crim].is_criminal(ctx.now_unix()));

    let PyRep::Tuple(hs_jet_t) = rpc(&ctx, &mut hs_crim_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(82_100_200)])]).await.unwrap() else { panic!() };
    let PyRep::List(hs_jet_ids) = &hs_jet_t[0] else { panic!() };
    let hs_jetcan = hs_jet_ids[0].as_i64().unwrap();
    let hs_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == hs_jetcan).unwrap().item_id);

    ctx.advance_virtual_clock(6);
    tick_background_cluster(&ctx).await;
    let hs_wreck = find_wreck_of_owner(&ctx, hs_crim, jita);

    // Advance 901s so `hs_crim`'s Criminal Timer expires: wreck & jetcan created while criminal remain permanently abandoned!
    ctx.advance_virtual_clock(901);
    assert!(!ctx.crimewatch.read().unwrap().states[&hs_crim].is_criminal(ctx.now_unix()));
    rpc(&ctx, &mut hs_loot_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let hs_inv = ctx.bind_object(&hs_loot_sess, "invbroker", hs_loot_s).unwrap();
    let hs_wrep = rpc(&ctx, &mut hs_loot_sess, "invbroker", Some(hs_inv), "LootAll", &[PyRep::Integer(hs_wreck)]).await.expect("High-sec criminal wreck auto-abandoned");
    assert_eq!(hs_wrep.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(hs_wrep.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    rpc(&ctx, &mut hs_loot_sess, "invbroker", Some(hs_inv), "Add", &[PyRep::Integer(hs_jet_item), PyRep::Integer(hs_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("High-sec criminal jetcan auto-abandoned");
    assert!(!ctx.crimewatch.read().unwrap().states.get(&hs_loot).is_some_and(|st| st.is_suspect(ctx.now_unix())));

    // 2. Low-sec (`Amamake` 30_002_537): pilot EWAR-attacks a Capsule (`670`) via `dogmaIM.Activate` -> acquires live Criminal Timer,
    //    jettisons a jetcan, and has their Impairor (`596`) destroyed via bounded Smartbomb (`14550`) cycles -> wreck & jetcan auto-abandoned!
    let lowsec = 30_002_537u32;
    let (ls_crim, ls_pod_c, ls_loot) = (92_200_001, 92_200_002, 92_200_003);
    let (ls_crim_s, ls_pod_s, ls_loot_s) = (82_200_001, 82_200_002, 82_200_003);
    let mut ls_crim_sess = spawn_pilot(&ctx, 511, ls_crim, 1_000_009, ls_crim_s, 596, lowsec, DVec3::ZERO).await;
    let _ls_pod_sess = spawn_pilot(&ctx, 512, ls_pod_c, 1_000_010, ls_pod_s, 670, lowsec, DVec3::new(9_000.0, 0.0, 0.0)).await;
    let mut ls_loot_sess = spawn_pilot(&ctx, 513, ls_loot, 1_000_011, ls_loot_s, 24692, lowsec, DVec3::new(400.0, 0.0, 0.0)).await;
    fit_online(&ctx, ls_crim, ls_crim_s, 82_200_101, 3242, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, ls_loot, ls_loot_s, 82_200_102, 14550, InventoryFlag::HiSlot0, None);
    seed_item(&ctx, 82_200_200, 34, ls_crim, ls_crim_s, InventoryFlag::Cargo, 40, false);
    for k in 1..=16 {
        seed_item(&ctx, 82_200_200 + k, 34, ls_crim, ls_crim_s, InventoryFlag::Cargo, 5, true);
    }
    lock_target(&ctx, &mut ls_crim_sess, lowsec, ls_pod_s).await;
    rpc(&ctx, &mut ls_crim_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ls_crim_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_200_101), PyRep::Integer(ls_pod_s)]).await.unwrap();
    assert!(ctx.crimewatch.read().unwrap().states[&ls_crim].is_criminal(ctx.now_unix()));

    let PyRep::Tuple(ls_jet_t) = rpc(&ctx, &mut ls_crim_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(82_200_200)])]).await.unwrap() else { panic!() };
    let PyRep::List(ls_jet_ids) = &ls_jet_t[0] else { panic!() };
    let ls_jetcan = ls_jet_ids[0].as_i64().unwrap();
    let ls_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == ls_jetcan).unwrap().item_id);

    rpc(&ctx, &mut ls_loot_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    for _ in 0..3 {
        if ctx.db.get_item(ls_crim_s).is_none() {
            break;
        }
        rpc(&ctx, &mut ls_loot_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_200_102)]).await.unwrap();
        ctx.advance_virtual_clock(11);
    }
    assert!(ctx.db.get_item(ls_crim_s).is_none(), "Low-sec criminal ship must be destroyed");
    let ls_wreck = find_wreck_of_owner(&ctx, ls_crim, lowsec);

    rpc(&ctx, &mut ls_loot_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let ls_inv = ctx.bind_object(&ls_loot_sess, "invbroker", ls_loot_s).unwrap();
    let ls_wrep = rpc(&ctx, &mut ls_loot_sess, "invbroker", Some(ls_inv), "LootAll", &[PyRep::Integer(ls_wreck)]).await.expect("Low-sec criminal wreck auto-abandoned");
    assert_eq!(ls_wrep.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(ls_wrep.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    rpc(&ctx, &mut ls_loot_sess, "invbroker", Some(ls_inv), "Add", &[PyRep::Integer(ls_jet_item), PyRep::Integer(ls_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("Low-sec criminal jetcan auto-abandoned");
    assert!(!ctx.crimewatch.read().unwrap().states.get(&ls_loot).is_some_and(|st| st.is_suspect(ctx.now_unix())));

    // 3. Null-sec (`Kaira` 30_003_887 -> stargate 50_007_698 -> `A2-V27` 30_003_942): criminal pilot jumps stargate into null-sec
    //    holding a live Criminal Timer, jettisons a jetcan, and is destroyed in null-sec -> wreck & jetcan auto-abandoned!
    let (kaira, a2_v27, gate_id) = (30_003_887u32, 30_003_942u32, 50_007_698i64);
    let gate_pos = DVec3::new(975_370_690_560.0, -4_693_133_598_720.0, -389_298_954_240.0);
    let (ns_crim, ns_pod_c, ns_loot) = (92_300_001, 92_300_002, 92_300_003);
    let (ns_crim_s, ns_pod_s, ns_loot_s) = (82_300_001, 82_300_002, 82_300_003);
    let mut ns_crim_sess = spawn_pilot(&ctx, 521, ns_crim, 1_000_009, ns_crim_s, 596, kaira, gate_pos).await;
    let _ns_pod_sess = spawn_pilot(&ctx, 522, ns_pod_c, 1_000_010, ns_pod_s, 670, kaira, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, ns_crim, ns_crim_s, 82_300_101, 447, InventoryFlag::MedSlot0, None);
    seed_item(&ctx, 82_300_200, 34, ns_crim, ns_crim_s, InventoryFlag::Cargo, 40, false);
    for k in 1..=16 {
        seed_item(&ctx, 82_300_200 + k, 34, ns_crim, ns_crim_s, InventoryFlag::Cargo, 5, true);
    }
    lock_target(&ctx, &mut ns_crim_sess, kaira, ns_pod_s).await;
    rpc(&ctx, &mut ns_crim_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ns_crim_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_300_101), PyRep::Integer(ns_pod_s)]).await.unwrap();
    ctx.advance_virtual_clock(61); // 60s Weapons Timer expires; 839s of live Criminal Timer remains
    let kaira_b = ctx.bind_object(&ns_crim_sess, "beyonce", kaira as i64).unwrap();
    rpc(&ctx, &mut ns_crim_sess, "beyonce", Some(kaira_b), "CmdStargateJump", &[PyRep::Integer(gate_id)]).await.expect("Low-sec criminal jumps gate into null-sec");
    assert_eq!(ns_crim_sess.solar_system_id, Some(a2_v27));
    assert!(ctx.crimewatch.read().unwrap().states[&ns_crim].is_criminal(ctx.now_unix()));

    let a2_h = ctx.systems.get_or_spawn(a2_v27);
    let (_, ns_crim_ball) = a2_h.get_ball_snapshot(ns_crim_s).await.unwrap().unwrap();
    let mut ns_loot_sess = spawn_pilot(&ctx, 523, ns_loot, 1_000_011, ns_loot_s, 24692, a2_v27, ns_crim_ball.position + DVec3::new(400.0, 0.0, 0.0)).await;
    fit_online(&ctx, ns_loot, ns_loot_s, 82_300_102, 14550, InventoryFlag::HiSlot0, None);
    let PyRep::Tuple(ns_jet_t) = rpc(&ctx, &mut ns_crim_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(82_300_200)])]).await.unwrap() else { panic!() };
    let PyRep::List(ns_jet_ids) = &ns_jet_t[0] else { panic!() };
    let ns_jetcan = ns_jet_ids[0].as_i64().unwrap();
    let ns_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == ns_jetcan).unwrap().item_id);

    for _ in 0..61 {
        let _ = a2_h.step_manual(1.0).await; // Expire 60-tick post-jump gate cloak/invulnerability naturally
    }
    ctx.advance_virtual_clock(65);
    assert!(ctx.crimewatch.read().unwrap().states[&ns_crim].is_criminal(ctx.now_unix()));
    for _ in 0..3 {
        if ctx.db.get_item(ns_crim_s).is_none() {
            break;
        }
        rpc(&ctx, &mut ns_loot_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_300_102)]).await.unwrap();
        ctx.advance_virtual_clock(11);
    }
    assert!(ctx.db.get_item(ns_crim_s).is_none(), "Null-sec criminal ship must be destroyed");
    let ns_wreck = find_wreck_of_owner(&ctx, ns_crim, a2_v27);

    rpc(&ctx, &mut ns_loot_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let ns_inv = ctx.bind_object(&ns_loot_sess, "invbroker", ns_loot_s).unwrap();
    let ns_wrep = rpc(&ctx, &mut ns_loot_sess, "invbroker", Some(ns_inv), "LootAll", &[PyRep::Integer(ns_wreck)]).await.expect("Null-sec criminal wreck auto-abandoned");
    assert_eq!(ns_wrep.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(ns_wrep.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    rpc(&ctx, &mut ns_loot_sess, "invbroker", Some(ns_inv), "Add", &[PyRep::Integer(ns_jet_item), PyRep::Integer(ns_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("Null-sec criminal jetcan auto-abandoned");

    // 4. Negative control: non-criminal pilot's jetcan and duel wreck are NOT auto-abandoned (`CantDoInCurrentSafetyLevel`)
    let (cl_a, cl_b, cl_sa, cl_sb) = (92_400_001, 92_400_002, 82_400_001, 82_400_002);
    let mut cl_sess_a = spawn_pilot(&ctx, 531, cl_a, 1_000_009, cl_sa, 596, lowsec, DVec3::new(50_000.0, 0.0, 0.0)).await;
    let mut cl_sess_b = spawn_pilot(&ctx, 532, cl_b, 1_000_010, cl_sb, 24692, lowsec, DVec3::new(50_400.0, 0.0, 0.0)).await;
    fit_online(&ctx, cl_b, cl_sb, 82_400_101, 14550, InventoryFlag::HiSlot0, None);
    seed_item(&ctx, 82_400_200, 34, cl_a, cl_sa, InventoryFlag::Cargo, 20, false);
    for k in 1..=16 {
        seed_item(&ctx, 82_400_200 + k, 34, cl_a, cl_sa, InventoryFlag::Cargo, 5, true);
    }
    let PyRep::Tuple(cl_jet_t) = rpc(&ctx, &mut cl_sess_a, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(82_400_200)])]).await.unwrap() else { panic!() };
    let PyRep::List(cl_jet_ids) = &cl_jet_t[0] else { panic!() };
    let cl_jetcan = cl_jet_ids[0].as_i64().unwrap();
    let cl_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == cl_jetcan).unwrap().item_id);
    rpc(&ctx, &mut cl_sess_a, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(cl_b)]).await.unwrap();
    rpc(&ctx, &mut cl_sess_b, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(cl_a), PyRep::Bool(true)]).await.unwrap();
    rpc(&ctx, &mut cl_sess_b, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    for _ in 0..3 {
        if ctx.db.get_item(cl_sa).is_none() {
            break;
        }
        rpc(&ctx, &mut cl_sess_b, "dogmaIM", None, "Activate", &[PyRep::Integer(82_400_101)]).await.unwrap();
        ctx.advance_virtual_clock(11);
    }
    assert!(ctx.db.get_item(cl_sa).is_none(), "Duel ship must be destroyed");
    let cl_wreck = find_wreck_of_owner(&ctx, cl_a, lowsec);
    let _ = ctx.systems.get_or_spawn(lowsec).set_ball_position(ls_loot_s, DVec3::new(50_200.0, 0.0, 0.0)).await;
    assert_eq!(rpc(&ctx, &mut ls_loot_sess, "invbroker", Some(ls_inv), "LootAll", &[PyRep::Integer(cl_wreck)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut ls_loot_sess, "invbroker", Some(ls_inv), "Add", &[PyRep::Integer(cl_jet_item), PyRep::Integer(cl_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
}

#[tokio::test]
async fn test_crimewatch_duel_limited_engagement_and_suspect_looting_ess() {
    // parity: feat:crimewatch.limited-engagement-duel, feat:crimewatch.suspect-timer-looting-ess, feat:crimewatch.safety-setting-action-block
    eve_testkit::attest(&[
        "feat:crimewatch.limited-engagement-duel",
        "feat:crimewatch.suspect-timer-looting-ess",
        "feat:crimewatch.safety-setting-action-block",
    ]);

    let ctx = new_ctx();
    let jita = 30_000_142u32;
    let lowsec = 30_002_537u32;
    let nullsec = 30_004_759u32;
    let (c1, c2, c3) = (93_000_001, 93_000_002, 93_000_003);
    let (s1, s2, s3) = (83_000_001, 83_000_002, 83_000_003);
    let mut sess1 = spawn_pilot(&ctx, 701, c1, 1_000_009, s1, 587, jita, DVec3::ZERO).await;
    let mut sess2 = spawn_pilot(&ctx, 702, c2, 1_000_010, s2, 587, jita, DVec3::new(1_500.0, 0.0, 0.0)).await;
    let mut sess3 = spawn_pilot(&ctx, 703, c3, 1_000_011, s3, 587, jita, DVec3::new(2_000.0, 0.0, 0.0)).await;

    fit_online(&ctx, c1, s1, 83_000_101, 486, InventoryFlag::HiSlot0, Some((83_000_102, 185, 200)));
    fit_online(&ctx, c1, s1, 83_000_103, 447, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, c2, s2, 83_000_201, 486, InventoryFlag::HiSlot0, Some((83_000_202, 185, 200)));
    fit_online(&ctx, c3, s3, 83_000_301, 486, InventoryFlag::HiSlot0, Some((83_000_302, 185, 200)));

    let sys_h = ctx.systems.get_or_spawn(jita);
    lock_target(&ctx, &mut sess1, jita, s2).await;
    lock_target(&ctx, &mut sess2, jita, s1).await;
    lock_target(&ctx, &mut sess3, jita, s1).await;

    // 1. Duel lifecycle: reject self/nonexistent/offline targets; challenge -> decline -> challenge -> accept -> 300s bidirectional LE
    let c_offline = 93_000_050i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&c1].clone();
        ch.character_id = c_offline;
        ch.user_id = 750;
        ch.active_ship_id = 0;
        snap.characters.insert(c_offline, ch);
    });
    assert_eq!(rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c1)]).await.unwrap_err().code, "InvalidDuelTarget");
    assert_eq!(rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(99_999_999)]).await.unwrap_err().code, "InvalidDuelTarget");
    assert_eq!(rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c_offline)]).await.unwrap_err().code, "DuelChallengeFailNoSesssionForTarget");

    let _ = ctx.drain_notifications_for_char(c1);
    let _ = ctx.drain_notifications_for_char(c2);
    rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c2)]).await.unwrap();
    assert!(ctx.drain_notifications_for_char(c2).iter().any(|n| n.event == "OnDuelChallenge"));
    rpc(&ctx, &mut sess2, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(c1), PyRep::Bool(false)]).await.unwrap();
    assert!(!with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(c1, c2, ctx.now_unix())));

    rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c2)]).await.unwrap();
    let acc = rpc(&ctx, &mut sess2, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(c1), PyRep::Bool(true)]).await.unwrap();
    assert_eq!(acc.dict_get("limitedEngagementSeconds").and_then(|v| v.as_i64()), Some(300));
    assert!(ctx.drain_notifications_for_char(c1).iter().any(|n| n.event == "OnCrimewatchEngagementCreated"));

    // Both duelists fight in high-sec on Safety Green (`2`) with zero CONCORD and zero sentry gun interference
    rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    rpc(&ctx, &mut sess2, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    rpc(&ctx, &mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_101), PyRep::Integer(s2)]).await.expect("Duelist 1 turret hit");
    rpc(&ctx, &mut sess2, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_201), PyRep::Integer(s1)]).await.expect("Duelist 2 return fire");
    tick_background_cluster(&ctx).await;
    assert!(ctx.crimewatch.read().unwrap().states.values().all(|st| st.pending_concord_arrival.is_none()), "CONCORD must ignore Limited Engagement");
    assert!(!with_combat_runtime(&ctx, |rt| rt.sentry_gun_aggression.values().any(|s| s.active_on_grid)), "Sentry guns must ignore Limited Engagement");

    // Subsequent offensive action after 120s resets LE back to `now + 300s`; after 301s idle, LE expires naturally
    ctx.advance_virtual_clock(120);
    let t_ref = ctx.now_unix();
    rpc(&ctx, &mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_103), PyRep::Integer(s2)]).await.expect("EWAR refreshes LE");
    let (e1, e2) = with_combat_runtime(&ctx, |rt| (rt.limited_engagements[&c1][&c2], rt.limited_engagements[&c2][&c1]));
    assert!(e1 >= t_ref + 300 && e2 >= t_ref + 300);

    ctx.advance_virtual_clock(301);
    let post_le_err = rpc(&ctx, &mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_101), PyRep::Integer(s2)]).await.unwrap_err();
    assert_eq!(post_le_err.code, "CantDoInCurrentSafetyLevel");

    // Unprovoked low-sec attack (Smartbomb requires Safety Red `0`) flags attacker Suspect WITHOUT creating an LE; victim returning fire creates 300s LE
    let (ls_a, ls_b, ls_sa, ls_sb) = (93_000_011, 93_000_012, 83_000_011, 83_000_012);
    let mut ls_sess_a = spawn_pilot(&ctx, 711, ls_a, 1_000_009, ls_sa, 626, lowsec, DVec3::ZERO).await;
    let mut ls_sess_b = spawn_pilot(&ctx, 712, ls_b, 1_000_010, ls_sb, 587, lowsec, DVec3::new(1_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, ls_a, ls_sa, 83_000_401, 3953, InventoryFlag::HiSlot0, None);
    fit_online(&ctx, ls_b, ls_sb, 83_000_402, 486, InventoryFlag::HiSlot0, Some((83_000_403, 185, 100)));
    lock_target(&ctx, &mut ls_sess_b, lowsec, ls_sa).await;
    rpc(&ctx, &mut ls_sess_a, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut ls_sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_401)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut ls_sess_a, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ls_sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_401)]).await.expect("Low-sec Smartbomb on Red");
    assert!(ctx.crimewatch.read().unwrap().states[&ls_a].is_suspect(ctx.now_unix()));
    assert!(!with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(ls_a, ls_b, ctx.now_unix())), "Unprovoked illegal hit must NOT create LE");
    rpc(&ctx, &mut ls_sess_b, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    rpc(&ctx, &mut ls_sess_b, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_402), PyRep::Integer(ls_sa)]).await.expect("Victim returns fire against Suspect");
    assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(ls_a, ls_b, ctx.now_unix())), "Returning fire against Suspect must create 300s LE");

    // 2. Suspect timer (900s) from foreign Cargo Container (`23` via `ship.Jettison`), foreign Wreck (`26561`), foreign Mobile Tractor Unit (`33475`, group 1250), and ESS Main Bank (`32882`)
    seed_item(&ctx, 83_099_001, 34, c2, s2, InventoryFlag::Cargo, 15, false);
    let _ = sys_h.set_ball_position(s2, DVec3::new(400.0, 0.0, 0.0)).await;
    let PyRep::Tuple(j_tup) = rpc(&ctx, &mut sess2, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(83_099_001)])]).await.unwrap() else { panic!() };
    let PyRep::List(j_ids) = &j_tup[0] else { panic!() };
    let foreign_jetcan = j_ids[0].as_i64().unwrap();
    assert_eq!(ctx.db.get_item(foreign_jetcan).unwrap().type_id, 23, "Jettison spawns Cargo Container (23)");
    let foreign_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == foreign_jetcan).unwrap().item_id);

    let inv1 = ctx.bind_object(&sess1, "invbroker", s1).unwrap();
    rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut sess1, "invbroker", Some(inv1), "Add", &[PyRep::Integer(foreign_jet_item), PyRep::Integer(foreign_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    let now_j = ctx.now_unix();
    rpc(&ctx, &mut sess1, "invbroker", Some(inv1), "Add", &[PyRep::Integer(foreign_jet_item), PyRep::Integer(foreign_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
    assert_eq!(ctx.crimewatch.read().unwrap().states[&c1].suspect_flag_until_unix, now_j + 900);

    for (offset, container_tid) in [(0i64, 26561u32), (20, 33475u32)] {
        ctx.advance_virtual_clock(901); // Let previous 900s Suspect Timer expire naturally
        let cid = 83_100_000 + offset;
        let lid = 83_100_001 + offset;
        seed_item(&ctx, cid, container_tid, c2, jita as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, lid, 34, c2, cid, InventoryFlag::Cargo, 10, false);
        let _ = sys_h.spawn_ball(Ball {
            id: cid, type_id: container_tid, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 5_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
        assert_eq!(rpc(&ctx, &mut sess1, "invbroker", Some(inv1), "Add", &[PyRep::Integer(lid), PyRep::Integer(cid), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
        rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
        let now_l = ctx.now_unix();
        rpc(&ctx, &mut sess1, "invbroker", Some(inv1), "Add", &[PyRep::Integer(lid), PyRep::Integer(cid), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
        assert_eq!(ctx.crimewatch.read().unwrap().states[&c1].suspect_flag_until_unix, now_l + 900);
    }

    // Any other player (`c3`) in high-sec on Safety Green (`2`) freely attacks Suspect `c1` without CONCORD or security-status penalty
    let sec3_before = ctx.db.get_character(c3).unwrap().security_status;
    rpc(&ctx, &mut sess3, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    rpc(&ctx, &mut sess3, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_301), PyRep::Integer(s1)]).await.expect("Free high-sec attack on Suspect");
    assert!(ctx.crimewatch.read().unwrap().states.values().all(|st| st.pending_concord_arrival.is_none()));
    assert_eq!(ctx.db.get_character(c3).unwrap().security_status, sec3_before);
    assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(c1, c3, ctx.now_unix())));

    // ESS Main Bank hack in null-sec grants 900s Suspect Timer (blocked on Green with CantDoInCurrentSafetyLevel, allowed on Yellow)
    let (ess_c, ess_s, ess_ball) = (93_000_099, 83_000_099, 820_000_000 + nullsec as i64);
    let mut ess_sess = spawn_pilot(&ctx, 799, ess_c, 1_000_009, ess_s, 587, nullsec, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| { snap.system_states.entry(nullsec).or_default().ess_main_bank = IskCents::from_isk(25_000_000); });
    let _ = ctx.systems.get_or_spawn(nullsec).spawn_ball(Ball {
        id: ess_ball, type_id: 32882, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 50_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    rpc(&ctx, &mut ess_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let ess_err = rpc(&ctx, &mut ess_sess, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball)]).await.unwrap_err();
    assert_eq!(ess_err.code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut ess_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    let now_ess = ctx.now_unix();
    rpc(&ctx, &mut ess_sess, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball)]).await.expect("ESS link on Yellow");
    assert_eq!(ctx.crimewatch.read().unwrap().states[&ess_c].suspect_flag_until_unix, now_ess + 900);
}

#[tokio::test]
async fn test_crimewatch_safety_setting_green_yellow_red_enforcement() {
    // parity: feat:crimewatch.safety-setting-action-block
    eve_testkit::attest(&["feat:crimewatch.safety-setting-action-block"]);

    let ctx = new_ctx();
    let jita = 30_000_142u32;
    let lowsec = 30_002_537u32;
    let (atk_c, vic_c, ally_c) = (94_000_001, 94_000_002, 94_000_003);
    let (atk_s, vic_s, ally_s) = (84_000_001, 84_000_002, 84_000_003);
    let mut atk_sess = spawn_pilot(&ctx, 801, atk_c, 1_000_009, atk_s, 626, jita, DVec3::ZERO).await;
    let mut vic_sess = spawn_pilot(&ctx, 802, vic_c, 1_000_010, vic_s, 587, jita, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let mut ally_sess = spawn_pilot(&ctx, 803, ally_c, 1_000_011, ally_s, 587, jita, DVec3::new(1_200.0, 0.0, 0.0)).await;

    let burst_tid = 580u32; // Burst Jammer I (group 80)
    fit_online(&ctx, atk_c, atk_s, 84_000_101, 486, InventoryFlag::HiSlot0, Some((84_000_102, 185, 100)));
    fit_online(&ctx, atk_c, atk_s, 84_000_103, 3953, InventoryFlag::HiSlot1, None);
    fit_online(&ctx, atk_c, atk_s, 84_000_104, burst_tid, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, atk_c, atk_s, 84_000_105, 11355, InventoryFlag::HiSlot2, None); // Small Remote Armor Repairer I
    assert!(ctx.get_or_rebuild_ship_fitting(atk_c, atk_s).unwrap().modules.contains_key(&84_000_105), "Remote repairer must reside in ShipFittingEngine.modules");
    fit_online(&ctx, ally_c, ally_s, 84_000_107, 486, InventoryFlag::HiSlot0, Some((84_000_108, 185, 50)));
    seed_item(&ctx, 84_000_106, 2454, atk_c, atk_s, InventoryFlag::DroneBay, 1, true);
    seed_item(&ctx, 84_000_109, 34, vic_c, vic_s, InventoryFlag::Cargo, 20, false);

    lock_target(&ctx, &mut atk_sess, jita, vic_s).await;
    lock_target(&ctx, &mut atk_sess, jita, ally_s).await;
    lock_target(&ctx, &mut ally_sess, jita, vic_s).await;
    rpc(&ctx, &mut atk_sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(84_000_106)])]).await.unwrap();
    let hob_ball = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.keys().copied().next().unwrap());

    // 1. Safety Green (`2`) in high-sec blocks turret, Smartbomb, Burst Jammer, offensive drone, and remote repair onto LE/Suspect (`CantDoInCurrentSafetyLevel`)
    let _ = ctx.drain_notifications_for_char(atk_c);
    rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let notifs = ctx.drain_notifications_for_char(atk_c);
    assert_eq!(notifs.iter().filter(|n| n.event == "OnSafetyLevelChanged" && n.args == vec![PyRep::Integer(2)]).count(), 1);

    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_101), PyRep::Integer(vic_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_103)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_104)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "entity", None, "CmdEngage", &[PyRep::List(vec![PyRep::Integer(hob_ball)]), PyRep::Integer(vic_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    // Live duel creates a real Limited Engagement on `ally_c` -> Green blocks remote repair (`CantDoInCurrentSafetyLevel`)
    rpc(&ctx, &mut ally_sess, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(vic_c)]).await.unwrap();
    rpc(&ctx, &mut vic_sess, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(ally_c), PyRep::Bool(true)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    ctx.advance_virtual_clock(301); // Duel LE expires naturally

    // Live theft gives `ally_c` a real Suspect Timer -> Green blocks remote repair (`CantDoInCurrentSafetyLevel`)
    let PyRep::Tuple(vj_t) = rpc(&ctx, &mut vic_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(84_000_109)])]).await.unwrap() else { panic!() };
    let PyRep::List(vj_ids) = &vj_t[0] else { panic!() };
    let v_can = vj_ids[0].as_i64().unwrap();
    let v_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == v_can).unwrap().item_id);
    let atk_inv = ctx.bind_object(&atk_sess, "invbroker", atk_s).unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "invbroker", Some(atk_inv), "Add", &[PyRep::Integer(v_item), PyRep::Integer(v_can), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "invbroker", Some(atk_inv), "LootAll", &[PyRep::Integer(v_can)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut ally_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    let ally_inv = ctx.bind_object(&ally_sess, "invbroker", ally_s).unwrap();
    rpc(&ctx, &mut ally_sess, "invbroker", Some(ally_inv), "Add", &[PyRep::Integer(v_item), PyRep::Integer(v_can), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    // 2. Safety Yellow (`1`) blocks Criminal actions (high-sec PvP, Smartbomb/Burst Jammer, repping Criminal, low-sec Capsule/AoE) while allowing Suspect actions
    rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_101), PyRep::Integer(vic_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_103)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_104)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.expect("Safety Yellow allows repping Suspect");
    assert!(ctx.crimewatch.read().unwrap().states[&atk_c].is_suspect(ctx.now_unix()), "Repping Suspect must inherit Suspect flag");
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(84_000_105)]).await.unwrap();

    // `ally_sess` switches to Red and shoots `vic_s` -> Criminal Timer -> `atk_sess` on Yellow blocked (`CantDoInCurrentSafetyLevel`), on Red inherits Criminal + CONCORD!
    rpc(&ctx, &mut ally_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ally_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_107), PyRep::Integer(vic_s)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    // Low-sec Green & Yellow vs Capsule (`670`), Smartbomb (`3953`), and Burst Jammer (`burst_tid`): all blocked with `CantDoInCurrentSafetyLevel`; Red (`0`) permits!
    let (ls_c, ls_vc, ls_s, ls_pod) = (94_000_021, 94_000_022, 84_000_021, 84_000_022);
    let mut ls_sess = spawn_pilot(&ctx, 821, ls_c, 1_000_009, ls_s, 626, lowsec, DVec3::ZERO).await;
    let _ls_pod_sess = spawn_pilot(&ctx, 822, ls_vc, 1_000_010, ls_pod, 670, lowsec, DVec3::new(1_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, ls_c, ls_s, 84_000_201, 447, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, ls_c, ls_s, 84_000_202, 3953, InventoryFlag::HiSlot0, None);
    fit_online(&ctx, ls_c, ls_s, 84_000_203, burst_tid, InventoryFlag::MedSlot1, None);
    lock_target(&ctx, &mut ls_sess, lowsec, ls_pod).await;
    for lvl in [2i64, 1i64] {
        rpc(&ctx, &mut ls_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(lvl)]).await.unwrap();
        assert_eq!(rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_201), PyRep::Integer(ls_pod)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
        assert_eq!(rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_202)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
        assert_eq!(rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_203)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    }
    let pod_cs = ctx.systems.get_or_spawn(lowsec).get_combat_state(ls_pod).await.unwrap().unwrap();
    assert_eq!(pod_cs.shield_hp, pod_cs.max_shield_hp, "Low-sec pod must take 0 Smartbomb damage under Safety Yellow");
    assert_eq!(pod_cs.ecm_jammed_remaining_s, 0.0, "Low-sec pod must not be ECM-burst jammed under Safety Yellow");
    assert!(!ctx.crimewatch.read().unwrap().states.get(&ls_c).is_some_and(|st| st.is_criminal(ctx.now_unix())));

    ctx.advance_virtual_clock(35);
    let ls_sec_before = ctx.db.get_character(ls_c).unwrap().security_status;
    rpc(&ctx, &mut ls_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_203)]).await.expect("Safety Red permits low-sec Burst Jammer near pod");
    rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_202)]).await.expect("Safety Red permits low-sec Smartbomb near pod");
    assert!(ctx.crimewatch.read().unwrap().states[&ls_c].is_criminal(ctx.now_unix()));
    assert!(ctx.db.get_character(ls_c).unwrap().security_status < ls_sec_before);

    rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.expect("Safety Red permits repping Criminal in high-sec");
    let atk_cw = ctx.crimewatch.read().unwrap().states[&atk_c].clone();
    assert!(atk_cw.is_criminal(ctx.now_unix()) && atk_cw.pending_concord_arrival.is_some(), "Repping Criminal in high-sec on Red must schedule CONCORD");
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_101), PyRep::Integer(vic_s)]).await.expect("Safety Red permits high-sec PvP");

    // 3. R1 negative test: out-of-range or no-charge activation on a neutral leaves crimewatch flags and security status unchanged
    let (neg_c, neg_vic, neg_s, neg_vs) = (94_000_031, 94_000_032, 84_000_031, 84_000_032);
    let mut neg_sess = spawn_pilot(&ctx, 831, neg_c, 1_000_009, neg_s, 626, jita, DVec3::ZERO).await;
    let _neg_vic_sess = spawn_pilot(&ctx, 832, neg_vic, 1_000_010, neg_vs, 587, jita, DVec3::new(30_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, neg_c, neg_s, 84_000_301, 447, InventoryFlag::MedSlot0, None); // Warp Scrambler I
    fit_online(&ctx, neg_c, neg_s, 84_000_302, 529, InventoryFlag::HiSlot1, None); // Small Remote Capacitor Transmitter I (falloff 0)
    lock_target(&ctx, &mut neg_sess, jita, neg_vs).await;
    rpc(&ctx, &mut neg_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    let neg_sec_before = ctx.db.get_character(neg_c).unwrap().security_status;

    // Out of range offensive module (30 km >> 9 km) -> fails with TargetOutOfRange; crimewatch flags and security status untouched
    let oor_atk = rpc(&ctx, &mut neg_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_301), PyRep::Integer(neg_vs)]).await;
    assert_eq!(oor_atk.unwrap_err().code, "TargetOutOfRange");
    assert_eq!(ctx.db.get_character(neg_c).unwrap().security_status, neg_sec_before);
    assert!(!ctx.crimewatch.read().unwrap().states.get(&neg_c).is_some_and(|st| st.is_criminal(ctx.now_unix())));

    // Out of range remote assistance module (30 km >> 6 km, falloff 0) -> fails with TargetOutOfRange; crimewatch flags and sec status untouched
    let oor_res = rpc(&ctx, &mut neg_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_302), PyRep::Integer(neg_vs)]).await;
    assert_eq!(oor_res.unwrap_err().code, "TargetOutOfRange");
    assert_eq!(ctx.db.get_character(neg_c).unwrap().security_status, neg_sec_before);
    assert!(!ctx.crimewatch.read().unwrap().states.get(&neg_c).is_some_and(|st| st.is_criminal(ctx.now_unix())));

    // In-range but drained capacitor -> fails with NotEnoughCapacitor; crimewatch flags and sec status untouched
    let _ = ctx.systems.get_or_spawn(jita).set_ball_position(neg_vs, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = ctx.systems.get_or_spawn(jita).set_capacitor(neg_s, 0.0).await;
    let cap_res = rpc(&ctx, &mut neg_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_301), PyRep::Integer(neg_vs)]).await;
    assert_eq!(cap_res.unwrap_err().code, "NotEnoughCapacitor");
    assert_eq!(ctx.db.get_character(neg_c).unwrap().security_status, neg_sec_before);
    assert!(!ctx.crimewatch.read().unwrap().states.get(&neg_c).is_some_and(|st| st.is_criminal(ctx.now_unix())));
}
