//! SQLite restart durability and transactional rollback integration tests for `D-6` Batch 3:
//! Corporation Governance (`feat:lifecycle.corp-governance-durable`, `feat:lifecycle.corp-governance-persist`),
//! CrimeWatch Timers & State (`feat:lifecycle.crimewatch-across-restart`, `feat:lifecycle.crimewatch-state-persist`),
//! and Warfare / FW / Sov / Insurgency (`feat:lifecycle.warfare-durable`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    corp_roles, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, IskCents,
    PersistedAggressionRecord, TrainedSkillRecord, UpwellStructureRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::{fw_services, insurgency_services, sov_services};
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const PILOT: i64 = 90_000_001;
const OTHER: i64 = 90_000_002;

fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600) * 10_000_000
}

fn temp_sqlite_path(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let p = std::env::temp_dir().join(format!("evomu_d6_{tag}_{pid}_{seq}.sqlite"));
    let _ = std::fs::remove_file(&p);
    p
}

fn open_ctx(db_path: &Path, now_unix: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let exists = db_path.exists();
    let repo = Arc::new(WorldRepository::open_sqlite(db_path).expect("open_sqlite"));
    if !exists {
        let seed_snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        repo.with_mut_snapshot(|snap| *snap = seed_snap);
    }
    let clock = Arc::new(VirtualServerClock::with_fixed_base(now_unix));
    repo.set_clock(clock.clone());
    let ctx = ServerContext::with_repository(repo);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

async fn login_and_select(ctx: &ServerContext, user: &str, pass: &str, char_id: i64, sess_id: u64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
    sess
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{service}.{method} failed: {e:?}"))
}

fn esi_get(ctx: &ServerContext, pattern: &str, params: &[(&str, i64)]) -> Value {
    let pp: HashMap<String, String> = params.iter().map(|(k, v)| ((*k).to_string(), v.to_string())).collect();
    let res = eve_server::esi::sample_routes::dispatch_operation(ctx, ctx.now_unix(), "GET", pattern, &pp, &HashMap::new(), None, 1, &[]);
    assert_eq!(res.status_code, 200, "ESI GET {pattern} failed: {res:?}");
    res.body.expect("ESI JSON body")
}

fn make_structure(id: i64, owner: i64, sys: u32, type_id: u32, name: &str) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: id, type_id, owner_corp_id: owner, solar_system_id: sys, name: name.into(),
        position: [200_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(), quantum_core_installed: true,
        fuel_blocks: 1_000, shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

#[tokio::test]
async fn test_d6_corp_governance_and_employment_history_persist_and_rollback() {
    // parity: feat:lifecycle.corp-governance-durable, feat:lifecycle.corp-governance-persist
    let db_path = temp_sqlite_path("corp_gov");
    let t0 = 1_775_000_000u64;
    let hangar_role: u64 = 1u64 << 13;

    let (corp_id, medal_id, vote_case_id, label_id, ad_id) = {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        ctx.db.with_mut_snapshot(|snap| {
            let sk = TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 };
            snap.skills.entry(PILOT).or_default().insert(3363, sk);
            snap.characters.get_mut(&PILOT).unwrap().balance = IskCents::from_isk(5_000_000_000);
        });
        let mut sess = login_and_select(&ctx, "demo", "demo-secret", PILOT, 8001).await;

        let add_corp = rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("D6 Durable Holdings".into()), PyRep::String("D6DH".into()), PyRep::Real(0.05)],
        )
        .await;
        let corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| add_corp.as_i64()).unwrap();

        // Add OTHER to corp via recruitment application workflow and update corp member limit from skills
        let mut sess_other = login_and_select(&ctx, "other", "other-secret", OTHER, 8009).await;
        let app_res = rpc(&ctx, &mut sess_other, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Join".into())]).await;
        let app_id = app_res.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap_or(1);
        rpc(&ctx, &mut sess, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await;
        rpc(&ctx, &mut sess, "corpRegistry", "UpdateCorporationAbilities", &[]).await;

        // Grant OTHER immediate Accountant + 24h pending hangar_role + grantable Junior_Accountant + HQ Auditor + title 1
        rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "UpdateMember",
            &[
                PyRep::Integer(OTHER),
                PyRep::Integer((hangar_role | corp_roles::ROLE_ACCOUNTANT) as i64),
                PyRep::Integer(1),
                PyRep::Bool(true),
                PyRep::Integer(corp_roles::ROLE_JUNIOR_ACCOUNTANT as i64),
                PyRep::Integer(corp_roles::ROLE_AUDITOR as i64),
            ],
        )
        .await;

        let medal_res = rpc(
            &ctx,
            &mut sess,
            "corporationSvc",
            "CreateMedal",
            &[PyRep::String("Valor of D6".into()), PyRep::String("Survived SQLite restart".into()), PyRep::List(vec![])],
        )
        .await;
        let medal_id = medal_res.dict_get("medalID").and_then(|v| v.as_i64()).unwrap_or(1);
        rpc(
            &ctx,
            &mut sess,
            "corporationSvc",
            "GiveMedalToCharacters",
            &[PyRep::Integer(medal_id), PyRep::List(vec![PyRep::Integer(PILOT)]), PyRep::String("Exemplary persistence".into())],
        )
        .await;
        rpc(&ctx, &mut sess, "corporationSvc", "SetMedalStatus", &[PyRep::Dict(vec![(PyRep::Integer(medal_id), PyRep::Integer(3))])]).await;

        rpc(&ctx, &mut sess, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Chief Archivist".into()), PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64)]).await;
        rpc(
            &ctx, &mut sess, "corpRegistry", "UpdateDivisionNames",
            &[
                PyRep::String("Hangar Alpha".into()), PyRep::String("Hangar Beta".into()), PyRep::String("Hangar 3".into()),
                PyRep::String("Hangar 4".into()), PyRep::String("Hangar 5".into()), PyRep::String("Hangar 6".into()), PyRep::String("Hangar 7".into()),
                PyRep::String("Treasury Prime".into()), PyRep::String("Ops Wallet".into()), PyRep::String("Div 3".into()),
                PyRep::String("Div 4".into()), PyRep::String("Div 5".into()), PyRep::String("Div 6".into()), PyRep::String("Div 7".into()),
            ],
        )
        .await;

        let vc = rpc(
            &ctx, &mut sess, "voteManager", "InsertVoteCase",
            &[PyRep::String("Expand Share Capital".into()), PyRep::String("Create 500 new shares".into()), PyRep::Integer(5), PyRep::Integer(500)],
        )
        .await;
        let vote_case_id = vc.dict_get("voteCaseID").and_then(|v| v.as_i64()).unwrap_or(1);
        rpc(&ctx, &mut sess, "voteManager", "InsertVote", &[PyRep::Integer(vote_case_id), PyRep::Integer(1)]).await;
        rpc(&ctx, &mut sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(PILOT), PyRep::Integer(750)]).await;
        rpc(&ctx, &mut sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(OTHER), PyRep::Integer(250)]).await;

        rpc(&ctx, &mut sess, "corpRegistry", "AddBulletin", &[PyRep::String("D6 Bulletin".into()), PyRep::String("Durable MOTD".into())]).await;
        rpc(&ctx, &mut sess, "corpRegistry", "RegisterNewAggressionSettings", &[PyRep::Bool(true)]).await;
        rpc(&ctx, &mut sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(OTHER), PyRep::Real(10.0), PyRep::Integer(0)]).await;
        let lbl = rpc(&ctx, &mut sess, "corpRegistry", "CreateLabel", &[PyRep::String("Coalition".into()), PyRep::Integer(0x112233)]).await;
        let label_id = lbl.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);
        rpc(&ctx, &mut sess, "corpRegistry", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(OTHER)]), PyRep::Integer(label_id)]).await;
        let ad = rpc(
            &ctx,
            &mut sess,
            "corpRecProxy",
            "CreateRecruitmentAd",
            &[PyRep::String("Join D6DH".into()), PyRep::String("Recruiting Indy & PvP".into()), PyRep::Integer(31)],
        )
        .await;
        let ad_id = ad.dict_get("adID").and_then(|v| v.as_i64()).unwrap_or(1);
        rpc(&ctx, &mut sess, "corpRegistry", "SetCorpWelcomeMail", &[PyRep::String("Welcome to D6DH".into()), PyRep::String("Read the bulletin!".into()), PyRep::Bool(true)]).await;
        rpc(&ctx, &mut sess, "corpRegistry", "RegisterNewCorpMailRestrictionSettings", &[PyRep::Bool(true)]).await;
        rpc(&ctx, &mut sess, "corpRegistry", "SetStructureReinforceDefault", &[PyRep::Integer(21)]).await;

        (corp_id, medal_id, vote_case_id, label_id, ad_id)
    };

    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 60);
        let mut sess2 = login_and_select(&ctx2, "demo", "demo-secret", PILOT, 8002).await;
        assert_eq!(sess2.corporation_id, Some(corp_id));

        let emp = rpc(&ctx2, &mut sess2, "corporationSvc", "GetEmploymentRecord", &[PyRep::Integer(PILOT)]).await;
        assert!(format!("{emp:?}").contains(&corp_id.to_string()), "employment history must persist: {emp:?}");

        let audit = rpc(&ctx2, &mut sess2, "corpmgr", "AuditMember", &[PyRep::Integer(OTHER)]).await;
        assert!(
            format!("{audit:?}").contains(&((hangar_role | corp_roles::ROLE_ACCOUNTANT) as i64).to_string()),
            "AuditMember log must persist: {audit:?}"
        );

        let bulletins = rpc(&ctx2, &mut sess2, "corpRegistry", "GetBulletins", &[]).await;
        assert!(format!("{bulletins:?}").contains("D6 Bulletin"), "bulletins must persist: {bulletins:?}");

        let agg = rpc(&ctx2, &mut sess2, "corpRegistry", "GetAggressionSettings", &[]).await;
        assert_eq!(agg.dict_get("pendingFriendlyFireLegal"), Some(&PyRep::Bool(true)));

        let medals = rpc(&ctx2, &mut sess2, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(PILOT)]).await;
        assert!(format!("{medals:?}").contains("Valor of D6") && format!("{medals:?}").contains(&medal_id.to_string()));

        let titles = rpc(&ctx2, &mut sess2, "corpRegistry", "GetTitles", &[]).await;
        assert!(format!("{titles:?}").contains("Chief Archivist"));

        let votes = rpc(&ctx2, &mut sess2, "voteManager", "GetVotes", &[PyRep::Integer(vote_case_id)]).await;
        assert!(format!("{votes:?}").contains(&PILOT.to_string()));

        let shares = rpc(&ctx2, &mut sess2, "corpRegistry", "GetSharesByShareholder", &[]).await;
        assert!(format!("{shares:?}").contains("750"), "PILOT shares must persist via RPC: {shares:?}");

        let ads = rpc(&ctx2, &mut sess2, "corpRecProxy", "GetRecruitmentAdsForCorporation", &[]).await;
        assert!(format!("{ads:?}").contains("Join D6DH") && format!("{ads:?}").contains(&ad_id.to_string()));

        let contacts = rpc(&ctx2, &mut sess2, "corpRegistry", "GetCorporateContacts", &[]).await;
        assert!(format!("{contacts:?}").contains(&OTHER.to_string()) && format!("{contacts:?}").contains(&label_id.to_string()));

        let labels = rpc(&ctx2, &mut sess2, "corpRegistry", "GetLabels", &[]).await;
        assert!(format!("{labels:?}").contains("Coalition"));

        let welcome = rpc(&ctx2, &mut sess2, "corpRegistry", "GetCorpWelcomeMail", &[]).await;
        assert_eq!(welcome.dict_get("subject").and_then(|v| v.as_str()), Some("Welcome to D6DH"));
        assert_eq!(rpc(&ctx2, &mut sess2, "corpRegistry", "DoesCorpRestrictCorpMails", &[]).await, PyRep::Bool(true));
        assert_eq!(rpc(&ctx2, &mut sess2, "corpRegistry", "GetStructureReinforceDefault", &[]).await, PyRep::Integer(21));

        // Verify ESI endpoints reflect all RPC-driven corp governance mutations after restart
        let divs = esi_get(&ctx2, "/corporations/{corporation_id}/divisions", &[("corporation_id", corp_id)]);
        assert_eq!(divs["hangar"][0]["name"], "Hangar Alpha");
        assert_eq!(divs["wallet"][0]["name"], "Treasury Prime");
        assert_eq!(esi_get(&ctx2, "/corporations/{corporation_id}/members/limit", &[("corporation_id", corp_id)]), 100);
        let esi_titles = esi_get(&ctx2, "/corporations/{corporation_id}/titles", &[("corporation_id", corp_id)]);
        assert_eq!(esi_titles[0]["name"], "Chief Archivist");
        assert_eq!(esi_titles[0]["roles"][0], "Director");
        let mem_titles = esi_get(&ctx2, "/corporations/{corporation_id}/members/titles", &[("corporation_id", corp_id)]);
        assert!(mem_titles.to_string().contains(&OTHER.to_string()));
        let esi_roles = esi_get(&ctx2, "/corporations/{corporation_id}/roles", &[("corporation_id", corp_id)]);
        assert!(esi_roles.to_string().contains("Accountant") && esi_roles.to_string().contains("Junior_Accountant") && esi_roles.to_string().contains("Auditor"));
        let role_hist = esi_get(&ctx2, "/corporations/{corporation_id}/roles/history", &[("corporation_id", corp_id)]);
        assert!(role_hist.to_string().contains("Accountant"));
        let char_roles = esi_get(&ctx2, "/characters/{character_id}/roles", &[("character_id", OTHER)]);
        assert!(char_roles["roles"].to_string().contains("Accountant") && char_roles["roles_at_hq"].to_string().contains("Auditor"));
        let sh = esi_get(&ctx2, "/corporations/{corporation_id}/shareholders", &[("corporation_id", corp_id)]);
        assert!(sh.to_string().contains("750") && sh.to_string().contains("250"));
        let esi_medals = esi_get(&ctx2, "/corporations/{corporation_id}/medals", &[("corporation_id", corp_id)]);
        assert_eq!(esi_medals[0]["title"], "Valor of D6");
        let issued = esi_get(&ctx2, "/corporations/{corporation_id}/medals/issued", &[("corporation_id", corp_id)]);
        assert_eq!(issued[0]["reason"], "Exemplary persistence");
        assert_eq!(issued[0]["status"], "public");
        assert!(esi_get(&ctx2, "/characters/{character_id}/corporationhistory", &[("character_id", PILOT)]).to_string().contains(&corp_id.to_string()));
        assert_eq!(esi_get(&ctx2, "/characters/{character_id}/titles", &[("character_id", OTHER)])[0]["name"], "Chief Archivist");
        assert_eq!(esi_get(&ctx2, "/characters/{character_id}/medals", &[("character_id", PILOT)])[0]["title"], "Valor of D6");

        ctx2.db.with_snapshot(|snap| {
            let gov = snap.corp_governance.get(&corp_id).expect("persisted CorpGovState");
            assert_eq!(gov.pending_role_grants.get(&OTHER), Some(&(hangar_role | corp_roles::ROLE_ACCOUNTANT, t0 + 86_400)));
        });

        let abort_res: Result<(), String> = ctx2.db.try_with_mut_snapshot(|snap| {
            snap.corp_governance.get_mut(&corp_id).unwrap().reinforce_hour = 3;
            snap.employment_history.clear();
            Err("simulated corp_governance abort".to_string())
        });
        assert!(abort_res.is_err());
        assert_eq!(ctx2.db.with_snapshot(|s| s.corp_governance[&corp_id].reinforce_hour), 21);
        assert!(ctx2.db.with_snapshot(|s| !s.employment_history.is_empty()));
    }

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_d6_crimewatch_timers_and_combat_crimewatch_persist_lazy_expire_and_rollback() {
    // parity: feat:lifecycle.crimewatch-across-restart, feat:lifecycle.crimewatch-state-persist
    let db_path = temp_sqlite_path("crimewatch");
    let t0 = 1_775_100_000u64;

    {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        let mut sess = login_and_select(&ctx, "demo", "demo-secret", PILOT, 8101).await;
        let mut sess_other = login_and_select(&ctx, "other", "other-secret", OTHER, 8109).await;

        rpc(&ctx, &mut sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await;
        rpc(&ctx, &mut sess, "bountyProxy", "AddToBounty", &[PyRep::Integer(OTHER), PyRep::Integer(5_000_000)]).await;
        rpc(&ctx, &mut sess, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(OTHER)]).await;
        rpc(&ctx, &mut sess_other, "crimewatch", "RespondToDuelChallenge", &[PyRep::Integer(PILOT), PyRep::Bool(true)]).await;
        rpc(&ctx, &mut sess, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(OTHER)]).await;
        rpc(&ctx, &mut sess, "entity", "ChangeDroneSettings", &[PyRep::Bool(true), PyRep::Bool(true)]).await;

        // Turn in a Clone Soldier Negotiator Tag (35664) via ApplyTagsForSecStatus while docked
        ctx.db.with_mut_snapshot(|snap| {
            let station_id = snap.characters[&PILOT].station_id.unwrap_or(60_003_760);
            snap.characters.get_mut(&PILOT).unwrap().security_status = -1.0;
            snap.characters.get_mut(&PILOT).unwrap().balance = IskCents::from_isk(500_000_000);
            snap.items.insert(
                880_001,
                InventoryItemRecord {
                    item_id: 880_001,
                    type_id: 35664,
                    owner_id: PILOT,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        });
        rpc(&ctx, &mut sess, "crimewatch", "ApplyTagsForSecStatus", &[PyRep::Integer(35664), PyRep::Integer(1)]).await;

        // Undock, jump Turnur -> Zarzakh via CmdStargateJump (creates 6h Zarzakh gate lock),
        // then jump Zarzakh -> Thera via WormholeJump (creates 300s wormhole polarization)
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await;
        let ship_id = sess.ship_id.unwrap();
        let mut ball = ctx.systems.get_or_spawn(30_000_142).remove_ball(ship_id).await.unwrap();
        ctx.db.set_character_solar_system(PILOT, 30_002_086, &ctx.sde).unwrap();
        ball.position = ctx.sde.jumps[&50_016_562].position;
        ball.mode = MovementMode::Stop;
        ctx.systems.get_or_spawn(30_002_086).spawn_ball(ball).await.unwrap();
        sess.solar_system_id = Some(30_002_086);
        let b_turnur = ctx.bind_object(&mut sess, "beyonce", 30_002_086).unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT);
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_turnur), "CmdStargateJump", &[PyRep::Integer(50_016_562)], &HashMap::new(), None).await.expect("jump into Zarzakh");
        let (_, z_ball) = ctx.systems.get_or_spawn(30_100_000).get_ball_snapshot(ship_id).await.unwrap().unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.cosmic_signatures.entry(30_100_000).or_default().push(CosmicSignatureRecord {
                signature_id: "WH-D6".into(),
                solar_system_id: 30_100_000,
                sig_group: "Wormhole".into(),
                type_name: "Unstable Wormhole".into(),
                position: [z_ball.position.x, z_ball.position.y, z_ball.position.z],
                signal_strength: 1.0,
                wormhole_type_id: Some(26272),
                destination_system_id: Some(31_000_005),
                created_unix: t0,
                lifetime_seconds: 86_400,
                max_mass_kg: 2_000_000_000.0,
                remaining_mass_kg: 2_000_000_000.0,
                max_jump_mass_kg: 300_000_000.0,
                collapsed: false,
            });
            snap.structures.insert(1_000_000_700_001, make_structure(1_000_000_700_001, 1_000_002, 30_000_142, 35832, "ACL Citadel"));
            snap.structure_docking_acl_denied.entry(1_000_000_700_001).or_default().insert(PILOT);
        });
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT);
        rpc(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::String("WH-D6".into())]).await;
        ctx.db.with_mut_snapshot(|snap| {
            let c = snap.characters.get_mut(&PILOT).unwrap();
            c.solar_system_id = 30_000_142;
            c.station_id = Some(60_003_760);
            let sh = snap.items.get_mut(&ship_id).unwrap();
            sh.location_id = 60_003_760;
            sh.flag = InventoryFlag::Hangar;
            snap.kill_rights.insert(77_001, eve_db::KillRightRecord { kill_right_id: 77_001, owner_id: PILOT, target_id: OTHER, activation_fee: IskCents::from_isk(2_500_000), is_active_suspect: false });
        });

        ctx.crimewatch.write().unwrap().get_or_create(OTHER).criminal_flag_until_unix = t0 + 900;
        let (flag, is_crim, _) = eve_server::services::combat_crimewatch::evaluate_remote_assistance_legality(&ctx, PILOT, OTHER, 30_000_142, t0);
        assert_eq!((flag, is_crim), ("Criminal", true));
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            let st = cw.get_or_create(PILOT);
            st.pvp_timer_until_unix = t0 + 900;
            st.suspect_flag_until_unix = t0 + 900;
            st.pending_concord_arrival = Some((30_000_142, t0 + 49, 100_000_001, OTHER));
            cw.aggression_mgr.record_aggression(PILOT, OTHER, 30_000_142, false, true, t0);
        }
        eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
            rt.npc_timers.insert(PILOT, t0 + 120);
            rt.disapproval_flags.insert(PILOT, t0 + 180);
            rt.kill_right_expires_unix.insert(77_001, t0 + 30 * 86_400);
        });
    }

    // Restart #1 at t0 + 30s (all timers still active!)
    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 30);
        let mut sess2 = login_and_select(&ctx2, "demo", "demo-secret", PILOT, 8102).await;

        let cs = rpc(&ctx2, &mut sess2, "crimewatch", "GetClientStates", &[]).await;
        let PyRep::Tuple(items) = cs else { panic!("expected 4-tuple, got {cs:?}") };
        let PyRep::Tuple(timers) = &items[0] else { panic!("expected combat timers tuple") };
        assert_eq!(timers[0], PyRep::Tuple(vec![PyRep::Integer(101), PyRep::Integer(unix_to_filetime(t0 + 60))]));
        assert_eq!(timers[1], PyRep::Tuple(vec![PyRep::Integer(201), PyRep::Integer(unix_to_filetime(t0 + 900))]));
        assert_eq!(timers[2], PyRep::Tuple(vec![PyRep::Integer(401), PyRep::Integer(unix_to_filetime(t0 + 120))]));
        assert_eq!(timers[3], PyRep::Tuple(vec![PyRep::Integer(301), PyRep::Integer(unix_to_filetime(t0 + 900))]));
        assert_eq!(timers[4], PyRep::Tuple(vec![PyRep::Integer(501), PyRep::Integer(unix_to_filetime(t0 + 180))]));
        assert!(matches!(
            &items[1],
            PyRep::Dict(entries)
                if entries.iter().any(|(k, v)| k.as_i64() == Some(OTHER)
                    && *v == PyRep::Integer((t0 + 300) as i64))
        ));
        assert_eq!(items[3], PyRep::Integer(0), "safety level Red must survive restart");

        {
            let cw = ctx2.crimewatch.read().unwrap();
            let st = cw.states.get(&PILOT).expect("persisted CrimeWatchState");
            assert!(st.is_weapon_timer_active(t0 + 30));
            assert!(st.is_pvp_active(t0 + 30));
            assert!(st.is_suspect(t0 + 30));
            assert!(st.is_criminal(t0 + 30));
            assert_eq!(st.pending_concord_arrival, Some((30_000_142, t0 + 49, 100_000_001, OTHER)));
            let agg: Option<&PersistedAggressionRecord> = cw.aggression_mgr.get_aggression(PILOT, OTHER, t0 + 30);
            assert!(agg.is_some(), "aggression record must survive SQLite restart");
        }

        let wh_pol = rpc(&ctx2, &mut sess2, "wormholeMgr", "GetWormholePolarization", &[PyRep::Integer(31_000_005)]).await;
        assert_eq!(wh_pol.dict_get("isPolarized"), Some(&PyRep::Bool(true)));
        assert_eq!(wh_pol.dict_get("remainingSeconds"), Some(&PyRep::Integer(270)));

        let sec_tx = rpc(&ctx2, &mut sess2, "crimewatch", "GetSecurityStatusTransactions", &[]).await;
        assert!(format!("{sec_tx:?}").contains("Clone Soldier Security Tag"));
        let bkr = rpc(&ctx2, &mut sess2, "bountyProxy", "GetBountiesAndKillRights", &[PyRep::List(vec![PyRep::Integer(OTHER)]), PyRep::List(vec![PyRep::Integer(OTHER)])]).await;
        assert!(format!("{bkr:?}").contains("5000000") && format!("{bkr:?}").contains("2500000"));

        ctx2.db.with_snapshot(|s| {
            let st = &s.structures[&1_000_000_700_001];
            assert!(!eve_server::esi::universe_routes::character_has_structure_acl_access(s, PILOT, st));
            assert_eq!(s.combat_crimewatch_state.drone_settings.get(&PILOT), Some(&(true, true)));
            assert_eq!(s.combat_crimewatch_state.player_bounty_pool.get(&OTHER).copied(), Some(5_000_000.0));
            assert!(s.combat_crimewatch_state.kill_right_expires_unix.values().any(|&exp| exp == t0 + 30 * 86_400));
            assert_eq!(s.zarzakh_gate_locks.get(&PILOT), Some(&(50_016_566, t0 + 21_600)));
        });

        let abort_res: Result<(), String> = ctx2.db.try_with_mut_snapshot(|snap| {
            snap.crimewatch_states.clear();
            snap.combat_crimewatch_state.safety_levels.insert(PILOT, 2);
            Err("simulated crimewatch abort".to_string())
        });
        assert!(abort_res.is_err());
        assert_eq!(ctx2.db.with_snapshot(|s| s.combat_crimewatch_state.safety_levels.get(&PILOT).copied()), Some(0));
        assert!(ctx2.db.with_snapshot(|s| s.crimewatch_states.contains_key(&PILOT)));
    }

    // Restart #2 at t0 + 1_000s (past 60s weapons, 120s npc, 180s disapproval, 300s engagement/polarization, and 900s suspect/criminal timers)
    {
        let (ctx3, _clock3) = open_ctx(&db_path, t0 + 1_000);
        let mut sess3 = login_and_select(&ctx3, "demo", "demo-secret", PILOT, 8103).await;

        let cs = rpc(&ctx3, &mut sess3, "crimewatch", "GetClientStates", &[]).await;
        let PyRep::Tuple(items) = cs else { panic!("expected 4-tuple") };
        let PyRep::Tuple(timers) = &items[0] else { panic!("expected combat timers tuple") };
        assert_eq!(timers[0], PyRep::Tuple(vec![PyRep::Integer(100), PyRep::None]), "weapons timer lazily expired");
        assert_eq!(timers[1], PyRep::Tuple(vec![PyRep::Integer(200), PyRep::None]), "pvp timer lazily expired");
        assert_eq!(timers[2], PyRep::Tuple(vec![PyRep::Integer(400), PyRep::None]), "npc timer lazily expired");
        assert_eq!(timers[3], PyRep::Tuple(vec![PyRep::Integer(300), PyRep::None]), "crim/suspect timer lazily expired");
        assert_eq!(timers[4], PyRep::Tuple(vec![PyRep::Integer(500), PyRep::None]), "disapproval timer lazily expired");
        assert_eq!(items[1], PyRep::Dict(vec![]), "limited engagement lazily expired");
        assert_eq!(items[3], PyRep::Integer(0), "safety level does not expire");

        let wh_pol = rpc(&ctx3, &mut sess3, "wormholeMgr", "GetWormholePolarization", &[PyRep::Integer(31_000_005)]).await;
        assert_eq!(wh_pol.dict_get("isPolarized"), Some(&PyRep::Bool(false)));

        // Undock and verify persisted StructureDockingAccessDenied & ZarzakhGateLocked via RPC dispatch
        rpc(&ctx3, &mut sess3, "stationSvc", "Undock", &[]).await;
        let ship_id = sess3.ship_id.expect("active ship");
        let h_jita = ctx3.systems.get_or_spawn(30_000_142);
        let (_, mut ball) = h_jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        ball.position = DVec3::new(201_000.0, 0.0, 0.0);
        ball.mode = MovementMode::Stop;
        h_jita.spawn_ball(ball.clone()).await.unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx3, PILOT);
        let dock_err = ctx3.dispatch_rpc(&mut sess3, "structureDocking", None, "Dock", &[PyRep::Integer(1_000_000_700_001)], &HashMap::new(), None).await.unwrap_err();
        assert_eq!(dock_err.code, "StructureDockingAccessDenied");

        let _ = h_jita.remove_ball(ship_id).await;
        ctx3.db.set_character_solar_system(PILOT, 30_100_000, &ctx3.sde).unwrap();
        ball.position = ctx3.sde.jumps[&50_016_567].position;
        ctx3.systems.get_or_spawn(30_100_000).spawn_ball(ball).await.unwrap();
        sess3.solar_system_id = Some(30_100_000);
        let beyonce = ctx3.bind_object(&mut sess3, "beyonce", 30_100_000).unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx3, PILOT);
        let gate_err = ctx3.dispatch_rpc(&mut sess3, "beyonce", Some(beyonce), "CmdStargateJump", &[PyRep::Integer(50_016_567)], &HashMap::new(), None).await.unwrap_err();
        assert_eq!(gate_err.code, "ZarzakhGateLocked");
    }

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_d6_warfare_fw_sov_and_insurgency_persist_and_rollback() {
    // parity: feat:lifecycle.warfare-durable
    let db_path = temp_sqlite_path("warfare");
    let t0 = 1_775_200_000u64;
    let fw_sys = fw_services::HQ_SYSTEM_AMO;
    let sov_sys = 30_004_759u32;
    let ins_sys = 30_002_537u32;

    let (corp_a, corp_b, alliance_id, war_id, neg_id, hub_id, campaign_id) = {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        ctx.db.with_mut_snapshot(|snap| {
            let sk = TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 };
            snap.skills.entry(PILOT).or_default().insert(3363, sk.clone());
            snap.skills.entry(OTHER).or_default().insert(3363, sk);
            snap.characters.get_mut(&PILOT).unwrap().balance = IskCents::from_isk(5_000_000_000);
            snap.characters.get_mut(&OTHER).unwrap().balance = IskCents::from_isk(5_000_000_000);
            snap.fw_system_occupations.insert(fw_sys, eve_db::snapshot_ext::FwSystemOccupationRecord { solar_system_id: fw_sys, owner_faction_id: fw_services::FACTION_MINMATAR, occupier_faction_id: fw_services::FACTION_MINMATAR, victory_points: 0, victory_points_threshold: 3000 });
        });
        let mut sess_a = login_and_select(&ctx, "demo", "demo-secret", PILOT, 8201).await;
        let mut sess_b = login_and_select(&ctx, "other", "other-secret", OTHER, 8202).await;

        let ca = rpc(&ctx, &mut sess_a, "corpRegistry", "AddCorporation", &[PyRep::String("Aggressor Armada".into()), PyRep::String("AGGR".into()), PyRep::Real(0.05)]).await;
        let corp_a = ca.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| ca.as_i64()).unwrap();

        let cb = rpc(&ctx, &mut sess_b, "corpRegistry", "AddCorporation", &[PyRep::String("Defender Bastion".into()), PyRep::String("DEFB".into()), PyRep::Real(0.05)]).await;
        let corp_b = cb.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| cb.as_i64()).unwrap();

        let hq_id = 700_000_901i64;
        let def_struct_id = 700_000_902i64;
        let hub_id = 700_000_903i64;
        let skyhook_id = 700_000_999i64;
        ctx.db.with_mut_snapshot(|snap| {
            for cid in [corp_a, corp_b] {
                if let Some(c) = snap.corporations.get_mut(&cid) {
                    c.ensure_wallet_divisions();
                    c.wallet_divisions[0].balance = IskCents::from_isk(3_000_000_000);
                    c.master_wallet = c.wallet_divisions[0].balance;
                }
            }
            snap.structures.insert(hq_id, make_structure(hq_id, corp_a, 30_000_142, 35832, "Aggressor Astrahus HQ"));
            snap.structures.insert(def_struct_id, make_structure(def_struct_id, corp_b, 30_000_142, 35832, "Defender Astrahus"));
            snap.structures.insert(hub_id, make_structure(hub_id, corp_a, sov_sys, sov_services::TYPE_SOVEREIGNTY_HUB, "Sov Hub Prime"));
            snap.structures.insert(skyhook_id, make_structure(skyhook_id, corp_a, sov_sys, sov_services::TYPE_ORBITAL_SKYHOOK, "Skyhook Prime"));
            snap.sov_prospecting_levels.insert(sov_sys, 4);
        });

        let al_res = rpc(&ctx, &mut sess_a, "corpRegistry", "CreateAlliance", &[PyRep::String("D6 Sovereign Pact".into()), PyRep::String("D6SP".into()), PyRep::String("Durable Alliance".into()), PyRep::String("https://d6.example".into())]).await;
        let alliance_id = al_res.dict_get("allianceID").and_then(|v| v.as_i64()).or_else(|| al_res.as_i64()).unwrap();

        rpc(&ctx, &mut sess_b, "corpRegistry", "ApplyToJoinAlliance", &[PyRep::Integer(alliance_id), PyRep::String("Joining D6SP".into())]).await;
        rpc(&ctx, &mut sess_a, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(corp_b)]).await;

        let war_res = rpc(&ctx, &mut sess_a, "corpRegistry", "DeclareWarAgainst", &[PyRep::Integer(corp_b), PyRep::Integer(hq_id)]).await;
        let war_id = war_res.dict_get("warID").and_then(|v| v.as_i64()).or_else(|| war_res.as_i64()).unwrap();

        rpc(&ctx, &mut sess_b, "warsInfoMgr", "SetOpenForAllies", &[PyRep::Integer(war_id), PyRep::Bool(true)]).await;
        let neg = rpc(&ctx, &mut sess_b, "warsInfoMgr", "CreateSurrenderNegotiation", &[PyRep::Integer(war_id), PyRep::Real(25_000_000.0), PyRep::String("Peace terms".into())]).await;
        let neg_id = neg.dict_get("negotiationID").and_then(|v| v.as_i64()).unwrap_or(1);
        rpc(&ctx, &mut sess_a, "warsInfoMgr", "AcceptSurrender", &[PyRep::Integer(neg_id)]).await;

        rpc(&ctx, &mut sess_a, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(fw_services::FACTION_MINMATAR)]).await;
        fw_services::add_victory_points(&ctx, fw_sys, 1_480);
        fw_services::update_system_advantage(&ctx, fw_sys, 0.25, 0.10);
        fw_services::award_fw_kill_lp(&ctx, PILOT, fw_sys, 100_000_000.0);
        fw_services::spawn_battlefield_site(&ctx, fw_sys, fw_services::FACTION_MINMATAR, "Amo Siege");

        rpc(&ctx, &mut sess_a, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id), PyRep::Integer(42)]).await;
        sov_services::set_skyhook_hackable(&ctx, sov_sys, skyhook_id, true);
        sov_services::record_hub_hack(&ctx, sov_sys, hub_id, 3_600);

        let campaign_id = 9001i64;
        insurgency_services::init_insurgency_campaign(&ctx, campaign_id, insurgency_services::FACTION_GURISTAS, fw_services::FACTION_CALDARI, ins_sys, vec![ins_sys]);
        insurgency_services::add_system_corruption(&ctx, campaign_id, ins_sys, 450);
        insurgency_services::add_system_suppression(&ctx, campaign_id, ins_sys, 220);
        insurgency_services::spawn_ice_heist(&ctx, ins_sys);

        (corp_a, corp_b, alliance_id, war_id, neg_id, hub_id, campaign_id)
    };

    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 120);
        let mut sess_a2 = login_and_select(&ctx2, "demo", "demo-secret", PILOT, 8203).await;
        assert_eq!(sess_a2.corporation_id, Some(corp_a));
        assert_eq!(sess_a2.alliance_id, Some(alliance_id));

        let wars_rep = rpc(&ctx2, &mut sess_a2, "warsInfoMgr", "GetWarsByOwnerID", &[PyRep::Integer(corp_a)]).await;
        assert!(format!("{wars_rep:?}").contains(&war_id.to_string()) && format!("{wars_rep:?}").contains(&corp_b.to_string()));
        let apps_rep = rpc(&ctx2, &mut sess_a2, "corpRegistry", "GetAllianceApplications", &[]).await;
        assert!(format!("{apps_rep:?}").contains(&corp_b.to_string()), "alliance applications must persist: {apps_rep:?}");

        let invites = rpc(&ctx2, &mut sess_a2, "mutualWarInviteMgr", "GetPendingInvitesForSession", &[]).await;
        assert!(format!("{invites:?}").contains(&corp_b.to_string()), "mutual war invites must persist: {invites:?}");

        let surr = rpc(&ctx2, &mut sess_a2, "warsInfoMgr", "GetSurrenderNegotiations", &[PyRep::Integer(war_id)]).await;
        assert!(format!("{surr:?}").contains(&neg_id.to_string()) && format!("{surr:?}").contains("Accepted") && format!("{surr:?}").contains("Peace terms"));

        let treaties = rpc(&ctx2, &mut sess_a2, "peaceTreatyMgr", "GetPeaceTreatiesForSession", &[]).await;
        assert!(format!("{treaties:?}").contains(&war_id.to_string()), "peace treaties must persist: {treaties:?}");

        let vp_states = rpc(&ctx2, &mut sess_a2, "fwVictoryPointMgr", "GetAllVictoryPointStates", &[]).await;
        assert!(format!("{vp_states:?}").contains("1500"), "FW victory points must persist via RPC: {vp_states:?}");
        assert_eq!(ctx2.db.with_snapshot(|s| s.fw_runtime.system_advantage.get(&fw_sys).copied()), Some((0.25, 0.10)));

        let bf = rpc(&ctx2, &mut sess_a2, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[PyRep::Integer(fw_sys as i64)]).await;
        assert!(format!("{bf:?}").contains("Amo Siege"));

        let fuel_grp = rpc(&ctx2, &mut sess_a2, "sovMgr", "GetSovHubFuelAccessGroup", &[PyRep::Integer(hub_id)]).await;
        assert_eq!(fuel_grp, PyRep::Integer(42));

        let skyhooks = rpc(&ctx2, &mut sess_a2, "colonyResourcesMgr", "GetMyCorporationSkyhooks", &[]).await;
        assert!(format!("{skyhooks:?}").contains("500") && format!("{skyhooks:?}").contains("700000999"));

        let ihub = rpc(&ctx2, &mut sess_a2, "sovMgr", "GetInfrastructureHubInfo", &[PyRep::Integer(sov_sys as i64)]).await;
        assert_eq!(ihub.dict_get("prospectingLevel"), Some(&PyRep::Integer(4)));

        let vis_camps = rpc(&ctx2, &mut sess_a2, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await;
        assert!(format!("{vis_camps:?}").contains(&campaign_id.to_string()));
        ctx2.db.set_character_solar_system(PILOT, ins_sys, &ctx2.sde).unwrap();
        let PyRep::Tuple(ins_loc) = rpc(&ctx2, &mut sess_a2, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await else { panic!("expected tuple") };
        assert_eq!(ins_loc[1].dict_get("phase"), Some(&PyRep::String("Forecasting".into())));
        assert_eq!(ins_loc[1].dict_get("corruptionPoints"), Some(&PyRep::Integer(450)));
        assert_eq!(ins_loc[1].dict_get("suppressionPoints"), Some(&PyRep::Integer(220)));
        assert_eq!(ins_loc[1].dict_get("corruptionStage"), Some(&PyRep::Integer(3)));
        assert_eq!(ins_loc[1].dict_get("suppressionStage"), Some(&PyRep::Integer(2)));
        let heist = rpc(&ctx2, &mut sess_a2, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", &[PyRep::Integer(ins_sys as i64)]).await;
        assert!(format!("{heist:?}").contains("800001"));

        // Verify ESI endpoints for Alliance, War, FW, and Sovereignty after restart
        let esi_al = esi_get(&ctx2, "/alliances/{alliance_id}", &[("alliance_id", alliance_id)]);
        assert_eq!(esi_al["creator_id"], PILOT);
        assert_eq!(esi_al["creator_corporation_id"], corp_a);
        let esi_war = esi_get(&ctx2, "/wars/{war_id}", &[("war_id", war_id)]);
        assert_eq!(esi_war["open_for_allies"], true);
        assert!(esi_war.get("finished").is_some(), "surrender finished timestamp must appear on ESI /wars/{{id}}: {esi_war}");
        let esi_fw_sys = esi_get(&ctx2, "/fw/systems", &[]);
        assert!(esi_fw_sys.to_string().contains(&fw_sys.to_string()) && esi_fw_sys.to_string().contains("1500"));
        let esi_fw_char = esi_get(&ctx2, "/characters/{character_id}/fw/stats", &[("character_id", PILOT)]);
        assert_eq!(esi_fw_char["kills"]["total"], 1);
        assert_eq!(esi_fw_char["victory_points"]["total"], 20);
        let esi_sov_camps = esi_get(&ctx2, "/sovereignty/campaigns", &[]);
        assert!(esi_sov_camps.to_string().contains(&hub_id.to_string()) && esi_sov_camps.to_string().contains("0.6"));

        let abort_res: Result<(), String> = ctx2.db.try_with_mut_snapshot(|snap| {
            snap.war_state.wars.clear();
            snap.alliance_war_state.alliances.clear();
            snap.fw_runtime.system_victory_points.clear();
            snap.sov_runtime.hacked_hubs.clear();
            snap.insurgency_runtime.campaigns.clear();
            Err("simulated warfare abort".to_string())
        });
        assert!(abort_res.is_err());
        ctx2.db.with_snapshot(|snap| {
            assert!(snap.war_state.wars.contains_key(&war_id));
            assert!(snap.alliance_war_state.alliances.contains_key(&alliance_id));
            assert_eq!(snap.fw_runtime.system_victory_points.get(&fw_sys).copied(), Some(1_500));
            assert!(snap.sov_runtime.hacked_hubs.contains_key(&hub_id));
            assert!(snap.insurgency_runtime.campaigns.contains_key(&campaign_id));
        });
    }

    let _ = std::fs::remove_file(&db_path);
}
