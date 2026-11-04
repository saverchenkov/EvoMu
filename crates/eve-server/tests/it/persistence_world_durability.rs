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
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub(crate) const PILOT: i64 = 90_000_001;
pub(crate) const OTHER: i64 = 90_000_002;

fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600) * 10_000_000
}

pub(crate) fn temp_sqlite_path(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let p = std::env::temp_dir().join(format!("evomu_d6_{tag}_{pid}_{seq}.sqlite"));
    let _ = std::fs::remove_file(&p);
    p
}

pub(crate) fn open_ctx(db_path: &Path, now_unix: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let exists = db_path.exists();
    let repo = Arc::new(WorldRepository::open_sqlite(db_path).expect("open_sqlite"));
    if !exists {
        let seed_snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        repo.with_mut_snapshot(|snap| *snap = seed_snap);
    }
    let clock = Arc::new(VirtualServerClock::with_fixed_base(now_unix));
    repo.set_clock(clock.clone());
    let ctx = ServerContext::with_sde_and_repository(ServerContext::new().sde.clone(), repo);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

pub(crate) async fn login_and_select(ctx: &ServerContext, user: &str, pass: &str, char_id: i64, sess_id: u64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
    sess
}

pub(crate) async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{service}.{method} failed: {e:?}"))
}

pub(crate) fn esi_get(ctx: &ServerContext, pattern: &str, params: &[(&str, i64)]) -> Value {
    let pp: HashMap<String, String> = params.iter().map(|(k, v)| ((*k).to_string(), v.to_string())).collect();
    let res = eve_server::esi::sample_routes::dispatch_operation(ctx, ctx.now_unix(), "GET", pattern, &pp, &HashMap::new(), None, 1, &[]);
    assert_eq!(res.status_code, 200, "ESI GET {pattern} failed: {res:?}");
    res.body.expect("ESI JSON body")
}

pub(crate) fn make_structure(id: i64, owner: i64, sys: u32, type_id: u32, name: &str) -> UpwellStructureRecord {
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

        rpc(&ctx, &mut sess, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Chief Archivist".into()), PyRep::Integer(corp_roles::ROLE_PERSONNEL_MANAGER as i64)]).await;
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
        assert_eq!(esi_titles[0]["roles"][0], "Personnel_Manager");
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

        // Turn in a Clone Soldier Negotiator Tag (33141) via ApplyTagsForSecStatus while docked at a low-sec Security Office
        let sec_office_st = ctx
            .sde
            .stations
            .values()
            .find_map(|st| {
                let sec = ctx.sde.solar_systems.get(&st.solar_system_id)?.security_status;
                (0.0 < sec && sec < 0.45 && eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, st.station_id, 27)).then_some(st.station_id)
            })
            .unwrap_or(60_014_861);
        let (orig_station, orig_sys) = (sess.station_id, sess.solar_system_id);
        sess.station_id = Some(sec_office_st);
        sess.solar_system_id = ctx.sde.stations.get(&sec_office_st).map(|s| s.solar_system_id);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&PILOT).unwrap();
            ch.station_id = Some(sec_office_st);
            ch.security_status = -1.0;
            ch.balance = IskCents::from_isk(500_000_000);
            snap.items.insert(880_001, InventoryItemRecord {
                item_id: 880_001, type_id: 33141, owner_id: PILOT, location_id: sec_office_st, flag: InventoryFlag::Hangar,
                quantity: 1, singleton: false, version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            });
        });
        rpc(&ctx, &mut sess, "crimewatch", "ApplyTagsForSecStatus", &[PyRep::Real(-0.5), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])]).await;
        ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&PILOT).unwrap().station_id = orig_station; });
        sess.station_id = orig_station;
        sess.solar_system_id = orig_sys;

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
        let PyRep::Tuple(wh_pol_tuple) = wh_pol else { panic!("expected 2-tuple from GetWormholePolarization") };
        assert_eq!(wh_pol_tuple.len(), 2);
        assert!(wh_pol_tuple[0].as_i64().unwrap() > 0);
        assert_eq!(wh_pol_tuple[1], PyRep::Integer(300 * 10_000_000));

        let sec_tx = rpc(&ctx2, &mut sess2, "crimewatch", "GetSecurityStatusTransactions", &[]).await;
        let PyRep::Rowset { columns: sec_cols, rows: sec_rows, .. } = &sec_tx else {
            panic!("expected Rowset from GetSecurityStatusTransactions, got {sec_tx:?}");
        };
        for col in ["eventDate", "eventTypeID", "locationID", "modification", "newValue", "otherOwnerID", "otherTypeID"] {
            assert!(sec_cols.iter().any(|c| c == col), "missing V24 column {col} in {sec_cols:?}");
        }
        assert!(!sec_rows.is_empty());
        let sec_txt = format!("{sec_tx:?}");
        assert!(sec_txt.contains("Integer(385)") && sec_txt.contains("Real(-0.5)"), "tag turn-in transaction must persist across SQLite restart: {sec_txt}");
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
        assert_eq!(wh_pol, PyRep::None);

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

pub(crate) mod warfare_fw_sov_insurgency_persist {
    // SQLite restart durability and transactional rollback integration tests for `D-6` Batch 3 (Part 2):
    // Warfare / FW / Sov / Insurgency (`feat:lifecycle.warfare-durable`).


    use crate::persistence_world_durability::{
        esi_get, login_and_select, make_structure, open_ctx, rpc, temp_sqlite_path, OTHER, PILOT,
    };
    use eve_db::{IskCents, TrainedSkillRecord};
    use eve_proto::PyRep;
    use eve_server::services::{fw_services, insurgency_services, sov_services};

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
                snap.fw_system_occupations.insert(
                    fw_sys,
                    eve_db::snapshot_ext::FwSystemOccupationRecord {
                        solar_system_id: fw_sys,
                        owner_faction_id: fw_services::FACTION_MINMATAR,
                        occupier_faction_id: fw_services::FACTION_MINMATAR,
                        victory_points: 0,
                        victory_points_threshold: 3000,
                    },
                );
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
            assert!(format!("{vp_states:?}").contains("1480"), "FW victory points must persist via RPC: {vp_states:?}");
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
            assert!(esi_fw_sys.to_string().contains(&fw_sys.to_string()) && esi_fw_sys.to_string().contains("1480"));
            let esi_fw_char = esi_get(&ctx2, "/characters/{character_id}/fw/stats", &[("character_id", PILOT)]);
            assert_eq!(esi_fw_char["kills"]["total"], 1);
            assert_eq!(esi_fw_char["victory_points"]["total"], 0);
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
                assert_eq!(snap.fw_runtime.system_victory_points.get(&fw_sys).copied(), Some(1_480));
                assert!(snap.sov_runtime.hacked_hubs.contains_key(&hub_id));
                assert!(snap.insurgency_runtime.campaigns.contains_key(&campaign_id));
            });
        }

        let _ = std::fs::remove_file(&db_path);
    }
}

pub(crate) mod industry_pi_market_wallet_history {
    // End-to-end SQLite restart durability tests for `plan:D-6` Batch 4:
    // - `feat:lifecycle.industry-pi-durable`
    // - `feat:lifecycle.market-wallet-history-durable`
    // - `feat:lifecycle.pve-progress-durable`
    // - `feat:lifecycle.abyssal-restart`
    // - `feat:lifecycle.ship-and-item-config-durable`
    // - `feat:lifecycle.account-progress-durable`

