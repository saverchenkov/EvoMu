//! Integration tests for Equinox Orbital Skyhooks and shared Sov/FW helpers (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{field, id_req, rpc, subscribe, varint};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::EvePublicNotice;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::sov_gameplay::{
    advance_skyhook_reagents_in_snap, SKYHOOK_MATURITY_CYCLE_SECS, SKYHOOK_SILO_LINK_DURATION_SECS,
    SKYHOOK_SILO_LINK_MAX_SPEED_MS,
};
use eve_server::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SUPERIONIC_ICE,
};
use glam::DVec3;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc::Receiver;

pub(crate) const PILOT: i64 = 90_000_001;
pub(crate) const CORP: i64 = 98_000_001;
pub(crate) const ALLIANCE: i64 = 99_000_001;
pub(crate) const PILOT_SHIP: i64 = 100_000_001;

static SQLITE_SEQ: AtomicU64 = AtomicU64::new(1);

pub(crate) fn temp_sqlite_path(tag: &str) -> PathBuf {
    let n = SQLITE_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("evomu_{tag}_{}_{n}.sqlite", std::process::id()))
}

pub(crate) fn open_sqlite_ctx(path: &std::path::Path, seed_demo: bool) -> ServerContext {
    let repo = eve_db::WorldRepository::open_sqlite(path).expect("open_sqlite");
    repo.with_mut_snapshot(|snap| {
        snap.markets_seeded = true;
    });
    if seed_demo {
        let tpl = eve_db::WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters.values().next().cloned().expect("demo char"));
        repo.with_mut_snapshot(|snap| {
            snap.characters.entry(tpl.character_id).or_insert(tpl);
        });
        repo.provision_player_account("demo", "demo-secret");
        repo.provision_player_account("demo2", "demo2-secret");
    }
    let ctx = ServerContext::with_repository(std::sync::Arc::new(repo))
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    crate::common::pin_clock(&ctx);
    ctx
}

pub(crate) async fn rpc_call(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let bound_id = if service == "beyonce" {
        session.solar_system_id.and_then(|s| {
            ctx.bind_object(session, "beyonce", s as i64).ok()
        })
    } else {
        None
    };
    ctx.dispatch_rpc(session, service, bound_id, method, args, &HashMap::new(), None)
        .await
}

pub(crate) fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 1_500.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 65.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

pub(crate) fn drain_quasar(rx: &mut Receiver<EvePublicNotice>) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for _ in 0..256 {
        match rx.try_recv() {
            Ok(n) => {
                if let Some(p) = n.payload {
                    out.push((p.normalized_type_name().to_string(), p.value));
                }
            }
            Err(_) => break,
        }
    }
    out
}

pub(crate) fn drain_notif_events(ctx: &ServerContext, char_id: i64) -> Vec<String> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| n.event)
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn register_online_pilot(
    ctx: &ServerContext,
    sid: u64,
    username: &str,
    password: &str,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
    corp_role: u64,
) -> SessionState {
    ctx.db.provision_player_account(username, password);
    let uid = ctx.db.find_user_id_by_username(username).expect("user_id");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap
            .characters
            .get(&char_id)
            .cloned()
            .or_else(|| snap.characters.values().next().cloned())
            .expect("provisioned template character");
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.alliance_id = alliance_id;
        ch.war_faction_id = None;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.clone_grade = 2;
        snap.characters.insert(char_id, ch);
        let corp = snap.corporations.entry(corp_id).or_default();
        corp.corporation_id = corp_id;
        if corp.ceo_id == 0 {
            corp.ceo_id = if corp_role != 0 { char_id } else { 90_999_999 };
        }
        corp.alliance_id = alliance_id;
        corp.members.insert(char_id);
        if corp_role != 0 {
            corp.member_roles.insert(char_id, corp_role);
        }
        if let Some(aid) = alliance_id {
            let al = snap.alliances.entry(aid).or_insert_with(|| eve_db::AllianceRecord {
                alliance_id: aid,
                name: format!("Alliance {aid}"),
                short_name: "ALLY".into(),
                executor_corp_id: corp_id,
                member_corp_ids: std::collections::HashSet::new(),
            });
            al.member_corp_ids.insert(corp_id);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut s = SessionState::new(sid);
    crate::common::login_pilot(ctx, &mut s, username, password).await;
    rpc_call(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(15);
    s.corporation_id = Some(corp_id);
    s.alliance_id = alliance_id;
    s.solar_system_id = Some(sys_id);
    s.station_id = None;
    s.structure_id = None;
    s.ship_id = Some(ship_id);
    s.corp_role = corp_role;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    let _ = sys_h
        .spawn_ball_with_combat(make_ball(ship_id, ship_type_id, pos), cs)
        .await;
    s
}

pub(crate) fn set_pilot_location(
    ctx: &ServerContext,
    session: &mut SessionState,
    sys_id: u32,
    station_id: Option<i64>,
) {
    session.solar_system_id = Some(sys_id);
    session.station_id = station_id;
    if let Some(sys_def) = ctx.sde.solar_systems.get(&sys_id) {
        session.constellation_id = Some(sys_def.constellation_id);
        session.region_id = Some(sys_def.region_id);
    }
    if let Some(cid) = session.character_id {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = station_id;
                if let Some(sys_def) = ctx.sde.solar_systems.get(&sys_id) {
                    ch.constellation_id = sys_def.constellation_id;
                    ch.region_id = sys_def.region_id;
                }
            }
        });
    }
}

pub(crate) async fn place_ship_ball(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.remove_ball(ship_id).await;
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    sys_h
        .spawn_ball_with_combat(make_ball(ship_id, ship_type_id, pos), cs)
        .await
        .expect("place_ship_ball");
}

pub(crate) async fn step_weapon_ticks(ctx: &ServerContext, sys_id: u32) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..45 {
        sys_h.step_manual(1.0).await.unwrap();
    }
}

pub(crate) async fn lock_target_via_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    target_id: i64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if let Ok(locks) = sys.get_target_locks(ship_id).await {
        if locks.iter().any(|l| l.target_id == target_id && l.locked) {
            return;
        }
    }
    rpc_call(ctx, session, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)])
        .await
        .expect("dogmaIM.AddTarget");
    for _ in 0..45 {
        let locks = sys.get_target_locks(ship_id).await.unwrap();
        if locks.iter().any(|l| l.target_id == target_id && l.locked) {
            return;
        }
        sys.step_manual(1.0).await.unwrap();
    }
    let locks = sys.get_target_locks(ship_id).await.unwrap();
    assert!(locks.iter().any(|l| l.target_id == target_id && l.locked));
}

pub(crate) fn fit_subcap_blasters(ctx: &ServerContext, char_id: i64, ship_id: i64, base_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.type_id = 16_229; // Brutix Battlecruiser (1,125 MW PG, 435 tf CPU, 3,100 GJ cap, 7 HiSlots, 6 Turret slots, 6 LoSlots)
        }
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [
            3300u32, 3304, 12211, 3310, 3311, 3312, 3315, 3316, 3317, 3318, 33097, 3426, 3413,
        ] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        let hi_flags = [
            InventoryFlag::HiSlot0,
            InventoryFlag::HiSlot1,
            InventoryFlag::HiSlot2,
            InventoryFlag::HiSlot3,
            InventoryFlag::HiSlot4,
            InventoryFlag::HiSlot5,
        ];
        for (i, flg) in hi_flags.into_iter().enumerate() {
            let gun_id = base_id + (i as i64) * 2;
            let ammo_id = gun_id + 1;
            snap.items.insert(
                gun_id,
                InventoryItemRecord {
                    item_id: gun_id,
                    type_id: 3_146, // Heavy Neutron Blaster II (187 MW PG, 33 tf CPU, 38.4 tracking, 4.41x dmg)
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                ammo_id,
                InventoryItemRecord {
                    item_id: ammo_id,
                    type_id: 230, // Antimatter Charge M (14 kin + 10 therm = 24 base dmg)
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 500,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gun_id, true);
        }
        for (i, flg) in [InventoryFlag::LoSlot0, InventoryFlag::LoSlot1, InventoryFlag::LoSlot2].into_iter().enumerate() {
            let mod_id = base_id + 20 + (i as i64);
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 10_190, // Magnetic Field Stabilizer II
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mod_id, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
}

pub(crate) fn fit_siege_artillery(ctx: &ServerContext, char_id: i64, ship_id: i64, base_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.type_id = 19_722; // Naglfar Dreadnought (540,000 MW PG, 800 tf CPU, 5 HiSlots, 3 Turret slots)
        }
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3300u32, 3303, 3306, 21667, 20533, 20532, 3456, 3426, 3413, 24571] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        let flags = [
            InventoryFlag::HiSlot0,
            InventoryFlag::HiSlot1,
            InventoryFlag::HiSlot2,
        ];
        for (i, flg) in flags.into_iter().enumerate() {
            let gun_id = base_id + (i as i64) * 2;
            let ammo_id = gun_id + 1;
            snap.items.insert(
                gun_id,
                InventoryItemRecord {
                    item_id: gun_id,
                    type_id: 20_454, // Quad 3500mm Siege Artillery I (990 HP/gun = 2,970 HP/volley)
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.items.insert(
                ammo_id,
                InventoryItemRecord {
                    item_id: ammo_id,
                    type_id: 17_668, // EMP XL
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 200,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gun_id, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
}

pub(crate) fn find_nullsec_planets(ctx: &ServerContext) -> (u32, i64, i64, i64, i64) {
    let mut sys_ids: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .collect();
    sys_ids.sort_unstable();
    for sys_id in &sys_ids {
        let cels = ctx.sde.celestials_in_system(*sys_id);
        let temp = cels.iter().find(|c| c.type_id == 11).map(|c| c.celestial_id);
        let lava = cels.iter().find(|c| c.type_id == 2015).map(|c| c.celestial_id);
        let ice = cels.iter().find(|c| c.type_id == 12).map(|c| c.celestial_id);
        let non_temp = cels.iter().find(|c| c.group_id == 7 && c.type_id != 11).map(|c| c.celestial_id);
        if let (Some(t), Some(l), Some(ic), Some(nt)) = (temp, lava, ice, non_temp) {
            return (*sys_id, t, l, ic, nt);
        }
    }
    panic!("expected sovereign-claimable null-sec system with Temperate, Lava, and Ice planets");
}

#[tokio::test]
async fn test_skyhook_anchoring_linear_maturity_and_live_silo_ship_link_theft() {
    // parity: feat:sovereignty.skyhook-acquire
    // DATA GAP: feat:sovereignty.sov-reagents — server-only planetary reagent yield table (docs/BLOCKERS.md §9)
    // parity: feat:sovereignty.skyhook-theft
    // parity: notify:OnSkyhookFirstActivation
    // parity: notify:OnSkyhookCanBeHacked
    eve_testkit::attest(&[
        "feat:sovereignty.skyhook-acquire",
        "feat:sovereignty.skyhook-theft",
        "notify:OnSkyhookFirstActivation",
        "notify:OnSkyhookCanBeHacked",
    ]);

    let db_path = temp_sqlite_path("sov_skyhook");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (sys_id, _, lava_planet, ice_planet, _) = find_nullsec_planets(&ctx);
    let cels = ctx.sde.celestials_in_system(sys_id);
    let lava_pos = cels.iter().find(|c| c.celestial_id == lava_planet).unwrap().position;
    let ice_pos = cels.iter().find(|c| c.celestial_id == ice_planet).unwrap().position;

    let mut s_dir = register_online_pilot(
        &ctx, 71_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        sys_id, PILOT_SHIP, 621, lava_pos, corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut rx = subscribe(&ctx, &mut s_dir).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(ALLIANCE);
    });

    // 1. Anchoring validation & live RPC anchoring (`structureDeployment.Anchor`).
    let jita_planet = ctx.sde.celestials_in_system(30_000_142).into_iter().find(|c| c.group_id == 7).unwrap();
    set_pilot_location(&ctx, &mut s_dir, 30_000_142, None);
    place_ship_ball(&ctx, 30_000_142, PILOT, PILOT_SHIP, 621, jita_planet.position).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(jita_planet.celestial_id)])
            .await.unwrap_err().code,
        "CannotAnchorSkyhookInEmpireSpace"
    );
    set_pilot_location(&ctx, &mut s_dir, sys_id, None);
    let mut s_norole = register_online_pilot(
        &ctx, 71_003, "demo3", "demo3-secret", 90_000_002, 98_000_002, None,
        sys_id, 100_000_002, 621, lava_pos, 0,
    ).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_norole, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)])
            .await.unwrap_err().code,
        "InsufficientCorpRoles"
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(999_999_999)])
            .await.unwrap_err().code,
        "InvalidPlanet"
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)])
            .await.unwrap_err().code,
        "MissingSkyhookItem"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_101,
            InventoryItemRecord {
                item_id: 990_101,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    let _ = drain_notif_events(&ctx, PILOT);
    let lava_rep = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)])
        .await.expect("anchor Lava Skyhook");
    let lava_skyhook = lava_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    assert!(drain_notif_events(&ctx, PILOT).iter().any(|e| e == "OnSkyhookFirstActivation"));
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)])
            .await.unwrap_err().code,
        "SkyhookAlreadyExistsAtPlanet"
    );

    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(make_ball(PILOT_SHIP, 621, ice_pos + DVec3::new(150_000.0, 0.0, 0.0))).await;
    let ice_rep = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(ice_planet)])
        .await.expect("anchor Ice Skyhook");
    let ice_skyhook = ice_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    let corp_sk = rpc_call(&ctx, &mut s_dir, "colonyResourcesMgr", "GetMyCorporationSkyhooks", &[]).await.expect("GetMyCorporationSkyhooks");
    assert!(format!("{corp_sk:?}").contains(&lava_skyhook.to_string()));
    let _ = rpc_call(&ctx, &mut s_dir, "sovMgr", "AcquireSkyhooks", &[PyRep::Integer(sys_id as i64)]).await.expect("AcquireSkyhooks");

    // 2. Harvest 10h (500 Magmatic Gas, 500 Superionic Ice) and verify exact 1 Hz linear maturity determinism.
    ctx.advance_secs(10 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (lava_t, lava_sec0, lava_insec0, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_skyhook]);
    let (ice_t, _, ice_insec0, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&ice_skyhook]);
    assert_eq!((lava_t, lava_sec0, lava_insec0), (TYPE_MAGMATIC_GAS, 250, 250));
    assert_eq!((ice_t, ice_insec0), (TYPE_SUPERIONIC_ICE, 250));

    let t0 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_active.insert(lava_skyhook, false);
        snap.sov_runtime.skyhook_active.insert(ice_skyhook, false);
        for step in 1..=43_200u64 {
            let _ = advance_skyhook_reagents_in_snap(snap, lava_skyhook, t0 + step);
        }
        let _ = advance_skyhook_reagents_in_snap(snap, ice_skyhook, t0 + SKYHOOK_MATURITY_CYCLE_SECS / 2);
        snap.sov_runtime.skyhook_active.insert(lava_skyhook, true);
        snap.sov_runtime.skyhook_active.insert(ice_skyhook, true);
        snap.sov_runtime.skyhook_last_harvest_unix.insert(lava_skyhook, t0 + SKYHOOK_MATURITY_CYCLE_SECS / 2);
        snap.sov_runtime.skyhook_last_harvest_unix.insert(ice_skyhook, t0 + SKYHOOK_MATURITY_CYCLE_SECS / 2);
    });
    ctx.advance_secs(SKYHOOK_MATURITY_CYCLE_SECS / 2);
    let (_, lava_sec12, lava_insec12, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_skyhook]);
    let (_, ice_sec12, ice_insec12, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&ice_skyhook]);
    assert_eq!((lava_sec12, lava_insec12), (375, 125));
    assert_eq!((lava_sec12, lava_insec12), (ice_sec12, ice_insec12));

    let sk_proto = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, lava_skyhook as u64))
        .await.payload.unwrap().value;
    let sim = field(&field(&sk_proto, 2).unwrap(), 2).unwrap();
    assert_eq!((varint(&sim, 1), varint(&sim, 2)), (Some(375), Some(125)));

    // 3. Live Orbital Skyhook Reagent Silo ship-link theft (`dogmaIM.Activate` -> `<= 10%` shield -> `InitiateLink` -> 600s tick).
    let thief_cid = 90_000_077i64;
    let thief_ship = 100_000_077i64;
    let sky_ball_pos = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(lava_skyhook).await.unwrap().unwrap().1.position;
    let mut s_thief = register_online_pilot(
        &ctx, 71_010, "demo2", "demo2-secret", thief_cid, 98_000_077, Some(99_000_077),
        sys_id, thief_ship, 621, sky_ball_pos + DVec3::new(4_000.0, 0.0, 0.0), 0,
    ).await;
    fit_siege_artillery(&ctx, thief_cid, thief_ship, 991_000);

    assert_eq!(
        rpc_call(&ctx, &mut s_thief, "beyonce", "CallComponentFromClient", &[PyRep::Integer(lava_skyhook), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into())])
            .await.unwrap_err().code,
        "SkyhookSiloShieldTooHigh"
    );
    let now = ctx.now_unix();
    let (tw_start, tw_end) = ctx.db.with_snapshot(|snap| {
        snap.sov_runtime.skyhook_theft_windows[&lava_skyhook]
    });
    assert!(tw_start <= now && now < tw_end);
    let _ = drain_notif_events(&ctx, PILOT);
    lock_target_via_rpc(&ctx, &mut s_thief, sys_id, thief_ship, lava_skyhook).await;
    for _ in 0..6 {
        for gun_id in [991_000, 991_002, 991_004] {
            let _ = rpc_call(&ctx, &mut s_thief, "dogmaIM", "Activate", &[
                PyRep::Integer(gun_id),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(lava_skyhook),
            ]).await;
        }
        step_weapon_ticks(&ctx, sys_id).await;
        if ctx.db.with_snapshot(|s| s.structures[&lava_skyhook].shield_hp) <= 1_000.0 {
            break;
        }
        ctx.advance_secs(25);
    }
    let notifs = ctx.drain_notifications_for_char(PILOT);
    let hack_notif = notifs.iter().find(|n| n.event == "OnSkyhookCanBeHacked").expect("OnSkyhookCanBeHacked");
    assert_eq!(hack_notif.args.first().and_then(|a| a.as_i64()), Some(sys_id as i64));
    assert_eq!(hack_notif.args.get(1).and_then(|a| a.as_i64()), Some(lava_skyhook));

    let _ = drain_quasar(&mut rx);
    let link_rep = rpc_call(
        &ctx, &mut s_thief, "beyonce", "CallComponentFromClient",
        &[PyRep::Integer(lava_skyhook), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into())],
    ).await.expect("InitiateLink on vulnerable <=10% shield Skyhook");
    assert_eq!(link_rep.dict_get("linked"), Some(&PyRep::Bool(true)));
    let linked_ball = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(thief_ship).await.unwrap().unwrap().1;
    assert_eq!(linked_ball.max_velocity, SKYHOOK_SILO_LINK_MAX_SPEED_MS);
    assert_eq!(
        rpc_call(&ctx, &mut s_thief, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ice_skyhook)])
            .await.unwrap_err().code,
        "SkyhookLinkActive"
    );

    ctx.advance_secs(SKYHOOK_SILO_LINK_DURATION_SECS + 1);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(drain_quasar(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.skyhook.api.SkyhookReagentsSimulatedNotice"));
    let (_, final_sec, final_insec, _) = ctx.db.with_snapshot(|s| s.sov_runtime.skyhook_reagent_bays[&lava_skyhook]);
    assert_eq!((final_sec, final_insec), (376, 0));
    let balls = ctx.systems.get_or_spawn(sys_id).list_balls().await.unwrap();
    let can_ball = balls.iter().find(|b| b.type_id == 23).expect("Cargo Container 23 spawned in space");
    let stolen_qty: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == can_ball.id && i.type_id == TYPE_MAGMATIC_GAS).map(|i| i.quantity).sum()
    });
    assert_eq!(stolen_qty, 124);

    let full_cargo_item_id = 991_999i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            full_cargo_item_id,
            InventoryItemRecord {
                item_id: full_cargo_item_id,
                type_id: 34, // Tritanium
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 50_000_000,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(make_ball(PILOT_SHIP, 621, sky_ball_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;
    assert_eq!(
        rpc_call(
            &ctx, &mut s_dir, "beyonce", "CallComponentFromClient",
            &[PyRep::Integer(lava_skyhook), PyRep::String("orbitalSkyhook".into()), PyRep::String("ExtractReagentsToShip".into())],
        ).await.unwrap_err().code,
        "NotEnoughCargoSpace"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&full_cargo_item_id);
    });
    let ext_rep = rpc_call(
        &ctx, &mut s_dir, "beyonce", "CallComponentFromClient",
        &[PyRep::Integer(lava_skyhook), PyRep::String("orbitalSkyhook".into()), PyRep::String("ExtractReagentsToShip".into())],
    ).await.expect("ExtractReagentsToShip");
    assert_eq!(ext_rep.dict_get("extractedQuantity").and_then(|v| v.as_i64()), Some(376));

    drop(ctx);
    let ctx_restarted = open_sqlite_ctx(&db_path, false);
    assert_eq!(ctx_restarted.db.with_snapshot(|s| s.sov_runtime.skyhook_planets.get(&lava_skyhook).copied()), Some(lava_planet));
    let _ = std::fs::remove_file(&db_path);
}

