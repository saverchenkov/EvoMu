//! Deep branch-coverage and mutation-killing unit tests for `config_service`
//! (`objectCaching`, `config`, `notificationMgr`), `agents_standings_arcs` (standing skill math,
//! derived faction standings, note persistence, epic arcs, `standingMgr`, `agentMgr`, `keeper`),
//! and `fleet_social_bookmarks` (personal/shared ACL folders, subfolders, known folders, copy/move).

#![forbid(unsafe_code)]

use crate::router::{NotificationRecord, ServerContext};
use crate::services::agents_missions::agents_standings_arcs::{
    apply_faction_standing_with_derived, apply_mission_completion_standings,
    compute_effective_agent_standing, compute_effective_standing_breakdown,
    get_sde_npc_npc_standings, handle_extended, handle_special_dialogue_action,
    is_pirate_entity, is_trig_or_edencom_entity, load_epic_arc_states, load_mission_meta,
    load_rnd_research_entries, notify_standing_changes, resolve_epic_arc_id_for_agent,
    save_epic_arc_states, save_mission_meta, save_rnd_research_entries,
    static_npc_faction_standings, EpicArcStateEntry, MissionTimingAndCargoMeta, RndResearchEntry,
};
use crate::services::config_service::{
    get_celestial_statistic, get_map_connections, get_map_objects, get_multi_graphics_ex,
    get_multi_inv_types_ex, get_multi_station_ex, handle as handle_config,
};
use crate::services::fleet_social::handle as handle_social;
use eve_db::{corp_roles, ActiveMissionRecord, CosmicSignatureRecord, TrainedSkillRecord, VirtualServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join("data/evomu_sde.sqlite");
            if candidate.exists() {
                return Arc::new(SdeRegistry::open(&candidate).expect("open evomu_sde.sqlite"));
            }
            if !dir.pop() {
                break;
            }
        }
        Arc::new(SdeRegistry::with_canonical_seed())
    }))
}

fn test_ctx_at(now_unix: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

fn make_sess(char_id: i64, corp_id: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(7100 + (char_id as u64 % 400));
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30_000_142);
    s.station_id = Some(60_003_760);
    s.corp_role = role;
    s
}

fn cfg_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_config(ctx, sess, svc, method, args, kwargs).expect("config handled")
}

async fn social_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle_social(ctx, sess, svc, method, args, &HashMap::new())
        .await
        .expect("social handled")
}

