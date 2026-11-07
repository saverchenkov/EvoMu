//! End-to-end integration tests for Shard 5 tactical modules, mobile depots,
//! insurance lifecycle, Pochven PvP loot drops, and in-space destruction notifications.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{buy, dock_station, fund_pilots, login, move_item, rpc, train_skills, travel, JITA_STATION, PILOT_1, PILOT_2};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, VirtualServerClock, WorldRepository};
use eve_destiny::DVec3;
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables as depots;
use eve_server::services::security_mgr::insurgency_pvp_loot_drop_threshold;
use eve_server::world_seeder::seed_full_market_catalog;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(dead_code)]
const JITA_SYS: u32 = 30_000_142;
#[allow(dead_code)]
const NULL_SYS: u32 = 30_000_208;
#[allow(dead_code)]
const POCHVEN_SYS: u32 = 30_000_157;

#[allow(dead_code)]
fn temp_db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_s5_combat_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[allow(dead_code)]
fn open_ctx(path: &Path, now_unix: u64) -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    db.with_mut_snapshot(|snap| {
        if snap.accounts.is_empty() {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        }
    });
    let ctx = Arc::new(ServerContext::with_repository(db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

#[allow(dead_code)]
fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i64, singleton: bool) {
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            item_id,
            InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity: quantity as i32, singleton, ..Default::default() },
        );
    });
}

#[allow(dead_code)]
fn seed_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|s| {
        let map = s.skills.entry(char_id).or_default();
        for &(tid, lvl) in skills {
            map.insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
    });
}

#[allow(dead_code)]
async fn undock_at(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, pos: DVec3) -> i64 {
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|st| st.solar_system_id == sys_id)
        .map(|st| st.station_id)
        .expect("NPC station in target system");
    let char_id = sess.character_id.unwrap();
    let ship_id = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        let sid = ch.active_ship_id;
        ch.solar_system_id = sys_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        if let Some(ship) = s.items.get_mut(&sid) {
            ship.location_id = station_id;
            ship.flag = InventoryFlag::Hangar;
        }
        sid
    });
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(station_id);
    sess.structure_id = None;
    rpc(ctx, sess, "stationSvc", "Undock", &[]).await.unwrap();
    let bind = rpc(ctx, sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(sys_id)), PyRep::None]).await.unwrap();
    let beyonce = match bind {
        PyRep::Tuple(parts) => parts[0].as_i64().unwrap(),
        _ => panic!("beyonce bind tuple"),
    };
    let sun = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.group_id == 6).expect("sun").celestial_id;
    let _ = sun;
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.set_ball_position(ship_id, pos).await;
    let _ = sys.step_manual(1.0).await;
    beyonce
}

#[allow(dead_code)]
async fn lock(ctx: &ServerContext, sys_id: u32, sess: &mut SessionState, target_id: i64) {
    let ship_id = sess.ship_id.unwrap();
    let sys = ctx.systems.get_or_spawn(sys_id);
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)]).await.expect("AddTarget");
    for _ in 0..90 {
        if sys.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == target_id && l.locked) {
            return;
        }
        let _ = sys.step_manual(1.0).await;
    }
    panic!("lock timeout for {target_id}");
}

