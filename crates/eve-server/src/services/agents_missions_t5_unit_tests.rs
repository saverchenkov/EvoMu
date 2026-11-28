//! Track 5 (`#1496`) regression tests: `PVE-001`, `PVE-022`, `PVE-020`, `PVE-024`, `PVE-012`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::mission_fixture::{self, MissionFixture};
use eve_db::{ServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        if let Some(env_path) = std::env::var_os("EVOMU_SDE_PATH").filter(|v| !v.is_empty()) {
            let p = std::path::PathBuf::from(env_path);
            if p.exists() {
                return Arc::new(SdeRegistry::open(&p).expect("open EVOMU_SDE_PATH sqlite"));
            }
        }
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join("data/evomu_sde.sqlite");
            if candidate.exists() {
                return Arc::new(SdeRegistry::open(&candidate).expect("open data/evomu_sde.sqlite"));
            }
            if !dir.pop() {
                break;
            }
        }
        Arc::new(SdeRegistry::with_canonical_seed())
    }))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn docked_session(char_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

fn move_demo_character(ctx: &ServerContext, char_id: i64, sys_id: u32, station_id: Option<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap
            .characters
            .get_mut(&char_id)
            .expect("demo character must exist in WorldRepository::with_demo_seed()");
        ch.solar_system_id = sys_id;
        ch.station_id = station_id;
    });
}

#[tokio::test]
async fn pve_001_022_two_pilots_same_mission_have_distinct_sites_and_allocated_asteroids() {
    let mut sde = (*shared_sde()).clone();
    let mining_ag = sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.agent_type_id == 2
                && a.division_id == 23
                && a.solar_system_id.is_some()
                && a.station_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .cloned()
        .unwrap_or_else(|| {
            let mut ag = sde
                .agents
                .values()
                .find(|a| a.station_id.is_some() && a.solar_system_id.is_some())
                .expect("stationed agent")
                .clone();
            ag.agent_id = 3_990_101;
            ag.level = 1;
            ag.agent_type_id = 2;
            ag.division_id = 23;
            sde.agents.insert(ag.agent_id, ag.clone());
            ag
        });
    let mining_sys = mining_ag.solar_system_id.unwrap();
    let mining_st = mining_ag.station_id.unwrap();
    let mining_mid = sde
        .missions
        .values()
        .filter(|m| {
            eve_db::decide_mission_kind(Some(m), Some(&mining_ag)) == eve_db::MissionKind::Mining
        })
        .map(|m| m.mission_id)
        .min()
        .unwrap_or_else(|| {
            let tmpl = eve_sde::MissionTemplate {
                mission_id: 98_101,
                title: "Mining Test Mission".into(),
                is_courier: false,
                lp_alpha: 10,
                lp_omega: 20,
                has_standing_rewards: true,
                content_template: Some("BasicMining".into()),
                dungeon_id: None,
            };
            sde.missions.insert(tmpl.mission_id, tmpl);
            98_101
        });
    let sec_ag = sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.agent_type_id == 2
                && a.division_id == 24
                && a.solar_system_id.is_some()
                && a.station_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .cloned()
        .unwrap_or_else(|| mining_ag.clone());
    let sec_sys = sec_ag.solar_system_id.unwrap();
    let sec_st = sec_ag.station_id.unwrap();
    let combat_mid = sde
        .missions
        .values()
        .filter(|m| {
            eve_db::decide_mission_kind(Some(m), Some(&sec_ag))
                == eve_db::MissionKind::Encounter
                && !is_storyline_template(m)
        })
        .map(|m| m.mission_id)
        .min()
        .unwrap_or_else(|| {
            let tmpl = eve_sde::MissionTemplate {
                mission_id: 98_102,
                title: "Encounter Test Mission".into(),
                is_courier: false,
                lp_alpha: 10,
                lp_omega: 20,
                has_standing_rewards: true,
                content_template: Some("BasicKill".into()),
                dungeon_id: None,
            };
            sde.missions.insert(tmpl.mission_id, tmpl);
            98_102
        });

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let (char_a, char_b) = (90_000_001_i64, 90_000_002_i64);
    let now = ctx.now_unix();
    move_demo_character(&ctx, char_a, mining_sys, Some(mining_st));
    move_demo_character(&ctx, char_b, mining_sys, Some(mining_st));
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [char_a, char_b] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.standings.insert(mining_ag.corporation_id, 2.0);
                ch.standings.insert(sec_ag.corporation_id, 2.0);
            }
            mission_fixture::insert_with_sde(
                snap,
                &ctx.sde,
                MissionFixture::offered(cid, mining_mid, mining_ag.agent_id, mining_sys)
                    .origin_station_id(Some(mining_st))
                    .offered_at_unix(now)
                    .offer_expiry_unix(now + 86_400)
                    .required_delivery_type_id(Some(1230))
                    .required_delivery_qty(500),
            );
        }
    });

    let kw = HashMap::new();
    let mut sess_a = docked_session(char_a, mining_sys, Some(mining_st));
    let mut sess_b = docked_session(char_b, mining_sys, Some(mining_st));
    let acc_a = handle(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(mining_ag.agent_id),
            PyRep::Integer(i64::from(mining_mid)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("char_a accept mining");
    let acc_b = handle(
        &ctx,
        &mut sess_b,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(mining_ag.agent_id),
            PyRep::Integer(i64::from(mining_mid)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("char_b accept mining");

    let ast_a = acc_a
        .dict_get("asteroidEntityID")
        .and_then(PyRep::as_i64)
        .expect("ast_a");
    let ast_b = acc_b
        .dict_get("asteroidEntityID")
        .and_then(PyRep::as_i64)
        .expect("ast_b");
    let legacy_formula_id = 910_000_000 + i64::from(mining_mid) * 1000 + 1;
    assert_ne!(ast_a, ast_b);
    assert_ne!(ast_a, legacy_formula_id);
    assert_ne!(ast_b, legacy_formula_id);
    assert_eq!(acc_a.dict_get("spawnedNpcBallID"), Some(&PyRep::None));
    assert_eq!(acc_b.dict_get("spawnedNpcBallID"), Some(&PyRep::None));

    let base_station_pos = ctx
        .sde
        .solar_systems
        .get(&mining_sys)
        .and_then(|s| s.stations.first().map(|st| st.position))
        .unwrap_or(glam::DVec3::ZERO);

    ctx.db.with_snapshot(|snap| {
        let m_a = snap
            .missions
            .get_by_character_template(char_a, mining_mid)
            .expect("m_a");
        let m_b = snap
            .missions
            .get_by_character_template(char_b, mining_mid)
            .expect("m_b");
        assert_ne!(m_a.instance_id, m_b.instance_id);
        assert_ne!(m_a.bookmark_id, m_b.bookmark_id);
        assert_eq!(m_a.asteroid_entity_id, Some(ast_a));
        assert_eq!(m_b.asteroid_entity_id, Some(ast_b));
        assert!(snap.asteroids.contains_key(&ast_a) && snap.asteroids.contains_key(&ast_b));
        let pos_a = glam::DVec3::from(m_a.site_origin_f64().expect("site_origin_m a"));
        let pos_b = glam::DVec3::from(m_b.site_origin_f64().expect("site_origin_m b"));
        assert!(pos_a.distance(pos_b) >= 150_000.0);
        assert!(pos_a.distance(base_station_pos) >= 5_000_000.0);
        assert!(pos_b.distance(base_station_pos) >= 5_000_000.0);
    });

    ctx.systems
        .wake_for_session_arrival(
            mining_sys,
            crate::system_actor::SessionArrivalWakeReason::LoginInSpace,
        )
        .expect("wake mining_sys");
    ensure_mission_asteroid_spawned_on_pocket_entry(&ctx, char_a, mining_sys, Some(mining_mid))
        .await;
    let sys_h = ctx
        .systems
        .get_handle(mining_sys)
        .expect("mining sys actor available on pocket entry");
    assert!(sys_h.get_ball_snapshot(ast_a).await.unwrap().is_some());
    assert!(sys_h.get_ball_snapshot(ast_b).await.unwrap().is_none());
    ensure_mission_asteroid_spawned_on_pocket_entry(&ctx, char_b, mining_sys, Some(mining_mid))
        .await;
    assert!(sys_h.get_ball_snapshot(ast_b).await.unwrap().is_some());

    let _ = abandon_or_decline_mission(&ctx, &sess_a, char_a, mining_mid)
        .await
        .unwrap();
    assert!(sys_h.get_ball_snapshot(ast_a).await.unwrap().is_none());
    assert!(sys_h.get_ball_snapshot(ast_b).await.unwrap().is_some());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.asteroids.contains_key(&ast_a));
        assert!(snap.asteroids.contains_key(&ast_b));
    });

    move_demo_character(&ctx, char_a, sec_sys, Some(sec_st));
    move_demo_character(&ctx, char_b, sec_sys, Some(sec_st));
    sess_a.solar_system_id = Some(sec_sys);
    sess_a.station_id = Some(sec_st);
    sess_b.solar_system_id = Some(sec_sys);
    sess_b.station_id = Some(sec_st);
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [char_a, char_b] {
            mission_fixture::insert_with_sde(
                snap,
                &ctx.sde,
                MissionFixture::offered(cid, combat_mid, sec_ag.agent_id, sec_sys)
                    .origin_station_id(Some(sec_st))
                    .offered_at_unix(now)
                    .offer_expiry_unix(now + 86_400)
                    .npcs_remaining(2),
            );
        }
    });
    handle(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(sec_ag.agent_id),
            PyRep::Integer(i64::from(combat_mid)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("char_a accept combat");
    handle(
        &ctx,
        &mut sess_b,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(sec_ag.agent_id),
            PyRep::Integer(i64::from(combat_mid)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("char_b accept combat");
    ctx.db.with_snapshot(|snap| {
        let c_a = snap
            .missions
            .get_by_character_template(char_a, combat_mid)
            .expect("c_a");
        let c_b = snap
            .missions
            .get_by_character_template(char_b, combat_mid)
            .expect("c_b");
        assert_ne!(c_a.instance_id, c_b.instance_id);
        assert_ne!(c_a.bookmark_id, c_b.bookmark_id);
        let p_a = glam::DVec3::from(c_a.site_origin_f64().unwrap());
        let p_b = glam::DVec3::from(c_b.site_origin_f64().unwrap());
        assert!(p_a.distance(p_b) >= 150_000.0);
    });
}

#[tokio::test]
async fn pve_020_accept_is_single_atomic_commit_without_spawning_system_actor() {
    let ctx = test_ctx();
    let ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.agent_type_id == 2
                && a.solar_system_id.is_some()
                && a.station_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .expect("level 1 stationed agent")
        .clone();
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    let char_id = 90_000_001_i64;
    let now = ctx.now_unix();
    let kw = HashMap::new();
    move_demo_character(&ctx, char_id, sys_id, Some(st_id));
    let mut sess = docked_session(char_id, sys_id, Some(st_id));

    assert_eq!(
        ctx.systems.active_systems_count(),
        0,
        "no system actors loaded before accept"
    );

    let mid_enc = 98_201_u32;
    let mid_cour = 98_202_u32;
    let mid_mine = 98_203_u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(ag.corporation_id, 2.0);
        }
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, mid_enc, ag.agent_id, sys_id)
                .origin_station_id(Some(st_id))
                .offered_at_unix(now)
                .offer_expiry_unix(now + 86_400)
                .npcs_remaining(2),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, mid_cour, ag.agent_id, sys_id)
                .origin_station_id(Some(st_id))
                .destination_station_id(Some(st_id))
                .required_delivery_type_id(Some(3814))
                .required_delivery_qty(10)
                .offered_at_unix(now)
                .offer_expiry_unix(now + 86_400),
        );
        mission_fixture::insert_with_sde(
            snap,
            &ctx.sde,
            MissionFixture {
                state: "Offered".into(),
                required_delivery_type_id: Some(1230),
                required_delivery_qty: 500,
                origin_station_id: Some(st_id),
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                has_metadata: true,
                ..MissionFixture::offered(char_id, mid_mine, ag.agent_id, sys_id)
            },
        );
        let _ = snap.missions.update_by_character_template(char_id, mid_mine, |m| {
            m.kind = eve_db::MissionKind::Mining;
        });
    });

    for mid in [mid_enc, mid_cour, mid_mine] {
        let commits_before = ctx.db.tx_commit_count();
        let rep = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "DoAction",
            &[
                PyRep::Integer(3),
                PyRep::Integer(ag.agent_id),
                PyRep::Integer(i64::from(mid)),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("single-commit accept");
        assert_eq!(ctx.db.tx_commit_count(), commits_before + 1);
        assert_eq!(ctx.systems.active_systems_count(), 0);
        assert_eq!(rep.dict_get("spawnedNpcBallID"), Some(&PyRep::None));
        if mid == mid_mine {
            let ast_id = rep
                .dict_get("asteroidEntityID")
                .and_then(PyRep::as_i64)
                .expect("mining asteroidEntityID");
            assert!(ctx.db.with_snapshot(|s| s.asteroids.contains_key(&ast_id)));
        }
    }

    let mut sde_bad = (*shared_sde()).clone();
    let unstationed_sys = 31_000_001_u32;
    sde_bad
        .stations
        .retain(|_, st| st.solar_system_id != unstationed_sys);
    let mut bad_ag = ag.clone();
    bad_ag.agent_id = 3_999_001;
    bad_ag.station_id = None;
    bad_ag.solar_system_id = Some(unstationed_sys);
    bad_ag.agent_type_id = 5;
    sde_bad.agents.insert(bad_ag.agent_id, bad_ag.clone());

    let ctx_bad = ServerContext::with_sde_and_repository(
        Arc::new(sde_bad),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let bad_mid = 98_204_u32;
    let now_bad = ctx_bad.now_unix();
    move_demo_character(&ctx_bad, char_id, sys_id, Some(st_id));
    ctx_bad.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, bad_mid, bad_ag.agent_id, unstationed_sys)
                .origin_station_id(None)
                .destination_station_id(Some(st_id))
                .required_delivery_type_id(Some(3814))
                .required_delivery_qty(10)
                .offered_at_unix(now_bad)
                .offer_expiry_unix(now_bad + 86_400),
        );
    });
    let commits_pre = ctx_bad.db.tx_commit_count();
    let (bms_pre, items_pre, ast_pre) = ctx_bad.db.with_snapshot(|snap| {
        (
            snap.bookmarks.len(),
            snap.items.len(),
            snap.asteroids.len(),
        )
    });
    let err = handle(
        &ctx_bad,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(5),
            PyRep::Integer(bad_ag.agent_id),
            PyRep::Integer(i64::from(bad_mid)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InvalidMissionLocation");
    assert_eq!(ctx_bad.db.tx_commit_count(), commits_pre);
    assert_eq!(ctx_bad.systems.active_systems_count(), 0);
    ctx_bad.db.with_snapshot(|snap| {
        let m = snap
            .missions
            .get_by_character_template(char_id, bad_mid)
            .expect("rolled-back offer remains");
        assert_eq!(m.state, eve_db::MissionState::Offered);
        assert_eq!(m.bookmark_id, None);
        assert_eq!(m.site_origin_m, None);
        assert_eq!(snap.bookmarks.len(), bms_pre);
        assert_eq!(snap.items.len(), items_pre);
        assert_eq!(snap.asteroids.len(), ast_pre);
    });
}

#[tokio::test]
async fn pve_024_mission_failure_and_quit_apply_derived_faction_standings() {
    let ctx = test_ctx();
    let clock = Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        1_800_300_000,
    ));
    ctx.set_clock(clock.clone());

    let caldari_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.agent_type_id == 2
                && a.station_id.is_some()
                && a.solar_system_id.is_some()
                && ctx
                    .sde
                    .npc_corporations
                    .get(&a.corporation_id)
                    .and_then(|c| c.faction_id)
                    == Some(500_001)
        })
        .min_by_key(|a| a.agent_id)
        .expect("Caldari L1 stationed agent")
        .clone();
    let sys_id = caldari_ag.solar_system_id.unwrap();
    let st_id = caldari_ag.station_id.unwrap();
    let tmpl_id = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            eve_db::decide_mission_kind(Some(m), Some(&caldari_ag))
                == eve_db::MissionKind::Encounter
                && !is_storyline_template(m)
        })
        .map(|m| m.mission_id)
        .min()
        .expect("SDE regular encounter mission");

    let (char_fail, char_quit) = (90_000_001_i64, 90_000_002_i64);
    let now = ctx.now_unix();
    let kw = HashMap::new();
    move_demo_character(&ctx, char_fail, sys_id, Some(st_id));
    move_demo_character(&ctx, char_quit, sys_id, Some(st_id));
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [char_fail, char_quit] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.standings.clear();
            }
            mission_fixture::insert_with_sde(
                snap,
                &ctx.sde,
                MissionFixture::offered(cid, tmpl_id, caldari_ag.agent_id, sys_id)
                    .origin_station_id(Some(st_id))
                    .offered_at_unix(now)
                    .offer_expiry_unix(now + 86_400),
            );
        }
    });

    ctx.db
        .acquire_online_lock(1, 501, Some(char_fail), true, now)
        .expect("online lock char_fail");
    ctx.db
        .acquire_online_lock(2, 502, Some(char_quit), true, now)
        .expect("online lock char_quit");

    let mut sess_fail = docked_session(char_fail, sys_id, Some(st_id));
    sess_fail.session_id = 501;
    let mut sess_quit = docked_session(char_quit, sys_id, Some(st_id));
    sess_quit.session_id = 502;
    sess_quit.user_id = Some(2);
    handle(
        &ctx,
        &mut sess_fail,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(caldari_ag.agent_id),
            PyRep::Integer(i64::from(tmpl_id)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("accept fail mission");
    handle(
        &ctx,
        &mut sess_quit,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(caldari_ag.agent_id),
            PyRep::Integer(i64::from(tmpl_id)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("accept quit mission");
    let _ = ctx.drain_notifications_for_char(char_fail);
    let _ = ctx.drain_notifications_for_char(char_quit);

    handle(
        &ctx,
        &mut sess_quit,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(11),
            PyRep::Integer(caldari_ag.agent_id),
            PyRep::Integer(i64::from(tmpl_id)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("quit mission");
    let quit_notes = ctx.drain_notifications_for_char(char_quit);
    assert!(quit_notes.iter().any(|n| n.event == "OnStandingsModified"));
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_quit];
        assert!(ch.standings[&caldari_ag.agent_id] < 0.0);
        assert!(ch.standings[&caldari_ag.corporation_id] < 0.0);
        assert!(ch.standings[&500_001] < 0.0);
        assert!(ch.standings.get(&500_003).copied().unwrap_or(0.0) < 0.0);
        assert!(ch.standings.get(&500_004).copied().unwrap_or(0.0) <= 0.0);
    });

    clock.advance_secs(604_801);
    let exp_err = handle(
        &ctx,
        &mut sess_fail,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(7),
            PyRep::Integer(caldari_ag.agent_id),
            PyRep::Integer(i64::from(tmpl_id)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(exp_err.code, "MissionExpired");
    let fail_notes = ctx.drain_notifications_for_char(char_fail);
    assert!(fail_notes.iter().any(|n| n.event == "OnStandingsModified"));
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_fail];
        assert!(ch.standings[&500_001] < 0.0);
        assert!(ch.standings.get(&500_003).copied().unwrap_or(0.0) < 0.0);
        assert!(ch.standings.get(&500_004).copied().unwrap_or(0.0) <= 0.0);
        let txs = &snap.standing_transactions[&char_fail];
        assert!(txs.iter().any(|tx| tx.from_id == 500_001 && tx.event_type_id == 84));
        assert!(txs.iter().any(|tx| tx.from_id == 500_003 && tx.event_type_id == 84));
    });
}

#[tokio::test]
async fn pve_012_persisted_objectives_match_briefing_and_reoffer() {
    let ctx = test_ctx();
    let ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("stationed agent")
        .clone();
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    let char_id = 90_000_001_i64;
    let now = ctx.now_unix();
    let kw = HashMap::new();
    move_demo_character(&ctx, char_id, sys_id, Some(st_id));
    let mut sess = docked_session(char_id, sys_id, Some(st_id));

    let trade_mid = 98_299_u32;
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, trade_mid, ag.agent_id, sys_id)
                .origin_station_id(Some(st_id))
                .required_delivery_type_id(Some(34))
                .required_delivery_qty(77)
                .offered_at_unix(now)
                .offer_expiry_unix(now + 86_400),
        );
        let _ = snap.missions.update_by_character_template(char_id, trade_mid, |m| {
            m.kind = eve_db::MissionKind::Trade;
        });
    });

    let reoffer = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(ag.agent_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("reoffer persisted trade mission");
    let obj_text = reoffer
        .dict_get("objectives")
        .and_then(PyRep::as_str)
        .expect("objectives");
    let briefing = reoffer
        .dict_get("briefing")
        .and_then(PyRep::as_str)
        .expect("briefing");
    assert!(obj_text.contains("77") && obj_text.contains("Tritanium"), "got {obj_text}");
    assert!(briefing.contains("77") && briefing.contains("Tritanium"), "got {briefing}");

    let obj_info = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(i64::from(trade_mid))],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetMissionObjectiveInfo");
    let Some(PyRep::List(v24_objs)) = obj_info.dict_get("objectives") else {
        panic!("expected V24 objectives list");
    };
    assert_eq!(v24_objs.len(), 2);
    let PyRep::Tuple(fetch_entry) = &v24_objs[1] else {
        panic!("expected fetch tuple");
    };
    assert_eq!(fetch_entry[0], PyRep::String("fetch".into()));
    let PyRep::Tuple(fetch_args) = &fetch_entry[1] else {
        panic!("expected fetch args");
    };
    assert_eq!(fetch_args[2].dict_get("typeID"), Some(&PyRep::Integer(34)));
    assert_eq!(fetch_args[2].dict_get("quantity"), Some(&PyRep::Integer(77)));

    let journal_info = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionJournalInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(i64::from(trade_mid))],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetMissionJournalInfo");
    let info_text = journal_info
        .dict_get("objectives")
        .and_then(PyRep::as_str)
        .expect("objectives");
    assert!(info_text.contains("77") && info_text.contains("Tritanium"), "got {info_text}");
    assert_eq!(journal_info.dict_get("requiredTypeID"), Some(&PyRep::Integer(34)));
    assert_eq!(journal_info.dict_get("requiredQuantity"), Some(&PyRep::Integer(77)));

    let brief_info = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(i64::from(trade_mid))],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetMissionBriefingInfo");
    assert_eq!(brief_info.dict_get("ContentID"), Some(&PyRep::Integer(i64::from(trade_mid))));
    assert!(brief_info
        .dict_get("Mission Briefing ID")
        .and_then(PyRep::as_str)
        .is_some_and(|s| s.contains("77") && s.contains("Tritanium")));
}