#[test]
fn config_service_object_caching_sde_tables_map_and_notifications() {
    let now = 1_700_000_000u64;
    let ctx = test_ctx_at(now);
    let mut unauth = SessionState::new(1);
    let mut sess = make_sess(90_000_001, 98_000_001, corp_roles::ROLE_DIRECTOR);
    let empty_kw = HashMap::new();

    // Unauthenticated rejected; unpopulated key returns CachedObjectNotFound; populated & bulk keys succeed
    assert_eq!(cfg_call(&ctx, &mut unauth, "objectCaching", "GetCachableObject", &[PyRep::Integer(1)], &empty_kw).unwrap_err().code, "Unauthorized");
    assert_eq!(cfg_call(&ctx, &mut sess, "objectCaching", "GetCachableObject", &[PyRep::Integer(999_888)], &empty_kw).unwrap_err().code, "CachedObjectNotFound");
    ctx.put_cached_object(PyRep::String("map_regions".into()), PyRep::Integer(42), true);
    for args in [
        vec![PyRep::Bool(true), PyRep::String("map_regions".into())],
        vec![PyRep::Integer(1), PyRep::String("config.BulkData.units".into())],
        vec![PyRep::String("config.Units".into())],
    ] {
        assert!(cfg_call(&ctx, &mut sess, "objectCaching", "GetCachableObject", &args, &empty_kw).is_ok());
    }
    assert!(cfg_call(&ctx, &mut sess, "objectCaching", "GetCachedObject", &[PyRep::Bool(false), PyRep::String("config.MarketGroups".into())], &empty_kw).is_ok());
    assert_eq!(cfg_call(&ctx, &mut sess, "objectCaching", "GetCachableObject", &[], &empty_kw).unwrap_err().code, "InvalidArgument");
    sess.role = u64::MAX;
    assert!(cfg_call(&ctx, &mut sess, "objectCaching", "InvalidateCachedObjects", &[PyRep::List(vec![PyRep::String("map_regions".into())])], &empty_kw).is_ok());

    // parse_id_list branches via GetMultiOwnersEx (List, Tuple, single Integer, kwargs, non-int element error)
    let id_list = PyRep::List(vec![
        PyRep::Integer(90_000_001), PyRep::Integer(98_000_001), PyRep::Integer(60_003_760),
        PyRep::Integer(30_000_142), PyRep::Integer(10_000_002),
    ]);
    assert!(cfg_call(&ctx, &mut sess, "config", "GetMultiOwnersEx", &[id_list], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "config", "GetMultiOwnersEx", &[PyRep::Tuple(vec![PyRep::Integer(90_000_001), PyRep::Integer(999_999_888)])], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "config", "GetMultiOwnersEx", &[PyRep::Integer(90_000_001)], &empty_kw).is_ok());
    let mut kw_ids = HashMap::new();
    kw_ids.insert("ownerIDs".to_string(), PyRep::List(vec![PyRep::Integer(90_000_001)]));
    assert!(cfg_call(&ctx, &mut sess, "config", "GetMultiOwnersEx", &[], &kw_ids).is_ok());
    assert_eq!(cfg_call(&ctx, &mut sess, "config", "GetMultiOwnersEx", &[PyRep::List(vec![PyRep::String("bad".into())])], &empty_kw).unwrap_err().code, "InvalidArgument");

    // GetMultiLocationsEx / GetMultiStationEx / GetMultiInvTypesEx / GetMultiGraphicsEx / GetCelestialStatistic
    assert!(cfg_call(&ctx, &mut sess, "config", "GetMultiLocationsEx", &[PyRep::List(vec![PyRep::Integer(60_003_760), PyRep::Integer(30_000_142), PyRep::Integer(40_009_072)])], &empty_kw).is_ok());
    assert!(get_multi_station_ex(&ctx, &sess, &[PyRep::List(vec![PyRep::Integer(60_003_760)])], &empty_kw).is_ok());
    assert!(get_multi_inv_types_ex(&ctx, &sess, &[PyRep::List(vec![PyRep::Integer(587), PyRep::Integer(34)])], &empty_kw).is_ok());
    assert_eq!(get_multi_graphics_ex(&ctx, &sess).unwrap_err().code, "UnimplementedRPC");
    assert_eq!(get_celestial_statistic(&ctx, &sess).unwrap_err().code, "UnimplementedRPC");

    // GetMultiAllianceShortNamesEx, GetMultiCorpTickerNamesEx, GetStationSolarSystemsByOwner, GetMap, GetMapObjects, GetMapConnections
    assert!(cfg_call(&ctx, &mut sess, "config", "GetMultiAllianceShortNamesEx", &[PyRep::List(vec![PyRep::Integer(99_000_001)])], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "config", "GetMultiCorpTickerNamesEx", &[PyRep::List(vec![PyRep::Integer(98_000_001), PyRep::Integer(1_000_035)])], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "config", "GetStationSolarSystemsByOwner", &[PyRep::Integer(1_000_035)], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "config", "GetMap", &[PyRep::Integer(30_000_142)], &empty_kw).is_ok());
    for id in [30_000_142i64, 20_000_020, 10_000_002] {
        assert!(get_map_objects(&ctx, &sess, &[PyRep::Integer(id)]).is_ok());
        assert!(get_map_connections(&ctx, &sess, &[PyRep::Integer(id)]).is_ok());
    }

    // notificationMgr lifecycle: GetAllNotifications, GetByGroupID, GetUnprocessed, Mark*, Log, Delete*
    {
        let mut inbox = ctx.notification_records.write().unwrap();
        for (nid, gid, processed) in [(9001, 4, false), (9002, 5, false), (9003, 5, true)] {
            inbox.push(NotificationRecord {
                notification_id: nid, type_id: 16, group_id: gid, sender_id: 1_000_035, receiver_id: 90_000_001,
                processed, created: now as i64, data: format!("Note {nid}"), deleted: false, interacted: false,
            });
        }
    }
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "GetAllNotifications", &[PyRep::Integer(0)], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "GetByGroupID", &[PyRep::Integer(4)], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "GetUnprocessed", &[], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "MarkAsProcessed", &[PyRep::List(vec![PyRep::Integer(9001)])], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "MarkGroupAsProcessed", &[PyRep::Integer(5)], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "MarkAllAsProcessed", &[], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "LogNotificationInteraction", &[PyRep::Integer(9001), PyRep::Integer(1)], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "DeleteNotifications", &[PyRep::List(vec![PyRep::Integer(9001)])], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "DeleteGroupNotifications", &[PyRep::Integer(5)], &empty_kw).is_ok());
    assert!(cfg_call(&ctx, &mut sess, "notificationMgr", "DeleteAllNotifications", &[], &empty_kw).is_ok());
}