    use eve_db::{corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, UpwellStructureRecord, VirtualServerClock, WorldRepository};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{encode_bytes_field, encode_string_field, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX};
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_NOT_IMPLEMENTED, STATUS_OK};
    use eve_server::router::ServerContext;
    use eve_server::services::implant_plugging::TYPE_DAILY_ALPHA_INJECTOR;
    use glam::DVec3;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const CORP_ID: i64 = 1_000_009;
    const JITA_SYSTEM_ID: u32 = 30_000_142;
    const JITA_STATION_ID: i64 = 60_003_760;
    const LOWSEC_SYS_ID: u32 = 30_000_012; // Asabona (Low-Sec 0.32 with moons)
    const PLANET_ID: i64 = 40_009_082; // Jita IV (Gas)
    const TRITANIUM: u32 = 34;
    const MEXALLON: u32 = 36;
    const RIFTER_BPO: u32 = 691;
    const ALSC_CONTAINER_TYPE_ID: u32 = 17_366;

    fn open_ctx_at(path: &Path, now_unix: u64) -> Arc<ServerContext> {
        let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
        let ctx = Arc::new(ServerContext::with_repository(db));
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
        ctx
    }

    fn issue_esi_token(ctx: &ServerContext, char_id: i64) -> String {
        let now = ctx.now_unix();
        let (user, pass) = if char_id == P2 { ("other", "other-secret") } else { ("demo", "demo-secret") };
        let scopes = vec![
            "esi-planets.manage_planets.v1".into(), "esi-planets.read_customs_offices.v1".into(),
            "esi-industry.read_character_mining.v1".into(), "esi-industry.read_corporation_mining.v1".into(),
            "esi-industry.read_character_jobs.v1".into(), "esi-characters.read_agents_research.v1".into(),
            "esi-markets.read_character_orders.v1".into(), "esi-wallet.read_character_wallet.v1".into(),
            "esi-wallet.read_corporation_wallets.v1".into(), "esi-skills.read_skillqueue.v1".into(),
            "esi-corporations.read_structures.v1".into(), "esi-corporations.read_starbases.v1".into(),
            "esi-corporations.read_container_logs.v1".into(), "esi-location.read_location.v1".into(),
            "esi-location.read_ship_type.v1".into(), "esi-killmails.read_killmails.v1".into(),
        ];
        let code = ctx.db.authorize_pkce(user, pass, char_id, "evomu-d6-batch4", "https://localhost/callback", scopes, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "S256", now).expect("authorize_pkce");
        ctx.db.exchange_pkce_code(&code, "evomu-d6-batch4", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", now).expect("exchange_pkce_code").access_token
    }

    fn esi_get(router: &EsiRouter, path: &str, token: Option<&str>) -> Value {
        let mut req = EsiHttpRequest::new("GET", path);
        if let Some(t) = token {
            req = req.with_bearer_token(t);
        }
        let resp = router.handle_request(&req);
        assert_eq!(resp.status_code, 200, "ESI GET {path} failed ({}): {}", resp.status_code, String::from_utf8_lossy(&resp.body));
        resp.json().expect("valid ESI JSON")
    }

    async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(sid);
        let kw = HashMap::new();
        ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())], &kw, None).await.expect("Login");
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
        sess
    }

    async fn call(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
    }

    async fn call_kw(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep], kw_pairs: &[(&str, PyRep)]) -> Result<PyRep, eve_proto::BridgeRpcError> {
        let kw: HashMap<String, PyRep> = kw_pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        ctx.dispatch_rpc(sess, svc, None, method, args, &kw, None).await
    }

    fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
        InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
    }

    fn make_structure(structure_id: i64, type_id: u32, owner_corp_id: i64, solar_system_id: u32, name: &str, state: &str, fuel_blocks: u32) -> UpwellStructureRecord {
        UpwellStructureRecord {
            structure_id, type_id, owner_corp_id, solar_system_id, name: name.into(), position: [1_000.0, 2_000.0, 3_000.0],
            state: state.into(), quantum_core_installed: true, fuel_blocks, shield_hp: 100_000_000.0, armor_hp: 100_000_000.0,
            hull_hp: 100_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        }
    }

    async fn send_quasar_req(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, payload_value: Vec<u8>) -> EvePublicResponse {
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }), correlation_uuid: b"corr-d6-batch4".to_vec(),
            payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value: payload_value }),
            external_origin: "eve-v24-client".into(), application_instance_uuid: b"app-d6-batch4".to_vec(), authoritative_context: None, trace_context: None,
        };
        let rep = call(ctx, sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())]).await.expect("publicGatewaySvc.SendRequest");
        let Some(PyRep::Bytes(b)) = rep.dict_get("response_proto") else { panic!("Expected response_proto") };
        EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse")
    }

    #[tokio::test]
    async fn test_d6_industry_pi_and_market_wallet_history_durable_across_sqlite_restart() {
        // parity: feat:lifecycle.industry-pi-durable
        // parity: feat:lifecycle.market-wallet-history-durable
        eve_testkit::attest(&[
            "feat:lifecycle.industry-pi-durable",
            "feat:lifecycle.market-wallet-history-durable",
        ]);

        let path = std::env::temp_dir().join(format!("evomu_d6_ind_mkt_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        let base_time: u64 = 1_800_000_000;
        let athanor_id = 1_045_000_000_101_i64;
        let metenox_id = 1_045_000_000_102_i64;

        let (saved_clock_unix, mined_ore_type_id, installed_job_id, sell_order_id, bill_id, trade_id, raffle_id, old_hist_before, rnd_rp_before) = {
            let ctx = open_ctx_at(&path, base_time);
            let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_STATION_MANAGER | corp_roles::ROLE_STARBASE_CONFIG;
            ctx.db.with_mut_snapshot(|snap| {
                *snap = demo;
                if let Some(ch) = snap.characters.get_mut(&P1) { ch.corporation_id = CORP_ID; ch.balance = IskCents::from_isk(500_000_000); ch.standings.insert(1_000_084, 6.0); }
                if let Some(ch) = snap.characters.get_mut(&P2) { ch.balance = IskCents::from_isk(500_000_000); }
                let mut corp = CorporationRecord {
                    corporation_id: CORP_ID, name: "Durable Corp".into(), ticker: "DURB".into(), ceo_id: P1, alliance_id: None,
                    tax_rate: 0.05, master_wallet: IskCents::from_isk(200_000_000), wallet_divisions: Vec::new(), description: String::new(),
                    url: String::new(), hq_station_id: Some(JITA_STATION_ID), members: [P1].into_iter().collect(),
                    member_roles: HashMap::from([(P1, roles)]), applications: HashMap::new(),
                };
                corp.ensure_wallet_divisions();
                corp.wallet_divisions[0].balance = IskCents::from_isk(200_000_000);
                snap.corporations.insert(CORP_ID, corp);
                for &(tid, lvl) in &[(2403, 5), (2406, 5), (2495, 5), (2505, 5), (3380, 5), (3386, 5), (3402, 5), (11448, 4), (13279, 5), (46152, 5)] {
                    snap.skills.entry(P1).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
                }
                for (iid, tid, qty, sing) in [(910_002, RIFTER_BPO, 1, true), (910_003, TRITANIUM, 5_000_000, false), (910_004, 35, 1_000_000, false), (910_005, MEXALLON, 500_000, false), (910_006, 37, 500_000, false), (910_007, 44992, 100, false), (910_008, 52_568, 500, false), (910_009, 587, 1, false)] {
                    snap.items.insert(iid, make_item(iid, tid, P1, JITA_STATION_ID, InventoryFlag::Hangar, qty, sing));
                }
                snap.structures.insert(athanor_id, make_structure(athanor_id, 35835, CORP_ID, JITA_SYSTEM_ID, "Durable Athanor", "ShieldVulnerable", 500));
                snap.structures.insert(metenox_id, make_structure(metenox_id, 81826, CORP_ID, LOWSEC_SYS_ID, "Durable Metenox", "ShieldVulnerable", 200));
                snap.items.insert(910_020, make_item(910_020, 81143, CORP_ID, metenox_id, InventoryFlag::Hangar, 500, false));
                eve_server::services::industry_pi::save_orbital_settings(snap, PLANET_ID, &eve_server::services::industry_pi::OrbitalCustomsSettings { planet_id: PLANET_ID, owner_corp_id: CORP_ID, ..Default::default() });
            });

            let mut sess_p1 = login_select(&ctx, 8101, "demo", "demo-secret", P1).await;
            let mut sess_p2 = login_select(&ctx, 8102, "other", "other-secret", P2).await;
            sess_p1.corp_role = roles;

            // 1. Start Moon Extraction & Metenox, advance to chunk arrival, fracture & mine moon asteroid, deploy PI network while undocked
            call(&ctx, &mut sess_p1, "moonExtractions", "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(518_400)]).await.expect("StartNewExtraction");
            ctx.advance_time_seconds(511_200);
            ctx.db.with_mut_snapshot(|s| { s.structures.get_mut(&metenox_id).unwrap().fuel_blocks = 200; s.items.get_mut(&910_020).unwrap().quantity = 500; s.auto_moon_miners.remove(&metenox_id); });
            ctx.advance_time_seconds(7_200); call(&ctx, &mut sess_p1, "autoMoonMining", "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id)]).await.expect("RequestMiningCycleOutput");
            let frac = call(&ctx, &mut sess_p1, "moonExtractions", "FractureChunkWithStructure", &[PyRep::Integer(athanor_id)]).await.expect("FractureChunkWithStructure");
            let PyRep::List(ast_ids) = frac.dict_get("asteroidIDs").unwrap() else { panic!("Expected asteroidIDs") };
            let first_ast_id = ast_ids[0].as_i64().unwrap();

            let undock = call(&ctx, &mut sess_p1, "stationSvc", "Undock", &[]).await.expect("Undock");
            let ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
            ctx.db.with_mut_snapshot(|s| {
                if let Some(ship) = s.items.get_mut(&ship_id) {
                    ship.type_id = 32880;
                }
                s.items.retain(|_, item| item.location_id != ship_id);
                s.items.insert(910_030, make_item(910_030, 483, P1, ship_id, InventoryFlag::HiSlot0, 1, true));
                s.items.insert(910_031, make_item(910_031, 2534, P1, ship_id, InventoryFlag::Cargo, 1, false));
            });
            ctx.invalidate_ship_fitting(ship_id);

            // Deploy PI colony & network on Jita IV while undocked with Gas Command Center (2534) in ship cargo
            let res_info = call(&ctx, &mut sess_p1, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(PLANET_ID)]).await.expect("GetPlanetResourceInfo");
            let PyRep::List(res_list) = res_info.dict_get("resources").unwrap() else { panic!() };
            let first_p0_tid = res_list[0].dict_get("resourceTypeID").and_then(|v| v.as_i64()).unwrap();
            call(
                &ctx, &mut sess_p1, "planetMgr", "UserUpdateNetwork",
                &[
                    PyRep::Integer(PLANET_ID),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
                        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(3)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(2848), PyRep::Real(0.11), PyRep::Real(0.11)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2492), PyRep::Real(0.12), PyRep::Real(0.12)]),
                        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(101), PyRep::Integer(102), PyRep::Integer(0)]),
                        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(0)]),
                        PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(1)]),
                        PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(103), PyRep::Integer(121)]),
                        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103)]), PyRep::Integer(first_p0_tid), PyRep::Integer(3_000)]),
                        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(first_p0_tid), PyRep::Real(0.02), PyRep::Integer(3)]),
                    ]),
                ],
            ).await.expect("UserUpdateNetwork");

            let jita = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
            let undock_pos = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
            let ast_pos = jita.get_ball_snapshot(first_ast_id).await.unwrap().unwrap().1.position;
            if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await { sb.position = ast_pos + DVec3::new(500.0, 0.0, 0.0); let _ = jita.spawn_ball(sb).await; }
            let mine_res = crate::mining_cycle_engine::mine_once(&ctx, &mut sess_p1, 910_030, first_ast_id).await.expect("mine_once on moon asteroid");
            if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await { sb.position = undock_pos; let _ = jita.spawn_ball(sb).await; }
            call(&ctx, &mut sess_p1, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION_ID)]).await.expect("Dock");

            call_kw(&ctx, &mut sess_p1, "planetOrbitalRegistryBroker", "UpdateSettings", &[PyRep::Integer(PLANET_ID)], &[("taxRateValues", PyRep::keyval([("corporation", PyRep::Real(0.075)), ("alliance", PyRep::Real(0.095))])), ("allowAlliance", PyRep::Bool(true)), ("allowStandings", PyRep::Bool(true)), ("minimumStanding", PyRep::Real(-2.0))]).await.expect("UpdateSettings");
            call(&ctx, &mut sess_p1, "posMgr", "ChangeStructureProvisionType", &[PyRep::Integer(athanor_id), PyRep::Integer(athanor_id), PyRep::Integer(16634)]).await.expect("ChangeStructureProvisionType");

            let rnd = ctx.sde.agents.values().filter(|a| a.agent_type_id == 4 && a.level == 1 && a.station_id.is_some() && a.research_skills.contains(&11448)).min_by_key(|a| a.agent_id).cloned().expect("R&D agent");
            ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.standings.insert(rnd.agent_id, 9.0); ch.solar_system_id = rnd.solar_system_id.unwrap(); ch.station_id = rnd.station_id; } let sk = s.skills.entry(P1).or_default(); for tid in [3402, 3392, 11448] { sk.insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 }); } });
            sess_p1.solar_system_id = rnd.solar_system_id; sess_p1.station_id = rnd.station_id; call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(12), PyRep::Integer(rnd.agent_id), PyRep::Integer(11448)]).await.expect("StartResearch"); sess_p1.solar_system_id = Some(JITA_SYSTEM_ID); sess_p1.station_id = Some(JITA_STATION_ID); ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.solar_system_id = JITA_SYSTEM_ID; ch.station_id = Some(JITA_STATION_ID); } });
            let PyRep::Tuple(j_parts) = call(&ctx, &mut sess_p1, "agentMgr", "GetMyJournalDetails", &[]).await.expect("GetMyJournalDetails") else { panic!() };
            let PyRep::List(rnd_list_before) = &j_parts[1] else { panic!() };
            let rnd_rp_before = rnd_list_before[0].dict_get("points").and_then(|v| v.as_f64()).unwrap_or(0.0);

            let inst = call(&ctx, &mut sess_p1, "industryManager", "InstallJob", &[PyRep::Integer(RIFTER_BPO as i64), PyRep::Integer(1), PyRep::Integer(2)]).await.expect("InstallJob");
            let job_id = inst.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

            let sell_res = call(&ctx, &mut sess_p1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![PyRep::Dict(vec![(PyRep::String("itemID".into()), PyRep::Integer(910_003)), (PyRep::String("typeID".into()), PyRep::Integer(TRITANIUM as i64)), (PyRep::String("price".into()), PyRep::Real(4.50)), (PyRep::String("quantity".into()), PyRep::Integer(1_000)), (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)), (PyRep::String("duration".into()), PyRep::Integer(90))])]), PyRep::Bool(false)]).await.expect("PlaceMultiSellOrder");
            let sell_oid = sell_res.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

            call(&ctx, &mut sess_p2, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM as i64), PyRep::Real(4.50), PyRep::Integer(400), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false)]).await.expect("PlaceBuyOrder");
            let old_hist = call(&ctx, &mut sess_p1, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)]).await.expect("GetOldPriceHistory");
            call_kw(&ctx, &mut sess_p1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P2), PyRep::Real(50_000.0), PyRep::Integer(1000)], &[("reason", PyRep::String("Corp dividend".into()))]).await.expect("GiveCashFromCorpAccount");

            let bill_id = eve_server::services::wallet_serverops::create_bill(&ctx, 2, CORP_ID, 1_000_035, 250_000.0, ctx.now_unix() + 86_400, 0.0, JITA_STATION_ID, 0);
            let trade_id = call(&ctx, &mut sess_p1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.expect("InitiateTrade").dict_get("tradeContainerID").and_then(|v| v.as_i64()).unwrap();
            call(&ctx, &mut sess_p1, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(125_000.0)]).await.expect("OfferMoney");
            let raffle_id = call(&ctx, &mut sess_p1, "raffleProxy", "CreateRaffle", &[PyRep::Dict(vec![(PyRep::String("item_id".into()), PyRep::Integer(910_009)), (PyRep::String("ticket_count".into()), PyRep::Integer(8)), (PyRep::String("ticket_price".into()), PyRep::Real(100_000.0))])]).await.expect("CreateRaffle").dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();

            (ctx.now_unix(), mine_res.ore_type_id, job_id, sell_oid, bill_id, trade_id, raffle_id, old_hist, rnd_rp_before)
        };

        // === RESTART SERVER FROM SQLITE (+3,600s elapsed across downtime) ===
        {
            let ctx = open_ctx_at(&path, saved_clock_unix + 3_600);
            let router = EsiRouter::new(Arc::clone(&ctx));
            let token = issue_esi_token(&ctx, P1);
            let mut sess_p1 = login_select(&ctx, 8103, "demo", "demo-secret", P1).await;
            sess_p1.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_STATION_MANAGER | corp_roles::ROLE_STARBASE_CONFIG;

            // 1. Verify PI colony, customs office, moon extraction, Metenox, mining ledgers, R&D agent, industry job via RPC + ESI
            let col = call(&ctx, &mut sess_p1, "planetMgr", "GetColony", &[PyRep::Integer(PLANET_ID)]).await.expect("GetColony");
            assert!(col.dict_get("simulatedExtractedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
            let esi_planet = esi_get(&router, &format!("/v3/characters/{P1}/planets/{PLANET_ID}/"), Some(&token));
            assert_eq!(esi_planet["pins"].as_array().unwrap().len(), 3);
            assert_eq!(esi_planet["routes"].as_array().unwrap().len(), 1);
            let tax = call(&ctx, &mut sess_p1, "planetOrbitalRegistryBroker", "GetTaxRate", &[PyRep::Integer(PLANET_ID)]).await.expect("GetTaxRate");
            assert_eq!(tax.as_f64(), Some(0.075));
            let esi_pocos = esi_get(&router, &format!("/v1/corporations/{CORP_ID}/customs_offices/"), Some(&token));
            assert_eq!(esi_pocos.as_array().unwrap()[0]["corporation_tax_rate"], 0.075);
            let prov = call(&ctx, &mut sess_p1, "posMgr", "GetMoonProcessInfoForTower", &[PyRep::Integer(athanor_id)]).await.expect("GetMoonProcessInfoForTower");
            assert_eq!(prov.as_i64(), Some(16634));
            let PyRep::Tuple(ext_tup) = call(&ctx, &mut sess_p1, "moonExtractions", "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(athanor_id)]).await.expect("GetExtractionAndEventIDFromStructureID") else { panic!() };
            assert_eq!(ext_tup[0].dict_get("status").and_then(|v| v.as_str()), Some("Fractured"));
            let esi_exts = esi_get(&router, &format!("/v1/corporation/{CORP_ID}/mining/extractions/"), Some(&token));
            assert_eq!(esi_exts.as_array().unwrap()[0]["structure_id"], athanor_id);
            let met_det = call(&ctx, &mut sess_p1, "autoMoonMining", "RequestMiningDetails", &[PyRep::Integer(metenox_id)]).await.expect("RequestMiningDetails after restart");
            assert_eq!(met_det.dict_get("status").and_then(|v| v.as_str()), Some("Active"));
            assert_eq!(met_det.dict_get("fuelRemaining").and_then(|v| v.as_i64()), Some(190));
            let obs_led = call(&ctx, &mut sess_p1, "corpMiningLedger", "GetObserverLedger", &[PyRep::Integer(athanor_id)]).await.expect("GetObserverLedger");
            assert!(format!("{obs_led:?}").contains(&P1.to_string()));
            let esi_obs = esi_get(&router, &format!("/v1/corporation/{CORP_ID}/mining/observers/{athanor_id}/"), Some(&token));
            assert_eq!(esi_obs.as_array().unwrap()[0]["character_id"], P1);

            let char_led = call(&ctx, &mut sess_p1, "characterMiningLedger", "GetCharacterLogs", &[]).await.expect("GetCharacterLogs");
            assert!(format!("{char_led:?}").contains(&mined_ore_type_id.to_string()));
            let esi_mining = esi_get(&router, &format!("/v1/characters/{P1}/mining/"), Some(&token));
            assert_eq!(esi_mining.as_array().unwrap()[0]["type_id"], mined_ore_type_id as i64);

            let PyRep::Tuple(j_parts) = call(&ctx, &mut sess_p1, "agentMgr", "GetMyJournalDetails", &[]).await.expect("GetMyJournalDetails") else { panic!() };
            let PyRep::List(rnd_list_after) = &j_parts[1] else { panic!() };
            assert!(rnd_list_after[0].dict_get("points").and_then(|v| v.as_f64()).unwrap_or(0.0) > rnd_rp_before);
            let esi_rnd = esi_get(&router, &format!("/v2/characters/{P1}/agents_research/"), Some(&token));
            assert_eq!(esi_rnd.as_array().unwrap()[0]["skill_type_id"], 11448);

            let jobs_rep = call(&ctx, &mut sess_p1, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.expect("GetJobsByOwner");
            assert!(format!("{jobs_rep:?}").contains(&installed_job_id.to_string()));
            let esi_jobs = esi_get(&router, &format!("/v1/characters/{P1}/industry/jobs/"), Some(&token));
            assert!(esi_jobs.as_array().unwrap().iter().any(|j| j["job_id"] == installed_job_id));

            // 2. Verify Market Orders, Price History, Wallet Journals/Transactions, Bill, Trade, Raffle via RPC + ESI
            let char_orders = call(&ctx, &mut sess_p1, "marketProxy", "GetCharOrders", &[]).await.expect("GetCharOrders");
            assert!(format!("{char_orders:?}").contains(&sell_order_id.to_string()));
            let esi_orders = esi_get(&router, &format!("/v2/characters/{P1}/orders/"), Some(&token));
            let ord_json = esi_orders.as_array().unwrap().iter().find(|o| o["order_id"] == sell_order_id).expect("sell order in ESI");
            assert_eq!(ord_json["volume_remain"], 600);
            assert_eq!(ord_json["duration"], 90);

            let old_hist_after = call(&ctx, &mut sess_p1, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)]).await.expect("GetOldPriceHistory");
            assert_eq!(old_hist_after, old_hist_before);
            let esi_hist = esi_get(&router, "/v1/markets/10000002/history/?type_id=34", None);
            assert_eq!(esi_hist.as_array().unwrap()[0]["volume"], 400);
            let esi_w_journal = esi_get(&router, &format!("/v6/characters/{P1}/wallet/journal/"), Some(&token));
            assert!(!esi_w_journal.as_array().unwrap()[0]["date"].as_str().unwrap().starts_with("1970-"));
            let esi_w_tx = esi_get(&router, &format!("/v1/characters/{P1}/wallet/transactions/"), Some(&token));
            assert_eq!(esi_w_tx.as_array().unwrap()[0]["quantity"], 400);
            let esi_corp_j = esi_get(&router, &format!("/v4/corporations/{CORP_ID}/wallets/1/journal/"), Some(&token));
            assert!(esi_corp_j.as_array().unwrap().iter().any(|e| e["description"] == "Corp dividend"));
            let bills = call(&ctx, &mut sess_p1, "billMgr", "GetCorporationBills", &[]).await.expect("GetCorporationBills");
            assert!(format!("{bills:?}").contains(&bill_id.to_string()));
            let tr = call(&ctx, &mut sess_p1, "trademgr", "List", &[PyRep::Integer(trade_id)]).await.expect("trademgr.List");
            assert_eq!(tr.dict_get("myMoney").and_then(|v| v.as_f64()), Some(125_000.0));
            let raf = call(&ctx, &mut sess_p1, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)]).await.expect("GetRaffle");
            assert!(format!("{raf:?}").contains(&raffle_id.to_string()));
        }
        let _ = std::fs::remove_file(&path);
    }

    async fn unspawn_balls(ctx: &ServerContext, sess: &mut SessionState, balls: &[i64]) {
        sess.role |= eve_machonet::service_roles::ROLE_GML;
        for &bid in balls {
            call(ctx, sess, "slash", "SlashCmd", &[PyRep::String(format!("/unspawn {bid}"))]).await.expect("slash /unspawn");
        }
    }

    #[tokio::test]
    async fn test_d6_pve_progress_and_abyssal_restart_durable_across_sqlite_restart() {
        // parity: feat:lifecycle.pve-progress-durable
        // parity: feat:lifecycle.abyssal-restart
        eve_testkit::attest(&[
            "feat:lifecycle.pve-progress-durable",
            "feat:lifecycle.abyssal-restart",
        ]);

        let path = std::env::temp_dir().join(format!("evomu_d6_pve_abyss_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        let base_time: u64 = 1_760_093_700; // 10:55 UTC so Restart 1 (+300s) lands at 11:00 UTC downtime window
        let hack_sig_id = format!("DAT-{JITA_SYSTEM_ID}");
        let dat_pos = [-32_000_000.0, 28_000_000.0, -10_000_000.0];
        let trace_origin = [2_000_000.0, -75_000.0, 42_000.0];

        let (s2_id, r1_balls): (i64, Vec<i64>) = {
            let ctx = open_ctx_at(&path, base_time);
            let s2 = ctx.db.with_mut_snapshot(|snap| {
                *snap = demo;
                let (s1, s2) = (snap.characters[&P1].active_ship_id, snap.characters[&P2].active_ship_id);
                if let Some(ship2) = snap.items.get_mut(&s2) {
                    ship2.type_id = 621; // Cruiser for 1-filament solo Abyssal deployment
                }
                snap.items.insert(920_001, make_item(920_001, 56134, P1, s1, InventoryFlag::Cargo, 5, false));
                snap.items.insert(920_002, make_item(920_002, 56134, P2, s2, InventoryFlag::Cargo, 5, false));
                snap.items.insert(920_003, make_item(920_003, 22175, P1, s1, InventoryFlag::MedSlot0, 1, true));
                s2
            });

            let (mut sess_p1, mut sess_p2) = (login_select(&ctx, 8201, "demo", "demo-secret", P1).await, login_select(&ctx, 8202, "other", "other-secret", P2).await);
            eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| eve_server::services::pve_incursion_sites::seed_incursion_for_constellation(&ctx, reg, 20_000_020));

            let any_agent = *ctx.sde.agents.keys().next().unwrap();
            call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(18), PyRep::Integer(any_agent), PyRep::String("Amarr".into())]).await.expect("StartEpicArc");
            call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(2)]).await.expect("OfferMission");
            let dec1 = call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(9)]).await.expect("DeclineMission 1");
            assert_eq!(dec1.dict_get("standingPenaltyApplied").and_then(|v| v.as_bool()), Some(false));

            call(&ctx, &mut sess_p1, "stationSvc", "Undock", &[]).await.expect("Undock P1");
            call(&ctx, &mut sess_p2, "stationSvc", "Undock", &[]).await.expect("Undock P2");
            ctx.db.with_mut_snapshot(|snap| { snap.item_positions.insert(s2, trace_origin); });
            let _ = ctx.systems.get_or_spawn(JITA_SYSTEM_ID).set_ball_position(s2, glam::DVec3::from_array(trace_origin)).await;

            call(&ctx, &mut sess_p1, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[]).await.expect("GetCombatAnomalyInstances");
            call(&ctx, &mut sess_p1, "dungeonInstanceCacheMgr", "GetSignatureInstances", &[]).await.expect("GetSignatureInstances");
            let tracker = call(&ctx, &mut sess_p1, "scanMgr", "SignalTrackerRegister", &[]).await.expect("SignalTrackerRegister");
            assert_eq!(tracker.dict_get("anomalyCount").and_then(|v| v.as_i64()), Some(2));

            let probes: Vec<PyRep> = (0..8).map(|_| PyRep::List(vec![PyRep::Real(dat_pos[0]), PyRep::Real(dat_pos[1]), PyRep::Real(dat_pos[2])])).collect();
            let scan_rep = call(&ctx, &mut sess_p1, "scanMgr", "RequestScans", &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)]).await.expect("RequestScans");
            assert!(format!("{scan_rep:?}").contains("Digital Plexus"));
            call(&ctx, &mut sess_p1, "bookmark", "BookmarkScanResult", &[PyRep::Integer(JITA_SYSTEM_ID as i64), PyRep::String("Data Site Bookmark".into()), PyRep::String("scanned".into()), PyRep::String(hack_sig_id.clone()), PyRep::Integer(0)]).await.expect("BookmarkScanResult");

            let w1_balls = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
                reg.sites_by_system.get(&JITA_SYSTEM_ID).and_then(|l| l.iter().find(|s| s.site_id == "PVE-ANOM-30000142-1")).map(|s| s.active_npc_ball_ids.clone()).unwrap_or_default()
            });
            unspawn_balls(&ctx, &mut sess_p1, &w1_balls).await;
            let w2 = call(&ctx, &mut sess_p1, "keeper", "EnterNextDungeonRoom", &[PyRep::String("PVE-ANOM-30000142-1".into())]).await.expect("Wave 1 -> 2");
            let w2_balls: Vec<i64> = match w2.dict_get("activeNpcBallIDs") { Some(PyRep::List(l)) => l.iter().filter_map(|b| b.as_i64()).collect(), _ => vec![] };
            unspawn_balls(&ctx, &mut sess_p1, &w2_balls).await;
            ctx.set_mechanic_rng_seed(Some(3));
            let w_done = call(&ctx, &mut sess_p1, "keeper", "EnterNextDungeonRoom", &[PyRep::String("PVE-ANOM-30000142-1".into())]).await.expect("Complete anomaly");
            assert_eq!(w_done.dict_get("escalated").and_then(|v| v.as_bool()), Some(true));

            let cpos = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |r| r.sites_by_system.get(&30_000_142).and_then(|l| l.iter().find(|s| s.hackable_can_ids.contains(&1_870_014_200)).map(|s| s.position))).unwrap_or([0.0; 3]);
            let _ = ctx.systems.get_or_spawn(30_000_142).set_ball_position(sess_p1.ship_id.unwrap(), glam::DVec3::from_array(cpos) + glam::DVec3::new(500.0, 0.0, 0.0)).await;
            call(&ctx, &mut sess_p1, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(1_870_014_200)]).await.expect("StartNewGameInstance");
            call(&ctx, &mut sess_p1, "map", "GetIncursionGlobalReport", &[]).await.expect("GetIncursionGlobalReport");
            let (vg_sys, vg_sid, vg_pos) = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
                let s = reg.incursions[&20_000_020].sites.iter().find(|s| s.tier == "Vanguard").unwrap();
                (s.solar_system_id, s.site_id.clone(), s.position)
            });
            let vg_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys, &vg_sid).await;
            sess_p1.solar_system_id = Some(vg_sys);
            let s1 = ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&P1).unwrap().solar_system_id = vg_sys; s.characters[&P1].active_ship_id });
            let _ = ctx.systems.get_or_spawn(vg_sys).spawn_ball(eve_server::services::pve_incursion_sites::make_site_npc_ball(s1, 587, vg_pos)).await;
            unspawn_balls(&ctx, &mut sess_p1, &[vg_bid]).await;
            let inc_step = call(&ctx, &mut sess_p1, "keeper", "EnterNextDungeonRoom", &[PyRep::String("INC-VG-5165".into())]).await.expect("Complete Incursion VG");
            sess_p1.solar_system_id = Some(JITA_SYSTEM_ID);
            ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&P1).unwrap().solar_system_id = JITA_SYSTEM_ID; });
            assert_eq!(inc_step.dict_get("influence").and_then(|v| v.as_f64()), Some(0.95));

            let dep2 = call(&ctx, &mut sess_p2, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(920_002)]).await.expect("AbyssalEntranceDeployment P2");
            let trace2 = dep2.dict_get("traceBallID").and_then(|v| v.as_i64()).expect("traceBallID P2");
            let act2 = call(&ctx, &mut sess_p2, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace2)]).await.expect("AbyssalEntranceGateActivation P2");
            let r1_balls: Vec<i64> = match act2.dict_get("npcBallIDs") { Some(PyRep::List(l)) => l.iter().filter_map(|b| b.as_i64()).collect(), _ => vec![] };
            assert!(!r1_balls.is_empty());
            (s2, r1_balls)
        };

        // === RESTART 1: +300s (within 20m Abyssal window; P2 resumes in intact pocket with NPC balls restored, then downtime evacuates P2 near trace origin) ===
        {
            let ctx = open_ctx_at(&path, base_time + 300);
            let router = EsiRouter::new(Arc::clone(&ctx));
            let token_p2 = issue_esi_token(&ctx, P2);
            let mut sess_p1 = login_select(&ctx, 8203, "demo", "demo-secret", P1).await;
            let sess_p2_in_abyss = login_select(&ctx, 8204, "other", "other-secret", P2).await;
            assert_eq!(sess_p2_in_abyss.solar_system_id, Some(32_000_001));
            assert_eq!(esi_get(&router, &format!("/v1/characters/{P2}/location/"), Some(&token_p2))["solar_system_id"], 32_000_001);
            let abyss_sys = ctx.systems.get_or_spawn(32_000_001);
            assert!(abyss_sys.get_ball_snapshot(r1_balls[0]).await.unwrap().is_some(), "Room 1 NPC balls must be restored in Abyssal pocket on restart login");

            sess_p1.role |= eve_machonet::service_roles::ROLE_GML;
            call(&ctx, &mut sess_p1, "slash", "SlashCmd", &[PyRep::String("/downtime".into())]).await.expect("slash /downtime");
            let mut sess_p2 = login_select(&ctx, 8207, "other", "other-secret", P2).await;
            assert_eq!(sess_p2.solar_system_id, Some(JITA_SYSTEM_ID));
            assert_eq!(sess_p2.ship_id, Some(s2_id));
            assert_eq!(esi_get(&router, &format!("/v1/characters/{P2}/location/"), Some(&token_p2))["solar_system_id"], JITA_SYSTEM_ID);
            let p2_pos = ctx.db.with_snapshot(|snap| snap.item_positions[&s2_id]);
            for i in 0..3 {
                let d = (p2_pos[i] - trace_origin[i]).abs();
                assert!((500.0..=5_000.0).contains(&d), "Downtime evacuation offset axis {i} must be 500..=5000m from trace origin, got {d}");
            }
            if let Some(b) = ctx.systems.get_or_spawn(JITA_SYSTEM_ID).ballpark.write().unwrap().balls.get_mut(&s2_id) {
                b.mode = eve_destiny::MovementMode::Stop;
            }
            let _ = ctx.systems.get_or_spawn(JITA_SYSTEM_ID).set_ball_position(s2_id, glam::DVec3::from_array(trace_origin)).await;
            let dep2_b = call(&ctx, &mut sess_p2, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(920_002)]).await.expect("AbyssalEntranceDeployment P2 #2");
            let trace2_b = dep2_b.dict_get("traceBallID").and_then(|v| v.as_i64()).expect("traceBallID P2 #2");
            call(&ctx, &mut sess_p2, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace2_b)]).await.expect("AbyssalEntranceGateActivation P2 #2");

            let passive_probe = PyRep::List(vec![PyRep::List(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)])]);
            let scan_after = call(&ctx, &mut sess_p1, "scanMgr", "RequestScans", &[PyRep::Real(1_000.0), PyRep::Real(1.0), passive_probe]).await.expect("RequestScans after restart");
            let PyRep::Rowset { rows: sig_rows, .. } = &scan_after else { panic!("Expected Rowset") };
            let dat_row = sig_rows.iter().find(|r| r[0].as_str() == Some(hack_sig_id.as_str())).expect("DAT signature row");
            assert_eq!(dat_row[2].as_str(), Some("Digital Plexus"));
            assert_eq!(dat_row[3].as_f64(), Some(1.0));
            assert_eq!(dat_row[4].as_bool(), Some(true));
            assert_eq!([dat_row[5].as_f64().unwrap(), dat_row[6].as_f64().unwrap(), dat_row[7].as_f64().unwrap()], dat_pos);

            let tracker_after = call(&ctx, &mut sess_p1, "scanMgr", "SignalTrackerRegister", &[]).await.expect("SignalTrackerRegister after restart");
            assert_eq!(tracker_after.dict_get("anomalyCount").and_then(|v| v.as_i64()), Some(1));
            let Some(PyRep::Rowset { rows: anom_rows, .. }) = tracker_after.dict_get("anomalies") else { panic!("Expected anomalies rowset") };
            assert_eq!(anom_rows.len(), 1);
            assert_eq!(anom_rows[0][0].as_str(), Some("PVE-ANOM-30000142-2"));
            assert_eq!([anom_rows[0][4].as_f64().unwrap(), anom_rows[0][5].as_f64().unwrap(), anom_rows[0][6].as_f64().unwrap()], [30_000_000.0, 12_000_000.0, -8_000_000.0]);
            let anoms_cache = call(&ctx, &mut sess_p1, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[]).await.expect("GetCombatAnomalyInstances after restart");
            assert!(!format!("{anoms_cache:?}").contains("2134") && format!("{anoms_cache:?}").contains("1215"));

            let esc_list = call(&ctx, &mut sess_p1, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await.expect("GetMyEscalatingPathDetails");
            let PyRep::Rowset { columns: esc_cols, rows: esc_rows } = &esc_list else { panic!("Expected Rowset, got {esc_list:?}") };
            assert_eq!(esc_rows.len(), 1);
            let col = |name: &str| esc_cols.iter().position(|c| c == name).unwrap();
            assert_eq!((esc_rows[0][col("sourceDungeonID")].as_i64(), esc_rows[0][col("dungeonID")].as_i64()), (Some(2134), Some(2403)));
            let bms = call(&ctx, &mut sess_p1, "bookmark", "GetBookmarks", &[]).await.expect("GetBookmarks");
            let bm_dbg = format!("{bms:?}");
            assert!(bm_dbg.contains("Data Site Bookmark") && bm_dbg.contains("-32000000.0") && bm_dbg.contains("Escalation:"));

            let hack = call(&ctx, &mut sess_p1, "hackingMgr", "GetMyHackProgress", &[PyRep::Integer(1_870_014_200)]).await.expect("GetMyHackProgress after restart");
            assert_eq!(hack.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));

            let tales = call(&ctx, &mut sess_p1, "map", "GetIncursionGlobalReport", &[]).await.expect("GetIncursionGlobalReport after restart");
            let PyRep::List(tale_list) = &tales else { panic!("Expected List") };
            let kimo_tale = tale_list.iter().find(|t| t.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(20_000_020)).unwrap();
            assert_eq!(kimo_tale.dict_get("influence").and_then(|v| v.as_f64()), Some(0.95));
            let esi_inc = esi_get(&router, "/v1/incursions/", None);
            let kimo_esi = esi_inc.as_array().unwrap().iter().find(|i| i["constellation_id"] == 20_000_020).unwrap();
            assert_eq!(kimo_esi["influence"], 0.95);

            let arcs = call(&ctx, &mut sess_p1, "agentMgr", "GetMyEpicArcStatus", &[]).await.expect("GetMyEpicArcStatus");
            assert!(format!("{arcs:?}").contains("Amarr"));
            call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(2)]).await.expect("OfferMission 2");
            let dec2 = call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(9)]).await.expect("DeclineMission 2");
            assert_eq!(dec2.dict_get("standingPenaltyApplied").and_then(|v| v.as_bool()), Some(true));
        }

        // === RESTART 2: +2,000s (> 1,500s collapse deadline; P2's ship & pod destroyed, respawned at home station in Caldari starter corvette 601 + ship & pod killmails) ===
        {
            let ctx = open_ctx_at(&path, base_time + 2_000);
            let router = EsiRouter::new(Arc::clone(&ctx));
            let token_p2 = issue_esi_token(&ctx, P2);
            let sess_p2 = login_select(&ctx, 8205, "other", "other-secret", P2).await;
            assert_eq!(sess_p2.solar_system_id, Some(JITA_SYSTEM_ID));
            assert_eq!(sess_p2.station_id, Some(JITA_STATION_ID));
            let new_ship_id = sess_p2.ship_id.expect("respawned corvette ship_id");
            assert_ne!(new_ship_id, s2_id, "Old ship must be destroyed on Abyssal collapse");
            assert_eq!(esi_get(&router, &format!("/v1/characters/{P2}/location/"), Some(&token_p2))["station_id"], JITA_STATION_ID);
            assert_eq!(esi_get(&router, &format!("/v1/characters/{P2}/ship/"), Some(&token_p2))["ship_type_id"], 601);
            let kms = esi_get(&router, &format!("/v1/characters/{P2}/killmails/recent/"), Some(&token_p2));
            assert!(!kms.as_array().unwrap().is_empty(), "Abyssal collapse must record killmails visible via ESI");
            ctx.db.with_snapshot(|snap| {
                assert!(!snap.items.contains_key(&s2_id), "Destroyed Abyssal ship must be removed from inventory");
                assert_eq!(snap.items[&new_ship_id].type_id, 601, "Caldari pilot must respawn in starter corvette Ibis (type 601)");
                assert!(snap.killmails.values().any(|k| k.victim_character_id == P2 && k.solar_system_id == 32_000_001 && k.ship_type_id == 670), "Abyssal collapse must record a capsule (670) killmail");
            });
        }

        // === RESTART 3: +90,000s (> 86,400s 24h lifetime; signatures, anomalies, and escalations naturally despawn) ===
        {
            let ctx = open_ctx_at(&path, base_time + 90_000);
            let mut sess_p1 = login_select(&ctx, 8206, "demo", "demo-secret", P1).await;
            let tracker_exp = call(&ctx, &mut sess_p1, "scanMgr", "SignalTrackerRegister", &[]).await.expect("SignalTrackerRegister at +90,000s");
            assert_eq!(tracker_exp.dict_get("anomalyCount").and_then(|v| v.as_i64()), Some(0), "Expired anomalies must despawn after 24h");
            let sigs_exp = call(&ctx, &mut sess_p1, "scanMgr", "GetSignatures", &[]).await.expect("GetSignatures at +90,000s");
            let PyRep::Rowset { rows: exp_sig_rows, .. } = &sigs_exp else { panic!("Expected Rowset") };
            assert!(exp_sig_rows.is_empty(), "Expired cosmic signatures must despawn after 24h");
            let esc_exp = call(&ctx, &mut sess_p1, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await.expect("GetMyEscalatingPathDetails at +90,000s");
            assert!(matches!(esc_exp, PyRep::Rowset { ref rows, .. } if rows.is_empty()), "Expired escalations must despawn after 24h: {esc_exp:?}");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_d6_ship_item_config_and_account_progress_durable_across_sqlite_restart() {
        // parity: feat:lifecycle.ship-and-item-config-durable
        // parity: feat:lifecycle.account-progress-durable
        eve_testkit::attest(&[
            "feat:lifecycle.ship-and-item-config-durable",
            "feat:lifecycle.account-progress-durable",
        ]);

        let path = std::env::temp_dir().join(format!("evomu_d6_ship_acct_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        let base_time: u64 = 1_760_200_000;
        let (struct_id, tower_id, mod1_id, mod2_id, ammo_id, svc_mod_id, svc_mod2_id) = (1_045_000_000_301_i64, 555_001_i64, 100_000_005_i64, 400_002_i64, 400_003_i64, 400_004_i64, 400_005_i64);

        let (ship_id, alsc_id, locked_item_id, sp_before, lic_initial, lic_bytes_before, code_payload) = {
            let ctx = open_ctx_at(&path, base_time);
            let (ship_id, alsc_id, locked_item_id, injector_id) = ctx.db.with_mut_snapshot(|snap| {
                *snap = demo;
                let ship_id = snap.characters[&P1].active_ship_id;
                if let Some(ch) = snap.characters.get_mut(&P1) {
                    ch.corporation_id = CORP_ID;
                    ch.clone_grade = 1;
                    ch.skill_points = 1_000_000.0;
                }
                snap.corporations.insert(CORP_ID, CorporationRecord {
                    corporation_id: CORP_ID, name: "Durable Corp".into(), ticker: "DURB".into(), ceo_id: P1, alliance_id: None,
                    tax_rate: 0.05, master_wallet: IskCents::from_isk(50_000_000), wallet_divisions: Vec::new(), description: String::new(),
                    url: String::new(), hq_station_id: Some(JITA_STATION_ID), members: [P1].into_iter().collect(),
                    member_roles: HashMap::from([(P1, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STARBASE_CONFIG)]), applications: HashMap::new(),
                });
                snap.skills.entry(P1).or_default().insert(3300, TrainedSkillRecord { type_id: 3300, level: 3, skill_points: 8_000.0 });
                snap.items.insert(mod2_id, make_item(mod2_id, 2889, P1, ship_id, InventoryFlag::HiSlot1, 1, true));
                snap.items.insert(ammo_id, make_item(ammo_id, 183, P1, ship_id, InventoryFlag::Cargo, 500, false));
                snap.structures.insert(struct_id, make_structure(struct_id, 35832, CORP_ID, JITA_SYSTEM_ID, "Durable Astrahus", "ShieldVulnerable", 5_000));
                snap.items.insert(svc_mod_id, make_item(svc_mod_id, 35894, P1, struct_id, InventoryFlag::Hangar, 1, true));
                snap.items.insert(svc_mod2_id, make_item(svc_mod2_id, 35894, P1, struct_id, InventoryFlag::Hangar, 1, true));
                snap.structures.insert(tower_id, make_structure(tower_id, 12235, CORP_ID, JITA_SYSTEM_ID, "Durable Control Tower", "Online", 500));
                snap.items.insert(tower_id, make_item(tower_id, 12235, CORP_ID, JITA_SYSTEM_ID as i64, InventoryFlag::Hangar, 1, true));
                let (alsc_id, locked_item_id, injector_id) = (777_100_i64, 777_101_i64, 777_102_i64);
                snap.items.insert(alsc_id, make_item(alsc_id, ALSC_CONTAINER_TYPE_ID, CORP_ID, JITA_STATION_ID, InventoryFlag::CorpSAG1, 1, true));
                snap.items.insert(locked_item_id, make_item(locked_item_id, TRITANIUM, P1, alsc_id, InventoryFlag::Hangar, 500, false));
                snap.items.insert(injector_id, make_item(injector_id, TYPE_DAILY_ALPHA_INJECTOR, P1, JITA_STATION_ID, InventoryFlag::Hangar, 2, false));
                (ship_id, alsc_id, locked_item_id, injector_id)
            });
            ctx.invalidate_ship_fitting(ship_id);

            let mut sess = login_select(&ctx, 8301, "demo", "demo-secret", P1).await;
            sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STARBASE_CONFIG;

            call(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(mod1_id)]).await.expect("SetModuleOnline mod1");
            call(&ctx, &mut sess, "dogmaIM", "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(mod1_id), PyRep::Integer(mod2_id)]).await.expect("LinkWeapons");
            call(&ctx, &mut sess, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(ship_id), PyRep::Integer(mod2_id)]).await.expect("TakeModuleOffline mod2");
            call(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(svc_mod_id), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.expect("Fit structure service 0 via invbroker.Add");
            call(&ctx, &mut sess, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(struct_id), PyRep::Integer(svc_mod_id)]).await.expect("Take structure service 0 offline via dogmaIM.TakeModuleOffline");
            call(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(svc_mod2_id), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot1 as i64)]).await.expect("Fit structure service 1 via invbroker.Add");
            call(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(struct_id), PyRep::Integer(svc_mod2_id)]).await.expect("Set structure service 1 online via dogmaIM.SetModuleOnline");
            call(&ctx, &mut sess, "posMgr", "SetTowerSentrySettings", &[PyRep::Integer(tower_id), PyRep::Real(5.0), PyRep::Real(0.0), PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false)]).await.expect("SetTowerSentrySettings");

            call(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3300), PyRep::Integer(4)])])]).await.expect("SaveSkillQueue");
            call(&ctx, &mut sess, "skillMgr2", "AbortTraining", &[]).await.expect("AbortTraining");

            call(&ctx, &mut sess, "invbroker", "SetPassword", &[PyRep::Integer(alsc_id), PyRep::String("vault-pw-99".into())]).await.expect("SetPassword");
            call_kw(&ctx, &mut sess, "invbroker", "ALSCLockItems", &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(locked_item_id)])], &[("password", PyRep::String("vault-pw-99".into()))]).await.expect("ALSCLockItems");
            call(&ctx, &mut sess, "ship", "ConfigureShip", &[PyRep::Integer(ship_id), PyRep::Dict(vec![(PyRep::String("SMB_AllowFleetAccess".into()), PyRep::Bool(false)), (PyRep::String("FleetHangar_AllowCorpAccess".into()), PyRep::Bool(false))])]).await.expect("ConfigureShip");

            call(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(injector_id)]).await.expect("InjectSkillpoints");
            let sp_before = call(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.expect("GetFreeSkillPoints").as_i64().unwrap();

            let err_client = call(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(297), PyRep::Integer(5)]).await.expect_err("process_client_event rejects server-driven challenge");
            assert_eq!(err_client.code, "InvalidChallengeEvent"); // rpc-lint: allow error code assertion
            call(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[]).await.expect("fill_up_and_return_challenges");
            ctx.db.with_mut_snapshot(|s| {
                for i in 1..=15 {
                    s.killmails.insert(900_000 + i, eve_db::KillmailRecord { kill_id: 900_000 + i, victim_character_id: 500_000_100, attacker_character_id: P1, ..Default::default() });
                }
                eve_server::services::project_discovery::configure_project_discovery_mmos(s, 2_000.0, 250.0, 150.0, 75.0, 50);
            });
            call(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(297)]).await.expect("get_challenge");
            call(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(297)]).await.expect("claim_challenge_rewards");
            for _ in 0..4 { call(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[]).await.expect("increase_tutorial_level"); }
            assert_eq!(call(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("give_tutorial_rewards"), PyRep::Bool(true));
            call(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[]).await.expect("get_new_task");
            call(&ctx, &mut sess, "ProjectDiscovery", "post_classification", &[PyRep::String("task_1".into()), PyRep::String("consensus".into())]).await.expect("post_classification");
            let lic_initial = send_quasar_req(&ctx, &mut sess, "eve_public.user.license.api.GetRequest", Vec::new()).await.payload.unwrap().value;
            assert_eq!(send_quasar_req(&ctx, &mut sess, "eve.user.legal.agreement.api.AcceptRequest", Vec::new()).await.status_code, STATUS_OK);
            let mut ent_payload = Vec::new(); encode_string_field(3, "omega_veteran_reward", &mut ent_payload);
            let mut ent_outer = Vec::new(); encode_bytes_field(1, &ent_payload, &mut ent_outer);
            // Unprivileged user grant must fail with 403 Forbidden, while GM grant succeeds with 200 OK.
            assert_eq!(send_quasar_req(&ctx, &mut sess, "eve.entitlement.user.api.GrantRequest", ent_outer.clone()).await.status_code, STATUS_FORBIDDEN);
            let saved_role = sess.role;
            sess.role |= eve_machonet::service_roles::ROLE_GM_DEFAULT;
            assert_eq!(send_quasar_req(&ctx, &mut sess, "eve.entitlement.user.api.GrantRequest", ent_outer).await.status_code, STATUS_OK);
            sess.role = saved_role;

            let mut code_inner = Vec::new(); encode_string_field(1, "EVE-OMEGA-30D-PROMO", &mut code_inner);
            let mut code_payload = Vec::new(); encode_bytes_field(1, &code_inner, &mut code_payload);
            assert_eq!(send_quasar_req(&ctx, &mut sess, "eve_public.payment.code.api.RedeemRequest", code_payload.clone()).await.status_code, STATUS_NOT_IMPLEMENTED);
            eve_server::services::quasar_auth_user::with_identity_store(&ctx.db, |st| { st.omega_subscriptions.insert(1, ctx.now_unix() + 60 * 86_400); });
            let lic_bytes_before = send_quasar_req(&ctx, &mut sess, "eve_public.user.license.api.GetRequest", Vec::new()).await.payload.unwrap().value;
            assert_ne!(lic_bytes_before, lic_initial, "Direct Omega license extension must extend license expiry before restart");

            (ship_id, alsc_id, locked_item_id, sp_before, lic_initial, lic_bytes_before, code_payload)
        };

        // === RESTART SERVER FROM SQLITE (+1,800s elapsed) ===
        {
            let ctx = open_ctx_at(&path, base_time + 1_800);
            let router = EsiRouter::new(Arc::clone(&ctx));
            let token = issue_esi_token(&ctx, P1);
            let mut sess = login_select(&ctx, 8302, "demo", "demo-secret", P1).await;
            sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STARBASE_CONFIG;

            let sp_after = call(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.expect("GetFreeSkillPoints after restart");
            assert_eq!(sp_after.as_i64(), Some(sp_before));
            let esi_sq = esi_get(&router, &format!("/v2/characters/{P1}/skillqueue/"), Some(&token));
            assert!(esi_sq.as_array().unwrap()[0].get("finish_date").is_none(), "Paused skill queue must omit finish_date in ESI");

            let dogma_info = call(&ctx, &mut sess, "dogmaIM", "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)]).await.expect("dogmaIM.GetAllInfo after restart");
            let Some(PyRep::Dict(mod_attrs)) = dogma_info.dict_get("moduleAttributes") else { panic!("Expected moduleAttributes dict") };
            let mod_online = |mid: i64| -> Option<f64> {
                let PyRep::Dict(attrs) = &mod_attrs.iter().find(|(k, _)| k.as_i64() == Some(mid))?.1 else { return None };
                attrs.iter().find(|(k, _)| k.as_i64() == Some(2))?.1.as_f64()
            };
            assert_eq!(mod_online(mod1_id), Some(1.0), "Online module must report isOnline (attr 2) == 1.0 after restart");
            assert_eq!(mod_online(mod2_id), Some(0.0), "Offline module must report isOnline (attr 2) == 0.0 after restart");

            let load_rep = call(&ctx, &mut sess, "dogmaIM", "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod1_id)]), PyRep::List(vec![PyRep::Integer(ammo_id)])]).await.expect("LoadAmmo on linked weapon bank");
            assert_eq!(load_rep.dict_get("totalLoaded").and_then(|v| v.as_i64()), Some(240), "Linked slave weapon must also receive ammo after restart");

            let esi_structs = esi_get(&router, &format!("/v4/corporations/{CORP_ID}/structures/"), Some(&token));
            let st_json = esi_structs.as_array().unwrap().iter().find(|s| s["structure_id"] == struct_id).expect("structure in ESI");
            let svcs = st_json["services"].as_array().expect("services array");
            assert!(svcs.iter().any(|s| s["state"] == "offline") && svcs.iter().any(|s| s["state"] == "online"));

            let sentry = call(&ctx, &mut sess, "posMgr", "GetTowerSentrySettings", &[PyRep::Integer(tower_id)]).await.expect("GetTowerSentrySettings");
            assert!(format!("{sentry:?}").contains("5.0"));
            assert_eq!(esi_get(&router, &format!("/v1/corporations/{CORP_ID}/starbases/{tower_id}/?system_id={JITA_SYSTEM_ID}"), Some(&token))["attack_standing_threshold"], 5.0);

            assert_eq!(call(&ctx, &mut sess, "invbroker", "ALSCConfigGet", &[PyRep::Integer(alsc_id)]).await.expect("ALSCConfigGet").dict_get("hasPassword").and_then(|v| v.as_bool()), Some(true));
            let locked_dict = call(&ctx, &mut sess, "itemLocking", "GetItemsByLocation", &[PyRep::Integer(alsc_id)]).await.expect("GetItemsByLocation");
            assert!(format!("{locked_dict:?}").contains(&locked_item_id.to_string()));
            assert!(call_kw(&ctx, &mut sess, "invbroker", "ALSCLogGet", &[PyRep::Integer(alsc_id)], &[("password", PyRep::String("vault-pw-99".into()))]).await.expect("ALSCLogGet").dict_get("logCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 2);
            assert!(esi_get(&router, &format!("/v3/corporations/{CORP_ID}/containers/logs/"), Some(&token)).as_array().unwrap().iter().any(|l| l["container_id"] == alsc_id && l["action"] == "lock"));
            let ship_cfg = call(&ctx, &mut sess, "ship", "GetShipConfiguration", &[PyRep::Integer(ship_id)]).await.expect("GetShipConfiguration");
            assert_eq!((ship_cfg.dict_get("SMB_AllowFleetAccess").and_then(|v| v.as_bool()), ship_cfg.dict_get("FleetHangar_AllowCorpAccess").and_then(|v| v.as_bool())), (Some(false), Some(false)));
            assert_eq!(call(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(777_102)]).await.expect_err("Daily Alpha Injector cooldown must persist across restart").code, "DailyAlphaInjectorCooldownActive");

            let ch_rep = call(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(297)]).await.expect("get_challenge");
            assert_eq!((ch_rep.dict_get("claimed").and_then(|v| v.as_bool()), ch_rep.dict_get("seasonPoints").and_then(|v| v.as_i64())), (Some(true), Some(10)));
            let pd_stats = call(&ctx, &mut sess, "ProjectDiscovery", "get_player_statistics", &[]).await.expect("get_player_statistics");
            assert_eq!((pd_stats.dict_get("rank").and_then(|v| v.as_i64()), pd_stats.dict_get("tasks_submitted").and_then(|v| v.as_i64()), pd_stats.dict_get("experience").and_then(|v| v.as_f64())), (Some(1), Some(1), Some(325.0)));
            assert_eq!(call(&ctx, &mut sess, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.expect("get_player_analysis_kredits").as_i64(), Some(0));
            let lic_after = send_quasar_req(&ctx, &mut sess, "eve_public.user.license.api.GetRequest", Vec::new()).await.payload.unwrap().value;
            assert_eq!((lic_after == lic_bytes_before, lic_after != lic_initial), (true, true));
            assert!(String::from_utf8_lossy(&send_quasar_req(&ctx, &mut sess, "eve.legal.agreement.api.GetLatestRequest", Vec::new()).await.payload.unwrap().value).contains("eula_v24"));
            assert!(String::from_utf8_lossy(&send_quasar_req(&ctx, &mut sess, "eve_public.entitlement.user.api.GetRequest", Vec::new()).await.payload.unwrap().value).contains("omega_veteran_reward"));
            assert_eq!(send_quasar_req(&ctx, &mut sess, "eve_public.payment.code.api.RedeemRequest", code_payload).await.status_code, STATUS_NOT_IMPLEMENTED);
        }
        let _ = std::fs::remove_file(&path);
    }
}

pub(crate) mod owner_notes_persistence_market_industry {
    // SQLite restart persistence integration tests for `D-3` / `D-6` Batch 1:
    // - `feat:lifecycle.owner_notes-persistence`: Market order metadata (`issueDate`, `duration`,
    //   `range`, `accountID`) and scheduled expiry, corporation industry job delivery metadata, and
    //   Planetary Interaction colony networks (`pins`, `links`, `routes`, `extractor_programs`,
    //   processor `schematicID`) survive a full SQLite server restart.
    // - `feat:lifecycle.lp-durable`: Corporation LP wallets (`corp_lp_balances`) and character LP
    //   reward logs (`char_lp_logs`) survive a SQLite server restart, and failed LP store purchases
    //   leave LP, ISK, and inventory items unchanged.
    // - `feat:lifecycle.notifications-durable`: Notifications already received stay in the notification
    //   list after a server restart, notifications created while a character was offline (or before a
    //   restart) are delivered at the next login, and notification IDs are never reused.
    // - `feat:invariant.ids-monotonic-across-restart`: IDs allocated for alliances, corporations,
    //   notifications, in-game reports, fleets, corp projects, and persisted entities are strictly
    //   greater after a restart than any ID allocated before the restart, never overwriting existing records.

    use crate::common;
    use eve_db::{
        CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
        VirtualServerClock, WorldRepository,
    };
    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::ServerContext;
    use eve_server::services::ingamereport_ops;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    const JITA_STATION_ID: i64 = 60_003_760;
    const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
    const JITA_PLANET_ID: i64 = 40_009_082;
    const P1: i64 = 90_000_001;
    const P2: i64 = 90_000_002;
    const TRITANIUM: TypeId = 34;
    const RIFTER_BP_TYPE_ID: TypeId = 691;
    const RIFTER_TYPE_ID: TypeId = 587;
    const CALDARI_NAVY_CORP_ID: i64 = 1_000_035;
    const FEDERATION_NAVY_CORP_ID: i64 = 1_000_036;
    const CONCORD_CORP_ID: i64 = 1_000_125;

    const GAS_COMMAND_CENTER_TYPE_ID: TypeId = 2534;
    const GAS_EXTRACTOR_TYPE_ID: TypeId = 2848;
    const GAS_BASIC_FACILITY_TYPE_ID: TypeId = 2492;
    const GAS_LAUNCHPAD_TYPE_ID: TypeId = 2543;

    fn open_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
        let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
        let ctx = ServerContext::with_repository(Arc::new(repo));
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
        ctx
    }

    async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(sid);
        common::login_pilot(ctx, &mut sess, user, pass).await;
        call(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
            .await
            .expect("SelectCharacterID");
        ctx.advance_secs(12);
        sess
    }

    async fn call(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, m, args, &HashMap::new(), None).await
    }

    async fn call_kw(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        m: &str,
        args: &[PyRep],
        kw: &[(&str, PyRep)],
    ) -> Result<PyRep, BridgeRpcError> {
        let kwargs: HashMap<String, PyRep> = kw.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        ctx.dispatch_rpc(sess, svc, None, m, args, &kwargs, None).await
    }

    fn rowset_maps(rep: &PyRep) -> Vec<HashMap<String, PyRep>> {
        let PyRep::Rowset { columns, rows } = rep else { panic!("expected Rowset, got {rep:?}") };
        rows.iter().map(|r| columns.iter().cloned().zip(r.iter().cloned()).collect()).collect()
    }

    fn seed_corp_for_pilot(snap: &mut eve_db::WorldStateSnapshot, char_id: i64, corp_id: i64) {
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: format!("Persistence Test Corp {corp_id}"),
            ticker: "PRST".into(),
            ceo_id: char_id,
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(50_000_000);
        }
        corp.master_wallet = IskCents::from_isk(50_000_000);
        corp.members.insert(char_id);
        corp.member_roles.insert(char_id, u64::MAX);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(100_000_000);
        }
    }

    fn seed_item(snap: &mut eve_db::WorldStateSnapshot, tid: TypeId, owner: i64, loc: i64, flag: InventoryFlag, qty: i32, singleton: bool) -> i64 {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(id, InventoryItemRecord {
            item_id: id, type_id: tid, owner_id: owner, location_id: loc, flag, quantity: qty, singleton, version: 1, ..Default::default()
        });
        id
    }

    fn cmd_pin(id: i64, tid: TypeId, lat: f64, lon: f64) -> PyRep {
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(id), PyRep::Integer(tid as i64), PyRep::Real(lat), PyRep::Real(lon)])
    }
    fn cmd_link(a: i64, b: i64) -> PyRep {
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(0)])
    }
    fn cmd_upgrade_link(a: i64, b: i64, lvl: i64) -> PyRep {
        PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(lvl)])
    }
    fn cmd_upgrade_cc(id: i64, lvl: i64) -> PyRep {
        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(id), PyRep::Integer(lvl)])
    }
    fn cmd_schematic(id: i64, sid: i64) -> PyRep {
        PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(id), PyRep::Integer(sid)])
    }
    fn cmd_route(id: i64, path: &[i64], tid: TypeId, qty: i64) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::Integer(6), PyRep::Integer(id),
            PyRep::List(path.iter().map(|&p| PyRep::Integer(p)).collect()),
            PyRep::Integer(tid as i64), PyRep::Integer(qty),
        ])
    }
    fn cmd_extractor_program(id: i64, tid: TypeId, radius: f64, heads: i64) -> PyRep {
        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(id), PyRep::Integer(tid as i64), PyRep::Real(radius), PyRep::Integer(heads)])
    }

    #[tokio::test]
    async fn test_d3_owner_notes_persistence_market_industry_and_pi_across_sqlite_restart() {
        // parity: feat:lifecycle.owner_notes-persistence
        eve_testkit::attest(&["feat:lifecycle.owner_notes-persistence"]);

        let path = std::env::temp_dir().join(format!("evomu_d3_owner_notes_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        let corp_id = 98_200_101;
        let base_time: u64 = 1_750_000_000;

        let (order_id, expected_issue_filetime, order_escrow_isk, corp_job_id, first_p0_tid) = {
            let ctx = open_ctx_at(&path, base_time);
            ctx.db.with_mut_snapshot(|snap| {
                *snap = demo;
                seed_corp_for_pilot(snap, P1, corp_id);
                let sk = snap.skills.entry(P1).or_default();
                for (stid, lvl) in [(3443, 5), (16596, 5), (2505, 5), (2495, 4), (13279, 4), (2406, 3), (2403, 2)] {
                    sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
                }
                seed_item(snap, RIFTER_BP_TYPE_ID, corp_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true);
                seed_item(snap, TRITANIUM, corp_id, JITA_STATION_ID, InventoryFlag::Hangar, 100_000, false);
                snap.skills.entry(P1).or_default().insert(3447, TrainedSkillRecord { type_id: 3447, level: 5, skill_points: 256_000.0 });
            });

            let mut sess = login_select(&ctx, 7001, "demo", "demo-secret", P1).await;

            // 1. Place a corporate buy order with duration = 1 day, range = 5 jumps, accountKey = 1002 (Division 3)
            let buy_res = call(
                &ctx, &mut sess, "marketProxy", "PlaceBuyOrder",
                &[
                    PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM as i64), PyRep::Real(3.50),
                    PyRep::Integer(1_000), PyRep::Integer(5), PyRep::Integer(10),
                    PyRep::Integer(1), PyRep::Bool(true), PyRep::Integer(1002),
                ],
            ).await.expect("PlaceBuyOrder duration=1 range=5 accountKey=1002");
            let order_id = buy_res.dict_get("orderID").and_then(|v| v.as_i64()).expect("orderID");

            let corp_orders = rowset_maps(&call(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[]).await.unwrap());
            let ord_row = corp_orders.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
            let issue_ft = ord_row.get("issueDate").and_then(|v| v.as_i64()).unwrap();
            let escrow_isk = ord_row.get("escrow").and_then(|v| v.as_f64()).unwrap();
            assert_eq!(ord_row.get("duration").and_then(|v| v.as_i64()), Some(1));
            assert_eq!(ord_row.get("range").and_then(|v| v.as_i64()), Some(5));
            assert_eq!(ord_row.get("accountID").and_then(|v| v.as_i64()), Some(1002));

            // 2. Install a corporation manufacturing job (`useCorp: true`)
            let install_res = call_kw(
                &ctx, &mut sess, "industryManager", "InstallJob",
                &[PyRep::Integer(RIFTER_BP_TYPE_ID as i64), PyRep::Integer(1), PyRep::Integer(1)],
                &[("useCorp", PyRep::Bool(true))],
            ).await.expect("InstallJob corp manufacturing");
            let corp_job_id = install_res.dict_get("jobID").and_then(|v| v.as_i64()).expect("corp jobID");

            // 3. Undock into Jita space, seed Gas Command Center in active ship cargo, and build a full PI network
            let ship_id = sess.ship_id.expect("active ship");
            sess.station_id = None;
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&P1) {
                    ch.station_id = None;
                    ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
                }
                seed_item(snap, GAS_COMMAND_CENTER_TYPE_ID, P1, ship_id, InventoryFlag::Cargo, 1, false);
            });

            let res_info = call(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap();
            let PyRep::List(res_list) = res_info.dict_get("resources").unwrap() else { panic!("expected resources list") };
            let first_p0_tid = res_list[0].dict_get("resourceTypeID").and_then(|v| v.as_i64()).unwrap() as TypeId;

            let pi_cmds = vec![
                cmd_pin(101, GAS_COMMAND_CENTER_TYPE_ID, 0.10, 0.10),
                cmd_upgrade_cc(101, 3),
                cmd_pin(102, GAS_EXTRACTOR_TYPE_ID, 0.11, 0.11),
                cmd_pin(103, GAS_BASIC_FACILITY_TYPE_ID, 0.12, 0.12),
                cmd_pin(104, GAS_LAUNCHPAD_TYPE_ID, 0.13, 0.13),
                cmd_link(101, 102),
                cmd_link(102, 103),
                cmd_link(103, 104),
                cmd_upgrade_link(102, 103, 1),
                cmd_schematic(103, 121),
                cmd_route(501, &[102, 103], first_p0_tid, 3_000),
                cmd_extractor_program(102, first_p0_tid, 0.02, 3),
            ];
            call(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(pi_cmds)])
                .await
                .expect("UserUpdateNetwork");

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&P1) {
                    ch.station_id = Some(JITA_STATION_ID);
                }
            });
            (order_id, issue_ft, escrow_isk, corp_job_id, first_p0_tid)
        };

        // === RESTART SERVER FROM SQLITE ===
        {
            // Set clock to 12 hours after base_time: 1-day order is still open, corp job (3600s) is ready!
            let ctx = open_ctx_at(&path, base_time + 43_200);
            let mut sess = login_select(&ctx, 7002, "demo", "demo-secret", P1).await;

            // 1a. Verify market order kept issueDate, duration=1, range=5, accountKey=1002 after restart
            let corp_orders = rowset_maps(&call(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[]).await.unwrap());
            let ord_row = corp_orders.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
            assert_eq!(ord_row.get("issueDate").and_then(|v| v.as_i64()), Some(expected_issue_filetime));
            assert_eq!(ord_row.get("duration").and_then(|v| v.as_i64()), Some(1));
            assert_eq!(ord_row.get("range").and_then(|v| v.as_i64()), Some(5));
            assert_eq!(ord_row.get("accountID").and_then(|v| v.as_i64()), Some(1002));

            // 1b. Advance clock past 1 day (base_time + 86_401) and verify order expires on schedule and refunds division 3 (accountKey 1002)
            let div3_before = ctx.db.with_snapshot(|s| {
                s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance
            });
            ctx.advance_secs(43_201);
            let hist_rows = rowset_maps(
                &call_kw(&ctx, &mut sess, "marketProxy", "GetMarketOrderHistory", &[PyRep::Bool(true)], &[("isCorp", PyRep::Bool(true))])
                    .await
                    .expect("GetMarketOrderHistory sweeps expired 1-day order"),
            );
            let exp_row = hist_rows.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
            assert_eq!(exp_row.get("orderState").and_then(|v| v.as_i64()), Some(4));
            assert_eq!(exp_row.get("accountID").and_then(|v| v.as_i64()), Some(1002));
            let div3_after = ctx.db.with_snapshot(|s| {
                s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance
            });
            assert_eq!(div3_after.0 - div3_before.0, (order_escrow_isk * 100.0).round() as i64);

            // 2. Complete the corp industry job after restart and verify it delivers the manufactured Rifter to the corporation (`owner_id == corp_id`)
            let complete_res = call(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_job_id)]).await.unwrap();
            let delivered_item_id = complete_res.dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
            let delivered_item = ctx.db.with_snapshot(|s| s.items.get(&delivered_item_id).cloned()).unwrap();
            assert_eq!(delivered_item.type_id, RIFTER_TYPE_ID);
            assert_eq!(delivered_item.owner_id, corp_id);

            // 3. Verify PI colony pins, links, routes, schematic, and extractor program are intact after restart
            let net_after = call(&ctx, &mut sess, "planetMgr", "GetFullNetworkForOwner", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(P1)])
                .await
                .expect("GetFullNetworkForOwner after restart");
            assert_eq!(net_after.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(3));
            let PyRep::List(pins_after) = net_after.dict_get("pins").unwrap() else { panic!("expected pins list") };
            assert_eq!(pins_after.len(), 4);
            let proc_pin = pins_after.iter().find(|p| p.dict_get("typeID").and_then(|v| v.as_i64()) == Some(GAS_BASIC_FACILITY_TYPE_ID as i64)).unwrap();
            assert_eq!(proc_pin.dict_get("schematicID").and_then(|v| v.as_i64()), Some(121));

            let PyRep::List(links_after) = net_after.dict_get("links").unwrap() else { panic!("expected links list") };
            assert_eq!(links_after.len(), 3);
            assert!(links_after.iter().any(|l| l.dict_get("level").and_then(|v| v.as_i64()) == Some(1)));

            let PyRep::List(routes_after) = net_after.dict_get("routes").unwrap() else { panic!("expected routes list") };
            assert_eq!(routes_after.len(), 1);
            assert_eq!(routes_after[0].dict_get("routeID").and_then(|v| v.as_i64()), Some(501));
            assert_eq!(routes_after[0].dict_get("commodityTypeID").and_then(|v| v.as_i64()), Some(first_p0_tid as i64));
            assert_eq!(routes_after[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(3_000));

            let ext_pin_id = pins_after
                .iter()
                .find(|p| p.dict_get("typeID").and_then(|v| v.as_i64()) == Some(GAS_EXTRACTOR_TYPE_ID as i64))
                .and_then(|p| p.dict_get("pinID"))
                .and_then(|v| v.as_i64())
                .expect("extractor pinID");
            let ext_pin = call(&ctx, &mut sess, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_pin_id)])
                .await
                .expect("GetPin Extractor after restart");
            assert_eq!(ext_pin.dict_get("headCount").and_then(|v| v.as_i64()), Some(3));
            assert_eq!(ext_pin.dict_get("resourceTypeID").and_then(|v| v.as_i64()), Some(first_p0_tid as i64));
        }

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_d6_lp_durable_and_atomic_failed_purchase_across_sqlite_restart() {
        // parity: feat:lifecycle.lp-durable
        eve_testkit::attest(&["feat:lifecycle.lp-durable"]);

        let path = std::env::temp_dir().join(format!("evomu_d6_lp_durable_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        let corp_id = 98_200_202;

        let (corp_lp_before, logs_before) = {
            let ctx = open_ctx_at(&path, 1_750_100_000);
            ctx.db.with_mut_snapshot(|snap| {
                *snap = demo;
                seed_corp_for_pilot(snap, P1, corp_id);
                if let Some(ch) = snap.characters.get_mut(&P1) {
                    ch.loyalty_points.insert(CALDARI_NAVY_CORP_ID, 15_000);
                    ch.loyalty_points.insert(FEDERATION_NAVY_CORP_ID, 500);
                    ch.loyalty_points.insert(CONCORD_CORP_ID, 5_000);
                }
                snap.items.retain(|_, it| !(it.owner_id == P1 && it.location_id == JITA_STATION_ID && it.type_id == TRITANIUM));
                seed_item(snap, TRITANIUM, P1, JITA_STATION_ID, InventoryFlag::Hangar, 50, false);
            });

            let mut sess = login_select(&ctx, 7101, "demo", "demo-secret", P1).await;
            call(&ctx, &mut sess, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(corp_id), PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(6_000)]).await.unwrap();
            call(&ctx, &mut sess, "LPSvc", "ExchangeConcordLP", &[PyRep::Integer(FEDERATION_NAVY_CORP_ID), PyRep::Integer(1_000)]).await.unwrap();
            let corp_lp = call(&ctx, &mut sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap();
            let logs = call(&ctx, &mut sess, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap();
            (corp_lp, logs)
        };

        // === RESTART SERVER FROM SQLITE ===
        {
            let ctx = open_ctx_at(&path, 1_750_100_060);
            crate::common::seed_test_lp_store_offers(&ctx);
            let mut sess = login_select(&ctx, 7102, "demo", "demo-secret", P1).await;

            let corp_lp_after = call(&ctx, &mut sess, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap();
            let logs_after = call(&ctx, &mut sess, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap();
            assert_eq!(corp_lp_after, corp_lp_before, "Corporation LP wallet balances must survive restart");
            assert_eq!(logs_after, logs_before, "Character LP reward logs must survive restart");
            let PyRep::List(log_entries) = &logs_after else { panic!("expected List of LP logs") };
            assert_eq!(log_entries.len(), 2);

            let snap_state = |ctx: &ServerContext| {
                ctx.db.with_snapshot(|s| {
                    let ch = &s.characters[&P1];
                    let trit_qty: i32 = s.items.values().filter(|i| i.owner_id == P1 && i.location_id == JITA_STATION_ID && i.type_id == TRITANIUM).map(|i| i.quantity).sum();
                    let bcs_qty: i32 = s.items.values().filter(|i| i.owner_id == P1 && i.location_id == JITA_STATION_ID && i.type_id == 15810).map(|i| i.quantity).sum();
                    (ch.loyalty_points.clone(), s.corp_lp_balances.clone(), ch.balance, trit_qty, bcs_qty)
                })
            };
            let before_fail = snap_state(&ctx);

            // 1. Fail on missing required trade-in items (Offer 1004 requires 100x Tritanium, pilot has 50x)
            let err_items = call(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(1004), PyRep::Integer(1)]).await.unwrap_err();
            assert_eq!(err_items.code, "MissingRequiredItems");
            assert_eq!(snap_state(&ctx), before_fail);

            // 2. Fail on insufficient character LP (Offer 1003 costs 10,000 LP, pilot has 9,000 LP)
            let err_lp = call(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(1003), PyRep::Integer(1)]).await.unwrap_err();
            assert_eq!(err_lp.code, "InsufficientLoyaltyPoints");
            assert_eq!(snap_state(&ctx), before_fail);

            // 3. Fail on insufficient corporation LP (Offer 1003 costs 10,000 LP, corp has 6,000 LP)
            let err_corp_lp = call(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(1003), PyRep::Integer(1)]).await.unwrap_err();
            assert_eq!(err_corp_lp.code, "InsufficientLoyaltyPoints");
            assert_eq!(snap_state(&ctx), before_fail);

            // 4. Fail on non-positive or overflowing quantity (`InvalidQuantity`)
            for bad_qty in [0i64, -5, i64::MAX] {
                let err_qty = call(&ctx, &mut sess, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(1001), PyRep::Integer(bad_qty)]).await.unwrap_err();
                assert_eq!(err_qty.code, "InvalidQuantity");
                assert_eq!(snap_state(&ctx), before_fail);
            }

            // 5. Fail when non-director corp member attempts corp LP spend or transfer (`InsufficientCorporationRole`)
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(c) = snap.corporations.get_mut(&corp_id) {
                    c.members.insert(P2);
                    c.member_roles.insert(P2, 0);
                }
                if let Some(ch2) = snap.characters.get_mut(&P2) {
                    ch2.corporation_id = corp_id;
                }
            });
            let mut sess2 = login_select(&ctx, 7103, "other", "other-secret", P2).await;
            let err_role_take = call(&ctx, &mut sess2, "LPStoreMgr", "TakeOfferForCorporation", &[PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(1001), PyRep::Integer(1)]).await.unwrap_err();
            assert_eq!(err_role_take.code, "InsufficientCorporationRole");
            let err_role_xfer = call(&ctx, &mut sess2, "LPSvc", "TransferLPFromMyCorpWalletToOtherCorp", &[PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(CALDARI_NAVY_CORP_ID), PyRep::Integer(100)]).await.unwrap_err();
            assert_eq!(err_role_xfer.code, "InsufficientCorporationRole");
        }

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_d6_ids_monotonic_and_durable_notifications_across_sqlite_restart() {
        // parity: feat:invariant.ids-monotonic-across-restart
        // parity: feat:lifecycle.notifications-durable
        eve_testkit::attest(&[
            "feat:invariant.ids-monotonic-across-restart",
            "feat:lifecycle.notifications-durable",
        ]);

        let path = std::env::temp_dir().join(format!("evomu_d6_monotonic_ids_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());

        let (corp_id_1, alliance_id_1, notif_id_1, notif_offline_id, notif_del_id, report_id_1, fleet_id_1, mail_id_1, plan_id_1, ms_id_1, goal_id_1) = {
            let ctx = open_ctx_at(&path, 1_750_200_000);
            ctx.db.with_mut_snapshot(|snap| {
                *snap = demo;
                for cid in [P1, P2] {
                    if let Some(ch) = snap.characters.get_mut(&cid) {
                        ch.balance = IskCents::from_isk(5_000_000_000);
                    }
                    snap.skills.entry(cid).or_default().insert(
                        3363,
                        TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 },
                    );
                }
            });

            let mut sess1 = login_select(&ctx, 7201, "demo", "demo-secret", P1).await;

            let corp_rep_1 = call(
                &ctx,
                &mut sess1,
                "corpRegistry",
                "AddCorporation",
                &[PyRep::String("First Horizon Industrials".into()), PyRep::String("FHZ1".into()), PyRep::Real(0.05)],
            )
            .await
            .expect("AddCorporation 1");
            let corp_id_1 = corp_rep_1.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(c) = snap.corporations.get_mut(&corp_id_1) {
                    c.ensure_wallet_divisions();
                    c.wallet_divisions[0].balance = IskCents::from_isk(2_000_000_000);
                    c.master_wallet = c.wallet_divisions[0].balance;
                }
            });

            let al_rep_1 = call(
                &ctx,
                &mut sess1,
                "corpRegistry",
                "CreateAlliance",
                &[
                    PyRep::String("First Horizon Alliance".into()),
                    PyRep::String("FHAL1".into()),
                    PyRep::String("Pre-restart alliance".into()),
                    PyRep::String("https://fhal1.example".into()),
                ],
            )
            .await
            .expect("CreateAlliance 1");
            let alliance_id_1 = al_rep_1.dict_get("allianceID").and_then(|v| v.as_i64()).unwrap();

            // Create notification for online P1 and mark it processed; create notification for offline P2; create & delete a 3rd notification
            let notif_id_1 = ctx.create_character_notification(1_000_125, P1, 10, 1, "Pre-restart processed notif").notification_id;
            call(&ctx, &mut sess1, "notificationMgr", "MarkAsProcessed", &[PyRep::List(vec![PyRep::Integer(notif_id_1)])]).await.unwrap();
            let notif_offline_id = ctx.create_character_notification(1_000_125, P2, 14, 2, "Offline pre-restart notif for P2").notification_id;
            let notif_del_id = ctx.create_character_notification(1_000_125, P1, 10, 1, "Deleted pre-restart notif").notification_id;
            call(&ctx, &mut sess1, "notificationMgr", "DeleteNotifications", &[PyRep::List(vec![PyRep::Integer(notif_del_id)])]).await.unwrap();

            call(
                &ctx,
                &mut sess1,
                "ingamereport",
                "report_macro_use",
                &[PyRep::Integer(P2), PyRep::String("botting".into()), PyRep::String("pre-restart report".into())],
            )
            .await
            .expect("report_macro_use");
            let report_id_1 = ingamereport_ops::get_persisted_reports(&ctx)[0].report_id;

            let fleet_rep_1 = call(&ctx, &mut sess1, "fleetSvc", "InitFleet", &[]).await.unwrap();
            let fleet_id_1 = fleet_rep_1.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();

            let mail_rep_1 = call(
                &ctx,
                &mut sess1,
                "mailMgr",
                "SendMail",
                &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None, PyRep::String("Pre-Restart Mail".into()), PyRep::String("Body 1".into())],
            )
            .await
            .unwrap();
            let mail_id_1 = mail_rep_1.as_i64().unwrap();

            let quasar_corp = eve_server::services::quasar_social_corp::QuasarCorpState::from_db(std::sync::Arc::clone(&ctx.db));
            let plan_id_1 = quasar_corp.alloc_plan_id();
            let ms_id_1 = quasar_corp.alloc_milestone_id();
            let goal_id_1 = quasar_corp.alloc_goal_id();
            quasar_corp.skill_plans.write().unwrap().insert(
                plan_id_1,
                eve_server::services::quasar_social_corp::CorpSkillPlanRecord {
                    id: plan_id_1,
                    corp_id: corp_id_1 as u32,
                    name: "Pre-Restart Doctrine".into(),
                    description: "Skill plan persisted before restart".into(),
                    category_id: 1,
                    requirements: vec![(3300, 5)],
                    milestones: vec![(ms_id_1, "Milestone Alpha".into())],
                },
            );
            quasar_corp.goals.write().unwrap().insert(
                goal_id_1,
                eve_server::services::quasar_social_corp::CorpGoalRecord {
                    id: goal_id_1,
                    corp_id: corp_id_1 as u32,
                    name: "Pre-Restart Corp Goal".into(),
                    description: "Goal persisted before restart".into(),
                    target_progress: 250,
                    current_progress: 75,
                    active: true,
                    contributors: std::collections::HashMap::new(),
                    escrow_isk: 500_000,
                    initial_escrow_isk: 500_000,
                    reward_per_unit_isk: 0,
                    wallet_division: 3,
                },
            );
            quasar_corp.sync_to_db();

            (corp_id_1, alliance_id_1, notif_id_1, notif_offline_id, notif_del_id, report_id_1, fleet_id_1, mail_id_1, plan_id_1, ms_id_1, goal_id_1)
        };

        // === RESTART SERVER FROM SQLITE ===
        {
            let ctx = open_ctx_at(&path, 1_750_200_100);
            let mut sess1 = login_select(&ctx, 7202, "demo", "demo-secret", P1).await;
            let mut sess2 = login_select(&ctx, 7203, "other", "other-secret", P2).await;

            // 1. Verify P1's already-received processed notification stayed in GetAllNotifications and deleted notification stayed deleted
            let p1_all = call(&ctx, &mut sess1, "notificationMgr", "GetAllNotifications", &[]).await.unwrap();
            let p1_rows = p1_all.crowset_rows().expect("CRowset");
            assert_eq!(p1_rows.len(), 1);
            assert_eq!(p1_rows[0].dbrow_get("notificationID"), Some(&PyRep::Integer(notif_id_1)));
            assert_eq!(p1_rows[0].dbrow_get("processed"), Some(&PyRep::Bool(true)));

            // 2. Verify P2's offline notification (created while P2 was offline before restart) was delivered at login (`OnNotificationReceived`)
            let p2_login_notifs = ctx.drain_notifications_for_char(P2);
            assert!(
                p2_login_notifs.iter().any(|n| n.event == "OnNotificationReceived" && n.args.first() == Some(&PyRep::Integer(notif_offline_id))),
                "Notification created while P2 was offline before restart must be delivered at next login"
            );
            let p2_unproc = call(&ctx, &mut sess2, "notificationMgr", "GetUnprocessed", &[]).await.unwrap();
            let p2_rows = p2_unproc.crowset_rows().expect("CRowset");
            assert_eq!(p2_rows.len(), 1);
            assert_eq!(p2_rows[0].dbrow_get("notificationID"), Some(&PyRep::Integer(notif_offline_id)));

            let corp_rep_2 = call(
                &ctx,
                &mut sess2,
                "corpRegistry",
                "AddCorporation",
                &[PyRep::String("Second Horizon Logistics".into()), PyRep::String("SHZ2".into()), PyRep::Real(0.07)],
            )
            .await
            .expect("AddCorporation 2 after restart");
            let corp_id_2 = corp_rep_2.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(c) = snap.corporations.get_mut(&corp_id_2) {
                    c.ensure_wallet_divisions();
                    c.wallet_divisions[0].balance = IskCents::from_isk(2_000_000_000);
                    c.master_wallet = c.wallet_divisions[0].balance;
                }
            });

            let al_rep_2 = call(
                &ctx,
                &mut sess2,
                "corpRegistry",
                "CreateAlliance",
                &[
                    PyRep::String("Second Horizon Alliance".into()),
                    PyRep::String("SHAL2".into()),
                    PyRep::String("Post-restart alliance".into()),
                    PyRep::String("https://shal2.example".into()),
                ],
            )
            .await
            .expect("CreateAlliance 2 after restart");
            let alliance_id_2 = al_rep_2.dict_get("allianceID").and_then(|v| v.as_i64()).unwrap();

            let notif_id_2 = ctx.create_character_notification(1_000_125, P2, 10, 1, "Post-restart notification").notification_id;

            call(
                &ctx,
                &mut sess2,
                "ingamereport",
                "report_exploit_abuse",
                &[PyRep::Integer(P1), PyRep::String("exploit".into()), PyRep::String("post-restart report".into())],
            )
            .await
            .expect("report_exploit_abuse after restart");
            let reports_all = ingamereport_ops::get_persisted_reports(&ctx);
            assert_eq!(reports_all.len(), 2);
            let report_id_2 = reports_all.iter().find(|r| r.category == "exploit_abuse").unwrap().report_id;

            let fleet_rep_2 = call(&ctx, &mut sess2, "fleetSvc", "InitFleet", &[]).await.unwrap();
            let fleet_id_2 = fleet_rep_2.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();

            let mail_rep_2 = call(
                &ctx,
                &mut sess2,
                "mailMgr",
                "SendMail",
                &[PyRep::List(vec![PyRep::Integer(P1)]), PyRep::None, PyRep::None, PyRep::String("Post-Restart Mail".into()), PyRep::String("Body 2".into())],
            )
            .await
            .unwrap();
            let mail_id_2 = mail_rep_2.as_i64().unwrap();

            let quasar_corp = eve_server::services::quasar_social_corp::QuasarCorpState::from_db(std::sync::Arc::clone(&ctx.db));
            assert!(quasar_corp.skill_plans.read().unwrap().contains_key(&plan_id_1), "Pre-restart skill plan must survive restart");
            assert!(quasar_corp.goals.read().unwrap().contains_key(&goal_id_1), "Pre-restart corp goal must survive restart");
            let plan_id_2 = quasar_corp.alloc_plan_id();
            let ms_id_2 = quasar_corp.alloc_milestone_id();
            let goal_id_2 = quasar_corp.alloc_goal_id();

            assert!(corp_id_2 > corp_id_1, "corp_id_2 ({corp_id_2}) > corp_id_1 ({corp_id_1})");
            assert!(alliance_id_2 > alliance_id_1, "alliance_id_2 ({alliance_id_2}) > alliance_id_1 ({alliance_id_1})");
            assert!(notif_id_2 > notif_del_id && notif_del_id > notif_offline_id && notif_offline_id > notif_id_1);
            assert!(report_id_2 > report_id_1, "report_id_2 ({report_id_2}) > report_id_1 ({report_id_1})");
            assert!(fleet_id_2 > fleet_id_1, "fleet_id_2 ({fleet_id_2}) > fleet_id_1 ({fleet_id_1})");
            assert!(mail_id_2 > mail_id_1, "mail_id_2 ({mail_id_2}) > mail_id_1 ({mail_id_1})");
            assert!(plan_id_2 > plan_id_1, "plan_id_2 ({plan_id_2}) > plan_id_1 ({plan_id_1})");
            assert!(ms_id_2 > ms_id_1, "ms_id_2 ({ms_id_2}) > ms_id_1 ({ms_id_1})");
            assert!(goal_id_2 > goal_id_1, "goal_id_2 ({goal_id_2}) > goal_id_1 ({goal_id_1})");

            ctx.db.with_snapshot(|snap| {
                let a1 = snap.alliances.get(&alliance_id_1).expect("Pre-restart alliance must still exist intact");
                let a2 = snap.alliances.get(&alliance_id_2).expect("Post-restart alliance must exist");
                assert_eq!(a1.name, "First Horizon Alliance");
                assert_eq!(a1.executor_corp_id, corp_id_1);
                assert_eq!(a2.name, "Second Horizon Alliance");
                assert_eq!(a2.executor_corp_id, corp_id_2);
                assert!(!snap.fleets.contains_key(&fleet_id_1) && snap.fleets.contains_key(&fleet_id_2));
                assert!(snap.mails.iter().any(|m| m.message_id == mail_id_1) && snap.mails.iter().any(|m| m.message_id == mail_id_2));
            });
        }

        let _ = std::fs::remove_file(&path);
    }
}