#[allow(dead_code, unused_imports)]
pub(crate) mod equinox_workforce_transit {
    use super::*;

//  Part 2 of Equinox Sovereignty closure tests: Mercenary Den Anchoring, Anarchy, MTO Combat, Repair & Loot (`SO-5`, `Q-8`).


use crate::quasar_sov_equinox_hub::{field, fields, id_req, rpc, structure, subscribe};
use crate::sovereignty_equinox_gameplay::{
    drain_notif_events, drain_quasar, find_nullsec_planets, fit_siege_artillery,
    lock_target_via_rpc, make_ball, register_online_pilot, rpc_call, set_pilot_location,
    step_weapon_ticks, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::encode_bytes_field;
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_CONFLICT, STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_sov_hub::compute_hub_resources;
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::{
    MERC_DEN_REINFORCEMENT_SECS, MERC_DEN_VULNERABILITY_WINDOW_SECS, TYPE_INFOMORPH_DECRYPTION_KEY,
    TYPE_MERCENARY_DEN,
};
use eve_server::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
use glam::DVec3;

#[tokio::test]
async fn test_mercenary_den_live_anchoring_anarchy_mto_combat_repair_and_loot() {
    // DATA GAP: feat:sovereignty.mercenary-den / mercenary-den-development-anarchy / mercenary-den-anarchy-evolution — synthetic yield/capacity/anarchy values pending V24.01 client/SDE tables (docs/BLOCKERS.md §10)
    // parity: feat:sovereignty.mercenary-den-anchoring
    // DATA GAP: feat:pve.mercenary-tactical-ops (open until real SDE tables imported)
    // parity: feat:sovereignty.mercenary-den-reinforcement-loot
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj
    // parity: notify:OnMercenaryDenRegistrationInvalid
    // parity: notify:OnMercenaryDenRegistrationSuccess
    // parity: notify:OnMyMercenaryDenRemoved
    eve_testkit::attest(&[
        "feat:sovereignty.mercenary-den-anchoring",
        "feat:sovereignty.mercenary-den-reinforcement-loot",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/activity/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/mercenaryden/api/notices_pb2.pyj",
        "notify:OnMercenaryDenRegistrationInvalid",
        "notify:OnMercenaryDenRegistrationSuccess",
        "notify:OnMyMercenaryDenRemoved",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (null_sys, temp_planet, _, _, non_temp_planet) = find_nullsec_planets(&ctx);
    let hub_id = 1_050_000_001i64;
    let skyhook_id = 1_050_000_002i64;
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx, 72_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 639, DVec3::new(50_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 1, skill_points: 25_000.0 },
        );
        snap.items.insert(
            990_201,
            InventoryItemRecord {
                item_id: 990_201,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    // Sovereign owner configures Skyhook Mercenary Den tax via authorized ConfirmSkyhookSovManagement RPC
    let tax_rep = rpc_call(&ctx, &mut s_owner, "sovMgr", "ConfirmSkyhookSovManagement", &[PyRep::Integer(skyhook_id), PyRep::Integer(20)])
        .await.expect("owner sets skyhook mercenary den tax");
    assert_eq!(tax_rep.dict_get("taxPercent").and_then(|v| v.as_i64()), Some(20));

    // 1. Live Mercenary Den anchoring validation (>10km, non-Temperate, skill cap) & notifications.
    let _ = drain_notif_events(&ctx, PILOT);
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
            .await.unwrap_err().code,
        "MercenaryDenTooFarFromSkyhook"
    );
    let notifs_inv = ctx.drain_notifications_for_char(PILOT);
    let inv_ev = notifs_inv.iter().find(|n| n.event == "OnMercenaryDenRegistrationInvalid").expect("OnMercenaryDenRegistrationInvalid");
    assert_eq!(inv_ev.args.first().and_then(|a| a.as_str()), Some("MercenaryDenTooFarFromSkyhook"));
    assert_eq!(inv_ev.args.get(1).and_then(|a| a.dict_get("itemID")).and_then(|v| v.as_i64()), Some(990_201));

    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 639, sh_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, non_temp_planet);
    });
    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(non_temp_planet)])
            .await.unwrap_err().code,
        "PlanetNotTemperate"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
    });

    let _ = drain_notif_events(&ctx, PILOT);
    let _ = drain_quasar(&mut rx);
    let den_rep = rpc_call(
        &ctx, &mut s_owner, "structureDeployment", "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)],
    ).await.expect("anchor Mercenary Den at Temperate Skyhook <=10km");
    let den_id = den_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let notifs_succ = ctx.drain_notifications_for_char(PILOT);
    let succ_ev = notifs_succ.iter().find(|n| n.event == "OnMercenaryDenRegistrationSuccess").expect("OnMercenaryDenRegistrationSuccess");
    assert_eq!(succ_ev.args.first().and_then(|a| a.as_str()), Some("MercenaryDenRegistered"));
    assert_eq!(succ_ev.args.get(1).and_then(|a| a.dict_get("itemID")).and_then(|v| v.as_i64()), Some(den_id));
    let anchor_q = drain_quasar(&mut rx);
    assert!(anchor_q.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice"));
    assert!(anchor_q.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice"));

    assert_eq!(
        rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(skyhook_id), PyRep::Integer(temp_planet)])
            .await.unwrap_err().code,
        "MercenaryDenCapExceeded"
    );

    // 2. Anarchy rises over 8 days (+5/24h -> Anarchy 40, stage 3 / UI level 2), siphoning 10% of host Temperate Skyhook workforce (150 of 1,500).
    let base_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    ctx.advance_secs(8 * 86_400);
    assert_eq!(
        rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.status_code,
        STATUS_OK
    );
    let siphoned_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    assert_eq!(siphoned_wf + 150, base_wf);

    // 3. Quasar MTO requests spawn live MTO combat target ball (`24690`), completed via live `dogmaIM.AddTarget` + `Activate`!
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
    let acts = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64))
        .await.payload.unwrap().value;
    let act_list = fields(&acts, 1);
    let first_act = act_list.last().unwrap().clone();
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&first_act, 1).unwrap(), &mut start_req);

    // Negative check: caller docked or in another solar system is rejected on StartRequest
    set_pilot_location(&ctx, &mut s_owner, null_sys, Some(60_003_760));
    assert_eq!(
        rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req.clone()).await.status_code,
        STATUS_FORBIDDEN
    );
    set_pilot_location(&ctx, &mut s_owner, null_sys, None);

    let _ = drain_quasar(&mut rx);
    let pre_mto_bal = ctx.db.with_snapshot(|s| s.characters[&PILOT].balance.0);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code, STATUS_OK);
    assert!(drain_quasar(&mut rx).iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice"));

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let mto_ball_id = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().expect("MTO ball"));
    let mto_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(mto_ball_id).await.unwrap().unwrap().1.position;

    fit_siege_artillery(&ctx, PILOT, PILOT_SHIP, 992_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            992_050,
            InventoryItemRecord {
                item_id: 992_050,
                type_id: 3_616, // Capital Remote Shield Booster I (960 HP/cycle)
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot3,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(992_050, true);
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    let cs_owner = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 19_722);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(PILOT_SHIP, 19_722, mto_pos + DVec3::new(2_500.0, 0.0, 0.0)), cs_owner).await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, mto_ball_id).await;
    let _ = drain_quasar(&mut rx);
    for _round in 0..25 {
        for gun in [992_000i64, 992_002, 992_004] {
            ctx.set_invention_roll_override(Some(0.005));
            // A still-cycling turret keeps firing; re-activation is rejected with ModuleAlreadyActive (V24 rule).
            let shot = match rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(gun), PyRep::Integer(mto_ball_id)]).await {
                Ok(r) => r,
                Err(e) if e.code == "ModuleAlreadyActive" => PyRep::None,
                Err(e) => panic!("dogmaIM.Activate turret on MTO ball: {e:?}"),
            };
            if shot.dict_get("destroyed") == Some(&PyRep::Bool(true))
                || ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball_id))
            {
                break;
            }
        }
        if ctx.db.with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball_id)) {
            break;
        }
        step_weapon_ticks(&ctx, null_sys).await;
    }
    let mto_q = drain_quasar(&mut rx);
    assert!(mto_q.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice"));
    assert!(mto_q.iter().any(|(t, _)| t == "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice"));
    let restored_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    assert_eq!(restored_wf, base_wf);

    // MTO completion grants Development / Anarchy / Infomorph rewards + 7.5M ISK
    let post_mto_bal = ctx.db.with_snapshot(|s| s.characters[&PILOT].balance.0);
    assert_eq!(post_mto_bal - pre_mto_bal, 7_500_000 * 100, "MTO completion must award 7.5M ISK");
    let has_mto_journal = ctx.db.with_snapshot(|s| {
        s.wallet_journal.iter().any(|e| e.character_id == PILOT && e.entry_type == "MercenaryDenReward")
    });
    assert!(has_mto_journal, "MTO completion must append MercenaryDenReward wallet journal entry");

    // Start an MTO that times out: uncompleted expiry despawns site ball and awards 0 ISK
    let acts_second = rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64))
        .await.payload.unwrap().value;
    let act_list_second = fields(&acts_second, 1);
    if let Some(second_act) = act_list_second.first() {
        let mut second_start = Vec::new();
        encode_bytes_field(1, &field(second_act, 1).unwrap(), &mut second_start);
        assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", second_start).await.status_code, STATUS_OK);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let balls_before = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.len());
        assert_eq!(balls_before, 1);
        let bal_before_timeout = ctx.db.with_snapshot(|s| s.characters[&PILOT].balance.0);
        let player_maelstrom_ship_id = 100_000_999i64;
        let cs_player_maelstrom = ctx.build_ship_combat_state(PILOT, player_maelstrom_ship_id, 24_690);
        let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(
            make_ball(player_maelstrom_ship_id, 24_690, DVec3::new(100_000.0, 0.0, 0.0)),
            cs_player_maelstrom,
        ).await;
        assert!(ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(player_maelstrom_ship_id).await.unwrap().is_some());
        ctx.advance_secs(1_801); // 30m timeout
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let balls_after = ctx.db.with_snapshot(|s| s.sov_runtime.mto_site_balls.len());
        assert_eq!(balls_after, 0, "expired MTO despawns its site ball");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT].balance.0), bal_before_timeout, "expired MTO awards no ISK");
        assert!(
            ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(player_maelstrom_ship_id).await.unwrap().is_some(),
            "player Maelstrom (type 24690) in den system must survive cluster ticks and not be swept",
        );
        let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(player_maelstrom_ship_id).await;
    }

    // 4. Live `beyonce.CallComponentFromClient` (`mercenaryDen.extract_infomorphs_to_ship`) deducts 20% sov tax.
    let den_pos = ctx.systems.get_or_spawn(null_sys).get_ball_snapshot(den_id).await.unwrap().unwrap().1.position;
    let cs_near = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 19_722);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(PILOT_SHIP, 19_722, den_pos + DVec3::new(2_000.0, 0.0, 0.0)), cs_near).await;

    // Negative cargo hold check: full cargo hold returns NotEnoughCargoSpace
    let full_cargo_id = 992_999i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            full_cargo_id,
            InventoryItemRecord {
                item_id: full_cargo_id,
                type_id: 34,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 50_000_000,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        rpc_call(
            &ctx, &mut s_owner, "beyonce", "CallComponentFromClient",
            &[PyRep::Integer(den_id), PyRep::String("mercenaryDen".into()), PyRep::String("extract_infomorphs_to_ship".into())],
        ).await.unwrap_err().code,
        "NotEnoughCargoSpace"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&full_cargo_id);
    });

    let ext_rep = rpc_call(
        &ctx, &mut s_owner, "beyonce", "CallComponentFromClient",
        &[PyRep::Integer(den_id), PyRep::String("mercenaryDen".into()), PyRep::String("extract_infomorphs_to_ship".into())],
    ).await.expect("extract_infomorphs_to_ship");
    let owner_share = ext_rep.dict_get("ownerShare").and_then(|v| v.as_i64()).unwrap();
    let tax_share = ext_rep.dict_get("taxShare").and_then(|v| v.as_i64()).unwrap();
    assert!(owner_share > 0 && tax_share > 0 && tax_share * 4 == owner_share);

    // 5. Hostile live combat weapon fire reinforces Den (24h + 6h window), Remote Shield Booster repairs <50% then >=50%, second kill drops ~50% stacks!
    let atk_cid = 90_000_088i64;
    let atk_ship = 100_000_088i64;
    let mut s_atk = register_online_pilot(
        &ctx, 72_010, "demo2", "demo2-secret", atk_cid, 98_000_088, Some(99_000_088),
        null_sys, atk_ship, 639, den_pos + DVec3::new(3_000.0, 0.0, 0.0), 0,
    ).await;
    fit_siege_artillery(&ctx, atk_cid, atk_ship, 993_000);
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, den_id).await;
    for _ in 0..6 {
        for gun_id in [993_000, 993_002, 993_004] {
            let _ = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(den_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&den_id].state == "HullReinforced") {
            break;
        }
        ctx.advance_secs(25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&den_id].state.clone()), "HullReinforced");
    let (reinf_until, vuln_ends) = ctx.db.with_snapshot(|snap| {
        let (r, v, _) = snap.sov_runtime.mercenary_den_reinforcement[&den_id];
        (r, v)
    });
    assert_eq!(reinf_until - ctx.now_unix(), MERC_DEN_REINFORCEMENT_SECS);
    assert_eq!(vuln_ends - reinf_until, MERC_DEN_VULNERABILITY_WINDOW_SECS);
    assert_eq!(rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await.status_code, STATUS_CONFLICT);

    ctx.advance_secs(MERC_DEN_REINFORCEMENT_SECS + 60);
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, den_id).await;
    for step_idx in 0..2 {
        let rep = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(992_050), PyRep::String("shieldTransfer".into()), PyRep::Integer(den_id)])
            .await.expect("Remote Shield Booster cycle <50%");
        if step_idx == 1 {
            assert_eq!(rep.dict_get("denEnabled"), Some(&PyRep::Bool(false)));
        }
        ctx.advance_secs(25);
    }
    let rep_ok = rpc_call(&ctx, &mut s_owner, "dogmaIM", "Activate", &[PyRep::Integer(992_050), PyRep::String("shieldTransfer".into()), PyRep::Integer(den_id)])
        .await.expect("Remote Shield Booster cycle >=50%");
    assert_eq!(rep_ok.dict_get("denEnabled"), Some(&PyRep::Bool(true)));

    ctx.db.with_mut_snapshot(|snap| {
        for idx in 0..8i64 {
            snap.items.insert(
                994_000 + idx,
                InventoryItemRecord {
                    item_id: 994_000 + idx,
                    type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                    owner_id: PILOT,
                    location_id: den_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 10,
                    ..Default::default()
                },
            );
        }
    });
    step_weapon_ticks(&ctx, null_sys).await;
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, den_id).await;
    for _ in 0..6 {
        for gun_id in [993_000, 993_002, 993_004] {
            let _ = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(den_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&den_id].state == "HullReinforced") {
            break;
        }
        ctx.advance_secs(25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&den_id].state.clone()), "HullReinforced");
    ctx.advance_secs(MERC_DEN_REINFORCEMENT_SECS + 120);
    let _ = drain_notif_events(&ctx, PILOT);
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, den_id).await;
    for _ in 0..6 {
        for gun_id in [993_000, 993_002, 993_004] {
            let _ = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(den_id)]).await;
        }
        step_weapon_ticks(&ctx, null_sys).await;
        if ctx.db.with_snapshot(|s| s.structures[&den_id].state == "Destroyed") {
            break;
        }
        ctx.advance_secs(25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&den_id].state.clone()), "Destroyed");
    let notifs_rem = ctx.drain_notifications_for_char(PILOT);
    let rem_ev = notifs_rem.iter().find(|n| n.event == "OnMyMercenaryDenRemoved").expect("OnMyMercenaryDenRemoved");
    assert_eq!(rem_ev.args.first().and_then(|a| a.as_str()), Some("MercenaryDenRemoved"));
    assert_eq!(rem_ev.args.get(1).and_then(|a| a.dict_get("itemID")).and_then(|v| v.as_i64()), Some(den_id));
    let dropped_stacks = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.type_id == TYPE_INFOMORPH_DECRYPTION_KEY && i.quantity > 0 && i.location_id != den_id).count()
    });
    assert!((1..=7).contains(&dropped_stacks));
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod equinox_skyhook_reagents {
    use super::*;

//  Part 3 of Equinox Sovereignty closure tests: Hub Fuel, Power, Workforce, Upgrades, Sites, Hacking, Flex & Tenebrex (`SO-5`, `Q-8`).


use crate::quasar_sov_equinox_hub::{field, fields, id_req, idm, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay::{
    drain_notif_events, drain_quasar, find_nullsec_planets, fit_siege_artillery,
    lock_target_via_rpc, make_ball, open_sqlite_ctx, register_online_pilot, rpc_call,
    step_weapon_ticks, temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
use eve_server::services::sov_gameplay::{
    TENEBREX_SPOOL_UP_SECS, TYPE_ANSIBLEX_JUMP_BRIDGE, TYPE_TENEBREX_CYNO_JAMMER,
    UPGRADE_ADVANCED_LOGISTICS_NETWORK, UPGRADE_CYNOSURAL_NAVIGATION,
    UPGRADE_CYNOSURAL_SUPPRESSION, UPGRADE_MAJOR_THREAT_DETECTION, UPGRADE_PROSPECTING_ARRAY,
};
use eve_server::services::sov_services::{
    claim_system_sovereignty, is_hub_hacked, TYPE_MAGMATIC_GAS, TYPE_PHAROLUX_CYNO_BEACON,
    TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use glam::DVec3;

#[tokio::test]
async fn test_sov_hub_fuel_power_workforce_upgrades_sites_hacking_flex_and_tenebrex() {
    // DATA GAP: feat:sovereignty.sov-hub-fuel / sov-hub-upgrades — 44 wiki-derived (build 3538132) upgrade definitions & 0 startup-cost placeholder pending V24.01 SDE import (docs/BLOCKERS.md §9)
    // DATA GAP: feat:sovereignty.sov-power — server-only celestial power table (docs/BLOCKERS.md §9)
    // DATA GAP: feat:sovereignty.sov-workforce — server-only celestial workforce table (docs/BLOCKERS.md §9)
    // parity: feat:sovereignty.sov-hub-hacking
    // parity: feat:sovereignty.sov-structures-flex
    // parity: feat:modules.cynosural-field-jammer-structure
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj
    // DATA GAP: quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj — server-only star power table (docs/BLOCKERS.md §9)
    // parity: notify:OnSovHubHacked
    // parity: notify:OnCynoJammerChanged
    eve_testkit::attest(&[
        "feat:sovereignty.sov-hub-hacking",
        "feat:sovereignty.sov-structures-flex",
        "feat:modules.cynosural-field-jammer-structure",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj",
        "notify:OnSovHubHacked",
        "notify:OnCynoJammerChanged",
    ]);

    let db_path = temp_sqlite_path("sov_hub_flex");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _, _, _, _) = find_nullsec_planets(&ctx);
    let mut adj_null: Vec<u32> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.from_solar_system_id == null_sys
                && ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| s.security_status <= 0.0)
        })
        .map(|j| j.to_solar_system_id)
        .filter(|&sys| {
            ctx.sde
                .celestials_in_system(sys)
                .into_iter()
                .filter(|c| c.group_id == 7)
                .map(|c| eve_server::services::quasar_sov_hub::planet_yield(c.type_id).1)
                .sum::<u64>()
                >= 5_000
        })
        .collect();
    adj_null.sort_unstable();
    let neighbor_sys = adj_null.into_iter().next().expect("adjacent null-sec system with >= 5000 workforce");

    let hub_a = 1_060_000_001i64;
    let hub_b = 1_060_000_002i64;
    let hub_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx, 73_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 11_957, hub_pos + DVec3::new(40_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut rx = subscribe(&ctx, &mut s_dir).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default();
        snap.system_states.entry(neighbor_sys).or_default();
        snap.structures.insert(hub_a, structure(hub_a, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_b, structure(hub_b, CORP, neighbor_sys, TYPE_SOVEREIGNTY_HUB));
        for (sys, base_sh) in [(null_sys, 1_060_000_100i64), (neighbor_sys, 1_060_000_200i64)] {
            for (idx, cel) in ctx.sde.celestials_in_system(sys).into_iter().filter(|c| c.group_id == 7).enumerate() {
                let sh_id = base_sh + idx as i64;
                snap.structures.insert(sh_id, structure(sh_id, CORP, sys, eve_server::services::sov_services::TYPE_ORBITAL_SKYHOOK));
                snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
            }
        }
        for (iid, tid, loc, qty) in [
            (995_001, TYPE_MAGMATIC_GAS, hub_a, 5_000),
            (995_002, TYPE_SUPERIONIC_ICE, hub_a, 5_000),
            (995_003, UPGRADE_MAJOR_THREAT_DETECTION, hub_a, 1),
            (995_004, UPGRADE_PROSPECTING_ARRAY, hub_a, 1),
            (995_005, UPGRADE_CYNOSURAL_NAVIGATION, hub_a, 1),
            (995_006, UPGRADE_CYNOSURAL_SUPPRESSION, hub_a, 1),
            (995_007, UPGRADE_ADVANCED_LOGISTICS_NETWORK, hub_a, 1),
            (995_009, 87_710, hub_a, 1), // Workforce Mecha-Tooling 1 (200 p, +5,000 wf, 8 Ice/hr)
            (995_020, TYPE_ANSIBLEX_JUMP_BRIDGE, PILOT_SHIP, 1),
            (995_021, TYPE_PHAROLUX_CYNO_BEACON, PILOT_SHIP, 1),
            (995_022, TYPE_TENEBREX_CYNO_JAMMER, PILOT_SHIP, 1),
            (995_025, 4_247, PILOT_SHIP, 500), // Helium Fuel Block for Tenebrex fuel bay
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: loc,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hub_a, TYPE_SOVEREIGNTY_HUB, hub_pos)).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_a).expect("claim null_sys");
    claim_system_sovereignty(&ctx, neighbor_sys, ALLIANCE, CORP, hub_b).expect("claim neighbor_sys");

    // 1. Star power configuration (`resource.star.api.GetAllConfigurationsRequest`), Fuel Access Group, & Flex upgrade gating.
    let star_rsp = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest", Vec::new()).await;
    assert_eq!(star_rsp.status_code, STATUS_OK);
    let star_cfgs = fields(&star_rsp.payload.unwrap().value, 1);
    assert!(!star_cfgs.is_empty() && varint(&star_cfgs[0], 2).unwrap_or(0) >= 200);

    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_a), PyRep::Integer(42_001)])
            .await.expect("SetSovHubFuelAccessGroup"),
        PyRep::Integer(42_001)
    );

    let allowed_cid = 90_000_071i64;
    let blocked_cid = 90_000_072i64;
    let mut s_allowed = register_online_pilot(
        &ctx, 73_071, "allowed", "sec-1", allowed_cid, CORP, Some(ALLIANCE),
        null_sys, 100_000_071, 11_957, hub_pos + DVec3::new(40_000.0, 0.0, 0.0), 0,
    ).await;
    let mut s_blocked = register_online_pilot(
        &ctx, 73_072, "blocked", "sec-2", blocked_cid, CORP, Some(ALLIANCE),
        null_sys, 100_000_072, 11_957, hub_pos + DVec3::new(40_000.0, 0.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            995_010,
            InventoryItemRecord {
                item_id: 995_010,
                type_id: TYPE_MAGMATIC_GAS,
                owner_id: allowed_cid,
                location_id: hub_a,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        let mut members = std::collections::HashMap::new();
        members.insert(allowed_cid, 1i64);
        members.insert(blocked_cid, -1i64);
        ext.owner_groups.insert(42_001, eve_server::services::station_structure::structure_upwell_ops::AccessGroupRecord {
            group_id: 42_001,
            creator_id: PILOT,
            owner_corp_id: CORP,
            name: "Fuel Group".into(),
            description: "Sov Hub Fuel ACL".into(),
            members,
        });
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[PyRep::Integer(hub_a)]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_allowed, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[PyRep::Integer(hub_a)]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_blocked, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[PyRep::Integer(hub_a)]).await.unwrap(),
        PyRep::Bool(false)
    );

    let mut add_denied = id_req(1, 995_001);
    encode_varint_field(2, 50, &mut add_denied);
    encode_bytes_field(3, &idm(hub_a as u64), &mut add_denied);
    assert_eq!(rpc(&ctx, &mut s_blocked, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_denied).await.status_code, STATUS_FORBIDDEN);

    let mut add_allowed = id_req(1, 995_010);
    encode_varint_field(2, 50, &mut add_allowed);
    encode_bytes_field(3, &idm(hub_a as u64), &mut add_allowed);
    assert_eq!(rpc(&ctx, &mut s_allowed, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_allowed).await.status_code, STATUS_OK);
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(995_020), PyRep::String("Ansiblex".into())])
            .await.unwrap_err().code,
        "RequiredSovUpgradeOffline"
    );

    // 2. Cross-system Workforce import/export (`hub.workforce.api.ConfigureRequest`) + `hub.api.GetResourcesRequest`.
    let wf_before = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    let base_wf_a = varint(&field(&wf_before, 2).unwrap(), 1).unwrap_or(0);

    let (mut exp, mut exp_cfg, mut cfg_b) = (Vec::new(), Vec::new(), id_req(1, hub_b as u64));
    encode_bytes_field(3, &idm(null_sys as u64), &mut exp);
    encode_varint_field(2, 3_500, &mut exp);
    encode_bytes_field(3, &exp, &mut exp_cfg);
    encode_bytes_field(2, &exp_cfg, &mut cfg_b);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_b).await.status_code, STATUS_OK);

    let (mut src, mut imp, mut imp_cfg, mut cfg_a) = (Vec::new(), Vec::new(), Vec::new(), id_req(1, hub_a as u64));
    encode_bytes_field(1, &idm(neighbor_sys as u64), &mut src);
    encode_bytes_field(1, &src, &mut imp);
    encode_bytes_field(2, &imp, &mut imp_cfg);
    encode_bytes_field(2, &imp_cfg, &mut cfg_a);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_a).await.status_code, STATUS_OK);

    let wf_after = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64)).await.payload.unwrap().value;
    assert_eq!(varint(&field(&wf_after, 2).unwrap(), 1).unwrap_or(0), base_wf_a + 3_500);

    // 3. Fuel Hub, install + online upgrades, and verify Sovereign Combat + Prospecting anomalies spawn via `tick_background_cluster`!
    for (fuel_iid, amt) in [(995_001i64, 3_000u64), (995_002i64, 3_000u64)] {
        let mut add = id_req(1, fuel_iid as u64);
        encode_varint_field(2, amt, &mut add);
        encode_bytes_field(3, &idm(hub_a as u64), &mut add);
        assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", add).await.status_code, STATUS_OK);
    }
    let mut proc = Vec::new();
    for iid in [995_009u64, 995_003, 995_004, 995_005, 995_006, 995_007] {
        encode_bytes_field(1, &idm(iid), &mut proc);
    }
    for tid in [87_710u64, UPGRADE_MAJOR_THREAT_DETECTION as u64, UPGRADE_PROSPECTING_ARRAY as u64] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut cfg);
        encode_varint_field(2, 1, &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc);
    }
    encode_bytes_field(3, &idm(hub_a as u64), &mut proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc).await.status_code, STATUS_OK);

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let anoms = ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&null_sys).cloned().unwrap_or_default());
    // Threat Detection sites carry their wiki site names (Haven, Sanctum, ...); match the upgrade's site signature.
    assert!(anoms.iter().any(|a| a.anomaly_type == "Combat" && a.anomaly_id.starts_with(&format!("SOV-{hub_a}-{}", UPGRADE_MAJOR_THREAT_DETECTION))));
    assert!(anoms.iter().any(|a| a.anomaly_type == "Ore" && a.name.contains("Prospecting Array")));
    let sys_balls = ctx.systems.get_or_spawn(null_sys).list_balls().await.unwrap();
    assert!(
        sys_balls.iter().any(|b| b.type_id == 1_230 || b.type_id == 1_228),
        "Ore upgrade anomaly must spawn real mineable asteroid balls in space"
    );
    assert!(
        sys_balls.iter().any(|b| b.type_id == 37_453),
        "Combat upgrade anomaly must spawn real combat-capable NPC rat balls in space"
    );

    // 4. Hostile Data Analyzer (`22175`) hacking on Sov Hub emits `OnSovHubHacked` and disables upgrades!
    let hacker_cid = 90_000_077i64;
    let hacker_ship = 100_000_077i64;
    let mut s_hacker = register_online_pilot(
        &ctx, 73_010, "demo2", "demo2-secret", hacker_cid, 98_000_077, Some(99_000_077),
        null_sys, hacker_ship, 621, hub_pos + DVec3::new(2_500.0, 0.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(hacker_cid).or_default();
        for sid in [3_405u32, 21_718] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(
            995_100,
            InventoryItemRecord {
                item_id: 995_100,
                type_id: 22_175,
                owner_id: hacker_cid,
                location_id: hacker_ship,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(995_100, true);
    });
    ctx.invalidate_ship_fitting(hacker_ship);
    lock_target_via_rpc(&ctx, &mut s_hacker, null_sys, hacker_ship, hub_a).await;
    let _ = drain_notif_events(&ctx, PILOT);
    let hack_rep = rpc_call(&ctx, &mut s_hacker, "dogmaIM", "Activate", &[PyRep::Integer(995_100), PyRep::String("dataHacking".into()), PyRep::Integer(hub_a)])
        .await.expect("hack Sov Hub via Data Analyzer I");
    assert_eq!(hack_rep.dict_get("containerUnlocked"), Some(&PyRep::Bool(false)));
    assert!(!is_hub_hacked(&ctx, hub_a));
    rpc_call(&ctx, &mut s_hacker, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(hub_a)])
        .await.expect("start live hacking minigame on Sov Hub");
    let mut won = false;
    for _pass in 0..8 {
        for tile in 1..24 {
            for _ in 0..5 {
                if let Ok(step) = rpc_call(&ctx, &mut s_hacker, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(hub_a), PyRep::Integer(tile)]).await {
                    if step.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                        won = true;
                        break;
                    }
                } else {
                    break;
                }
            }
            if won {
                break;
            }
        }
        if won {
            break;
        }
    }
    assert!(won, "Data Analyzer hacking minigame on Sov Hub must complete with Won");
    assert!(is_hub_hacked(&ctx, hub_a));
    assert!(drain_notif_events(&ctx, PILOT).iter().any(|e| e == "OnSovHubHacked"));
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let hacked_balls = ctx.systems.get_or_spawn(null_sys).list_balls().await.unwrap();
    assert!(
        !hacked_balls.iter().any(|b| matches!(b.type_id, 1_230 | 1_228 | 37_453)),
        "hacking Sov Hub despawns upgrade-spawned asteroid and NPC balls from ballpark"
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(995_020), PyRep::String("Ansiblex".into())])
            .await.unwrap_err().code,
        "SovHubHacked"
    );

    ctx.advance_secs(3_605);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!is_hub_hacked(&ctx, hub_a));
    let respawned_balls = ctx.systems.get_or_spawn(null_sys).list_balls().await.unwrap();
    assert!(respawned_balls.iter().any(|b| b.type_id == 37_453), "expired hack restores fueled online upgrades and respawns site balls");

    // R-SOVFW-05: Verify 35840, 35841, 37534 all require their respective Sov Hub upgrades online
    for (flex_iid, flex_label) in [
        (995_020, "Ansiblex"),
        (995_021, "Pharolux"),
        (995_022, "Tenebrex"),
    ] {
        assert_eq!(
            rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(flex_iid), PyRep::String(flex_label.into())])
                .await.unwrap_err().code,
            "RequiredSovUpgradeOffline"
        );
    }

    // Online Cynosural Suppression upgrade (offlining Prospecting Array to stay within 1,100p / 10,000wf V24 budget)
    let mut on_proc = Vec::new();
    for (tid, online) in [
        (87_710u64, 1u64),
        (UPGRADE_MAJOR_THREAT_DETECTION as u64, 1),
        (UPGRADE_PROSPECTING_ARRAY as u64, 0),
        (UPGRADE_CYNOSURAL_SUPPRESSION as u64, 1),
    ] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut cfg);
        encode_varint_field(2, online, &mut cfg);
        encode_bytes_field(2, &cfg, &mut on_proc);
    }
    encode_bytes_field(3, &idm(hub_a as u64), &mut on_proc);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", on_proc).await.status_code, STATUS_OK);

    // 5. Tenebrex Cyno Jammer (`37534`): 5m spool-up, `OnCynoJammerChanged`, `CynoJammed`, & immediate jam stop on reinforcement!
    let deep_space = DVec3::new(50_000_000.0, 50_000_000.0, 50_000_000.0);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(PILOT_SHIP, 11_957, deep_space)).await;
    let tenebrex_rep = rpc_call(&ctx, &mut s_dir, "structureDeployment", "Anchor", &[PyRep::Integer(995_022), PyRep::String("Tenebrex Jammer".into())])
        .await.expect("anchor Tenebrex");
    let tenebrex_id = tenebrex_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    let _ = ctx.systems.get_or_spawn(null_sys).set_ball_position(PILOT_SHIP, deep_space + DVec3::new(60_000.0, 0.0, 0.0)).await;
    rpc_call(&ctx, &mut s_dir, "invbroker", "Add", &[PyRep::Integer(995_025), PyRep::Integer(tenebrex_id), PyRep::Integer(133), PyRep::Integer(500)])
        .await.expect("load Helium Fuel Blocks into Tenebrex via invbroker.Add");
    // Advance through initial vulnerability (15m) + FLEX anchoring timer (45m * (1 + strategic_index)) + onlining (15m):
    let strat_idx = ctx.db.with_snapshot(|s| s.system_states.get(&null_sys).map(|st| u64::from(st.strategic_index)).unwrap_or(0));
    ctx.advance_secs(15 * 60 + 45 * 60 * (1 + strat_idx) + 15 * 60 + 10);
    eve_server::services::station_structure::structure_lifecycle::sync_all_structures(&ctx);
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&tenebrex_id].state.clone()), "ShieldVulnerable");
    assert!(ctx.db.with_snapshot(|s| s.sov_runtime.tenebrex_spool_ready_unix.contains_key(&tenebrex_id)));
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(PILOT).or_default();
        for sid in [21_603u32, 3_335, 12_092, 22_761] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(
            995_200,
            InventoryItemRecord {
                item_id: 995_200,
                type_id: 21_096,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            995_201,
            InventoryItemRecord {
                item_id: 995_201,
                type_id: 16_273,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 2_000,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(995_200, true);
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);

    let jam_during_spool = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState");
    assert_eq!(jam_during_spool.dict_get("isJammed"), Some(&PyRep::Bool(false)));

    let _ = drain_notif_events(&ctx, PILOT);
    ctx.advance_secs(TENEBREX_SPOOL_UP_SECS + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(drain_notif_events(&ctx, PILOT).iter().any(|e| e == "OnCynoJammerChanged"));
    let jam_ready = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState");
    assert_eq!(jam_ready.dict_get("isJammed"), Some(&PyRep::Bool(true)));
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(5001, eve_db::FleetRecord { fleet_id: 5001, boss_id: PILOT, members: vec![PILOT], ..Default::default() });
    });
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(995_200), PyRep::String("cynosuralGeneration".into())])
            .await.unwrap_err().code,
        "CynoJammed"
    );

    ctx.db.with_mut_snapshot(|s| {
        if let Some(st) = s.structures.get_mut(&tenebrex_id) {
            st.shield_hp = 1_500.0;
        }
    });
    fit_siege_artillery(&ctx, hacker_cid, hacker_ship, 995_300);
    let t_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&tenebrex_id].position));
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hacker_ship, 19_722, t_pos + DVec3::new(2_500.0, 0.0, 0.0))).await;
    lock_target_via_rpc(&ctx, &mut s_hacker, null_sys, hacker_ship, tenebrex_id).await;
    for _ in 0..4 {
        rpc_call(&ctx, &mut s_hacker, "dogmaIM", "Activate", &[
            PyRep::Integer(995_300),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(tenebrex_id),
        ]).await.expect("attack Tenebrex");
        step_weapon_ticks(&ctx, null_sys).await;
        ctx.advance_secs(25);
        if ctx.db.with_snapshot(|s| s.structures[&tenebrex_id].state != "ShieldVulnerable") {
            break;
        }
    }
    let jam_reinf = rpc_call(&ctx, &mut s_dir, "beyonce", "GetCynoJammerState", &[]).await.expect("GetCynoJammerState");
    assert_eq!(jam_reinf.dict_get("isJammed"), Some(&PyRep::Bool(false)));
    assert!(rpc_call(&ctx, &mut s_dir, "dogmaIM", "Activate", &[PyRep::Integer(995_200), PyRep::String("cynosuralGeneration".into())]).await.is_ok());

    let _ = drain_quasar(&mut rx);
    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod equinox_mercenary_dens {
    use super::*;

//  Part 4 of Equinox Sovereignty closure tests: Dev Indices, ADM Vulnerability, Live Entosis & Command Nodes (`SO-5`).


use crate::quasar_sov_equinox_hub::structure;
use crate::sovereignty_equinox_gameplay::{
    drain_notif_events, find_nullsec_planets, fit_siege_artillery, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, register_online_pilot, rpc_call, temp_sqlite_path, ALLIANCE, CORP, PILOT,
    PILOT_SHIP,
};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_machonet::SessionState;
use eve_server::router::ServerContext;
use eve_server::services::sov_gameplay::{
    ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_NODE_BASE_CAPTURE_SECS,
};
use eve_server::services::sov_services::{claim_system_sovereignty, TYPE_SOVEREIGNTY_HUB};
use glam::DVec3;

fn pick_active_node(ctx: &ServerContext, hub_id: i64) -> (i64, u32, [f64; 3]) {
    ctx.db.with_snapshot(|s| {
        let (&nid, &(_, nsys, _)) = s
            .sov_runtime
            .command_nodes
            .iter()
            .find(|(_, &(hid, _, _))| hid == hub_id)
            .expect("active command node");
        (nid, nsys, s.item_positions[&nid])
    })
}

async fn entosis_node(
    ctx: &ServerContext,
    sess: &mut SessionState,
    cid: i64,
    ship_id: i64,
    ship_type: u32,
    mod_id: i64,
    node_id: i64,
    nsys: u32,
    npos: [f64; 3],
    wait_secs: u64,
) {
    let prev_sys = sess.solar_system_id.unwrap_or(nsys);
    if prev_sys != nsys {
        let _ = ctx.systems.get_or_spawn(prev_sys).remove_ball(ship_id).await;
    }
    sess.solar_system_id = Some(nsys);
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.solar_system_id = nsys;
            ch.active_ship_id = ship_id;
        }
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.location_id = nsys as i64;
        }
    });
    let cs = ctx.build_ship_combat_state(cid, ship_id, ship_type);
    let _ = ctx
        .systems
        .get_or_spawn(nsys)
        .spawn_ball_with_combat(make_ball(ship_id, ship_type, DVec3::from_array(npos) + DVec3::new(4_000.0, 0.0, 0.0)), cs)
        .await;
    lock_target_via_rpc(ctx, sess, nsys, ship_id, node_id).await;
    rpc_call(
        ctx,
        sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id), PyRep::String("default".into()), PyRep::Integer(node_id)],
    )
    .await
    .expect("activate Entosis Link on Command Node");
    ctx.advance_secs(wait_secs);
    eve_server::sp6_lifecycle::tick_background_cluster(ctx).await;
}