#[tokio::test]
async fn history_lookup_recovers_career_plausible_missions_and_completed_courier_objectives() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let kw = HashMap::new();

    // 1. Career agent (`agent_type_id == 12`) mission completed & archived into history:
    //    `agentMgr.GetPlausibleMissionIDs` without an explicit/bound agent must still include it.
    let career_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.agent_type_id == 12 && a.solar_system_id.is_some() && a.station_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .expect("career agent in SDE")
        .clone();
    let career_sys = career_ag.solar_system_id.expect("career agent sys");
    let career_st = career_ag.station_id.expect("career agent station");
    let career_mid = ctx
        .sde
        .missions
        .values()
        .filter(|m| mission_matches_agent_pool(m, &career_ag))
        .map(|m| m.mission_id)
        .min()
        .expect("career mission template in SDE");

    move_demo_character(&ctx, char_id, career_sys, Some(career_st));
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, career_mid, career_ag.agent_id, career_sys)
                .state("Completed"),
        );
    });
    let mut sess = docked_session(char_id, career_sys, Some(career_st));
    sess.session_id = 1653_001;
    let plausible = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetPlausibleMissionIDs", &[], &kw, None)
        .await
        .expect("GetPlausibleMissionIDs with archived career mission");
    let PyRep::List(plausible_ids) = plausible else {
        panic!("expected plausible mission list, got {plausible:?}");
    };
    assert!(
        !plausible_ids.is_empty(),
        "plausible mission IDs must not be empty when character has archived career mission history"
    );

    // 2. Archived `Completed` courier mission queried via `GetMissionObjectiveInfo`:
    //    must hydrate courier destination/cargo from SDE so `"transport"` objective is emitted.
    let courier_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.agent_type_id == 2
                && a.division_id == 22
                && a.solar_system_id.is_some()
                && a.station_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .expect("distribution courier agent in SDE")
        .clone();
    let courier_sys = courier_ag.solar_system_id.expect("courier agent sys");
    let courier_st = courier_ag.station_id.expect("courier agent station");
    let courier_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            m.content_template
                .as_deref()
                .unwrap_or_default()
                .contains("BasicCourier")
                && resolve_courier_mission_details(&ctx, m, &courier_ag).is_ok()
        })
        .min_by_key(|m| m.mission_id)
        .expect("courier mission template in SDE");
    let details = resolve_courier_mission_details(&ctx, courier_tmpl, &courier_ag)
        .expect("courier details");

    move_demo_character(&ctx, char_id, courier_sys, Some(courier_st));
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert_with_sde(
            snap,
            &ctx.sde,
            MissionFixture::accepted(
                char_id,
                courier_tmpl.mission_id,
                courier_ag.agent_id,
                courier_sys,
            )
            .state("Completed")
            .destination_station_id(Some(details.destination_station_id)),
        );
    });
    let mut obj_sess = docked_session(char_id, courier_sys, Some(courier_st));
    obj_sess.session_id = 1653_002;
    let obj_info = ctx
        .dispatch_rpc(
            &mut obj_sess,
            "agentMgr",
            None,
            "GetMissionObjectiveInfo",
            &[
                PyRep::Integer(courier_ag.agent_id),
                PyRep::Integer(i64::from(courier_tmpl.mission_id)),
            ],
            &kw,
            None,
        )
        .await
        .expect("GetMissionObjectiveInfo for archived completed courier mission");
    let Some(PyRep::List(objs)) = obj_info.dict_get("objectives") else {
        panic!("expected objectives list, got {obj_info:?}");
    };
    assert_eq!(objs.len(), 2, "completed courier history must include transport objective");
    let PyRep::Tuple(transport_entry) = &objs[1] else {
        panic!("expected transport tuple");
    };
    assert_eq!(transport_entry[0], PyRep::String("transport".into()));
    let PyRep::Tuple(transport_data) = &transport_entry[1] else {
        panic!("expected transport data tuple");
    };
    assert_eq!(
        transport_data[4].dict_get("hasCargo"),
        Some(&PyRep::Bool(true))
    );
    assert_eq!(
        transport_data[4].dict_get("typeID"),
        Some(&PyRep::Integer(i64::from(details.cargo_type_id)))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cmd_warp_to_stuff_range_truncation_legacy_none_and_post_eviction_respawn() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();
    let pocket_pos = glam::DVec3::new(5.0e12, 0.0, 0.0);
    let bm_id = 995_101_i64;
    let mid_scoped = 98_201_u32;
    let mid_legacy = 98_202_u32;
    let agent_id = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(sys_id))
        .map(|a| a.agent_id)
        .unwrap_or(3_018_601);

    let handle = ctx.systems.get_or_spawn(sys_id);
    let ship_cs = ctx.build_type_combat_state(587);
    let _ = handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_067_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ship_cs,
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(sys_id);
            ship.flag = eve_db::InventoryFlag::None;
        }
        mission_fixture::clear_character(snap, char_id);
        snap.bookmarks.insert(
            bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "BF1 Pocket".into(),
                x: pocket_pos.x,
                y: pocket_pos.y,
                z: pocket_pos.z,
                item_id: None,
                memo: "Mission Pocket Destination".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_scoped, agent_id, sys_id)
                .bookmark_id(Some(bm_id))
                .site_origin_m(Some([pocket_pos.x as i64, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![34_155])),
        );
        // Legacy mission with no known pocket origin or bookmark must not trigger on unrelated warps.
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_legacy, agent_id, sys_id)
                .bookmark_id(None)
                .site_origin_m(None)
                .npcs_remaining(1)
                .initial_wave_count(Some(1))
                .initial_wave_types(Some(vec![34_155])),
        );
    });

    // 1. Unscoped stargate/station warp must NOT materialize legacy `known_origin == None` mission.
    let st_pos = ctx
        .sde
        .stations
        .get(&st_id)
        .map(|s| s.position)
        .unwrap_or(glam::DVec3::ZERO);
    agents_missions_warp::materialize_mission_pocket_on_cmd_warp(
        &ctx, &handle, char_id, sys_id, "item", st_id, st_pos,
    )
    .await;
    ctx.db.with_snapshot(|snap| {
        assert!(mission_fixture::get(snap, char_id, mid_legacy).unwrap().spawned_npc_ball_ids.is_empty());
        assert!(mission_fixture::get(snap, char_id, mid_scoped).unwrap().spawned_npc_ball_ids.is_empty());
    });

    // 2. Low-cap truncated CmdWarpToStuff("bookmark", bm_id) stopping > 200 km short AND explicit
    //    bubble-dragged bookmark warp (> 200 km from pocket_pos) must NOT spawn NPCs.
    let mut sess = docked_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);
    handle.try_update_combat_state(ship_id, |cs| {
        cs.capacitor = 0.01;
        cs.max_capacitor = 250.0;
    });
    assert!(
        (handle.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 0.01).abs() < 1e-6
    );
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)],
            &kw,
            None,
        )
        .await
        .expect("low-cap CmdWarpToStuff initiates partial warp");
    ctx.db.with_snapshot(|snap| {
        assert!(
            mission_fixture::get(snap, char_id, mid_scoped)
                .unwrap()
                .spawned_npc_ball_ids
                .is_empty(),
            "truncated low-cap warp must not materialize pocket NPCs in deep space"
        );
    });
    let _ = handle.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
    let _ = handle.set_ball_position(ship_id, glam::DVec3::ZERO).await;
    handle.try_update_combat_state(ship_id, |cs| {
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
    });
    let stop_bubble_id = 995_199_i64;
    handle
        .add_warp_bubble(
            stop_bubble_id,
            pocket_pos - glam::DVec3::new(500_000.0, 0.0, 0.0),
            20_000.0,
        )
        .await
        .unwrap();
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)],
            &kw,
            None,
        )
        .await
        .expect("bubble-dragged CmdWarpToStuff initiates partial warp");
    match handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(
                destination.distance(pocket_pos) > 200_000.0,
                "bubble must intercept CmdWarpToStuff >200 km short of pocket_pos, got {destination:?}"
            );
        }
        other => panic!("expected Warp mode for bubble-dragged ship, got {other:?}"),
    }
    ctx.db.with_snapshot(|snap| {
        assert!(
            mission_fixture::get(snap, char_id, mid_scoped)
                .unwrap()
                .spawned_npc_ball_ids
                .is_empty(),
            "bubble-dragged CmdWarpToStuff bookmark warp (>200km from pocket) must not materialize NPCs"
        );
    });
    assert!(handle.remove_warp_bubble(stop_bubble_id).await.unwrap());

    // 3. Full-cap 100 km range warp to bookmark must materialize NPCs around canonical `pocket_pos` (NOT 100 km dest_pos).
    let _ = handle.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
    let _ = handle.set_ball_position(ship_id, glam::DVec3::ZERO).await;
    handle.try_update_combat_state(ship_id, |cs| {
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
    });
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[
                PyRep::String("bookmark".into()),
                PyRep::Integer(bm_id),
                PyRep::Real(100_000.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("100km CmdWarpToStuff succeeds");
    let first_bids = ctx.db.with_snapshot(|snap| {
        assert!(
            mission_fixture::get(snap, char_id, mid_legacy)
                .unwrap()
                .spawned_npc_ball_ids
                .is_empty()
        );
        mission_fixture::get(snap, char_id, mid_scoped)
            .unwrap()
            .spawned_npc_ball_ids
    });
    assert_eq!(first_bids.len(), 2);
    for &bid in &first_bids {
        let (_, b) = handle.get_ball_snapshot(bid).await.unwrap().expect("npc ball spawned");
        assert!(
            (b.position.distance(pocket_pos) - 4_500.0).abs() < 1.0,
            "100km range warp must anchor NPC wave around canonical pocket_pos, got dist={}",
            b.position.distance(pocket_pos)
        );
    }

    // 4. Simulate ballpark eviction removing NPC balls; subsequent CmdWarpToStuff must reconcile & respawn.
    for &bid in &first_bids {
        let _ = handle.remove_ball(bid).await;
    }
    let _ = handle.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
    let _ = handle.set_ball_position(ship_id, glam::DVec3::ZERO).await;
    handle.try_update_combat_state(ship_id, |cs| {
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
    });
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)],
            &kw,
            None,
        )
        .await
        .expect("post-eviction CmdWarpToStuff succeeds");
    let respawned_bids = ctx.db.with_snapshot(|snap| {
        mission_fixture::get(snap, char_id, mid_scoped)
            .unwrap()
            .spawned_npc_ball_ids
    });
    assert_eq!(respawned_bids.len(), 2);
    assert_eq!(respawned_bids, first_bids);
    for &bid in &respawned_bids {
        assert!(handle.get_ball_snapshot(bid).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn pocket_spawned_mission_npcs_preserve_sde_ewar_reps_and_uses_synthetic_false() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mid = 98_203_u32;
    let pocket_pos = glam::DVec3::new(10_000_000.0, 0.0, 0.0);

    // Real SDE NPCs: 34155 ("Burner Jaguar": web + scram + turrets) & 34154 ("Burner Burst": remote shield rep)
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid, 3_018_601, sys_id)
                .site_origin_m(Some([pocket_pos.x as i64, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![34_155, 34_154])),
        );
    });

    let spawned = crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
        &ctx,
        char_id,
        sys_id,
        pocket_pos,
        Some(mid),
    );
    assert_eq!(spawned, 2);

    let bids = ctx.db.with_snapshot(|snap| {
        mission_fixture::get(snap, char_id, mid)
            .unwrap()
            .spawned_npc_ball_ids
    });
    assert_eq!(bids.len(), 2);

    let handle = ctx.systems.get_or_spawn(sys_id);
    let expected_corp =
        crate::services::combat_crimewatch::combat_npc_ai::resolve_npc_corp_id_for_faction(
            &ctx, 500_002,
        );
    let slim0 = handle.get_ball_slim_item(bids[0]).await.unwrap().expect("slim0");
    assert_eq!(slim0.dict_get("name"), Some(&PyRep::String("Burner Jaguar".into())));
    assert_eq!(slim0.dict_get("ownerID"), Some(&PyRep::Integer(500_002)));
    assert_eq!(slim0.dict_get("corpID"), Some(&PyRep::Integer(expected_corp)));

    let slim1 = handle.get_ball_slim_item(bids[1]).await.unwrap().expect("slim1");
    assert_eq!(slim1.dict_get("name"), Some(&PyRep::String("Burner Burst".into())));
    assert_eq!(slim1.dict_get("ownerID"), Some(&PyRep::Integer(500_002)));
    assert_eq!(slim1.dict_get("corpID"), Some(&PyRep::Integer(expected_corp)));

    let e0 = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let e0 = rt.npc_entities[&bids[0]].clone();
        assert_eq!(e0.type_id, 34_155);
        assert!(e0.web_speed_factor < 0.0, "expected SDE web factor, got {}", e0.web_speed_factor);
        assert!(e0.scram_strength >= 2.0, "expected SDE scram strength, got {}", e0.scram_strength);
        assert!(e0.scram_range >= 10_000.0, "expected SDE scram range, got {}", e0.scram_range);

        let e1 = &rt.npc_entities[&bids[1]];
        assert_eq!(e1.type_id, 34_154);
        assert!(e1.remote_shield_rep >= 100.0, "expected SDE remote shield rep, got {}", e1.remote_shield_rep);
        assert!(e1.remote_rep_range >= 20_000.0, "expected SDE remote rep range, got {}", e1.remote_rep_range);
        assert!(e1.bounty_isk >= 10_000.0);
        e0
    });
    let wrt0 = crate::combat::npc_ai_weapons_loot::take_or_init_extended_npc_weapon_runtime(
        &ctx, sys_id, &e0,
    );
    assert!(
        !wrt0.uses_synthetic_legacy,
        "pocket-spawned SDE mission NPC must initialize uses_synthetic_legacy == false"
    );
}