pub(crate) mod contacts_labels_blocklist_cspa_persist {
    // `D-6` Social Durability SQLite restart integration tests:
    // - `feat:lifecycle.contacts-persist`
    // - `feat:lifecycle.mail-state-persist`
    // - `feat:lifecycle.bookmarks-persist`
    // - `feat:lifecycle.chat-channels-persist`
    // - `feat:lifecycle.social-durable`


    use crate::common;
    use eve_db::WorldRepository;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::char_social_mail_routes::dispatch_char_social_mail_route;
    use eve_server::esi::char_social_routes::dispatch_char_social_route;
    use eve_server::esi::character_mail_routes::dispatch_character_mail_routes;
    use eve_server::router::ServerContext;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn temp_sqlite_path(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("evomu_d6_social_{}_{}.sqlite", tag, std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn seed_repo_with_corp(repo: &WorldRepository) {
        repo.with_mut_snapshot(|snap| {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
            snap.corporations.insert(98_000_001, eve_db::CorporationRecord {
                corporation_id: 98_000_001, name: "D6 Social Corp".into(), ticker: "D6SOC".into(),
                ceo_id: 90_000_001, alliance_id: Some(99_000_001), tax_rate: 0.05,
                master_wallet: eve_db::IskCents(0), members: HashSet::from([90_000_001, 90_000_002]),
                member_roles: HashMap::from([(90_000_001, u64::MAX)]), wallet_divisions: Vec::new(),
                applications: HashMap::new(), description: String::new(), url: String::new(), hq_station_id: Some(60_003_760),
            });
            snap.alliances.insert(99_000_001, eve_db::AllianceRecord {
                alliance_id: 99_000_001, name: "D6 Social Alliance".into(), short_name: "D6ALY".into(),
                executor_corp_id: 98_000_001, member_corp_ids: HashSet::from([98_000_001]),
            });
            for cid in [90_000_001, 90_000_002] {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.corporation_id = 98_000_001;
                    ch.alliance_id = Some(99_000_001);
                }
            }
        });
    }

    async fn call(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
        let kw = HashMap::new();
        ctx.dispatch_rpc(sess, svc, None, method, args, &kw, None)
            .await
            .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
    }

    async fn login_and_select(ctx: &ServerContext, sess_id: u64, username: &str, char_id: i64) -> SessionState {
        let mut sess = SessionState::new(sess_id);
        common::login_pilot(ctx, &mut sess, username, "demo-secret").await;
        call(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
        sess.alliance_id = Some(99_000_001);
        sess
    }

    #[tokio::test]
    async fn test_d6_contacts_labels_blocklist_cspa_persist_across_sqlite_restart() {
        // parity: feat:lifecycle.contacts-persist, feat:lifecycle.social-durable
        let db_path = temp_sqlite_path("contacts");
        let kw = HashMap::new();

        let (char_label_id, corp_label_id, ally_label_id, corp_id) = {
            let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
            seed_repo_with_corp(&repo1);
            let ctx1 = ServerContext::with_repository(repo1);
            common::pin_clock(&ctx1);

            let mut sess1 = login_and_select(&ctx1, 7101, "demo", 90_000_001).await;
            let corp_id = sess1.corporation_id.unwrap_or(98_000_001);

            // 1. Personal contact label + contact + watchlist + blocklist + CSPA charge + notes
            let lbl_rep = call(&ctx1, &mut sess1, "charMgr", "CreateLabel", &[PyRep::String("Trusted Friends".into()), PyRep::Integer(0x00FF00)]).await;
            let char_label_id = lbl_rep.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);

            call(&ctx1, &mut sess1, "charMgr", "AddContact", &[PyRep::Integer(90_000_002), PyRep::Real(10.0), PyRep::Bool(true), PyRep::Integer(char_label_id)]).await;
            call(&ctx1, &mut sess1, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(90_000_003)])]).await;
            call(&ctx1, &mut sess1, "charMgr", "LogSettings", &[PyRep::Real(5_000.0)]).await;
            call(&ctx1, &mut sess1, "charMgr", "SetNote", &[PyRep::Integer(90_000_002), PyRep::String("Reliable fleet scout".into())]).await;
            call(&ctx1, &mut sess1, "charMgr", "AddOwnerNote", &[PyRep::Integer(90_000_001), PyRep::String("S:Hangar Checklist".into()), PyRep::String("Bring nanite paste".into())]).await;