#[tokio::test]
async fn test_sov_dev_indices_adm_vulnerability_live_entosis_and_command_nodes() {
    // parity: feat:sovereignty.sov-vulnerability
    eve_testkit::attest(&[
        "feat:sovereignty.sov-vulnerability",
    ]);

    let db_path = temp_sqlite_path("sov_entosis_nodes");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, _, _, _, _) = find_nullsec_planets(&ctx);
    let hub_id = 1_070_000_001i64;
    let hub_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_def = register_online_pilot(
        &ctx, 74_001, "demo", "demo-secret", PILOT, CORP, Some(ALLIANCE),
        null_sys, PILOT_SHIP, 639, hub_pos + DVec3::new(2_500.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.system_states.entry(null_sys).or_default();
        entry.solar_system_id = null_sys;
        snap.structures.insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(hub_id, TYPE_SOVEREIGNTY_HUB, hub_pos)).await;
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_id).expect("claim sov");

    // 1. Live Military (NPC rat kill), Industrial (asteroid mining), and Strategic (claimed days) Indices + ADM!
    fit_siege_artillery(&ctx, PILOT, PILOT_SHIP, 996_000);
    ctx.db.with_mut_snapshot(|snap| {
        // Free `HiSlot2` turret hardpoint on Naglfar (3 turret hardpoints) so `Miner I` (`483`) stays online.
        snap.items.remove(&996_004);
        snap.items.remove(&996_005);
        snap.module_online_states.remove(&996_004);
        snap.skills.entry(PILOT).or_default().insert(
            3_386,
            TrainedSkillRecord { type_id: 3_386, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            996_050,
            InventoryItemRecord {
                item_id: 996_050,
                type_id: 483, // Miner I
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(996_050, true);
        // Fit Expanded Cargohold II (`1319`) so Naglfar cargo hold exceeds 1,500 m3 for >= 1,000 m3 ore threshold
        for (slot_idx, flag) in [InventoryFlag::LoSlot0, InventoryFlag::LoSlot1].into_iter().enumerate() {
            let mod_id = 996_060 + slot_idx as i64;
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 1_319,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mod_id, true);
        }
    });
    ctx.invalidate_ship_fitting(PILOT_SHIP);
    let cs_def = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 19_722);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(PILOT_SHIP, 19_722, hub_pos + DVec3::new(2_500.0, 0.0, 0.0)), cs_def).await;

    ctx.set_invention_roll_override(Some(0.50));
    let _ = drain_notif_events(&ctx, PILOT);

    // Negative check: ejecting from player ships (`ship.Eject`, `category_id == 6`) in sovereign nullsec
    // and destroying the empty hulls grants ZERO Military Index points!
    for ej_idx in 0..5i64 {
        let alt_cid = 90_090_001 + ej_idx;
        let alt_ship = 879_001 + ej_idx;
        let alt_pos = hub_pos + DVec3::new(3_200.0 + (ej_idx as f64) * 80.0, 0.0, 0.0);
        let mut s_alt = register_online_pilot(
            &ctx, (74_500 + ej_idx) as u64, &format!("ej{ej_idx}"), "ej-secret", alt_cid, CORP, Some(ALLIANCE),
            null_sys, alt_ship, 24_690, alt_pos, 0,
        ).await;
        rpc_call(&ctx, &mut s_alt, "ship", "Eject", &[]).await.expect("eject from player ship in nullsec");
        let mut hull_cs = ctx.build_type_combat_state(24_690);
        hull_cs.signature_radius = 40_000.0;
        hull_cs.max_shield_hp = 10.0;
        hull_cs.shield_hp = 10.0;
        hull_cs.max_armor_hp = 10.0;
        hull_cs.armor_hp = 10.0;
        hull_cs.max_structure_hp = 10.0;
        hull_cs.structure_hp = 10.0;
        let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(alt_ship, 24_690, alt_pos), hull_cs).await;
        lock_target_via_rpc(&ctx, &mut s_def, null_sys, PILOT_SHIP, alt_ship).await;
        let gun_id = if ej_idx % 2 == 0 { 996_000 } else { 996_002 };
        rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(alt_ship)])
            .await.expect("destroy ejected empty player hull");
        crate::sovereignty_equinox_gameplay::step_weapon_ticks(&ctx, null_sys).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.sov_npc_kills.get(&null_sys).copied().unwrap_or(0)), 0, "destroying 5 ejected player hulls must credit 0 NPC kills");
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].military_index), 0, "destroying 5 ejected player hulls must not raise Military Index");

    // Positive check: destroying 5 genuine SDE `category_id == 11` Guristas rats (`17005` Pith Extinguisher) advances Military Index to 1!
    for idx in 0..5i64 {
        let rat_id = 880_001 + idx;
        let mut rat_cs = ctx.build_type_combat_state(17_005);
        rat_cs.signature_radius = 40_000.0;
        rat_cs.max_shield_hp = 10.0;
        rat_cs.shield_hp = 10.0;
        rat_cs.max_armor_hp = 10.0;
        rat_cs.armor_hp = 10.0;
        rat_cs.max_structure_hp = 10.0;
        rat_cs.structure_hp = 10.0;
        let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(rat_id, 17_005, hub_pos + DVec3::new(3_500.0 + (idx as f64) * 100.0, 0.0, 0.0)), rat_cs).await;
        lock_target_via_rpc(&ctx, &mut s_def, null_sys, PILOT_SHIP, rat_id).await;
        let gun_id = if idx % 2 == 0 { 996_000 } else { 996_002 };
        rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(rat_id)])
            .await.expect("destroy NPC rat to advance Military Index");
        if idx == 0 {
            assert_eq!(
                ctx.db.with_snapshot(|s| s.system_states[&null_sys].military_index),
                0,
                "single NPC kill must not advance Military Index 1 (threshold is 5 kills)"
            );
        }
        crate::sovereignty_equinox_gameplay::step_weapon_ticks(&ctx, null_sys).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].military_index), 1);

    let rock_id = 880_010i64;
    ctx.systems
        .get_or_spawn(null_sys)
        .spawn_asteroid(
            eve_server::system_actor::AsteroidState {
                asteroid_id: rock_id,
                belt_id: 0,
                type_id: 1_228, // Scordite
                initial_units: 50_000,
                remaining_units: 50_000,
                unit_volume: 0.15,
                depleted: false,
                respawn_at_tick: None,
            },
            make_ball(rock_id, 1_228, hub_pos + DVec3::new(3_000.0, 0.0, 0.0)),
        )
        .await
        .expect("spawn Scordite asteroid");
    lock_target_via_rpc(&ctx, &mut s_def, null_sys, PILOT_SHIP, rock_id).await;
    rpc_call(&ctx, &mut s_def, "dogmaIM", "Activate", &[PyRep::Integer(996_050), PyRep::String("miningLaser".into()), PyRep::Integer(rock_id)])
        .await.expect("activate Miner I on Scordite");
    ctx.advance_secs(65);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&null_sys].industrial_index),
        0,
        "single 50 m3 mining cycle must not advance Industrial Index 1 (threshold is 1,000 m3)"
    );
    ctx.advance_secs(25 * 60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].industrial_index >= 1));
    let _ = rpc_call(&ctx, &mut s_def, "dogmaIM", "Deactivate", &[PyRep::Integer(996_050), PyRep::String("miningLaser".into())]).await;
    let _ = drain_notif_events(&ctx, PILOT);

    ctx.advance_secs(8 * 86_400);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].strategic_index), 1);
    let dev_notifs = ctx.drain_notifications_for_char(PILOT);
    let dev_notif = dev_notifs.iter().find(|n| n.event == "OnSolarSystemDevIndexChanged").expect("OnSolarSystemDevIndexChanged");
    assert_eq!(dev_notif.args, vec![PyRep::Integer(null_sys as i64)]);

    let sov_info = rpc_call(&ctx, &mut s_def, "sovMgr", "GetSystemSovereigntyInfo", &[PyRep::Integer(null_sys as i64)])
        .await.expect("GetSystemSovereigntyInfo");
    let adm = sov_info.dict_get("vulnerabilityOccupancyLevel").and_then(|v| v.as_f64()).unwrap();
    assert!(adm > 1.0);

    // 2. ADM-scaled midnight-wrapping vulnerability window (`center_hour_utc = 23`) via `structureVulnerability.UpdateReinforceTimesFromClient`.
    rpc_call(&ctx, &mut s_def, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(hub_id), PyRep::Integer(23), PyRep::Integer(0)])
        .await.expect("UpdateReinforceTimesFromClient");
    let (v_start, v_end) = ctx.db.with_snapshot(|s| s.sov_runtime.adm_vulnerability_windows[&hub_id]);
    assert!(
        v_start > v_end,
        "23:00 UTC center with ADM > 1.0 must wrap across midnight (start_sec={v_start} > end_sec={v_end})"
    );

    // Advance to 10:00 UTC (outside the wrapped [~17:22 .. ~04:37] window)
    let cur_sec = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 10 * 3_600 - cur_sec) % 86_400);

    let atk_cid = 90_000_077i64;
    let atk_corp = 98_000_077i64;
    let atk_ally = 99_000_077i64;
    let atk_ship = 100_000_077i64;
    let entosis_mod = 996_200i64;
    let stront_item = 996_201i64;
    let mut s_atk = register_online_pilot(
        &ctx, 74_010, "demo2", "demo2-secret", atk_cid, atk_corp, Some(atk_ally),
        null_sys, atk_ship, 621, hub_pos + DVec3::new(5_000.0, 0.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(atk_cid).or_default().insert(
            3_426,
            TrainedSkillRecord { type_id: 3_426, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            entosis_mod,
            InventoryItemRecord {
                item_id: entosis_mod,
                type_id: 34_593,
                owner_id: atk_cid,
                location_id: atk_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            stront_item,
            InventoryItemRecord {
                item_id: stront_item,
                type_id: 16_275,
                owner_id: atk_cid,
                location_id: atk_ship,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(entosis_mod, true);
    });
    let atk2_cid = 90_000_078i64;
    let atk2_corp = 98_000_078i64;
    let atk2_ally = 99_000_078i64;
    let atk2_ship = 100_000_078i64;
    let entosis_mod2 = 996_202i64;
    let stront_item2 = 996_203i64;
    let mut s_atk2 = register_online_pilot(
        &ctx, 74_011, "demo3", "demo3-secret", atk2_cid, atk2_corp, Some(atk2_ally),
        null_sys, atk2_ship, 621, hub_pos + DVec3::new(6_000.0, 0.0, 0.0), 0,
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(atk2_cid).or_default().insert(
            3_426,
            TrainedSkillRecord { type_id: 3_426, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            entosis_mod2,
            InventoryItemRecord {
                item_id: entosis_mod2,
                type_id: 34_593,
                owner_id: atk2_cid,
                location_id: atk2_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            stront_item2,
            InventoryItemRecord {
                item_id: stront_item2,
                type_id: 16_275,
                owner_id: atk2_cid,
                location_id: atk2_ship,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(entosis_mod2, true);
    });
    ctx.invalidate_ship_fitting(atk2_ship);
    let def_ship = 100_000_002i64;
    let def_entosis_mod = 996_070i64;
    let def_stront = 996_071i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            def_ship,
            InventoryItemRecord {
                item_id: def_ship,
                type_id: 639,
                owner_id: PILOT,
                location_id: null_sys as i64,
                flag: InventoryFlag::None,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            def_entosis_mod,
            InventoryItemRecord {
                item_id: def_entosis_mod,
                type_id: 34_593,
                owner_id: PILOT,
                location_id: def_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            def_stront,
            InventoryItemRecord {
                item_id: def_stront,
                type_id: 16_275,
                owner_id: PILOT,
                location_id: def_ship,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(def_entosis_mod, true);
    });
    ctx.invalidate_ship_fitting(def_ship);
    ctx.invalidate_ship_fitting(atk_ship);
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, hub_id).await;

    assert_eq!(
        rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
            .await.unwrap_err().code,
        "SovHubInvulnerableOutsideWindow"
    );

    // Advance 15 hours to ~01:00 UTC (after midnight, inside the wrapped [~17:22 .. ~04:37] window)
    ctx.advance_secs(15 * 3_600);
    let act1 = rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("activate Entosis Link I inside midnight-wrapped vulnerability window");
    assert_eq!(act1.dict_get("cycleIndex").and_then(|v| v.as_i64()), Some(1));
    assert!(ctx.db.with_snapshot(|s| s.items[&stront_item].quantity) < 100);

    let hub_capture_wait = 320 + ((ENTOSIS_HUB_BASE_CAPTURE_SECS * adm).ceil() as u64) + 30;
    ctx.advance_secs(hub_capture_wait);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "HullReinforced");
    ctx.advance_secs(48 * 3_600 + 1);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let nodes: Vec<(i64, u32, [f64; 3])> = ctx.db.with_snapshot(|s| {
        let mut v: Vec<_> = s.sov_runtime.command_nodes.iter().map(|(&nid, &(_, nsys, _))| (nid, nsys, s.item_positions[&nid])).collect();
        v.sort_by_key(|x| x.0);
        v
    });
    assert_eq!(nodes.len(), 4);

    let init_prog = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((init_prog - 0.60).abs() < 1e-5, "initial campaign progress must start at 0.60 (60%)");

    // 3. Contested Command Node captures & continuous respawn preventing deadlock!
    // Defender captures 2 nodes (+0.07 each -> 0.74), Attacker captures 2 nodes (-0.07 each -> 0.60).
    // Active nodes remain 4 after every capture.
    let node_wait = 320 + ((ENTOSIS_NODE_BASE_CAPTURE_SECS * adm).ceil() as u64) + 30;

    let (n1, sys1, pos1) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_def, PILOT, def_ship, 639, def_entosis_mod, n1, sys1, pos1, node_wait).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 4, "defender capture 1 respawns node");
    let p1 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p1 - 0.67).abs() < 1e-4);

    let (n2, sys2, pos2) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_atk, atk_cid, atk_ship, 621, entosis_mod, n2, sys2, pos2, node_wait).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 4, "attacker capture 1 respawns node");
    let p2 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p2 - 0.60).abs() < 1e-4);

    let (n3, sys3, pos3) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_def, PILOT, def_ship, 639, def_entosis_mod, n3, sys3, pos3, node_wait).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 4, "defender capture 2 respawns node");
    let p3 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p3 - 0.67).abs() < 1e-4);

    let (n4, sys4, pos4) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_atk, atk_cid, atk_ship, 621, entosis_mod, n4, sys4, pos4, node_wait).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 4, "attacker capture 2 respawns node (2-2 split)");
    let p4 = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p4 - 0.60).abs() < 1e-4);

    // Defender captures 6 nodes to push progress from 0.60 to >= 1.0 (0.60 + 6 * 0.07 = 1.02 >= 1.0)
    for cap_idx in 0..5 {
        let (nid, nsys, npos) = pick_active_node(&ctx, hub_id);
        entosis_node(&ctx, &mut s_def, PILOT, def_ship, 639, def_entosis_mod, nid, nsys, npos, node_wait).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 4, "defender intermediate capture {cap_idx}");
    }
    let (n_final, sys_final, pos_final) = pick_active_node(&ctx, hub_id);
    entosis_node(&ctx, &mut s_def, PILOT, def_ship, 639, def_entosis_mod, n_final, sys_final, pos_final, node_wait).await;

    // Defender victory assertion: hub restored to "Online" (invulnerable), sovereign alliance remains ALLIANCE
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "Online");
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), Some(ALLIANCE));
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 0, "all command nodes despawned on resolution");

    // 4. Attackers re-entosis Sov Hub and capture 9 nodes unified under Attacker side to destroy hub & clear sovereignty!
    s_atk.solar_system_id = Some(null_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&atk_cid) { ch.solar_system_id = null_sys; }
        if let Some(it) = snap.items.get_mut(&atk_ship) { it.location_id = null_sys as i64; }
    });
    let cs_atk_re = ctx.build_ship_combat_state(atk_cid, atk_ship, 621);
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball_with_combat(make_ball(atk_ship, 621, hub_pos + DVec3::new(5_000.0, 0.0, 0.0)), cs_atk_re).await;
    lock_target_via_rpc(&ctx, &mut s_atk, null_sys, atk_ship, hub_id).await;
    let cur_sec = ctx.now_unix() % 86_400;
    ctx.advance_secs((86_400 + 23 * 3_600 - cur_sec) % 86_400);
    rpc_call(&ctx, &mut s_atk, "dogmaIM", "Activate", &[PyRep::Integer(entosis_mod), PyRep::String("default".into()), PyRep::Integer(hub_id)])
        .await.expect("activate Entosis Link on Sov Hub for second campaign");
    ctx.advance_secs(hub_capture_wait);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "HullReinforced");
    ctx.advance_secs(48 * 3_600 + 1);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 4);

    // Multi-attacker unification: Attacker 1 captures 5 nodes (-0.35, prog drops 0.60 -> 0.25).
    // Attacker 2 captures 4 nodes (-0.28, prog drops 0.25 -> 0.0, attackers win!).
    // Per EVE rules: On attacker victory, Sovereignty Hub is destroyed, system sov is cleared.
    for _ in 0..5 {
        let (nid, nsys, npos) = pick_active_node(&ctx, hub_id);
        entosis_node(&ctx, &mut s_atk, atk_cid, atk_ship, 621, entosis_mod, nid, nsys, npos, node_wait).await;
    }
    let p_mid = ctx.db.with_snapshot(|s| s.sov_runtime.command_node_scores[&hub_id][0].1);
    assert!((p_mid - 0.25).abs() < 1e-4, "progress drops to 0.25 after 5 attacker-1 captures");

    for _ in 0..4 {
        let (nid, nsys, npos) = pick_active_node(&ctx, hub_id);
        entosis_node(&ctx, &mut s_atk2, atk2_cid, atk2_ship, 621, entosis_mod2, nid, nsys, npos, node_wait).await;
    }

    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "Destroyed");
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), None);
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&null_sys].strategic_index), 0);
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.command_nodes.len()), 0);

    drop(ctx);
    let ctx_restarted = open_sqlite_ctx(&db_path, false);
    assert_eq!(ctx_restarted.db.with_snapshot(|s| s.system_states[&null_sys].sov_alliance_id), None);
    assert_eq!(ctx_restarted.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "Destroyed");
    let _ = std::fs::remove_file(&db_path);
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod sov_upgrades_and_reagents {
    use super::*;

//  End-to-end parity closure tests (Part 1) for Equinox Sovereignty Hub
//  anchoring, fuel, upgrades, power, workforce, reagents, and Mercenary Dens.


use crate::quasar_sov_equinox_hub::{field, id_req, idm, rpc, structure, subscribe, varint};
use crate::sovereignty_equinox_gameplay::{
    drain_quasar, find_nullsec_planets, fit_subcap_blasters, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, set_pilot_location,
    step_weapon_ticks, temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};

fn err_code<T: std::fmt::Debug>(res: Result<T, eve_proto::BridgeRpcError>) -> String {
    res.unwrap_err().code
}
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::services::quasar_sov_hub::{compute_hub_resources, simulate_fuel, star_power};
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::{
    TYPE_MERCENARY_DEN, UPGRADE_MAJOR_THREAT_DETECTION, UPGRADE_PROSPECTING_ARRAY,
};
use eve_server::services::sov_services::{
    claim_system_sovereignty, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE,
};
use eve_server::services::station_structure::structure_lifecycle as life;
use glam::DVec3;

#[tokio::test]
async fn test_sov_hub_anchor_upgrades_power_and_fuel_closure() {
    // parity: feat:sovereignty.sov-hub-fuel
    // parity: feat:sovereignty.sov-hub-upgrades
    // parity: feat:sovereignty.sov-power
    eve_testkit::attest(&[
        "feat:sovereignty.sov-hub-fuel",
        "feat:sovereignty.sov-hub-upgrades",
        "feat:sovereignty.sov-power",
    ]);

    let db_path = temp_sqlite_path("parity_sov_hub_core");
    let ctx = open_sqlite_ctx(&db_path, true);
    crate::common::pin_clock(&ctx);
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status <= 0.0
                && s.region_id < 11_000_000
                && ctx.sde.regions.get(&s.region_id).is_none_or(|r| r.faction_id.is_none())
        })
        .find(|s| eve_server::services::quasar_sov_hub::system_yield(&ctx.sde, s.solar_system_id).planet_workforce >= 2_500)
        .map(|s| s.solar_system_id)
        .unwrap_or_else(|| find_nullsec_planets(&ctx).0);
    let hub_pos = DVec3::new(150_000.0, 0.0, 0.0);

    let mut s_dir = register_online_pilot(
        &ctx,
        88_001,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        30_000_142,
        PILOT_SHIP,
        639,
        hub_pos,
        corp_roles::ROLE_DIRECTOR,
    )
    .await;

    // Negative: high-sec anchoring fails closed.
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_dir,
                "structureDeployment",
                "Anchor",
                &[PyRep::Integer(TYPE_SOVEREIGNTY_HUB as i64)],
            )
            .await
        ),
        "CannotAnchorSovHubOutsideNullSec"
    );

    // Move to unclaimed null-sec, anchor Sovereignty Hub from cargo, and complete onlining.
    let _ = ctx.systems.get_or_spawn(30_000_142).remove_ball(PILOT_SHIP).await;
    set_pilot_location(&ctx, &mut s_dir, null_sys, None);
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 639, hub_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            988_001,
            InventoryItemRecord {
                item_id: 988_001,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });

    let anchor_rep = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(988_001), PyRep::String("Equinox Hub".into())],
    )
    .await
    .expect("anchor Sovereignty Hub");
    let hub_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states.get(&null_sys).and_then(|st| st.sov_alliance_id)),
        None
    );

    let anchor_end = ctx.db.with_snapshot(|s| s.structures.get(&hub_id).unwrap().reinforced_until_unix);
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(anchor_end + 1)));
    life::sync_all_structures(&ctx);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states.get(&null_sys).and_then(|st| st.sov_alliance_id)),
        Some(ALLIANCE)
    );

    // Fuel Access Group via SetSovHubFuelAccessGroup.
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id), PyRep::Integer(55_001)])
            .await
            .expect("SetSovHubFuelAccessGroup"),
        PyRep::Integer(55_001)
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_dir, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[PyRep::Integer(hub_id)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );

    // Anchor Skyhooks on all planets so system power/workforce are harvested, and verify star power.
    let expected_star_power: u64 = ctx
        .sde
        .celestials_in_system(null_sys)
        .into_iter()
        .filter(|c| c.group_id == 6)
        .map(|c| star_power(&ctx.sde, c.type_id))
        .sum();
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, cel) in ctx.sde.celestials_in_system(null_sys).into_iter().filter(|c| c.group_id == 7).enumerate() {
            let sh_id = 1_098_000_100 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        for (iid, tid, qty) in [
            (988_010, TYPE_SUPERIONIC_ICE, 100),
            (988_011, 87_703, 1), // Power Monitoring Division 1 (+200 power, 2,500 wf, 8 Ice/hr)
            (988_012, UPGRADE_MAJOR_THREAT_DETECTION, 1), // 400 power, 2,700 wf
            (988_013, UPGRADE_PROSPECTING_ARRAY, 1),      // 450 power, 6,400 wf
            (988_014, 87_948, 1),                         // Minor Threat Detection: 400 power, 4,000 wf (overloads power)
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: hub_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });

    let mut rx = subscribe(&ctx, &mut s_dir).await;
    let base_power = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().power_available);
    assert!(base_power >= expected_star_power);

    // Add 16 Superionic Ice (enough for 2 hours of Power Monitoring Division 1 at 8 Ice/hr).
    let mut add_fuel = id_req(1, 988_010);
    encode_varint_field(2, 16, &mut add_fuel);
    encode_bytes_field(3, &idm(hub_id as u64), &mut add_fuel);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.fuel.api.AddRequest", add_fuel).await.status_code, STATUS_OK);

    // Configure Power Monitoring Division 1 + Major Threat Detection + 2x Prospecting Array (overloads power).
    let mut proc_cfg = Vec::new();
    for iid in [988_011u64, 988_012, 988_013, 988_014] {
        encode_bytes_field(1, &idm(iid), &mut proc_cfg);
    }
    for tid in [87_703u64, UPGRADE_MAJOR_THREAT_DETECTION as u64, UPGRADE_PROSPECTING_ARRAY as u64, 87_948u64] {
        let mut cfg = Vec::new();
        encode_bytes_field(1, &idm(tid), &mut cfg);
        encode_varint_field(2, 1, &mut cfg);
        encode_bytes_field(2, &cfg, &mut proc_cfg);
    }
    encode_bytes_field(3, &idm(hub_id as u64), &mut proc_cfg);
    assert_eq!(
        rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", proc_cfg)
            .await
            .status_code,
        STATUS_OK
    );
    let _ = drain_quasar(&mut rx);

    let res_after = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap());
    assert_eq!(res_after.power_available, base_power + 200);
    assert!(res_after.power_allocated <= res_after.power_available, "power overload must take excess upgrades offline");

    // Hourly fuel burn: 2 hours burns 16 Ice, 3rd hour empties bay and offlines fuel-burning upgrade.
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let hub = snap.sov_runtime.sov_hubs.get_mut(&hub_id).unwrap();
        hub.last_updated_unix = now;
        let _ = simulate_fuel(&ctx.sde, hub, now + 7_200);
        assert_eq!(hub.fuel.get(&TYPE_SUPERIONIC_ICE).copied().unwrap_or(0), 0);
        assert!(simulate_fuel(&ctx.sde, hub, now + 10_800));
        let pwr_mon = hub.upgrades.iter().find(|u| u.type_id == 87_703).unwrap();
        assert!(!pwr_mon.online || pwr_mon.low_power, "empty fuel bay must offline fuel-consuming upgrade");
    });

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_sov_workforce_reagents_and_mercenary_den_closure() {
    // parity: feat:sovereignty.sov-workforce
    // parity: feat:sovereignty.sov-reagents
    // parity: feat:sovereignty.mercenary-den
    eve_testkit::attest(&[
        "feat:sovereignty.sov-workforce",
        "feat:sovereignty.sov-reagents",
        "feat:sovereignty.mercenary-den",
    ]);

    let db_path = temp_sqlite_path("parity_sov_wf_reagents_den");
    let ctx = open_sqlite_ctx(&db_path, true);
    crate::common::pin_clock(&ctx);
    let (null_sys, temp_planet, lava_planet, ice_planet, _) = find_nullsec_planets(&ctx);
    let mut adj_null: Vec<u32> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| {
            j.from_solar_system_id == null_sys
                && ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| {
                    s.security_status <= 0.0
                        && ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_none()
                        && ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_none()
                })
        })
        .map(|j| j.to_solar_system_id)
        .filter(|&sys| {
            ctx.sde
                .celestials_in_system(sys)
                .into_iter()
                .filter(|c| c.group_id == 7)
                .map(|c| eve_server::services::quasar_sov_hub::planet_yield(c.type_id).1)
                .sum::<u64>()
                >= 2_000
        })
        .collect();
    adj_null.sort_unstable();
    let neighbor_sys = adj_null.into_iter().next().expect("adjacent null-sec system with >= 2000 workforce");

    let cels = ctx.sde.celestials_in_system(null_sys);
    let lava_pos = cels.iter().find(|c| c.celestial_id == lava_planet).unwrap().position;
    let ice_pos = cels.iter().find(|c| c.celestial_id == ice_planet).unwrap().position;
    let temp_pos = cels.iter().find(|c| c.celestial_id == temp_planet).unwrap().position;
    let (hub_a, hub_b, sh_temp) = (1_099_000_001i64, 1_099_000_002i64, 1_099_000_003i64);

    let mut s_dir = register_online_pilot(
        &ctx,
        88_002,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        19_722,
        lava_pos,
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_dir).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().solar_system_id = null_sys;
        snap.system_states.entry(neighbor_sys).or_default().solar_system_id = neighbor_sys;
        snap.structures.insert(hub_a, structure(hub_a, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_b, structure(hub_b, CORP, neighbor_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(sh_temp, structure(sh_temp, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(sh_temp, temp_planet);
        for (idx, cel) in ctx.sde.celestials_in_system(neighbor_sys).into_iter().filter(|c| c.group_id == 7).enumerate() {
            let sh_id = 1_099_000_200 + idx as i64;
            snap.structures.insert(sh_id, structure(sh_id, CORP, neighbor_sys, TYPE_ORBITAL_SKYHOOK));
            snap.sov_runtime.skyhook_planets.insert(sh_id, cel.celestial_id);
        }
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord { type_id: TYPE_MERCENARY_DEN_MANAGEMENT, level: 2, skill_points: 50_000.0 },
        );
        for (iid, tid, qty) in [
            (988_101, TYPE_ORBITAL_SKYHOOK, 2),
            (988_102, TYPE_MERCENARY_DEN, 1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: PILOT,
                    location_id: PILOT_SHIP,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });
    claim_system_sovereignty(&ctx, null_sys, ALLIANCE, CORP, hub_a).expect("claim null_sys");
    claim_system_sovereignty(&ctx, neighbor_sys, ALLIANCE, CORP, hub_b).expect("claim neighbor_sys");
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(sh_temp, TYPE_ORBITAL_SKYHOOK, temp_pos)).await;

    // 1. Anchor Skyhooks on Lava & Ice planets and verify Magmatic Gas / Superionic Ice accumulation.
    let sh_lava = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(lava_planet)],
    )
    .await
    .expect("anchor Lava Skyhook")
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(PILOT_SHIP).await;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, ice_pos).await;
    let sh_ice = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64), PyRep::Integer(ice_planet)],
    )
    .await
    .expect("anchor Ice Skyhook")
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(PILOT_SHIP).await;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, DVec3::new(200_000.0, 0.0, 0.0)).await;
    let _ = rpc_call(&ctx, &mut s_dir, "sovMgr", "AcquireSkyhooks", &[PyRep::Integer(null_sys as i64)])
        .await
        .expect("AcquireSkyhooks");

    let ready_unix = ctx.db.with_snapshot(|s| s.structures.get(&sh_ice).unwrap().reinforced_until_unix);
    let now_unix = ctx.now_unix();
    if ready_unix + 7_200 > now_unix {
        ctx.advance_secs(ready_unix + 7_200 - now_unix);
    } else {
        ctx.advance_secs(7_200);
    }
    life::sync_all_structures(&ctx);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (gas_qty, ice_qty) = ctx.db.with_snapshot(|s| {
        let g = s.sov_runtime.skyhook_reagent_bays.get(&sh_lava).map(|b| b.1 + b.2).unwrap_or(0);
        let i = s.sov_runtime.skyhook_reagent_bays.get(&sh_ice).map(|b| b.1 + b.2).unwrap_or(0);
        (g, i)
    });
    assert!(gas_qty > 0 && ice_qty > 0, "Lava ({gas_qty}) and Ice ({ice_qty}) Skyhooks must accumulate reagents");

    // 2. Cross-system Workforce export/import via hub.workforce.api.ConfigureRequest.
    let wf_before = varint(
        &field(
            &rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64))
                .await
                .payload
                .unwrap()
                .value,
            2,
        )
        .unwrap(),
        1,
    )
    .unwrap_or(0);

    let (mut exp, mut exp_cfg, mut cfg_b) = (Vec::new(), Vec::new(), id_req(1, hub_b as u64));
    encode_bytes_field(3, &idm(null_sys as u64), &mut exp);
    encode_varint_field(2, 1_500, &mut exp);
    encode_bytes_field(3, &exp, &mut exp_cfg);
    encode_bytes_field(2, &exp_cfg, &mut cfg_b);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_b).await.status_code, STATUS_OK);

    let (mut src, mut imp, mut imp_cfg, mut cfg_a) = (Vec::new(), Vec::new(), Vec::new(), id_req(1, hub_a as u64));
    encode_bytes_field(1, &idm(neighbor_sys as u64), &mut src);
    encode_bytes_field(1, &src, &mut imp);
    encode_bytes_field(2, &imp, &mut imp_cfg);
    encode_bytes_field(2, &imp_cfg, &mut cfg_a);
    assert_eq!(rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_a).await.status_code, STATUS_OK);

    let wf_after = varint(
        &field(
            &rpc(&ctx, &mut s_dir, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_a as u64))
                .await
                .payload
                .unwrap()
                .value,
            2,
        )
        .unwrap(),
        1,
    )
    .unwrap_or(0);
    assert_eq!(wf_after, wf_before + 1_500);

    // 3. Anchor Mercenary Den on Temperate Skyhook, generate Infomorphs, and attack it.
    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(PILOT_SHIP).await;
    place_ship_ball(&ctx, null_sys, PILOT, PILOT_SHIP, 19_722, temp_pos + DVec3::new(3_000.0, 0.0, 0.0)).await;
    let den_id = rpc_call(
        &ctx,
        &mut s_dir,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(TYPE_MERCENARY_DEN as i64), PyRep::Integer(sh_temp), PyRep::Integer(temp_planet)],
    )
    .await
    .expect("anchor Mercenary Den")
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();

    let den_ready = ctx.db.with_snapshot(|s| s.structures.get(&den_id).unwrap().reinforced_until_unix);
    let now_u = ctx.now_unix();
    if den_ready + 3_600 > now_u {
        ctx.advance_secs(den_ready + 3_600 - now_u);
    } else {
        ctx.advance_secs(3_600);
    }
    life::sync_all_structures(&ctx);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let idks = ctx.db.with_snapshot(|s| {
        s.sov_runtime
            .mercenary_dens
            .get(&den_id)
            .map(|d| d.infomorphs)
            .unwrap_or(0)
    });
    assert!(idks > 0, "Mercenary Den must generate Infomorph Decryption Keys over time");

    // Attack Mercenary Den with blasters and verify shield HP drops.
    let blaster_base = 988_900i64;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, blaster_base);
    lock_target_via_rpc(&ctx, &mut s_dir, null_sys, PILOT_SHIP, den_id).await;
    let shield_before = ctx.db.with_snapshot(|s| s.structures.get(&den_id).unwrap().shield_hp);
    let _ = rpc_call(
        &ctx,
        &mut s_dir,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(blaster_base), PyRep::String("effects.ProjectileFired".into()), PyRep::Integer(den_id)],
    )
    .await;
    step_weapon_ticks(&ctx, null_sys).await;
    let shield_after = ctx.db.with_snapshot(|s| s.structures.get(&den_id).unwrap().shield_hp);
    assert!(shield_after < shield_before, "attacking Mercenary Den must reduce its shield HP");
    let _ = drain_quasar(&mut rx);

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

}