#[test]
fn standings_math_derived_arcs_and_extended_agent_standing_rpcs() {
    let now = 1_700_000_000u64;
    let ctx = test_ctx_at(now);
    let cid = 90_000_001i64;
    let mut sess = make_sess(cid, 98_000_001, corp_roles::ROLE_DIRECTOR);

    // Seed social skills: Connections(3359)=4, Diplomacy(3357)=5, Criminal Connections(3361)=3, Social(3355)=5
    let m_chg = ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(cid).or_default();
        for (tid, lvl) in [(3359u32, 4u8), (3357, 5), (3361, 3), (3355, 5)] {
            sk.insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 100_000.0 });
        }
        // Triglavian (500_026) vs EDENCOM (500_027) vs Angel Cartel (500_011) vs Caldari State (500_001)
        assert!(is_trig_or_edencom_entity(&ctx, 500_026));
        assert!(is_trig_or_edencom_entity(&ctx, 500_027));
        assert!(!is_trig_or_edencom_entity(&ctx, 500_001));
        assert!(is_pirate_entity(&ctx, 500_011));
        assert!(!is_pirate_entity(&ctx, 500_001));

        // 1. Triglavian/EDENCOM bypasses social skills and clamps [-10, 10]
        assert_eq!(
            compute_effective_standing_breakdown(&ctx, snap, cid, 500_026, -12.5, true),
            (-10.0, 0.0, 0.0, 0.0)
        );
        // 2. Negative standing uses Diplomacy: (10 - (-4.0)) * 0.04 * 5 = 2.8 -> -1.2
        let (eff_neg, conn_neg, dip_neg, crim_neg) =
            compute_effective_standing_breakdown(&ctx, snap, cid, 500_002, -4.0, true);
        assert!((dip_neg - 2.8).abs() < 1e-9);
        assert!((eff_neg - (-1.2)).abs() < 1e-9);
        assert_eq!((conn_neg, crim_neg), (0.0, 0.0));

        // 3. Pirate positive standing uses Criminal Connections: (10 - 2.0) * 0.04 * 3 = 0.96
        let (eff_pir, conn_pir, dip_pir, crim_pir) =
            compute_effective_standing_breakdown(&ctx, snap, cid, 500_011, 2.0, true);
        assert!((crim_pir - 0.96).abs() < 1e-9);
        assert!((eff_pir - 2.96).abs() < 1e-9);
        assert_eq!((conn_pir, dip_pir), (0.0, 0.0));

        // 4. Empire positive/zero-with-entry uses Connections: (10 - 0.0) * 0.04 * 4 = 1.60
        let (eff_emp, conn_emp, _, _) =
            compute_effective_standing_breakdown(&ctx, snap, cid, 500_001, 0.0, true);
        assert!((conn_emp - 1.6).abs() < 1e-9);
        assert!((eff_emp - 1.6).abs() < 1e-9);

        // 5. Missing entry & zero standing -> (0, 0, 0, 0)
        assert_eq!(
            compute_effective_standing_breakdown(&ctx, snap, cid, 500_001, 0.0, false),
            (0.0, 0.0, 0.0, 0.0)
        );

        // Agent standing hostile floor rule: if any <= -2.0 returns min, else max
        {
            let ch = snap.characters.get_mut(&cid).unwrap();
            ch.standings.insert(3_008_416, 4.0);
            ch.standings.insert(1_000_035, 1.0);
            ch.standings.insert(500_001, -7.5);
        }
        assert!(compute_effective_agent_standing(&ctx, snap, cid, 3_008_416, 1_000_035, Some(500_001)) < -2.0);
        snap.characters.get_mut(&cid).unwrap().standings.insert(500_001, 0.5);
        assert!(compute_effective_agent_standing(&ctx, snap, cid, 3_008_416, 1_000_035, Some(500_001)) >= 4.0);

        // Derived faction standing & mission completion standing transactions
        let mut chg = Vec::new();
        apply_faction_standing_with_derived(&ctx, snap, cid, 500_001, 10.0, true, &mut chg);
        assert!(chg.len() > 1);
        let (_, m_chg) = apply_mission_completion_standings(&ctx, snap, cid, 3_008_416, 101, 1_000_035, 5.0, true);
        assert!(!m_chg.is_empty());

        // Mission meta, R&D research entries, and Epic Arc note serialization helpers
        save_mission_meta(
            snap,
            cid,
            &MissionTimingAndCargoMeta {
                mission_id: 101,
                agent_id: 3_008_416,
                offered_at_unix: now,
                offer_expiry_unix: now + 7200,
                ..Default::default()
            },
        );
        assert_eq!(load_mission_meta(snap, cid, 101).unwrap().offer_expiry_unix, now + 7200);
        save_rnd_research_entries(
            snap,
            cid,
            &[RndResearchEntry {
                agent_id: 3_011_111,
                research_type_id: 11_441,
                points_per_day: 25.0,
                initial_points: 10.0,
                started_unix: now,
            }],
        );
        assert_eq!(load_rnd_research_entries(snap, cid).len(), 1);
        save_epic_arc_states(
            snap,
            cid,
            &[EpicArcStateEntry {
                epic_arc_id: 1,
                agent_id: 3_018_817,
                faction_id: 500_021,
                chapter: 2,
                chosen_branch: Some("PathA".into()),
                completed_missions: vec![1, 2],
                completed_at_unix: None,
                cooldown_until_unix: None,
            }],
        );
        assert_eq!(load_epic_arc_states(snap, cid).len(), 1);
        snap.active_missions.insert(
            (cid, 101),
            ActiveMissionRecord {
                mission_id: 101,
                character_id: cid,
                agent_id: 3_008_416,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        m_chg
    });
    notify_standing_changes(&ctx, cid, &m_chg);

    assert!(!static_npc_faction_standings().is_empty());
    assert!(!get_sde_npc_npc_standings(&ctx.sde).is_empty());
    let _ = resolve_epic_arc_id_for_agent(&ctx, 3_018_817);
    assert_eq!(
        handle_special_dialogue_action(&ctx, &mut sess, cid, 30_000_142, 12, 999_999_111, &[], &HashMap::new())
            .unwrap()
            .unwrap_err()
            .code,
        "UnknownAgent"
    );

    // Async handle_extended RPCs: standingMgr, agentMgr, epicArcStatus, missionTrackerMgr, air_npe, keeper
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for (svc, m, args) in [
                ("standingMgr", "GetCorpStandings", vec![]),
                ("standingMgr", "GetNPCNPCStandings", vec![]),
                ("standingMgr", "GetStandingCompositions", vec![]),
                ("agentMgr", "GetMyEpicArcStatus", vec![]),
                ("agentMgr", "GetMessagesForEpicArcMissions", vec![]),
                ("agentMgr", "GetDisabledMissions", vec![]),
                ("agentMgr", "GetPlausibleMissionIDs", vec![PyRep::Integer(3_008_416)]),
                ("missionTrackerMgr", "GetAllMissionObjectives", vec![]),
                ("air_npe", "get_air_npe_state", vec![]),
                ("air_npe", "skip_air_npe", vec![]),
                ("epicArcStatus", "AgentHasEpicMissionsForCharacter", vec![PyRep::Integer(3_008_416)]),
            ] {
                assert!(handle_extended(&ctx, &mut sess, svc, m, &args, &HashMap::new()).await.unwrap().is_ok());
            }
            // Docked keeper gate activation rejected with ShipDocked
            assert_eq!(
                handle_extended(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(101)], &HashMap::new())
                    .await
                    .unwrap()
                    .unwrap_err()
                    .code,
                "ShipDocked"
            );
        });
}

