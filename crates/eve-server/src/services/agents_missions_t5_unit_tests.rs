//! Track 5 (`#1496`) regression tests: `PVE-001`, `PVE-022`, `PVE-020`, `PVE-024`, `PVE-012`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::mission_fixture::{self, MissionFixture};
use eve_db::WorldRepository;
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

    let mut sess_fail = docked_session(char_fail, sys_id, Some(st_id));
    let mut sess_quit = docked_session(char_quit, sys_id, Some(st_id));
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