#[tokio::test]
async fn clamped_penalty_and_journal_expiry_penalties() {
    let ctx = test_ctx();
    let clock = Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_400_000));
    ctx.set_clock(clock);

    let caldari_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.agent_type_id == 2
                && a.station_id.is_some()
                && a.solar_system_id.is_some()
                && ctx
                    .sde
                    .npc_corporations
                    .get(&a.corporation_id)
                    .and_then(|c| c.faction_id)
                    == Some(500_001)
        })
        .min_by_key(|a| a.agent_id)
        .expect("Caldari L1 stationed agent")
        .clone();
    let sys_id = caldari_ag.solar_system_id.unwrap();
    let st_id = caldari_ag.station_id.unwrap();
    let char_id = 90_000_001_i64;
    let kw = HashMap::new();

    // 1. Clamped -10.0 agent/corp/faction standings must never emit zero-delta changes or tx rows.
    let agent_corp_clamped = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
            ch.standings.insert(caldari_ag.agent_id, -10.0);
            ch.standings.insert(caldari_ag.corporation_id, -10.0);
        }
        snap.standing_transactions.remove(&char_id);
        apply_mission_penalty_standings(
            &ctx,
            snap,
            char_id,
            caldari_ag.agent_id,
            caldari_ag.corporation_id,
            None,
            -0.10,
            -0.10,
            0.0,
            84,
        )
    });
    assert!(
        agent_corp_clamped.is_empty(),
        "clamped -10.0 agent/corp penalty must not push zero-delta notifications"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.standing_transactions
                .get(&char_id)
                .is_none_or(|txs| txs.is_empty()),
            "clamped -10.0 agent/corp penalty must not insert zero-delta standing transactions"
        );
    });

    let changes = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
            ch.standings.insert(caldari_ag.agent_id, -10.0);
            ch.standings.insert(caldari_ag.corporation_id, -10.0);
            ch.standings.insert(500_001, -10.0);
        }
        snap.standing_transactions.remove(&char_id);
        apply_mission_penalty_standings(
            &ctx,
            snap,
            char_id,
            caldari_ag.agent_id,
            caldari_ag.corporation_id,
            Some(500_001),
            -0.10,
            -0.10,
            -0.5,
            84,
        )
    });
    assert!(changes.iter().all(|(_, _, delta)| delta.abs() > 1e-9));
    assert!(
        !changes.iter().any(|&(eid, _, _)| {
            eid == caldari_ag.agent_id || eid == caldari_ag.corporation_id || eid == 500_001
        }),
        "clamped agent/corp/primary faction must not emit notifications: {changes:?}"
    );
    ctx.db.with_snapshot(|snap| {
        if let Some(txs) = snap.standing_transactions.get(&char_id) {
            assert!(txs.iter().all(|tx| tx.modification.abs() > 1e-9));
            assert!(!txs.iter().any(|tx| {
                tx.from_id == caldari_ag.agent_id
                    || tx.from_id == caldari_ag.corporation_id
                    || tx.from_id == 500_001
            }));
        }
    });

    // 2. GetMyJournalDetails expiry sweep of Accepted mission applies unified penalties + post-commit notify.
    let exp_mid = 98_204_u32;
    let now = ctx.now_unix();
    move_demo_character(&ctx, char_id, sys_id, Some(st_id));
    ctx.db
        .acquire_online_lock(1, 503, Some(char_id), true, now)
        .expect("online lock");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
            ch.standings.insert(caldari_ag.agent_id, 1.0);
            ch.standings.insert(caldari_ag.corporation_id, 1.0);
            ch.standings.insert(500_001, 1.0);
        }
        snap.standing_transactions.remove(&char_id);
        mission_fixture::clear_character(snap, char_id);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, exp_mid, caldari_ag.agent_id, sys_id)
                .origin_station_id(Some(st_id))
                .offered_at_unix(now.saturating_sub(200))
                .offer_expiry_unix(now + 100)
                .accepted_at_unix(Some(now.saturating_sub(100)))
                .accept_expiry_unix(Some(now.saturating_sub(5))),
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);

    let mut sess = docked_session(char_id, sys_id, Some(st_id));
    sess.session_id = 503;
    let _ = handle(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[], &kw)
        .await
        .unwrap()
        .expect("GetMyJournalDetails succeeds");
    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(notes.iter().any(|n| n.event == "OnStandingsModified"));
    assert!(notes.iter().any(|n| n.event == "OnAgentMissionObjectivesFailed"));
    assert!(notes.iter().any(|n| n.event == "OnAgentMissionChange"));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(mission_fixture::latest_history(snap, char_id, exp_mid).as_deref(), Some("Failed"));
        let ch = &snap.characters[&char_id];
        assert!(ch.standings[&caldari_ag.agent_id] < 1.0);
        assert!(ch.standings[&caldari_ag.corporation_id] < 1.0);
        assert!(ch.standings[&500_001] < 1.0);
        let txs = &snap.standing_transactions[&char_id];
        assert!(txs.iter().any(|tx| tx.from_id == caldari_ag.agent_id && tx.event_type_id == 84));
        assert!(txs.iter().any(|tx| tx.from_id == caldari_ag.corporation_id && tx.event_type_id == 84));
        assert!(txs.iter().any(|tx| tx.from_id == 500_001 && tx.event_type_id == 84));
    });
}

