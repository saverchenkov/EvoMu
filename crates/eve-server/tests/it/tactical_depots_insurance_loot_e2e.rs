//! End-to-end integration tests for Shard 5 tactical modules, mobile depots,
//! insurance lifecycle, Pochven PvP loot drops, and in-space destruction notifications.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{buy, fund_pilots, login, move_item, rpc, train_skills, JITA_STATION, PILOT_1, PILOT_2};
use crate::pve_e2e_common::{bind_service, move_ship_to_ball, undock_from_station, wait_for_warp_end, warp_and_dock_at_station};
use eve_db::{InventoryFlag, VirtualServerClock, WorldRepository};
use eve_destiny::DVec3;
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables as depots;
use eve_server::services::security_mgr::insurgency_pvp_loot_drop_threshold;
use eve_server::world_seeder::seed_full_market_catalog;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(dead_code)]
const JITA_SYS: u32 = 30_000_142;
#[allow(dead_code)]
const NULL_SYS: u32 = 30_002_423;
#[allow(dead_code)]
const NULL_STATION: i64 = 60_012_730;
#[allow(dead_code)]
const CYNO_MID_SYS: u32 = 30_000_191;
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
    if !path.exists() {
        WorldRepository::with_demo_seed()
            .save_to_sqlite(path)
            .expect("save_to_sqlite");
    }
    let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

fn shared_combat_world() -> &'static (Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot) {
    static WORLD: std::sync::OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> =
        std::sync::OnceLock::new();
    WORLD.get_or_init(|| {
        let sde = Arc::new(
            eve_sde::SdeRegistry::open(eve_sde::SdeRegistry::default_db_path()).expect("open SDE"),
        );
        let db = WorldRepository::with_demo_seed();
        seed_full_market_catalog(&db, &sde);
        let keep_types: HashSet<u32> = [
            44_992, 530, 533, 377, 629, 10_836, 641, 3041, 246, 33_474, 28_668, 77_283, 4_292,
            77_399, 16_275, 16_274, 11_567, 40_631, 19_724, 17_887, 11_957, 21_096, 16_273, 587,
            484, 184, 56_070, 601, 34,
        ]
        .into_iter()
        .collect();
        let snap = db.with_mut_snapshot(|s| {
            s.market_orders.retain(|_, o| {
                keep_types.contains(&o.type_id)
                    || sde.types.get(&o.type_id).is_some_and(|t| t.category_id == 16)
            });
            s.clone()
        });
        (sde, snap)
    })
}

#[allow(dead_code)]
fn open_mem_ctx(now_unix: u64) -> Arc<ServerContext> {
    let (sde, base_snap) = shared_combat_world();
    let db = Arc::new(WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = base_snap.clone());
    let ctx = Arc::new(
        ServerContext::with_sde_and_repository(sde.clone(), db)
            .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

#[allow(dead_code)]
async fn create_funded_pilot(ctx: &ServerContext, sess_id: u64, user: &str, name: &str, isk: f64) -> (SessionState, i64) {
    let pass = format!("{user}-secret");
    if !ctx.db.with_snapshot(|s| s.accounts.contains_key(user)) {
        ctx.db.provision_player_account(user, &pass);
    }
    let mut sess = SessionState::new(sess_id);
    sess.address = "127.0.0.1:26000".into();
    common::login_pilot(ctx, &mut sess, user, &pass).await;
    let doll = PyRep::keyval([("costume", PyRep::Integer(301)), ("gender", PyRep::Integer(1)), ("hair", PyRep::Integer(101)), ("skin", PyRep::Integer(201))]);
    let portrait = PyRep::keyval([("background", PyRep::Integer(42)), ("camera", PyRep::Integer(1)), ("pose", PyRep::Integer(7))]);
    let anc = ctx.sde.ancestries.values().find(|a| a.bloodline_id == 1).map(|a| i64::from(a.ancestry_id)).unwrap_or(1);
    let sch = ctx.sde.schools.values().filter(|s| s.race_id == 1 && s.solar_system_id == JITA_SYS).min_by_key(|s| s.school_id).or_else(|| ctx.sde.schools.values().filter(|s| s.race_id == 1).min_by_key(|s| s.school_id)).map(|s| i64::from(s.school_id)).unwrap_or(17);
    let char_id = rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String(name.into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(anc),
            doll,
            portrait,
            PyRep::Integer(sch),
            PyRep::None,
            PyRep::Integer(i64::from(JITA_SYS)),
        ],
    )
    .await
    .unwrap()
    .as_i64()
    .unwrap();
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.unwrap();
    let ch = ctx.db.get_character(char_id).unwrap();
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(ch.corporation_id);
    sess.solar_system_id = Some(ch.solar_system_id);
    sess.station_id = ch.station_id;
    sess.region_id = ctx.sde.solar_systems.get(&ch.solar_system_id).map(|s| s.region_id);
    sess.ship_id = Some(ch.active_ship_id);
    fund_pilots(ctx, &[(char_id, isk)]).await;
    fast_dock_station(ctx, &mut sess, JITA_STATION).await;
    (sess, char_id)
}

#[allow(dead_code)]
async fn upgrade_omega_via_market(ctx: &ServerContext, sess: &mut SessionState, months: i64) {
    // Market PLEX purchases (type 44_992) settle directly into the account PLEX Vault.
    let total_plex = 500 * months;
    let _ = buy(ctx, sess, 44_992, total_plex).await;
    rpc(
        ctx,
        sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1001), PyRep::String("PLX".into()), PyRep::Integer(months)],
    )
    .await
    .expect("storeManager.buy_offer Omega");
}

#[allow(dead_code)]
async fn fast_warp_to(ctx: &ServerContext, sess: &mut SessionState, target: i64, dist: f64) {
    let sys_id = sess.solar_system_id.unwrap();
    let ship_id = sess.ship_id.unwrap();
    let _ = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(sys_id))).await;
    match rpc(
        ctx,
        sess,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(target), PyRep::Real(dist)],
    )
    .await
    {
        Ok(_) => wait_for_warp_end(ctx, sys_id, ship_id).await,
        Err(e) if e.code == "WarpDistanceTooShort" => {}
        Err(e) => panic!("warp to {target}: {e:?}"),
    }
    ctx.advance_secs(15);
}