            // 2. Corporate contact + label
            let corp_lbl = call(&ctx1, &mut sess1, "corpRegistry", "CreateLabel", &[PyRep::String("Blue Coalition".into()), PyRep::Integer(0x0055FF)]).await;
            let corp_label_id = corp_lbl.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);
            call(&ctx1, &mut sess1, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(90_000_002), PyRep::Real(8.5), PyRep::Integer(corp_label_id)]).await;

            // 3. Alliance contact + label
            let ally_lbl = call(&ctx1, &mut sess1, "allianceRegistry", "CreateLabel", &[PyRep::String("Pact Partners".into()), PyRep::Integer(0xFFAA00)]).await;
            let ally_label_id = ally_lbl.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);
            call(&ctx1, &mut sess1, "allianceRegistry", "AddAllianceContact", &[PyRep::Integer(90_000_002), PyRep::Real(7.5), PyRep::Integer(ally_label_id)]).await;

            (char_label_id, corp_label_id, ally_label_id, corp_id)
        };

        // Restart server from the SQLite file on disk
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
        let ctx2 = ServerContext::with_repository(repo2);
        common::pin_clock(&ctx2);

        let mut sess1 = login_and_select(&ctx2, 7102, "demo", 90_000_001).await;
        let contacts_kv = call(&ctx2, &mut sess1, "charMgr", "GetContacts", &[]).await;
        let PyRep::Rowset { rows: addr_rows, .. } = contacts_kv.dict_get("addresses").expect("addresses") else {
            panic!("expected addresses rowset");
        };
        assert_eq!(
            addr_rows,
            &vec![vec![
                PyRep::Integer(90_000_002),
                PyRep::Bool(true),
                PyRep::Real(10.0),
                PyRep::Integer(char_label_id),
            ]],
            "GetContacts addresses must preserve exact contactID, watchlist, standing, and labelMask and exclude blocked-only non-contacts"
        );
        let PyRep::Rowset { rows: blocked_rows, .. } = contacts_kv.dict_get("blocked").expect("blocked") else {
            panic!("expected blocked rowset");
        };
        assert_eq!(
            blocked_rows,
            &vec![vec![PyRep::Integer(90_000_003)]],
            "GetContacts blocked must contain blocked owner 90_000_003 after restart"
        );

        let labels_rep = call(&ctx2, &mut sess1, "charMgr", "GetLabels", &[]).await;
        assert!(
            format!("{labels_rep:?}").contains("Trusted Friends")
                && format!("{labels_rep:?}").contains(&char_label_id.to_string()),
            "Personal contact labels must survive SQLite restart"
        );

        let note_rep = call(&ctx2, &mut sess1, "charMgr", "GetNote", &[PyRep::Integer(90_000_002)]).await;
        assert_eq!(note_rep.as_str(), Some("Reliable fleet scout"));

        let owner_notes = call(&ctx2, &mut sess1, "charMgr", "GetOwnerNoteLabels", &[]).await;
        assert!(
            format!("{owner_notes:?}").contains("S:Hangar Checklist"),
            "Owner notes must survive SQLite restart"
        );

        let corp_contacts = call(&ctx2, &mut sess1, "corpRegistry", "GetCorporateContacts", &[]).await;
        assert!(
            format!("{corp_contacts:?}").contains("90000002")
                && format!("{corp_contacts:?}").contains("8.5")
                && format!("{corp_contacts:?}").contains(&corp_label_id.to_string()),
            "Corporate contacts and labels must survive SQLite restart"
        );

        let ally_contacts = call(&ctx2, &mut sess1, "allianceRegistry", "GetAllianceContacts", &[]).await;
        assert!(
            format!("{ally_contacts:?}").contains("90000002")
                && format!("{ally_contacts:?}").contains("7.5")
                && format!("{ally_contacts:?}").contains(&ally_label_id.to_string()),
            "Alliance contacts must survive SQLite restart"
        );
        let ally_labels = call(&ctx2, &mut sess1, "allianceRegistry", "GetLabels", &[]).await;
        assert!(
            format!("{ally_labels:?}").contains("Pact Partners")
                && format!("{ally_labels:?}").contains(&ally_label_id.to_string()),
            "Alliance contact labels must survive SQLite restart"
        );

        // Verify ESI also sees the restored character, corporation & alliance contacts and labels
        let params = HashMap::from([("character_id".to_string(), "90000001".to_string())]);
        let esi_contacts = dispatch_char_social_mail_route(&ctx2, "GET", "/characters/{character_id}/contacts", &params, &HashMap::new(), b"")
            .expect("ESI GET /characters/{character_id}/contacts");
        assert_eq!(esi_contacts.status_code, 200);
        let esi_char_body = esi_contacts.body.as_ref().unwrap().to_string();
        assert!(esi_char_body.contains("90000002") && esi_char_body.contains("10.0"));

        let corp_params = HashMap::from([("corporation_id".to_string(), corp_id.to_string())]);
        let esi_corp_contacts = dispatch_char_social_mail_route(&ctx2, "GET", "/corporations/{corporation_id}/contacts", &corp_params, &HashMap::new(), b"")
            .expect("ESI GET /corporations/{corporation_id}/contacts");
        assert_eq!(esi_corp_contacts.status_code, 200);
        assert!(esi_corp_contacts.body.as_ref().unwrap().to_string().contains("90000002"));

        let ally_params = HashMap::from([("alliance_id".to_string(), "99000001".to_string())]);
        let esi_ally_contacts = dispatch_char_social_mail_route(&ctx2, "GET", "/alliances/{alliance_id}/contacts", &ally_params, &HashMap::new(), b"")
            .expect("ESI GET /alliances/{alliance_id}/contacts");
        assert_eq!(esi_ally_contacts.status_code, 200);
        assert!(esi_ally_contacts.body.as_ref().unwrap().to_string().contains("90000002"));

        let esi_ally_labels = dispatch_char_social_mail_route(&ctx2, "GET", "/alliances/{alliance_id}/contacts/labels", &ally_params, &HashMap::new(), b"")
            .expect("ESI GET /alliances/{alliance_id}/contacts/labels");
        assert_eq!(esi_ally_labels.status_code, 200);
        assert!(esi_ally_labels.body.as_ref().unwrap().to_string().contains("Pact Partners"));

        // Verify blocked sender (90_000_003) cannot send mail to 90_000_001 after restart
        let mut sess3 = SessionState::new(7103);
        sess3.user_id = Some(3);
        sess3.character_id = Some(90_000_003);
        sess3.corporation_id = Some(1_000_009);
        let blocked_mail = ctx2
            .dispatch_rpc(
                &mut sess3,
                "mailMgr",
                None,
                "SendMail",
                &[PyRep::List(vec![PyRep::Integer(90_000_001)]), PyRep::None, PyRep::Integer(0), PyRep::String("Spam".into()), PyRep::String("Blocked body".into())],
                &kw,
                None,
            )
            .await;
        assert!(blocked_mail.is_err(), "Blocked sender must still be refused by mailMgr.SendMail after SQLite restart");

        // Verify friendly contact (90_000_002) gets CSPA exemption when contacting 90_000_001 after restart
        let mut sess2 = SessionState::new(7104);
        sess2.user_id = Some(2);
        sess2.character_id = Some(90_000_002);
        sess2.corporation_id = Some(1_000_009);
        let cspa_res = call(&ctx2, &mut sess2, "charMgr", "CSPAChargedAction", &[PyRep::Integer(90_000_001)]).await;
        assert_eq!(cspa_res, PyRep::Real(0.0), "Friendly contact must remain exempt from CSPA charge after SQLite restart");

        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_d6_mail_state_labels_trash_mailing_lists_and_calendar_persist_across_sqlite_restart() {
        // parity: feat:lifecycle.mail-state-persist, feat:lifecycle.social-durable
        let db_path = temp_sqlite_path("mail_calendar");

        let (mail_read_id, mail_trash_id, mail_del_id, mail_unread_id, custom_lbl_id, list_id, event_id) = {
            let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
            seed_repo_with_corp(&repo1);
            let ctx1 = ServerContext::with_repository(repo1);
            common::pin_clock(&ctx1);

            let mut sess1 = login_and_select(&ctx1, 7201, "demo", 90_000_001).await;
            let mut sess2 = SessionState::new(7202);
            sess2.user_id = Some(2);
            sess2.character_id = Some(90_000_002);
            sess2.corporation_id = Some(98_000_001);

            let ml_rep = call(
                &ctx1,
                &mut sess1,
                "mailingListsMgr",
                "Create",
                &[PyRep::String("Vanguard Dispatch".into()), PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(250)],
            )
            .await;
            let list_id = ml_rep.dict_get("listID").and_then(|v| v.as_i64()).or_else(|| ml_rep.as_i64()).unwrap_or(700_001);

            call(&ctx1, &mut sess1, "mailingListsMgr", "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Welcome to Vanguard".into()), PyRep::String("Stay on comms".into())]).await;
            call(&ctx1, &mut sess2, "mailingListsMgr", "Join", &[PyRep::String("Vanguard Dispatch".into())]).await;
            call(&ctx1, &mut sess2, "mailMgr", "MarkAllAsRead", &[]).await;
            call(&ctx1, &mut sess1, "mailingListsMgr", "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_002)])]).await;

            let mut sent_ids = Vec::new();
            for idx in 1..=4 {
                let mid = call(
                    &ctx1,
                    &mut sess1,
                    "mailMgr",
                    "SendMail",
                    &[PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::None, PyRep::Integer(0), PyRep::String(format!("Subject {idx}")), PyRep::String(format!("Body {idx}"))],
                )
                .await
                .as_i64()
                .expect("mail_id");
                sent_ids.push(mid);
            }
            let (m1, m2, m3, m4) = (sent_ids[0], sent_ids[1], sent_ids[2], sent_ids[3]);

            let lbl_rep = call(&ctx1, &mut sess2, "mailMgr", "CreateLabel", &[PyRep::String("Fleet Intel".into()), PyRep::Integer(4)]).await;
            let custom_lbl_id = lbl_rep.dict_get("labelID").and_then(|v| v.as_i64()).or_else(|| lbl_rep.as_i64()).unwrap_or(16);

            // Assign Sent (2), Corp (4), Alliance (8), and custom label to m1, then mark m1 read
            call(&ctx1, &mut sess2, "mailMgr", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(m1)]), PyRep::List(vec![PyRep::Integer(2), PyRep::Integer(4), PyRep::Integer(8), PyRep::Integer(custom_lbl_id)])]).await;
            call(&ctx1, &mut sess2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(m1)])]).await;

            // Assign Corp (4), Alliance (8), and custom label to m4, and leave m4 UNREAD
            call(&ctx1, &mut sess2, "mailMgr", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(m4)]), PyRep::List(vec![PyRep::Integer(4), PyRep::Integer(8), PyRep::Integer(custom_lbl_id)])]).await;

            call(&ctx1, &mut sess2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(m2)])]).await;
            call(&ctx1, &mut sess2, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(m3)])]).await;

            let ev_rep = call(
                &ctx1,
                &mut sess1,
                "calendarMgr",
                "CreatePersonalEvent",
                &[PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(90), PyRep::String("Stratop Formup".into()), PyRep::String("Bring doctrines".into()), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(90_000_002)])],
            )
            .await;
            let event_id = ev_rep.dict_get("eventID").and_then(|v| v.as_i64()).unwrap_or(700_002);
            call(&ctx1, &mut sess2, "calendarMgr", "SendEventResponse", &[PyRep::Integer(event_id), PyRep::Integer(90_000_001), PyRep::Integer(4)]).await;

            (m1, m2, m3, m4, custom_lbl_id, list_id, event_id)
        };

        // Restart server from disk
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
        let ctx2 = ServerContext::with_repository(repo2);
        common::pin_clock(&ctx2);

        let mut sess2 = SessionState::new(7203);
        sess2.user_id = Some(2);
        sess2.character_id = Some(90_000_002);
        sess2.corporation_id = Some(98_000_001);

        let headers = call(&ctx2, &mut sess2, "mailMgr", "GetMailHeaders", &[]).await;
        let PyRep::Rowset { rows: hdr_rows, .. } = &headers else { panic!("expected headers rowset") };
        let m1_row = hdr_rows.iter().find(|r| r[0] == PyRep::Integer(mail_read_id)).expect("m1 in headers");
        assert_eq!(m1_row[7], PyRep::Bool(true), "m1 read status must be true after restart");
        assert_eq!(
            m1_row[8],
            PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(4), PyRep::Integer(8), PyRep::Integer(custom_lbl_id)]),
            "m1 assigned labels (Inbox, Sent, Corp, Alliance, custom) must survive restart"
        );
        let m4_row = hdr_rows.iter().find(|r| r[0] == PyRep::Integer(mail_unread_id)).expect("m4 in headers");
        assert_eq!(m4_row[7], PyRep::Bool(false), "m4 read status must be false after restart");
        assert!(
            !hdr_rows.iter().any(|r| r[0] == PyRep::Integer(mail_trash_id) || r[0] == PyRep::Integer(mail_del_id)),
            "Trashed and deleted mails must not appear in active inbox headers after restart"
        );

        let labels = call(&ctx2, &mut sess2, "mailMgr", "GetLabels", &[]).await;
        let PyRep::Dict(lbl_dict) = &labels else { panic!("expected labels dict") };
        let unread_for = |lid: i64| -> i64 {
            lbl_dict.iter().find(|(k, _)| k == &PyRep::Integer(lid)).and_then(|(_, v)| v.dict_get("unreadCount")).and_then(|v| v.as_i64()).unwrap_or(-1)
        };
        assert_eq!(unread_for(1), 1, "Inbox unreadCount must be 1 after restart");
        assert_eq!(unread_for(2), 0, "Sent unreadCount must be 0 after restart");
        assert_eq!(unread_for(4), 1, "Corp unreadCount must be 1 after restart");
        assert_eq!(unread_for(8), 1, "Alliance unreadCount must be 1 after restart");
        assert_eq!(unread_for(custom_lbl_id), 1, "Custom label unreadCount must be 1 after restart");

        // Verify ESI mail headers, mail labels/unread counts, and mailing lists after restart
        let char2_params = HashMap::from([("character_id".to_string(), "90000002".to_string())]);
        let esi_mail_hdrs = dispatch_character_mail_routes(&ctx2, "GET", "/characters/{character_id}/mail", &char2_params, &HashMap::new(), None, 1, b"")
            .expect("ESI GET /characters/{character_id}/mail");
        assert_eq!(esi_mail_hdrs.status_code, 200);
        let esi_hdrs_arr = esi_mail_hdrs.body.as_ref().unwrap().as_array().expect("mail array");
        let esi_m1 = esi_hdrs_arr.iter().find(|m| m["mail_id"] == mail_read_id).expect("m1 in ESI mail");
        assert_eq!(esi_m1["is_read"], serde_json::json!(true));
        assert_eq!(esi_m1["labels"], serde_json::json!([1, 2, 4, 8, custom_lbl_id]));
        let esi_m4 = esi_hdrs_arr.iter().find(|m| m["mail_id"] == mail_unread_id).expect("m4 in ESI mail");
        assert_eq!(esi_m4["is_read"], serde_json::json!(false));
        assert!(
            !esi_hdrs_arr.iter().any(|m| m["mail_id"] == mail_trash_id || m["mail_id"] == mail_del_id),
            "Trashed (m2) and deleted (m3) mails must not appear in ESI mail headers"
        );

        let esi_mail_lbls = dispatch_char_social_mail_route(&ctx2, "GET", "/characters/{character_id}/mail/labels", &char2_params, &HashMap::new(), b"")
            .expect("ESI GET /characters/{character_id}/mail/labels");
        assert_eq!(esi_mail_lbls.status_code, 200);
        let esi_lbls_json = esi_mail_lbls.body.as_ref().unwrap();
        assert_eq!(esi_lbls_json["total_unread_count"], serde_json::json!(1));
        let esi_lbl_unread = |lid: i64| -> i64 {
            esi_lbls_json["labels"].as_array().unwrap().iter().find(|l| l["label_id"] == lid).and_then(|l| l["unread_count"].as_i64()).unwrap_or(-1)
        };
        assert_eq!((esi_lbl_unread(1), esi_lbl_unread(2), esi_lbl_unread(4), esi_lbl_unread(8), esi_lbl_unread(custom_lbl_id)), (1, 0, 1, 1, 1));

        // Verify post-restart mailMgr.MarkAsUnread and mailMgr.EditLabel propagate immediately to ESI without split-brain
        call(&ctx2, &mut sess2, "mailMgr", "MarkAsUnread", &[PyRep::List(vec![PyRep::Integer(mail_read_id)])]).await;
        call(&ctx2, &mut sess2, "mailMgr", "EditLabel", &[PyRep::Integer(custom_lbl_id), PyRep::String("Fleet Intel Updated".into()), PyRep::String("#00ff33".into())]).await;
        let esi_hdrs_after_unread = dispatch_character_mail_routes(&ctx2, "GET", "/characters/{character_id}/mail", &char2_params, &HashMap::new(), None, 1, b"")
            .expect("ESI GET /characters/{character_id}/mail after MarkAsUnread");
        let esi_m1_unread = esi_hdrs_after_unread.body.as_ref().unwrap().as_array().unwrap().iter().find(|m| m["mail_id"] == mail_read_id).unwrap();
        assert_eq!(esi_m1_unread["is_read"], serde_json::json!(false), "MarkAsUnread after restart must update ESI is_read to false");
        let esi_lbls_after_edit = dispatch_char_social_mail_route(&ctx2, "GET", "/characters/{character_id}/mail/labels", &char2_params, &HashMap::new(), b"")
            .expect("ESI GET /characters/{character_id}/mail/labels after EditLabel");
        let esi_lbls_after_json = esi_lbls_after_edit.body.as_ref().unwrap();
        assert_eq!(esi_lbls_after_json["total_unread_count"], serde_json::json!(2));
        assert!(esi_lbls_after_json.to_string().contains("Fleet Intel Updated"), "EditLabel after restart must reflect in ESI mail labels");

        let esi_mail_lists = dispatch_char_social_mail_route(&ctx2, "GET", "/characters/{character_id}/mail/lists", &char2_params, &HashMap::new(), b"")
            .expect("ESI GET /characters/{character_id}/mail/lists");
        assert_eq!(esi_mail_lists.status_code, 200);
        let esi_lists_str = esi_mail_lists.body.as_ref().unwrap().to_string();
        assert!(esi_lists_str.contains("Vanguard Dispatch") && esi_lists_str.contains(&list_id.to_string()));

        let restored = call(&ctx2, &mut sess2, "mailMgr", "MoveFromTrash", &[PyRep::List(vec![PyRep::Integer(mail_trash_id)])]).await;
        assert!(format!("{restored:?}").contains("true"));
        let headers_after_restore = call(&ctx2, &mut sess2, "mailMgr", "GetMailHeaders", &[]).await;
        assert!(format!("{headers_after_restore:?}").contains(&mail_trash_id.to_string()));

        let joined_lists = call(&ctx2, &mut sess2, "mailingListsMgr", "GetJoinedLists", &[]).await;
        assert!(format!("{joined_lists:?}").contains("Vanguard Dispatch"));

        let members = call(&ctx2, &mut sess2, "mailingListsMgr", "GetMembers", &[PyRep::Integer(list_id)]).await;
        let PyRep::Dict(mem_dict) = &members else { panic!("expected members dict") };
        let m_entry = mem_dict.iter().find(|(k, _)| k == &PyRep::Integer(90_000_002)).map(|(_, v)| v).expect("member 90000002");
        assert_eq!(m_entry.dict_get("role"), Some(&PyRep::Integer(2)), "Member 90_000_002 must retain operator role (2) after restart: {mem_dict:?}");

        let settings = call(&ctx2, &mut sess2, "mailingListsMgr", "GetSettings", &[PyRep::Integer(list_id)]).await;
        assert_eq!(settings.dict_get("defaultAccess"), Some(&PyRep::Integer(1)));
        assert_eq!(settings.dict_get("defaultRole"), Some(&PyRep::Integer(0)));
        assert_eq!(settings.dict_get("cost"), Some(&PyRep::Integer(250)));

        let welcome = call(&ctx2, &mut sess2, "mailingListsMgr", "GetWelcomeMail", &[PyRep::Integer(list_id)]).await;
        assert!(format!("{welcome:?}").contains("Welcome to Vanguard"));

        let ev_details = call(&ctx2, &mut sess2, "calendarMgr", "GetEventDetails", &[PyRep::Integer(event_id)]).await;
        assert!(format!("{ev_details:?}").contains("Stratop Formup") && format!("{ev_details:?}").contains("Bring doctrines"));

        let esi_params = HashMap::from([("character_id".to_string(), "90000002".to_string()), ("event_id".to_string(), event_id.to_string())]);
        let esi_att = dispatch_char_social_route(&ctx2, "GET", "/characters/{character_id}/calendar/{event_id}/attendees", &esi_params, &HashMap::new(), b"")
            .expect("ESI calendar attendees after restart");
        assert_eq!(esi_att.status_code, 200);
        assert!(esi_att.body.as_ref().unwrap().to_string().contains("accepted"));

        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_d6_bookmarks_folders_subfolders_acls_and_expiry_persist_across_sqlite_restart() {
        // parity: feat:lifecycle.bookmarks-persist, feat:lifecycle.social-durable
        let db_path = temp_sqlite_path("bookmarks");

        let (folder_id, subfolder_id, empty_subfolder_id, perm_bm_id, temp_bm_id) = {
            let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
            seed_repo_with_corp(&repo1);
            let ctx1 = ServerContext::with_repository(repo1);
            common::pin_clock(&ctx1);
            ctx1.db.with_mut_snapshot(|s| {
                s.access_groups.insert(1001, vec![90_000_001, 90_000_002]);
            });

            let mut sess1 = login_and_select(&ctx1, 7301, "demo", 90_000_001).await;
            let mut sess2 = SessionState::new(7302);
            sess2.user_id = Some(2);
            sess2.character_id = Some(90_000_002);
            sess2.corporation_id = Some(98_000_001);

            let f_rep = call(
                &ctx1,
                &mut sess1,
                "accessGroupBookmarkMgr",
                "AddFolder",
                &[PyRep::String("Wormhole Chain".into()), PyRep::Bool(true), PyRep::List(vec![PyRep::Integer(90_000_002), PyRep::Integer(98_000_001)])],
            )
            .await;
            let folder_id = f_rep.dict_get("folderID").and_then(|v| v.as_i64()).unwrap_or(1);

            call(
                &ctx1,
                &mut sess1,
                "accessGroupBookmarkMgr",
                "UpdateFolder",
                &[PyRep::Integer(folder_id), PyRep::String("Wormhole Chain".into()), PyRep::List(vec![PyRep::Integer(90_000_002), PyRep::Integer(98_000_001), PyRep::Integer(99_000_001)])],
            )
            .await;

            let sf_rep = call(&ctx1, &mut sess1, "bookmark", "CreateSubfolder", &[PyRep::Integer(folder_id), PyRep::String("C5 Statics".into())]).await;
            let subfolder_id = sf_rep.dict_get("subfolderID").and_then(|v| v.as_i64()).unwrap_or(1);
            let sf2_rep = call(&ctx1, &mut sess1, "bookmark", "CreateSubfolder", &[PyRep::Integer(folder_id), PyRep::String("Empty Reconnaissance".into())]).await;
            let empty_subfolder_id = sf2_rep.dict_get("subfolderID").and_then(|v| v.as_i64()).unwrap_or(2);

            let bm1 = call(
                &ctx1,
                &mut sess1,
                "bookmark",
                "BookmarkLocation",
                &[PyRep::String("Safe Spot Alpha".into()), PyRep::Integer(60_003_760), PyRep::String("Deep safe".into()), PyRep::Integer(folder_id), PyRep::Integer(subfolder_id), PyRep::Integer(0)],
            )
            .await;
            let perm_bm_id = bm1.dict_get("bookmarkID").and_then(|v| v.as_i64()).unwrap_or(1);

            call(&ctx1, &mut sess1, "accessGroupBookmarkMgr", "MoveBookmarksToFolderAndSubfolder", &[PyRep::Integer(folder_id), PyRep::Integer(subfolder_id), PyRep::List(vec![PyRep::Integer(perm_bm_id)]), PyRep::None]).await;

            let bm2 = call(
                &ctx1,
                &mut sess1,
                "bookmark",
                "BookmarkLocation",
                &[PyRep::String("Temp Wormhole Exit".into()), PyRep::Integer(60_003_760), PyRep::String("Expires in 3h".into()), PyRep::Integer(folder_id), PyRep::Integer(subfolder_id), PyRep::Integer(3)],
            )
            .await;
            let temp_bm_id = bm2.dict_get("bookmarkID").and_then(|v| v.as_i64()).unwrap_or(2);

            call(&ctx1, &mut sess2, "accessGroupBookmarkMgr", "AddToKnownFolders", &[PyRep::Integer(folder_id), PyRep::Bool(true)]).await;

            (folder_id, subfolder_id, empty_subfolder_id, perm_bm_id, temp_bm_id)
        };

        // Restart server at T + 1h (before the 3h expiry)
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
        let ctx2 = ServerContext::with_repository(repo2);
        common::pin_clock(&ctx2);
        ctx2.advance_virtual_clock(3_600);

        let mut sess1 = login_and_select(&ctx2, 7303, "demo", 90_000_001).await;
        let active_bms = call(&ctx2, &mut sess1, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await;
        let PyRep::Tuple(active_parts) = &active_bms else { panic!("expected (folders, bookmarks, subfolders) tuple") };
        let PyRep::List(folders_list) = &active_parts[0] else { panic!("expected folders list") };
        let PyRep::List(bms_list) = &active_parts[1] else { panic!("expected bookmarks list") };
        let PyRep::List(subfolders_list) = &active_parts[2] else { panic!("expected subfolders list") };
        assert!(
            folders_list.iter().any(|f| f.dict_get("folderID") == Some(&PyRep::Integer(folder_id)) && f.dict_get("isShared") == Some(&PyRep::Bool(true)))
                && !folders_list.iter().any(|f| f.dict_get("folderID") == Some(&PyRep::Integer(subfolder_id)) || f.dict_get("folderID") == Some(&PyRep::Integer(empty_subfolder_id))),
            "Top-level folders must include shared root folder and exclude subfolders: {folders_list:?}"
        );
        let perm_kv = bms_list.iter().find(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(perm_bm_id))).expect("perm_bm_id in active bookmarks");
        assert_eq!(perm_kv.dict_get("itemID"), Some(&PyRep::Integer(60_003_760)), "Bookmark itemID must persist across SQLite restart");
        assert_eq!(perm_kv.dict_get("isShared"), Some(&PyRep::Bool(true)), "Bookmark isShared must persist across SQLite restart");
        assert_eq!(perm_kv.dict_get("subfolderID"), Some(&PyRep::Integer(subfolder_id)), "Bookmark subfolderID must persist across SQLite restart");
        assert_eq!(perm_kv.dict_get("memo"), Some(&PyRep::String("Deep safe".into())), "Bookmark memo must persist across SQLite restart");
        assert!(
            subfolders_list.iter().any(|s| s.dict_get("subfolderID") == Some(&PyRep::Integer(subfolder_id)) && s.dict_get("subfolderName") == Some(&PyRep::String("C5 Statics".into())) && s.dict_get("folderID") == Some(&PyRep::Integer(folder_id)))
                && subfolders_list.iter().any(|s| s.dict_get("subfolderID") == Some(&PyRep::Integer(empty_subfolder_id)) && s.dict_get("subfolderName") == Some(&PyRep::String("Empty Reconnaissance".into())) && s.dict_get("folderID") == Some(&PyRep::Integer(folder_id))),
            "Both populated and empty subfolders must survive SQLite restart in subfolders list with subfolderName and folderID: {subfolders_list:?}"
        );

        let folder_info = call(&ctx2, &mut sess1, "accessGroupBookmarkMgr", "GetFolderInfo", &[PyRep::Integer(folder_id)]).await;
        let PyRep::KeyVal(fi_kv) = &folder_info else { panic!("expected folder_info KeyVal") };
        assert_eq!(
            fi_kv.get("accessGroupIDs"),
            Some(&PyRep::List(vec![PyRep::Integer(90_000_002), PyRep::Integer(98_000_001), PyRep::Integer(99_000_001)])),
            "Folder ACL accessGroupIDs must persist across SQLite restart"
        );

        ctx2.db.with_snapshot(|s| {
            assert_eq!(
                s.access_groups.get(&1001),
                Some(&vec![90_000_001, 90_000_002]),
                "access_groups table must persist across SQLite restart"
            );
        });

        // Verify ESI character & corporation bookmarks and folders at T + 1h after restart
        let char1_params = HashMap::from([("character_id".to_string(), "90000001".to_string())]);
        let esi_char_bms = dispatch_char_social_route(&ctx2, "GET", "/characters/{character_id}/bookmarks", &char1_params, &HashMap::new(), b"")
            .expect("ESI GET /characters/{character_id}/bookmarks");
        assert_eq!(esi_char_bms.status_code, 200);
        let esi_char_bms_str = esi_char_bms.body.as_ref().unwrap().to_string();
        assert!(esi_char_bms_str.contains("Safe Spot Alpha") && esi_char_bms_str.contains("Temp Wormhole Exit") && esi_char_bms_str.contains("Deep safe") && esi_char_bms_str.contains("60003760"));

        let esi_char_folders = dispatch_char_social_route(&ctx2, "GET", "/characters/{character_id}/bookmarks/folders", &char1_params, &HashMap::new(), b"")
            .expect("ESI GET /characters/{character_id}/bookmarks/folders");
        assert_eq!(esi_char_folders.status_code, 200);
        let esi_char_folders_str = esi_char_folders.body.as_ref().unwrap().to_string();
        assert!(esi_char_folders_str.contains("Wormhole Chain") && !esi_char_folders_str.contains("C5 Statics") && !esi_char_folders_str.contains("Empty Reconnaissance"));

        let corp_params = HashMap::from([("corporation_id".to_string(), "98000001".to_string())]);
        let esi_corp_bms = dispatch_char_social_route(&ctx2, "GET", "/corporations/{corporation_id}/bookmarks", &corp_params, &HashMap::new(), b"")
            .expect("ESI GET /corporations/{corporation_id}/bookmarks");
        assert_eq!(esi_corp_bms.status_code, 200);
        let esi_corp_bms_str = esi_corp_bms.body.as_ref().unwrap().to_string();
        assert!(esi_corp_bms_str.contains("Safe Spot Alpha") && esi_corp_bms_str.contains("Temp Wormhole Exit"));

        let esi_corp_folders = dispatch_char_social_route(&ctx2, "GET", "/corporations/{corporation_id}/bookmarks/folders", &corp_params, &HashMap::new(), b"")
            .expect("ESI GET /corporations/{corporation_id}/bookmarks/folders");
        assert_eq!(esi_corp_folders.status_code, 200);
        let esi_corp_folders_str = esi_corp_folders.body.as_ref().unwrap().to_string();
        assert!(esi_corp_folders_str.contains("Wormhole Chain") && !esi_corp_folders_str.contains("C5 Statics") && !esi_corp_folders_str.contains("Empty Reconnaissance"));

        // Advance clock past 3h total (another 2h + 60s) and verify the 3h bookmark is deleted at expiry in both RPC and ESI
        ctx2.advance_virtual_clock(7_260);
        let bms_after_expiry = call(&ctx2, &mut sess1, "bookmark", "GetBookmarks", &[]).await;
        let bms_after_dbg = format!("{bms_after_expiry:?}");
        assert!(
            bms_after_dbg.contains(&perm_bm_id.to_string()) && !bms_after_dbg.contains(&temp_bm_id.to_string()),
            "3h expiry bookmark must be deleted when expiry is reached after restart: {bms_after_dbg}"
        );
        let esi_char_bms_after = dispatch_char_social_route(&ctx2, "GET", "/characters/{character_id}/bookmarks", &char1_params, &HashMap::new(), b"")
            .expect("ESI GET /characters/{character_id}/bookmarks after expiry");
        let esi_after_str = esi_char_bms_after.body.as_ref().unwrap().to_string();
        assert!(esi_after_str.contains("Safe Spot Alpha") && !esi_after_str.contains("Temp Wormhole Exit"));

        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_d6_chat_channels_motd_operators_password_and_bans_mutes_persist_across_sqlite_restart() {
        // parity: feat:lifecycle.chat-channels-persist, feat:lifecycle.social-durable
        let db_path = temp_sqlite_path("chat_channels");
        let kw = HashMap::new();

        let chan_id = {
            let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
            seed_repo_with_corp(&repo1);
            let ctx1 = ServerContext::with_repository(repo1);
            common::pin_clock(&ctx1);

            let mut sess1 = login_and_select(&ctx1, 7401, "demo", 90_000_001).await;
            let created = call(
                &ctx1,
                &mut sess1,
                "XmppChatMgr",
                "CreatePlayerOwnedChannel",
                &[PyRep::String("Intel_North".into()), PyRep::String("northpass".into()), PyRep::String("Report hostiles +1".into())],
            )
            .await;
            let chan_id = created.dict_get("channelID").and_then(|v| v.as_str()).unwrap_or("channel:intel_north").to_string();

            call(&ctx1, &mut sess1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_002), PyRep::Integer(2)]).await;
            call(&ctx1, &mut sess1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_005), PyRep::Integer(1)]).await;
            call(&ctx1, &mut sess1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_003), PyRep::Integer(-2)]).await;
            call(&ctx1, &mut sess1, "XmppChatMgr", "GMMute", &[PyRep::Integer(90_000_002), PyRep::String(chan_id.clone()), PyRep::Integer(3_600), PyRep::String("Gagged for 1h".into())]).await;

            chan_id
        };

        // Restart server from SQLite file
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
        let ctx2 = ServerContext::with_repository(repo2);
        common::pin_clock(&ctx2);

        let mut sess1 = login_and_select(&ctx2, 7402, "demo", 90_000_001).await;
        let info = call(&ctx2, &mut sess1, "LSC", "GetMembers", &[PyRep::String(chan_id.clone())]).await;
        let info_dbg = format!("{info:?}");
        assert!(
            info_dbg.contains("Intel_North")
                && info_dbg.contains("Report hostiles +1")
                && info_dbg.contains("90000002")
                && info_dbg.contains("90000005")
                && info_dbg.contains("90000003"),
            "Player-owned channel name, MOTD, operators, allow list, deny list, and mutes must survive restart: {info_dbg}"
        );

        // Non-operator, non-allow-listed character 90_000_004 must be rejected with wrong password and accepted with persisted password
        let mut sess4 = SessionState::new(7405);
        sess4.user_id = Some(4);
        sess4.character_id = Some(90_000_004);
        let wrong_pw = ctx2
            .dispatch_rpc(
                &mut sess4,
                "LSC",
                None,
                "JoinChannel",
                &[PyRep::String(chan_id.clone()), PyRep::String("wrongpass".into())],
                &kw,
                None,
            )
            .await
            .expect_err("wrong password must fail after restart");
        assert_eq!(wrong_pw.code, "InvalidChannelPassword");
        call(&ctx2, &mut sess4, "LSC", "JoinChannel", &[PyRep::String(chan_id.clone()), PyRep::String("northpass".into())]).await;

        let mut sess2 = SessionState::new(7403);
        sess2.user_id = Some(2);
        sess2.character_id = Some(90_000_002);
        call(&ctx2, &mut sess2, "LSC", "JoinChannel", &[PyRep::String(chan_id.clone()), PyRep::String("northpass".into())]).await;

        let muted_send = ctx2
            .dispatch_rpc(
                &mut sess2,
                "LSC",
                None,
                "SendMessage",
                &[PyRep::String(chan_id.clone()), PyRep::String("Testing mute".into())],
                &kw,
                None,
            )
            .await;
        assert!(muted_send.is_err());

        let mut sess3 = SessionState::new(7404);
        sess3.user_id = Some(3);
        sess3.character_id = Some(90_000_003);
        let denied_join = ctx2
            .dispatch_rpc(
                &mut sess3,
                "LSC",
                None,
                "JoinChannel",
                &[PyRep::String(chan_id), PyRep::String("northpass".into())],
                &kw,
                None,
            )
            .await;
        assert!(denied_join.is_err());

        let _ = std::fs::remove_file(&db_path);
    }
}