#[test]
fn bookmarks_acl_folders_subfolders_known_folders_and_batch_copy_move() {
    let now = 1_700_000_000u64;
    let ctx = test_ctx_at(now);
    let ceo_id = 90_000_001i64;
    let member_id = 90_000_002i64;
    let corp_id = 98_000_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut m2 = snap.characters[&ceo_id].clone();
        m2.character_id = member_id;
        m2.user_id = member_id as u64;
        m2.corporation_id = corp_id;
        snap.characters.insert(member_id, m2);
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = ceo_id;
            corp.members.insert(ceo_id);
            corp.members.insert(member_id);
        }
    });

    let mut ceo_sess = make_sess(ceo_id, corp_id, corp_roles::ROLE_DIRECTOR);
    let mut mem_sess = make_sess(member_id, corp_id, 0);

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // Non-director cannot create shared corp folder
            assert_eq!(
                social_call(
                    &ctx,
                    &mut mem_sess,
                    "bookmark",
                    "AddFolder",
                    &[PyRep::Bool(false), PyRep::String("Forbidden".into())],
                )
                .await
                .unwrap_err()
                .code,
                "BookmarkAccessDenied"
            );

            // Director creates shared corp folder + personal folder + subfolder
            let shared_f = social_call(
                &ctx,
                &mut ceo_sess,
                "bookmark",
                "AddFolder",
                &[
                    PyRep::Bool(false),
                    PyRep::String("Corp Safes".into()),
                    PyRep::List(vec![PyRep::Integer(42)]),
                ],
            )
            .await
            .unwrap();
            let shared_fid = shared_f.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();

            let pers_f = social_call(
                &ctx,
                &mut ceo_sess,
                "bookmark",
                "AddFolder",
                &[PyRep::Bool(true), PyRep::String("Personal Spots".into())],
            )
            .await
            .unwrap();
            let pers_fid = pers_f.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();

            let sub_f = social_call(
                &ctx,
                &mut ceo_sess,
                "bookmark",
                "CreateSubfolder",
                &[PyRep::Integer(shared_fid), PyRep::String("Staging".into())],
            )
            .await
            .unwrap();
            let sub_fid = sub_f
                .dict_get("subfolderID")
                .or_else(|| sub_f.dict_get("folderID"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            assert!(social_call(
                &ctx,
                &mut ceo_sess,
                "bookmark",
                "UpdateFolder",
                &[
                    PyRep::Integer(shared_fid),
                    PyRep::String("Corp Safes v2".into()),
                    PyRep::List(vec![PyRep::Integer(42), PyRep::Integer(43)]),
                ],
            )
            .await
            .is_ok());

            // Known folders add/remove
            assert!(social_call(&ctx, &mut mem_sess, "bookmark", "AddToKnownFolders", &[PyRep::Integer(shared_fid)]).await.is_ok());
            assert!(social_call(&ctx, &mut ceo_sess, "bookmark", "GetFolders", &[]).await.is_ok());
            assert!(social_call(&ctx, &mut mem_sess, "bookmark", "RemoveFromKnownFolders", &[PyRep::Integer(shared_fid)]).await.is_ok());

            // BookmarkScanResult requires in-space session (NotInSpace when docked), then succeeds in space for resolved signature
            ctx.db.with_mut_snapshot(|s| {
                s.cosmic_signatures.entry(30_000_142).or_default().push(CosmicSignatureRecord {
                    signature_id: "ABC-123".into(), solar_system_id: 30_000_142, sig_group: "Relic".into(),
                    type_name: "Ruined Sansha Temple".into(), position: [1e9, 2e9, 3e9], signal_strength: 1.0,
                    wormhole_type_id: None, destination_system_id: None, created_unix: now, lifetime_seconds: 86_400,
                    max_mass_kg: 0.0, remaining_mass_kg: 0.0, max_jump_mass_kg: 0.0, collapsed: false,
                });
            });
            let scan_args = [
                PyRep::String("ABC-123".into()),
                PyRep::String("Relic ABC".into()),
                PyRep::Integer(shared_fid),
            ];
            assert_eq!(
                social_call(&ctx, &mut ceo_sess, "beyonce", "BookmarkScanResult", &scan_args)
                    .await
                    .unwrap_err()
                    .code,
                "NotInSpace"
            );
            ceo_sess.station_id = None;
            let PyRep::Tuple(scan_tup) = social_call(&ctx, &mut ceo_sess, "beyonce", "BookmarkScanResult", &scan_args)
                .await
                .unwrap()
            else {
                panic!("expected BookmarkScanResult tuple");
            };
            let bm_id = scan_tup[0].as_i64().unwrap();

            assert!(social_call(
                &ctx,
                &mut ceo_sess,
                "bookmark",
                "UpdateBookmark",
                &[
                    PyRep::Integer(bm_id),
                    PyRep::String("Relic Updated".into()),
                    PyRep::String("Cleared".into()),
                ],
            )
            .await
            .is_ok());
            assert!(social_call(&ctx, &mut ceo_sess, "bookmark", "GetBookmarks", &[]).await.is_ok());

            let too_many: Vec<PyRep> = (1..=21).map(PyRep::Integer).collect();
            assert_eq!(
                social_call(
                    &ctx,
                    &mut ceo_sess,
                    "bookmark",
                    "CopyBookmarksToFolderAndSubfolder",
                    &[PyRep::List(too_many), PyRep::Integer(pers_fid)],
                )
                .await
                .unwrap_err()
                .code,
                "BookmarkCopyBatchLimitExceeded"
            );
            assert!(social_call(
                &ctx,
                &mut ceo_sess,
                "bookmark",
                "CopyBookmarksToFolderAndSubfolder",
                &[PyRep::List(vec![PyRep::Integer(bm_id)]), PyRep::Integer(pers_fid)],
            )
            .await
            .is_ok());
            assert!(social_call(
                &ctx,
                &mut ceo_sess,
                "bookmark",
                "MoveBookmarksToFolderAndSubfolder",
                &[PyRep::List(vec![PyRep::Integer(bm_id)]), PyRep::Integer(shared_fid), PyRep::Integer(sub_fid)],
            )
            .await
            .is_ok());

            assert!(social_call(&ctx, &mut ceo_sess, "bookmark", "DeleteBookmarks", &[PyRep::List(vec![PyRep::Integer(bm_id)])]).await.is_ok());
            if sub_fid > 0 {
                let _ = social_call(&ctx, &mut ceo_sess, "bookmark", "DeleteSubfolder", &[PyRep::Integer(sub_fid)]).await;
            }
            assert!(social_call(&ctx, &mut ceo_sess, "bookmark", "DeleteFolder", &[PyRep::Integer(shared_fid)]).await.is_ok());
            let _ = HashSet::<i64>::new();
        });
}