#[allow(dead_code)]
async fn fast_fly_stargates_to(ctx: &ServerContext, sess: &mut SessionState, dest_sys: u32) {
    let start_sys = sess.solar_system_id.unwrap_or(JITA_SYS);
    if start_sys == dest_sys {
        return;
    }
    if sess.station_id.is_some() {
        undock_from_station(ctx, sess, start_sys).await;
        ctx.advance_secs(11);
    }
    let ship_id = sess.ship_id.expect("active ship in fast_fly_stargates_to");
    let (mut visited, mut prev, mut queue) = (
        HashSet::from([start_sys]),
        HashMap::new(),
        VecDeque::from([start_sys]),
    );
    while let Some(cur) = queue.pop_front() {
        if cur == dest_sys {
            break;
        }
        for j in ctx.sde.jumps.values().filter(|j| {
            j.from_solar_system_id == cur && (j.to_solar_system_id != 30_100_000 || dest_sys == 30_100_000)
        }) {
            if visited.insert(j.to_solar_system_id) {
                prev.insert(j.to_solar_system_id, (cur, j.stargate_id));
                queue.push_back(j.to_solar_system_id);
            }
        }
    }
    let (mut hops, mut trace) = (Vec::new(), dest_sys);
    while trace != start_sys {
        let (p_sys, fg) = prev[&trace];
        hops.push((p_sys, fg, trace));
        trace = p_sys;
    }
    hops.reverse();
    let empty = HashMap::new();
    for (cur_sys, fg, next_sys) in hops {
        let b_oid = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(cur_sys))).await as u64;
        let sys_h = ctx.systems.get_or_spawn(cur_sys);
        match ctx
            .dispatch_rpc(
                sess,
                "beyonce",
                Some(b_oid),
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(fg), PyRep::Real(0.0)],
                &empty,
                None,
            )
            .await
        {
            Ok(_) => {
                let _ = sys_h.step_manual(120.0).await;
                if matches!(
                    sys_h.get_ball_snapshot(ship_id).await.ok().flatten(),
                    Some((_, b)) if matches!(b.mode, eve_destiny::MovementMode::Warp { .. })
                ) {
                    wait_for_warp_end(ctx, cur_sys, ship_id).await;
                }
            }
            Err(e) if e.code == "ShipInvulnerable" => {
                let _ = sys_h.step_manual(65.0).await;
                move_ship_to_ball(ctx, sess, cur_sys, ship_id, "item", PyRep::Integer(fg)).await;
            }
            Err(e) if e.code == "WarpDistanceTooShort" => {}
            Err(_) => {
                move_ship_to_ball(ctx, sess, cur_sys, ship_id, "item", PyRep::Integer(fg)).await;
            }
        }
        ctx.advance_secs(11);
        let jump_args = [PyRep::Integer(fg), PyRep::Integer(i64::from(next_sys))];
        if let Err(e) = rpc(ctx, sess, "beyonce", "CmdStargateJump", &jump_args).await {
            if e.code == "OutsideStargateJumpRange" || e.code == "ShipInWarp" {
                move_ship_to_ball(ctx, sess, cur_sys, ship_id, "item", PyRep::Integer(fg)).await;
                ctx.advance_secs(11);
                rpc(ctx, sess, "beyonce", "CmdStargateJump", &jump_args)
                    .await
                    .unwrap_or_else(|e2| panic!("CmdStargateJump retry {fg} -> {next_sys}: {e2:?}"));
            } else {
                panic!("CmdStargateJump {fg} -> {next_sys}: {e:?}");
            }
        }
        sess.solar_system_id = Some(next_sys);
    }
    sess.solar_system_id = Some(dest_sys);
    sess.station_id = None;
    sess.region_id = ctx.sde.solar_systems.get(&dest_sys).map(|s| s.region_id);
    let _ = bind_service(ctx, sess, "beyonce", PyRep::Integer(i64::from(dest_sys))).await;
}

#[allow(dead_code)]
async fn fast_travel(ctx: &ServerContext, sess: &mut SessionState, dest_sys: u32) {
    if sess.solar_system_id == Some(dest_sys) {
        if sess.station_id.is_some() {
            undock_from_station(ctx, sess, dest_sys).await;
        }
    } else {
        fast_fly_stargates_to(ctx, sess, dest_sys).await;
    }
}

#[allow(dead_code)]
async fn fast_dock_station(ctx: &ServerContext, sess: &mut SessionState, station_id: i64) {
    if sess.station_id == Some(station_id) {
        return;
    }
    let dest_sys = ctx.sde.stations[&station_id].solar_system_id;
    fast_travel(ctx, sess, dest_sys).await;
    let ship_id = sess.ship_id.unwrap();
    warp_and_dock_at_station(ctx, sess, ship_id, dest_sys, station_id).await;
    ctx.advance_secs(15);
}