#[allow(dead_code, unused_imports)]
pub(crate) mod pve_boosters_and_blockade_runners {
    use super::*;

//  End-to-end parity closure tests (Part 2) for FW Boosters, Abyssal Bioadaptive
//  Cache rewards, Cynosural Field lighting rules, and Blockade Runner cargo-scan immunity.


use crate::sovereignty_equinox_gameplay::{
    fit_subcap_blasters, lock_target_via_rpc, open_sqlite_ctx, place_ship_ball,
    register_online_pilot, rpc_call, set_pilot_location, step_weapon_ticks, temp_sqlite_path,
    ALLIANCE, CORP, PILOT, PILOT_SHIP,
};

fn err_code<T: std::fmt::Debug>(res: Result<T, eve_proto::BridgeRpcError>) -> String {
    res.unwrap_err().code
}
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
};
use eve_proto::PyRep;
use eve_server::services::lp_store_catalog::{register_lp_store_offer, LPOffer};
use eve_server::services::pve_special_sites_abyssal::{
    abyssal_mutaplasmid_for_tier, BIOADAPTIVE_CACHE_TYPE_ID, EXTRACTION_SUBNODE_TYPE_ID,
    TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID, ZERO_POINT_CONDENSATE_TYPE_ID,
};
use eve_server::services::structure_acl_defense_rules::TYPE_MOBILE_CYNO_INHIBITOR;
use glam::DVec3;