// parity: feat:modules.disruptive-lance
// parity: feat:modules.energy-nosferatu-relative-capacitor
// parity: feat:modules.lance-doomsday-directional-geometry
// parity: feat:modules.nanite-repair-paste-in-space
// parity: feat:modules.mobile-depot-deployment-and-refitting
#[tokio::test]
async fn test_shard5_lance_doomsday_nosferatu_nanite_and_mobile_depot() {
    eve_testkit::attest(&[
        "feat:modules.disruptive-lance",
        "feat:modules.energy-nosferatu-relative-capacitor",
        "feat:modules.lance-doomsday-directional-geometry",
        "feat:modules.nanite-repair-paste-in-space",
        "feat:modules.mobile-depot-deployment-and-refitting",
    ]);

    let db_path = temp_db("modules");
    let base_time = common::TEST_CLOCK_BASE_UNIX;
    let dep_id: i64;
    let refit_mod_id: i64;
    let titan_ship_id: i64;
    let megathron_ship_id: i64;
    let laser_id: i64;
    let reload_time: u64;

    {
        let ctx = open_ctx(&db_path, base_time);
        ctx.set_mechanic_rng_seed(Some(2026));
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 10_000_000_000.0), (PILOT_2, 10_000_000_000.0)]).await;

        let mut p1 = login(&ctx, 51_001, "demo", PILOT_1).await;
        let mut p2 = login(&ctx, 51_002, "other", PILOT_2).await;
        assert_eq!(p1.role & service_roles::ROLE_GM_MASK, 0);
        assert_eq!(p2.role & service_roles::ROLE_GM_MASK, 0);

        train_skills(&ctx, &mut p1, &[530, 533, 377], true).await;
        train_skills(&ctx, &mut p2, &[629, 10_836], false).await;

        // 1. Energy Nosferatu absolute GJ vs percentage rule (`feat:modules.energy-nosferatu-relative-capacitor`)
        let v_ship = p1.ship_id.unwrap();
        let nos = buy(&ctx, &mut p1, 530, 1).await[0];
        let neut = buy(&ctx, &mut p1, 533, 1).await[0];
        move_item(&ctx, &mut p1, nos, v_ship, 28).await;
        move_item(&ctx, &mut p1, neut, v_ship, 29).await;

        let rupture = buy(&ctx, &mut p2, 629, 1).await[0];
        rpc(&ctx, &mut p2, "ship", "AssembleShip", &[PyRep::Integer(rupture)]).await.unwrap();
        ctx.advance_secs(12);
        rpc(&ctx, &mut p2, "ship", "Board", &[PyRep::Integer(rupture)]).await.unwrap();
        let msb = buy(&ctx, &mut p2, 10_836, 1).await[0];
        move_item(&ctx, &mut p2, msb, rupture, 19).await;

        let sun = ctx.sde.celestials_in_system(JITA_SYS).into_iter().find(|c| c.group_id == 6).expect("sun").celestial_id;
        travel(&ctx, &mut p1, JITA_SYS).await;
        crate::industry_live_support::warp_to(&ctx, &mut p1, sun, 100_000.0).await;
        travel(&ctx, &mut p2, JITA_SYS).await;
        crate::industry_live_support::warp_to(&ctx, &mut p2, sun, 100_000.0).await;
        ctx.advance_secs(15);
        let jita = ctx.systems.get_or_spawn(JITA_SYS);
        for _ in 0..15 {
            let _ = jita.step_manual(1.0).await;
        }
        rpc(&ctx, &mut p1, "dogmaIM", "AddTarget", &[PyRep::Integer(rupture)]).await.unwrap();
        let _ = jita.step_manual(12.0).await;

        // Rupture burns its cap down below 58% (~500 GJ) > Rifter max (250 GJ), and 1 neut cycle gives Rifter empty cap room (> 8 GJ)
        rpc(&ctx, &mut p2, "dogmaIM", "Activate", &[PyRep::Integer(msb), PyRep::String("shieldBoosting".into())]).await.unwrap();
        for _ in 0..300 {
            let _ = jita.step_manual(1.0).await;
            let st = jita.get_combat_state(rupture).await.unwrap().unwrap();
            if st.capacitor / st.max_capacitor < 0.58 {
                break;
            }
        }
        rpc(&ctx, &mut p2, "dogmaIM", "Deactivate", &[PyRep::Integer(msb), PyRep::String("shieldBoosting".into())]).await.unwrap();
        let _ = jita.step_manual(3.5).await;
        rpc(&ctx, &mut p1, "dogmaIM", "Activate", &[PyRep::Integer(neut), PyRep::String("energyNeutralizerFalloff".into()), PyRep::Integer(rupture)]).await.unwrap();
        let _ = jita.step_manual(1.0).await;
        rpc(&ctx, &mut p1, "dogmaIM", "Deactivate", &[PyRep::Integer(neut), PyRep::String("energyNeutralizerFalloff".into())]).await.unwrap();
        let _ = jita.step_manual(6.0).await;

        let v_cap0 = jita.get_combat_state(v_ship).await.unwrap().unwrap().capacitor;
        let r_cap0 = jita.get_combat_state(rupture).await.unwrap().unwrap().capacitor;
        assert!(r_cap0 > v_cap0);
        rpc(&ctx, &mut p1, "dogmaIM", "Activate", &[PyRep::Integer(nos), PyRep::String("energyNosferatuFalloff".into()), PyRep::Integer(rupture)]).await.unwrap();
        let mut prev_r = r_cap0;
        let mut drained = false;
        for _ in 0..50 {
            let _ = jita.step_manual(0.1).await;
            let now_r = jita.get_combat_state(rupture).await.unwrap().unwrap().capacitor;
            if now_r < prev_r - 1.0 {
                drained = true;
                break;
            }
            prev_r = now_r;
        }
        rpc(&ctx, &mut p1, "dogmaIM", "Deactivate", &[PyRep::Integer(nos), PyRep::String("energyNosferatuFalloff".into())]).await.unwrap();
        assert!(drained, "Nosferatu drains target when target GJ > caster GJ");

        // 2. Nanite Repair Paste in space (`feat:modules.nanite-repair-paste-in-space`)
        ctx.advance_secs(905);
        dock_station(&ctx, &mut p1, JITA_STATION).await;
        dock_station(&ctx, &mut p2, JITA_STATION).await;
        seed_skills(&ctx, PILOT_1, &[(28164, 5), (28879, 5), (28880, 5), (3336, 5), (3423, 5), (3347, 5), (21803, 5)]);
        megathron_ship_id = 950_100_001;
        laser_id = 950_100_002;
        let crystal_id = 950_100_003;
        seed_item(&ctx, megathron_ship_id, 641, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, laser_id, 3041, PILOT_1, megathron_ship_id, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, crystal_id, 12_565, PILOT_1, megathron_ship_id, InventoryFlag::Cargo, 1, false);
        rpc(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(megathron_ship_id)]).await.unwrap();
        ctx.advance_secs(12);
        rpc(&ctx, &mut p1, "dogmaIM", "LoadAmmo", &[PyRep::Integer(megathron_ship_id), PyRep::List(vec![PyRep::Integer(laser_id)]), PyRep::List(vec![PyRep::Integer(crystal_id)])]).await.unwrap();
        let depot_item = buy(&ctx, &mut p1, 33_474, 1).await[0];
        let ext_item = buy(&ctx, &mut p1, 377, 1).await[0];
        refit_mod_id = ext_item;
        ctx.db.with_mut_snapshot(|s| {
            for id in [depot_item, ext_item] {
                if let Some(it) = s.items.get_mut(&id) {
                    it.location_id = megathron_ship_id;
                    it.flag = InventoryFlag::Cargo;
                }
            }
        });

        let p2_stab = 950_100_010;
        ctx.db.with_mut_snapshot(|s| {
            s.service_state_blobs.remove("account_subscriptions");
            for ch in s.characters.values_mut() {
                ch.clone_grade = 2;
            }
        });
        train_skills(&ctx, &mut p2, &[624, 641, 3764, 19_724], true).await;
        train_skills(&ctx, &mut p1, &[641, 3764, 77_283, 11_567, 4_292, 77_399, 40_631], true).await;
        seed_item(&ctx, p2_stab, 19_724, PILOT_2, JITA_STATION, InventoryFlag::Hangar, 1, true);
        rpc(&ctx, &mut p2, "ship", "Board", &[PyRep::Integer(p2_stab)]).await.unwrap();
        ctx.advance_secs(12);

        let (null_sys_id, null_station_id) = ctx
            .sde
            .stations
            .values()
            .find(|st| ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|sys| sys.security_status <= 0.0))
            .map(|st| (st.solar_system_id, st.station_id))
            .expect("nullsec NPC station");
        undock_at(&ctx, &mut p1, null_sys_id, DVec3::new(0.0, 0.0, 0.0)).await;
        undock_at(&ctx, &mut p2, null_sys_id, DVec3::new(2_500.0, 0.0, 0.0)).await;
        let null_sys = ctx.systems.get_or_spawn(null_sys_id);
        lock(&ctx, null_sys_id, &mut p1, p2_stab).await;

        ctx.db.with_mut_snapshot(|s| {
            if let Some(it) = s.items.get_mut(&laser_id) {
                it.dynamic_attributes.insert(eve_dogma::attr_ids::DAMAGE, 12.0);
            }
        });
        let no_paste = rpc(&ctx, &mut p1, "dogmaIM", "RepairModule", &[PyRep::Integer(laser_id)]).await.unwrap_err();
        assert_eq!(no_paste.code, "NotEnoughNanitePaste");

        let paste_id = 950_100_011;
        seed_item(&ctx, paste_id, 28_668, PILOT_1, megathron_ship_id, InventoryFlag::Cargo, 40, false);
        let rep = rpc(&ctx, &mut p1, "dogmaIM", "RepairModule", &[PyRep::Integer(laser_id)]).await.unwrap();
        assert_eq!(rep.dict_get("pasteConsumed"), Some(&PyRep::Integer(1)));
        for _ in 0..15 {
            null_sys.try_step_full_tick(1.0);
        }
        assert!(
            ctx.db
                .with_snapshot(|s| {
                    s.items
                        .get(&laser_id)
                        .and_then(|i| i.dynamic_attributes.get(&eve_dogma::attr_ids::DAMAGE).copied())
                        .unwrap_or(0.0)
                })
                < 12.0
        );
        ctx.db.with_mut_snapshot(|s| {
            if let Some(it) = s.items.get_mut(&laser_id) {
                it.dynamic_attributes.insert(eve_dogma::attr_ids::DAMAGE, 6.0);
            }
        });
        rpc(&ctx, &mut p1, "dogmaIM", "RepairModule", &[PyRep::Integer(laser_id)]).await.unwrap();
        rpc(&ctx, &mut p1, "dogmaIM", "StopRepairModule", &[PyRep::Integer(laser_id)]).await.unwrap();

        // 3. Mobile Depot deployment & in-space refitting (`feat:modules.mobile-depot-deployment-and-refitting`)
        null_sys.set_ball_position(megathron_ship_id, DVec3::new(100_000.0, 0.0, 0.0)).await;
        let _ = null_sys.step_manual(1.0).await;
        let dep_rep = rpc(&ctx, &mut p1, "posMgr", "AnchorStructure", &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(100_400.0), PyRep::Real(0.0), PyRep::Real(0.0)])]).await.unwrap();
        dep_id = dep_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        ctx.advance_secs(65);
        let _ = null_sys.step_manual(1.0).await;
        rpc(&ctx, &mut p1, "invbroker", "Add", &[PyRep::Integer(refit_mod_id), PyRep::Integer(megathron_ship_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)]).await.unwrap();
        let st = ctx.db.with_snapshot(|s| depots::read_deployable_state_snap(s).deployables.get(&dep_id).cloned()).unwrap();
        assert!(st.anchor_completed);

        // 4. Disruptive Lance & Directional Lance Doomsday (`feat:modules.disruptive-lance`, `feat:modules.lance-doomsday-directional-geometry`)
        dock_station(&ctx, &mut p1, null_station_id).await;
        dock_station(&ctx, &mut p2, null_station_id).await;
        let lancer_ship_id = 950_100_015;
        let siege_id = 950_100_016;
        let dlance_id = 950_100_017;
        seed_item(&ctx, lancer_ship_id, 77_283, PILOT_1, null_station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, siege_id, 4_292, PILOT_1, lancer_ship_id, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, dlance_id, 77_399, PILOT_1, lancer_ship_id, InventoryFlag::HiSlot1, 1, true);
        seed_item(&ctx, 950_100_018, 16_275, PILOT_1, lancer_ship_id, InventoryFlag::Cargo, 250, false);
        seed_item(&ctx, 950_100_019, 16_274, PILOT_1, lancer_ship_id, InventoryFlag::SpecializedFuelBay, 30_000, false);
        rpc(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(lancer_ship_id)]).await.unwrap();
        ctx.advance_secs(12);
        undock_at(&ctx, &mut p1, null_sys_id, DVec3::new(0.0, 200_000.0, 0.0)).await;
        null_sys.set_ball_position(lancer_ship_id, DVec3::new(0.0, 200_000.0, 0.0)).await;
        let p2_beyonce = undock_at(&ctx, &mut p2, null_sys_id, DVec3::new(0.0, 240_000.0, 0.0)).await;
        ctx.dispatch_rpc(&mut p2, "beyonce", Some(p2_beyonce as u64), "CmdGotoPoint", &[PyRep::Real(0.0), PyRep::Real(240_000.0), PyRep::Real(0.0)], &std::collections::HashMap::new(), None).await.unwrap();
        null_sys.set_ball_position(p2_stab, DVec3::new(0.0, 240_000.0, 0.0)).await;
        let _ = null_sys.step_manual(1.0).await;
        let dl_shield0 = null_sys.get_combat_state(p2_stab).await.unwrap().unwrap().shield_hp;
        rpc(&ctx, &mut p1, "dogmaIM", "Activate", &[PyRep::Integer(siege_id)]).await.unwrap();
        let dl_res = rpc(&ctx, &mut p1, "dogmaIM", "Activate", &[PyRep::Integer(dlance_id), PyRep::String("default".into()), PyRep::Integer(p2_stab)]).await.unwrap();
        assert_eq!(dl_res.dict_get("hitCount"), Some(&PyRep::Integer(1)));
        assert_eq!(dl_res.dict_get("fuelConsumed"), Some(&PyRep::Integer(20_000)));
        assert!(null_sys.get_combat_state(p2_stab).await.unwrap().unwrap().shield_hp < dl_shield0);
        let dl_warp_err = ctx.dispatch_rpc(&mut p2, "beyonce", Some(p2_beyonce as u64), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(p2_beyonce)], &std::collections::HashMap::new(), None).await.unwrap_err();
        assert_eq!(dl_warp_err.code, "DisruptiveLanceDebuffActive");

        // Titan Directional Lance Doomsday via `superWeaponMgr.ActivateSlashModule`
        let _ = rpc(&ctx, &mut p1, "dogmaIM", "Deactivate", &[PyRep::Integer(siege_id)]).await;
        ctx.advance_secs(310);
        let _ = null_sys.step_manual(310.0).await;
        dock_station(&ctx, &mut p1, null_station_id).await;
        titan_ship_id = 950_100_020;
        let doomsday_id = 950_100_021;
        seed_item(&ctx, titan_ship_id, 11_567, PILOT_1, null_station_id, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, doomsday_id, 40_631, PILOT_1, titan_ship_id, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, 950_100_022, 16_274, PILOT_1, titan_ship_id, InventoryFlag::Cargo, 100_000, false);
        rpc(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(titan_ship_id)]).await.unwrap();
        ctx.advance_secs(12);
        undock_at(&ctx, &mut p1, null_sys_id, DVec3::new(0.0, 200_000.0, 0.0)).await;
        null_sys.set_ball_position(titan_ship_id, DVec3::new(0.0, 200_000.0, 0.0)).await;
        null_sys.set_ball_position(p2_stab, DVec3::new(0.0, 250_000.0, 0.0)).await;
        let _ = null_sys.step_manual(1.0).await;
        let cs_before = null_sys.get_combat_state(p2_stab).await.unwrap().unwrap();

        let origin_arg = PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(200_000.0), PyRep::Real(0.0)]);
        let endpoint_arg = PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(400_000.0), PyRep::Real(0.0)]);
        let lance_res = rpc(&ctx, &mut p1, "superWeaponMgr", "ActivateSlashModule", &[PyRep::Integer(doomsday_id), origin_arg, endpoint_arg]).await.unwrap();
        assert_eq!(lance_res.dict_get("warmupSeconds"), Some(&PyRep::Real(15.0)));
        assert_eq!(lance_res.dict_get("jumpBlockSeconds"), Some(&PyRep::Real(300.0)));
        assert_eq!(lance_res.dict_get("fuelConsumed"), Some(&PyRep::Integer(50_000)));
        for _ in 0..22 {
            let _ = null_sys.step_manual(1.0).await;
        }
        let lance_damaged_or_destroyed = match null_sys.get_combat_state(p2_stab).await.unwrap() {
            Some(cs) => cs.shield_hp < cs_before.shield_hp || cs.armor_hp < cs_before.armor_hp || cs.structure_hp < cs_before.structure_hp,
            None => true,
        };
        assert!(lance_damaged_or_destroyed, "Directional lance beam must damage target inside beam corridor");
        ctx.db.with_mut_snapshot(|_| {});
        reload_time = ctx.now_unix() + 120;
    }

    // Reload SQLite and verify deployed depot & fitted refit module persisted
    let ctx2 = open_ctx(&db_path, reload_time);
    let refit_item = ctx2.db.with_snapshot(|s| s.items.get(&refit_mod_id).cloned()).expect("persisted refit module");
    assert_eq!(refit_item.location_id, megathron_ship_id);
    assert_eq!(refit_item.flag, InventoryFlag::MedSlot0);
    let titan_iso: i64 = ctx2.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == titan_ship_id && i.type_id == 16_274).map(|i| i64::from(i.quantity)).sum());
    assert_eq!(titan_iso, 50_000);
    let persisted_depot = ctx2.db.with_snapshot(|s| depots::read_deployable_state_snap(s).deployables.get(&dep_id).cloned()).expect("persisted depot");
    assert_eq!(persisted_depot.owner_id, PILOT_1);
    let _ = std::fs::remove_file(&db_path);
}