#[tokio::test]
async fn solo_agent_warp_dungeon_and_bookmark_respect_200km_landing_radius() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();
    let pocket_enc_pos = glam::DVec3::new(5.0e12, 0.0, 0.0);
    let pocket_mine_pos = glam::DVec3::new(5.5e12, 0.0, 0.0);
    let goto_pocket_pos = glam::DVec3::new(50_000.0, 0.0, 0.0);
    let bm_enc_id = 995_401_i64;
    let bm_mine_id = 995_402_i64;
    let ast_id = 995_403_i64;
    let bubble_enc_id = 995_404_i64;
    let bubble_mine_id = 995_405_i64;
    let mid_enc = 98_401_u32;
    let mid_mine = 98_402_u32;
    let bm_goto_id = 995_406_i64;
    let ast_goto_id = 995_407_i64;
    let mid_goto = 98_403_u32;
    let mut jita_agents: Vec<i64> = ctx.sde.agents.keys().copied().collect();
    jita_agents.sort_unstable();
    let agent_enc_id = jita_agents[0];
    let agent_mine_id = jita_agents[1];
    let agent_goto_id = jita_agents[2];

    let handle_sys = ctx.systems.get_or_spawn(sys_id);
    let ship_cs = ctx.build_type_combat_state(587);
    let _ = handle_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_067_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ship_cs,
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(sys_id);
            ship.flag = eve_db::InventoryFlag::None;
        }
        mission_fixture::clear_character(snap, char_id);
        snap.bookmarks.insert(
            bm_enc_id,
            eve_db::BookmarkRecord {
                bookmark_id: bm_enc_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "OWN1 Enc Pocket".into(),
                x: pocket_enc_pos.x,
                y: pocket_enc_pos.y,
                z: pocket_enc_pos.z,
                item_id: None,
                memo: "Mission Pocket Destination".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_enc, agent_enc_id, sys_id)
                .bookmark_id(Some(bm_enc_id))
                .site_origin_m(Some([pocket_enc_pos.x as i64, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![34_155])),
        );
        for (bid, aid, m_id, ag_id, origin) in [
            (bm_mine_id, ast_id, mid_mine, agent_mine_id, pocket_mine_pos),
            (bm_goto_id, ast_goto_id, mid_goto, agent_goto_id, goto_pocket_pos),
        ] {
            snap.bookmarks.insert(
                bid,
                eve_db::BookmarkRecord {
                    bookmark_id: bid,
                    owner_id: char_id,
                    solar_system_id: sys_id,
                    label: format!("OWN1 Mine Pocket {m_id}"),
                    x: origin.x,
                    y: origin.y,
                    z: origin.z,
                    item_id: None,
                    memo: "Mission Pocket Destination".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
            snap.asteroids.insert(
                aid,
                eve_db::AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id: sys_id,
                    belt_id: 0,
                    type_id: 1230,
                    quantity: 5_000,
                    initial_quantity: 5_000,
                    position: (origin.x + 8_000.0, origin.y, origin.z),
                    radius: 50.0,
                },
            );
            mission_fixture::insert(
                snap,
                MissionFixture::accepted(char_id, m_id, ag_id, sys_id)
                    .bookmark_id(Some(bid))
                    .site_origin_m(Some([origin.x as i64, 0, 0]))
                    .asteroid_entity_id(Some(aid))
                    .required_delivery_type_id(Some(1230))
                    .required_delivery_qty(100),
            );
            snap.missions
                .update_by_character_template(char_id, m_id, |m| {
                    m.kind = eve_db::MissionKind::Mining;
                })
                .unwrap();
        }
    });

    let mut sess = docked_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);

    // 1. Low-cap truncated solo agentMgr.WarpToLocation("dungeon", ...) must NOT spawn NPCs or asteroids.
    for (loc_num, ag_id, ast_check) in [
        (i64::from(mid_enc), agent_enc_id, None),
        (i64::from(mid_mine), agent_mine_id, Some(ast_id)),
    ] {
        let _ = handle_sys.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
        let _ = handle_sys.set_ball_position(ship_id, glam::DVec3::ZERO).await;
        handle_sys.try_update_combat_state(ship_id, |cs| {
            cs.capacitor = 10.0;
            cs.max_capacitor = 250.0;
        });
        let res = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[
                PyRep::String("dungeon".into()),
                PyRep::Integer(loc_num),
                PyRep::Real(0.0),
                PyRep::Bool(false),
                PyRep::Integer(ag_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("low-cap dungeon warp initiates");
        assert_eq!(res, PyRep::Bool(true));
        if ast_check.is_none() {
            ctx.db.with_snapshot(|snap| {
                assert!(mission_fixture::get(snap, char_id, mid_enc).unwrap().spawned_npc_ball_ids.is_empty());
            });
        } else {
            assert!(handle_sys.get_ball_snapshot(ast_id).await.unwrap().is_none());
        }
    }

    // 2. Destination warp bubble > 200 km short must prevent BOTH "dungeon" and "bookmark" spawns.
    handle_sys
        .add_warp_bubble(bubble_enc_id, pocket_enc_pos - glam::DVec3::new(500_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    handle_sys
        .add_warp_bubble(bubble_mine_id, pocket_mine_pos - glam::DVec3::new(500_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    for (loc_type, loc_num, ag_id, is_enc) in [
        ("dungeon", i64::from(mid_enc), agent_enc_id, true),
        ("bookmark", bm_enc_id, agent_enc_id, true),
        ("dungeon", i64::from(mid_mine), agent_mine_id, false),
        ("bookmark", bm_mine_id, agent_mine_id, false),
    ] {
        let _ = handle_sys.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
        let _ = handle_sys.set_ball_position(ship_id, glam::DVec3::ZERO).await;
        handle_sys.try_update_combat_state(ship_id, |cs| {
            cs.capacitor = 250.0;
            cs.max_capacitor = 250.0;
        });
        let r = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[
                PyRep::String(loc_type.into()),
                PyRep::Integer(loc_num),
                PyRep::Real(0.0),
                PyRep::Bool(false),
                PyRep::Integer(ag_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("bubbled solo warp initiates");
        assert_eq!(r, PyRep::Bool(true));
        if is_enc {
            ctx.db.with_snapshot(|snap| {
                assert!(
                    mission_fixture::get(snap, char_id, mid_enc).unwrap().spawned_npc_ball_ids.is_empty(),
                    "bubble-dragged {loc_type} warp must not spawn NPCs"
                );
            });
        } else {
            assert!(
                handle_sys.get_ball_snapshot(ast_id).await.unwrap().is_none(),
                "bubble-dragged {loc_type} warp must not spawn asteroid"
            );
        }
    }

    // 3. Remove bubbles -> solo agentMgr.WarpToLocation("bookmark", ...) materializes NPCs + asteroid.
    assert!(handle_sys.remove_warp_bubble(bubble_enc_id).await.unwrap());
    assert!(handle_sys.remove_warp_bubble(bubble_mine_id).await.unwrap());
    for (bid, ag_id, is_enc) in [(bm_enc_id, agent_enc_id, true), (bm_mine_id, agent_mine_id, false)] {
        let _ = handle_sys.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
        let _ = handle_sys.set_ball_position(ship_id, glam::DVec3::ZERO).await;
        handle_sys.try_update_combat_state(ship_id, |cs| {
            cs.capacitor = 250.0;
            cs.max_capacitor = 250.0;
        });
        let bm_ok = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[
                PyRep::String("bookmark".into()),
                PyRep::Integer(bid),
                PyRep::Real(0.0),
                PyRep::Bool(false),
                PyRep::Integer(ag_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("bookmark warp reaches pocket");
        assert_eq!(bm_ok, PyRep::Bool(true));
        if is_enc {
            ctx.db.with_snapshot(|snap| {
                assert_eq!(mission_fixture::get(snap, char_id, mid_enc).unwrap().spawned_npc_ball_ids.len(), 2);
            });
        } else {
            assert!(handle_sys.get_ball_snapshot(ast_id).await.unwrap().is_some());
        }
    }

    // 4. Subwarp agentMgr.GotoLocation("bookmark", bm_goto_id) within 200 km materializes pocket asteroid.
    let _ = handle_sys.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
    let _ = handle_sys.set_ball_position(ship_id, glam::DVec3::ZERO).await;
    let goto_ok = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GotoLocation",
        &[
            PyRep::String("bookmark".into()),
            PyRep::Integer(bm_goto_id),
            PyRep::Integer(agent_goto_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("subwarp GotoLocation succeeds");
    assert_eq!(goto_ok, PyRep::Bool(true));
    assert!(handle_sys.get_ball_snapshot(ast_goto_id).await.unwrap().is_some());
}

#[tokio::test]
async fn fleet_agent_warp_dungeon_and_bookmark_respect_200km_landing_radius() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let mut jita_agents: Vec<i64> = ctx.sde.agents.keys().copied().collect();
    jita_agents.sort_unstable();
    let ag_enc = jita_agents[0];
    let ag_mine = jita_agents[1];
    let ag_enc2 = jita_agents[2];
    let ag_mine2 = jita_agents[3];
    let char_fc = 959_601_i64;
    let char_wing = 959_602_i64;
    let ship_fc = 959_611_i64;
    let ship_wing = 959_612_i64;
    let fleet_id = 959_620_i64;
    let bm_enc_id = 959_630_i64;
    let mid_enc = 98_404_u32;
    let bm_mine_id = 959_631_i64;
    let ast_id = 959_632_i64;
    let mid_mine = 98_405_u32;
    let bm_enc2_id = 959_633_i64;
    let mid_enc2 = 98_406_u32;
    let bm_mine2_id = 959_634_i64;
    let ast2_id = 959_635_i64;
    let mid_mine2 = 98_407_u32;
    let bubble_enc_id = 959_640_i64;
    let bubble_mine_id = 959_641_i64;
    let pocket_enc_pos = glam::DVec3::new(5.0e12, 0.0, 0.0);
    let pocket_mine_pos = glam::DVec3::new(5.5e12, 0.0, 0.0);
    let pocket_enc2_pos = glam::DVec3::new(6.0e12, 0.0, 0.0);
    let pocket_mine2_pos = glam::DVec3::new(6.5e12, 0.0, 0.0);
    let start_pos = glam::DVec3::ZERO;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let template_acc = snap.accounts.values().next().unwrap().clone();
        let template_ch = snap.characters.values().next().unwrap().clone();
        for (idx, (cid, sid)) in [(char_fc, ship_fc), (char_wing, ship_wing)].into_iter().enumerate() {
            let uid = 959_650 + idx as u64;
            let mut acc = template_acc.clone();
            acc.user_id = uid;
            acc.username = format!("fleet_warp_{idx}");
            snap.accounts.insert(acc.username.clone(), acc);
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let mut ch = template_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);
        }
        for (bid, m_id, ag_id, origin) in [
            (bm_enc_id, mid_enc, ag_enc, pocket_enc_pos),
            (bm_enc2_id, mid_enc2, ag_enc2, pocket_enc2_pos),
        ] {
            snap.bookmarks.insert(
                bid,
                eve_db::BookmarkRecord {
                    bookmark_id: bid,
                    owner_id: char_fc,
                    solar_system_id: sys_id,
                    label: format!("OWN1 Fleet Enc {m_id}"),
                    x: origin.x,
                    y: origin.y,
                    z: origin.z,
                    item_id: None,
                    memo: "Mission Pocket".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
            mission_fixture::insert(
                snap,
                MissionFixture::accepted(char_fc, m_id, ag_id, sys_id)
                    .bookmark_id(Some(bid))
                    .site_origin_m(Some([origin.x as i64, 0, 0]))
                    .npcs_remaining(2)
                    .initial_wave_count(Some(2))
                    .initial_wave_types(Some(vec![34_155])),
            );
        }
        for (bid, aid, m_id, ag_id, origin) in [
            (bm_mine_id, ast_id, mid_mine, ag_mine, pocket_mine_pos),
            (bm_mine2_id, ast2_id, mid_mine2, ag_mine2, pocket_mine2_pos),
        ] {
            snap.bookmarks.insert(
                bid,
                eve_db::BookmarkRecord {
                    bookmark_id: bid,
                    owner_id: char_fc,
                    solar_system_id: sys_id,
                    label: format!("OWN1 Fleet Mine {m_id}"),
                    x: origin.x,
                    y: origin.y,
                    z: origin.z,
                    item_id: None,
                    memo: "Mission Pocket".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
            snap.asteroids.insert(
                aid,
                eve_db::AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id: sys_id,
                    belt_id: 0,
                    type_id: 1230,
                    quantity: 5_000,
                    initial_quantity: 5_000,
                    position: (origin.x + 8_000.0, origin.y, origin.z),
                    radius: 50.0,
                },
            );
            mission_fixture::insert(
                snap,
                MissionFixture::accepted(char_fc, m_id, ag_id, sys_id)
                    .bookmark_id(Some(bid))
                    .site_origin_m(Some([origin.x as i64, 0, 0]))
                    .asteroid_entity_id(Some(aid))
                    .required_delivery_type_id(Some(1230))
                    .required_delivery_qty(100),
            );
            snap.missions
                .update_by_character_template(char_fc, m_id, |m| {
                    m.kind = eve_db::MissionKind::Mining;
                })
                .unwrap();
        }
        let mut member_details = HashMap::new();
        member_details.insert(
            char_fc,
            eve_db::FleetMemberRecord {
                character_id: char_fc,
                role: eve_db::FleetRole::FleetCommander,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        member_details.insert(
            char_wing,
            eve_db::FleetMemberRecord {
                character_id: char_wing,
                role: eve_db::FleetRole::SquadMember,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: char_fc,
                members: vec![char_fc, char_wing],
                member_details,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (sid, offset) in [
        (ship_fc, glam::DVec3::ZERO),
        (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
        sys_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: sid,
                    type_id: 587,
                    position: start_pos + offset,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_067_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                cs,
            )
            .await
            .unwrap();
    }
    let now = ctx.now_unix();
    for (idx, cid) in [char_fc, char_wing].into_iter().enumerate() {
        ctx.db
            .acquire_online_lock(959_650 + idx as u64, 959_670 + idx as u64, Some(cid), true, now)
            .expect("online lock");
    }

    let mut sess = docked_session(char_fc, sys_id, None);
    sess.user_id = Some(959_650);
    sess.session_id = 959_670;
    sess.ship_id = Some(ship_fc);
    sess.fleet_id = Some(fleet_id);

    // 1. Bubble > 200 km short intercepts all fleetmates -> neither "dungeon" nor "bookmark" spawns.
    sys_handle
        .add_warp_bubble(bubble_enc_id, pocket_enc_pos - glam::DVec3::new(500_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    sys_handle
        .add_warp_bubble(bubble_mine_id, pocket_mine_pos - glam::DVec3::new(500_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    for (loc_type, loc_num, ag_id, is_enc) in [
        ("dungeon", i64::from(mid_enc), ag_enc, true),
        ("bookmark", bm_enc_id, ag_enc, true),
        ("dungeon", i64::from(mid_mine), ag_mine, false),
        ("bookmark", bm_mine_id, ag_mine, false),
    ] {
        for (sid, offset) in [
            (ship_fc, glam::DVec3::ZERO),
            (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
        ] {
            let _ = sys_handle.set_movement_mode(sid, eve_destiny::MovementMode::Stop).await;
            let _ = sys_handle.set_ball_position(sid, start_pos + offset).await;
            sys_handle.try_update_combat_state(sid, |cs| {
                cs.capacitor = 250.0;
                cs.max_capacitor = 250.0;
            });
        }
        let r = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[
                PyRep::String(loc_type.into()),
                PyRep::Integer(loc_num),
                PyRep::Real(0.0),
                PyRep::Bool(true),
                PyRep::Integer(ag_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("bubbled fleet warp succeeds");
        assert_eq!(r, PyRep::Bool(true));
        if is_enc {
            ctx.db.with_snapshot(|snap| {
                assert!(
                    mission_fixture::get(snap, char_fc, mid_enc).unwrap().spawned_npc_ball_ids.is_empty(),
                    "bubbled fleet {loc_type} warp must not spawn NPCs"
                );
            });
        } else {
            assert!(sys_handle.get_ball_snapshot(ast_id).await.unwrap().is_none());
        }
    }

    // 2. Remove bubbles; cap-truncate all fleetmates -> fleet "dungeon" still skips spawning.
    assert!(sys_handle.remove_warp_bubble(bubble_enc_id).await.unwrap());
    assert!(sys_handle.remove_warp_bubble(bubble_mine_id).await.unwrap());
    for (loc_num, ag_id, is_enc) in [
        (i64::from(mid_enc), ag_enc, true),
        (i64::from(mid_mine), ag_mine, false),
    ] {
        for (sid, offset) in [
            (ship_fc, glam::DVec3::ZERO),
            (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
        ] {
            let _ = sys_handle.set_movement_mode(sid, eve_destiny::MovementMode::Stop).await;
            let _ = sys_handle.set_ball_position(sid, start_pos + offset).await;
            sys_handle.try_update_combat_state(sid, |cs| {
                cs.capacitor = 10.0;
                cs.max_capacitor = 250.0;
            });
        }
        let truncated_ok = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[
                PyRep::String("dungeon".into()),
                PyRep::Integer(loc_num),
                PyRep::Real(0.0),
                PyRep::Bool(true),
                PyRep::Integer(ag_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("low-cap fleet warp initiates");
        assert_eq!(truncated_ok, PyRep::Bool(true));
        if is_enc {
            ctx.db.with_snapshot(|snap| {
                assert!(mission_fixture::get(snap, char_fc, mid_enc).unwrap().spawned_npc_ball_ids.is_empty());
            });
        } else {
            assert!(sys_handle.get_ball_snapshot(ast_id).await.unwrap().is_none());
        }
    }

    // 3. Wingmate has full cap and lands on grid while FC stops short -> BOTH "dungeon" and "bookmark" materialize.
    for (loc_type, loc_num, ag_id, enc_mid_opt, ast_opt) in [
        ("dungeon", i64::from(mid_enc), ag_enc, Some(mid_enc), None),
        ("dungeon", i64::from(mid_mine), ag_mine, None, Some(ast_id)),
        ("bookmark", bm_enc2_id, ag_enc2, Some(mid_enc2), None),
        ("bookmark", bm_mine2_id, ag_mine2, None, Some(ast2_id)),
    ] {
        for (sid, offset, cap) in [
            (ship_fc, glam::DVec3::ZERO, 10.0),
            (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0), 250.0),
        ] {
            let _ = sys_handle.set_movement_mode(sid, eve_destiny::MovementMode::Stop).await;
            let _ = sys_handle.set_ball_position(sid, start_pos + offset).await;
            sys_handle.try_update_combat_state(sid, move |cs| {
                cs.capacitor = cap;
                cs.max_capacitor = 250.0;
            });
        }
        let ok = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "WarpToLocation",
            &[
                PyRep::String(loc_type.into()),
                PyRep::Integer(loc_num),
                PyRep::Real(0.0),
                PyRep::Bool(true),
                PyRep::Integer(ag_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("partial fleet arrival succeeds");
        assert_eq!(ok, PyRep::Bool(true));
        if let Some(m_check) = enc_mid_opt {
            ctx.db.with_snapshot(|snap| {
                assert_eq!(
                    mission_fixture::get(snap, char_fc, m_check).unwrap().spawned_npc_ball_ids.len(),
                    2,
                    "fleet {loc_type} warp with wingmate landing must spawn NPCs"
                );
            });
        }
        if let Some(aid_check) = ast_opt {
            assert!(
                sys_handle.get_ball_snapshot(aid_check).await.unwrap().is_some(),
                "fleet {loc_type} warp with wingmate landing must spawn asteroid"
            );
        }
    }
}

#[tokio::test]
async fn cmd_warp_to_stuff_mission_dungeon_and_fleet_bookmark_materialize() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let mut all_agents: Vec<i64> = ctx.sde.agents.keys().copied().collect();
    all_agents.sort_unstable();
    assert!(all_agents.len() >= 5);
    let ag_solo_enc = all_agents[0];
    let ag_solo_mine = all_agents[1];
    let ag_fleet_enc = all_agents[2];
    let ag_fleet_mine = all_agents[3];
    let ag_fleet_direct = all_agents[4];

    // Part 1: solo beyonce.CmdWarpToStuff("mission"|"dungeon", mid) resolves SplitMix64 site origin,
    // stays bubble-safe (>200 km short -> no spawn), and materializes NPCs + asteroid on arrival.
    let char_solo = 90_000_001_i64;
    let ship_solo = 100_000_001_i64;
    let mid_s_enc = 98_420_u32;
    let mid_s_mine = 98_421_u32;
    let ast_s_id = 995_701_i64;
    let solo_handle = ctx.systems.get_or_spawn(sys_id);
    let mut solo_cs = ctx.build_type_combat_state(587);
    solo_cs.capacitor = 250.0;
    solo_cs.max_capacitor = 250.0;
    solo_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_solo,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_067_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            solo_cs,
        )
        .await
        .unwrap();

    let (solo_enc_origin, solo_mine_origin) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_solo) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_solo;
        }
        if let Some(ship) = snap.items.get_mut(&ship_solo) {
            ship.location_id = i64::from(sys_id);
            ship.flag = eve_db::InventoryFlag::None;
        }
        mission_fixture::clear_character(snap, char_solo);
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_solo, mid_s_enc, ag_solo_enc, sys_id)
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![34_155])),
        );
        let enc_inst = snap
            .missions
            .get_by_character_template(char_solo, mid_s_enc)
            .unwrap()
            .instance_id;
        let enc_origin = agents_missions_warp::mission_site_origin_for_instance(&ctx, sys_id, enc_inst);
        snap.missions
            .update_by_character_template(char_solo, mid_s_enc, |m| {
                m.site_origin_m = Some([
                    enc_origin.x.round() as i64,
                    enc_origin.y.round() as i64,
                    enc_origin.z.round() as i64,
                ]);
            })
            .unwrap();

        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_solo, mid_s_mine, ag_solo_mine, sys_id)
                .asteroid_entity_id(Some(ast_s_id))
                .required_delivery_type_id(Some(1230))
                .required_delivery_qty(100),
        );
        let mine_inst = snap
            .missions
            .get_by_character_template(char_solo, mid_s_mine)
            .unwrap()
            .instance_id;
        let mine_origin = agents_missions_warp::mission_site_origin_for_instance(&ctx, sys_id, mine_inst);
        snap.asteroids.insert(
            ast_s_id,
            eve_db::AsteroidRecord {
                asteroid_id: ast_s_id,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (mine_origin.x + 8_000.0, mine_origin.y, mine_origin.z),
                radius: 50.0,
            },
        );
        snap.missions
            .update_by_character_template(char_solo, mid_s_mine, |m| {
                m.kind = eve_db::MissionKind::Mining;
                m.site_origin_m = Some([
                    mine_origin.x.round() as i64,
                    mine_origin.y.round() as i64,
                    mine_origin.z.round() as i64,
                ]);
            })
            .unwrap();
        (enc_origin, mine_origin)
    });

    let mut solo_sess = docked_session(char_solo, sys_id, None);
    solo_sess.ship_id = Some(ship_solo);
    let kw = HashMap::new();

    let enc_dir = solo_enc_origin.normalize();
    let mine_dir = solo_mine_origin.normalize();
    let b_enc_id = 995_710_i64;
    let b_mine_id = 995_711_i64;
    solo_handle
        .add_warp_bubble(b_enc_id, solo_enc_origin - enc_dir * 500_000.0, 20_000.0)
        .await
        .unwrap();
    solo_handle
        .add_warp_bubble(b_mine_id, solo_mine_origin - mine_dir * 500_000.0, 20_000.0)
        .await
        .unwrap();

    for (stuff_type, mid_val, is_enc) in [
        ("mission", i64::from(mid_s_enc), true),
        ("dungeon", i64::from(mid_s_mine), false),
    ] {
        let _ = solo_handle.set_movement_mode(ship_solo, eve_destiny::MovementMode::Stop).await;
        let _ = solo_handle.set_ball_position(ship_solo, glam::DVec3::ZERO).await;
        solo_handle.try_update_combat_state(ship_solo, |cs| {
            cs.capacitor = 250.0;
            cs.max_capacitor = 250.0;
        });
        let _ = ctx
            .dispatch_rpc(
                &mut solo_sess,
                "beyonce",
                None,
                "CmdWarpToStuff",
                &[PyRep::String(stuff_type.into()), PyRep::Integer(mid_val)],
                &kw,
                None,
            )
            .await
            .expect("bubbled CmdWarpToStuff initiates");
        if is_enc {
            ctx.db.with_snapshot(|snap| {
                assert!(mission_fixture::get(snap, char_solo, mid_s_enc).unwrap().spawned_npc_ball_ids.is_empty());
            });
        } else {
            assert!(solo_handle.get_ball_snapshot(ast_s_id).await.unwrap().is_none());
        }
    }

    assert!(solo_handle.remove_warp_bubble(b_enc_id).await.unwrap());
    assert!(solo_handle.remove_warp_bubble(b_mine_id).await.unwrap());
    for (stuff_type, mid_val, expected_origin, is_enc) in [
        ("mission", i64::from(mid_s_enc), solo_enc_origin, true),
        ("dungeon", i64::from(mid_s_mine), solo_mine_origin, false),
    ] {
        let _ = solo_handle.set_movement_mode(ship_solo, eve_destiny::MovementMode::Stop).await;
        let _ = solo_handle.set_ball_position(ship_solo, glam::DVec3::ZERO).await;
        solo_handle.try_update_combat_state(ship_solo, |cs| {
            cs.capacitor = 250.0;
            cs.max_capacitor = 250.0;
        });
        let _ = ctx
            .dispatch_rpc(
                &mut solo_sess,
                "beyonce",
                None,
                "CmdWarpToStuff",
                &[PyRep::String(stuff_type.into()), PyRep::Integer(mid_val)],
                &kw,
                None,
            )
            .await
            .expect("unblocked CmdWarpToStuff(mission|dungeon) succeeds");
        match solo_handle.get_ball_snapshot(ship_solo).await.unwrap().unwrap().1.mode {
            eve_destiny::MovementMode::Warp { destination, .. } => {
                assert!(destination.distance(expected_origin) <= 1.0);
            }
            other => panic!("expected Warp mode, got {other:?}"),
        }
        if is_enc {
            ctx.db.with_snapshot(|snap| {
                assert_eq!(mission_fixture::get(snap, char_solo, mid_s_enc).unwrap().spawned_npc_ball_ids.len(), 2);
            });
        } else {
            assert!(solo_handle.get_ball_snapshot(ast_s_id).await.unwrap().is_some());
        }
    }

    // Part 2: beyonce.CmdWarpToStuff("bookmark", bm_id, {"fleet": True}) + direct handle_fleet_warp([bm_id])
    // materialize encounter NPCs and mining asteroids via landingPositions while staying bubble-safe & idempotent.
    let char_fc = 959_801_i64;
    let char_wing = 959_802_i64;
    let ship_fc = 959_811_i64;
    let ship_wing = 959_812_i64;
    let fleet_id = 959_820_i64;
    let bm_f_enc = 959_830_i64;
    let mid_f_enc = 98_422_u32;
    let bm_f_mine = 959_831_i64;
    let ast_f_mine = 959_832_i64;
    let mid_f_mine = 98_423_u32;
    let bm_f_direct = 959_833_i64;
    let ast_f_direct = 959_834_i64;
    let mid_f_direct = 98_424_u32;
    let pos_f_enc = glam::DVec3::new(7.0e12, 0.0, 0.0);
    let pos_f_mine = glam::DVec3::new(7.5e12, 0.0, 0.0);
    let pos_f_direct = glam::DVec3::new(8.0e12, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        let template_acc = snap.accounts.values().next().unwrap().clone();
        let template_ch = snap.characters.values().next().unwrap().clone();
        for (idx, (cid, sid)) in [(char_fc, ship_fc), (char_wing, ship_wing)].into_iter().enumerate() {
            let uid = 959_850 + idx as u64;
            let mut acc = template_acc.clone();
            acc.user_id = uid;
            acc.username = format!("fleet_cmd_warp_{idx}");
            snap.accounts.insert(acc.username.clone(), acc);
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let mut ch = template_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);
        }
        snap.bookmarks.insert(
            bm_f_enc,
            eve_db::BookmarkRecord {
                bookmark_id: bm_f_enc,
                owner_id: char_fc,
                solar_system_id: sys_id,
                label: "OWN R3 Fleet Enc".into(),
                x: pos_f_enc.x,
                y: pos_f_enc.y,
                z: pos_f_enc.z,
                item_id: None,
                memo: "Mission Pocket".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_fc, mid_f_enc, ag_fleet_enc, sys_id)
                .bookmark_id(Some(bm_f_enc))
                .site_origin_m(Some([pos_f_enc.x as i64, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![34_155])),
        );
        for (bid, aid, mid_m, ag_id, origin) in [
            (bm_f_mine, ast_f_mine, mid_f_mine, ag_fleet_mine, pos_f_mine),
            (bm_f_direct, ast_f_direct, mid_f_direct, ag_fleet_direct, pos_f_direct),
        ] {
            snap.bookmarks.insert(
                bid,
                eve_db::BookmarkRecord {
                    bookmark_id: bid,
                    owner_id: char_fc,
                    solar_system_id: sys_id,
                    label: format!("OWN R3 Fleet Mine {mid_m}"),
                    x: origin.x,
                    y: origin.y,
                    z: origin.z,
                    item_id: None,
                    memo: "Mission Pocket".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
            snap.asteroids.insert(
                aid,
                eve_db::AsteroidRecord {
                    asteroid_id: aid,
                    solar_system_id: sys_id,
                    belt_id: 0,
                    type_id: 1230,
                    quantity: 5_000,
                    initial_quantity: 5_000,
                    position: (origin.x + 8_000.0, origin.y, origin.z),
                    radius: 50.0,
                },
            );
            mission_fixture::insert(
                snap,
                MissionFixture::accepted(char_fc, mid_m, ag_id, sys_id)
                    .bookmark_id(Some(bid))
                    .site_origin_m(Some([origin.x as i64, 0, 0]))
                    .asteroid_entity_id(Some(aid))
                    .required_delivery_type_id(Some(1230))
                    .required_delivery_qty(100),
            );
            snap.missions
                .update_by_character_template(char_fc, mid_m, |m| {
                    m.kind = eve_db::MissionKind::Mining;
                })
                .unwrap();
        }
        let mut member_details = HashMap::new();
        member_details.insert(
            char_fc,
            eve_db::FleetMemberRecord {
                character_id: char_fc,
                role: eve_db::FleetRole::FleetCommander,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        member_details.insert(
            char_wing,
            eve_db::FleetMemberRecord {
                character_id: char_wing,
                role: eve_db::FleetRole::SquadMember,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: char_fc,
                members: vec![char_fc, char_wing],
                member_details,
                ..Default::default()
            },
        );
    });

    for (sid, offset) in [
        (ship_fc, glam::DVec3::ZERO),
        (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
        solo_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: sid,
                    type_id: 587,
                    position: offset,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_067_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                cs,
            )
            .await
            .unwrap();
    }
    let now = ctx.now_unix();
    for (idx, cid) in [char_fc, char_wing].into_iter().enumerate() {
        ctx.db
            .acquire_online_lock(959_850 + idx as u64, 959_870 + idx as u64, Some(cid), true, now)
            .expect("online lock");
    }

    let mut f_sess = docked_session(char_fc, sys_id, None);
    f_sess.user_id = Some(959_850);
    f_sess.session_id = 959_870;
    f_sess.ship_id = Some(ship_fc);
    f_sess.fleet_id = Some(fleet_id);
    let mut fleet_kw = HashMap::new();
    fleet_kw.insert("fleet".into(), PyRep::Bool(true));

    let fb_enc_id = 959_840_i64;
    let fb_mine_id = 959_841_i64;
    solo_handle
        .add_warp_bubble(fb_enc_id, pos_f_enc - glam::DVec3::new(500_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    solo_handle
        .add_warp_bubble(fb_mine_id, pos_f_mine - glam::DVec3::new(500_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    for (bid, is_enc) in [(bm_f_enc, true), (bm_f_mine, false)] {
        for (sid, offset) in [
            (ship_fc, glam::DVec3::ZERO),
            (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
        ] {
            let _ = solo_handle.set_movement_mode(sid, eve_destiny::MovementMode::Stop).await;
            let _ = solo_handle.set_ball_position(sid, offset).await;
            solo_handle.try_update_combat_state(sid, |cs| {
                cs.capacitor = 250.0;
                cs.max_capacitor = 250.0;
            });
        }
        let _ = ctx
            .dispatch_rpc(
                &mut f_sess,
                "beyonce",
                None,
                "CmdWarpToStuff",
                &[
                    PyRep::String("bookmark".into()),
                    PyRep::Integer(bid),
                    PyRep::Real(0.0),
                ],
                &fleet_kw,
                None,
            )
            .await
            .expect("bubbled fleet CmdWarpToStuff succeeds");
        if is_enc {
            ctx.db.with_snapshot(|snap| {
                assert!(mission_fixture::get(snap, char_fc, mid_f_enc).unwrap().spawned_npc_ball_ids.is_empty());
            });
        } else {
            assert!(solo_handle.get_ball_snapshot(ast_f_mine).await.unwrap().is_none());
        }
    }

    assert!(solo_handle.remove_warp_bubble(fb_enc_id).await.unwrap());
    assert!(solo_handle.remove_warp_bubble(fb_mine_id).await.unwrap());
    let mut enc_first_bids = Vec::new();
    for (run_idx, (bid, is_enc)) in [
        (bm_f_enc, true),
        (bm_f_enc, true), // verify idempotence on repeat fleet bookmark warp
        (bm_f_mine, false),
    ]
    .into_iter()
    .enumerate()
    {
        for (sid, offset, cap) in [
            (ship_fc, glam::DVec3::ZERO, 10.0),
            (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0), 250.0),
        ] {
            let _ = solo_handle.set_movement_mode(sid, eve_destiny::MovementMode::Stop).await;
            let _ = solo_handle.set_ball_position(sid, offset).await;
            solo_handle.try_update_combat_state(sid, move |cs| {
                cs.capacitor = cap;
                cs.max_capacitor = 250.0;
            });
        }
        let _ = ctx
            .dispatch_rpc(
                &mut f_sess,
                "beyonce",
                None,
                "CmdWarpToStuff",
                &[
                    PyRep::String("bookmark".into()),
                    PyRep::Integer(bid),
                    PyRep::Real(0.0),
                ],
                &fleet_kw,
                None,
            )
            .await
            .expect("fleet CmdWarpToStuff(bookmark) succeeds");
        if is_enc {
            let bids = ctx.db.with_snapshot(|snap| {
                mission_fixture::get(snap, char_fc, mid_f_enc).unwrap().spawned_npc_ball_ids
            });
            assert_eq!(bids.len(), 2);
            if run_idx == 0 {
                enc_first_bids = bids;
            } else {
                assert_eq!(bids, enc_first_bids);
            }
        } else {
            assert!(solo_handle.get_ball_snapshot(ast_f_mine).await.unwrap().is_some());
        }
    }

    // Direct handle_fleet_warp([bm_f_direct]) path must also materialize mission pocket.
    for (sid, offset) in [
        (ship_fc, glam::DVec3::ZERO),
        (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let _ = solo_handle.set_movement_mode(sid, eve_destiny::MovementMode::Stop).await;
        let _ = solo_handle.set_ball_position(sid, offset).await;
        solo_handle.try_update_combat_state(sid, |cs| {
            cs.capacitor = 250.0;
            cs.max_capacitor = 250.0;
        });
    }
    let _ = crate::services::fleet_social::fleet_social_fleet_ops::handle_fleet_warp(
        &ctx,
        &f_sess,
        &[PyRep::Integer(bm_f_direct)],
        &kw,
    )
    .await
    .unwrap()
    .expect("direct handle_fleet_warp(bm) succeeds");
    assert!(solo_handle.get_ball_snapshot(ast_f_direct).await.unwrap().is_some());
}

#[tokio::test]
async fn cmd_warp_to_stuff_short_stop_bubble_and_zarzakh_mission_origin_guards() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let kw = HashMap::new();
    let agent_id = *ctx.sde.agents.keys().min().expect("SDE agent exists");

    // 1. Short >=150 km CmdWarpToStuff caught by a stop bubble <150 km ahead must enter warp to the
    //    bubble perimeter rather than failing with WarpDistanceTooShort, and skip unreached pockets.
    let bm_300km = 995_801_i64;
    let bm_400km = 995_802_i64;
    let mid_400km = 98_430_u32;
    let stop_bubble_id = 995_803_i64;
    let handle_sys = ctx.systems.get_or_spawn(sys_id);
    let mut ship_cs = ctx.build_type_combat_state(587);
    ship_cs.capacitor = 250.0;
    ship_cs.max_capacitor = 250.0;
    handle_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_067_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ship_cs,
        )
        .await
        .unwrap();
    handle_sys
        .add_warp_bubble(stop_bubble_id, glam::DVec3::new(120_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = i64::from(sys_id);
            ship.flag = eve_db::InventoryFlag::None;
        }
        mission_fixture::clear_character(snap, char_id);
        for (bid, x) in [(bm_300km, 300_000.0), (bm_400km, 400_000.0)] {
            snap.bookmarks.insert(
                bid,
                eve_db::BookmarkRecord {
                    bookmark_id: bid,
                    owner_id: char_id,
                    solar_system_id: sys_id,
                    label: format!("Stop Bubble BM {bid}"),
                    x,
                    y: 0.0,
                    z: 0.0,
                    item_id: None,
                    memo: "Test BM".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
        }
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_400km, agent_id, sys_id)
                .bookmark_id(Some(bm_400km))
                .site_origin_m(Some([400_000, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![34_155])),
        );
    });

    let mut sess = docked_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);
    for bm in [bm_300km, bm_400km] {
        let _ = handle_sys.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
        let _ = handle_sys.set_ball_position(ship_id, glam::DVec3::ZERO).await;
        handle_sys.try_update_combat_state(ship_id, |cs| {
            cs.capacitor = 250.0;
            cs.max_capacitor = 250.0;
        });
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                None,
                "CmdWarpToStuff",
                &[PyRep::String("bookmark".into()), PyRep::Integer(bm)],
                &kw,
                None,
            )
            .await
            .expect("300/400 km CmdWarpToStuff caught by <150 km stop bubble must warp into bubble");
        match handle_sys.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode {
            eve_destiny::MovementMode::Warp { destination, .. } => {
                assert!(
                    destination.distance(glam::DVec3::new(100_000.0, 0.0, 0.0)) < 1.0,
                    "expected stop-bubble perimeter at (100_000, 0, 0), got {destination:?}"
                );
            }
            other => panic!("expected Warp mode into stop bubble, got {other:?}"),
        }
    }
    ctx.db.with_snapshot(|snap| {
        assert!(mission_fixture::get(snap, char_id, mid_400km).unwrap().spawned_npc_ball_ids.is_empty());
    });
    assert!(handle_sys.remove_warp_bubble(stop_bubble_id).await.unwrap());

    // 2. resolve_accepted_mission_warp_origin enforces bookmark expiry and ACL checks.
    let now = ctx.now_unix();
    let other_char_id = 90_000_099_i64;
    let bm_expired = 995_804_i64;
    let bm_foreign = 995_805_i64;
    let bm_valid = 995_806_i64;
    let mid_exp = 98_431_u32;
    let mid_for = 98_432_u32;
    let mid_ok = 98_433_u32;
    let custom_origin = glam::DVec3::new(9.1e12, 1_000.0, 2_000.0);
    let (exp_inst, for_inst) = ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        snap.bookmarks.insert(
            bm_expired,
            eve_db::BookmarkRecord {
                bookmark_id: bm_expired,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Expired Mission BM".into(),
                x: custom_origin.x,
                y: custom_origin.y,
                z: custom_origin.z,
                item_id: None,
                memo: "Expired".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmark_expiries.insert(bm_expired, now.saturating_sub(10));
        snap.bookmarks.insert(
            bm_foreign,
            eve_db::BookmarkRecord {
                bookmark_id: bm_foreign,
                owner_id: other_char_id,
                solar_system_id: sys_id,
                label: "Foreign Unshared BM".into(),
                x: custom_origin.x,
                y: custom_origin.y,
                z: custom_origin.z,
                item_id: None,
                memo: "Private".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            bm_valid,
            eve_db::BookmarkRecord {
                bookmark_id: bm_valid,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Valid Mission BM".into(),
                x: custom_origin.x,
                y: custom_origin.y,
                z: custom_origin.z,
                item_id: None,
                memo: "Valid".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        mission_fixture::insert(snap, MissionFixture::accepted(char_id, mid_exp, agent_id, sys_id).bookmark_id(Some(bm_expired)));
        mission_fixture::insert(snap, MissionFixture::accepted(char_id, mid_for, agent_id, sys_id).bookmark_id(Some(bm_foreign)));
        mission_fixture::insert(snap, MissionFixture::accepted(char_id, mid_ok, agent_id, sys_id).bookmark_id(Some(bm_valid)));
        (
            snap.missions.get_by_character_template(char_id, mid_exp).unwrap().instance_id,
            snap.missions.get_by_character_template(char_id, mid_for).unwrap().instance_id,
        )
    });
    assert_eq!(
        agents_missions_warp::resolve_accepted_mission_warp_origin(&ctx, &sess, char_id, sys_id, i64::from(mid_exp)).unwrap(),
        Some(agents_missions_warp::mission_site_origin_for_instance(&ctx, sys_id, exp_inst)),
        "expired bookmark fallback must not expose expired bookmark coordinates"
    );
    assert_eq!(
        agents_missions_warp::resolve_accepted_mission_warp_origin(&ctx, &sess, char_id, sys_id, i64::from(mid_for)).unwrap(),
        Some(agents_missions_warp::mission_site_origin_for_instance(&ctx, sys_id, for_inst)),
        "inaccessible foreign bookmark fallback must not bypass ACL"
    );
    assert_eq!(
        agents_missions_warp::resolve_accepted_mission_warp_origin(&ctx, &sess, char_id, sys_id, i64::from(mid_ok)).unwrap(),
        Some(custom_origin)
    );

    // 3. Zarzakh DunBlacklistCannotWarp check on CmdWarpToStuff("mission", mid), fleet mission warp,
    //    and non-Zarzakh mission with attached Zarzakh bookmark.
    let zz_sys = crate::services::space_warp_jump::SOLAR_SYSTEM_ZARZAKH;
    let zz_mid = 98_434_u32;
    let zz_bm = 995_807_i64;
    let zz_fleet_id = 995_810_i64;
    let zz_ship_id = 100_000_801_i64;
    let jita_zz_mid = 98_435_u32;
    let zz_handle = ctx.systems.get_or_spawn(zz_sys);
    zz_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: zz_ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_067_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = zz_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = zz_ship_id;
        }
        snap.items.insert(
            zz_ship_id,
            eve_db::InventoryItemRecord {
                item_id: zz_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(zz_sys),
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.bookmarks.insert(
            zz_bm,
            eve_db::BookmarkRecord {
                bookmark_id: zz_bm,
                owner_id: char_id,
                solar_system_id: zz_sys,
                label: "Zarzakh Mission BM".into(),
                x: 5.0e12,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: "Zarzakh".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, zz_mid, agent_id, zz_sys).bookmark_id(Some(zz_bm)),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, jita_zz_mid, agent_id, sys_id).bookmark_id(Some(zz_bm)),
        );
        let mut member_details = HashMap::new();
        member_details.insert(
            char_id,
            eve_db::FleetMemberRecord {
                character_id: char_id,
                role: eve_db::FleetRole::FleetCommander,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            zz_fleet_id,
            eve_db::FleetRecord {
                fleet_id: zz_fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                member_details,
                ..Default::default()
            },
        );
    });
    ctx.db
        .acquire_online_lock(1, 995_820, Some(char_id), true, now)
        .expect("online lock");
    let mut zz_sess = docked_session(char_id, zz_sys, None);
    zz_sess.session_id = 995_820;
    zz_sess.ship_id = Some(zz_ship_id);
    zz_sess.fleet_id = Some(zz_fleet_id);
    let zz_err = ctx
        .dispatch_rpc(
            &mut zz_sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("mission".into()), PyRep::Integer(i64::from(zz_mid))],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(zz_err.code, "DunBlacklistCannotWarp");
    let mut fleet_kw = HashMap::new();
    fleet_kw.insert("fleet".into(), PyRep::Bool(true));
    let zz_fleet_err = ctx
        .dispatch_rpc(
            &mut zz_sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("mission".into()), PyRep::Integer(i64::from(zz_mid))],
            &fleet_kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(zz_fleet_err.code, "DunBlacklistCannotWarp");

    // Non-Zarzakh mission with attached Zarzakh bookmark must also reject with DunBlacklistCannotWarp.
    let _ = handle_sys.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
    let _ = handle_sys.set_ball_position(ship_id, glam::DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    sess.session_id = 995_820;
    let jita_zz_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("mission".into()), PyRep::Integer(i64::from(jita_zz_mid))],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(jita_zz_err.code, "DunBlacklistCannotWarp");
}

#[tokio::test]
async fn cmd_warp_to_stuff_legacy_mission_bookmark_guards_and_downtime_nan_safety() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let other_char_id = 90_000_099_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();
    let custom_origin = glam::DVec3::new(9.2e12, 1_000.0, 2_000.0);

    let mut agents: Vec<i64> = ctx.sde.agents.keys().copied().collect();
    agents.sort_unstable();
    assert!(agents.len() >= 5);

    let bm_expired = 995_901_i64;
    let bm_foreign = 995_902_i64;
    let bm_item = 995_903_i64;
    let bm_wrong_sys = 995_904_i64;
    let bm_item_mine = 995_905_i64;
    let ast_item_id = 995_906_i64;

    let mid_exp = 98_501_u32;
    let mid_for = 98_502_u32;
    let mid_item = 98_503_u32;
    let mid_wrong_sys = 98_504_u32;
    let mid_item_mine = 98_505_u32;

    let handle_sys = ctx.systems.get_or_spawn(sys_id);
    let mut ship_cs = ctx.build_type_combat_state(587);
    ship_cs.capacitor = 250.0;
    ship_cs.max_capacitor = 250.0;
    handle_sys
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_067_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ship_cs,
        )
        .await
        .unwrap();

    let (inst_exp, inst_for, inst_item, inst_wrong_sys, inst_mine) = ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::clear_character(snap, char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.bookmarks.insert(
            bm_expired,
            eve_db::BookmarkRecord {
                bookmark_id: bm_expired,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Expired Legacy BM".into(),
                x: custom_origin.x,
                y: custom_origin.y,
                z: custom_origin.z,
                item_id: None,
                memo: "Expired".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmark_expiries.insert(bm_expired, now.saturating_sub(10));
        snap.bookmarks.insert(
            bm_foreign,
            eve_db::BookmarkRecord {
                bookmark_id: bm_foreign,
                owner_id: other_char_id,
                solar_system_id: sys_id,
                label: "Foreign Legacy BM".into(),
                x: custom_origin.x,
                y: custom_origin.y,
                z: custom_origin.z,
                item_id: None,
                memo: "Private".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            bm_item,
            eve_db::BookmarkRecord {
                bookmark_id: bm_item,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Item Attached Legacy BM".into(),
                x: custom_origin.x,
                y: custom_origin.y,
                z: custom_origin.z,
                item_id: Some(123_456),
                memo: "Station Item".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            bm_wrong_sys,
            eve_db::BookmarkRecord {
                bookmark_id: bm_wrong_sys,
                owner_id: char_id,
                solar_system_id: sys_id + 1,
                label: "Wrong System Legacy BM".into(),
                x: custom_origin.x,
                y: custom_origin.y,
                z: custom_origin.z,
                item_id: None,
                memo: "Other System".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            bm_item_mine,
            eve_db::BookmarkRecord {
                bookmark_id: bm_item_mine,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Item Attached Mining BM".into(),
                x: custom_origin.x,
                y: custom_origin.y,
                z: custom_origin.z,
                item_id: Some(123_457),
                memo: "Station Item".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.asteroids.insert(
            ast_item_id,
            eve_db::AsteroidRecord {
                asteroid_id: ast_item_id,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (custom_origin.x, custom_origin.y, custom_origin.z),
                radius: 50.0,
            },
        );

        for (idx, (mid, bmid)) in [
            (mid_exp, bm_expired),
            (mid_for, bm_foreign),
            (mid_item, bm_item),
            (mid_wrong_sys, bm_wrong_sys),
        ]
        .into_iter()
        .enumerate()
        {
            mission_fixture::insert(
                snap,
                MissionFixture::accepted(char_id, mid, agents[idx], sys_id)
                    .bookmark_id(Some(bmid))
                    .npcs_remaining(2)
                    .initial_wave_count(Some(2))
                    .initial_wave_types(Some(vec![34_155])),
            );
        }
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_id, mid_item_mine, agents[4], sys_id)
                .bookmark_id(Some(bm_item_mine))
                .asteroid_entity_id(Some(ast_item_id))
                .required_delivery_type_id(Some(1230))
                .required_delivery_qty(100),
        );
        snap.missions
            .update_by_character_template(char_id, mid_item_mine, |m| {
                m.kind = eve_db::MissionKind::Mining;
            })
            .unwrap();

        (
            snap.missions.get_by_character_template(char_id, mid_exp).unwrap().instance_id,
            snap.missions.get_by_character_template(char_id, mid_for).unwrap().instance_id,
            snap.missions.get_by_character_template(char_id, mid_item).unwrap().instance_id,
            snap.missions.get_by_character_template(char_id, mid_wrong_sys).unwrap().instance_id,
            snap.missions.get_by_character_template(char_id, mid_item_mine).unwrap().instance_id,
        )
    });

    // In-memory NaN bookmark check (WorldRepository rejects non-finite DB writes, so test canonical
    // helper directly on a corrupted in-memory snapshot as well as NaN inputs).
    let mut nan_snap = ctx.db.with_snapshot(|s| s.clone());
    if let Some(bm) = nan_snap.bookmarks.get_mut(&bm_item) {
        bm.item_id = None;
        bm.x = f64::NAN;
    }
    let nan_mission = nan_snap.missions.get_by_character_template(char_id, mid_item).unwrap().clone();
    assert_eq!(
        agents_missions_warp::resolve_mission_pocket_origin_in_snap(
            &ctx,
            &nan_snap,
            None,
            &nan_mission,
            sys_id,
            now,
            false,
        )
        .unwrap(),
        None
    );
    assert_eq!(
        agents_missions_warp::resolve_mission_pocket_origin_in_snap(
            &ctx,
            &nan_snap,
            None,
            &nan_mission,
            sys_id,
            now,
            true,
        )
        .unwrap(),
        Some(agents_missions_warp::mission_site_origin_for_instance(
            &ctx,
            sys_id,
            inst_item,
        ))
    );

    // Unrelated proximity/downtime entry near `custom_origin` or with `NaN` must NOT trigger spawns
    // on expired/foreign/item-attached/wrong-system legacy mission bookmarks.
    assert_eq!(
        crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_if_needed(
            &ctx,
            char_id,
            sys_id,
            glam::DVec3::NAN,
        ),
        0
    );
    assert_eq!(
        crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_if_needed(
            &ctx,
            char_id,
            sys_id,
            custom_origin,
        ),
        0
    );
    agents_missions_warp::materialize_mission_pocket_on_cmd_warp(
        &ctx,
        &handle_sys,
        char_id,
        sys_id,
        "celestial",
        999_999,
        glam::DVec3::NAN,
    )
    .await;
    agents_missions_warp::materialize_mission_pocket_on_cmd_warp(
        &ctx,
        &handle_sys,
        char_id,
        sys_id,
        "celestial",
        999_999,
        custom_origin,
    )
    .await;
    ctx.db.with_snapshot(|snap| {
        for mid in [mid_exp, mid_for, mid_item, mid_wrong_sys] {
            assert!(
                mission_fixture::get(snap, char_id, mid).unwrap().spawned_npc_ball_ids.is_empty(),
                "proximity warp at rejected bookmark coords must not materialize mission {mid}"
            );
        }
    });
    assert!(handle_sys.get_ball_snapshot(ast_item_id).await.unwrap().is_none());

    // Live CmdWarpToStuff("mission", mid) for all 5 legacy cases falls back to
    // mission_site_origin_for_instance AND materializes NPCs / asteroid there.
    let mut sess = docked_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);
    for (mid, inst_id, is_enc) in [
        (mid_exp, inst_exp, true),
        (mid_for, inst_for, true),
        (mid_item, inst_item, true),
        (mid_wrong_sys, inst_wrong_sys, true),
        (mid_item_mine, inst_mine, false),
    ] {
        let _ = handle_sys.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
        let _ = handle_sys.set_ball_position(ship_id, glam::DVec3::ZERO).await;
        handle_sys.try_update_combat_state(ship_id, |cs| {
            cs.capacitor = 250.0;
            cs.max_capacitor = 250.0;
        });
        let expected_origin = agents_missions_warp::mission_site_origin_for_instance(&ctx, sys_id, inst_id);
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                None,
                "CmdWarpToStuff",
                &[PyRep::String("mission".into()), PyRep::Integer(i64::from(mid))],
                &kw,
                None,
            )
            .await
            .expect("CmdWarpToStuff mission fallback succeeds");
        match handle_sys.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode {
            eve_destiny::MovementMode::Warp { destination, .. } => {
                assert!(
                    destination.distance(expected_origin) <= 1.0,
                    "ship must warp to canonical fallback for mission {mid}, got {destination:?} vs {expected_origin:?}"
                );
            }
            other => panic!("expected Warp mode for mission {mid}, got {other:?}"),
        }
        if is_enc {
            let bids = ctx.db.with_snapshot(|snap| {
                mission_fixture::get(snap, char_id, mid).unwrap().spawned_npc_ball_ids
            });
            assert_eq!(bids.len(), 2, "legacy mission {mid} must materialize 2 NPCs");
            for &bid in &bids {
                let (_, b) = handle_sys.get_ball_snapshot(bid).await.unwrap().expect("npc ball spawned");
                assert!(b.position.is_finite());
                assert!(
                    (b.position.distance(expected_origin) - 4_500.0).abs() < 1.0,
                    "NPC {bid} for mission {mid} must spawn around canonical fallback"
                );
            }
        } else {
            assert!(
                handle_sys.get_ball_snapshot(ast_item_id).await.unwrap().is_some(),
                "legacy mining mission {mid} must materialize asteroid on fallback warp"
            );
        }
    }
}

#[tokio::test]
async fn agent_warp_to_location_fleet_bookmark_materializes_wingmate_pocket() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();
    let mut agents: Vec<i64> = ctx.sde.agents.keys().copied().collect();
    agents.sort_unstable();
    assert!(agents.len() >= 2);

    let corp_id = 98_959_001_i64;
    let folder_id = 959_910_i64;
    let char_fc = 959_901_i64;
    let char_wing = 959_902_i64;
    let ship_fc = 959_911_i64;
    let ship_wing = 959_912_i64;
    let fleet_id = 959_920_i64;
    let bm_wing_enc = 959_930_i64;
    let bm_wing_mine = 959_931_i64;
    let ast_wing_id = 959_932_i64;
    let mid_wing_enc = 98_510_u32;
    let mid_wing_mine = 98_511_u32;
    let pos_wing_enc = glam::DVec3::new(9.5e12, 0.0, 0.0);
    let pos_wing_mine = glam::DVec3::new(9.8e12, 0.0, 0.0);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        let template_acc = snap.accounts.values().next().unwrap().clone();
        let template_ch = snap.characters.values().next().unwrap().clone();
        for (idx, (cid, sid)) in [(char_fc, ship_fc), (char_wing, ship_wing)].into_iter().enumerate() {
            let uid = 959_950 + idx as u64;
            let mut acc = template_acc.clone();
            acc.user_id = uid;
            acc.username = format!("wing_bm_fleet_{idx}");
            snap.accounts.insert(acc.username.clone(), acc);
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let mut ch = template_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.corporation_id = corp_id;
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);
            mission_fixture::clear_character(snap, cid);
        }
        snap.bookmark_folders.insert(
            folder_id,
            eve_db::BookmarkFolderRecord {
                folder_id,
                owner_id: char_wing,
                folder_name: "Wing Shared".into(),
                is_shared: true,
                corporation_id: Some(corp_id),
                ..Default::default()
            },
        );
        snap.bookmark_folder_acls.insert(folder_id, vec![char_fc, char_wing]);
        snap.bookmarks.insert(
            bm_wing_enc,
            eve_db::BookmarkRecord {
                bookmark_id: bm_wing_enc,
                owner_id: char_wing,
                solar_system_id: sys_id,
                label: "Wingmate Shared Encounter".into(),
                x: pos_wing_enc.x,
                y: pos_wing_enc.y,
                z: pos_wing_enc.z,
                item_id: None,
                memo: "Wing Enc".into(),
                folder_id: Some(folder_id),
                is_shared: true,
            },
        );
        snap.bookmarks.insert(
            bm_wing_mine,
            eve_db::BookmarkRecord {
                bookmark_id: bm_wing_mine,
                owner_id: char_wing,
                solar_system_id: sys_id,
                label: "Wingmate Shared Mining".into(),
                x: pos_wing_mine.x,
                y: pos_wing_mine.y,
                z: pos_wing_mine.z,
                item_id: None,
                memo: "Wing Mine".into(),
                folder_id: Some(folder_id),
                is_shared: true,
            },
        );
        snap.asteroids.insert(
            ast_wing_id,
            eve_db::AsteroidRecord {
                asteroid_id: ast_wing_id,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (pos_wing_mine.x + 8_000.0, pos_wing_mine.y, pos_wing_mine.z),
                radius: 50.0,
            },
        );
        // Only the wingmate holds missions; the FC holds zero missions.
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_wing, mid_wing_enc, agents[0], sys_id)
                .bookmark_id(Some(bm_wing_enc))
                .site_origin_m(Some([pos_wing_enc.x as i64, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![34_155])),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(char_wing, mid_wing_mine, agents[1], sys_id)
                .bookmark_id(Some(bm_wing_mine))
                .site_origin_m(Some([pos_wing_mine.x as i64, 0, 0]))
                .asteroid_entity_id(Some(ast_wing_id))
                .required_delivery_type_id(Some(1230))
                .required_delivery_qty(100),
        );
        snap.missions
            .update_by_character_template(char_wing, mid_wing_mine, |m| {
                m.kind = eve_db::MissionKind::Mining;
            })
            .unwrap();
        let mut member_details = HashMap::new();
        member_details.insert(
            char_fc,
            eve_db::FleetMemberRecord {
                character_id: char_fc,
                role: eve_db::FleetRole::FleetCommander,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        member_details.insert(
            char_wing,
            eve_db::FleetMemberRecord {
                character_id: char_wing,
                role: eve_db::FleetRole::SquadMember,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            fleet_id,
            eve_db::FleetRecord {
                fleet_id,
                boss_id: char_fc,
                members: vec![char_fc, char_wing],
                member_details,
                ..Default::default()
            },
        );
    });

    for (idx, (cid, sid, offset)) in [
        (char_fc, ship_fc, glam::DVec3::ZERO),
        (char_wing, ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
    ]
    .into_iter()
    .enumerate()
    {
        let mut cs = ctx.build_type_combat_state(587);
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
        sys_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: sid,
                    type_id: 587,
                    position: offset,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_067_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                cs,
            )
            .await
            .unwrap();
        ctx.db
            .acquire_online_lock(959_950 + idx as u64, 959_970 + idx as u64, Some(cid), true, now)
            .expect("online lock");
    }

    let mut fc_sess = docked_session(char_fc, sys_id, None);
    fc_sess.user_id = Some(959_950);
    fc_sess.session_id = 959_970;
    fc_sess.corporation_id = Some(corp_id);
    fc_sess.ship_id = Some(ship_fc);
    fc_sess.fleet_id = Some(fleet_id);

    for (bm_id, is_enc) in [(bm_wing_enc, true), (bm_wing_mine, false)] {
        for (sid, offset) in [
            (ship_fc, glam::DVec3::ZERO),
            (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0)),
        ] {
            let _ = sys_handle.set_movement_mode(sid, eve_destiny::MovementMode::Stop).await;
            let _ = sys_handle.set_ball_position(sid, offset).await;
            sys_handle.try_update_combat_state(sid, |cs| {
                cs.capacitor = 250.0;
                cs.max_capacitor = 250.0;
            });
        }
        let res = ctx
            .dispatch_rpc(
                &mut fc_sess,
                "agentMgr",
                None,
                "WarpToLocation",
                &[
                    PyRep::String("bookmark".into()),
                    PyRep::Integer(bm_id),
                    PyRep::Real(0.0),
                    PyRep::Bool(true),
                ],
                &kw,
                None,
            )
            .await
            .expect("agentMgr.WarpToLocation fleet bookmark succeeds");
        assert_eq!(res, PyRep::Bool(true));
        if is_enc {
            let bids = ctx.db.with_snapshot(|snap| {
                mission_fixture::get(snap, char_wing, mid_wing_enc)
                    .unwrap()
                    .spawned_npc_ball_ids
            });
            assert_eq!(
                bids.len(),
                2,
                "agentMgr.WarpToLocation fleet bookmark must materialize wingmate's NPCs"
            );
            for &bid in &bids {
                let (_, b) = sys_handle.get_ball_snapshot(bid).await.unwrap().expect("wingmate NPC ball spawned");
                assert!((b.position.distance(pos_wing_enc) - 4_500.0).abs() < 1.0);
            }
        } else {
            assert!(
                sys_handle.get_ball_snapshot(ast_wing_id).await.unwrap().is_some(),
                "agentMgr.WarpToLocation fleet bookmark must materialize wingmate's mining asteroid"
            );
        }
    }
}

struct SharedMissionBookmarkFixture {
    ctx: ServerContext,
    sys_id: u32,
    pilot_a: i64,
    pilot_b: i64,
    ship_a: i64,
    ship_b: i64,
    bm_enc: i64,
    bm_mine: i64,
    ast_id: i64,
    mid_enc: u32,
    mid_mine: u32,
    pos_enc: glam::DVec3,
    pos_mine: glam::DVec3,
    fleet_id: i64,
    sess_a: SessionState,
}

async fn setup_shared_mission_bookmark_fixture(
    takes_fleet_warp_b: bool,
    dock_b: bool,
) -> SharedMissionBookmarkFixture {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let mut agents: Vec<i64> = ctx.sde.agents.keys().copied().collect();
    agents.sort_unstable();
    let (corp_id, folder_id) = (98_960_001_i64, 960_010_i64);
    let (pilot_a, pilot_b) = (960_001_i64, 960_002_i64);
    let (ship_a, ship_b) = (960_011_i64, 960_012_i64);
    let (fleet_id, bm_enc, bm_mine, ast_id) = (960_020_i64, 960_030_i64, 960_031_i64, 960_032_i64);
    let (mid_enc, mid_mine) = (98_520_u32, 98_521_u32);
    let pos_enc = glam::DVec3::new(9.6e12, 0.0, 0.0);
    let pos_mine = glam::DVec3::new(9.9e12, 0.0, 0.0);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        let tpl_acc = snap.accounts.values().next().unwrap().clone();
        let tpl_ch = snap.characters.values().next().unwrap().clone();
        for (idx, (cid, sid, docked)) in
            [(pilot_a, ship_a, false), (pilot_b, ship_b, dock_b)].into_iter().enumerate()
        {
            let uid = 960_050 + idx as u64;
            let mut acc = tpl_acc.clone();
            acc.user_id = uid;
            acc.username = format!("shared_bm_{idx}");
            snap.accounts.insert(acc.username.clone(), acc);
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: if docked { station_id } else { i64::from(sys_id) },
                    flag: if docked { eve_db::InventoryFlag::Hangar } else { eve_db::InventoryFlag::None },
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            let mut ch = tpl_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.corporation_id = corp_id;
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = docked.then_some(station_id);
            ch.structure_id = None;
            snap.characters.insert(cid, ch);
            mission_fixture::clear_character(snap, cid);
        }
        snap.bookmark_folders.insert(
            folder_id,
            eve_db::BookmarkFolderRecord {
                folder_id,
                owner_id: pilot_b,
                folder_name: "ACL Shared".into(),
                is_shared: true,
                corporation_id: Some(corp_id),
                ..Default::default()
            },
        );
        snap.bookmark_folder_acls.insert(folder_id, vec![pilot_a, pilot_b]);
        for (bid, label, pos) in [(bm_enc, "Shared Enc", pos_enc), (bm_mine, "Shared Mine", pos_mine)] {
            snap.bookmarks.insert(
                bid,
                eve_db::BookmarkRecord {
                    bookmark_id: bid,
                    owner_id: pilot_b,
                    solar_system_id: sys_id,
                    label: label.into(),
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    item_id: None,
                    memo: label.into(),
                    folder_id: Some(folder_id),
                    is_shared: true,
                },
            );
        }
        snap.asteroids.insert(
            ast_id,
            eve_db::AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (pos_mine.x + 8_000.0, pos_mine.y, pos_mine.z),
                radius: 50.0,
            },
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(pilot_b, mid_enc, agents[0], sys_id)
                .bookmark_id(Some(bm_enc))
                .site_origin_m(Some([pos_enc.x as i64, 0, 0]))
                .npcs_remaining(2)
                .initial_wave_count(Some(2))
                .initial_wave_types(Some(vec![34_155])),
        );
        mission_fixture::insert(
            snap,
            MissionFixture::accepted(pilot_b, mid_mine, agents[1], sys_id)
                .bookmark_id(Some(bm_mine))
                .site_origin_m(Some([pos_mine.x as i64, 0, 0]))
                .asteroid_entity_id(Some(ast_id))
                .required_delivery_type_id(Some(1230))
                .required_delivery_qty(100),
        );
        snap.missions
            .update_by_character_template(pilot_b, mid_mine, |m| m.kind = eve_db::MissionKind::Mining)
            .unwrap();
        let mut member_details = HashMap::new();
        member_details.insert(pilot_a, eve_db::FleetMemberRecord { character_id: pilot_a, role: eve_db::FleetRole::FleetCommander, takes_fleet_warp: true, ..Default::default() });
        member_details.insert(pilot_b, eve_db::FleetMemberRecord { character_id: pilot_b, role: eve_db::FleetRole::SquadMember, takes_fleet_warp: takes_fleet_warp_b, ..Default::default() });
        snap.fleets.insert(fleet_id, eve_db::FleetRecord { fleet_id, boss_id: pilot_a, members: vec![pilot_a, pilot_b], member_details, ..Default::default() });
    });
    for (idx, (cid, sid, spawn_ball)) in
        [(pilot_a, ship_a, true), (pilot_b, ship_b, !dock_b)].into_iter().enumerate()
    {
        if spawn_ball {
            let mut cs = ctx.build_type_combat_state(587);
            cs.capacitor = 250.0;
            cs.max_capacitor = 250.0;
            sys_handle
                .spawn_ball_with_combat(
                    eve_destiny::Ball { id: sid, type_id: 587, position: glam::DVec3::new((idx as f64) * 1_000.0, 0.0, 0.0), velocity: glam::DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_067_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop },
                    cs,
                )
                .await
                .unwrap();
        }
        ctx.db.acquire_online_lock(960_050 + idx as u64, 960_070 + idx as u64, Some(cid), true, now).unwrap();
    }
    let mut sess_a = docked_session(pilot_a, sys_id, None);
    sess_a.user_id = Some(960_050);
    sess_a.session_id = 960_070;
    sess_a.corporation_id = Some(corp_id);
    sess_a.ship_id = Some(ship_a);
    sess_a.fleet_id = Some(fleet_id);
    SharedMissionBookmarkFixture { ctx, sys_id, pilot_a, pilot_b, ship_a, ship_b, bm_enc, bm_mine, ast_id, mid_enc, mid_mine, pos_enc, pos_mine, fleet_id, sess_a }
}

#[tokio::test]
async fn solo_shared_bookmark_warp_materializes_owner_encounter_and_mining_pockets() {
    let mut f = setup_shared_mission_bookmark_fixture(true, false).await;
    let kw = HashMap::new();
    let h = f.ctx.systems.get_or_spawn(f.sys_id);
    let _ = f.ctx.dispatch_rpc(&mut f.sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(f.bm_enc)], &kw, None).await.expect("solo CmdWarpToStuff shared enc");
    let _ = h.set_movement_mode(f.ship_a, eve_destiny::MovementMode::Stop).await;
    let _ = h.set_ball_position(f.ship_a, glam::DVec3::ZERO).await;
    h.try_update_combat_state(f.ship_a, |cs| {
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
    });
    f.ctx.dispatch_rpc(&mut f.sess_a, "agentMgr", None, "WarpToLocation", &[PyRep::String("bookmark".into()), PyRep::Integer(f.bm_mine), PyRep::Real(0.0), PyRep::Bool(false)], &kw, None).await.expect("solo WarpToLocation shared mine");
    let (bids, mine_ok) = f.ctx.db.with_snapshot(|s| (
        mission_fixture::get(s, f.pilot_b, f.mid_enc).unwrap().spawned_npc_ball_ids,
        mission_fixture::get(s, f.pilot_b, f.mid_mine).is_some(),
    ));
    assert!(mine_ok);
    assert_eq!(bids.len(), 2);
    assert!(h.get_ball_snapshot(bids[0]).await.unwrap().unwrap().1.position.distance(f.pos_enc) <= 5_000.0);
    let (_, ast_b) = h.get_ball_snapshot(f.ast_id).await.unwrap().expect("shared mining asteroid");
    assert!(ast_b.position.distance(f.pos_mine) <= 9_000.0);
}

#[tokio::test]
async fn solo_copied_bookmark_warp_materializes_source_mission_pocket() {
    let mut f = setup_shared_mission_bookmark_fixture(true, false).await;
    let kw = HashMap::new();
    let h = f.ctx.systems.get_or_spawn(f.sys_id);
    let copied = f.ctx.dispatch_rpc(&mut f.sess_a, "bookmark", None, "CopyBookmarksToFolderAndSubfolder", &[PyRep::List(vec![PyRep::Integer(f.bm_enc)]), PyRep::None], &kw, None).await.expect("copy shared bookmark");
    let bm_copy = match copied { PyRep::KeyVal(kv) => kv.get("bookmarkIDs").and_then(|v| match v { PyRep::List(l) => l.first().and_then(PyRep::as_i64), _ => None }).unwrap(), _ => panic!("expected KeyVal") };
    f.ctx.db.with_snapshot(|s| assert_eq!(s.bookmarks.get(&bm_copy).unwrap().owner_id, f.pilot_a));
    assert_ne!(bm_copy, f.bm_enc);
    let _ = f.ctx.dispatch_rpc(&mut f.sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(bm_copy)], &kw, None).await.expect("solo warp to copied bookmark");
    let bids = f.ctx.db.with_snapshot(|s| mission_fixture::get(s, f.pilot_b, f.mid_enc).unwrap().spawned_npc_ball_ids);
    assert_eq!(bids.len(), 2);
    assert!(h.get_ball_snapshot(bids[0]).await.unwrap().unwrap().1.position.distance(f.pos_enc) <= 5_000.0);
}

#[tokio::test]
async fn fleet_bookmark_warp_owner_exempt_or_docked_still_materializes_pocket() {
    let mut f = setup_shared_mission_bookmark_fixture(false, false).await;
    let kw = HashMap::new();
    let h = f.ctx.systems.get_or_spawn(f.sys_id);
    f.ctx.dispatch_rpc(&mut f.sess_a, "fleetMgr", None, "FleetWarp", &[PyRep::Integer(f.bm_enc), PyRep::Real(0.0)], &kw, None).await.expect("FleetWarp with takes_fleet_warp=false owner");
    let bids = f.ctx.db.with_snapshot(|s| mission_fixture::get(s, f.pilot_b, f.mid_enc).unwrap().spawned_npc_ball_ids);
    assert_eq!(bids.len(), 2);
    assert!(h.get_ball_snapshot(bids[0]).await.unwrap().is_some());
    let _ = f.ship_b;

    let mut f_docked = setup_shared_mission_bookmark_fixture(true, true).await;
    let mut fleet_kw = HashMap::new();
    fleet_kw.insert("fleet".into(), PyRep::Bool(true));
    let _ = f_docked.ctx.dispatch_rpc(&mut f_docked.sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(f_docked.bm_mine)], &fleet_kw, None).await.expect("fleet CmdWarpToStuff with docked owner");
    assert!(f_docked.ctx.systems.get_or_spawn(f_docked.sys_id).get_ball_snapshot(f_docked.ast_id).await.unwrap().is_some());
    assert_eq!(f_docked.fleet_id, 960_020);
}

#[tokio::test]
async fn expired_or_item_attached_bookmark_never_materializes_foreign_mission() {
    let f = setup_shared_mission_bookmark_fixture(true, false).await;
    let now = f.ctx.now_unix();
    let (bm_exp, bm_item) = (960_090_i64, 960_091_i64);
    f.ctx.db.with_mut_snapshot(|snap| {
        for (bid, item_id) in [(bm_exp, None), (bm_item, Some(999_i64))] {
            snap.bookmarks.insert(bid, eve_db::BookmarkRecord { bookmark_id: bid, owner_id: f.pilot_b, solar_system_id: f.sys_id, label: "Invalid".into(), x: f.pos_enc.x, y: f.pos_enc.y, z: f.pos_enc.z, item_id, memo: "Invalid".into(), folder_id: None, is_shared: true });
        }
        snap.bookmark_expiries.insert(bm_exp, now.saturating_sub(10));
    });
    let h = f.ctx.systems.get_or_spawn(f.sys_id);
    for bid in [bm_exp, bm_item] {
        agents_missions_warp::materialize_mission_pocket_on_cmd_warp(&f.ctx, &h, f.pilot_a, f.sys_id, "bookmark", bid, f.pos_enc).await;
        agents_missions_warp::materialize_mission_pocket_on_fleet_warp(&f.ctx, &h, f.pilot_a, f.sys_id, "bookmark", bid, &[(f.pilot_a, f.pos_enc)]).await;
    }
    f.ctx.db.with_snapshot(|s| assert!(mission_fixture::get(s, f.pilot_b, f.mid_enc).unwrap().spawned_npc_ball_ids.is_empty()));
}

#[tokio::test]
async fn site_origin_precedes_bookmark_across_briefing_warp_and_materialization() {
    let mut f = setup_shared_mission_bookmark_fixture(true, false).await;
    let kw = HashMap::new();
    let h = f.ctx.systems.get_or_spawn(f.sys_id);
    let mut agents: Vec<i64> = f.ctx.sde.agents.keys().copied().collect();
    agents.sort_unstable();
    let (mid_1, mid_2) = (98_530_u32, 98_531_u32);
    let (bm_1, bm_2) = (960_130_i64, 960_131_i64);
    let site_1 = glam::DVec3::new(4.2e10, 1.5e8, -2.5e8);
    let site_2 = glam::DVec3::new(5.4e10, -1.8e8, 2.2e8);
    let div_bm_1 = glam::DVec3::new(9.1e11, 0.0, 0.0);
    let div_bm_2 = glam::DVec3::new(9.5e11, 0.0, 0.0);

    f.ctx.db.with_mut_snapshot(|snap| {
        for (bid, pos, mid, ag_id, site) in [
            (bm_1, div_bm_1, mid_1, agents[0], site_1),
            (bm_2, div_bm_2, mid_2, agents[1], site_2),
        ] {
            snap.bookmarks.insert(
                bid,
                eve_db::BookmarkRecord {
                    bookmark_id: bid,
                    owner_id: f.pilot_a,
                    solar_system_id: f.sys_id,
                    label: "Diverging Bookmark".into(),
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    item_id: None,
                    memo: "Diverging".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
            mission_fixture::insert(
                snap,
                MissionFixture::accepted(f.pilot_a, mid, ag_id, f.sys_id)
                    .bookmark_id(Some(bid))
                    .site_origin_m(Some([site.x as i64, site.y as i64, site.z as i64]))
                    .npcs_remaining(2)
                    .initial_wave_count(Some(2))
                    .initial_wave_types(Some(vec![34_155])),
            );
        }
    });

    for (ag_id, mid, expected_site) in [(agents[0], mid_1, site_1), (agents[1], mid_2, site_2)] {
        let journal = f
            .ctx
            .dispatch_rpc(
                &mut f.sess_a,
                "agentMgr",
                None,
                "GetMissionJournalInfo",
                &[PyRep::Integer(ag_id), PyRep::Integer(i64::from(mid))],
                &kw,
                None,
            )
            .await
            .expect("GetMissionJournalInfo");
        let PyRep::List(locs) = journal.dict_get("locations").expect("locations list") else {
            panic!("expected locations PyRep::List");
        };
        let dun_loc = locs
            .iter()
            .find(|l| l.dict_get("locationType").and_then(PyRep::as_str) == Some("dungeon"))
            .expect("dungeon location entry");
        let PyRep::Tuple(coords) = dun_loc.dict_get("coords").expect("dungeon coords") else {
            panic!("expected coords tuple");
        };
        let journal_pos = glam::DVec3::new(
            coords[0].as_f64().unwrap(),
            coords[1].as_f64().unwrap(),
            coords[2].as_f64().unwrap(),
        );
        assert!(
            journal_pos.distance(expected_site) <= 1.0,
            "journal dungeon coords {journal_pos:?} must match site_origin_m {expected_site:?}"
        );
    }

    // `agentMgr.WarpToLocation("dungeon")` must resolve and materialize at `site_1` over diverging `bm_1`.
    f.ctx
        .dispatch_rpc(
            &mut f.sess_a,
            "agentMgr",
            None,
            "WarpToLocation",
            &[PyRep::String("dungeon".into()), PyRep::Integer(i64::from(mid_1))],
            &kw,
            None,
        )
        .await
        .expect("WarpToLocation dungeon mid_1");
    let (_, b1) = h.get_ball_snapshot(f.ship_a).await.unwrap().unwrap();
    match b1.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(
                destination.distance(site_1) <= 1.0,
                "WarpToLocation dest {destination:?} != {site_1:?}"
            );
        }
        other => panic!("expected Warp mode after WarpToLocation, got {other:?}"),
    }
    let bids_1 = f
        .ctx
        .db
        .with_snapshot(|s| mission_fixture::get(s, f.pilot_a, mid_1).unwrap().spawned_npc_ball_ids);
    assert_eq!(bids_1.len(), 2);
    assert!(h.get_ball_snapshot(bids_1[0]).await.unwrap().unwrap().1.position.distance(site_1) <= 5_000.0);

    // `beyonce.CmdWarpToStuff("mission")` must resolve and materialize at `site_2` over diverging `bm_2`.
    let _ = h.set_movement_mode(f.ship_a, eve_destiny::MovementMode::Stop).await;
    let _ = h.set_ball_position(f.ship_a, glam::DVec3::ZERO).await;
    h.try_update_combat_state(f.ship_a, |cs| {
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
    });
    let _ = f
        .ctx
        .dispatch_rpc(
            &mut f.sess_a,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("mission".into()), PyRep::Integer(i64::from(mid_2))],
            &kw,
            None,
        )
        .await
        .expect("CmdWarpToStuff mission mid_2");
    let (_, b2) = h.get_ball_snapshot(f.ship_a).await.unwrap().unwrap();
    match b2.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            assert!(
                destination.distance(site_2) <= 1.0,
                "CmdWarpToStuff dest {destination:?} != {site_2:?}"
            );
        }
        other => panic!("expected Warp mode after CmdWarpToStuff, got {other:?}"),
    }
    let bids_2 = f
        .ctx
        .db
        .with_snapshot(|s| mission_fixture::get(s, f.pilot_a, mid_2).unwrap().spawned_npc_ball_ids);
    assert_eq!(bids_2.len(), 2);
    assert!(h.get_ball_snapshot(bids_2[0]).await.unwrap().unwrap().1.position.distance(site_2) <= 5_000.0);
}

#[tokio::test]
async fn cold_system_ensure_mission_asteroid_spawned_on_pocket_entry_stays_cold() {
    let mut sde = (*shared_sde()).clone();
    let mining_ag = sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.agent_type_id == 2
                && a.division_id == 23
                && a.solar_system_id.is_some()
                && a.station_id.is_some()
        })
        .min_by_key(|a| a.agent_id)
        .cloned()
        .unwrap_or_else(|| {
            let mut ag = sde
                .agents
                .values()
                .find(|a| a.station_id.is_some() && a.solar_system_id.is_some())
                .expect("stationed agent")
                .clone();
            ag.agent_id = 3_990_201;
            ag.level = 1;
            ag.agent_type_id = 2;
            ag.division_id = 23;
            sde.agents.insert(ag.agent_id, ag.clone());
            ag
        });
    let mining_sys = mining_ag.solar_system_id.unwrap();
    let mining_st = mining_ag.station_id.unwrap();
    let mining_mid = sde
        .missions
        .values()
        .filter(|m| {
            eve_db::decide_mission_kind(Some(m), Some(&mining_ag)) == eve_db::MissionKind::Mining
        })
        .map(|m| m.mission_id)
        .min()
        .unwrap_or_else(|| {
            let tmpl = eve_sde::MissionTemplate {
                mission_id: 98_201,
                title: "Mining Cold Test Mission".into(),
                is_courier: false,
                lp_alpha: 10,
                lp_omega: 20,
                has_standing_rewards: true,
                content_template: Some("BasicMining".into()),
                dungeon_id: None,
            };
            sde.missions.insert(tmpl.mission_id, tmpl);
            98_201
        });

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let char_a = 90_000_001_i64;
    let now = ctx.now_unix();
    move_demo_character(&ctx, char_a, mining_sys, Some(mining_st));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_a) {
            ch.standings.insert(mining_ag.corporation_id, 2.0);
        }
        mission_fixture::insert_with_sde(
            snap,
            &ctx.sde,
            MissionFixture::offered(char_a, mining_mid, mining_ag.agent_id, mining_sys)
                .origin_station_id(Some(mining_st))
                .offered_at_unix(now)
                .offer_expiry_unix(now + 86_400)
                .required_delivery_type_id(Some(1230))
                .required_delivery_qty(500),
        );
    });

    let kw = HashMap::new();
    let mut sess_a = docked_session(char_a, mining_sys, Some(mining_st));
    let acc_a = handle(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(mining_ag.agent_id),
            PyRep::Integer(i64::from(mining_mid)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("char_a accept mining");
    let ast_a = acc_a
        .dict_get("asteroidEntityID")
        .and_then(PyRep::as_i64)
        .expect("ast_a");

    assert!(ctx.systems.get_handle(mining_sys).is_none());
    ensure_mission_asteroid_spawned_on_pocket_entry(&ctx, char_a, mining_sys, Some(mining_mid))
        .await;
    assert!(
        ctx.systems.get_handle(mining_sys).is_none(),
        "ensure_mission_asteroid_spawned_on_pocket_entry must not wake cold system"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(snap.asteroids.contains_key(&ast_a));
        assert_eq!(
            snap.missions
                .get_by_character_template(char_a, mining_mid)
                .and_then(|m| m.asteroid_entity_id),
            Some(ast_a)
        );
    });
}

