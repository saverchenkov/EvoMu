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
    if seed_demo {
        let tpl = eve_db::WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters.values().next().cloned().expect("demo char"));
        repo.with_mut_snapshot(|snap| {
            snap.characters.entry(tpl.character_id).or_insert(tpl);
        });
        repo.provision_player_account("demo", "demo-secret");
        repo.provision_player_account("demo2", "demo2-secret");
    }
    let ctx = ServerContext::with_repository(std::sync::Arc::new(repo));
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
    rpc_call(ctx, session, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)])
        .await
        .expect("dogmaIM.AddTarget");
    let sys = ctx.systems.get_or_spawn(sys_id);
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
        .filter(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
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
    // parity: feat:sovereignty.sov-reagents
    // parity: feat:sovereignty.skyhook-reagent-maturity
    // parity: feat:sovereignty.skyhook-theft
    // parity: notify:OnSkyhookFirstActivation
    // parity: notify:OnSkyhookCanBeHacked
    eve_testkit::attest(&[
        "feat:sovereignty.skyhook-acquire",
        "feat:sovereignty.sov-reagents",
        "feat:sovereignty.skyhook-reagent-maturity",
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
    assert_eq!((lava_t, lava_sec0, lava_insec0), (TYPE_MAGMATIC_GAS, 0, 500));
    assert_eq!((ice_t, ice_insec0), (TYPE_SUPERIONIC_ICE, 500));

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
    assert_eq!((lava_sec12, lava_insec12), (250, 250));
    assert_eq!((lava_sec12, lava_insec12), (ice_sec12, ice_insec12));

    let sk_proto = rpc(&ctx, &mut s_dir, "eve_public.sovereignty.skyhook.api.GetRequest", id_req(1, lava_skyhook as u64))
        .await.payload.unwrap().value;
    let sim = field(&field(&sk_proto, 2).unwrap(), 2).unwrap();
    assert_eq!((varint(&sim, 1), varint(&sim, 2)), (Some(250), Some(250)));

    // 3. Live Orbital Skyhook Reagent Silo ship-link theft (`AttackStructure` -> `<= 10%` shield -> `InitiateLink` -> 600s tick).
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
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_theft_windows.insert(lava_skyhook, (now - 60, now + 3_600));
    });
    let _ = drain_notif_events(&ctx, PILOT);
    for _ in 0..4 {
        // rpc-lint: allow canonical action_plan S-7 / feat:structures structureControl.AttackStructure live combat RPC
        rpc_call(&ctx, &mut s_thief, "structureControl", "AttackStructure", &[PyRep::Integer(lava_skyhook)])
            .await.expect("damage skyhook silo shield to <= 10%");
        step_weapon_ticks(&ctx, sys_id).await;
        ctx.advance_secs(25);
        if ctx.db.with_snapshot(|s| s.structures[&lava_skyhook].shield_hp) <= 1_000.0 {
            break;
        }
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
    assert_eq!((final_sec, final_insec), (377, 0));
    let balls = ctx.systems.get_or_spawn(sys_id).list_balls().await.unwrap();
    let can_ball = balls.iter().find(|b| b.type_id == 23).expect("Cargo Container 23 spawned in space");
    let stolen_qty: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.location_id == can_ball.id && i.type_id == TYPE_MAGMATIC_GAS).map(|i| i.quantity).sum()
    });
    assert_eq!(stolen_qty, 123);

    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(make_ball(PILOT_SHIP, 621, sky_ball_pos + DVec3::new(3_000.0, 0.0, 0.0))).await;
    let ext_rep = rpc_call(
        &ctx, &mut s_dir, "beyonce", "CallComponentFromClient",
        &[PyRep::Integer(lava_skyhook), PyRep::String("orbitalSkyhook".into()), PyRep::String("ExtractReagentsToShip".into())],
    ).await.expect("ExtractReagentsToShip");
    assert_eq!(ext_rep.dict_get("extractedQuantity").and_then(|v| v.as_i64()), Some(377));

    drop(ctx);
    let ctx_restarted = open_sqlite_ctx(&db_path, false);
    assert_eq!(ctx_restarted.db.with_snapshot(|s| s.sov_runtime.skyhook_planets.get(&lava_skyhook).copied()), Some(lava_planet));
    let _ = std::fs::remove_file(&db_path);
}