// parity: feat:combat.insurance-void-and-expiry
// parity: feat:combat.pochven-loot-drop
// parity: notify:OnKillNotification
// parity: notify:OnShipDeath
// parity: notify:OnMultiEvent
#[tokio::test]
async fn test_shard5_insurance_pochven_loot_and_kill_notifications() {
    eve_testkit::attest(&[
        "feat:combat.insurance-void-and-expiry",
        "feat:combat.pochven-loot-drop",
        "notify:OnKillNotification",
        "notify:OnShipDeath",
        "notify:OnMultiEvent",
    ]);

    let db_path = temp_db("insurance_pochven");
    let base_time = common::TEST_CLOCK_BASE_UNIX;
    let insured_keep_ship: i64;

    {
        let ctx = open_ctx(&db_path, base_time);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 5_000_000_000.0), (PILOT_2, 5_000_000_000.0)]).await;

        let mut p1 = login(&ctx, 51_101, "demo", PILOT_1).await;
        let mut p2 = login(&ctx, 51_102, "other", PILOT_2).await;

        // 1. Insurance void (repackage) & 12-week expiry (`feat:combat.insurance-void-and-expiry`)
        let hull_void = buy(&ctx, &mut p1, 587, 1).await[0];
        rpc(&ctx, &mut p1, "ship", "AssembleShip", &[PyRep::Integer(hull_void)]).await.unwrap();
        let price_info = rpc(&ctx, &mut p1, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(587)]).await.unwrap();
        assert!(price_info.as_f64().unwrap_or(1.0) > 0.0);
        rpc(&ctx, &mut p1, "insuranceSvc", "InsureShip", &[PyRep::Integer(hull_void), PyRep::Real(1.0), PyRep::Bool(false)]).await.unwrap();
        assert!(!matches!(rpc(&ctx, &mut p1, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(hull_void)]).await.unwrap(), PyRep::None));
        rpc(&ctx, &mut p1, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![PyRep::Integer(hull_void)])]).await.unwrap();
        assert_eq!(rpc(&ctx, &mut p1, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(hull_void)]).await.unwrap(), PyRep::None);

        let hull_expire = buy(&ctx, &mut p1, 587, 1).await[0];
        rpc(&ctx, &mut p1, "ship", "AssembleShip", &[PyRep::Integer(hull_expire)]).await.unwrap();
        rpc(&ctx, &mut p1, "insuranceSvc", "InsureShip", &[PyRep::Integer(hull_expire), PyRep::Real(1.0), PyRep::Bool(false)]).await.unwrap();
        ctx.advance_secs(12 * 7 * 86_400 + 5);
        assert_eq!(rpc(&ctx, &mut p1, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(hull_expire)]).await.unwrap(), PyRep::None);

        insured_keep_ship = buy(&ctx, &mut p1, 587, 1).await[0];
        rpc(&ctx, &mut p1, "ship", "AssembleShip", &[PyRep::Integer(insured_keep_ship)]).await.unwrap();
        rpc(&ctx, &mut p1, "insuranceSvc", "InsureShip", &[PyRep::Integer(insured_keep_ship), PyRep::Real(1.0), PyRep::Bool(false)]).await.unwrap();

        // 2. Pochven loot drop threshold + target OnMultiEvent + OnShipDeath + OnKillNotification
        assert_eq!(insurgency_pvp_loot_drop_threshold(&ctx, POCHVEN_SYS, PILOT_1, PILOT_2, false), 0.60);
        assert_eq!(insurgency_pvp_loot_drop_threshold(&ctx, JITA_SYS, PILOT_1, PILOT_2, false), 0.50);

        let attacker_ship = 950_200_001;
        let victim_ship = 950_200_002;
        seed_skills(&ctx, PILOT_1, &[(3300, 5), (3302, 5), (3328, 5)]);
        seed_item(&ctx, attacker_ship, 587, PILOT_1, JITA_STATION, InventoryFlag::Hangar, 1, true);
        let gun1 = 950_200_003;
        let gun2 = 950_200_004;
        let ammo1 = 950_200_005;
        seed_item(&ctx, gun1, 484, PILOT_1, attacker_ship, InventoryFlag::HiSlot0, 1, true);
        seed_item(&ctx, gun2, 484, PILOT_1, attacker_ship, InventoryFlag::HiSlot1, 1, true);
        seed_item(&ctx, ammo1, 184, PILOT_1, attacker_ship, InventoryFlag::Cargo, 400, false);
        rpc(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(attacker_ship)]).await.unwrap();

        seed_item(&ctx, victim_ship, 601, PILOT_2, JITA_STATION, InventoryFlag::Hangar, 1, true);
        seed_item(&ctx, 950_200_010, 34, PILOT_2, victim_ship, InventoryFlag::Cargo, 500, false);
        rpc(&ctx, &mut p2, "ship", "Board", &[PyRep::Integer(victim_ship)]).await.unwrap();
        ctx.advance_secs(12);

        // Undock both ships from Pochven NPC station into Pochven space for kill & loot threshold test
        let pochven_station = ctx
            .sde
            .stations
            .values()
            .find(|s| s.solar_system_id == POCHVEN_SYS)
            .map(|s| s.station_id)
            .expect("Pochven NPC station");
        ctx.db.with_mut_snapshot(|s| {
            for (cid, sid) in [(PILOT_1, attacker_ship), (PILOT_2, victim_ship)] {
                if let Some(ch) = s.characters.get_mut(&cid) {
                    ch.solar_system_id = POCHVEN_SYS;
                    ch.station_id = Some(pochven_station);
                    ch.active_ship_id = sid;
                }
                if let Some(ship) = s.items.get_mut(&sid) {
                    ship.location_id = pochven_station;
                    ship.flag = InventoryFlag::Hangar;
                }
            }
        });
        p1.solar_system_id = Some(POCHVEN_SYS);
        p1.station_id = Some(pochven_station);
        p1.ship_id = Some(attacker_ship);
        p2.solar_system_id = Some(POCHVEN_SYS);
        p2.station_id = Some(pochven_station);
        p2.ship_id = Some(victim_ship);
        rpc(&ctx, &mut p1, "stationSvc", "Undock", &[]).await.unwrap();
        rpc(&ctx, &mut p2, "stationSvc", "Undock", &[]).await.unwrap();
        let pochven = ctx.systems.get_or_spawn(POCHVEN_SYS);
        pochven.set_ball_position(attacker_ship, DVec3::new(0.0, 0.0, 0.0)).await;
        pochven.set_ball_position(victim_ship, DVec3::new(1_500.0, 0.0, 0.0)).await;
        for _ in 0..12 {
            let _ = pochven.step_manual(1.0).await;
        }

        let _ = ctx.drain_notifications_for_char(PILOT_1);
        let _ = ctx.drain_notifications_for_char(PILOT_2);
        rpc(&ctx, &mut p1, "dogmaIM", "LoadAmmo", &[
            PyRep::Integer(attacker_ship),
            PyRep::List(vec![PyRep::Integer(gun1), PyRep::Integer(gun2)]),
            PyRep::List(vec![PyRep::Integer(ammo1)]),
        ]).await.unwrap();
        lock(&ctx, POCHVEN_SYS, &mut p1, victim_ship).await;
        assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnMultiEvent"));

        for _ in 0..50 {
            if pochven.get_ball_snapshot(victim_ship).await.ok().flatten().is_none() {
                break;
            }
            for gid in [gun1, gun2] {
                let _ = rpc(&ctx, &mut p1, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(victim_ship)]).await;
            }
            for _ in 0..5 {
                let _ = pochven.step_manual(1.0).await;
            }
            ctx.advance_secs(5);
        }
        assert!(pochven.get_ball_snapshot(victim_ship).await.ok().flatten().is_none(), "victim ship destroyed in Pochven");
        let n1 = ctx.drain_notifications_for_char(PILOT_1);
        let n2 = ctx.drain_notifications_for_char(PILOT_2);
        assert!(n1.iter().chain(n2.iter()).any(|n| n.event == "OnKillNotification"));
        assert!(n2.iter().chain(n1.iter()).any(|n| n.event == "OnShipDeath"));
    }

    // Verify insurance contract on `insured_keep_ship` persists across SQLite restart
    let ctx2 = open_ctx(&db_path, base_time + 12 * 7 * 86_400 + 60);
    let mut p1_reload = login(&ctx2, 51_109, "demo", PILOT_1).await;
    assert!(!matches!(rpc(&ctx2, &mut p1_reload, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(insured_keep_ship)]).await.unwrap(), PyRep::None));
    let _ = std::fs::remove_file(&db_path);
}
