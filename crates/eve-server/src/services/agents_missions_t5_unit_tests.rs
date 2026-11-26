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

    ensure_mission_asteroid_spawned_on_pocket_entry(&ctx, char_a, mining_sys, Some(mining_mid))
        .await;
    let sys_h = ctx
        .systems
        .get_handle(mining_sys)
        .expect("mining sys actor spawned on pocket entry");
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
    let info_text = brief_info
        .dict_get("objectives")
        .and_then(PyRep::as_str)
        .expect("objectives");
    assert!(info_text.contains("77") && info_text.contains("Tritanium"), "got {info_text}");
    assert_eq!(brief_info.dict_get("requiredTypeID"), Some(&PyRep::Integer(34)));
    assert_eq!(brief_info.dict_get("requiredQuantity"), Some(&PyRep::Integer(77)));
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
async fn bf1_cmd_warp_to_stuff_range_truncation_legacy_none_and_post_eviction_respawn() {
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
    agents_missions_warp::materialize_mission_pocket_on_cmd_warp(
        &ctx,
        &handle,
        char_id,
        sys_id,
        "bookmark",
        bm_id,
        pocket_pos + glam::DVec3::new(300_000.0, 0.0, 0.0),
    )
    .await;
    ctx.db.with_snapshot(|snap| {
        assert!(
            mission_fixture::get(snap, char_id, mid_scoped)
                .unwrap()
                .spawned_npc_ball_ids
                .is_empty(),
            "bubble-dragged bookmark warp (>200km from pocket) must not materialize NPCs"
        );
    });

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
    assert_ne!(respawned_bids, first_bids);
    for &bid in &respawned_bids {
        assert!(handle.get_ball_snapshot(bid).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn bf2_pocket_spawned_mission_npcs_preserve_sde_ewar_reps_and_uses_synthetic_false() {
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
async fn bf4_clamped_penalty_and_journal_expiry_penalties() {
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
async fn own1_solo_agent_warp_dungeon_and_bookmark_respect_200km_landing_radius() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();
    let pocket_pos = glam::DVec3::new(5.0e12, 0.0, 0.0);
    let goto_pocket_pos = glam::DVec3::new(50_000.0, 0.0, 0.0);
    let bm_id = 995_401_i64;
    let ast_id = 995_402_i64;
    let bubble_id = 995_403_i64;
    let mid = 98_401_u32;
    let bm_goto_id = 995_404_i64;
    let ast_goto_id = 995_405_i64;
    let mid_goto = 98_402_u32;
    let agent_id = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(sys_id))
        .map(|a| a.agent_id)
        .unwrap_or(3_018_601);

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
        for (bid, aid, m_id, origin) in [
            (bm_id, ast_id, mid, pocket_pos),
            (bm_goto_id, ast_goto_id, mid_goto, goto_pocket_pos),
        ] {
            snap.bookmarks.insert(
                bid,
                eve_db::BookmarkRecord {
                    bookmark_id: bid,
                    owner_id: char_id,
                    solar_system_id: sys_id,
                    label: format!("OWN1 Pocket {m_id}"),
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
                MissionFixture::accepted(char_id, m_id, agent_id, sys_id)
                    .bookmark_id(Some(bid))
                    .site_origin_m(Some([origin.x as i64, 0, 0]))
                    .asteroid_entity_id(Some(aid))
                    .npcs_remaining(2)
                    .initial_wave_count(Some(2))
                    .initial_wave_types(Some(vec![34_155])),
            );
            let _ = snap.missions.update_by_character_template(char_id, m_id, |m| {
                m.kind = eve_db::MissionKind::Mining;
            });
        }
    });

    let mut sess = docked_session(char_id, sys_id, None);
    sess.ship_id = Some(ship_id);

    // 1. Low-cap truncated solo agentMgr.WarpToLocation("dungeon", ...) must NOT spawn NPCs/asteroids.
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
            PyRep::Integer(i64::from(mid)),
            PyRep::Real(0.0),
            PyRep::Bool(false),
            PyRep::Integer(agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("low-cap dungeon warp initiates");
    assert_eq!(res, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert!(mission_fixture::get(snap, char_id, mid).unwrap().spawned_npc_ball_ids.is_empty());
    });
    assert!(handle_sys.get_ball_snapshot(ast_id).await.unwrap().is_none());

    // 2. Destination warp bubble > 200 km short must prevent BOTH "dungeon" and "bookmark" spawns.
    let _ = handle_sys.set_movement_mode(ship_id, eve_destiny::MovementMode::Stop).await;
    let _ = handle_sys.set_ball_position(ship_id, glam::DVec3::ZERO).await;
    handle_sys.try_update_combat_state(ship_id, |cs| {
        cs.capacitor = 250.0;
        cs.max_capacitor = 250.0;
    });
    handle_sys
        .add_warp_bubble(bubble_id, pocket_pos - glam::DVec3::new(500_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    for (loc_type, loc_num) in [("dungeon", i64::from(mid)), ("bookmark", bm_id)] {
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
                PyRep::Integer(agent_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("bubbled solo warp initiates");
        assert_eq!(r, PyRep::Bool(true));
        ctx.db.with_snapshot(|snap| {
            assert!(
                mission_fixture::get(snap, char_id, mid).unwrap().spawned_npc_ball_ids.is_empty(),
                "bubble-dragged {loc_type} warp must not spawn NPCs"
            );
        });
        assert!(
            handle_sys.get_ball_snapshot(ast_id).await.unwrap().is_none(),
            "bubble-dragged {loc_type} warp must not spawn asteroid"
        );
    }

    // 3. Remove bubble -> solo agentMgr.WarpToLocation("bookmark", bm_id) materializes NPCs + asteroid.
    assert!(handle_sys.remove_warp_bubble(bubble_id).await.unwrap());
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
            PyRep::Integer(bm_id),
            PyRep::Real(0.0),
            PyRep::Bool(false),
            PyRep::Integer(agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("bookmark warp reaches pocket");
    assert_eq!(bm_ok, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(mission_fixture::get(snap, char_id, mid).unwrap().spawned_npc_ball_ids.len(), 2);
    });
    assert!(handle_sys.get_ball_snapshot(ast_id).await.unwrap().is_some());

    // 4. Subwarp agentMgr.GotoLocation("bookmark", bm_goto_id) within 200 km materializes pocket.
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
            PyRep::Integer(agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("subwarp GotoLocation succeeds");
    assert_eq!(goto_ok, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            mission_fixture::get(snap, char_id, mid_goto).unwrap().spawned_npc_ball_ids.len(),
            2
        );
    });
    assert!(handle_sys.get_ball_snapshot(ast_goto_id).await.unwrap().is_some());
}

#[tokio::test]
async fn own1_fleet_agent_warp_dungeon_and_bookmark_respect_200km_landing_radius() {
    let ctx = test_ctx();
    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.solar_system_id == Some(30_000_142))
        .expect("Jita agent exists")
        .clone();
    let sys_id = agent.solar_system_id.unwrap();
    let char_fc = 959_601_i64;
    let char_wing = 959_602_i64;
    let ship_fc = 959_611_i64;
    let ship_wing = 959_612_i64;
    let fleet_id = 959_620_i64;
    let bm_id = 959_630_i64;
    let ast_id = 959_631_i64;
    let mid = 98_403_u32;
    let bm2_id = 959_632_i64;
    let ast2_id = 959_633_i64;
    let mid2 = 98_404_u32;
    let bubble_id = 959_640_i64;
    let pocket_pos = glam::DVec3::new(5.0e12, 0.0, 0.0);
    let pocket2_pos = glam::DVec3::new(6.0e12, 0.0, 0.0);
    let start_pos = glam::DVec3::ZERO;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let template_acc = snap.accounts.values().next().unwrap().clone();
        let template_ch = snap.characters.values().next().unwrap().clone();
        for (idx, (cid, sid)) in [(char_fc, ship_fc), (char_wing, ship_wing)].into_iter().enumerate() {
            let uid = 959_650 + idx as u64;
            let mut acc = template_acc.clone();
            acc.user_id = uid;
            acc.username = format!("own1_fleet_{idx}");
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
        for (bid, aid, m_id, origin) in [
            (bm_id, ast_id, mid, pocket_pos),
            (bm2_id, ast2_id, mid2, pocket2_pos),
        ] {
            snap.bookmarks.insert(
                bid,
                eve_db::BookmarkRecord {
                    bookmark_id: bid,
                    owner_id: char_fc,
                    solar_system_id: sys_id,
                    label: format!("OWN1 Fleet Pocket {m_id}"),
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
                MissionFixture::accepted(char_fc, m_id, agent.agent_id, sys_id)
                    .bookmark_id(Some(bid))
                    .site_origin_m(Some([origin.x as i64, 0, 0]))
                    .asteroid_entity_id(Some(aid))
                    .npcs_remaining(2)
                    .initial_wave_count(Some(2))
                    .initial_wave_types(Some(vec![34_155])),
            );
            let _ = snap.missions.update_by_character_template(char_fc, m_id, |m| {
                m.kind = eve_db::MissionKind::Mining;
            });
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
        .add_warp_bubble(bubble_id, pocket_pos - glam::DVec3::new(500_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();
    for (loc_type, loc_num) in [("dungeon", i64::from(mid)), ("bookmark", bm_id)] {
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
                PyRep::Integer(agent.agent_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("bubbled fleet warp succeeds");
        assert_eq!(r, PyRep::Bool(true));
        ctx.db.with_snapshot(|snap| {
            assert!(
                mission_fixture::get(snap, char_fc, mid).unwrap().spawned_npc_ball_ids.is_empty(),
                "bubbled fleet {loc_type} warp must not spawn NPCs"
            );
        });
        assert!(sys_handle.get_ball_snapshot(ast_id).await.unwrap().is_none());
    }

    // 2. Remove bubble; cap-truncate all fleetmates -> fleet "dungeon" still skips spawning.
    assert!(sys_handle.remove_warp_bubble(bubble_id).await.unwrap());
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
            PyRep::Integer(i64::from(mid)),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Integer(agent.agent_id),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("low-cap fleet warp initiates");
    assert_eq!(truncated_ok, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert!(mission_fixture::get(snap, char_fc, mid).unwrap().spawned_npc_ball_ids.is_empty());
    });
    assert!(sys_handle.get_ball_snapshot(ast_id).await.unwrap().is_none());

    // 3. One fleetmate (wingmate) has full cap and lands on grid while FC stops short -> BOTH "dungeon" and "bookmark" materialize.
    for (loc_type, loc_num, m_check, aid_check) in [
        ("dungeon", i64::from(mid), mid, ast_id),
        ("bookmark", bm2_id, mid2, ast2_id),
    ] {
        for (sid, offset, cap) in [
            (ship_fc, glam::DVec3::ZERO, 10.0),
            (ship_wing, glam::DVec3::new(1_000.0, 0.0, 0.0), 250.0),
        ] {
            let _ = sys_handle.set_movement_mode(sid, eve_destiny::MovementMode::Stop).await;
            let _ = sys_handle.set_ball_position(sid, start_pos + offset).await;
            sys_handle.try_update_combat_state(sid, |cs| {
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
                PyRep::Integer(agent.agent_id),
            ],
            &kw,
        )
        .await
        .unwrap()
        .expect("partial fleet arrival succeeds");
        assert_eq!(ok, PyRep::Bool(true));
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                mission_fixture::get(snap, char_fc, m_check).unwrap().spawned_npc_ball_ids.len(),
                2,
                "fleet {loc_type} warp with wingmate landing must spawn NPCs"
            );
        });
        assert!(
            sys_handle.get_ball_snapshot(aid_check).await.unwrap().is_some(),
            "fleet {loc_type} warp with wingmate landing must spawn asteroid"
        );
    }
}