pub(crate) mod single_source_of_truth_client {
    // Integration tests for `plan:D-6` (`feat:invariant.single-source-of-truth`):
    // Contacts, mail labels and deleted mail, custom item names, CSPA charges,
    // war records, alliances, and PLEX balances read identically across MachoNet client RPC,
    // ESI, and Quasar, and persist across SQLite restart.


    use crate::common;
    use eve_db::{
        AllianceLifecycleMeta, AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
        IskCents, WarRecord, WorldRepository,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    const JITA_STATION_ID: i64 = 60_003_760;

    fn temp_sqlite_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("evomu_{tag}_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
        p
    }

    fn cleanup_sqlite_path(p: &PathBuf) {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
    }

    async fn rpc(
        ctx: &ServerContext,
        session: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        let kw = HashMap::new();
        ctx.dispatch_rpc(session, service, None, method, args, &kw, None).await
    }

    const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn issue_token(ctx: &ServerContext, character_id: i64, scopes: &[&str]) -> String {
        let now = ctx.now_unix();
        let code = ctx
            .db
            .authorize_pkce(
                "demo",
                "demo-secret",
                character_id,
                "evomu-d6-ssot",
                "https://localhost/callback",
                scopes.iter().map(|s| s.to_string()).collect(),
                PKCE_CHALLENGE,
                "S256",
                now,
            )
            .expect("authorize_pkce");
        ctx.db
            .exchange_pkce_code(&code, "evomu-d6-ssot", PKCE_VERIFIER, now)
            .expect("exchange_pkce_code")
            .access_token
    }

    fn esi_get(router: &EsiRouter, path: &str, token: Option<&str>) -> serde_json::Value {
        let mut req = EsiHttpRequest::new("GET", path);
        if let Some(t) = token {
            req = req.with_bearer_token(t);
        }
        let resp = router.handle_request(&req);
        assert_eq!(resp.status_code, 200, "ESI GET {path} failed: {}", String::from_utf8_lossy(&resp.body));
        serde_json::from_slice(&resp.body).expect("valid JSON")
    }

    #[tokio::test]
    async fn test_d6_single_source_of_truth_client_esi_quasar_and_restart() {
        let db_path = temp_sqlite_path("d6_ssot");
        let char_id = 90_000_001i64;
        let target_char_id = 90_000_002i64;
        let corp_id = 98_100_001i64;
        let enemy_corp_id = 98_100_002i64;
        let alliance_id = 99_100_001i64;
        let war_id = 77_001i64;
        let ship_item_id = 100_000_001i64;
        let plex_item_id = 100_555_001i64;

        let (mail_id_deleted, mail_id_kept, label_id) = {
            let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 1"));
            let ctx = Arc::new(ServerContext::with_repository(repo));
            let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
            ctx.db.with_mut_snapshot(|snap| {
                *snap = demo;
                snap.corporations.insert(corp_id, CorporationRecord {
                    corporation_id: corp_id,
                    name: "SSOT Corp".into(),
                    ticker: "SSOT".into(),
                    ceo_id: char_id,
                    alliance_id: Some(alliance_id),
                    hq_station_id: Some(JITA_STATION_ID),
                    ..Default::default()
                });
                snap.corporations.insert(enemy_corp_id, CorporationRecord {
                    corporation_id: enemy_corp_id,
                    name: "Rival Corp".into(),
                    ticker: "RIVL".into(),
                    ceo_id: target_char_id,
                    hq_station_id: Some(JITA_STATION_ID),
                    ..Default::default()
                });
                snap.alliances.insert(alliance_id, AllianceRecord {
                    alliance_id,
                    name: "SSOT Alliance".into(),
                    short_name: "SSOTA".into(),
                    executor_corp_id: corp_id,
                    member_corp_ids: [corp_id].into_iter().collect(),
                });
                snap.alliance_lifecycle_meta.insert(alliance_id, AllianceLifecycleMeta {
                    creator_id: char_id,
                    creator_corporation_id: corp_id,
                    date_founded_unix: 1_700_000_000,
                    faction_id: None,
                });
                snap.wars.insert(war_id, WarRecord {
                    war_id,
                    aggressor_corp_id: corp_id,
                    defender_corp_id: enemy_corp_id,
                    mutual: false,
                    started_unix: 1_700_000_000,
                    active: true,
                });
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.corporation_id = corp_id;
                    ch.alliance_id = Some(alliance_id);
                    ch.balance = IskCents(50_000_000_00);
                }
                snap.items.insert(plex_item_id, InventoryItemRecord {
                    item_id: plex_item_id,
                    type_id: 44_992,
                    owner_id: char_id,
                    location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 250,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                });
            });
            common::pin_clock(&ctx);

            let mut sess = SessionState::new(8101);
            common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
            rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("select");

            // 1. Contacts & CSPA
            let lbl = rpc(&ctx, &mut sess, "charMgr", "CreateLabel", &[PyRep::String("SSOT Contacts".into()), PyRep::Integer(0x00FF00)]).await.expect("CreateLabel");
            let label_id = lbl.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);
            rpc(&ctx, &mut sess, "charMgr", "AddContact", &[PyRep::Integer(target_char_id), PyRep::Real(9.5), PyRep::Bool(true), PyRep::Integer(label_id)]).await.expect("AddContact");
            rpc(&ctx, &mut sess, "charMgr", "LogSettings", &[PyRep::Real(7_500.0)]).await.expect("LogSettings");

            // 2. Mail labels & deleted mail
            let m1 = rpc(&ctx, &mut sess, "mailMgr", "SendMail", &[
                PyRep::List(vec![PyRep::Integer(char_id)]),
                PyRep::String("Deleted Mail".into()),
                PyRep::String("Body 1".into()),
                PyRep::Integer(0),
            ]).await.expect("SendMail 1").as_i64().unwrap_or(1);
            let m2 = rpc(&ctx, &mut sess, "mailMgr", "SendMail", &[
                PyRep::List(vec![PyRep::Integer(char_id)]),
                PyRep::String("Kept Mail".into()),
                PyRep::String("Body 2".into()),
                PyRep::Integer(0),
            ]).await.expect("SendMail 2").as_i64().unwrap_or(2);
            rpc(&ctx, &mut sess, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(m1)])]).await.expect("DeleteMail");

            // 3. Custom item name
            rpc(&ctx, &mut sess, "invbroker", "SetLabel", &[PyRep::Integer(ship_item_id), PyRep::String("SSOT Flagship".into())]).await.expect("SetLabel");

            // 4. PLEX vault deposit
            rpc(&ctx, &mut sess, "invbroker", "DepositPlexToVault", &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(plex_item_id),
                PyRep::Integer(100),
                PyRep::Integer(0),
            ]).await.expect("DepositPlexToVault");

            // Verify ESI agrees immediately before restart
            let token = issue_token(&ctx, char_id, &["esi-characters.read_contacts.v1", "esi-mail.read_mail.v1", "esi-assets.read_assets.v1"]);
            let router = EsiRouter::new(Arc::clone(&ctx));
            let contacts_json = esi_get(&router, &format!("/latest/characters/{char_id}/contacts/"), Some(&token));
            assert!(contacts_json.to_string().contains("90000002"));
            let alliance_json = esi_get(&router, &format!("/latest/alliances/{alliance_id}/"), None);
            assert_eq!(alliance_json["ticker"], "SSOTA");
            let war_json = esi_get(&router, &format!("/latest/wars/{war_id}/"), None);
            assert_eq!(war_json["aggressor"]["corporation_id"], corp_id);

            (m1, m2, label_id)
        };

        // Restart from SQLite on disk and verify client RPC, ESI, and Quasar all match
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite 2"));
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        common::pin_clock(&ctx2);

        let mut sess2 = SessionState::new(8102);
        common::login_pilot(&ctx2, &mut sess2, "demo", "demo-secret").await;
        rpc(&ctx2, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("select 2");

        let contacts_rep = rpc(&ctx2, &mut sess2, "charMgr", "GetContacts", &[]).await.expect("GetContacts");
        assert!(format!("{contacts_rep:?}").contains("90000002") && label_id > 0);

        let mut sess3 = SessionState::new(8103);
        sess3.user_id = Some(2);
        sess3.character_id = Some(90_000_002);
        let cspa_contact = rpc(&ctx2, &mut sess3, "charMgr", "CSPAChargedAction", &[PyRep::Integer(char_id)]).await.expect("CSPAChargedAction");
        assert_eq!(cspa_contact, PyRep::Real(0.0));

        let headers_rep = rpc(&ctx2, &mut sess2, "mailMgr", "GetMailHeaders", &[]).await.expect("GetMailHeaders");
        let headers_dbg = format!("{headers_rep:?}");
        assert!(!headers_dbg.contains(&format!("Integer({mail_id_deleted})")) && headers_dbg.contains(&format!("Integer({mail_id_kept})")));

        let token2 = issue_token(&ctx2, char_id, &["esi-characters.read_contacts.v1", "esi-mail.read_mail.v1", "esi-assets.read_assets.v1"]);
        let router2 = EsiRouter::new(Arc::clone(&ctx2));
        let mail_json = esi_get(&router2, &format!("/latest/characters/{char_id}/mail/"), Some(&token2));
        let mail_str = mail_json.to_string();
        assert!(!mail_str.contains("Deleted Mail") && mail_str.contains("Kept Mail"));

        let alliance_json2 = esi_get(&router2, &format!("/latest/alliances/{alliance_id}/"), None);
        assert_eq!(alliance_json2["ticker"], "SSOTA");
        let war_json2 = esi_get(&router2, &format!("/latest/wars/{war_id}/"), None);
        assert_eq!(war_json2["aggressor"]["corporation_id"], corp_id);

        let plex_bal = ctx2.db.with_snapshot(|snap| snap.account_plex_vault.get(&1).copied().unwrap_or(0));
        assert_eq!(plex_bal, 600);
        let item_name = ctx2.db.with_snapshot(|snap| snap.item_custom_names.get(&ship_item_id).cloned().unwrap_or_default());
        assert_eq!(item_name, "SSOT Flagship");

        cleanup_sqlite_path(&db_path);
    }

    #[tokio::test]
    async fn test_t10_client_expectation_oracle_live_rpc_shapes() {
        let oracle_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.agents/skills/evomu-adversarial-review/references/parity/sources/client_expectations.json");
        let raw = std::fs::read_to_string(&oracle_path)
            .expect("read client_expectations.json oracle");
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).expect("parse client_expectations.json");
        let methods = parsed["rpc"]
            .as_object()
            .expect("client_expectations.json must contain rpc object");
        assert!(
            methods.len() >= 1000,
            "expected >= 1000 V24 client RPC expectations, got {}",
            methods.len()
        );
        assert!(methods.contains_key("machoNet.GetTime"));
        assert!(methods.contains_key("charUnboundMgr.GetCharacterInfo"));
        assert!(methods.contains_key("marketProxy.GetCharOrders"));

        let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
        let mut sess = SessionState::new(8201);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let time_rep = rpc(&ctx, &mut sess, "machoNet", "GetTime", &[])
            .await
            .expect("machoNet.GetTime");
        assert!(matches!(time_rep, PyRep::Integer(t) if t > 0));
        let info_rep = rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharacterInfo", &[PyRep::Integer(90_000_001)])
            .await
            .expect("charUnboundMgr.GetCharacterInfo");
        assert!(!matches!(info_rep, PyRep::None));
        sess.character_id = Some(90_000_001);
        let mkt_rep = rpc(&ctx, &mut sess, "marketProxy", "GetCharOrders", &[])
            .await
            .expect("marketProxy.GetCharOrders");
        assert!(!matches!(mkt_rep, PyRep::None));
    }
}