#[tokio::test]
async fn test_fw_boosters_lp_store_and_consumption_closure() {
    // parity: feat:fw.fw-boosters
    eve_testkit::attest(&["feat:fw.fw-boosters"]);

    let db_path = temp_sqlite_path("parity_fw_boosters");
    let ctx = open_sqlite_ctx(&db_path, true);
    let militia_corp = 1_000_179i64; // 24th Imperial Crusade
    let booster_tid = 91_916u32;   // Imperial Electronics Booster I

    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|st| st.owner_corporation_id == militia_corp)
        .map(|st| st.station_id)
        .expect("24th Imperial Crusade station in SDE");
    let sys_id = ctx.sde.stations.get(&station_id).unwrap().solar_system_id;

    let mut sess = register_online_pilot(
        &ctx,
        88_003,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        None,
        sys_id,
        PILOT_SHIP,
        639,
        DVec3::ZERO,
        0,
    )
    .await;
    sess.station_id = Some(station_id);

    ctx.db.with_mut_snapshot(|snap| {
        register_lp_store_offer(
            snap,
            LPOffer {
                offer_id: 990_179,
                corp_id: militia_corp,
                type_id: booster_tid,
                quantity: 1,
                lp_cost: 2_500,
                isk_cost: IskCents::from_isk(1_000_000),
                req_items: Vec::new(),
                ak_cost: 0,
                min_standing: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.station_id = Some(station_id);
            ch.balance = IskCents::from_isk(5_000_000);
        }
        snap.skills.entry(PILOT).or_default().insert(
            3405, // Biology
            TrainedSkillRecord { type_id: 3405, level: 2, skill_points: 10_000.0 },
        );
    });

    // 1. GetAvailableOffersFromCorp lists the FW booster offer.
    let offers = rpc_call(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(militia_corp)])
        .await
        .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offer_list) = offers else { panic!("expected offer list"); };
    assert!(offer_list.iter().any(|o| o.dict_get("typeID").and_then(|v| v.as_i64()) == Some(i64::from(booster_tid))));

    // 2. Negative: insufficient LP fails closed.
    assert!(
        rpc_call(
            &ctx,
            &mut sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(militia_corp), PyRep::Integer(990_179), PyRep::Integer(1)],
        )
        .await
        .is_err()
    );

    // 3. Grant LP, purchase FW booster, and consume via dogmaIM.UseBooster.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.loyalty_points.insert(militia_corp, 5_000);
        }
    });
    rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(militia_corp), PyRep::Integer(990_179), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCharacter");

    let booster_item_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.owner_id == PILOT && i.location_id == station_id && i.type_id == booster_tid)
            .map(|i| i.item_id)
            .expect("purchased FW booster in station hangar")
    });

    rpc_call(&ctx, &mut sess, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_item_id)])
        .await
        .expect("UseBooster");
    let active = rpc_call(&ctx, &mut sess, "skillMgr2", "GetBoosters", &[])
        .await
        .expect("GetBoosters");
    let PyRep::Dict(active_dict) = active else { panic!("expected active boosters dict"); };
    assert!(active_dict.iter().any(|(k, _)| k.as_i64() == Some(i64::from(booster_tid))));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_abyssal_bioadaptive_cache_rewards_closure() {
    // parity: feat:abyssal.bioadaptive-cache-rewards
    eve_testkit::attest(&["feat:abyssal.bioadaptive-cache-rewards"]);

    let db_path = temp_sqlite_path("parity_abyssal_cache");
    let ctx = open_sqlite_ctx(&db_path, true);
    let origin_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| (0.45..0.85).contains(&s.security_status) && s.region_id < 11_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_659);

    let mut sess = register_online_pilot(
        &ctx,
        88_004,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        None,
        origin_sys,
        PILOT_SHIP,
        621, // Caracal (Cruiser)
        DVec3::new(10_000.0, 0.0, 0.0),
        0,
    )
    .await;

    // Enter T1 Exotic pocket (47761) via abyssalMgr RPCs and verify Bioadaptive Cache + Extraction Sub-node + T1 HP.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            988_401,
            InventoryItemRecord {
                item_id: 988_401,
                type_id: 47_761,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let dep1 = rpc_call(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(988_401)])
        .await
        .expect("AbyssalEntranceDeployment T1");
    let tr1 = dep1.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let ent1 = rpc_call(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(tr1)])
        .await
        .expect("AbyssalEntranceGateActivation T1");
    let abyss_sys_t1 = ent1.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let t1_balls = ctx.systems.get_or_spawn(abyss_sys_t1).list_balls().await.unwrap();
    let r1_cache_t1 = t1_balls
        .iter()
        .find(|b| b.type_id == BIOADAPTIVE_CACHE_TYPE_ID)
        .map(|b| b.id)
        .expect("Bioadaptive Cache ball in T1 Abyssal room");
    assert!(t1_balls.iter().any(|b| b.type_id == EXTRACTION_SUBNODE_TYPE_ID));
    let t1_hp = ctx
        .systems
        .get_or_spawn(abyss_sys_t1)
        .get_combat_state(r1_cache_t1)
        .await
        .unwrap()
        .unwrap()
        .max_structure_hp;

    // Enter T5 Exotic pocket (47891) for a second pilot and verify 25x higher hull HP.
    let pilot2 = 90_000_202i64;
    let ship2 = 100_000_202i64;
    let mut s2 = register_online_pilot(
        &ctx,
        88_005,
        "demo2",
        "demo2-secret",
        pilot2,
        CORP,
        None,
        origin_sys,
        ship2,
        621,
        DVec3::new(2_000_000.0, 0.0, 0.0),
        0,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            988_402,
            InventoryItemRecord {
                item_id: 988_402,
                type_id: 47_891,
                owner_id: pilot2,
                location_id: ship2,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let dep5 = rpc_call(&ctx, &mut s2, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(988_402)])
        .await
        .expect("AbyssalEntranceDeployment T5");
    let tr5 = dep5.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let ent5 = rpc_call(&ctx, &mut s2, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(tr5)])
        .await
        .expect("AbyssalEntranceGateActivation T5");
    let abyss_sys_t5 = ent5.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let t5_balls = ctx.systems.get_or_spawn(abyss_sys_t5).list_balls().await.unwrap();
    let r1_cache_t5 = t5_balls
        .iter()
        .find(|b| b.type_id == BIOADAPTIVE_CACHE_TYPE_ID && b.id != r1_cache_t1)
        .map(|b| b.id)
        .expect("Bioadaptive Cache ball in T5 Abyssal room");
    let t5_hp = ctx
        .systems
        .get_or_spawn(abyss_sys_t5)
        .get_combat_state(r1_cache_t5)
        .await
        .unwrap()
        .unwrap()
        .max_structure_hp;
    assert!(t5_hp >= t1_hp * 20.0, "T5 cache hull HP ({t5_hp}) must be much higher than T1 ({t1_hp})");

    // Destroy T1 Bioadaptive Cache with blasters and verify wreck container drops tier rewards.
    set_pilot_location(&ctx, &mut sess, abyss_sys_t1, None);
    place_ship_ball(&ctx, abyss_sys_t1, PILOT, PILOT_SHIP, 621, DVec3::new(0.0, 0.0, 1_000.0)).await;
    let blaster_base = 988_910i64;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, blaster_base);
    lock_target_via_rpc(&ctx, &mut sess, abyss_sys_t1, PILOT_SHIP, r1_cache_t1).await;
    let _ = rpc_call(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(blaster_base), PyRep::String("effects.ProjectileFired".into()), PyRep::Integer(r1_cache_t1)],
    )
    .await;
    step_weapon_ticks(&ctx, abyss_sys_t1).await;

    let expected_muta = abyssal_mutaplasmid_for_tier(1);
    let dropped_types: Vec<u32> = ctx.db.with_snapshot(|snap| {
        let wreck_id = snap
            .items
            .values()
            .find(|i| i.location_id == abyss_sys_t1 as i64 && i.item_id != r1_cache_t1 && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186))
            .map(|i| i.item_id)
            .expect("wreck container produced on Bioadaptive Cache destruction");
        snap.items.values().filter(|i| i.location_id == wreck_id).map(|i| i.type_id).collect()
    });
    assert!(dropped_types.contains(&expected_muta), "cache drop must include tier mutaplasmid");
    assert!(dropped_types.contains(&TRIGLAVIAN_SURVEY_DATABASE_TYPE_ID), "cache drop must include Triglavian survey database");
    assert!(dropped_types.contains(&47_761), "cache drop must include filament");
    assert!(dropped_types.contains(&ZERO_POINT_CONDENSATE_TYPE_ID), "cache drop must include zero-point condensate");

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_cyno_lighting_rules_and_blockade_runner_scan_immunity_closure() {
    // parity: feat:modules.cyno-lighting-rules
    // parity: feat:ships.blockade-runner-cargo-scan-immunity
    eve_testkit::attest(&[
        "feat:modules.cyno-lighting-rules",
        "feat:ships.blockade-runner-cargo-scan-immunity",
    ]);

    let db_path = temp_sqlite_path("parity_cyno_and_br");
    let ctx = open_sqlite_ctx(&db_path, true);
    let high_sys = 30_000_142u32; // Jita (0.95)
    let low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.44 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("low-sec system in SDE");
    let wh_sys = 31_000_001u32;   // Wormhole

    let mut s_recon = register_online_pilot(
        &ctx,
        88_006,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        high_sys,
        PILOT_SHIP,
        11_957, // Falcon (Force Recon)
        DVec3::new(10_000.0, 0.0, 0.0),
        0,
    )
    .await;

    // 1. Hull restriction: Rifter (587) cannot fit Cynosural Field Generator I (21096).
    let rifter_id = 100_000_801i64;
    let cyno_mod_id = 988_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rifter_id,
            InventoryItemRecord {
                item_id: rifter_id,
                type_id: 587,
                owner_id: PILOT,
                location_id: high_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cyno_mod_id,
            InventoryItemRecord {
                item_id: cyno_mod_id,
                type_id: 21_096,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            988_202,
            InventoryItemRecord {
                item_id: 988_202,
                type_id: 16_273, // Liquid Ozone
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                ..Default::default()
            },
        );
        snap.skills.entry(PILOT).or_default().insert(
            21_603, // Cynosural Field Theory V
            TrainedSkillRecord { type_id: 21_603, level: 5, skill_points: 256_000.0 },
        );
    });
    assert!(
        eve_dogma::ShipFittingEngine::from_sde(1, 587, &ctx.sde)
            .fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde)
            .is_err(),
        "normal cyno must not fit on non-Recon/non-BlackOps hull"
    );

    // 2. High-sec, non-fleet, and Mobile Cyno Inhibitor checks.
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CannotLightCynoInSystem"
    );

    let _ = ctx.systems.get_or_spawn(high_sys).remove_ball(PILOT_SHIP).await;
    set_pilot_location(&ctx, &mut s_recon, low_sys, None);
    place_ship_ball(&ctx, low_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "MustBeInFleet"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            77_001,
            FleetRecord {
                fleet_id: 77_001,
                boss_id: PILOT,
                members: vec![PILOT],
                ..Default::default()
            },
        );
        snap.items.insert(
            988_205,
            InventoryItemRecord {
                item_id: 988_205,
                type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id: PILOT,
                location_id: low_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(988_205, [10_000.0, 0.0, 0.0]);
    });
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CynoInhibited"
    );

    // Move outside inhibitor range, light cyno: consumes 50 Liquid Ozone on Force Recon, immobilizes, blocks eject.
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&988_205); });
    rpc_call(
        &ctx,
        &mut s_recon,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
    )
    .await
    .expect("light normal cyno on Force Recon");
    let ozone_left = ctx.db.with_snapshot(|s| s.items.get(&988_202).unwrap().quantity);
    assert_eq!(ozone_left, 450, "Force Recon with CFT V must consume 50 Liquid Ozone");
    assert_eq!(
        err_code(rpc_call(&ctx, &mut s_recon, "ship", "Eject", &[]).await),
        "CynosuralFieldActive"
    );

    // 3. Wormhole space: cyno can be lit, but jump drive cannot lock/jump to it.
    let _ = ctx.systems.get_or_spawn(low_sys).remove_ball(PILOT_SHIP).await;
    eve_server::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(&ctx, PILOT, PILOT_SHIP);
    ctx.advance_secs(601);
    set_pilot_location(&ctx, &mut s_recon, wh_sys, None);
    place_ship_ball(&ctx, wh_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;
    rpc_call(
        &ctx,
        &mut s_recon,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
    )
    .await
    .expect("light cyno in wormhole space");

    let jumper_cid = 90_000_301i64;
    let jumper_ship = 100_000_301i64;
    let mut s_jumper = register_online_pilot(
        &ctx,
        88_007,
        "jumper",
        "sec",
        jumper_cid,
        CORP,
        Some(ALLIANCE),
        low_sys,
        jumper_ship,
        22_428, // Redeemer
        DVec3::ZERO,
        0,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&77_001).unwrap().members.push(jumper_cid);
    });
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_jumper,
                "beyonce",
                "CmdJumpToFleetModuleBeacon",
                &[PyRep::Integer(PILOT), PyRep::Integer(wh_sys as i64), PyRep::Integer(0)],
            )
            .await
        ),
        "JumpDriveForbiddenInWormholeSpace"
    );

    // 4. Blockade Runner (Prorator 12733, group 1202) cargo scan immunity vs high-sec NPC customs scan.
    let br_ship = 100_000_401i64;
    let scanner_mod = 988_301i64;
    let contraband_item = 988_302i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            br_ship,
            InventoryItemRecord {
                item_id: br_ship,
                type_id: 12_733, // Prorator (Blockade Runner)
                owner_id: PILOT,
                location_id: high_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            contraband_item,
            InventoryItemRecord {
                item_id: contraband_item,
                type_id: 3_721, // Slaves (contraband in high-sec)
                owner_id: PILOT,
                location_id: br_ship,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
        snap.items.insert(
            scanner_mod,
            InventoryItemRecord {
                item_id: scanner_mod,
                type_id: 442, // Cargo Scanner I
                owner_id: jumper_cid,
                location_id: jumper_ship,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(low_sys).remove_ball(jumper_ship).await;
    set_pilot_location(&ctx, &mut s_jumper, high_sys, None);
    place_ship_ball(&ctx, high_sys, jumper_cid, jumper_ship, 22_428, DVec3::new(1_000.0, 0.0, 0.0)).await;
    place_ship_ball(&ctx, high_sys, PILOT, br_ship, 12_733, DVec3::new(5_000.0, 0.0, 0.0)).await;
    lock_target_via_rpc(&ctx, &mut s_jumper, high_sys, jumper_ship, br_ship).await;

    let scan_res = rpc_call(
        &ctx,
        &mut s_jumper,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(scanner_mod), PyRep::String("effects.CargoScan".into()), PyRep::Integer(br_ship)],
    )
    .await
    .expect("cargo scan Blockade Runner");
    assert_eq!(scan_res.dict_get("cargoScanImmune").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(scan_res.dict_get("totalScannedQuantity").and_then(|v| v.as_i64()), Some(0));

    // High-sec NPC customs contraband scan still detects & confiscates contraband on Blockade Runner.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT) {
            ch.solar_system_id = high_sys;
            ch.active_ship_id = br_ship;
            ch.station_id = None;
        }
    });
    let confiscated = eve_server::services::combat_npc_police::scan_and_enforce_highsec_contraband(&ctx, high_sys, br_ship);
    assert!(!confiscated.is_empty(), "high-sec NPC customs must still confiscate contraband from Blockade Runner");
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&contraband_item)));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

}