#[allow(dead_code)]
async fn travel_pochven_avoiding_home(ctx: &ServerContext, sess: &mut SessionState, dest_sys: u32) {
    let start_sys = sess.solar_system_id.unwrap();
    if start_sys == dest_sys {
        return;
    }
    let mut prev: HashMap<u32, (u32, i64)> = HashMap::new();
    let mut q = VecDeque::from([start_sys]);
    let mut seen = HashSet::from([start_sys]);
    while let Some(sys) = q.pop_front() {
        if sys == dest_sys {
            break;
        }
        let mut gates: Vec<(&i64, &eve_sde::StargateJump)> = ctx
            .sde
            .jumps
            .iter()
            .filter(|(_, j)| j.from_solar_system_id == sys && !matches!(j.to_solar_system_id, 30_001_372 | 30_003_504 | 30_002_702))
            .collect();
        gates.sort_by_key(|g| *g.0);
        for (&gid, j) in gates {
            if seen.insert(j.to_solar_system_id) {
                prev.insert(j.to_solar_system_id, (sys, gid));
                q.push_back(j.to_solar_system_id);
            }
        }
    }
    let mut path = Vec::new();
    let mut cur = dest_sys;
    while cur != start_sys {
        let (p, gid) = prev[&cur];
        path.push((p, gid, cur));
        cur = p;
    }
    path.reverse();
    let ship_id = sess.ship_id.unwrap();
    for (cur_sys, gid, next_sys) in path {
        move_ship_to_ball(ctx, sess, cur_sys, ship_id, "item", PyRep::Integer(gid)).await;
        ctx.advance_secs(12);
        let _ = ctx.systems.get_or_spawn(cur_sys).step_manual(12.0).await;
        rpc(
            ctx,
            sess,
            "beyonce",
            "CmdStargateJump",
            &[PyRep::Integer(gid), PyRep::Integer(i64::from(next_sys))],
        )
        .await
        .unwrap_or_else(|e| panic!("CmdStargateJump {gid} -> {next_sys}: {e:?}"));
        ctx.advance_secs(12);
    }
    assert_eq!(sess.solar_system_id, Some(dest_sys));
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

#[allow(dead_code)]
async fn warp_and_approach_fleet_ship(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    target_char_id: i64,
    target_ship_id: i64,
) {
    match rpc(ctx, sess, "beyonce", "CmdWarpToStuff", &[PyRep::String("char".into()), PyRep::Integer(target_char_id)]).await {
        Ok(_) => wait_for_warp_end(ctx, sys_id, ship_id).await,
        Err(e) if e.code == "WarpDistanceTooShort" => {}
        Err(e) => panic!("CmdWarpToStuff char {target_char_id}: {e:?}"),
    }
    move_ship_to_ball(ctx, sess, sys_id, ship_id, "item", PyRep::Integer(target_ship_id)).await;
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
    let titan_iso_expected: i64;
    let megathron_ship_id: i64;
    let laser_id: i64;
    let reload_time: u64;

    {
        let ctx = open_mem_ctx(base_time);
        ctx.set_mechanic_rng_seed(Some(2026));
        fund_pilots(&ctx, &[(PILOT_1, 800_000_000_000.0)]).await;
        fund_pilots(&ctx, &[(PILOT_1, 800_000_000_000.0)]).await;
        fund_pilots(&ctx, &[(PILOT_2, 500_000_000_000.0)]).await;

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
        fast_travel(&ctx, &mut p1, JITA_SYS).await;
        fast_warp_to(&ctx, &mut p1, sun, 100_000.0).await;
        fast_travel(&ctx, &mut p2, JITA_SYS).await;
        fast_warp_to(&ctx, &mut p2, sun, 100_000.0).await;
        ctx.advance_secs(15);
        let jita = ctx.systems.get_or_spawn(JITA_SYS);
        for _ in 0..15 {
            let _ = jita.step_manual(1.0).await;
        }
        rpc(&ctx, &mut p1, "dogmaIM", "AddTarget", &[PyRep::Integer(rupture)]).await.unwrap();
        let _ = jita.step_manual(12.0).await;

        // Rupture burns its cap down below 58% (~500 GJ) > Rifter max (250 GJ), and 1 neut cycle gives Rifter empty cap room (> 8 GJ)
        rpc(&ctx, &mut p2, "dogmaIM", "Activate", &[PyRep::Integer(msb), PyRep::String("shieldBoosting".into())]).await.unwrap();
        for _ in 0..100 {
            let _ = jita.step_manual(3.0).await;
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
        fast_dock_station(&ctx, &mut p1, JITA_STATION).await;
        fast_dock_station(&ctx, &mut p2, JITA_STATION).await;

        let (mut cyno_mid_sess, cyno_mid_cid) = create_funded_pilot(&ctx, 51_055, "cyno_mid", "Cyno Mid", 500_000_000_000.0).await;
        let (mut cyno_null_sess, cyno_null_cid) = create_funded_pilot(&ctx, 51_056, "cyno_null", "Cyno Null", 500_000_000_000.0).await;
        let (mut lancer_sess, lancer_cid) = create_funded_pilot(&ctx, 51_057, "lancer_alt", "Lancer Pilot", 600_000_000_000.0).await;
        let (mut titan_sess, titan_cid) = create_funded_pilot(&ctx, 51_058, "titan_alt", "Titan Pilot", 800_000_000_000.0).await;

        // PILOT_1 starts as permanent Omega in demo_seed.
        train_skills(
            &ctx,
            &mut p1,
            &[641, 3041, 12_565, 28_668, 33_474, 377, 28_164, 28_879, 28_880, 3_336, 3_423, 3_347, 21_803],
            true,
        )
        .await;
        // Upgrade Alpha pilots via Jita PLEX market right before their own skill training
        // so prior train_skills clock advances do not expire their Omega time before their skills queue.
        upgrade_omega_via_market(&ctx, &mut cyno_mid_sess, 120).await;
        train_skills(&ctx, &mut cyno_mid_sess, &[11_957, 21_096], true).await;
        upgrade_omega_via_market(&ctx, &mut cyno_null_sess, 120).await;
        train_skills(&ctx, &mut cyno_null_sess, &[11_957, 21_096], true).await;
        upgrade_omega_via_market(&ctx, &mut lancer_sess, 120).await;
        train_skills(&ctx, &mut lancer_sess, &[77_283, 4_292, 77_399, 3_456, 21_611], true).await;
        upgrade_omega_via_market(&ctx, &mut titan_sess, 120).await;
        train_skills(&ctx, &mut titan_sess, &[11_567, 40_631, 3_456, 21_611], true).await;
        upgrade_omega_via_market(&ctx, &mut p2, 120).await;
        train_skills(&ctx, &mut p2, &[19_724, 3_456, 21_611], true).await;
        // Refresh Alpha->Omega window after sequential skill-queue clock advances.
        upgrade_omega_via_market(&ctx, &mut cyno_mid_sess, 2).await;
        upgrade_omega_via_market(&ctx, &mut cyno_null_sess, 2).await;
        upgrade_omega_via_market(&ctx, &mut lancer_sess, 2).await;
        upgrade_omega_via_market(&ctx, &mut titan_sess, 2).await;
        upgrade_omega_via_market(&ctx, &mut p2, 2).await;

        // Purchase & fit Megathron for PILOT_1, Bane for lancer_alt, Avatar for titan_alt in Jita
        megathron_ship_id = buy(&ctx, &mut p1, 641, 1).await[0];
        rpc(&ctx, &mut p1, "ship", "AssembleShip", &[PyRep::Integer(megathron_ship_id)]).await.unwrap();
        let mut mega_lasers = Vec::new();
        let mut mega_crystals = Vec::new();
        let depot_item = buy(&ctx, &mut p1, 33_474, 1).await[0];
        let ext_item = buy(&ctx, &mut p1, 377, 1).await[0];
        refit_mod_id = ext_item;
        let paste_id = buy(&ctx, &mut p1, 28_668, 40).await[0];

        let lancer_ship_id = buy(&ctx, &mut lancer_sess, 77_283, 1).await[0];
        rpc(&ctx, &mut lancer_sess, "ship", "AssembleShip", &[PyRep::Integer(lancer_ship_id)]).await.unwrap();
        let siege_id = buy(&ctx, &mut lancer_sess, 4_292, 1).await[0];
        let dlance_id = buy(&ctx, &mut lancer_sess, 77_399, 1).await[0];
        let bane_stront = buy(&ctx, &mut lancer_sess, 16_275, 250).await[0];
        let bane_iso = buy(&ctx, &mut lancer_sess, 16_274, 70_000).await[0];
        rpc(&ctx, &mut lancer_sess, "ship", "Board", &[PyRep::Integer(lancer_ship_id)]).await.unwrap();
        ctx.advance_secs(12);
        move_item(&ctx, &mut lancer_sess, siege_id, lancer_ship_id, InventoryFlag::HiSlot0 as i64).await;
        move_item(&ctx, &mut lancer_sess, dlance_id, lancer_ship_id, InventoryFlag::HiSlot1 as i64).await;
        move_item(&ctx, &mut lancer_sess, bane_stront, lancer_ship_id, InventoryFlag::Cargo as i64).await;
        move_item(&ctx, &mut lancer_sess, bane_iso, lancer_ship_id, InventoryFlag::SpecializedFuelBay as i64).await;

        titan_ship_id = buy(&ctx, &mut titan_sess, 11_567, 1).await[0];
        rpc(&ctx, &mut titan_sess, "ship", "AssembleShip", &[PyRep::Integer(titan_ship_id)]).await.unwrap();
        let doomsday_id = buy(&ctx, &mut titan_sess, 40_631, 1).await[0];
        let titan_jump_iso = buy(&ctx, &mut titan_sess, 16_274, 40_000).await[0];
        let titan_lance_iso = buy(&ctx, &mut titan_sess, 16_274, 100_000).await[0];
        rpc(&ctx, &mut titan_sess, "ship", "Board", &[PyRep::Integer(titan_ship_id)]).await.unwrap();
        ctx.advance_secs(12);
        move_item(&ctx, &mut titan_sess, doomsday_id, titan_ship_id, InventoryFlag::HiSlot0 as i64).await;
        move_item(&ctx, &mut titan_sess, titan_jump_iso, titan_ship_id, InventoryFlag::SpecializedFuelBay as i64).await;
        move_item(&ctx, &mut titan_sess, titan_lance_iso, titan_ship_id, InventoryFlag::Cargo as i64).await;

        rpc(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(megathron_ship_id)]).await.unwrap();
        ctx.advance_secs(12);
        for slot in [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2, InventoryFlag::HiSlot3] {
            let l_id = buy(&ctx, &mut p1, 3041, 1).await[0];
            move_item(&ctx, &mut p1, l_id, megathron_ship_id, slot as i64).await;
            mega_lasers.push(l_id);
            let c_id = buy(&ctx, &mut p1, 246, 1).await[0];
            move_item(&ctx, &mut p1, c_id, megathron_ship_id, InventoryFlag::Cargo as i64).await;
            mega_crystals.push(c_id);
        }
        laser_id = mega_lasers[0];
        move_item(&ctx, &mut p1, depot_item, megathron_ship_id, InventoryFlag::Cargo as i64).await;
        move_item(&ctx, &mut p1, ext_item, megathron_ship_id, InventoryFlag::Cargo as i64).await;
        move_item(&ctx, &mut p1, paste_id, megathron_ship_id, InventoryFlag::Cargo as i64).await;
        rpc(&ctx, &mut p1, "dogmaIM", "LoadAmmo", &[
            PyRep::Integer(megathron_ship_id),
            PyRep::List(mega_lasers.iter().map(|&id| PyRep::Integer(id)).collect()),
            PyRep::List(mega_crystals.iter().map(|&id| PyRep::Integer(id)).collect()),
        ]).await.unwrap();

        // Purchase & fit Moros for PILOT_2 in Jita
        let p2_stab = buy(&ctx, &mut p2, 19_724, 1).await[0];
        rpc(&ctx, &mut p2, "ship", "AssembleShip", &[PyRep::Integer(p2_stab)]).await.unwrap();
        let moros_iso = buy(&ctx, &mut p2, 17_887, 40_000).await[0];
        rpc(&ctx, &mut p2, "ship", "Board", &[PyRep::Integer(p2_stab)]).await.unwrap();
        ctx.advance_secs(12);
        move_item(&ctx, &mut p2, moros_iso, p2_stab, InventoryFlag::SpecializedFuelBay as i64).await;

        // Purchase & fit Falcon cyno recons for cyno_mid & cyno_null in Jita
        let falcon_mid_id = buy(&ctx, &mut cyno_mid_sess, 11_957, 1).await[0];
        rpc(&ctx, &mut cyno_mid_sess, "ship", "AssembleShip", &[PyRep::Integer(falcon_mid_id)]).await.unwrap();
        let cyno_mid_mod = buy(&ctx, &mut cyno_mid_sess, 21_096, 1).await[0];
        let ozone_mid = buy(&ctx, &mut cyno_mid_sess, 16_273, 700).await[0];
        rpc(&ctx, &mut cyno_mid_sess, "ship", "Board", &[PyRep::Integer(falcon_mid_id)]).await.unwrap();
        ctx.advance_secs(12);
        move_item(&ctx, &mut cyno_mid_sess, cyno_mid_mod, falcon_mid_id, InventoryFlag::HiSlot0 as i64).await;
        move_item(&ctx, &mut cyno_mid_sess, ozone_mid, falcon_mid_id, InventoryFlag::Cargo as i64).await;

        let falcon_null_id = buy(&ctx, &mut cyno_null_sess, 11_957, 1).await[0];
        rpc(&ctx, &mut cyno_null_sess, "ship", "AssembleShip", &[PyRep::Integer(falcon_null_id)]).await.unwrap();
        let cyno_null_mod = buy(&ctx, &mut cyno_null_sess, 21_096, 1).await[0];
        let ozone_null = buy(&ctx, &mut cyno_null_sess, 16_273, 700).await[0];
        rpc(&ctx, &mut cyno_null_sess, "ship", "Board", &[PyRep::Integer(falcon_null_id)]).await.unwrap();
        ctx.advance_secs(12);
        move_item(&ctx, &mut cyno_null_sess, cyno_null_mod, falcon_null_id, InventoryFlag::HiSlot0 as i64).await;
        move_item(&ctx, &mut cyno_null_sess, ozone_null, falcon_null_id, InventoryFlag::Cargo as i64).await;

        // Form fleet (PILOT_1 leader, PILOT_2, cyno_mid, cyno_null, lancer_alt, titan_alt)
        rpc(&ctx, &mut p1, "fleetMgr", "CreateFleet", &[]).await.unwrap();
        rpc(&ctx, &mut p1, "fleetMgr", "Invite", &[PyRep::Integer(PILOT_2), PyRep::Integer(4)]).await.unwrap();
        rpc(&ctx, &mut p2, "fleetMgr", "AcceptInvite", &[]).await.unwrap();
        rpc(&ctx, &mut p1, "fleetMgr", "Invite", &[PyRep::Integer(cyno_mid_cid), PyRep::Integer(4)]).await.unwrap();
        rpc(&ctx, &mut cyno_mid_sess, "fleetMgr", "AcceptInvite", &[]).await.unwrap();
        rpc(&ctx, &mut p1, "fleetMgr", "Invite", &[PyRep::Integer(cyno_null_cid), PyRep::Integer(4)]).await.unwrap();
        rpc(&ctx, &mut cyno_null_sess, "fleetMgr", "AcceptInvite", &[]).await.unwrap();
        rpc(&ctx, &mut p1, "fleetMgr", "Invite", &[PyRep::Integer(lancer_cid), PyRep::Integer(4)]).await.unwrap();
        rpc(&ctx, &mut lancer_sess, "fleetMgr", "AcceptInvite", &[]).await.unwrap();
        rpc(&ctx, &mut p1, "fleetMgr", "Invite", &[PyRep::Integer(titan_cid), PyRep::Integer(4)]).await.unwrap();
        rpc(&ctx, &mut titan_sess, "fleetMgr", "AcceptInvite", &[]).await.unwrap();

        let (null_sys_id, null_station_id) = (NULL_SYS, NULL_STATION);
        let null_sun = ctx.sde.celestials_in_system(null_sys_id).into_iter().find(|c| c.group_id == 6).expect("FDZ4-A sun").celestial_id;
        let null_sys = ctx.systems.get_or_spawn(null_sys_id);

        // PILOT_1 flies Megathron along stargates to FDZ4-A, docks, and stashes Nanite Paste in hangar first
        fast_fly_stargates_to(&ctx, &mut p1, null_sys_id).await;
        warp_and_dock_at_station(&ctx, &mut p1, megathron_ship_id, null_sys_id, null_station_id).await;
        move_item(&ctx, &mut p1, paste_id, null_station_id, InventoryFlag::Hangar as i64).await;

        // Station cyno_mid at Walvalin midpoint and cyno_null at FDZ4-A once; Moros jumps Jita -> Walvalin -> FDZ4-A
        fast_fly_stargates_to(&ctx, &mut cyno_mid_sess, CYNO_MID_SYS).await;
        fast_fly_stargates_to(&ctx, &mut cyno_null_sess, null_sys_id).await;
        fast_warp_to(&ctx, &mut cyno_null_sess, null_sun, 100_000.0).await;
        ctx.advance_secs(12);
        let _ = ctx.systems.get_or_spawn(CYNO_MID_SYS).step_manual(12.0).await;
        let cyno1 = rpc(&ctx, &mut cyno_mid_sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_mid_mod), PyRep::String("cynosuralGeneration".into())]).await.unwrap();
        let beacon1 = cyno1.dict_get("beaconID").and_then(|v| v.as_i64()).expect("cyno1 beaconID");

        undock_from_station(&ctx, &mut p2, JITA_SYS).await;
        ctx.advance_secs(15);
        let _ = ctx.systems.get_or_spawn(JITA_SYS).step_manual(15.0).await;
        rpc(&ctx, &mut p2, "beyonce", "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_mid_cid), PyRep::Integer(i64::from(CYNO_MID_SYS)), PyRep::Integer(beacon1)]).await.unwrap();
        let _ = rpc(&ctx, &mut cyno_mid_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(cyno_mid_mod)]).await;
        for _ in 0..16 {
            let _ = ctx.systems.get_or_spawn(CYNO_MID_SYS).step_manual(60.0).await;
        }
        // Clear max jump reactivation cooldown cap (30 min = 1800s) before 2nd jump.
        ctx.advance_secs(1800);

        ctx.advance_secs(12);
        let _ = null_sys.step_manual(12.0).await;
        let cyno2 = rpc(&ctx, &mut cyno_null_sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_null_mod), PyRep::String("cynosuralGeneration".into())]).await.unwrap();
        let beacon2 = cyno2.dict_get("beaconID").and_then(|v| v.as_i64()).expect("cyno2 beaconID");
        rpc(&ctx, &mut p2, "beyonce", "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_null_cid), PyRep::Integer(i64::from(null_sys_id)), PyRep::Integer(beacon2)]).await.unwrap();
        warp_and_dock_at_station(&ctx, &mut p2, p2_stab, null_sys_id, null_station_id).await;
        let _ = rpc(&ctx, &mut cyno_null_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(cyno_null_mod)]).await;
        for _ in 0..20 {
            let _ = null_sys.step_manual(60.0).await;
        }
        ctx.advance_secs(1200);

        // Undock Megathron (no paste in cargo) and Moros in FDZ4-A near the sun
        undock_from_station(&ctx, &mut p1, null_sys_id).await;
        undock_from_station(&ctx, &mut p2, null_sys_id).await;
        fast_warp_to(&ctx, &mut p2, null_sun, 100_000.0).await;
        warp_and_approach_fleet_ship(&ctx, &mut p1, null_sys_id, megathron_ship_id, PILOT_2, p2_stab).await;
        ctx.advance_secs(15);
        for _ in 0..15 {
            let _ = null_sys.step_manual(1.0).await;
        }
        lock(&ctx, null_sys_id, &mut p1, p2_stab).await;

        // Overheat lasers legitimately via dogmaIM.Overload until heat damage accumulates
        for &l_id in &mega_lasers {
            rpc(&ctx, &mut p1, "dogmaIM", "Overload", &[PyRep::Integer(l_id), PyRep::String("targetAttack".into()), PyRep::Integer(p2_stab), PyRep::Integer(200)]).await.unwrap();
        }
        let mut heat_dmg = 0.0_f64;
        for _ in 0..240 {
            null_sys.try_step_full_tick(1.0);
            ctx.advance_secs(1);
            heat_dmg = ctx.db.with_snapshot(|s| {
                s.items.get(&laser_id).and_then(|i| i.dynamic_attributes.get(&eve_dogma::attr_ids::DAMAGE).copied()).unwrap_or(0.0)
            });
            if heat_dmg >= 1.0 {
                break;
            }
        }
        let rack_heat = ctx.db.with_snapshot(|s| {
            s.transient_item_attributes.get(&megathron_ship_id).and_then(|m| m.get(&1175).copied()).unwrap_or(0.0)
        });
        assert!(heat_dmg >= 1.0, "Overheating pulse laser must inflict real heat damage (got {heat_dmg}, hi_rack_heat={rack_heat})");
        for &l_id in &mega_lasers {
            let _ = rpc(&ctx, &mut p1, "dogmaIM", "Deactivate", &[PyRep::Integer(l_id), PyRep::String("targetAttack".into())]).await;
        }
        for _ in 0..8 {
            null_sys.try_step_full_tick(1.0);
        }

        let no_paste = rpc(&ctx, &mut p1, "dogmaIM", "RepairModule", &[PyRep::Integer(laser_id)]).await.unwrap_err();
        assert_eq!(no_paste.code, "NotEnoughNanitePaste");

        // Dock at FDZ4-A station, retrieve Nanite Repair Paste into Megathron cargo, undock and repair in space
        ctx.advance_secs(60);
        warp_and_dock_at_station(&ctx, &mut p1, megathron_ship_id, null_sys_id, null_station_id).await;
        move_item(&ctx, &mut p1, paste_id, megathron_ship_id, InventoryFlag::Cargo as i64).await;
        undock_from_station(&ctx, &mut p1, null_sys_id).await;
        ctx.advance_secs(12);
        let _ = null_sys.step_manual(12.0).await;

        let rep = rpc(&ctx, &mut p1, "dogmaIM", "RepairModule", &[PyRep::Integer(laser_id)]).await.unwrap();
        assert_eq!(rep.dict_get("pasteConsumed"), Some(&PyRep::Integer(1)));
        let mut dmg_now = heat_dmg;
        for _ in 0..6 {
            null_sys.try_step_full_tick(1.0);
            dmg_now = ctx.db.with_snapshot(|s| {
                s.items.get(&laser_id).and_then(|i| i.dynamic_attributes.get(&eve_dogma::attr_ids::DAMAGE).copied()).unwrap_or(0.0)
            });
            if dmg_now < heat_dmg {
                break;
            }
        }
        assert!(dmg_now < heat_dmg, "Nanite Repair Paste tick must reduce real module heat damage ({dmg_now} vs {heat_dmg})");
        rpc(&ctx, &mut p1, "dogmaIM", "StopRepairModule", &[PyRep::Integer(laser_id)]).await.unwrap();
        if dmg_now > 0.05 {
            rpc(&ctx, &mut p1, "dogmaIM", "RepairModule", &[PyRep::Integer(laser_id)]).await.unwrap();
            rpc(&ctx, &mut p1, "dogmaIM", "StopRepairModule", &[PyRep::Integer(laser_id)]).await.unwrap();
        }

        // 3. Mobile Depot deployment & in-space refitting (`feat:modules.mobile-depot-deployment-and-refitting`)
        let null_gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == null_sys_id).expect("FDZ4-A stargate").stargate_id;
        fast_warp_to(&ctx, &mut p1, null_gate, 100_000.0).await;
        let mega_pos = null_sys.get_ball_snapshot(megathron_ship_id).await.unwrap().unwrap().1.position;
        let dep_rep = rpc(&ctx, &mut p1, "posMgr", "AnchorStructure", &[PyRep::Integer(depot_item), PyRep::Tuple(vec![PyRep::Real(mega_pos.x + 400.0), PyRep::Real(mega_pos.y), PyRep::Real(mega_pos.z)])]).await.unwrap();
        dep_id = dep_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        ctx.advance_secs(65);
        let _ = null_sys.step_manual(1.0).await;
        rpc(&ctx, &mut p1, "invbroker", "Add", &[PyRep::Integer(refit_mod_id), PyRep::Integer(megathron_ship_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)]).await.unwrap();
        let st = ctx.db.with_snapshot(|s| depots::read_deployable_state_snap(s).deployables.get(&dep_id).cloned()).unwrap();
        assert!(st.anchor_completed);

        // 4. Disruptive Lance & Directional Lance Doomsday (`feat:modules.disruptive-lance`, `feat:modules.lance-doomsday-directional-geometry`)
        // Dock Megathron at FDZ4-A; lancer_alt undocks Bane in Jita and cyno-jumps via Walvalin to FDZ4-A.
        warp_and_dock_at_station(&ctx, &mut p1, megathron_ship_id, null_sys_id, null_station_id).await;

        ctx.advance_secs(12);
        let _ = ctx.systems.get_or_spawn(CYNO_MID_SYS).step_manual(12.0).await;
        let cyno3 = rpc(&ctx, &mut cyno_mid_sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_mid_mod), PyRep::String("cynosuralGeneration".into())]).await.unwrap();
        let beacon3 = cyno3.dict_get("beaconID").and_then(|v| v.as_i64()).expect("cyno3 beaconID");
        undock_from_station(&ctx, &mut lancer_sess, JITA_SYS).await;
        ctx.advance_secs(15);
        let _ = ctx.systems.get_or_spawn(JITA_SYS).step_manual(15.0).await;
        rpc(&ctx, &mut lancer_sess, "beyonce", "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_mid_cid), PyRep::Integer(i64::from(CYNO_MID_SYS)), PyRep::Integer(beacon3)]).await.unwrap();
        let _ = rpc(&ctx, &mut cyno_mid_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(cyno_mid_mod)]).await;
        for _ in 0..16 {
            let _ = ctx.systems.get_or_spawn(CYNO_MID_SYS).step_manual(60.0).await;
        }
        // Clear max jump reactivation cooldown cap (30 min = 1800s) before Bane's 2nd jump.
        ctx.advance_secs(1800);

        warp_and_approach_fleet_ship(&ctx, &mut cyno_null_sess, null_sys_id, falcon_null_id, PILOT_2, p2_stab).await;
        ctx.advance_secs(12);
        let _ = null_sys.step_manual(12.0).await;
        let cyno4 = rpc(&ctx, &mut cyno_null_sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_null_mod), PyRep::String("cynosuralGeneration".into())]).await.unwrap();
        let beacon4 = cyno4.dict_get("beaconID").and_then(|v| v.as_i64()).expect("cyno4 beaconID");
        rpc(&ctx, &mut lancer_sess, "beyonce", "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_null_cid), PyRep::Integer(i64::from(null_sys_id)), PyRep::Integer(beacon4)]).await.unwrap();
        warp_and_approach_fleet_ship(&ctx, &mut lancer_sess, null_sys_id, lancer_ship_id, PILOT_2, p2_stab).await;
        let _ = rpc(&ctx, &mut cyno_null_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(cyno_null_mod)]).await;
        for _ in 0..20 {
            let _ = null_sys.step_manual(60.0).await;
        }
        ctx.advance_secs(1200);
        // Move Falcon out of Disruptive Lance collateral beam corridor (`2,500 m` / `10,000 m`) before firing (#697/#583).
        fast_warp_to(&ctx, &mut cyno_null_sess, null_gate, 100_000.0).await;

        let p2_beyonce = match rpc(&ctx, &mut p2, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(null_sys_id)), PyRep::None]).await.unwrap() {
            PyRep::Tuple(parts) => parts[0].as_i64().unwrap() as u64,
            _ => panic!("p2 beyonce bind tuple"),
        };
        let dl_shield0 = null_sys.get_combat_state(p2_stab).await.unwrap().unwrap().shield_hp;
        rpc(&ctx, &mut lancer_sess, "dogmaIM", "Activate", &[PyRep::Integer(siege_id)]).await.unwrap();
        let dl_res = rpc(&ctx, &mut lancer_sess, "dogmaIM", "Activate", &[PyRep::Integer(dlance_id), PyRep::String("default".into()), PyRep::Integer(p2_stab)]).await.unwrap();
        assert_eq!(dl_res.dict_get("hitCount"), Some(&PyRep::Integer(1)));
        assert_eq!(dl_res.dict_get("fuelConsumed"), Some(&PyRep::Integer(20_000)));
        assert!(null_sys.get_combat_state(p2_stab).await.unwrap().unwrap().shield_hp < dl_shield0);
        let dl_warp_err = ctx.dispatch_rpc(&mut p2, "beyonce", Some(p2_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(null_sun)], &HashMap::new(), None).await.unwrap_err();
        assert_eq!(dl_warp_err.code, "DisruptiveLanceDebuffActive");

        // Titan Directional Lance Doomsday via `superWeaponMgr.ActivateSlashModule`
        let _ = rpc(&ctx, &mut lancer_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(siege_id)]).await;
        ctx.advance_secs(310);
        let _ = null_sys.step_manual(310.0).await;
        warp_and_dock_at_station(&ctx, &mut lancer_sess, lancer_ship_id, null_sys_id, null_station_id).await;

        ctx.advance_secs(12);
        let _ = ctx.systems.get_or_spawn(CYNO_MID_SYS).step_manual(12.0).await;
        let cyno5 = rpc(&ctx, &mut cyno_mid_sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_mid_mod), PyRep::String("cynosuralGeneration".into())]).await.unwrap();
        let beacon5 = cyno5.dict_get("beaconID").and_then(|v| v.as_i64()).expect("cyno5 beaconID");
        undock_from_station(&ctx, &mut titan_sess, JITA_SYS).await;
        ctx.advance_secs(15);
        let _ = ctx.systems.get_or_spawn(JITA_SYS).step_manual(15.0).await;
        rpc(&ctx, &mut titan_sess, "beyonce", "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_mid_cid), PyRep::Integer(i64::from(CYNO_MID_SYS)), PyRep::Integer(beacon5)]).await.unwrap();
        let _ = rpc(&ctx, &mut cyno_mid_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(cyno_mid_mod)]).await;
        for _ in 0..16 {
            let _ = ctx.systems.get_or_spawn(CYNO_MID_SYS).step_manual(60.0).await;
        }
        // Clear max jump reactivation cooldown cap (30 min = 1800s) before Avatar's 2nd jump.
        ctx.advance_secs(1800);

        warp_and_approach_fleet_ship(&ctx, &mut cyno_null_sess, null_sys_id, falcon_null_id, PILOT_2, p2_stab).await;
        ctx.advance_secs(12);
        let _ = null_sys.step_manual(12.0).await;
        let cyno6 = rpc(&ctx, &mut cyno_null_sess, "dogmaIM", "Activate", &[PyRep::Integer(cyno_null_mod), PyRep::String("cynosuralGeneration".into())]).await.unwrap();
        let beacon6 = cyno6.dict_get("beaconID").and_then(|v| v.as_i64()).expect("cyno6 beaconID");
        rpc(&ctx, &mut titan_sess, "beyonce", "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_null_cid), PyRep::Integer(i64::from(null_sys_id)), PyRep::Integer(beacon6)]).await.unwrap();
        warp_and_approach_fleet_ship(&ctx, &mut titan_sess, null_sys_id, titan_ship_id, PILOT_2, p2_stab).await;
        let _ = rpc(&ctx, &mut cyno_null_sess, "dogmaIM", "Deactivate", &[PyRep::Integer(cyno_null_mod)]).await;
        for _ in 0..15 {
            let _ = null_sys.step_manual(1.0).await;
        }
        for _ in 0..30 {
            let _ = null_sys.step_manual(60.0).await;
        }
        ctx.advance_secs(1815);
        let cs_before = null_sys.get_combat_state(p2_stab).await.unwrap().unwrap();
        let p1_pos = null_sys.get_ball_snapshot(titan_ship_id).await.unwrap().unwrap().1.position;
        let p2_pos = null_sys.get_ball_snapshot(p2_stab).await.unwrap().unwrap().1.position;
        let dir = (p2_pos - p1_pos).normalize_or_zero();
        let end_pos = p1_pos + if dir.length_squared() > 0.0 { dir } else { DVec3::Y } * 180_000.0;

        let origin_arg = PyRep::Tuple(vec![PyRep::Real(p1_pos.x), PyRep::Real(p1_pos.y), PyRep::Real(p1_pos.z)]);
        let endpoint_arg = PyRep::Tuple(vec![PyRep::Real(end_pos.x), PyRep::Real(end_pos.y), PyRep::Real(end_pos.z)]);
        let titan_iso_before: i64 = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == titan_ship_id && i.type_id == 16_274).map(|i| i64::from(i.quantity)).sum());
        titan_iso_expected = titan_iso_before - 50_000;
        let lance_res = rpc(&ctx, &mut titan_sess, "superWeaponMgr", "ActivateSlashModule", &[PyRep::Integer(doomsday_id), origin_arg, endpoint_arg]).await.unwrap();
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
        ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
        reload_time = ctx.now_unix() + 120;
    }

    // Reload SQLite and verify deployed depot & fitted refit module persisted
    let ctx2 = open_ctx(&db_path, reload_time);
    let refit_item = ctx2.db.with_snapshot(|s| s.items.get(&refit_mod_id).cloned()).expect("persisted refit module");
    assert_eq!(refit_item.location_id, megathron_ship_id);
    assert_eq!(refit_item.flag, InventoryFlag::MedSlot0);
    let titan_iso: i64 = ctx2.db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == titan_ship_id && i.type_id == 16_274).map(|i| i64::from(i.quantity)).sum());
    assert_eq!(titan_iso, titan_iso_expected);
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
    let reload_time: u64;

    {
        let ctx = open_mem_ctx(base_time);
        fund_pilots(&ctx, &[(PILOT_1, 5_000_000_000.0), (PILOT_2, 5_000_000_000.0)]).await;

        let mut p1 = login(&ctx, 51_101, "demo", PILOT_1).await;
        let mut p2 = login(&ctx, 51_102, "other", PILOT_2).await;
        train_skills(&ctx, &mut p1, &[587, 484, 184, 3_300, 3_302, 3_328], true).await;
        train_skills(&ctx, &mut p2, &[601], false).await;

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

        let attacker_ship = buy(&ctx, &mut p1, 587, 1).await[0];
        rpc(&ctx, &mut p1, "ship", "AssembleShip", &[PyRep::Integer(attacker_ship)]).await.unwrap();
        let gun1 = buy(&ctx, &mut p1, 484, 1).await[0];
        let gun2 = buy(&ctx, &mut p1, 484, 1).await[0];
        let ammo1 = buy(&ctx, &mut p1, 184, 400).await[0];
        let fil_id = buy(&ctx, &mut p1, 56_070, 1).await[0];
        rpc(&ctx, &mut p1, "ship", "Board", &[PyRep::Integer(attacker_ship)]).await.unwrap();
        move_item(&ctx, &mut p1, gun1, attacker_ship, InventoryFlag::HiSlot0 as i64).await;
        move_item(&ctx, &mut p1, gun2, attacker_ship, InventoryFlag::HiSlot1 as i64).await;
        move_item(&ctx, &mut p1, ammo1, attacker_ship, InventoryFlag::Cargo as i64).await;
        move_item(&ctx, &mut p1, fil_id, attacker_ship, InventoryFlag::Cargo as i64).await;

        let victim_ship = buy(&ctx, &mut p2, 601, 1).await[0];
        rpc(&ctx, &mut p2, "ship", "AssembleShip", &[PyRep::Integer(victim_ship)]).await.unwrap();
        let trit_id = buy(&ctx, &mut p2, 34, 500).await[0];
        rpc(&ctx, &mut p2, "ship", "Board", &[PyRep::Integer(victim_ship)]).await.unwrap();
        move_item(&ctx, &mut p2, trit_id, victim_ship, InventoryFlag::Cargo as i64).await;
        ctx.advance_secs(12);

        // Form fleet, undock from Jita, warp >1,000 km from stations/gates to Jita sun, activate Pochven filament, then fly to Otela
        rpc(&ctx, &mut p1, "fleetMgr", "CreateFleet", &[]).await.unwrap();
        rpc(&ctx, &mut p1, "fleetMgr", "Invite", &[PyRep::Integer(PILOT_2), PyRep::Integer(4)]).await.unwrap();
        rpc(&ctx, &mut p2, "fleetMgr", "AcceptInvite", &[]).await.unwrap();

        let jita_sun = ctx.sde.celestials_in_system(JITA_SYS).into_iter().find(|c| c.group_id == 6).expect("Jita sun").celestial_id;
        undock_from_station(&ctx, &mut p1, JITA_SYS).await;
        undock_from_station(&ctx, &mut p2, JITA_SYS).await;
        fast_warp_to(&ctx, &mut p2, jita_sun, 100_000.0).await;
        fast_warp_to(&ctx, &mut p1, jita_sun, 100_000.0).await;
        ctx.advance_secs(15);
        for _ in 0..15 {
            let _ = ctx.systems.get_or_spawn(JITA_SYS).step_manual(1.0).await;
        }
        match rpc(&ctx, &mut p1, "beyonce", "CmdWarpToStuff", &[PyRep::String("char".into()), PyRep::Integer(PILOT_2)]).await {
            Ok(_) => wait_for_warp_end(&ctx, JITA_SYS, attacker_ship).await,
            Err(e) if e.code == "WarpDistanceTooShort" => {}
            Err(e) => panic!("CmdWarpToStuff char PILOT_2 failed: {e:?}"),
        }
        move_ship_to_ball(&ctx, &mut p1, JITA_SYS, attacker_ship, "item", PyRep::Integer(victim_ship)).await;
        ctx.advance_secs(15);
        for _ in 0..15 {
            let _ = ctx.systems.get_or_spawn(JITA_SYS).step_manual(1.0).await;
        }

        rpc(&ctx, &mut p1, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(fil_id)]).await.unwrap();
        p2.solar_system_id = p1.solar_system_id;
        assert_ne!(p1.solar_system_id, Some(JITA_SYS));
        assert_eq!(p2.solar_system_id, p1.solar_system_id);

        let entry_sys = p1.solar_system_id.unwrap();
        ctx.advance_secs(12);
        for _ in 0..12 {
            let _ = ctx.systems.get_or_spawn(entry_sys).step_manual(1.0).await;
        }
        travel_pochven_avoiding_home(&ctx, &mut p1, POCHVEN_SYS).await;
        travel_pochven_avoiding_home(&ctx, &mut p2, POCHVEN_SYS).await;

        let pochven_sun = ctx.sde.celestials_in_system(POCHVEN_SYS).into_iter().find(|c| c.group_id == 6).expect("Otela sun").celestial_id;
        fast_warp_to(&ctx, &mut p2, pochven_sun, 100_000.0).await;
        fast_warp_to(&ctx, &mut p1, pochven_sun, 100_000.0).await;
        let pochven = ctx.systems.get_or_spawn(POCHVEN_SYS);
        for _ in 0..12 {
            let _ = pochven.step_manual(1.0).await;
            ctx.advance_secs(1);
        }
        match rpc(&ctx, &mut p1, "beyonce", "CmdWarpToStuff", &[PyRep::String("char".into()), PyRep::Integer(PILOT_2)]).await {
            Ok(_) => wait_for_warp_end(&ctx, POCHVEN_SYS, attacker_ship).await,
            Err(e) if e.code == "WarpDistanceTooShort" => {}
            Err(e) => panic!("Otela CmdWarpToStuff char PILOT_2 failed: {e:?}"),
        }
        move_ship_to_ball(&ctx, &mut p1, POCHVEN_SYS, attacker_ship, "item", PyRep::Integer(victim_ship)).await;
        for _ in 0..12 {
            let _ = pochven.step_manual(1.0).await;
            ctx.advance_secs(1);
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
        ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
        reload_time = ctx.now_unix() + 60;
    }

    // Verify insurance contract on `insured_keep_ship` persists across SQLite restart
    let ctx2 = open_ctx(&db_path, reload_time);
    let mut p1_reload = login(&ctx2, 51_109, "demo", PILOT_1).await;
    assert!(!matches!(rpc(&ctx2, &mut p1_reload, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(insured_keep_ship)]).await.unwrap(), PyRep::None));
    let _ = std::fs::remove_file(&db_path);
}
